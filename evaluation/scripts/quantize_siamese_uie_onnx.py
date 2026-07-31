#!/usr/bin/env python3
"""Quantize the exported SiameseUIE graph and verify entity decisions."""

from __future__ import annotations

import argparse
import json
import os
import tempfile
import time
from pathlib import Path

import numpy as np
import onnx
import onnxruntime as ort
from onnxruntime.quantization import QuantType, quantize_dynamic

from export_siamese_uie_onnx import (
    HINTS,
    SAMPLE_TEXTS,
    configure_offline_environment,
    decode_entities,
    file_sha256,
    tensor_inputs_to_numpy,
    tokenize_inputs,
)


DEFAULT_INPUT = Path(
    "evaluation/models/cache/siamese-uie-chinese-base-onnx/model.onnx"
)
DEFAULT_OUTPUT = Path(
    "evaluation/models/cache/siamese-uie-chinese-base-onnx/model-int8.onnx"
)
DEFAULT_REPORT = Path(
    "evaluation/models/cache/siamese-uie-chinese-base-onnx/"
    "quantization-report.json"
)
DEFAULT_SOURCE_MODEL = Path(
    "evaluation/models/cache/siamese-uie-chinese-base"
)


def quantize_model(input_path: Path, output_path: Path) -> float:
    output_path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        prefix=f".{output_path.stem}-",
        suffix=".onnx",
        dir=output_path.parent,
        delete=False,
    ) as temporary:
        temporary_path = Path(temporary.name)
    temporary_path.unlink()
    started = time.perf_counter()
    try:
        quantize_dynamic(
            model_input=input_path,
            model_output=temporary_path,
            op_types_to_quantize=["MatMul", "Gemm"],
            per_channel=True,
            weight_type=QuantType.QInt8,
            use_external_data_format=False,
        )
        onnx.checker.check_model(str(temporary_path))
        os.replace(temporary_path, output_path)
    finally:
        temporary_path.unlink(missing_ok=True)
    return (time.perf_counter() - started) * 1000


def verify_quantized_model(
    extractor: object,
    fp32_path: Path,
    int8_path: Path,
) -> tuple[list[dict[str, object]], float, float]:
    fp32_session = ort.InferenceSession(
        str(fp32_path),
        providers=["CPUExecutionProvider"],
    )
    int8_session = ort.InferenceSession(
        str(int8_path),
        providers=["CPUExecutionProvider"],
    )
    samples: list[dict[str, object]] = []
    maximum_error = 0.0
    total_absolute_error = 0.0
    compared_values = 0
    for text in SAMPLE_TEXTS:
        inputs, offsets = tokenize_inputs(extractor, text)
        numpy_inputs = tensor_inputs_to_numpy(inputs)
        expected = fp32_session.run(None, numpy_inputs)
        started = time.perf_counter()
        actual = int8_session.run(None, numpy_inputs)
        runtime_ms = (time.perf_counter() - started) * 1000
        sample_errors = [
            np.abs(expected_value - actual_value)
            for expected_value, actual_value in zip(expected, actual)
        ]
        sample_maximum_error = max(
            float(np.max(error)) for error in sample_errors
        )
        maximum_error = max(maximum_error, sample_maximum_error)
        total_absolute_error += sum(
            float(np.sum(error)) for error in sample_errors
        )
        compared_values += sum(error.size for error in sample_errors)
        fp32_entities = decode_entities(
            extractor,
            text,
            offsets,
            expected[0],
            expected[1],
            HINTS,
        )
        int8_entities = decode_entities(
            extractor,
            text,
            offsets,
            actual[0],
            actual[1],
            HINTS,
        )
        if int8_entities != fp32_entities:
            raise RuntimeError("INT8 entity output differs from FP32 ONNX")
        samples.append(
            {
                "text": text,
                "runtime_ms": runtime_ms,
                "max_abs_logit_error": sample_maximum_error,
                "entities": int8_entities,
            }
        )
    mean_absolute_error = total_absolute_error / compared_values
    return samples, maximum_error, mean_absolute_error


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--input", type=Path, default=DEFAULT_INPUT)
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument("--report", type=Path, default=DEFAULT_REPORT)
    parser.add_argument("--source-model", type=Path, default=DEFAULT_SOURCE_MODEL)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    input_path = args.input.resolve(strict=True)
    output_path = args.output.resolve()
    report_path = args.report.resolve()
    source_model = args.source_model.resolve(strict=True)
    configure_offline_environment(source_model.parent)

    from modelscope.pipelines import pipeline
    from modelscope.utils.constant import Tasks

    extractor = pipeline(
        Tasks.siamese_uie,
        model=str(source_model),
        device="cpu",
        trust_remote_code=True,
    )
    quantization_ms = quantize_model(input_path, output_path)
    samples, maximum_error, mean_error = verify_quantized_model(
        extractor,
        input_path,
        output_path,
    )
    input_size = input_path.stat().st_size
    output_size = output_path.stat().st_size
    report = {
        "format": "onnx-int8-dynamic",
        "input": str(input_path),
        "output": str(output_path),
        "quantized_ops": ["MatMul", "Gemm"],
        "per_channel": True,
        "weight_type": "QInt8",
        "quantization_ms": quantization_ms,
        "input_size_bytes": input_size,
        "output_size_bytes": output_size,
        "size_ratio": output_size / input_size,
        "sha256": file_sha256(output_path),
        "verification": {
            "provider": "CPUExecutionProvider",
            "max_abs_logit_error": maximum_error,
            "mean_abs_logit_error": mean_error,
            "entity_outputs_equal": True,
            "samples": samples,
        },
        "versions": {
            "onnx": onnx.__version__,
            "onnxruntime": ort.__version__,
        },
    }
    report_path.parent.mkdir(parents=True, exist_ok=True)
    report_path.write_text(
        json.dumps(report, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )
    print(json.dumps(report, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
