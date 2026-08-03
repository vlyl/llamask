#!/usr/bin/env python3
"""Build a hash-pinned LlaMask desktop runtime resource directory."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import tempfile
from pathlib import Path, PurePosixPath
from typing import Any

REQUIRED_TOOLS = {"pdfinfo", "pdftoppm"}
VALID_DETECTOR_KINDS = {"information_extraction", "llm_review", "ocr"}


class RecipeError(ValueError):
    pass


def load_object(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise RecipeError("runtime recipe is not readable JSON") from error
    if not isinstance(value, dict):
        raise RecipeError("runtime recipe must be a JSON object")
    return value


def relative_path(value: Any, field: str) -> Path:
    if not isinstance(value, str) or not value or "\\" in value or ":" in value:
        raise RecipeError(f"{field} must be a portable relative path")
    pure = PurePosixPath(value)
    if pure.is_absolute() or any(part in {"", ".", ".."} for part in pure.parts):
        raise RecipeError(f"{field} must stay inside the payload directory")
    return Path(*pure.parts)


def source_path(payload: Path, value: Any, field: str, *, directory: bool = False) -> Path:
    relative = relative_path(value, field)
    candidate = payload.joinpath(relative)
    current = payload
    for part in relative.parts:
        current = current.joinpath(part)
        if current.is_symlink():
            raise RecipeError(f"{field} cannot contain a symbolic link")
    if directory:
        if not candidate.is_dir():
            raise RecipeError(f"{field} directory is missing")
    elif not candidate.is_file():
        raise RecipeError(f"{field} file is missing")
    try:
        candidate.resolve(strict=True).relative_to(payload.resolve(strict=True))
    except (OSError, ValueError) as error:
        raise RecipeError(f"{field} escapes the payload directory") from error
    return relative


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        while chunk := handle.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def expand_assets(payload: Path, values: Any, field: str) -> set[Path]:
    if not isinstance(values, list):
        raise RecipeError(f"{field} must be an array")
    assets: set[Path] = set()
    for index, value in enumerate(values):
        relative = relative_path(value, f"{field}[{index}]")
        candidate = payload.joinpath(relative)
        if candidate.is_symlink():
            raise RecipeError(f"{field}[{index}] cannot be a symbolic link")
        if candidate.is_file():
            source_path(payload, value, f"{field}[{index}]")
            assets.add(relative)
            continue
        if not candidate.is_dir():
            raise RecipeError(f"{field}[{index}] is missing")
        source_path(payload, value, f"{field}[{index}]", directory=True)
        for child in sorted(candidate.rglob("*")):
            if child.is_symlink():
                raise RecipeError(f"{field}[{index}] contains a symbolic link")
            if child.is_file():
                assets.add(child.relative_to(payload))
    return assets


def asset_records(payload: Path, paths: set[Path]) -> list[dict[str, str]]:
    return [
        {
            "path": path.as_posix(),
            "sha256": sha256_file(payload.joinpath(path)),
        }
        for path in sorted(paths, key=lambda item: item.as_posix())
    ]


def build_registry(recipe: dict[str, Any], payload: Path) -> tuple[dict[str, Any], set[Path], set[Path]]:
    if recipe.get("schema_version") != 1:
        raise RecipeError("only desktop runtime recipe schema_version=1 is supported")
    detectors_value = recipe.get("detectors")
    tools_value = recipe.get("tools")
    if not isinstance(detectors_value, list) or not detectors_value:
        raise RecipeError("the desktop runtime requires at least one detector")
    if not isinstance(tools_value, list):
        raise RecipeError("the desktop runtime tools field must be an array")

    detector_ids: set[str] = set()
    has_ocr = False
    copied_files: set[Path] = set()
    copied_directories: set[Path] = set()
    detectors: list[dict[str, Any]] = []
    for index, value in enumerate(detectors_value):
        if not isinstance(value, dict):
            raise RecipeError(f"detectors[{index}] must be an object")
        detector_id = value.get("id")
        kind = value.get("kind")
        timeout_ms = value.get("timeout_ms")
        args = value.get("args", [])
        if not isinstance(detector_id, str) or not detector_id.strip():
            raise RecipeError(f"detectors[{index}].id is invalid")
        if detector_id in detector_ids:
            raise RecipeError(f"detector id is duplicated: {detector_id}")
        detector_ids.add(detector_id)
        if kind not in VALID_DETECTOR_KINDS:
            raise RecipeError(f"detectors[{index}].kind is invalid")
        has_ocr |= kind == "ocr"
        if not isinstance(timeout_ms, int) or not 100 <= timeout_ms <= 600_000:
            raise RecipeError(f"detectors[{index}].timeout_ms is invalid")
        if not isinstance(args, list) or not all(isinstance(arg, str) for arg in args):
            raise RecipeError(f"detectors[{index}].args must contain strings")
        executable = source_path(
            payload, value.get("executable"), f"detectors[{index}].executable"
        )
        assets = expand_assets(payload, value.get("assets", []), f"detectors[{index}].assets")
        assets.add(executable)
        working_directory_value = value.get("working_directory")
        working_directory = None
        if working_directory_value is not None:
            working_directory = source_path(
                payload,
                working_directory_value,
                f"detectors[{index}].working_directory",
                directory=True,
            )
            copied_directories.add(working_directory)
        copied_files.update(assets)
        detector = {
            "id": detector_id,
            "kind": kind,
            "executable": executable.as_posix(),
            "args": args,
            "assets": asset_records(payload, assets),
            "timeout_ms": timeout_ms,
        }
        if working_directory is not None:
            detector["working_directory"] = working_directory.as_posix()
        detectors.append(detector)
    if not has_ocr:
        raise RecipeError("the desktop runtime requires an OCR detector")

    tool_kinds: set[str] = set()
    tools: list[dict[str, Any]] = []
    for index, value in enumerate(tools_value):
        if not isinstance(value, dict):
            raise RecipeError(f"tools[{index}] must be an object")
        kind = value.get("kind")
        if kind not in REQUIRED_TOOLS:
            raise RecipeError(f"tools[{index}].kind is invalid")
        if kind in tool_kinds:
            raise RecipeError(f"tool kind is duplicated: {kind}")
        tool_kinds.add(kind)
        executable = source_path(payload, value.get("executable"), f"tools[{index}].executable")
        assets = expand_assets(payload, value.get("assets", []), f"tools[{index}].assets")
        assets.discard(executable)
        copied_files.add(executable)
        copied_files.update(assets)
        tools.append(
            {
                "kind": kind,
                "executable": executable.as_posix(),
                "sha256": sha256_file(payload.joinpath(executable)),
                "assets": asset_records(payload, assets),
            }
        )
    if tool_kinds != REQUIRED_TOOLS:
        missing = ", ".join(sorted(REQUIRED_TOOLS - tool_kinds))
        raise RecipeError(f"the desktop runtime is missing required tools: {missing}")

    return (
        {"schema_version": 1, "detectors": detectors, "tools": tools},
        copied_files,
        copied_directories,
    )


def prepare(recipe_path: Path, payload: Path, output: Path) -> None:
    if not payload.is_dir():
        raise RecipeError("payload directory is missing")
    if output.exists():
        raise RecipeError("output already exists; remove it explicitly before rebuilding")
    registry, copied_files, copied_directories = build_registry(load_object(recipe_path), payload)
    output.parent.mkdir(parents=True, exist_ok=True)
    staging = Path(tempfile.mkdtemp(prefix=f".{output.name}-", dir=output.parent))
    try:
        for directory in sorted(copied_directories, key=lambda item: item.as_posix()):
            staging.joinpath(directory).mkdir(parents=True, exist_ok=True)
        for relative in sorted(copied_files, key=lambda item: item.as_posix()):
            destination = staging.joinpath(relative)
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(payload.joinpath(relative), destination)
        registry_bytes = json.dumps(registry, ensure_ascii=False, indent=2).encode("utf-8") + b"\n"
        staging.joinpath("default.json").write_bytes(registry_bytes)
        os.replace(staging, output)
    except Exception:
        shutil.rmtree(staging, ignore_errors=True)
        raise


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Prepare hash-pinned OCR and PDF resources for an offline LlaMask desktop bundle."
    )
    parser.add_argument("--recipe", type=Path, required=True)
    parser.add_argument("--payload", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        prepare(args.recipe, args.payload, args.output)
    except RecipeError as error:
        parser.error(str(error))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
