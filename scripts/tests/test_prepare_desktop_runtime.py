from __future__ import annotations

import hashlib
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "prepare-desktop-runtime.py"


def digest(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


class PrepareDesktopRuntimeTests(unittest.TestCase):
    def make_fixture(self, directory: Path) -> tuple[Path, Path]:
        payload = directory / "payload"
        files = {
            "ocr/python": b"embedded python",
            "ocr/sidecar/main.py": b"print('offline ocr')\n",
            "ocr/models/det/inference.onnx": b"detector weights",
            "ocr/models/rec/inference.onnx": b"recognizer weights",
            "pdf/bin/pdfinfo": b"pinned pdfinfo",
            "pdf/bin/pdftoppm": b"pinned pdftoppm",
            "pdf/lib/poppler.fixture": b"pinned poppler dependency",
        }
        for relative, content in files.items():
            path = payload / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(content)
        recipe = {
            "schema_version": 1,
            "detectors": [
                {
                    "id": "pp_ocr_small",
                    "kind": "ocr",
                    "executable": "ocr/python",
                    "working_directory": "ocr",
                    "args": [
                        "sidecar/main.py",
                        "--tier",
                        "small",
                        "--detector-model",
                        "models/det",
                        "--recognizer-model",
                        "models/rec",
                    ],
                    "assets": ["ocr/sidecar", "ocr/models"],
                    "timeout_ms": 120000,
                }
            ],
            "tools": [
                {
                    "kind": "pdfinfo",
                    "executable": "pdf/bin/pdfinfo",
                    "assets": ["pdf/lib"],
                },
                {
                    "kind": "pdftoppm",
                    "executable": "pdf/bin/pdftoppm",
                    "assets": ["pdf/lib"],
                },
            ],
        }
        recipe_path = directory / "recipe.json"
        recipe_path.write_text(json.dumps(recipe), encoding="utf-8")
        return payload, recipe_path

    def run_script(self, recipe: Path, payload: Path, output: Path) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "--recipe",
                str(recipe),
                "--payload",
                str(payload),
                "--output",
                str(output),
            ],
            check=False,
            capture_output=True,
            text=True,
        )

    def test_builds_relative_hash_pinned_runtime_resources(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            payload, recipe = self.make_fixture(directory)
            output = directory / "runtime-payload"

            result = self.run_script(recipe, payload, output)

            self.assertEqual(result.returncode, 0, result.stderr)
            registry = json.loads((output / "default.json").read_text(encoding="utf-8"))
            self.assertEqual(registry["schema_version"], 1)
            self.assertEqual([tool["kind"] for tool in registry["tools"]], ["pdfinfo", "pdftoppm"])
            self.assertEqual(registry["tools"][0]["sha256"], digest(b"pinned pdfinfo"))
            self.assertEqual(registry["detectors"][0]["executable"], "ocr/python")
            detector_assets = {
                asset["path"]: asset["sha256"] for asset in registry["detectors"][0]["assets"]
            }
            self.assertEqual(detector_assets["ocr/python"], digest(b"embedded python"))
            self.assertTrue((output / "ocr/models/rec/inference.onnx").is_file())
            self.assertTrue((output / "pdf/lib/poppler.fixture").is_file())
            self.assertFalse(
                any(
                    Path(asset["path"]).is_absolute()
                    for asset in registry["detectors"][0]["assets"]
                )
            )

    def test_rejects_paths_outside_the_payload(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            payload, recipe = self.make_fixture(directory)
            value = json.loads(recipe.read_text(encoding="utf-8"))
            value["detectors"][0]["executable"] = "../outside-python"
            recipe.write_text(json.dumps(value), encoding="utf-8")
            output = directory / "runtime-payload"

            result = self.run_script(recipe, payload, output)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("must stay inside the payload directory", result.stderr)
            self.assertFalse(output.exists())

    def test_refuses_to_overwrite_an_existing_output(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            payload, recipe = self.make_fixture(directory)
            output = directory / "runtime-payload"
            output.mkdir()
            sentinel = output / "keep.txt"
            sentinel.write_text("keep", encoding="utf-8")

            result = self.run_script(recipe, payload, output)

            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(sentinel.read_text(encoding="utf-8"), "keep")

    def test_rejects_symlinked_payload_components(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            payload, recipe = self.make_fixture(directory)
            link = payload / "ocr/python-link"
            try:
                link.symlink_to(payload / "ocr/python")
            except OSError as error:
                self.skipTest(f"symbolic links are unavailable: {error}")
            value = json.loads(recipe.read_text(encoding="utf-8"))
            value["detectors"][0]["executable"] = "ocr/python-link"
            recipe.write_text(json.dumps(value), encoding="utf-8")
            output = directory / "runtime-payload"

            result = self.run_script(recipe, payload, output)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("symbolic link", result.stderr)
            self.assertFalse(output.exists())


if __name__ == "__main__":
    unittest.main()
