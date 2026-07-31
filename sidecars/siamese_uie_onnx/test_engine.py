from __future__ import annotations

import unittest

import numpy as np

from sidecars.siamese_uie_onnx.engine import (
    get_entities,
    merge_window_logits,
    split_windows,
)


class SiameseOnnxEngineTests(unittest.TestCase):
    def test_splits_with_the_same_overlap_and_last_window_padding(self) -> None:
        windows = split_windows(list(range(10)), 6, 4, 0)
        self.assertEqual(
            windows,
            [
                (0, [0, 1, 2, 3, 4, 5]),
                (4, [4, 5, 6, 7, 8, 9]),
            ],
        )
        padded = split_windows(list(range(11)), 6, 4, 0)
        self.assertEqual(padded[-1], (8, [8, 9, 10, 0, 0, 0]))

    def test_merges_overlapping_logits(self) -> None:
        first = np.asarray([[1.0, 2.0, 3.0]], dtype=np.float32)
        second = np.asarray([[5.0, 6.0]], dtype=np.float32)
        merged = merge_window_logits([(0, first), (2, second)], 4)
        np.testing.assert_allclose(merged, [[1.0, 2.0, 4.0, 6.0]])

    def test_decodes_python_unicode_offsets(self) -> None:
        entities = get_entities(
            "甲🙂沈景行乙",
            [(0, 0), (0, 1), (1, 2), (2, 3), (3, 4), (4, 5), (5, 6)],
            np.asarray([-2, -2, -2, 1, -2, -2, -2], dtype=np.float32),
            np.asarray([-2, -2, -2, -2, -2, 1, -2], dtype=np.float32),
            0.5,
        )
        self.assertEqual(
            entities,
            [{"span": "沈景行", "offset": [2, 5]}],
        )


if __name__ == "__main__":
    unittest.main()
