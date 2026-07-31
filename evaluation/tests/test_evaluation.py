from __future__ import annotations

import sys
import unittest
from pathlib import Path


SCRIPTS = Path(__file__).resolve().parents[1] / "scripts"
sys.path.insert(0, str(SCRIPTS))

import generate_synthetic_zh as generator  # noqa: E402
import merge_predictions as merger  # noqa: E402
import run_context_rules as context_rules  # noqa: E402
import run_qwen_extraction as qwen  # noqa: E402
import run_rules_baseline as rules  # noqa: E402
import run_siamese_uie_benchmark as siamese  # noqa: E402
import score_predictions as scorer  # noqa: E402


class GeneratorTests(unittest.TestCase):
    def test_compose_uses_unicode_code_point_offsets(self) -> None:
        text, entities = generator.compose(
            "联系人",
            generator.Sensitive("沈晓宁", "PERSON_NAME"),
            "🙂电话",
            generator.Sensitive("13800000001", "PHONE_NUMBER"),
        )
        self.assertEqual(text[entities[0]["start"] : entities[0]["end"]], "沈晓宁")
        self.assertEqual(text[entities[1]["start"] : entities[1]["end"]], "13800000001")

    def test_generated_identifiers_pass_their_checksum(self) -> None:
        for index in range(1, 100):
            self.assertTrue(rules.valid_cn_id(generator.fake_cn_id(index)))
            self.assertTrue(rules.valid_luhn(generator.fake_bank_card(index)))

    def test_generation_is_deterministic_and_unique(self) -> None:
        first = list(generator.generate_cases(100, generator.SEED))
        second = list(generator.generate_cases(100, generator.SEED))
        self.assertEqual(first, second)
        self.assertEqual(len({item["text"] for item in first}), 100)


class RuleTests(unittest.TestCase):
    def test_rules_find_exact_identifiers(self) -> None:
        phone = generator.fake_phone(1)
        email = generator.fake_email(1)
        cn_id = generator.fake_cn_id(1)
        text = f"{phone} {email} {cn_id}"
        labels = {item["label"] for item in rules.detect(text)}
        self.assertTrue({"PHONE_NUMBER", "EMAIL", "CN_ID_NUMBER"} <= labels)

    def test_invalid_check_digits_are_rejected(self) -> None:
        cn_id = generator.fake_cn_id(3)
        replacement = "0" if cn_id[-1] != "0" else "1"
        self.assertFalse(rules.valid_cn_id(cn_id[:-1] + replacement))

        card = generator.fake_bank_card(3)
        replacement = str((int(card[-1]) + 1) % 10)
        self.assertFalse(rules.valid_luhn(card[:-1] + replacement))

    def test_context_rules_classify_salary_address_and_project(self) -> None:
        text = (
            "林嘉禾本月税前工资为人民币168,260.00元；"
            "办公地址：海川市云岚区松云路18号；"
            "Project code: 墨石-2026-12"
        )
        labels = {item["label"] for item in context_rules.detect(text)}
        self.assertTrue({"SALARY_AMOUNT", "ADDRESS", "PROJECT_CODE"} <= labels)

    def test_context_rules_skip_explicitly_public_email(self) -> None:
        self.assertEqual(
            context_rules.detect("组织公共邮箱support@example.com用于售后服务。"),
            [],
        )

    def test_context_rules_reject_bank_number_as_cn_id(self) -> None:
        card = generator.fake_bank_card(17)
        labels = {
            item["label"]
            for item in context_rules.detect(f"银行卡号：{card}")
        }
        self.assertEqual(labels, {"BANK_CARD_NUMBER"})


class ScoringTests(unittest.TestCase):
    def test_exact_and_overlap_metrics_differ_for_wide_span(self) -> None:
        expected = [{"label": "PERSON_NAME", "start": 1, "end": 4}]
        predicted = [{"label": "PERSON_NAME", "start": 0, "end": 4}]
        exact = scorer.counts_exact(expected, predicted)
        overlap = scorer.counts_overlap(expected, predicted)
        self.assertEqual(exact.true_positive, 0)
        self.assertEqual(overlap.true_positive, 1)

    def test_missing_prediction_becomes_false_negative(self) -> None:
        result = scorer.score(
            [
                {
                    "id": "case-1",
                    "text": "沈晓宁",
                    "entities": [
                        {
                            "label": "PERSON_NAME",
                            "start": 0,
                            "end": 3,
                            "text": "沈晓宁",
                        }
                    ],
                    "difficulty": "easy",
                    "tags": ["test"],
                }
            ],
            [],
        )
        self.assertEqual(result["missing_predictions"], 1)
        self.assertEqual(result["exact_micro"]["fn"], 1)

    def test_complete_subset_has_no_missing_predictions(self) -> None:
        record = {
            "id": "dev-1",
            "text": "姓名：沈景行",
            "entities": [
                {
                    "start": 3,
                    "end": 6,
                    "label": "PERSON_NAME",
                    "text": "沈景行",
                }
            ],
            "difficulty": "easy",
            "tags": [],
            "split": "dev",
        }
        prediction = {
            "id": "dev-1",
            "entities": list(record["entities"]),
            "structured_output_valid": True,
        }
        result = scorer.score([record], [prediction])
        self.assertEqual(result["records"], 1)
        self.assertEqual(result["missing_predictions"], 0)
        self.assertEqual(result["exact_micro"]["f1"], 1.0)


class QwenAdapterTests(unittest.TestCase):
    def test_exact_value_is_aligned_to_every_occurrence(self) -> None:
        entities = qwen.align_values(
            "沈晓宁负责交付，请联系沈晓宁。",
            [{"text": "沈晓宁", "label": "PERSON_NAME"}],
        )
        self.assertEqual([(item["start"], item["end"]) for item in entities], [(0, 3), (11, 14)])

    def test_generated_value_not_present_in_source_is_discarded(self) -> None:
        entities = qwen.align_values(
            "联系人为沈晓宁。",
            [{"text": "沈小宁", "label": "PERSON_NAME"}],
        )
        self.assertEqual(entities, [])


class MergeTests(unittest.TestCase):
    def test_trusted_wider_rule_span_replaces_overlapping_model_label(self) -> None:
        rules_found = [
            {
                "start": 10,
                "end": 24,
                "label": "SALARY_AMOUNT",
                "text": "人民币168,260.00元",
            }
        ]
        model_found = [
            {
                "start": 13,
                "end": 23,
                "label": "FINANCIAL_AMOUNT",
                "text": "168,260.00",
            }
        ]
        self.assertEqual(
            merger.merge_entities(rules_found, model_found),
            rules_found,
        )

    def test_customer_context_overrides_model_organization_label(self) -> None:
        qwen_found = [
            {
                "start": 8,
                "end": 17,
                "label": "ORG_NAME",
                "text": "星河智联供应链集团",
                "detector": "qwen3.5-4b-zero-shot",
            }
        ]
        siamese_found = [
            {
                "start": 8,
                "end": 17,
                "label": "CUSTOMER_NAME",
                "text": "星河智联供应链集团",
                "detector": "siamese-uie-zero-shot",
            }
        ]
        self.assertEqual(
            merger.merge_entities(qwen_found, siamese_found),
            siamese_found,
        )


class SiameseAdapterTests(unittest.TestCase):
    def test_customer_context_relabels_organization(self) -> None:
        text = "重点客户名单包含星河智联供应链集团。"
        findings = siamese.normalize_findings(
            text,
            {
                "output": [
                    [
                        {
                            "type": "组织机构",
                            "span": "星河智联供应链集团",
                            "offset": [8, 17],
                        }
                    ]
                ]
            },
        )
        self.assertEqual(findings[0]["label"], "CUSTOMER_NAME")


if __name__ == "__main__":
    unittest.main()
