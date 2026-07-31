#!/usr/bin/env python3
"""Validate LlaMask JSONL evaluation data without third-party dependencies."""

from __future__ import annotations

import argparse
import collections
import json
from pathlib import Path


REQUIRED_FIELDS = {
    "id",
    "split",
    "language",
    "text",
    "entities",
    "tags",
    "difficulty",
    "policy",
}
VALID_SPLITS = {"dev", "test"}
VALID_LANGUAGES = {"zh-CN", "zh-TW", "mixed"}
VALID_DIFFICULTIES = {"easy", "normal", "hard"}


def validate(path: Path) -> tuple[int, list[str], collections.Counter[str]]:
    errors: list[str] = []
    labels: collections.Counter[str] = collections.Counter()
    ids: set[str] = set()
    texts: set[str] = set()
    records = 0

    with path.open(encoding="utf-8") as handle:
        for line_number, line in enumerate(handle, start=1):
            if not line.strip():
                errors.append(f"line {line_number}: blank line")
                continue
            try:
                record = json.loads(line)
            except json.JSONDecodeError as exc:
                errors.append(f"line {line_number}: invalid JSON: {exc}")
                continue
            records += 1
            missing = REQUIRED_FIELDS - record.keys()
            extra = record.keys() - REQUIRED_FIELDS
            if missing:
                errors.append(f"line {line_number}: missing fields {sorted(missing)}")
            if extra:
                errors.append(f"line {line_number}: unknown fields {sorted(extra)}")

            record_id = record.get("id")
            if not isinstance(record_id, str) or not record_id:
                errors.append(f"line {line_number}: invalid id")
            elif record_id in ids:
                errors.append(f"line {line_number}: duplicate id {record_id}")
            else:
                ids.add(record_id)

            text = record.get("text")
            if not isinstance(text, str) or not text:
                errors.append(f"line {line_number}: text must be non-empty")
                continue
            if text in texts:
                errors.append(f"line {line_number}: duplicate text")
            texts.add(text)

            if record.get("split") not in VALID_SPLITS:
                errors.append(f"line {line_number}: invalid split")
            if record.get("language") not in VALID_LANGUAGES:
                errors.append(f"line {line_number}: invalid language")
            if record.get("difficulty") not in VALID_DIFFICULTIES:
                errors.append(f"line {line_number}: invalid difficulty")
            if not isinstance(record.get("tags"), list) or not record["tags"]:
                errors.append(f"line {line_number}: tags must be a non-empty list")

            previous_start = -1
            for entity_index, entity in enumerate(record.get("entities", [])):
                prefix = f"line {line_number}, entity {entity_index}"
                try:
                    start = entity["start"]
                    end = entity["end"]
                    label = entity["label"]
                    entity_text = entity["text"]
                except (KeyError, TypeError):
                    errors.append(f"{prefix}: missing required entity fields")
                    continue
                if not isinstance(start, int) or not isinstance(end, int):
                    errors.append(f"{prefix}: offsets must be integers")
                    continue
                if not (0 <= start < end <= len(text)):
                    errors.append(f"{prefix}: offsets out of bounds")
                    continue
                if text[start:end] != entity_text:
                    errors.append(
                        f"{prefix}: span mismatch expected={entity_text!r} "
                        f"actual={text[start:end]!r}"
                    )
                if start < previous_start:
                    errors.append(f"{prefix}: entities must be sorted")
                previous_start = start
                if not isinstance(label, str) or not label:
                    errors.append(f"{prefix}: invalid label")
                else:
                    labels[label] += 1

    return records, errors, labels


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("dataset", type=Path)
    args = parser.parse_args()
    records, errors, labels = validate(args.dataset)
    print(f"records={records}")
    print(f"errors={len(errors)}")
    for label, count in sorted(labels.items()):
        print(f"label.{label}={count}")
    if errors:
        for error in errors[:50]:
            print(f"ERROR {error}")
        if len(errors) > 50:
            print(f"ERROR ... {len(errors) - 50} more")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
