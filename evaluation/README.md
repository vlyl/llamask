# LlaMask 模型评测

这里是产品开发前的模型验证工作区。它只用于决定模型组合，不包含桌面产品代码。

## 目标

- 所有候选模型使用同一份中文脱敏数据。
- 分开统计各敏感信息类型，避免总平均掩盖高风险漏检。
- 同时记录准确度、span 对齐、结构化输出、耗时、内存和模型体积。
- 数据、模型、推理参数和结果都可复现。

## 目录

```text
evaluation/
├── datasets/
│   ├── README.md
│   └── generated/          # 脚本生成，不提交
├── models/
│   ├── manifest.json
│   └── cache/              # 模型权重，不提交
├── runs/                   # 模型输出和评分报告，不提交
├── schemas/
│   └── case.schema.json
└── scripts/
    ├── generate_synthetic_zh.py
    ├── generate_visual_ocr_dataset.py
    ├── generate_visual_ocr_hard_dataset.py
    ├── generate_docx_fixture.py
    ├── validate_dataset.py
    ├── run_rules_baseline.py
    ├── run_ocr_benchmark.py
    ├── score_ocr_predictions.py
    └── score_predictions.py
```

## 第一轮评测组

1. 规则和校验器。
2. 规则 + Qwen3.5-4B。
3. 规则 + SiameseUIE Chinese base + Qwen3.5-4B。
4. 规则 + PP-UIE-0.5B + Qwen3.5-4B。
5. PP-OCRv6 small 与 medium。
6. Qwen3.5-4B BF16 与候选 Q4。

Qwen3.5-9B 暂不下载。只有 4B 在复杂语义子集上接近但未达到门槛时，再加入 9B 对照。

首轮冻结结果见 [模型评测与首发冻结结论](../docs/07-model-benchmark-results.md)。
最终选择为上下文规则、SiameseUIE、Qwen3.5-4B Q4_K_M，以及
PP-OCRv6 small/medium 双档；PP-UIE、9B 和版面模型暂缓。
SiameseUIE 已有 FP32 ONNX 导出、量化和纯 ONNX 评测脚本；默认保留
与 PyTorch 冻结预测一致的 FP32 图，INT8 仅作为语义漂移实验。

## 快速运行

```bash
python3 evaluation/scripts/generate_synthetic_zh.py
python3 evaluation/scripts/validate_dataset.py \
  evaluation/datasets/generated/synthetic_zh_v2.jsonl
python3 evaluation/scripts/run_rules_baseline.py \
  evaluation/datasets/generated/synthetic_zh_v2.jsonl \
  evaluation/runs/rules/predictions.jsonl
python3 evaluation/scripts/score_predictions.py \
  evaluation/datasets/generated/synthetic_zh_v2.jsonl \
  evaluation/runs/rules/predictions.jsonl \
  evaluation/runs/rules/report

python3 evaluation/scripts/generate_visual_ocr_dataset.py
python3 evaluation/scripts/generate_visual_ocr_hard_dataset.py
python3 evaluation/scripts/run_ocr_benchmark.py \
  evaluation/datasets/generated/ocr_zh_v2.jsonl \
  evaluation/runs/ocr-small/predictions.jsonl \
  --tier small
python3 evaluation/scripts/score_ocr_predictions.py \
  evaluation/datasets/generated/ocr_zh_v2.jsonl \
  evaluation/runs/ocr-small/predictions.jsonl \
  evaluation/runs/ocr-small/report
```

生成器固定随机种子。每次生成后会显示记录数和 SHA-256；哈希不一致表示数据定义发生了变化。

使用 `run_llamacpp_benchmark.py` 的 `--split` 或 `--limit` 运行子集时，
评分脚本需要传入相同参数，避免把未运行的记录误计为漏检。

OCR 运行器会显式关闭模型源联网检查，并且只接受已经下载到本地的
ONNX 模型目录。`ocr_zh_v2` 用于基础回归；`ocr_zh_hard_v2` 加入透视、
阴影、印章遮挡、表格线、低分辨率和强压缩，只用于压力测试。

DOCX 合成回归样本可以确定性重建：

```bash
python3 evaluation/scripts/generate_docx_fixture.py \
  fixtures/docx/comprehensive.docx
```

样本包含跨 run、表格、页眉页脚、脚注、批注、修订删除、字段代码和隐私
元数据。它不含真实个人或组织信息。

## 数据使用限制

- 合成数据用于管线回归和初筛，不足以单独做最终模型决定。
- 冻结模型前，需要补充合法授权、去标识化并经人工复核的真实版式样本。
- 禁止把客户原始隐私、恢复映射或生产日志放入本目录。
- 测试样本中的号码和名称是程序生成的虚构内容，不代表真实个人或组织。
