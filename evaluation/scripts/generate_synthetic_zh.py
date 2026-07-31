#!/usr/bin/env python3
"""Generate a deterministic Chinese sensitive-information evaluation set."""

from __future__ import annotations

import argparse
import hashlib
import json
import random
from dataclasses import dataclass
from datetime import date, timedelta
from pathlib import Path
from typing import Callable, Iterable


SEED = 20260731
DEFAULT_COUNT = 1800
DEFAULT_OUTPUT = Path("evaluation/datasets/generated/synthetic_zh_v2.jsonl")


@dataclass(frozen=True)
class Sensitive:
    value: str
    label: str


def compose(*parts: str | Sensitive) -> tuple[str, list[dict[str, object]]]:
    text_parts: list[str] = []
    entities: list[dict[str, object]] = []
    cursor = 0
    for part in parts:
        if isinstance(part, Sensitive):
            start = cursor
            text_parts.append(part.value)
            cursor += len(part.value)
            entities.append(
                {
                    "start": start,
                    "end": cursor,
                    "label": part.label,
                    "text": part.value,
                }
            )
        else:
            text_parts.append(part)
            cursor += len(part)
    return "".join(text_parts), entities


def luhn_check_digit(prefix: str) -> str:
    digits = [int(value) for value in prefix] + [0]
    parity = len(digits) % 2
    total = 0
    for index, value in enumerate(digits):
        if index % 2 == parity:
            value *= 2
            if value > 9:
                value -= 9
        total += value
    return str((-total) % 10)


def cn_id_check_digit(first_17: str) -> str:
    weights = [7, 9, 10, 5, 8, 4, 2, 1, 6, 3, 7, 9, 10, 5, 8, 4, 2]
    checks = "10X98765432"
    return checks[sum(int(v) * w for v, w in zip(first_17, weights)) % 11]


def fake_phone(index: int) -> str:
    return f"1380000{index % 10000:04d}"


def fake_email(index: int) -> str:
    return f"case{index:04d}@example.com"


def fake_bank_card(index: int) -> str:
    prefix = f"6222020000{index % 10_000_000:07d}"
    return prefix + luhn_check_digit(prefix)


def fake_cn_id(index: int) -> str:
    birthday = date(1980, 1, 1) + timedelta(days=(index * 37) % 12_000)
    first_17 = f"110105{birthday:%Y%m%d}{index % 1000:03d}"
    return first_17 + cn_id_check_digit(first_17)


SURNAMES = ["沈", "林", "周", "顾", "唐", "许", "陆", "秦", "乔", "程", "宋", "叶"]
GIVEN_NAMES = ["晓宁", "清和", "远舟", "知夏", "景行", "书言", "嘉禾", "云川", "若安", "星澜"]
ORG_PREFIXES = ["星河智联", "远岚新材", "青屿环科", "云杉数科", "北辰物流", "澄明生物"]
ORG_SUFFIXES = ["科技有限公司", "咨询中心", "供应链集团", "数据研究院", "制造有限公司"]
ROAD_NAMES = ["松云路", "临江大道", "海棠街", "望川巷", "清泉路"]
PROJECT_WORDS = ["青岚", "星桥", "远帆", "云雀", "北斗", "澄海", "松塔", "墨石"]


def fake_person(rng: random.Random) -> str:
    return rng.choice(SURNAMES) + rng.choice(GIVEN_NAMES)


def fake_org(rng: random.Random) -> str:
    return rng.choice(ORG_PREFIXES) + rng.choice(ORG_SUFFIXES)


def fake_address(rng: random.Random, index: int) -> str:
    return f"海川市云岚区{rng.choice(ROAD_NAMES)}{10 + index % 800}号{1 + index % 9}栋"


def fake_amount(index: int) -> str:
    major = 10_000 + (index * 7_913) % 8_000_000
    return f"人民币{major:,}.00元"


def fake_project(rng: random.Random, index: int) -> str:
    return f"{rng.choice(PROJECT_WORDS)}-{2026 + index % 3}-{index % 100:02d}"


def fake_contract(index: int) -> str:
    return f"HT-LLM-{2026 + index % 3}-{index % 100000:05d}"


def case_personal(rng: random.Random, index: int) -> tuple[str, list[dict[str, object]], list[str], str]:
    person = fake_person(rng)
    variants: list[Callable[[], tuple[str, list[dict[str, object]]]]] = [
        lambda: compose(
            "联系人：",
            Sensitive(person, "PERSON_NAME"),
            "；手机：",
            Sensitive(fake_phone(index), "PHONE_NUMBER"),
            "；邮箱：",
            Sensitive(fake_email(index), "EMAIL"),
            "。",
        ),
        lambda: compose(
            Sensitive(person, "PERSON_NAME"),
            "的居民身份证号码为",
            Sensitive(fake_cn_id(index), "CN_ID_NUMBER"),
            "，仅限本次核验使用。",
        ),
        lambda: compose(
            "收款户名",
            Sensitive(person, "PERSON_NAME"),
            "，银行卡号",
            Sensitive(fake_bank_card(index), "BANK_CARD_NUMBER"),
            "。",
        ),
        lambda: compose(
            "请将材料寄给",
            Sensitive(person, "PERSON_NAME"),
            "，地址：",
            Sensitive(fake_address(rng, index), "ADDRESS"),
            "，电话",
            Sensitive(fake_phone(index), "PHONE_NUMBER"),
            "。",
        ),
    ]
    text, entities = rng.choice(variants)()
    return text, entities, ["synthetic", "personal", "plain_text"], "easy"


def case_organization(rng: random.Random, index: int) -> tuple[str, list[dict[str, object]], list[str], str]:
    person = fake_person(rng)
    org = fake_org(rng)
    variants: list[Callable[[], tuple[str, list[dict[str, object]]]]] = [
        lambda: compose(
            Sensitive(person, "PERSON_NAME"),
            "代表",
            Sensitive(org, "ORG_NAME"),
            "签署本备忘录。",
        ),
        lambda: compose(
            "机构名称：",
            Sensitive(org, "ORG_NAME"),
            "\n项目负责人：",
            Sensitive(person, "PERSON_NAME"),
            "\n办公地址：",
            Sensitive(fake_address(rng, index), "ADDRESS"),
        ),
        lambda: compose(
            "| 单位 | 联系人 |\n| --- | --- |\n| ",
            Sensitive(org, "ORG_NAME"),
            " | ",
            Sensitive(person, "PERSON_NAME"),
            " |",
        ),
        lambda: compose(
            "请联系 ",
            Sensitive(person, "PERSON_NAME"),
            "（",
            Sensitive(fake_email(index), "EMAIL"),
            "），其所在机构为",
            Sensitive(org, "ORG_NAME"),
            "。",
        ),
    ]
    text, entities = rng.choice(variants)()
    return text, entities, ["synthetic", "organization", "structured_text"], "normal"


def case_financial(rng: random.Random, index: int) -> tuple[str, list[dict[str, object]], list[str], str]:
    amount = fake_amount(index)
    percentage = f"{8 + index % 43}.{index % 10}%"
    variants: list[Callable[[], tuple[str, list[dict[str, object]]]]] = [
        lambda: compose(
            "内部预算草案：",
            Sensitive(amount, "FINANCIAL_AMOUNT"),
            "，未经批准不得向供应商披露。",
        ),
        lambda: compose(
            Sensitive(fake_person(rng), "PERSON_NAME"),
            "本月税前工资为",
            Sensitive(amount, "SALARY_AMOUNT"),
            "，请按薪酬密级处理。",
        ),
        lambda: compose(
            "本季度未公开毛利率为",
            Sensitive(percentage, "BUSINESS_METRIC"),
            "，预计下月董事会后披露。",
        ),
        lambda: compose(
            "底价测算表中，项目可接受最低报价为",
            Sensitive(amount, "BUSINESS_METRIC"),
            "。",
        ),
    ]
    text, entities = rng.choice(variants)()
    return text, entities, ["synthetic", "financial", "context_required"], "hard"


def case_business(rng: random.Random, index: int) -> tuple[str, list[dict[str, object]], list[str], str]:
    org = fake_org(rng)
    variants: list[Callable[[], tuple[str, list[dict[str, object]]]]] = [
        lambda: compose(
            "保密项目代号：",
            Sensitive(fake_project(rng, index), "PROJECT_CODE"),
            "；当前阶段禁止出现在外发文件中。",
        ),
        lambda: compose(
            "合同编号",
            Sensitive(fake_contract(index), "CONTRACT_ID"),
            "对应的附件尚未公开。",
        ),
        lambda: compose(
            "重点客户名单包含",
            Sensitive(org, "CUSTOMER_NAME"),
            "，不得向其他渠道商披露。",
        ),
        lambda: compose(
            "项目",
            Sensitive(fake_project(rng, index), "PROJECT_CODE"),
            "由",
            Sensitive(org, "ORG_NAME"),
            "承接，合同号",
            Sensitive(fake_contract(index), "CONTRACT_ID"),
            "。",
        ),
    ]
    text, entities = rng.choice(variants)()
    return text, entities, ["synthetic", "business", "context_required"], "hard"


def case_repeated(rng: random.Random, index: int) -> tuple[str, list[dict[str, object]], list[str], str]:
    person = fake_person(rng)
    org = fake_org(rng)
    text, entities = compose(
        Sensitive(person, "PERSON_NAME"),
        "负责",
        Sensitive(org, "ORG_NAME"),
        "的交付。若资料有误，请直接联系",
        Sensitive(person, "PERSON_NAME"),
        "，不要转发给无关人员。",
    )
    return text, entities, ["synthetic", "repeated_entity", "cross_sentence"], "normal"


def case_mixed(rng: random.Random, index: int) -> tuple[str, list[dict[str, object]], list[str], str]:
    person = fake_person(rng)
    text, entities = compose(
        "Owner: ",
        Sensitive(person, "PERSON_NAME"),
        "\nEmail: ",
        Sensitive(fake_email(index), "EMAIL"),
        "\nProject code: ",
        Sensitive(fake_project(rng, index), "PROJECT_CODE"),
        "\nStatus: INTERNAL ONLY",
    )
    return text, entities, ["synthetic", "mixed_language", "multiline"], "normal"


def case_hard_negative(rng: random.Random, index: int) -> tuple[str, list[dict[str, object]], list[str], str]:
    invalid_card = fake_bank_card(index)[:-1] + str((int(fake_bank_card(index)[-1]) + 1) % 10)
    invalid_id = fake_cn_id(index)[:-1] + ("0" if fake_cn_id(index)[-1] != "0" else "1")
    negatives = [
        "《王者荣耀》是游戏名称，这里的“王”不是联系人姓名。",
        "李子和桃子属于水果，句子中没有人员名单。",
        "产品公开售价为人民币1,999.00元，已在官方网站发布。",
        "公开客服电话为400-000-0000，可保留在宣传材料中。",
        "组织公共邮箱support@example.com用于售后服务。",
        f"测试字符串{invalid_id}具有18位外形，但校验位无效。",
        f"版本构建号为{invalid_card}，虽然位数相似但不是银行卡。",
        "手机号：未填写；身份证号：未采集；银行卡：不适用。",
        "本页为第12页，共128页，修订版本为2026.07。",
        "预算功能用于设置告警阈值，本段没有任何实际金额。",
        "客户成功团队负责培训，这里的“客户”不是客户名单。",
        "项目代号字段由管理员配置，本说明书没有给出真实代号。",
    ]
    return (
        rng.choice(negatives),
        [],
        ["synthetic", "hard_negative"],
        "hard",
    )


BUILDERS = [
    case_personal,
    case_organization,
    case_financial,
    case_business,
    case_repeated,
    case_mixed,
    case_hard_negative,
]


def generate_cases(count: int, seed: int) -> Iterable[dict[str, object]]:
    rng = random.Random(seed)
    seen_texts: set[str] = set()
    index = 1
    attempts = 0
    while index <= count:
        attempts += 1
        if attempts > count * 100:
            raise RuntimeError("Unable to generate enough unique cases")
        builder = BUILDERS[(index - 1) % len(BUILDERS)]
        text, entities, tags, difficulty = builder(rng, index + attempts)
        if text in seen_texts:
            continue
        seen_texts.add(text)
        split = "dev" if index % 5 == 0 else "test"
        language = "mixed" if "mixed_language" in tags else "zh-CN"
        yield {
            "id": f"syn-zh-{index:06d}",
            "split": split,
            "language": language,
            "text": text,
            "entities": entities,
            "tags": tags,
            "difficulty": difficulty,
            "policy": "default-high-recall",
        }
        index += 1


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--count", type=int, default=DEFAULT_COUNT)
    parser.add_argument("--seed", type=int, default=SEED)
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    args = parser.parse_args()

    if args.count < 1:
        parser.error("--count must be positive")

    args.output.parent.mkdir(parents=True, exist_ok=True)
    digest = hashlib.sha256()
    with args.output.open("w", encoding="utf-8", newline="\n") as handle:
        for case in generate_cases(args.count, args.seed):
            line = json.dumps(case, ensure_ascii=False, separators=(",", ":")) + "\n"
            handle.write(line)
            digest.update(line.encode("utf-8"))

    print(f"records={args.count}")
    print(f"seed={args.seed}")
    print(f"sha256={digest.hexdigest()}")
    print(f"output={args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
