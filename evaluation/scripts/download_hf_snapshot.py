#!/usr/bin/env python3
"""Download a pinned Hugging Face snapshot into the local model cache."""

from __future__ import annotations

import argparse
from pathlib import Path

from huggingface_hub import snapshot_download


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("repo_id")
    parser.add_argument("revision")
    parser.add_argument("destination", type=Path)
    parser.add_argument("--allow", action="append", default=[])
    parser.add_argument("--max-workers", type=int, default=4)
    args = parser.parse_args()

    args.destination.mkdir(parents=True, exist_ok=True)
    path = snapshot_download(
        repo_id=args.repo_id,
        revision=args.revision,
        local_dir=args.destination,
        allow_patterns=args.allow or None,
        max_workers=args.max_workers,
    )
    print(f"repo_id={args.repo_id}")
    print(f"revision={args.revision}")
    print(f"path={path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
