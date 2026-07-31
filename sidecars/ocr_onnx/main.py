#!/usr/bin/env python3
"""One-shot, offline PP-OCRv6 sidecar for Llamask's image protocol."""

from __future__ import annotations

import argparse
import contextlib
import hashlib
import json
import os
import sys
from pathlib import Path
from typing import Any

MAX_REQUEST_BYTES = 64 * 1024
MAX_IMAGE_BYTES = 50 * 1024 * 1024
MAX_DIMENSION = 20_000
MAX_PIXELS = 100_000_000


def fail(message: str) -> "NoReturn":
    print(f"OCR sidecar failed: {message}", file=sys.stderr)
    raise SystemExit(1)


def read_request() -> dict[str, Any]:
    raw = sys.stdin.buffer.read(MAX_REQUEST_BYTES + 1)
    if len(raw) > MAX_REQUEST_BYTES:
        fail("request too large")
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError):
        fail("invalid request JSON")
    if not isinstance(value, dict):
        fail("request must be an object")
    return value


def sha256_file(path: Path) -> tuple[str, int]:
    digest = hashlib.sha256()
    size = 0
    with path.open("rb") as handle:
        while chunk := handle.read(1024 * 1024):
            size += len(chunk)
            if size > MAX_IMAGE_BYTES:
                fail("image exceeds size limit")
            digest.update(chunk)
    return digest.hexdigest(), size


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--tier", choices=["small", "medium"], required=True)
    parser.add_argument("--detector-model", type=Path, required=True)
    parser.add_argument("--recognizer-model", type=Path, required=True)
    args = parser.parse_args()

    detector_model = args.detector_model.resolve()
    recognizer_model = args.recognizer_model.resolve()
    for model in (detector_model, recognizer_model):
        if not (model / "inference.onnx").is_file():
            fail("pinned model is unavailable")

    cache_dir = detector_model.parent / ".paddlex"
    os.environ["PADDLE_PDX_CACHE_HOME"] = str(cache_dir)
    os.environ["PADDLE_PDX_DISABLE_MODEL_SOURCE_CHECK"] = "True"
    os.environ["HF_HUB_OFFLINE"] = "1"
    os.environ["TRANSFORMERS_OFFLINE"] = "1"
    os.environ["MODELSCOPE_OFFLINE"] = "1"

    request = read_request()
    if request.get("protocol_version") != 1:
        fail("unsupported protocol version")
    request_id = request.get("request_id")
    detector_id = request.get("detector_id")
    expected_sha256 = request.get("expected_sha256")
    image_path_value = request.get("image_path")
    if not all(isinstance(value, str) and value for value in (
        request_id,
        detector_id,
        expected_sha256,
        image_path_value,
    )):
        fail("missing request field")
    if len(expected_sha256) != 64 or any(
        char not in "0123456789abcdefABCDEF" for char in expected_sha256
    ):
        fail("invalid expected hash")

    image_path = Path(image_path_value)
    if not image_path.is_absolute() or not image_path.is_file():
        fail("image path is unavailable")
    actual_sha256, _ = sha256_file(image_path)
    if actual_sha256.lower() != expected_sha256.lower():
        fail("source hash mismatch")

    from PIL import Image, ImageOps

    try:
        with Image.open(image_path) as source:
            image_format = source.format
            normalized_image = ImageOps.exif_transpose(source).convert("RGB")
            width, height = normalized_image.size
    except Exception:
        fail("image decode failed")
    if image_format not in {"PNG", "JPEG"}:
        fail("unsupported image format")
    if (
        width <= 0
        or height <= 0
        or width > MAX_DIMENSION
        or height > MAX_DIMENSION
        or width * height > MAX_PIXELS
    ):
        fail("image dimensions exceed limits")

    # PaddleX announces model loading on stdout. Protocol stdout must contain JSON only.
    with contextlib.redirect_stdout(sys.stderr):
        from paddleocr import PaddleOCR

        ocr = PaddleOCR(
            text_detection_model_name=f"PP-OCRv6_{args.tier}_det",
            text_detection_model_dir=str(detector_model),
            text_recognition_model_name=f"PP-OCRv6_{args.tier}_rec",
            text_recognition_model_dir=str(recognizer_model),
            engine="onnxruntime",
            use_doc_orientation_classify=False,
            use_doc_unwarping=False,
            use_textline_orientation=False,
        )
        import numpy as np

        results = list(ocr.predict(np.asarray(normalized_image)))
    if len(results) != 1:
        fail("expected one image result")
    result = results[0].json["res"]
    rec_texts = [str(value) for value in result["rec_texts"]]
    rec_scores = [float(value) for value in result["rec_scores"]]
    rec_boxes = [list(map(int, value)) for value in result["rec_boxes"]]
    rec_polys = [
        [list(map(int, point)) for point in polygon]
        for polygon in result["rec_polys"]
    ]
    if not (len(rec_texts) == len(rec_scores) == len(rec_boxes) == len(rec_polys)):
        fail("inconsistent OCR arrays")

    response = {
        "protocol_version": 1,
        "request_id": request_id,
        "source_sha256": actual_sha256,
        "width": width,
        "height": height,
        "lines": [
            {
                "text": text,
                "score": score,
                "bbox": box,
                "polygon": polygon,
            }
            for text, score, box, polygon in zip(
                rec_texts, rec_scores, rec_boxes, rec_polys
            )
        ],
        "warnings": [],
    }
    sys.stdout.write(json.dumps(response, ensure_ascii=False, separators=(",", ":")))
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
