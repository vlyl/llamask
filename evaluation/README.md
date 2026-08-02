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
    ├── generate_xlsx_fixture.mjs
    ├── prepare_xlsx_fixture.py
    ├── inspect_xlsx_fixture.mjs
    ├── generate_pptx_fixture.mjs
    ├── prepare_pptx_fixture.py
    ├── inspect_pptx_fixture.mjs
    ├── generate_pdf_fixture.py
    ├── inspect_pdf_fixture.py
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

XLSX 合成回归样本分两步构建：先用工作簿引擎生成可正常打开的基础文件，
再确定性加入共享字符串、传统批注、页眉和隐藏工作表等 OOXML 边界情况。
最后用同一工作簿引擎重新导入、检查公式并渲染预览：

```bash
node evaluation/scripts/generate_xlsx_fixture.mjs /tmp/llamask-xlsx-base.xlsx
python3 evaluation/scripts/prepare_xlsx_fixture.py \
  /tmp/llamask-xlsx-base.xlsx fixtures/xlsx/comprehensive.xlsx
node evaluation/scripts/inspect_xlsx_fixture.mjs \
  fixtures/xlsx/comprehensive.xlsx /tmp/llamask-xlsx-preview.png
```

这三个 JavaScript 脚本需要开发环境中的 `@oai/artifact-tool`。提交的
`fixtures/xlsx/comprehensive.xlsx` 不依赖该工具即可运行 Rust 回归测试，
且只包含合成号码和邮箱。

PPTX 合成回归同样先由演示文稿引擎生成，再确定性加入隐藏页、母版和版式
边界；最后重新导入、逐页渲染并执行溢出检查：

```bash
node evaluation/scripts/generate_pptx_fixture.mjs \
  /tmp/llamask-pptx-base.pptx /tmp/llamask-pptx-base-preview
python3 evaluation/scripts/prepare_pptx_fixture.py \
  /tmp/llamask-pptx-base.pptx fixtures/pptx/comprehensive.pptx
node evaluation/scripts/inspect_pptx_fixture.mjs \
  fixtures/pptx/comprehensive.pptx /tmp/llamask-pptx-preview
```

可在生成命令末尾增加一个 PNG 路径，构造嵌入图片 OCR 端到端样本。提交的
`fixtures/pptx/comprehensive.pptx` 不含图片和真实信息，覆盖跨 run、表格、
备注、批注及回复、隐藏页、母版、版式和外部超链接。

PDF 合成回归样本覆盖原生文字、AcroForm、批注、链接、整页扫描图、不可见
文字、元数据、附件和 JavaScript。生成器使用固定 PDF 时间信息；检查器只
输出结构计数、对象类型和每页提取字符数，不打印敏感测试值：

```bash
python3 evaluation/scripts/generate_pdf_fixture.py fixtures/pdf/comprehensive.pdf
python3 evaluation/scripts/inspect_pdf_fixture.py fixtures/pdf/comprehensive.pdf
pdftoppm -png -r 150 fixtures/pdf/comprehensive.pdf /tmp/llamask-pdf-preview
```

提交的样本只含合成号码和邮箱。PDF 成品仍需逐页渲染目视检查，不能只依赖
对象计数和 OCR 通过状态。

## 数据使用限制

- 合成数据用于管线回归和初筛，不足以单独做最终模型决定。
- 冻结模型前，需要补充合法授权、去标识化并经人工复核的真实版式样本。
- 禁止把客户原始隐私、恢复映射或生产日志放入本目录。
- 测试样本中的号码和名称是程序生成的虚构内容，不代表真实个人或组织。
