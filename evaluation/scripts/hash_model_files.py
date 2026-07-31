#!/usr/bin/env python3
"""Create a deterministic SHA-256 inventory for downloaded model files."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path


IGNORED_PARTS = {".cache", "__pycache__"}


def hash_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        while chunk := handle.read(8 * 1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("model_dir", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()

    files: list[dict[str, object]] = []
    for path in sorted(args.model_dir.rglob("*")):
        if not path.is_file() or any(part in IGNORED_PARTS for part in path.parts):
            continue
        files.append(
            {
                "path": path.relative_to(args.model_dir).as_posix(),
                "bytes": path.stat().st_size,
                "sha256": hash_file(path),
            }
        )

    inventory = {
        "model_dir": args.model_dir.as_posix(),
        "total_bytes": sum(int(item["bytes"]) for item in files),
        "files": files,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(
        json.dumps(inventory, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )
    print(f"files={len(files)}")
    print(f"total_bytes={inventory['total_bytes']}")
    print(f"output={args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
