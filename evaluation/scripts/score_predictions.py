#!/usr/bin/env python3
"""Score exact, overlap and character-level entity extraction metrics."""

from __future__ import annotations

import argparse
import collections
import json
import statistics
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable


@dataclass
class Counts:
    true_positive: int = 0
    false_positive: int = 0
    false_negative: int = 0

    @property
    def precision(self) -> float:
        denominator = self.true_positive + self.false_positive
        return self.true_positive / denominator if denominator else 0.0

    @property
    def recall(self) -> float:
        denominator = self.true_positive + self.false_negative
        return self.true_positive / denominator if denominator else 0.0

    @property
    def f1(self) -> float:
        denominator = self.precision + self.recall
        return 2 * self.precision * self.recall / denominator if denominator else 0.0

    def add(self, other: "Counts") -> None:
        self.true_positive += other.true_positive
        self.false_positive += other.false_positive
        self.false_negative += other.false_negative


def load_jsonl(path: Path) -> list[dict[str, object]]:
    records: list[dict[str, object]] = []
    with path.open(encoding="utf-8") as handle:
        for line_number, line in enumerate(handle, start=1):
            if not line.strip():
                continue
            try:
                records.append(json.loads(line))
            except json.JSONDecodeError as exc:
                raise ValueError(f"{path}:{line_number}: invalid JSON: {exc}") from exc
    return records


def entity_key(entity: dict[str, object]) -> tuple[str, int, int]:
    return str(entity["label"]), int(entity["start"]), int(entity["end"])


def overlaps(left: dict[str, object], right: dict[str, object]) -> bool:
    return (
        left["label"] == right["label"]
        and int(left["start"]) < int(right["end"])
        and int(right["start"]) < int(left["end"])
    )


def counts_exact(
    expected: Iterable[dict[str, object]], predicted: Iterable[dict[str, object]]
) -> Counts:
    expected_keys = collections.Counter(entity_key(item) for item in expected)
    predicted_keys = collections.Counter(entity_key(item) for item in predicted)
    true_positive = sum((expected_keys & predicted_keys).values())
    return Counts(
        true_positive=true_positive,
        false_positive=sum(predicted_keys.values()) - true_positive,
        false_negative=sum(expected_keys.values()) - true_positive,
    )


def counts_overlap(
    expected: list[dict[str, object]], predicted: list[dict[str, object]]
) -> Counts:
    used: set[int] = set()
    true_positive = 0
    for expected_entity in expected:
        for index, predicted_entity in enumerate(predicted):
            if index not in used and overlaps(expected_entity, predicted_entity):
                used.add(index)
                true_positive += 1
                break
    return Counts(
        true_positive=true_positive,
        false_positive=len(predicted) - true_positive,
        false_negative=len(expected) - true_positive,
    )


def counts_characters(
    expected: list[dict[str, object]], predicted: list[dict[str, object]]
) -> Counts:
    expected_chars: set[tuple[str, int]] = set()
    predicted_chars: set[tuple[str, int]] = set()
    for entity in expected:
        expected_chars.update(
            (str(entity["label"]), position)
            for position in range(int(entity["start"]), int(entity["end"]))
        )
    for entity in predicted:
        predicted_chars.update(
            (str(entity["label"]), position)
            for position in range(int(entity["start"]), int(entity["end"]))
        )
    true_positive = len(expected_chars & predicted_chars)
    return Counts(
        true_positive=true_positive,
        false_positive=len(predicted_chars - expected_chars),
        false_negative=len(expected_chars - predicted_chars),
    )


def metric_dict(counts: Counts) -> dict[str, object]:
    return {
        "tp": counts.true_positive,
        "fp": counts.false_positive,
        "fn": counts.false_negative,
        "precision": counts.precision,
        "recall": counts.recall,
        "f1": counts.f1,
    }


def score(
    dataset: list[dict[str, object]], predictions: list[dict[str, object]]
) -> dict[str, object]:
    prediction_map = {str(item["id"]): item for item in predictions}
    expected_ids = {str(item["id"]) for item in dataset}
    extra_ids = sorted(set(prediction_map) - expected_ids)
    if extra_ids:
        raise ValueError(f"predictions contain unknown ids: {extra_ids[:5]}")

    exact_total = Counts()
    overlap_total = Counts()
    character_total = Counts()
    label_counts: dict[str, Counts] = collections.defaultdict(Counts)
    difficulty_counts: dict[str, Counts] = collections.defaultdict(Counts)
    tag_counts: dict[str, Counts] = collections.defaultdict(Counts)
    runtimes: list[float] = []
    valid_outputs = 0
    missing_predictions = 0

    for record in dataset:
        record_id = str(record["id"])
        expected = list(record["entities"])
        prediction = prediction_map.get(record_id)
        if prediction is None:
            missing_predictions += 1
            predicted: list[dict[str, object]] = []
        else:
            predicted = list(prediction.get("entities", []))
            if "runtime_ms" in prediction:
                runtimes.append(float(prediction["runtime_ms"]))
            if prediction.get("structured_output_valid") is True:
                valid_outputs += 1

        for entity in predicted:
            start = int(entity["start"])
            end = int(entity["end"])
            if not (0 <= start < end <= len(str(record["text"]))):
                raise ValueError(f"{record_id}: predicted span out of bounds")
            if str(record["text"])[start:end] != entity.get("text"):
                raise ValueError(f"{record_id}: predicted text does not match span")

        exact = counts_exact(expected, predicted)
        overlap = counts_overlap(expected, predicted)
        characters = counts_characters(expected, predicted)
        exact_total.add(exact)
        overlap_total.add(overlap)
        character_total.add(characters)
        difficulty_counts[str(record["difficulty"])].add(exact)
        for tag in record["tags"]:
            tag_counts[str(tag)].add(exact)

        labels = {str(item["label"]) for item in expected + predicted}
        for label in labels:
            label_expected = [item for item in expected if item["label"] == label]
            label_predicted = [item for item in predicted if item["label"] == label]
            label_counts[label].add(counts_exact(label_expected, label_predicted))

    runtime_summary: dict[str, float | None] = {
        "mean_ms": statistics.mean(runtimes) if runtimes else None,
        "median_ms": statistics.median(runtimes) if runtimes else None,
        "p95_ms": None,
        "total_ms": sum(runtimes) if runtimes else None,
    }
    if runtimes:
        ordered = sorted(runtimes)
        runtime_summary["p95_ms"] = ordered[min(len(ordered) - 1, int(len(ordered) * 0.95))]

    return {
        "records": len(dataset),
        "prediction_records": len(predictions),
        "missing_predictions": missing_predictions,
        "exact_micro": metric_dict(exact_total),
        "overlap_micro": metric_dict(overlap_total),
        "character_micro": metric_dict(character_total),
        "by_label": {
            label: metric_dict(counts) for label, counts in sorted(label_counts.items())
        },
        "by_difficulty": {
            label: metric_dict(counts) for label, counts in sorted(difficulty_counts.items())
        },
        "by_tag": {
            label: metric_dict(counts) for label, counts in sorted(tag_counts.items())
        },
        "structured_output_valid_rate": valid_outputs / len(dataset) if dataset else 0.0,
        "runtime": runtime_summary,
    }


def format_percent(value: float) -> str:
    return f"{value * 100:.2f}%"


def markdown_report(result: dict[str, object]) -> str:
    exact = result["exact_micro"]
    overlap = result["overlap_micro"]
    character = result["character_micro"]
    lines = [
        "# LlaMask 评测报告",
        "",
        f"- 样本数：{result['records']}",
        f"- 预测记录数：{result['prediction_records']}",
        f"- 缺失预测：{result['missing_predictions']}",
        f"- 结构化输出有效率：{format_percent(result['structured_output_valid_rate'])}",
        "",
        "## 总体指标",
        "",
        "| 指标 | Precision | Recall | F1 |",
        "| --- | ---: | ---: | ---: |",
        (
            f"| 精确 span | {format_percent(exact['precision'])} | "
            f"{format_percent(exact['recall'])} | {format_percent(exact['f1'])} |"
        ),
        (
            f"| 重叠 span | {format_percent(overlap['precision'])} | "
            f"{format_percent(overlap['recall'])} | {format_percent(overlap['f1'])} |"
        ),
        (
            f"| 字符级 | {format_percent(character['precision'])} | "
            f"{format_percent(character['recall'])} | {format_percent(character['f1'])} |"
        ),
        "",
        "## 按实体类型",
        "",
        "| 类型 | TP | FP | FN | Precision | Recall | F1 |",
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    for label, metrics in result["by_label"].items():
        lines.append(
            f"| {label} | {metrics['tp']} | {metrics['fp']} | {metrics['fn']} | "
            f"{format_percent(metrics['precision'])} | "
            f"{format_percent(metrics['recall'])} | "
            f"{format_percent(metrics['f1'])} |"
        )
    runtime = result["runtime"]
    lines.extend(
        [
            "",
            "## 运行时间",
            "",
            f"- 平均每条：{runtime['mean_ms'] if runtime['mean_ms'] is not None else 'N/A'} ms",
            f"- 中位数：{runtime['median_ms'] if runtime['median_ms'] is not None else 'N/A'} ms",
            f"- P95：{runtime['p95_ms'] if runtime['p95_ms'] is not None else 'N/A'} ms",
            "",
        ]
    )
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("dataset", type=Path)
    parser.add_argument("predictions", type=Path)
    parser.add_argument("output_prefix", type=Path)
    parser.add_argument("--split", choices=["dev", "test", "all"], default="all")
    parser.add_argument("--limit", type=int)
    parser.add_argument(
        "--filter-extra",
        action="store_true",
        help="Ignore predictions outside the selected split/limit.",
    )
    args = parser.parse_args()

    dataset = load_jsonl(args.dataset)
    if args.split != "all":
        dataset = [record for record in dataset if record["split"] == args.split]
    if args.limit is not None:
        dataset = dataset[: args.limit]
    predictions = load_jsonl(args.predictions)
    if args.filter_extra:
        selected_ids = {str(record["id"]) for record in dataset}
        predictions = [
            record for record in predictions if str(record["id"]) in selected_ids
        ]
    result = score(dataset, predictions)
    args.output_prefix.parent.mkdir(parents=True, exist_ok=True)
    json_path = args.output_prefix.with_suffix(".json")
    markdown_path = args.output_prefix.with_suffix(".md")
    json_path.write_text(
        json.dumps(result, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )
    markdown_path.write_text(markdown_report(result), encoding="utf-8")
    print(json.dumps(result["exact_micro"], ensure_ascii=False))
    print(f"json={json_path}")
    print(f"markdown={markdown_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
