#!/usr/bin/env python3
"""LlaMask protocol adapter for Qwen through llama.cpp completion."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path
from typing import Any


PROTOCOL_VERSION = 1
MAX_REQUEST_BYTES = 2 * 1024 * 1024
ALLOWED_ENTITY_TYPES = {
    "PERSON_NAME",
    "ORG_NAME",
    "ADDRESS",
    "PHONE_NUMBER",
    "EMAIL",
    "CN_ID_NUMBER",
    "BANK_CARD_NUMBER",
    "FINANCIAL_AMOUNT",
    "SALARY_AMOUNT",
    "BUSINESS_METRIC",
    "PROJECT_CODE",
    "CONTRACT_ID",
    "CUSTOMER_NAME",
}


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


def first_json_object(output: str) -> dict[str, object]:
    start = output.find("{")
    if start < 0:
        raise ValueError("missing JSON object")
    depth = 0
    in_string = False
    escaped = False
    for index in range(start, len(output)):
        character = output[index]
        if in_string:
            if escaped:
                escaped = False
            elif character == "\\":
                escaped = True
            elif character == '"':
                in_string = False
            continue
        if character == '"':
            in_string = True
        elif character == "{":
            depth += 1
        elif character == "}":
            depth -= 1
            if depth == 0:
                parsed = json.loads(output[start : index + 1])
                if not isinstance(parsed, dict):
                    raise ValueError("response must be an object")
                return parsed
    raise ValueError("incomplete JSON object")


def build_user_prompt(request: dict[str, Any]) -> str:
    payload = [{"id": request["request_id"], "text": request["text"]}]
    return "请处理以下JSON数组：\n" + json.dumps(
        payload,
        ensure_ascii=False,
        separators=(",", ":"),
    )


def run_llama(
    runner: Path,
    model: Path,
    schema: Path,
    system_prompt: Path,
    user_prompt: str,
    timeout_seconds: int,
    context_size: int,
    max_tokens: int,
    device: str,
) -> dict[str, object]:
    command = [
        str(runner),
        "--model",
        str(model),
        "--ctx-size",
        str(context_size),
        "--predict",
        str(max_tokens),
        "--temperature",
        "0",
        "--seed",
        "20260731",
        "--simple-io",
        "--single-turn",
        "--no-display-prompt",
        "--verbosity",
        "0",
        "--json-schema-file",
        str(schema),
        "--fit",
        "off",
    ]
    if device == "cpu":
        command.extend(["--device", "none", "--gpu-layers", "0"])
    combined_prompt = (
        system_prompt.read_text(encoding="utf-8") + "\n\n" + user_prompt
    ).replace("\n", " ")
    completed = subprocess.run(
        command,
        input=combined_prompt + "\n",
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=timeout_seconds,
        check=False,
        env={
            **os.environ,
            "NO_PROXY": "*",
            "HTTP_PROXY": "",
            "HTTPS_PROXY": "",
            "ALL_PROXY": "",
        },
    )
    if completed.returncode != 0:
        raise RuntimeError("llama.cpp failed")
    return first_json_object(completed.stdout)


def align_entities(
    text: str,
    parsed: dict[str, object],
    operational_confidence: float,
) -> list[dict[str, object]]:
    entities = parsed.get("entities")
    if not isinstance(entities, list):
        raise ValueError("entities must be a list")
    findings: list[dict[str, object]] = []
    seen: set[tuple[int, int, str]] = set()
    for entity in entities:
        if not isinstance(entity, dict):
            raise ValueError("entity must be an object")
        matched_text = entity.get("text")
        entity_type = entity.get("label")
        if (
            not isinstance(matched_text, str)
            or not matched_text
            or entity_type not in ALLOWED_ENTITY_TYPES
        ):
            raise ValueError("invalid entity")
        search_start = 0
        while (start := text.find(matched_text, search_start)) >= 0:
            end = start + len(matched_text)
            key = (start, end, str(entity_type))
            if key not in seen:
                seen.add(key)
                findings.append(
                    {
                        "start": start,
                        "end": end,
                        "entity_type": entity_type,
                        "matched_text": matched_text,
                        "confidence": operational_confidence,
                        "reason_code": "QWEN_ZERO_SHOT_UNCALIBRATED",
                    }
                )
            search_start = end
    return sorted(
        findings,
        key=lambda finding: (
            int(finding["start"]),
            int(finding["end"]),
            str(finding["entity_type"]),
        ),
    )


def entities_for_request(
    parsed: dict[str, object],
    request_id: str,
) -> list[dict[str, object]]:
    items = parsed.get("items")
    if not isinstance(items, list):
        raise ValueError("items must be a list")
    if not items:
        return []
    if len(items) != 1 or not isinstance(items[0], dict):
        raise ValueError("expected exactly one result item")
    item = items[0]
    if item.get("id") != request_id or not isinstance(item.get("entities"), list):
        raise ValueError("result id or entities mismatch")
    return item["entities"]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--runner", type=Path, required=True)
    parser.add_argument("--model", type=Path, required=True)
    parser.add_argument("--schema", type=Path, required=True)
    parser.add_argument("--system-prompt", type=Path, required=True)
    parser.add_argument("--detector-id", default="qwen_review")
    parser.add_argument("--max-chars", type=int, default=2_000)
    parser.add_argument("--operational-confidence", type=float, default=0.80)
    parser.add_argument("--timeout-seconds", type=int, default=120)
    parser.add_argument("--context-size", type=int, default=4_096)
    parser.add_argument("--max-tokens", type=int, default=1_024)
    parser.add_argument("--device", choices=("cpu", "auto"), default="cpu")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.max_chars <= 0 or args.timeout_seconds <= 0:
        raise ValueError("limits must be positive")
    if not 0.0 <= args.operational_confidence <= 1.0:
        raise ValueError("operational confidence must be between 0 and 1")
    runner = args.runner.resolve(strict=True)
    model = args.model.resolve(strict=True)
    schema = args.schema.resolve(strict=True)
    system_prompt = args.system_prompt.resolve(strict=True)
    request = read_request(args.detector_id, args.max_chars)
    parsed = run_llama(
        runner,
        model,
        schema,
        system_prompt,
        build_user_prompt(request),
        args.timeout_seconds,
        args.context_size,
        args.max_tokens,
        args.device,
    )
    response = {
        "protocol_version": PROTOCOL_VERSION,
        "request_id": request["request_id"],
        "findings": align_entities(
            str(request["text"]),
            {
                "entities": entities_for_request(
                    parsed,
                    str(request["request_id"]),
                )
            },
            args.operational_confidence,
        ),
        "warnings": ["QWEN_CONFIDENCE_IS_NOT_CALIBRATED"],
    }
    sys.stdout.write(
        json.dumps(response, ensure_ascii=False, separators=(",", ":")) + "\n"
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as error:
        print(f"qwen_sidecar_error={type(error).__name__}", file=sys.stderr)
        raise SystemExit(1) from None
