#!/usr/bin/env python3
"""Generate deterministic OCR stress images with document-like artifacts."""

from __future__ import annotations

import argparse
import json
import random
from pathlib import Path

import cv2
import numpy as np
from PIL import Image, ImageDraw, ImageEnhance, ImageFilter, ImageFont

from generate_visual_ocr_dataset import CHINESE_FONTS, load_cases, render_case


SEED = 20260801
DEFAULT_SOURCE = Path("evaluation/datasets/generated/synthetic_zh_v2.jsonl")
DEFAULT_OUTPUT_DIR = Path("evaluation/datasets/generated/ocr_zh_hard_v2")
DEFAULT_MANIFEST = Path("evaluation/datasets/generated/ocr_zh_hard_v2.jsonl")
WIDTH = 1280
HEIGHT = 720


def transform_bbox(bbox: list[int], matrix: np.ndarray) -> list[int]:
    points = np.array(
        [
            [bbox[0], bbox[1]],
            [bbox[2], bbox[1]],
            [bbox[2], bbox[3]],
            [bbox[0], bbox[3]],
        ],
        dtype=np.float32,
    ).reshape(1, 4, 2)
    transformed = cv2.perspectiveTransform(points, matrix)[0]
    x0 = max(0, int(np.floor(transformed[:, 0].min())))
    y0 = max(0, int(np.floor(transformed[:, 1].min())))
    x1 = min(WIDTH, int(np.ceil(transformed[:, 0].max())))
    y1 = min(HEIGHT, int(np.ceil(transformed[:, 1].max())))
    return [x0, y0, x1, y1]


def warp(
    image: Image.Image,
    record: dict[str, object],
    destination: np.ndarray,
) -> Image.Image:
    source = np.float32(
        [[0, 0], [WIDTH - 1, 0], [WIDTH - 1, HEIGHT - 1], [0, HEIGHT - 1]]
    )
    matrix = cv2.getPerspectiveTransform(source, destination.astype(np.float32))
    array = cv2.cvtColor(np.asarray(image), cv2.COLOR_RGB2BGR)
    warped = cv2.warpPerspective(
        array,
        matrix,
        (WIDTH, HEIGHT),
        borderMode=cv2.BORDER_CONSTANT,
        borderValue=(205, 205, 205),
    )
    for region_name in ("line_regions", "sensitive_regions"):
        for region in record[region_name]:
            region["bbox"] = transform_bbox(region["bbox"], matrix)
    return Image.fromarray(cv2.cvtColor(warped, cv2.COLOR_BGR2RGB))


def add_shadow(image: Image.Image) -> Image.Image:
    array = np.asarray(image).astype(np.float32)
    x_axis = np.linspace(0.62, 1.03, WIDTH, dtype=np.float32)
    y_axis = np.linspace(0.94, 1.02, HEIGHT, dtype=np.float32)
    shade = np.outer(y_axis, x_axis)[..., None]
    return Image.fromarray(np.clip(array * shade, 0, 255).astype(np.uint8))


def add_scan_noise(image: Image.Image, rng: np.random.Generator) -> Image.Image:
    array = np.asarray(image).astype(np.float32)
    noise = rng.normal(0.0, 10.0, array.shape)
    array = np.clip(array + noise, 0, 255).astype(np.uint8)
    result = Image.fromarray(array).filter(ImageFilter.GaussianBlur(0.65))
    result = ImageEnhance.Contrast(result).enhance(0.68)
    draw = ImageDraw.Draw(result)
    for y in range(12, HEIGHT, 19):
        draw.line((0, y, WIDTH, y), fill=(214, 214, 214), width=1)
    return result


def add_table_grid(image: Image.Image) -> Image.Image:
    draw = ImageDraw.Draw(image)
    color = (105, 105, 105)
    for x in (42, 360, 720, 1010, 1240):
        draw.line((x, 28, x, 420), fill=color, width=2)
    for y in range(28, 421, 49):
        draw.line((42, y, 1240, y), fill=color, width=2)
    return image


def add_stamp(image: Image.Image, font_path: Path) -> Image.Image:
    overlay = Image.new("RGBA", image.size, (0, 0, 0, 0))
    draw = ImageDraw.Draw(overlay)
    draw.ellipse((4, -36, 760, 224), outline=(200, 25, 25, 112), width=12)
    font = ImageFont.truetype(str(font_path), 58)
    draw.text((178, 55), "内部资料", font=font, fill=(200, 25, 25, 98))
    rotated = overlay.rotate(-5, resample=Image.Resampling.BICUBIC)
    return Image.alpha_composite(image.convert("RGBA"), rotated).convert("RGB")


def degrade_resolution(image: Image.Image) -> Image.Image:
    return image.resize((426, 240), Image.Resampling.BILINEAR).resize(
        (WIDTH, HEIGHT), Image.Resampling.BILINEAR
    )


def process_style(
    image: Image.Image,
    record: dict[str, object],
    style: str,
    font_path: Path,
    rng: np.random.Generator,
) -> Image.Image:
    if style == "perspective_shadow":
        destination = np.float32([[34, 32], [1247, 8], [1272, 686], [9, 715]])
        return add_shadow(warp(image, record, destination))
    if style == "faint_scan":
        return add_scan_noise(image, rng)
    if style == "table_grid":
        return add_table_grid(image)
    if style == "stamp_overlap":
        return add_stamp(image, font_path)
    if style == "mobile_photo":
        destination = np.float32([[94, 3], [1199, 62], [1270, 670], [22, 704]])
        return add_shadow(warp(image, record, destination)).filter(
            ImageFilter.GaussianBlur(0.85)
        )
    if style == "aggressive_jpeg":
        return ImageEnhance.Sharpness(add_scan_noise(image, rng)).enhance(0.45)
    if style == "downsampled_small_text":
        return degrade_resolution(image)
    if style == "mixed_artifacts":
        destination = np.float32([[51, 45], [1233, 18], [1274, 691], [18, 703]])
        image = warp(image, record, destination)
        return degrade_resolution(add_shadow(add_scan_noise(image, rng)))
    raise ValueError(style)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", type=Path, default=DEFAULT_SOURCE)
    parser.add_argument("--output-dir", type=Path, default=DEFAULT_OUTPUT_DIR)
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument("--count", type=int, default=320)
    args = parser.parse_args()

    fonts = [path for path in CHINESE_FONTS if path.exists()]
    if not fonts:
        raise RuntimeError("No supported Chinese font was found")
    styles = [
        "perspective_shadow",
        "faint_scan",
        "table_grid",
        "stamp_overlap",
        "mobile_photo",
        "aggressive_jpeg",
        "downsampled_small_text",
        "mixed_artifacts",
    ]
    font_sizes = [18, 20, 22, 24, 28]
    cases = load_cases(args.source, args.count)
    py_rng = random.Random(SEED)
    np_rng = np.random.default_rng(SEED)
    args.output_dir.mkdir(parents=True, exist_ok=True)
    args.manifest.parent.mkdir(parents=True, exist_ok=True)

    with args.manifest.open("w", encoding="utf-8", newline="\n") as handle:
        for index, case in enumerate(cases, start=1):
            style = styles[(index - 1) % len(styles)]
            font_path = fonts[(index - 1) % len(fonts)]
            font_size = font_sizes[py_rng.randrange(len(font_sizes))]
            temp_path = args.output_dir / f"ocr-hard-{index:04d}.tmp.png"
            record = render_case(
                case,
                temp_path,
                "clean",
                font_path,
                font_size,
                np_rng,
            )
            with Image.open(temp_path) as source:
                image = process_style(
                    source.convert("RGB"), record, style, font_path, np_rng
                )
            suffix = ".jpg" if style in {"aggressive_jpeg", "mixed_artifacts"} else ".png"
            final_path = args.output_dir / f"ocr-hard-{index:04d}{suffix}"
            if suffix == ".jpg":
                image.save(final_path, quality=30, optimize=True)
            else:
                image.save(final_path, optimize=True)
            temp_path.unlink()
            record["id"] = f"ocr-hard-{case['id']}"
            record["image"] = final_path.as_posix()
            record["style"] = style
            record["font_size"] = font_size
            record["tags"] = list(case["tags"]) + [
                "synthetic_ocr",
                "stress_test",
                style,
            ]
            handle.write(json.dumps(record, ensure_ascii=False) + "\n")

    print(f"records={len(cases)}")
    print(f"manifest={args.manifest}")
    print(f"images={args.output_dir}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
