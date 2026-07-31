#!/usr/bin/env python3
"""Download a pinned-by-content official ModelScope mirror snapshot."""

from __future__ import annotations

import argparse
from pathlib import Path

from modelscope.hub.snapshot_download import snapshot_download


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("model_id")
    parser.add_argument("revision")
    parser.add_argument("destination", type=Path)
    parser.add_argument("--allow", action="append", default=[])
    parser.add_argument("--ignore", action="append", default=[])
    parser.add_argument("--max-workers", type=int, default=4)
    args = parser.parse_args()

    args.destination.mkdir(parents=True, exist_ok=True)
    path = snapshot_download(
        model_id=args.model_id,
        revision=args.revision,
        local_dir=str(args.destination),
        allow_file_pattern=args.allow or None,
        ignore_file_pattern=args.ignore or None,
        max_workers=args.max_workers,
    )
    print(f"model_id={args.model_id}")
    print(f"revision={args.revision}")
    print(f"path={path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
