#!/usr/bin/env python3
"""Run safety-oriented deterministic and contextual sensitive-data rules."""

from __future__ import annotations

import argparse
import datetime
import json
import re
import time
from pathlib import Path

import run_rules_baseline as base


AMOUNT_PATTERN = re.compile(r"人民币\d{1,3}(?:,\d{3})*(?:\.\d{1,2})?元")
PERCENT_PATTERN = re.compile(r"(?<![\d.])\d+(?:\.\d+)?%(?![\d.])")
FIELD_ADDRESS_PATTERN = re.compile(
    r"(?:办公)?地址\s*[:：]\s*(?P<value>[^，。,；;\n]{6,60})"
)
PROJECT_FIELD_PATTERN = re.compile(
    r"(?:保密)?项目(?:代号|代码)\s*[:：]\s*"
    r"(?P<value>[\u4e00-\u9fffA-Za-z][\u4e00-\u9fffA-Za-z0-9_.-]{2,40})",
    re.IGNORECASE,
)
PROJECT_INLINE_PATTERN = re.compile(
    r"项目(?P<value>[\u4e00-\u9fffA-Za-z]{1,12}-\d{4}-[A-Za-z0-9]{1,12})(?=由|，|。|\s)"
)
PROJECT_ENGLISH_PATTERN = re.compile(
    r"Project\s+code\s*[:：]\s*"
    r"(?P<value>[\u4e00-\u9fffA-Za-z][\u4e00-\u9fffA-Za-z0-9_.-]{2,40})",
    re.IGNORECASE,
)
CONTRACT_PATTERN = re.compile(
    r"(?:合同|协议)(?:编号|号)\s*[:：]?\s*"
    r"(?P<value>[A-Za-z][A-Za-z0-9_.-]{5,50})",
    re.IGNORECASE,
)

PUBLIC_CUES = ("公开", "官网", "宣传", "公共邮箱", "客服电话", "售后服务")
INVALID_IDENTIFIER_CUES = ("校验位无效", "不是银行卡", "版本构建号", "测试字符串")
ADDRESS_CUES = ("省", "市", "区", "县", "街", "路", "大道", "巷", "号", "栋", "室")


def context_window(text: str, start: int, end: int, radius: int = 28) -> str:
    return text[max(0, start - radius) : min(len(text), end + radius)]


def is_explicitly_public(text: str, start: int, end: int) -> bool:
    window = context_window(text, start, end)
    return any(cue in window for cue in PUBLIC_CUES)


def has_invalid_identifier_cue(text: str, start: int, end: int) -> bool:
    window = context_window(text, start, end, radius=24)
    return any(cue in window for cue in INVALID_IDENTIFIER_CUES)


def has_valid_cn_id_date(value: str) -> bool:
    try:
        datetime.datetime.strptime(value[6:14], "%Y%m%d")
    except ValueError:
        return False
    return value[:2] not in {"00", "62"}


def entity(
    text: str,
    start: int,
    end: int,
    label: str,
    detector: str,
) -> dict[str, object]:
    return {
        "start": start,
        "end": end,
        "label": label,
        "text": text[start:end],
        "confidence": 1.0,
        "detector": detector,
    }


def classify_amount(text: str, start: int, end: int) -> str | None:
    window = context_window(text, start, end, radius=32)
    if any(cue in window for cue in ("工资", "薪酬", "奖金", "税前")):
        return "SALARY_AMOUNT"
    if any(cue in window for cue in ("底价", "最低报价", "内部报价")):
        return "BUSINESS_METRIC"
    if any(
        cue in window
        for cue in ("内部预算", "预算草案", "未公开预算", "成本", "收入")
    ):
        return "FINANCIAL_AMOUNT"
    return None


def add_unique(
    findings: list[dict[str, object]],
    candidate: dict[str, object],
) -> None:
    key = (candidate["start"], candidate["end"], candidate["label"])
    if any((item["start"], item["end"], item["label"]) == key for item in findings):
        return
    findings.append(candidate)


def detect(text: str) -> list[dict[str, object]]:
    findings: list[dict[str, object]] = []
    base_findings = base.detect(text)
    cn_id_spans = {
        (int(item["start"]), int(item["end"]))
        for item in base_findings
        if item["label"] == "CN_ID_NUMBER"
        and has_valid_cn_id_date(str(item["text"]))
        and not has_invalid_identifier_cue(
            text, int(item["start"]), int(item["end"])
        )
    }
    for item in base_findings:
        start = int(item["start"])
        end = int(item["end"])
        label = str(item["label"])
        if label == "CN_ID_NUMBER" and not has_valid_cn_id_date(str(item["text"])):
            continue
        if label == "BANK_CARD_NUMBER" and (start, end) in cn_id_spans:
            continue
        if label in {"CN_ID_NUMBER", "BANK_CARD_NUMBER"} and has_invalid_identifier_cue(
            text, start, end
        ):
            continue
        if label in {"EMAIL", "PHONE_NUMBER"} and is_explicitly_public(
            text, start, end
        ):
            continue
        updated = dict(item)
        updated["detector"] = "context-rules-v1"
        add_unique(findings, updated)

    for match in AMOUNT_PATTERN.finditer(text):
        label = classify_amount(text, match.start(), match.end())
        if label and not is_explicitly_public(text, match.start(), match.end()):
            add_unique(
                findings,
                entity(
                    text,
                    match.start(),
                    match.end(),
                    label,
                    "context-rules-v1",
                ),
            )

    for match in PERCENT_PATTERN.finditer(text):
        window = context_window(text, match.start(), match.end())
        if any(cue in window for cue in ("未公开", "毛利率", "利润率", "内部")):
            add_unique(
                findings,
                entity(
                    text,
                    match.start(),
                    match.end(),
                    "BUSINESS_METRIC",
                    "context-rules-v1",
                ),
            )

    for match in FIELD_ADDRESS_PATTERN.finditer(text):
        value = match.group("value")
        if any(cue in value for cue in ADDRESS_CUES):
            add_unique(
                findings,
                entity(
                    text,
                    match.start("value"),
                    match.end("value"),
                    "ADDRESS",
                    "context-rules-v1",
                ),
            )

    for pattern in (
        PROJECT_FIELD_PATTERN,
        PROJECT_INLINE_PATTERN,
        PROJECT_ENGLISH_PATTERN,
    ):
        for match in pattern.finditer(text):
            add_unique(
                findings,
                entity(
                    text,
                    match.start("value"),
                    match.end("value"),
                    "PROJECT_CODE",
                    "context-rules-v1",
                ),
            )

    for match in CONTRACT_PATTERN.finditer(text):
        add_unique(
            findings,
            entity(
                text,
                match.start("value"),
                match.end("value"),
                "CONTRACT_ID",
                "context-rules-v1",
            ),
        )

    return sorted(findings, key=lambda item: (item["start"], item["end"], item["label"]))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("dataset", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    args.output.parent.mkdir(parents=True, exist_ok=True)

    count = 0
    total_ms = 0.0
    with args.dataset.open(encoding="utf-8") as source, args.output.open(
        "w", encoding="utf-8", newline="\n"
    ) as target:
        for line in source:
            if not line.strip():
                continue
            record = json.loads(line)
            started = time.perf_counter()
            entities = detect(str(record["text"]))
            runtime_ms = (time.perf_counter() - started) * 1000
            total_ms += runtime_ms
            target.write(
                json.dumps(
                    {
                        "id": record["id"],
                        "entities": entities,
                        "runtime_ms": runtime_ms,
                        "structured_output_valid": True,
                    },
                    ensure_ascii=False,
                )
                + "\n"
            )
            count += 1

    print(f"records={count}")
    print(f"runtime_ms.total={total_ms:.3f}")
    print(f"runtime_ms.mean={total_ms / max(count, 1):.6f}")
    print(f"output={args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
