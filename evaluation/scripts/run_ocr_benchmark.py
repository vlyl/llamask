#!/usr/bin/env python3
"""Run PP-OCRv6 through local ONNX Runtime with no model-host checks."""

from __future__ import annotations

import argparse
import json
import os
import time
from pathlib import Path

import psutil


def load_jsonl(path: Path) -> list[dict[str, object]]:
    records: list[dict[str, object]] = []
    with path.open(encoding="utf-8") as handle:
        for line in handle:
            if line.strip():
                records.append(json.loads(line))
    return records


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("manifest", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--tier", choices=["small", "medium"], required=True)
    parser.add_argument("--model-root", type=Path, default=Path("evaluation/models/cache"))
    parser.add_argument("--limit", type=int)
    args = parser.parse_args()

    cache_dir = args.model_root / ".paddlex"
    os.environ["PADDLE_PDX_CACHE_HOME"] = str(cache_dir.resolve())
    os.environ["PADDLE_PDX_DISABLE_MODEL_SOURCE_CHECK"] = "True"

    from paddleocr import PaddleOCR

    detector_name = f"PP-OCRv6_{args.tier}_det"
    recognizer_name = f"PP-OCRv6_{args.tier}_rec"
    detector_dir = args.model_root / f"pp-ocrv6-{args.tier}-det-onnx"
    recognizer_dir = args.model_root / f"pp-ocrv6-{args.tier}-rec-onnx"
    for required in [detector_dir / "inference.onnx", recognizer_dir / "inference.onnx"]:
        if not required.exists():
            raise FileNotFoundError(required)

    records = load_jsonl(args.manifest)
    if args.limit is not None:
        records = records[: args.limit]
    args.output.parent.mkdir(parents=True, exist_ok=True)

    load_started = time.perf_counter()
    ocr = PaddleOCR(
        text_detection_model_name=detector_name,
        text_detection_model_dir=str(detector_dir),
        text_recognition_model_name=recognizer_name,
        text_recognition_model_dir=str(recognizer_dir),
        engine="onnxruntime",
        use_doc_orientation_classify=False,
        use_doc_unwarping=False,
        use_textline_orientation=False,
    )
    load_ms = (time.perf_counter() - load_started) * 1000
    process = psutil.Process()
    peak_rss = process.memory_info().rss

    with args.output.open("w", encoding="utf-8", newline="\n") as target:
        for index, record in enumerate(records, start=1):
            started = time.perf_counter()
            results = list(ocr.predict(str(record["image"])))
            runtime_ms = (time.perf_counter() - started) * 1000
            if len(results) != 1:
                raise RuntimeError(f"{record['id']}: expected one page result")
            result = results[0].json["res"]
            rec_texts = [str(value) for value in result["rec_texts"]]
            rec_scores = [float(value) for value in result["rec_scores"]]
            rec_boxes = [list(map(int, value)) for value in result["rec_boxes"]]
            rec_polys = [
                [list(map(int, point)) for point in polygon]
                for polygon in result["rec_polys"]
            ]
            lines = [
                {
                    "text": text,
                    "score": score,
                    "bbox": box,
                    "polygon": polygon,
                }
                for text, score, box, polygon in zip(
                    rec_texts, rec_scores, rec_boxes, rec_polys
                )
            ]
            rss = process.memory_info().rss
            peak_rss = max(peak_rss, rss)
            prediction = {
                "id": record["id"],
                "recognized_text": "\n".join(rec_texts),
                "lines": lines,
                "runtime_ms": runtime_ms,
                "rss_bytes": rss,
            }
            target.write(json.dumps(prediction, ensure_ascii=False) + "\n")
            if index == 1 or index % 20 == 0 or index == len(records):
                print(
                    f"tier={args.tier} completed={index}/{len(records)} "
                    f"runtime_ms={runtime_ms:.1f}",
                    flush=True,
                )

    runtime_summary = args.output.with_suffix(".runtime.json")
    runtime_summary.write_text(
        json.dumps(
            {
                "tier": args.tier,
                "records": len(records),
                "model_load_ms": load_ms,
                "peak_rss_bytes": peak_rss,
                "peak_rss_gb": peak_rss / 1024**3,
                "network_host_check_disabled": True,
                "engine": "onnxruntime",
            },
            ensure_ascii=False,
            indent=2,
        )
        + "\n",
        encoding="utf-8",
    )
    print(f"output={args.output}")
    print(f"runtime={runtime_summary}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
