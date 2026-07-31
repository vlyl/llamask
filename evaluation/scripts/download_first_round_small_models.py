#!/usr/bin/env python3
"""Download pinned OCR and layout snapshots used in the first benchmark."""

from __future__ import annotations

from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

from huggingface_hub import snapshot_download


MODEL_ROOT = Path("evaluation/models/cache")
DOWNLOADS = [
    (
        "PaddlePaddle/PP-OCRv6_small_det_onnx",
        "28fe5895c24fd108c19eb3e8479f4ab385fbfc62",
        MODEL_ROOT / "pp-ocrv6-small-det-onnx",
    ),
    (
        "PaddlePaddle/PP-OCRv6_small_rec_onnx",
        "b8f84f0b80c529de40b4fbb3544b84fa7233a513",
        MODEL_ROOT / "pp-ocrv6-small-rec-onnx",
    ),
    (
        "PaddlePaddle/PP-OCRv6_medium_det_onnx",
        "61323801669c338b7891481ec7bac61ce31b576a",
        MODEL_ROOT / "pp-ocrv6-medium-det-onnx",
    ),
    (
        "PaddlePaddle/PP-OCRv6_medium_rec_onnx",
        "50c7eacafc52fa7bcf4194e8cd08e46f8558504b",
        MODEL_ROOT / "pp-ocrv6-medium-rec-onnx",
    ),
    (
        "PaddlePaddle/PP-DocLayoutV3_safetensors",
        "97d101e6db2642e162a1d05392d1b0231c91033e",
        MODEL_ROOT / "pp-doclayoutv3-safetensors",
    ),
]


def download(item: tuple[str, str, Path]) -> tuple[str, str]:
    repo_id, revision, destination = item
    destination.mkdir(parents=True, exist_ok=True)
    path = snapshot_download(
        repo_id=repo_id,
        revision=revision,
        local_dir=destination,
        max_workers=2,
    )
    return repo_id, path


def main() -> int:
    failures: list[str] = []
    with ThreadPoolExecutor(max_workers=3) as executor:
        futures = {executor.submit(download, item): item[0] for item in DOWNLOADS}
        for future in as_completed(futures):
            repo_id = futures[future]
            try:
                completed_repo, path = future.result()
                print(f"downloaded={completed_repo} path={path}", flush=True)
            except Exception as exc:
                failures.append(f"{repo_id}: {exc}")
                print(f"failed={repo_id} error={exc}", flush=True)
    if failures:
        print("\n".join(failures))
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
