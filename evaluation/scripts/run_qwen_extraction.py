#!/usr/bin/env python3
"""Run batched zero-shot sensitive-entity extraction through llama-server."""

from __future__ import annotations

import argparse
import json
import time
from pathlib import Path
from typing import Iterable

import requests


DEFAULT_SCHEMA = Path(
    "evaluation/schemas/qwen-extraction-response.schema.json"
)
LABEL_GUIDE = """
PERSON_NAME：自然人姓名；联系人、负责人、员工姓名都属于此类
ORG_NAME：组织、公司、机构的完整名称
ADDRESS：详细地址
PHONE_NUMBER：电话号码
EMAIL：非公开邮箱
CN_ID_NUMBER：中国居民身份证格式号码
BANK_CARD_NUMBER：银行卡格式号码
FINANCIAL_AMOUNT：尚未公开的预算、收入、成本等财务金额
SALARY_AMOUNT：工资、薪酬、奖金；出现“工资、薪酬、奖金、税前”等词时优先使用此类，不要标为 FINANCIAL_AMOUNT
BUSINESS_METRIC：未公开的报价、底价、利润率、产量等业务指标
PROJECT_CODE：“项目代号、项目代码、Project code”字段中的值
CONTRACT_ID：合同或协议编号
CUSTOMER_NAME：客户名单或保密客户上下文中的名称
""".strip()

SYSTEM_PROMPT = f"""
你是离线数据脱敏系统中的候选识别器。请找出输入记录中需要脱敏的原文片段。

标签定义：
{LABEL_GUIDE}

规则：
1. text 必须逐字复制输入中的连续原文，不得改写、补全或解释。
2. 同一原文只输出一次；系统会定位它的全部出现位置。
3. 只有输入原文明说“公开、官网已发布、宣传、说明书示例、字段为空”时才跳过；
   不要根据名称看起来像虚构内容而自行判断为公开。
4. 无效校验位号码、页码、版本号、普通日期和普通数量不要按高风险号码输出。
5. 财务金额和业务数字必须结合“内部、未公开、保密、底价、预算、薪酬”等上下文判断。
6. 不要输出整句，只输出最小、完整的敏感片段。
7. 每个输入 id 必须在结果中恰好出现一次；没有命中时 entities 为空数组。
8. 联系人所在机构仍是 ORG_NAME；同一姓名在原文重复时只输出一次原文值。
9. 金额应复制完整写法，例如“人民币168,260.00元”；项目代码应包含完整前缀、年份和序号。
10. 只返回符合给定 JSON Schema 的 JSON。

示例：
输入：员工林嘉禾本月税前工资为人民币168,260.00元。
输出实体：林嘉禾/PERSON_NAME，人民币168,260.00元/SALARY_AMOUNT。
输入：Owner: 叶景行；Project code: 墨石-2026-12；Status: INTERNAL ONLY
输出实体：叶景行/PERSON_NAME，墨石-2026-12/PROJECT_CODE。
""".strip()


def load_jsonl(path: Path) -> list[dict[str, object]]:
    records: list[dict[str, object]] = []
    with path.open(encoding="utf-8") as handle:
        for line in handle:
            if line.strip():
                records.append(json.loads(line))
    return records


def batches(items: list[dict[str, object]], size: int) -> Iterable[list[dict[str, object]]]:
    for index in range(0, len(items), size):
        yield items[index : index + size]


def align_values(
    text: str, entities: list[dict[str, str]]
) -> list[dict[str, object]]:
    aligned: list[dict[str, object]] = []
    seen: set[tuple[int, int, str]] = set()
    for entity in entities:
        value = entity["text"]
        label = entity["label"]
        start = 0
        while value and (position := text.find(value, start)) != -1:
            end = position + len(value)
            key = (position, end, label)
            if key not in seen:
                seen.add(key)
                aligned.append(
                    {
                        "start": position,
                        "end": end,
                        "label": label,
                        "text": value,
                        "confidence": None,
                        "detector": "qwen3.5-4b-zero-shot",
                    }
                )
            start = end
    return sorted(aligned, key=lambda item: (item["start"], item["end"], item["label"]))


def call_server(
    base_url: str,
    model: str,
    schema: dict[str, object],
    records: list[dict[str, object]],
    timeout: int,
) -> tuple[dict[str, object], float]:
    payload_records = [
        {"id": record["id"], "text": record["text"]} for record in records
    ]
    user_prompt = (
        "请处理以下JSON数组：\n"
        + json.dumps(payload_records, ensure_ascii=False, separators=(",", ":"))
    )
    body = {
        "model": model,
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": user_prompt},
        ],
        "temperature": 0.0,
        "seed": 20260731,
        "max_tokens": 2048,
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "name": "sensitive_entity_extraction",
                "strict": True,
                "schema": schema,
            },
        },
    }
    started = time.perf_counter()
    response = requests.post(
        f"{base_url.rstrip('/')}/v1/chat/completions",
        json=body,
        timeout=timeout,
    )
    runtime_ms = (time.perf_counter() - started) * 1000
    response.raise_for_status()
    envelope = response.json()
    content = envelope["choices"][0]["message"]["content"]
    return json.loads(content), runtime_ms


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("dataset", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--base-url", default="http://127.0.0.1:18080")
    parser.add_argument("--model", default="qwen3.5-4b-q4-k-m")
    parser.add_argument("--schema", type=Path, default=DEFAULT_SCHEMA)
    parser.add_argument("--split", choices=["dev", "test", "all"], default="dev")
    parser.add_argument("--limit", type=int)
    parser.add_argument("--batch-size", type=int, default=6)
    parser.add_argument("--timeout", type=int, default=600)
    parser.add_argument("--retries", type=int, default=2)
    args = parser.parse_args()

    records = load_jsonl(args.dataset)
    if args.split != "all":
        records = [record for record in records if record["split"] == args.split]
    if args.limit is not None:
        records = records[: args.limit]
    schema = json.loads(args.schema.read_text(encoding="utf-8"))
    allowed_labels = set(
        schema["properties"]["items"]["items"]["properties"]["entities"]["items"][
            "properties"
        ]["label"]["enum"]
    )
    args.output.parent.mkdir(parents=True, exist_ok=True)

    completed = 0
    invalid_batches = 0
    with args.output.open("w", encoding="utf-8", newline="\n") as target:
        for batch_number, batch in enumerate(batches(records, args.batch_size), start=1):
            parsed: dict[str, object] | None = None
            runtime_ms = 0.0
            error_message: str | None = None
            for attempt in range(args.retries + 1):
                try:
                    parsed, runtime_ms = call_server(
                        args.base_url,
                        args.model,
                        schema,
                        batch,
                        args.timeout,
                    )
                    error_message = None
                    break
                except Exception as exc:
                    error_message = f"{type(exc).__name__}: {exc}"
                    if attempt < args.retries:
                        time.sleep(1 + attempt)

            per_record_ms = runtime_ms / max(len(batch), 1)
            expected_ids = {str(record["id"]) for record in batch}
            returned: dict[str, list[dict[str, str]]] = {}
            batch_valid = parsed is not None
            if parsed is not None:
                try:
                    returned_items = parsed["items"]
                    if not returned_items and len(batch) == 1:
                        # Some constrained decoders represent a single negative
                        # record as an empty top-level list. For a one-record
                        # request this is unambiguous and safe to normalize.
                        returned[str(batch[0]["id"])] = []
                    for item_value in returned_items:
                        item_id = str(item_value["id"])
                        if item_id not in expected_ids or item_id in returned:
                            raise ValueError(f"unexpected or duplicate id: {item_id}")
                        values: list[dict[str, str]] = []
                        for entity in item_value["entities"]:
                            value = str(entity["text"])
                            label = str(entity["label"])
                            if not value or label not in allowed_labels:
                                raise ValueError("invalid entity value or label")
                            values.append({"text": value, "label": label})
                        returned[item_id] = values
                    if set(returned) != expected_ids:
                        raise ValueError("not every input id was returned")
                except Exception as exc:
                    batch_valid = False
                    error_message = f"{type(exc).__name__}: {exc}"

            if not batch_valid:
                invalid_batches += 1
            for record in batch:
                record_id = str(record["id"])
                entities = (
                    align_values(str(record["text"]), returned.get(record_id, []))
                    if batch_valid
                    else []
                )
                prediction = {
                    "id": record_id,
                    "entities": entities,
                    "runtime_ms": per_record_ms,
                    "structured_output_valid": batch_valid,
                }
                if error_message:
                    prediction["error"] = error_message
                target.write(json.dumps(prediction, ensure_ascii=False) + "\n")
                completed += 1
            print(
                f"batch={batch_number} completed={completed}/{len(records)} "
                f"valid={batch_valid} runtime_ms={runtime_ms:.1f}",
                flush=True,
            )

    print(f"records={completed}")
    print(f"invalid_batches={invalid_batches}")
    print(f"output={args.output}")
    return 1 if invalid_batches else 0


if __name__ == "__main__":
    raise SystemExit(main())
