#!/usr/bin/env python3
"""Benchmark the local SiameseUIE model on semantic entity classes."""

from __future__ import annotations

import argparse
import json
import os
import time
from pathlib import Path
from typing import Iterable

import psutil


DEFAULT_MODEL = Path("evaluation/models/cache/siamese-uie-chinese-base")
SCHEMA = {"人物": None, "组织机构": None}
TYPE_TO_LABEL = {"人物": "PERSON_NAME", "组织机构": "ORG_NAME"}
CUSTOMER_CUES = ("重点客户", "客户名单", "客户名称", "客户：", "客户:")


def load_jsonl(path: Path) -> list[dict[str, object]]:
    with path.open(encoding="utf-8") as handle:
        return [json.loads(line) for line in handle if line.strip()]


def flatten(items: object) -> Iterable[dict[str, object]]:
    if isinstance(items, dict):
        yield items
    elif isinstance(items, list):
        for item in items:
            yield from flatten(item)


def normalize_findings(text: str, raw: dict[str, object]) -> list[dict[str, object]]:
    findings: list[dict[str, object]] = []
    seen: set[tuple[int, int, str]] = set()
    customer_context = any(cue in text for cue in CUSTOMER_CUES)
    for item in flatten(raw.get("output", [])):
        source_type = str(item.get("type", ""))
        if source_type not in TYPE_TO_LABEL:
            continue
        offset = item.get("offset")
        if not isinstance(offset, list) or len(offset) != 2:
            continue
        start = int(offset[0])
        end = int(offset[1])
        if not (0 <= start < end <= len(text)):
            continue
        value = str(item.get("span", ""))
        if text[start:end] != value:
            continue
        label = TYPE_TO_LABEL[source_type]
        if label == "ORG_NAME" and customer_context:
            label = "CUSTOMER_NAME"
        key = (start, end, label)
        if key in seen:
            continue
        seen.add(key)
        findings.append(
            {
                "start": start,
                "end": end,
                "label": label,
                "text": value,
                "confidence": None,
                "detector": "siamese-uie-zero-shot",
            }
        )
    return sorted(findings, key=lambda item: (item["start"], item["end"], item["label"]))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("dataset", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--model", type=Path, default=DEFAULT_MODEL)
    parser.add_argument("--split", choices=["dev", "test", "all"], default="dev")
    parser.add_argument("--limit", type=int)
    args = parser.parse_args()

    cache_root = Path("evaluation/models/cache").resolve()
    os.environ["MODELSCOPE_CACHE"] = str(cache_root / ".modelscope")
    os.environ["HF_HOME"] = str(cache_root / ".huggingface")
    os.environ["HF_HUB_OFFLINE"] = "1"
    os.environ["TRANSFORMERS_OFFLINE"] = "1"

    from modelscope.pipelines import pipeline
    from modelscope.utils.constant import Tasks

    records = load_jsonl(args.dataset)
    if args.split != "all":
        records = [record for record in records if record["split"] == args.split]
    if args.limit is not None:
        records = records[: args.limit]

    load_started = time.perf_counter()
    extractor = pipeline(
        Tasks.siamese_uie,
        model=str(args.model),
        trust_remote_code=True,
    )
    load_ms = (time.perf_counter() - load_started) * 1000
    process = psutil.Process()
    peak_rss = process.memory_info().rss
    args.output.parent.mkdir(parents=True, exist_ok=True)

    with args.output.open("w", encoding="utf-8", newline="\n") as target:
        for index, record in enumerate(records, start=1):
            text = str(record["text"])
            started = time.perf_counter()
            raw = extractor(input=text, schema=SCHEMA)
            runtime_ms = (time.perf_counter() - started) * 1000
            entities = normalize_findings(text, raw)
            rss = process.memory_info().rss
            peak_rss = max(peak_rss, rss)
            target.write(
                json.dumps(
                    {
                        "id": record["id"],
                        "entities": entities,
                        "runtime_ms": runtime_ms,
                        "structured_output_valid": True,
                    },
                    ensure_ascii=False,
                )
                + "\n"
            )
            if index == 1 or index % 20 == 0 or index == len(records):
                print(
                    f"completed={index}/{len(records)} runtime_ms={runtime_ms:.1f}",
                    flush=True,
                )

    runtime_path = args.output.with_suffix(".runtime.json")
    runtime_path.write_text(
        json.dumps(
            {
                "model": str(args.model),
                "records": len(records),
                "model_load_ms": load_ms,
                "peak_rss_bytes": peak_rss,
                "peak_rss_gb": peak_rss / 1024**3,
                "offline_mode": True,
                "schema": SCHEMA,
            },
            ensure_ascii=False,
            indent=2,
        )
        + "\n",
        encoding="utf-8",
    )
    print(f"output={args.output}")
    print(f"runtime={runtime_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
