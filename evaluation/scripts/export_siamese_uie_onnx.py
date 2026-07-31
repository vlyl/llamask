#!/usr/bin/env python3
"""Export SiameseUIE as one offline ONNX graph and verify it against PyTorch."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import tempfile
import time
from pathlib import Path
from typing import Any

import numpy as np
import onnx
import onnxruntime as ort
import torch


DEFAULT_MODEL = Path("evaluation/models/cache/siamese-uie-chinese-base")
DEFAULT_OUTPUT = Path(
    "evaluation/models/cache/siamese-uie-chinese-base-onnx/model.onnx"
)
DEFAULT_REPORT = Path(
    "evaluation/models/cache/siamese-uie-chinese-base-onnx/export-report.json"
)
HINTS = ("人物: ", "组织机构: ")
SAMPLE_TEXTS = (
    "联系人沈景行，所属单位星海科技有限公司。",
    "重点客户青屿环科咨询中心，负责人陆晓宁。",
)


def configure_offline_environment(cache_root: Path) -> None:
    os.environ["MODELSCOPE_CACHE"] = str(cache_root / ".modelscope")
    os.environ["HF_HOME"] = str(cache_root / ".huggingface")
    os.environ["TORCH_HOME"] = str(cache_root / ".torch")
    os.environ["HF_HUB_OFFLINE"] = "1"
    os.environ["TRANSFORMERS_OFFLINE"] = "1"
    os.environ["MODELSCOPE_OFFLINE"] = "1"
    os.environ["TOKENIZERS_PARALLELISM"] = "false"
    os.environ["NO_PROXY"] = "*"
    for name in ("HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"):
        os.environ.pop(name, None)


class SiameseUieOnnxWrapper(torch.nn.Module):
    """Run one text against a dynamic batch of schema hints."""

    def __init__(self, model: torch.nn.Module) -> None:
        super().__init__()
        self.model = model

    def forward(
        self,
        input_ids: torch.Tensor,
        attention_mask: torch.Tensor,
        hint_ids: torch.Tensor,
        hint_attention_mask: torch.Tensor,
    ) -> tuple[torch.Tensor, torch.Tensor]:
        text_sequence = self.model.get_plm_sequence_output(
            input_ids,
            attention_mask,
        )
        hint_batch_size = hint_ids.size(0)
        text_sequence = text_sequence.expand(hint_batch_size, -1, -1)
        expanded_attention_mask = attention_mask.expand(
            hint_batch_size,
            -1,
        )
        return self.model.fast_inference(
            text_sequence,
            expanded_attention_mask,
            hint_ids,
            hint_attention_mask,
        )


def tokenize_inputs(
    extractor: Any,
    text: str,
    hints: tuple[str, ...] = HINTS,
) -> tuple[dict[str, torch.Tensor], list[list[int]]]:
    tokenized_text = extractor.preprocessor([text])[0]
    tokenized_hints = extractor.preprocessor(
        list(hints),
        padding=True,
        truncation=True,
        max_length=extractor.hint_max_len,
    )
    inputs = {
        "input_ids": torch.tensor(
            [tokenized_text.ids],
            dtype=torch.long,
        ),
        "attention_mask": torch.tensor(
            [tokenized_text.attention_mask],
            dtype=torch.long,
        ),
        "hint_ids": torch.tensor(
            [tokenized_hints[index].ids for index in range(len(hints))],
            dtype=torch.long,
        ),
        "hint_attention_mask": torch.tensor(
            [
                tokenized_hints[index].attention_mask
                for index in range(len(hints))
            ],
            dtype=torch.long,
        ),
    }
    return inputs, tokenized_text.offsets


def decode_entities(
    extractor: Any,
    text: str,
    offsets: list[list[int]],
    head_logits: np.ndarray,
    tail_logits: np.ndarray,
    hints: tuple[str, ...] = HINTS,
) -> list[dict[str, object]]:
    entities: list[dict[str, object]] = []
    for index, hint in enumerate(hints):
        entity_type = hint.removesuffix(": ")
        for item in extractor.get_entities(
            text,
            offsets,
            head_logits[index].tolist(),
            tail_logits[index].tolist(),
        ):
            entities.append(
                {
                    "type": entity_type,
                    "span": item["span"],
                    "offset": item["offset"],
                }
            )
    return entities


def tensor_inputs_to_numpy(
    inputs: dict[str, torch.Tensor],
) -> dict[str, np.ndarray]:
    return {name: value.detach().cpu().numpy() for name, value in inputs.items()}


def file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def export_model(
    wrapper: SiameseUieOnnxWrapper,
    inputs: dict[str, torch.Tensor],
    output: Path,
    opset: int,
) -> float:
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        prefix=f".{output.stem}-",
        suffix=".onnx",
        dir=output.parent,
        delete=False,
    ) as temporary:
        temporary_path = Path(temporary.name)
    started = time.perf_counter()
    try:
        with torch.inference_mode():
            torch.onnx.export(
                wrapper,
                (
                    inputs["input_ids"],
                    inputs["attention_mask"],
                    inputs["hint_ids"],
                    inputs["hint_attention_mask"],
                ),
                temporary_path,
                input_names=list(inputs),
                output_names=["head_logits", "tail_logits"],
                dynamic_axes={
                    "input_ids": {1: "text_length"},
                    "attention_mask": {1: "text_length"},
                    "hint_ids": {
                        0: "hint_batch",
                        1: "hint_length",
                    },
                    "hint_attention_mask": {
                        0: "hint_batch",
                        1: "hint_length",
                    },
                    "head_logits": {
                        0: "hint_batch",
                        1: "text_length",
                    },
                    "tail_logits": {
                        0: "hint_batch",
                        1: "text_length",
                    },
                },
                opset_version=opset,
                dynamo=False,
                external_data=False,
                do_constant_folding=True,
            )
        onnx.checker.check_model(str(temporary_path))
        os.replace(temporary_path, output)
    finally:
        temporary_path.unlink(missing_ok=True)
    return (time.perf_counter() - started) * 1000


def verify_model(
    wrapper: SiameseUieOnnxWrapper,
    extractor: Any,
    output: Path,
) -> tuple[list[dict[str, object]], float]:
    session = ort.InferenceSession(
        str(output),
        providers=["CPUExecutionProvider"],
    )
    sample_results: list[dict[str, object]] = []
    maximum_error = 0.0
    for text in SAMPLE_TEXTS:
        inputs, offsets = tokenize_inputs(extractor, text)
        with torch.inference_mode():
            torch_outputs = wrapper(**inputs)
        expected = tuple(value.detach().cpu().numpy() for value in torch_outputs)
        started = time.perf_counter()
        actual = session.run(None, tensor_inputs_to_numpy(inputs))
        runtime_ms = (time.perf_counter() - started) * 1000
        errors = [
            float(np.max(np.abs(expected_value - actual_value)))
            for expected_value, actual_value in zip(expected, actual)
        ]
        maximum_error = max(maximum_error, *errors)
        for expected_value, actual_value in zip(expected, actual):
            np.testing.assert_allclose(
                actual_value,
                expected_value,
                rtol=1e-4,
                atol=1e-4,
            )
        pytorch_entities = decode_entities(
            extractor,
            text,
            offsets,
            expected[0],
            expected[1],
        )
        onnx_entities = decode_entities(
            extractor,
            text,
            offsets,
            actual[0],
            actual[1],
        )
        if onnx_entities != pytorch_entities:
            raise RuntimeError("ONNX entity output differs from PyTorch")
        sample_results.append(
            {
                "text": text,
                "text_tokens": int(inputs["input_ids"].shape[1]),
                "hint_batch": int(inputs["hint_ids"].shape[0]),
                "hint_tokens": int(inputs["hint_ids"].shape[1]),
                "onnx_runtime_ms": runtime_ms,
                "max_abs_error": max(errors),
                "entities": onnx_entities,
            }
        )
    return sample_results, maximum_error


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--model", type=Path, default=DEFAULT_MODEL)
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument("--report", type=Path, default=DEFAULT_REPORT)
    parser.add_argument("--opset", type=int, default=17)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    model_path = args.model.resolve(strict=True)
    output_path = args.output.resolve()
    report_path = args.report.resolve()
    if not model_path.is_dir():
        raise ValueError("model path must be a directory")
    if args.opset < 17:
        raise ValueError("opset must be at least 17")
    configure_offline_environment(model_path.parent)

    from modelscope.pipelines import pipeline
    from modelscope.utils.constant import Tasks

    load_started = time.perf_counter()
    extractor = pipeline(
        Tasks.siamese_uie,
        model=str(model_path),
        device="cpu",
        trust_remote_code=True,
    )
    load_ms = (time.perf_counter() - load_started) * 1000
    wrapper = SiameseUieOnnxWrapper(extractor.model).eval()
    sample_inputs, _ = tokenize_inputs(extractor, SAMPLE_TEXTS[0])
    export_ms = export_model(
        wrapper,
        sample_inputs,
        output_path,
        args.opset,
    )
    sample_results, maximum_error = verify_model(
        wrapper,
        extractor,
        output_path,
    )
    model_proto = onnx.load(str(output_path), load_external_data=False)
    report = {
        "format": "onnx",
        "source_model": str(model_path),
        "output": str(output_path),
        "opset": args.opset,
        "parameters": sum(parameter.numel() for parameter in wrapper.parameters()),
        "model_load_ms": load_ms,
        "export_ms": export_ms,
        "file_size_bytes": output_path.stat().st_size,
        "sha256": file_sha256(output_path),
        "graph_inputs": [value.name for value in model_proto.graph.input],
        "graph_outputs": [value.name for value in model_proto.graph.output],
        "external_weight_files": [],
        "verification": {
            "provider": "CPUExecutionProvider",
            "rtol": 1e-4,
            "atol": 1e-4,
            "max_abs_error": maximum_error,
            "entity_outputs_equal": True,
            "samples": sample_results,
        },
        "versions": {
            "torch": torch.__version__,
            "onnx": onnx.__version__,
            "onnxruntime": ort.__version__,
        },
    }
    report_path.parent.mkdir(parents=True, exist_ok=True)
    report_path.write_text(
        json.dumps(report, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )
    print(json.dumps(report, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
