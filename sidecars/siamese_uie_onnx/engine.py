"""SiameseUIE inference without PyTorch, Transformers, or ModelScope."""

from __future__ import annotations

from collections.abc import Sequence
from pathlib import Path

import numpy as np
import onnxruntime as ort
from tokenizers import BertWordPieceTokenizer, Encoding


HINTS = ("人物: ", "组织机构: ")
HINT_TYPES = ("人物", "组织机构")
EXPECTED_INPUTS = {
    "input_ids",
    "attention_mask",
    "hint_ids",
    "hint_attention_mask",
}
EXPECTED_OUTPUTS = {"head_logits", "tail_logits"}


def split_windows(
    values: Sequence[int],
    max_length: int,
    slide_length: int,
    pad_value: int,
) -> list[tuple[int, list[int]]]:
    if max_length <= 0:
        raise ValueError("max length must be positive")
    if not 0 < slide_length <= max_length:
        raise ValueError("slide length must be between 1 and max length")
    if len(values) <= max_length:
        return [(0, list(values))]
    windows: list[tuple[int, list[int]]] = []
    start = 0
    while start < len(values):
        window = list(values[start : start + max_length])
        if len(window) < max_length:
            window.extend([pad_value] * (max_length - len(window)))
        windows.append((start, window))
        if start + max_length >= len(values):
            break
        start += slide_length
    return windows


def merge_window_logits(
    window_logits: Sequence[tuple[int, np.ndarray]],
    token_count: int,
) -> np.ndarray:
    if not window_logits:
        raise ValueError("at least one logit window is required")
    hint_count = window_logits[0][1].shape[0]
    merged = np.full((hint_count, token_count), -1.0, dtype=np.float32)
    for shift, logits in window_logits:
        if logits.ndim != 2 or logits.shape[0] != hint_count:
            raise ValueError("inconsistent logit window shape")
        for local_index in range(logits.shape[1]):
            target_index = shift + local_index
            if target_index >= token_count:
                break
            current = merged[:, target_index]
            incoming = logits[:, local_index]
            merged[:, target_index] = np.where(
                current == -1.0,
                incoming,
                (current + incoming) / 2.0,
            )
    return merged


def get_entities(
    text: str,
    offsets: Sequence[tuple[int, int]],
    head_logits: np.ndarray,
    tail_logits: np.ndarray,
    threshold: float,
) -> list[dict[str, object]]:
    entities: list[dict[str, object]] = []
    potential_heads = np.flatnonzero(head_logits > threshold)
    for potential_head in potential_heads:
        potential_tails = np.flatnonzero(
            tail_logits[int(potential_head) :] > threshold
        )
        if potential_tails.size == 0:
            continue
        potential_tail = int(potential_head) + int(potential_tails[0])
        char_head = offsets[int(potential_head)][0]
        char_tail = offsets[potential_tail][1]
        if char_head >= char_tail:
            continue
        entities.append(
            {
                "span": text[char_head:char_tail],
                "offset": [char_head, char_tail],
            }
        )
    return sorted(entities, key=lambda item: tuple(item["offset"]))


class SiameseUieOnnxEngine:
    def __init__(
        self,
        model_path: Path,
        vocab_path: Path,
        *,
        threshold: float = 0.5,
        max_length: int = 384,
        slide_length: int = 352,
        hint_max_length: int = 128,
        intra_op_threads: int = 0,
    ) -> None:
        if not 0.0 <= threshold <= 1.0:
            raise ValueError("threshold must be between 0 and 1")
        if max_length + hint_max_length > 512:
            raise ValueError("text and hint limits exceed model positions")
        session_options = ort.SessionOptions()
        if intra_op_threads > 0:
            session_options.intra_op_num_threads = intra_op_threads
        session_options.graph_optimization_level = (
            ort.GraphOptimizationLevel.ORT_ENABLE_ALL
        )
        self.session = ort.InferenceSession(
            str(model_path),
            sess_options=session_options,
            providers=["CPUExecutionProvider"],
        )
        inputs = {value.name for value in self.session.get_inputs()}
        outputs = {value.name for value in self.session.get_outputs()}
        if inputs != EXPECTED_INPUTS or outputs != EXPECTED_OUTPUTS:
            raise ValueError("unexpected SiameseUIE ONNX graph interface")
        self.tokenizer = BertWordPieceTokenizer(
            str(vocab_path),
            lowercase=True,
        )
        self.threshold = threshold
        self.max_length = max_length
        self.slide_length = slide_length
        self.hint_max_length = hint_max_length
        self.hint_ids, self.hint_attention_mask = self._tokenize_hints(HINTS)

    def _tokenize_hints(
        self,
        hints: Sequence[str],
    ) -> tuple[np.ndarray, np.ndarray]:
        encodings = self.tokenizer.encode_batch(list(hints))
        longest = max(len(encoding.ids) for encoding in encodings)
        if longest > self.hint_max_length:
            raise ValueError("hint exceeds configured maximum length")
        ids: list[list[int]] = []
        masks: list[list[int]] = []
        for encoding in encodings:
            padding = longest - len(encoding.ids)
            ids.append(encoding.ids + [0] * padding)
            masks.append(encoding.attention_mask + [0] * padding)
        return np.asarray(ids, dtype=np.int64), np.asarray(
            masks,
            dtype=np.int64,
        )

    def _run_window(
        self,
        input_ids: Sequence[int],
        attention_mask: Sequence[int],
    ) -> tuple[np.ndarray, np.ndarray]:
        outputs = self.session.run(
            ["head_logits", "tail_logits"],
            {
                "input_ids": np.asarray([input_ids], dtype=np.int64),
                "attention_mask": np.asarray(
                    [attention_mask],
                    dtype=np.int64,
                ),
                "hint_ids": self.hint_ids,
                "hint_attention_mask": self.hint_attention_mask,
            },
        )
        return outputs[0], outputs[1]

    def extract(self, text: str) -> dict[str, object]:
        encoding: Encoding = self.tokenizer.encode(text)
        id_windows = split_windows(
            encoding.ids,
            self.max_length,
            self.slide_length,
            0,
        )
        mask_windows = split_windows(
            encoding.attention_mask,
            self.max_length,
            self.slide_length,
            0,
        )
        head_windows: list[tuple[int, np.ndarray]] = []
        tail_windows: list[tuple[int, np.ndarray]] = []
        for (id_shift, ids), (mask_shift, masks) in zip(
            id_windows,
            mask_windows,
            strict=True,
        ):
            if id_shift != mask_shift:
                raise RuntimeError("token and mask windows are misaligned")
            head_logits, tail_logits = self._run_window(ids, masks)
            head_windows.append((id_shift, head_logits))
            tail_windows.append((id_shift, tail_logits))
        merged_heads = merge_window_logits(head_windows, len(encoding.ids))
        merged_tails = merge_window_logits(tail_windows, len(encoding.ids))
        output: list[list[dict[str, object]]] = []
        for index, entity_type in enumerate(HINT_TYPES):
            for entity in get_entities(
                text,
                encoding.offsets,
                merged_heads[index],
                merged_tails[index],
                self.threshold,
            ):
                output.append(
                    [
                        {
                            "type": entity_type,
                            "span": entity["span"],
                            "offset": entity["offset"],
                        }
                    ]
                )
        return {"output": output}
