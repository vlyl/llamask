#!/usr/bin/env python3
"""Score OCR text accuracy and safety-oriented box coverage."""

from __future__ import annotations

import argparse
import collections
import json
import statistics
from pathlib import Path


def load_jsonl(path: Path) -> list[dict[str, object]]:
    with path.open(encoding="utf-8") as handle:
        return [json.loads(line) for line in handle if line.strip()]


def normalize(text: str) -> str:
    return "".join(char for char in text if not char.isspace())


def edit_distance(left: str, right: str) -> int:
    if len(left) < len(right):
        left, right = right, left
    previous = list(range(len(right) + 1))
    for left_index, left_char in enumerate(left, start=1):
        current = [left_index]
        for right_index, right_char in enumerate(right, start=1):
            current.append(
                min(
                    current[-1] + 1,
                    previous[right_index] + 1,
                    previous[right_index - 1] + (left_char != right_char),
                )
            )
        previous = current
    return previous[-1]


def intersection_over_union(left: list[int], right: list[int]) -> float:
    x0 = max(left[0], right[0])
    y0 = max(left[1], right[1])
    x1 = min(left[2], right[2])
    y1 = min(left[3], right[3])
    intersection = max(0, x1 - x0) * max(0, y1 - y0)
    left_area = max(0, left[2] - left[0]) * max(0, left[3] - left[1])
    right_area = max(0, right[2] - right[0]) * max(0, right[3] - right[1])
    union = left_area + right_area - intersection
    return intersection / union if union else 0.0


def covered_fraction(expected: list[int], predicted: list[int]) -> float:
    x0 = max(expected[0], predicted[0])
    y0 = max(expected[1], predicted[1])
    x1 = min(expected[2], predicted[2])
    y1 = min(expected[3], predicted[3])
    intersection = max(0, x1 - x0) * max(0, y1 - y0)
    expected_area = max(0, expected[2] - expected[0]) * max(
        0, expected[3] - expected[1]
    )
    return intersection / expected_area if expected_area else 0.0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("manifest", type=Path)
    parser.add_argument("predictions", type=Path)
    parser.add_argument("output_prefix", type=Path)
    args = parser.parse_args()

    expected_records = load_jsonl(args.manifest)
    prediction_map = {
        str(record["id"]): record for record in load_jsonl(args.predictions)
    }
    total_expected_chars = 0
    total_edit_distance = 0
    exact_sensitive = 0
    total_sensitive = 0
    covered_sensitive = 0
    detected_lines = 0
    total_lines = 0
    runtimes: list[float] = []
    by_style: dict[str, dict[str, float]] = collections.defaultdict(
        lambda: {
            "expected_chars": 0,
            "edit_distance": 0,
            "sensitive": 0,
            "sensitive_exact": 0,
            "sensitive_covered": 0,
            "lines": 0,
            "lines_detected": 0,
        }
    )

    for expected in expected_records:
        prediction = prediction_map.get(str(expected["id"]))
        if prediction is None:
            recognized = ""
            predicted_boxes: list[list[int]] = []
        else:
            recognized = normalize(str(prediction["recognized_text"]))
            predicted_boxes = [line["bbox"] for line in prediction["lines"]]
            runtimes.append(float(prediction["runtime_ms"]))
        expected_text = normalize(str(expected["text"]))
        distance = edit_distance(expected_text, recognized)
        total_expected_chars += len(expected_text)
        total_edit_distance += distance
        style = str(expected["style"])
        style_metrics = by_style[style]
        style_metrics["expected_chars"] += len(expected_text)
        style_metrics["edit_distance"] += distance

        for region in expected["sensitive_regions"]:
            total_sensitive += 1
            style_metrics["sensitive"] += 1
            value = normalize(str(region["text"]))
            if value and value in recognized:
                exact_sensitive += 1
                style_metrics["sensitive_exact"] += 1
            if any(
                covered_fraction(region["bbox"], predicted_box) >= 0.8
                for predicted_box in predicted_boxes
            ):
                covered_sensitive += 1
                style_metrics["sensitive_covered"] += 1

        for region in expected["line_regions"]:
            total_lines += 1
            style_metrics["lines"] += 1
            if any(
                intersection_over_union(region["bbox"], predicted_box) >= 0.5
                for predicted_box in predicted_boxes
            ):
                detected_lines += 1
                style_metrics["lines_detected"] += 1

    result = {
        "records": len(expected_records),
        "character_error_rate": (
            total_edit_distance / total_expected_chars if total_expected_chars else 0.0
        ),
        "character_accuracy": (
            1 - total_edit_distance / total_expected_chars
            if total_expected_chars
            else 0.0
        ),
        "sensitive_fragment_exact_recall": (
            exact_sensitive / total_sensitive if total_sensitive else 0.0
        ),
        "sensitive_box_coverage_recall": (
            covered_sensitive / total_sensitive if total_sensitive else 0.0
        ),
        "line_detection_recall_iou_50": (
            detected_lines / total_lines if total_lines else 0.0
        ),
        "sensitive_fragments": total_sensitive,
        "runtime": {
            "mean_ms": statistics.mean(runtimes) if runtimes else None,
            "median_ms": statistics.median(runtimes) if runtimes else None,
            "p95_ms": (
                sorted(runtimes)[min(len(runtimes) - 1, int(len(runtimes) * 0.95))]
                if runtimes
                else None
            ),
        },
        "by_style": {},
    }
    for style, metrics in sorted(by_style.items()):
        result["by_style"][style] = {
            "character_accuracy": (
                1 - metrics["edit_distance"] / metrics["expected_chars"]
                if metrics["expected_chars"]
                else 0.0
            ),
            "sensitive_fragment_exact_recall": (
                metrics["sensitive_exact"] / metrics["sensitive"]
                if metrics["sensitive"]
                else 0.0
            ),
            "sensitive_box_coverage_recall": (
                metrics["sensitive_covered"] / metrics["sensitive"]
                if metrics["sensitive"]
                else 0.0
            ),
            "line_detection_recall_iou_50": (
                metrics["lines_detected"] / metrics["lines"]
                if metrics["lines"]
                else 0.0
            ),
        }

    args.output_prefix.parent.mkdir(parents=True, exist_ok=True)
    json_path = args.output_prefix.with_suffix(".json")
    markdown_path = args.output_prefix.with_suffix(".md")
    json_path.write_text(
        json.dumps(result, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )
    lines = [
        "# OCR评测报告",
        "",
        f"- 样本数：{result['records']}",
        f"- 字符准确率：{result['character_accuracy']:.2%}",
        f"- 敏感片段精确召回：{result['sensitive_fragment_exact_recall']:.2%}",
        f"- 敏感区域覆盖召回：{result['sensitive_box_coverage_recall']:.2%}",
        f"- 行检测召回（IoU≥0.5）：{result['line_detection_recall_iou_50']:.2%}",
        f"- 平均每页：{result['runtime']['mean_ms']:.1f} ms",
        "",
        "## 按图像风格",
        "",
        "| 风格 | 字符准确率 | 敏感片段召回 | 敏感区域覆盖 | 行检测召回 |",
        "| --- | ---: | ---: | ---: | ---: |",
    ]
    for style, metrics in result["by_style"].items():
        lines.append(
            f"| {style} | {metrics['character_accuracy']:.2%} | "
            f"{metrics['sensitive_fragment_exact_recall']:.2%} | "
            f"{metrics['sensitive_box_coverage_recall']:.2%} | "
            f"{metrics['line_detection_recall_iou_50']:.2%} |"
        )
    markdown_path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(json.dumps(result, ensure_ascii=False))
    print(f"json={json_path}")
    print(f"markdown={markdown_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
