from __future__ import annotations

import unittest

from sidecars.siamese_uie.main import normalize_findings, validate_request


class SiameseSidecarTests(unittest.TestCase):
    def test_normalizes_unicode_offsets_and_customer_label(self) -> None:
        text = "重点客户🙂星海科技有限公司"
        raw = {
            "output": [
                [
                    {
                        "type": "组织机构",
                        "span": "星海科技有限公司",
                        "offset": [5, 13],
                    }
                ]
            ]
        }
        findings = normalize_findings(text, raw, 0.85)
        self.assertEqual(len(findings), 1)
        self.assertEqual(findings[0]["entity_type"], "CUSTOMER_NAME")
        self.assertEqual(findings[0]["matched_text"], "星海科技有限公司")
        self.assertEqual(findings[0]["confidence"], 0.85)

    def test_discards_a_span_that_does_not_match_source(self) -> None:
        raw = {
            "output": [
                {
                    "type": "人物",
                    "span": "模型编造",
                    "offset": [0, 2],
                }
            ]
        }
        self.assertEqual(normalize_findings("联系人沈景行", raw, 0.85), [])

    def test_rejects_wrong_protocol(self) -> None:
        request = {
            "protocol_version": 2,
            "request_id": "request-1",
            "detector_id": "siamese_uie",
            "document_id": "document-1",
            "part_id": "part-1",
            "offset_unit": "unicode_scalar",
            "text": "测试",
            "candidates": [],
        }
        with self.assertRaisesRegex(ValueError, "protocol mismatch"):
            validate_request(request, "siamese_uie", 20_000)


if __name__ == "__main__":
    unittest.main()
