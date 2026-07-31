#!/usr/bin/env python3
"""LlaMask protocol adapter for the local SiameseUIE model."""

from __future__ import annotations

import argparse
import contextlib
import json
import os
import sys
from collections.abc import Iterable
from pathlib import Path
from typing import Any


PROTOCOL_VERSION = 1
MAX_REQUEST_BYTES = 2 * 1024 * 1024
SCHEMA = {"人物": None, "组织机构": None}
TYPE_TO_ENTITY = {"人物": "PERSON_NAME", "组织机构": "ORG_NAME"}
CUSTOMER_CUES = ("重点客户", "客户名单", "客户名称", "客户：", "客户:")


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


def flatten(items: object) -> Iterable[dict[str, object]]:
    if isinstance(items, dict):
        yield items
    elif isinstance(items, list):
        for item in items:
            yield from flatten(item)


def normalize_findings(
    text: str,
    raw: dict[str, object],
    operational_confidence: float,
) -> list[dict[str, object]]:
    findings: list[dict[str, object]] = []
    seen: set[tuple[int, int, str]] = set()
    customer_context = any(cue in text for cue in CUSTOMER_CUES)
    for item in flatten(raw.get("output", [])):
        source_type = str(item.get("type", ""))
        entity_type = TYPE_TO_ENTITY.get(source_type)
        if entity_type is None:
            continue
        offset = item.get("offset")
        if (
            not isinstance(offset, list)
            or len(offset) != 2
            or any(isinstance(value, bool) or not isinstance(value, int) for value in offset)
        ):
            continue
        start, end = offset
        if not (0 <= start < end <= len(text)):
            continue
        matched_text = str(item.get("span", ""))
        if text[start:end] != matched_text:
            continue
        if entity_type == "ORG_NAME" and customer_context:
            entity_type = "CUSTOMER_NAME"
        key = (start, end, entity_type)
        if key in seen:
            continue
        seen.add(key)
        findings.append(
            {
                "start": start,
                "end": end,
                "entity_type": entity_type,
                "matched_text": matched_text,
                "confidence": operational_confidence,
                "reason_code": "SIAMESE_UIE_ZERO_SHOT_UNCALIBRATED",
            }
        )
    return sorted(
        findings,
        key=lambda finding: (
            int(finding["start"]),
            int(finding["end"]),
            str(finding["entity_type"]),
        ),
    )


def validate_request(
    request: object,
    detector_id: str,
    max_chars: int,
) -> dict[str, Any]:
    if not isinstance(request, dict):
        raise ValueError("request must be an object")
    if request.get("protocol_version") != PROTOCOL_VERSION:
        raise ValueError("protocol mismatch")
    if request.get("detector_id") != detector_id:
        raise ValueError("detector mismatch")
    if request.get("offset_unit") != "unicode_scalar":
        raise ValueError("offset unit mismatch")
    for field in ("request_id", "document_id", "part_id"):
        if not isinstance(request.get(field), str) or not request[field]:
            raise ValueError(f"invalid {field}")
    text = request.get("text")
    if not isinstance(text, str) or len(text) > max_chars:
        raise ValueError("invalid text")
    candidates = request.get("candidates")
    if not isinstance(candidates, list):
        raise ValueError("invalid candidates")
    return request


def read_request(detector_id: str, max_chars: int) -> dict[str, Any]:
    payload = sys.stdin.buffer.read(MAX_REQUEST_BYTES + 1)
    if len(payload) > MAX_REQUEST_BYTES:
        raise ValueError("request too large")
    return validate_request(json.loads(payload), detector_id, max_chars)


def run_model(model_path: Path, text: str) -> dict[str, object]:
    with contextlib.redirect_stdout(sys.stderr):
        from modelscope.pipelines import pipeline
        from modelscope.utils.constant import Tasks

        extractor = pipeline(
            Tasks.siamese_uie,
            model=str(model_path),
            device="cpu",
            trust_remote_code=True,
        )
        return extractor(input=text, schema=SCHEMA)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--model", type=Path, required=True)
    parser.add_argument("--detector-id", default="siamese_uie")
    parser.add_argument("--max-chars", type=int, default=20_000)
    parser.add_argument("--operational-confidence", type=float, default=0.85)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.max_chars <= 0:
        raise ValueError("max chars must be positive")
    if not 0.0 <= args.operational_confidence <= 1.0:
        raise ValueError("operational confidence must be between 0 and 1")
    model_path = args.model.resolve(strict=True)
    if not model_path.is_dir():
        raise ValueError("model path must be a directory")
    configure_offline_environment(model_path.parent)
    request = read_request(args.detector_id, args.max_chars)
    raw = run_model(model_path, str(request["text"]))
    response = {
        "protocol_version": PROTOCOL_VERSION,
        "request_id": request["request_id"],
        "findings": normalize_findings(
            str(request["text"]),
            raw,
            args.operational_confidence,
        ),
        "warnings": ["SIAMESE_UIE_CONFIDENCE_IS_NOT_CALIBRATED"],
    }
    sys.stdout.write(
        json.dumps(response, ensure_ascii=False, separators=(",", ":")) + "\n"
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as error:
        print(f"siamese_sidecar_error={type(error).__name__}", file=sys.stderr)
        raise SystemExit(1) from None
