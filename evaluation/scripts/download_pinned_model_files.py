#!/usr/bin/env python3
"""Download pinned public model files over resumable HTTPS.

This path avoids environment-specific Xet client behavior while still using
immutable Hugging Face revision URLs.
"""

from __future__ import annotations

import argparse
import os
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass
from pathlib import Path

import requests


MODEL_ROOT = Path("evaluation/models/cache")


@dataclass(frozen=True)
class Download:
    group: str
    repo: str
    revision: str
    filename: str
    destination: Path
    expected_bytes: int

    @property
    def url(self) -> str:
        return (
            f"https://huggingface.co/{self.repo}/resolve/"
            f"{self.revision}/{self.filename}"
        )


def item(
    group: str,
    repo: str,
    revision: str,
    filename: str,
    destination_dir: str,
    expected_bytes: int,
) -> Download:
    return Download(
        group=group,
        repo=repo,
        revision=revision,
        filename=filename,
        destination=MODEL_ROOT / destination_dir / filename,
        expected_bytes=expected_bytes,
    )


DOWNLOADS = [
    item(
        "qwen",
        "Qwen/Qwen3.5-4B",
        "851bf6e806efd8d0a36b00ddf55e13ccb7b8cd0a",
        "model.safetensors-00001-of-00002.safetensors",
        "qwen3.5-4b-bf16",
        5_329_398_688,
    ),
    item(
        "qwen",
        "Qwen/Qwen3.5-4B",
        "851bf6e806efd8d0a36b00ddf55e13ccb7b8cd0a",
        "model.safetensors-00002-of-00002.safetensors",
        "qwen3.5-4b-bf16",
        3_990_429_408,
    ),
    item(
        "qwen",
        "Qwen/Qwen3.5-4B",
        "851bf6e806efd8d0a36b00ddf55e13ccb7b8cd0a",
        "tokenizer.json",
        "qwen3.5-4b-bf16",
        12_807_982,
    ),
    item(
        "small",
        "PaddlePaddle/PP-OCRv6_small_det_onnx",
        "28fe5895c24fd108c19eb3e8479f4ab385fbfc62",
        "inference.onnx",
        "pp-ocrv6-small-det-onnx",
        9_880_512,
    ),
    item(
        "small",
        "PaddlePaddle/PP-OCRv6_small_rec_onnx",
        "b8f84f0b80c529de40b4fbb3544b84fa7233a513",
        "inference.onnx",
        "pp-ocrv6-small-rec-onnx",
        21_159_378,
    ),
    item(
        "small",
        "PaddlePaddle/PP-OCRv6_medium_det_onnx",
        "61323801669c338b7891481ec7bac61ce31b576a",
        "inference.onnx",
        "pp-ocrv6-medium-det-onnx",
        62_032_837,
    ),
    item(
        "small",
        "PaddlePaddle/PP-OCRv6_medium_rec_onnx",
        "50c7eacafc52fa7bcf4194e8cd08e46f8558504b",
        "inference.onnx",
        "pp-ocrv6-medium-rec-onnx",
        76_554_979,
    ),
    item(
        "small",
        "PaddlePaddle/PP-OCRv6_medium_rec_onnx",
        "50c7eacafc52fa7bcf4194e8cd08e46f8558504b",
        "inference.yml",
        "pp-ocrv6-medium-rec-onnx",
        150_580,
    ),
    item(
        "small",
        "PaddlePaddle/PP-DocLayoutV3_safetensors",
        "97d101e6db2642e162a1d05392d1b0231c91033e",
        "model.safetensors",
        "pp-doclayoutv3-safetensors",
        133_270_468,
    ),
    item(
        "small",
        "PaddlePaddle/PP-DocLayoutV3_safetensors",
        "97d101e6db2642e162a1d05392d1b0231c91033e",
        "config.json",
        "pp-doclayoutv3-safetensors",
        2_460,
    ),
    item(
        "small",
        "PaddlePaddle/PP-DocLayoutV3_safetensors",
        "97d101e6db2642e162a1d05392d1b0231c91033e",
        "preprocessor_config.json",
        "pp-doclayoutv3-safetensors",
        575,
    ),
]


def download(entry: Download) -> str:
    entry.destination.parent.mkdir(parents=True, exist_ok=True)
    if entry.destination.exists():
        size = entry.destination.stat().st_size
        if size == entry.expected_bytes:
            return f"skipped={entry.destination} bytes={size}"
        raise RuntimeError(
            f"Existing file has wrong size: {entry.destination} "
            f"{size} != {entry.expected_bytes}"
        )

    partial = entry.destination.with_suffix(entry.destination.suffix + ".part")
    offset = partial.stat().st_size if partial.exists() else 0
    if offset > entry.expected_bytes:
        raise RuntimeError(f"Partial file is too large: {partial}")

    headers = {"Range": f"bytes={offset}-"} if offset else {}
    with requests.get(
        entry.url,
        headers=headers,
        stream=True,
        allow_redirects=True,
        timeout=(30, 120),
    ) as response:
        response.raise_for_status()
        if offset and response.status_code != 206:
            offset = 0
            mode = "wb"
        else:
            mode = "ab" if offset else "wb"

        started = time.monotonic()
        last_report = started
        with partial.open(mode) as handle:
            for chunk in response.iter_content(chunk_size=8 * 1024 * 1024):
                if not chunk:
                    continue
                handle.write(chunk)
                now = time.monotonic()
                if now - last_report >= 15:
                    current = handle.tell()
                    elapsed = max(now - started, 0.001)
                    transferred = current - offset
                    speed_mib = transferred / elapsed / 1024 / 1024
                    print(
                        f"progress={entry.destination.name} "
                        f"bytes={current}/{entry.expected_bytes} "
                        f"speed_mib_s={speed_mib:.2f}",
                        flush=True,
                    )
                    last_report = now
            handle.flush()
            os.fsync(handle.fileno())

    actual_bytes = partial.stat().st_size
    if actual_bytes != entry.expected_bytes:
        raise RuntimeError(
            f"Downloaded size mismatch for {entry.destination}: "
            f"{actual_bytes} != {entry.expected_bytes}"
        )
    partial.replace(entry.destination)
    return f"downloaded={entry.destination} bytes={actual_bytes}"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--group", choices=["qwen", "small", "all"], default="all")
    parser.add_argument("--workers", type=int, default=2)
    args = parser.parse_args()

    selected = [
        entry for entry in DOWNLOADS if args.group == "all" or entry.group == args.group
    ]
    failures: list[str] = []
    with ThreadPoolExecutor(max_workers=args.workers) as executor:
        futures = {executor.submit(download, entry): entry for entry in selected}
        for future in as_completed(futures):
            entry = futures[future]
            try:
                print(future.result(), flush=True)
            except Exception as exc:
                message = f"failed={entry.destination} error={exc}"
                failures.append(message)
                print(message, flush=True)
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
