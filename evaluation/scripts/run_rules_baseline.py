#!/usr/bin/env python3
"""Run deterministic regex and checksum detectors as the evaluation baseline."""

from __future__ import annotations

import argparse
import json
import re
import time
from pathlib import Path
from typing import Callable


PATTERNS: list[tuple[str, re.Pattern[str], Callable[[str], bool]]] = []


def always(_: str) -> bool:
    return True


def valid_luhn(value: str) -> bool:
    digits = [int(char) for char in value if char.isdigit()]
    if len(digits) < 12 or len(digits) > 19:
        return False
    total = 0
    parity = len(digits) % 2
    for index, digit in enumerate(digits):
        if index % 2 == parity:
            digit *= 2
            if digit > 9:
                digit -= 9
        total += digit
    return total % 10 == 0


def valid_cn_id(value: str) -> bool:
    if not re.fullmatch(r"\d{17}[\dXx]", value):
        return False
    weights = [7, 9, 10, 5, 8, 4, 2, 1, 6, 3, 7, 9, 10, 5, 8, 4, 2]
    checks = "10X98765432"
    expected = checks[sum(int(v) * w for v, w in zip(value[:17], weights)) % 11]
    return value[-1].upper() == expected


PATTERNS.extend(
    [
        (
            "EMAIL",
            re.compile(r"(?<![\w.+-])[\w.+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}(?![\w.-])"),
            always,
        ),
        (
            "PHONE_NUMBER",
            re.compile(r"(?<!\d)1[3-9]\d{9}(?!\d)"),
            always,
        ),
        (
            "CN_ID_NUMBER",
            re.compile(r"(?<!\d)\d{17}[\dXx](?![\dXx])"),
            valid_cn_id,
        ),
        (
            "BANK_CARD_NUMBER",
            re.compile(r"(?<!\d)(?:\d[ -]?){11,18}\d(?!\d)"),
            valid_luhn,
        ),
    ]
)


def detect(text: str) -> list[dict[str, object]]:
    findings: list[dict[str, object]] = []
    occupied: set[tuple[int, int, str]] = set()
    for label, pattern, validator in PATTERNS:
        for match in pattern.finditer(text):
            value = match.group(0)
            normalized = re.sub(r"[ -]", "", value)
            if not validator(normalized):
                continue
            key = (match.start(), match.end(), label)
            if key in occupied:
                continue
            occupied.add(key)
            findings.append(
                {
                    "start": match.start(),
                    "end": match.end(),
                    "label": label,
                    "text": value,
                    "confidence": 1.0,
                    "detector": "rules-v1",
                }
            )
    return sorted(findings, key=lambda item: (item["start"], item["end"], item["label"]))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("dataset", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    args.output.parent.mkdir(parents=True, exist_ok=True)

    count = 0
    total_ms = 0.0
    with args.dataset.open(encoding="utf-8") as source, args.output.open(
        "w", encoding="utf-8", newline="\n"
    ) as target:
        for line in source:
            if not line.strip():
                continue
            record = json.loads(line)
            started = time.perf_counter()
            entities = detect(record["text"])
            runtime_ms = (time.perf_counter() - started) * 1000
            total_ms += runtime_ms
            prediction = {
                "id": record["id"],
                "entities": entities,
                "runtime_ms": runtime_ms,
                "structured_output_valid": True,
            }
            target.write(json.dumps(prediction, ensure_ascii=False) + "\n")
            count += 1

    print(f"records={count}")
    print(f"runtime_ms.total={total_ms:.3f}")
    print(f"runtime_ms.mean={total_ms / max(count, 1):.6f}")
    print(f"output={args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
