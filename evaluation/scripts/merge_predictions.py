#!/usr/bin/env python3
"""Merge trusted rule spans with model spans, preferring safer rule labels."""

from __future__ import annotations

import argparse
import json
from pathlib import Path


def load_jsonl(path: Path) -> list[dict[str, object]]:
    with path.open(encoding="utf-8") as handle:
        return [json.loads(line) for line in handle if line.strip()]


def overlaps(left: dict[str, object], right: dict[str, object]) -> bool:
    return int(left["start"]) < int(right["end"]) and int(right["start"]) < int(
        left["end"]
    )


def merge_entities(
    rules: list[dict[str, object]],
    model: list[dict[str, object]],
) -> list[dict[str, object]]:
    merged = [dict(item) for item in rules]
    for candidate in model:
        if candidate["label"] == "CUSTOMER_NAME":
            merged = [
                item
                for item in merged
                if not (
                    overlaps(candidate, item)
                    and item["label"] == "ORG_NAME"
                    and item.get("detector") not in {"rules-v1", "context-rules-v1"}
                )
            ]
        if any(overlaps(candidate, trusted) for trusted in merged):
            continue
        key = (candidate["start"], candidate["end"], candidate["label"])
        if any((item["start"], item["end"], item["label"]) == key for item in merged):
            continue
        merged.append(dict(candidate))
    return sorted(merged, key=lambda item: (item["start"], item["end"], item["label"]))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("rule_predictions", type=Path)
    parser.add_argument("model_predictions", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()

    rules = {str(item["id"]): item for item in load_jsonl(args.rule_predictions)}
    models = {str(item["id"]): item for item in load_jsonl(args.model_predictions)}
    if not set(models) <= set(rules):
        raise ValueError("rule predictions are missing model record ids")

    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open("w", encoding="utf-8", newline="\n") as target:
        for record_id, model_record in models.items():
            rule_record = rules[record_id]
            target.write(
                json.dumps(
                    {
                        "id": record_id,
                        "entities": merge_entities(
                            list(rule_record.get("entities", [])),
                            list(model_record.get("entities", [])),
                        ),
                        "runtime_ms": float(rule_record.get("runtime_ms", 0.0))
                        + float(model_record.get("runtime_ms", 0.0)),
                        "structured_output_valid": bool(
                            rule_record.get("structured_output_valid", True)
                            and model_record.get("structured_output_valid", False)
                        ),
                    },
                    ensure_ascii=False,
                )
                + "\n"
            )
    print(f"records={len(models)}")
    print(f"output={args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
