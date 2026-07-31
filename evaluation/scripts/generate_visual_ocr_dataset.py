#!/usr/bin/env python3
"""Render deterministic synthetic Chinese OCR pages with coordinate labels."""

from __future__ import annotations

import argparse
import json
import random
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFilter, ImageFont


SEED = 20260731
DEFAULT_SOURCE = Path("evaluation/datasets/generated/synthetic_zh_v2.jsonl")
DEFAULT_OUTPUT_DIR = Path("evaluation/datasets/generated/ocr_zh_v2")
DEFAULT_MANIFEST = Path("evaluation/datasets/generated/ocr_zh_v2.jsonl")
CHINESE_FONTS = [
    Path("/System/Library/Fonts/Hiragino Sans GB.ttc"),
    Path("/System/Library/Fonts/STHeiti Medium.ttc"),
    Path("/System/Library/Fonts/Supplemental/Songti.ttc"),
]


def load_cases(path: Path, count: int) -> list[dict[str, object]]:
    all_cases: list[dict[str, object]] = []
    with path.open(encoding="utf-8") as handle:
        for line in handle:
            if line.strip():
                all_cases.append(json.loads(line))
    positives = [case for case in all_cases if case["entities"]]
    negatives = [case for case in all_cases if not case["entities"]]
    selected: list[dict[str, object]] = []
    positive_target = min(len(positives), int(count * 0.8))
    negative_target = min(len(negatives), count - positive_target)
    selected.extend(positives[:positive_target])
    selected.extend(negatives[:negative_target])
    return selected[:count]


def wrap_with_offsets(text: str, max_chars: int) -> list[tuple[str, int, int]]:
    lines: list[tuple[str, int, int]] = []
    line_start = 0
    current: list[str] = []
    for index, char in enumerate(text):
        if char == "\n" or len(current) >= max_chars:
            if current:
                value = "".join(current)
                lines.append((value, line_start, line_start + len(value)))
            current = []
            line_start = index + 1 if char == "\n" else index
        if char != "\n":
            current.append(char)
    if current:
        value = "".join(current)
        lines.append((value, line_start, line_start + len(value)))
    return lines


def add_noise(image: Image.Image, rng: np.random.Generator, sigma: float) -> Image.Image:
    array = np.asarray(image).astype(np.float32)
    noise = rng.normal(0.0, sigma, array.shape)
    return Image.fromarray(np.clip(array + noise, 0, 255).astype(np.uint8), mode="RGB")


def render_case(
    case: dict[str, object],
    output_path: Path,
    style: str,
    font_path: Path,
    font_size: int,
    rng: np.random.Generator,
) -> dict[str, object]:
    width = 1280
    height = 720
    background = (248, 248, 246)
    foreground = (24, 24, 24)
    if style == "low_contrast":
        background = (224, 226, 224)
        foreground = (120, 122, 120)
    elif style == "colored_background":
        background = (226, 238, 247)
        foreground = (20, 52, 76)

    image = Image.new("RGB", (width, height), background)
    draw = ImageDraw.Draw(image)
    font = ImageFont.truetype(str(font_path), font_size)
    x = 64
    y = 54
    line_gap = max(10, font_size // 3)
    lines = wrap_with_offsets(str(case["text"]), 28 if font_size >= 30 else 34)
    line_regions: list[dict[str, object]] = []
    sensitive_regions: list[dict[str, object]] = []

    if style == "screen_photo":
        for grid_x in range(0, width, 4):
            draw.line((grid_x, 0, grid_x, height), fill=(238, 238, 236), width=1)

    for line_text, line_start, line_end in lines:
        bbox = draw.textbbox((x, y), line_text, font=font)
        draw.text((x, y), line_text, font=font, fill=foreground)
        line_regions.append(
            {
                "text": line_text,
                "start": line_start,
                "end": line_end,
                "bbox": list(bbox),
            }
        )
        for entity in case["entities"]:
            entity_start = int(entity["start"])
            entity_end = int(entity["end"])
            overlap_start = max(entity_start, line_start)
            overlap_end = min(entity_end, line_end)
            if overlap_start >= overlap_end:
                continue
            prefix = str(case["text"])[line_start:overlap_start]
            fragment = str(case["text"])[overlap_start:overlap_end]
            fragment_x0 = x + draw.textlength(prefix, font=font)
            fragment_x1 = fragment_x0 + draw.textlength(fragment, font=font)
            sensitive_regions.append(
                {
                    "text": fragment,
                    "label": entity["label"],
                    "start": overlap_start,
                    "end": overlap_end,
                    "bbox": [
                        round(fragment_x0),
                        bbox[1],
                        round(fragment_x1),
                        bbox[3],
                    ],
                }
            )
        y = bbox[3] + line_gap

    if style == "blur":
        image = image.filter(ImageFilter.GaussianBlur(radius=1.2))
    elif style == "low_resolution":
        image = image.resize(
            (width // 2, height // 2), Image.Resampling.BILINEAR
        ).resize((width, height), Image.Resampling.BILINEAR)
    elif style == "jpeg_noise":
        image = add_noise(image, rng, sigma=5.0)

    output_path.parent.mkdir(parents=True, exist_ok=True)
    if style == "jpeg_noise":
        image.save(output_path.with_suffix(".jpg"), quality=58, optimize=True)
        final_path = output_path.with_suffix(".jpg")
    else:
        image.save(output_path, optimize=True)
        final_path = output_path

    normalized_text = "".join(char for char in str(case["text"]) if not char.isspace())
    return {
        "id": f"ocr-{case['id']}",
        "source_id": case["id"],
        "image": final_path.as_posix(),
        "text": case["text"],
        "normalized_text": normalized_text,
        "style": style,
        "font": font_path.name,
        "font_size": font_size,
        "width": width,
        "height": height,
        "line_regions": line_regions,
        "sensitive_regions": sensitive_regions,
        "tags": list(case["tags"]) + ["synthetic_ocr", style],
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", type=Path, default=DEFAULT_SOURCE)
    parser.add_argument("--output-dir", type=Path, default=DEFAULT_OUTPUT_DIR)
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument("--count", type=int, default=240)
    args = parser.parse_args()

    fonts = [path for path in CHINESE_FONTS if path.exists()]
    if not fonts:
        raise RuntimeError("No supported Chinese font was found")
    styles = [
        "clean",
        "low_contrast",
        "blur",
        "low_resolution",
        "colored_background",
        "screen_photo",
        "jpeg_noise",
    ]
    font_sizes = [26, 30, 34, 40]
    cases = load_cases(args.source, args.count)
    py_rng = random.Random(SEED)
    np_rng = np.random.default_rng(SEED)
    args.manifest.parent.mkdir(parents=True, exist_ok=True)

    with args.manifest.open("w", encoding="utf-8", newline="\n") as handle:
        for index, case in enumerate(cases, start=1):
            style = styles[(index - 1) % len(styles)]
            font_path = fonts[(index - 1) % len(fonts)]
            font_size = font_sizes[py_rng.randrange(len(font_sizes))]
            output_path = args.output_dir / f"ocr-{index:04d}.png"
            record = render_case(
                case,
                output_path,
                style,
                font_path,
                font_size,
                np_rng,
            )
            handle.write(json.dumps(record, ensure_ascii=False) + "\n")

    print(f"records={len(cases)}")
    print(f"manifest={args.manifest}")
    print(f"images={args.output_dir}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
