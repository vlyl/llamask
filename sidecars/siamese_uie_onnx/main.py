#!/usr/bin/env python3
"""LlaMask protocol adapter for the pure ONNX SiameseUIE runtime."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path


PROJECT_ROOT = Path(__file__).resolve().parents[2]
if str(PROJECT_ROOT) not in sys.path:
    sys.path.insert(0, str(PROJECT_ROOT))

from sidecars.siamese_uie.main import (  # noqa: E402
    PROTOCOL_VERSION,
    configure_offline_environment,
    normalize_findings,
    read_request,
)
from sidecars.siamese_uie_onnx.engine import SiameseUieOnnxEngine  # noqa: E402


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--model", type=Path, required=True)
    parser.add_argument("--vocab", type=Path, required=True)
    parser.add_argument("--detector-id", default="siamese_uie")
    parser.add_argument("--max-chars", type=int, default=20_000)
    parser.add_argument("--operational-confidence", type=float, default=0.85)
    parser.add_argument("--intra-op-threads", type=int, default=0)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.max_chars <= 0:
        raise ValueError("max chars must be positive")
    if not 0.0 <= args.operational_confidence <= 1.0:
        raise ValueError("operational confidence must be between 0 and 1")
    if args.intra_op_threads < 0:
        raise ValueError("intra-op threads cannot be negative")
    model_path = args.model.resolve(strict=True)
    vocab_path = args.vocab.resolve(strict=True)
    if not model_path.is_file() or not vocab_path.is_file():
        raise ValueError("model and vocabulary must be files")
    configure_offline_environment(model_path.parent)
    request = read_request(args.detector_id, args.max_chars)
    engine = SiameseUieOnnxEngine(
        model_path,
        vocab_path,
        intra_op_threads=args.intra_op_threads,
    )
    text = str(request["text"])
    raw = engine.extract(text)
    response = {
        "protocol_version": PROTOCOL_VERSION,
        "request_id": request["request_id"],
        "findings": normalize_findings(
            text,
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
        print(f"siamese_onnx_sidecar_error={type(error).__name__}", file=sys.stderr)
        raise SystemExit(1) from None
