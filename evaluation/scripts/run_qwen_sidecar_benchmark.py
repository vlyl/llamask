#!/usr/bin/env python3
"""Benchmark the production-shaped Qwen sidecar on a dataset subset."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
from pathlib import Path


def load_jsonl(path: Path) -> list[dict[str, object]]:
    with path.open(encoding="utf-8") as handle:
        return [json.loads(line) for line in handle if line.strip()]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("dataset", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--sidecar", type=Path, required=True)
    parser.add_argument("--runner", type=Path, required=True)
    parser.add_argument("--model", type=Path, required=True)
    parser.add_argument("--schema", type=Path, required=True)
    parser.add_argument("--system-prompt", type=Path, required=True)
    parser.add_argument("--split", choices=("dev", "test", "all"), default="test")
    parser.add_argument("--limit", type=int)
    parser.add_argument("--timeout-seconds", type=int, default=180)
    args = parser.parse_args()

    records = load_jsonl(args.dataset)
    if args.split != "all":
        records = [record for record in records if record["split"] == args.split]
    if args.limit is not None:
        records = records[: args.limit]
    args.output.parent.mkdir(parents=True, exist_ok=True)
    invalid = 0
    with args.output.open("w", encoding="utf-8", newline="\n") as target:
        for index, record in enumerate(records, start=1):
            request_id = str(record["id"])
            request = {
                "protocol_version": 1,
                "request_id": request_id,
                "detector_id": "qwen_review",
                "document_id": request_id,
                "part_id": "part-0001",
                "offset_unit": "unicode_scalar",
                "text": record["text"],
                "candidates": [],
            }
            command = [
                sys.executable,
                str(args.sidecar),
                "--runner",
                str(args.runner),
                "--model",
                str(args.model),
                "--schema",
                str(args.schema),
                "--system-prompt",
                str(args.system_prompt),
                "--device",
                "cpu",
            ]
            started = time.perf_counter()
            completed = subprocess.run(
                command,
                input=json.dumps(request, ensure_ascii=False),
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                timeout=args.timeout_seconds,
                check=False,
            )
            runtime_ms = (time.perf_counter() - started) * 1000
            valid = completed.returncode == 0
            entities: list[dict[str, object]] = []
            if valid:
                try:
                    response = json.loads(completed.stdout)
                    if response["request_id"] != request_id:
                        raise ValueError("request id mismatch")
                    entities = [
                        {
                            "start": finding["start"],
                            "end": finding["end"],
                            "label": finding["entity_type"],
                            "text": finding["matched_text"],
                            "confidence": finding["confidence"],
                            "detector": "qwen-review-sidecar-v1",
                        }
                        for finding in response["findings"]
                    ]
                except (KeyError, TypeError, ValueError, json.JSONDecodeError):
                    valid = False
                    entities = []
            if not valid:
                invalid += 1
            target.write(
                json.dumps(
                    {
                        "id": request_id,
                        "entities": entities,
                        "runtime_ms": runtime_ms,
                        "structured_output_valid": valid,
                    },
                    ensure_ascii=False,
                )
                + "\n"
            )
            print(
                f"completed={index}/{len(records)} valid={valid} "
                f"runtime_ms={runtime_ms:.1f}",
                flush=True,
            )
    print(f"invalid={invalid}")
    print(f"output={args.output}")
    return 1 if invalid else 0


if __name__ == "__main__":
    raise SystemExit(main())
