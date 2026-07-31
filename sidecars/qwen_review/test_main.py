from __future__ import annotations

import unittest

from sidecars.qwen_review.main import (
    align_entities,
    entities_for_request,
    first_json_object,
    validate_request,
)


class QwenSidecarTests(unittest.TestCase):
    def test_extracts_first_balanced_json_object(self) -> None:
        output = 'prompt echo\n{"entities":[{"text":"A}B","label":"ORG_NAME"}]}\n> EOF'
        parsed = first_json_object(output)
        self.assertEqual(parsed["entities"][0]["text"], "A}B")

    def test_aligns_every_exact_unicode_occurrence(self) -> None:
        text = "沈景行🙂联系沈景行"
        findings = align_entities(
            text,
            {"entities": [{"text": "沈景行", "label": "PERSON_NAME"}]},
            0.80,
        )
        self.assertEqual([(item["start"], item["end"]) for item in findings], [(0, 3), (6, 9)])

    def test_discards_a_generated_value_absent_from_source(self) -> None:
        self.assertEqual(
            align_entities(
                "联系人沈景行",
                {"entities": [{"text": "模型编造", "label": "PERSON_NAME"}]},
                0.80,
            ),
            [],
        )

    def test_rejects_wrong_offset_unit(self) -> None:
        request = {
            "protocol_version": 1,
            "request_id": "request-1",
            "detector_id": "qwen_review",
            "document_id": "document-1",
            "part_id": "part-1",
            "offset_unit": "utf16",
            "text": "测试",
            "candidates": [],
        }
        with self.assertRaisesRegex(ValueError, "offset unit mismatch"):
            validate_request(request, "qwen_review", 2_000)

    def test_requires_the_same_request_id(self) -> None:
        with self.assertRaisesRegex(ValueError, "mismatch"):
            entities_for_request(
                {"items": [{"id": "wrong", "entities": []}]},
                "request-1",
            )


if __name__ == "__main__":
    unittest.main()
