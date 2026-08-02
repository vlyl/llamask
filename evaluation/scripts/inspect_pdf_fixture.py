#!/usr/bin/env python3
"""Inspect PDF logical structures without printing synthetic sensitive values."""

from __future__ import annotations

import argparse
import json
from collections import Counter
from pathlib import Path

import pdfplumber
from pypdf import PdfReader


def inspect(path: Path) -> dict[str, object]:
    reader = PdfReader(path)
    fields = reader.get_fields() or {}
    annotation_types: Counter[str] = Counter()
    widgets = 0
    for page in reader.pages:
        for reference in page.get("/Annots", []):
            annotation = reference.get_object()
            subtype = str(annotation.get("/Subtype", "unknown"))
            annotation_types[subtype] += 1
            if subtype == "/Widget":
                widgets += 1
    attachments = reader.attachments
    with pdfplumber.open(path) as document:
        extracted_char_counts = [len(page.extract_text() or "") for page in document.pages]
    raw = path.read_bytes()
    return {
        "pages": len(reader.pages),
        "encrypted": reader.is_encrypted,
        "field_count": len(fields),
        "field_names": sorted(fields),
        "widget_count": widgets,
        "annotation_types": dict(sorted(annotation_types.items())),
        "attachment_names": sorted(attachments),
        "has_javascript": b"/JavaScript" in raw or b"/JS" in raw,
        "metadata_keys": sorted(str(key) for key in (reader.metadata or {})),
        "extracted_char_counts": extracted_char_counts,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("input", type=Path)
    args = parser.parse_args()
    print(json.dumps(inspect(args.input), ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
