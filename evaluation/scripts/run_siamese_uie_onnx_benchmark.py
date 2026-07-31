#!/usr/bin/env python3
"""Benchmark the pure ONNX SiameseUIE adapter."""

from __future__ import annotations

import argparse
import json
import sys
import time
from pathlib import Path

import psutil


PROJECT_ROOT = Path(__file__).resolve().parents[2]
if str(PROJECT_ROOT) not in sys.path:
    sys.path.insert(0, str(PROJECT_ROOT))

from sidecars.siamese_uie.main import normalize_findings  # noqa: E402
from sidecars.siamese_uie_onnx.engine import SiameseUieOnnxEngine  # noqa: E402


DEFAULT_MODEL = Path(
    "evaluation/models/cache/siamese-uie-chinese-base-onnx/model.onnx"
)
DEFAULT_VOCAB = Path(
    "evaluation/models/cache/siamese-uie-chinese-base/vocab.txt"
)


def load_jsonl(path: Path) -> list[dict[str, object]]:
    with path.open(encoding="utf-8") as handle:
        return [json.loads(line) for line in handle if line.strip()]


def benchmark_entities(
    text: str,
    raw: dict[str, object],
) -> list[dict[str, object]]:
    return [
        {
            "start": finding["start"],
            "end": finding["end"],
            "label": finding["entity_type"],
            "text": finding["matched_text"],
            "confidence": None,
            "detector": "siamese-uie-onnx-zero-shot",
        }
        for finding in normalize_findings(text, raw, 0.85)
    ]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("dataset", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--model", type=Path, default=DEFAULT_MODEL)
    parser.add_argument("--vocab", type=Path, default=DEFAULT_VOCAB)
    parser.add_argument("--split", choices=["dev", "test", "all"], default="dev")
    parser.add_argument("--limit", type=int)
    parser.add_argument("--intra-op-threads", type=int, default=0)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    records = load_jsonl(args.dataset)
    if args.split != "all":
        records = [
            record for record in records if record["split"] == args.split
        ]
    if args.limit is not None:
        records = records[: args.limit]

    load_started = time.perf_counter()
    engine = SiameseUieOnnxEngine(
        args.model.resolve(strict=True),
        args.vocab.resolve(strict=True),
        intra_op_threads=args.intra_op_threads,
    )
    load_ms = (time.perf_counter() - load_started) * 1000
    process = psutil.Process()
    peak_rss = process.memory_info().rss
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open("w", encoding="utf-8", newline="\n") as target:
        for index, record in enumerate(records, start=1):
            text = str(record["text"])
            started = time.perf_counter()
            raw = engine.extract(text)
            runtime_ms = (time.perf_counter() - started) * 1000
            peak_rss = max(peak_rss, process.memory_info().rss)
            target.write(
                json.dumps(
                    {
                        "id": record["id"],
                        "entities": benchmark_entities(text, raw),
                        "runtime_ms": runtime_ms,
                        "structured_output_valid": True,
                    },
                    ensure_ascii=False,
                )
                + "\n"
            )
            if index == 1 or index % 20 == 0 or index == len(records):
                print(
                    f"completed={index}/{len(records)} "
                    f"runtime_ms={runtime_ms:.1f}",
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
                "provider": "CPUExecutionProvider",
                "pure_onnx_runtime": True,
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
