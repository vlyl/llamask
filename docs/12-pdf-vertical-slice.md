# PDF 纵向切片与安全边界

文档状态：V0.1，全页安全栅格化基线已实现（2026-08-01）

## 1. 已完成的用户流程

当前 CLI 已跑通完整的 PDF 脱敏事务：

1. 限量读取 PDF，把源字节复制到随机临时目录，避免原路径出现在 PDF 工具
   的进程参数中。
2. 使用本地 `pdfinfo` 检查页数、加密、表单和 JavaScript 状态。
3. 使用本地 `pdftoppm` 在固定 200 DPI 下逐页渲染 PNG。
4. 每页复用图片 OCR、规则和可选本地文本模型，生成可编辑 `mask_rect`。
5. 用户可以修改每页的 `selected`、`reviewed` 和 `mask_rect`，也可以让全部
   高置信自动结果直接进入批量导出。
6. 导出时重新渲染源文件并核对每页 PNG 的 SHA-256、格式和尺寸，防止扫描
   后源文件或渲染结果变化。
7. 在页面像素上烧录实心遮罩，再把所有页面编码为 JPEG 95，从零生成只含
   页面图像的新 PDF。
8. 候选 PDF 重新解析结构、重新逐页渲染并再次 OCR；全部通过后原子落盘。

任务 JSON 包含 OCR 原文和命中值，应当视为敏感文件。验证报告只包含页码、
坐标、类型、检测器和错误码，不回显残留原文。

## 2. 为什么首版全部栅格化

PDF 可以同时保存可见文字、不可见文字层、批注、表单、附件、脚本、可选
内容层、对象流、增量保存历史和数字签名。只在页面上盖一个黑色矩形并不
会删除底层值；复制、搜索、移除遮罩或解析对象仍可能恢复原文。

首版采用安全优先的最小可信实现：不修改或复制源对象树，只把经过打码的
页面像素写入一个新 PDF。这样可以用同一条规则消除未知容器载荷，并把
验证范围收敛为页面图像和少量可审计的 PDF 对象。

该取舍符合“尽量保持原样”的视觉目标，但明确牺牲：

- 文字搜索和复制。
- 字体、路径和其他矢量对象的编辑。
- 表单字段和链接交互。
- 批注内容、附件、脚本、隐藏文字和可选内容层。
- 源文件元数据、增量历史和数字签名；原签名不会保留有效性。

对象保留与干净搜索层重建仍是后续模式，不能用来降低当前安全模式的验收
标准。

## 3. 输入和运行限制

| 项目 | 当前上限或行为 |
| --- | --- |
| PDF 文件 | 100 MiB |
| 页数 | 200 页 |
| 渲染总像素 | 5 亿像素 |
| 页面 DPI | 固定 200 DPI |
| 加密 PDF | 拒绝；要求用户先在受信任环境生成解密副本 |
| 输出压缩 | 页面 JPEG 质量 95 |
| 源文件变化 | SHA-256 不一致则拒绝导出 |
| 页面渲染变化 | 每页 PNG 哈希、尺寸或格式不一致则拒绝导出 |
| 覆盖行为 | 不覆盖原件或已有输出 |

PDF 工具由 `LLAMASK_PDFINFO` 和 `LLAMASK_PDFTOPPM` 环境变量选择；省略时
分别使用 `pdfinfo`、`pdftoppm`。工具标准输出和标准错误均有限额，执行有
超时；元数据等 `pdfinfo` 原始输出只在内存中解析，不写入普通日志。发布包
需要捆绑并固定经过双平台回归的 Poppler 工具，同时完成其许可证复核。

## 4. 任务与手动复核

`PdfTaskDraft.pages[]` 中的每一页都是独立的 `ImageTaskDraft`：

- `page_number` 是一开始的源页码。
- `ocr_rect` 是 OCR 返回的原始行框。
- `mask_rect` 是用户可以调整的最终遮罩框。
- `group_id` 把跨行的一个逻辑命中合并计数；PDF 计数还包含页码，避免不同
  页面上相同的局部 id 冲突。
- `selected` 决定是否烧录遮罩，`reviewed` 决定是否允许导出。

比例字体会使字符比例估算出现一个字符左右的误差。核心除策略像素边距外，
还在书写方向增加半个 OCR 行高/行宽的安全余量；旋转或无法可靠细分的文字
仍回退到更大的行框。安全余量可能覆盖紧邻值的空格或标点，用户可在预览
中修改，但缩小遮罩后仍必须通过成品 OCR 复检。

## 5. 从零构造的输出结构

每个输出页面只包含：

- 一个 `/Page` 对象。
- 一个 `/Image` XObject，`DeviceRGB`、8 bit、`DCTDecode`。
- 一个固定形式的内容流，只执行 `q`、变换矩阵、`/Im0 Do` 和 `Q`。

输出不写 `/Info`，也不允许 `/Metadata`、`/Annots`、`/AcroForm`、
`/EmbeddedFiles`、`/JavaScript`、`/JS`、`/OpenAction`、`/AA`、`/Names`、
`/ObjStm`、`/OCProperties`、`/RichMedia`、`/XFA`、`/Sig` 或加密字典。
结构复检要求所有 stream 使用直接长度；图片流必须是 JPEG，非图片流必须
严格匹配固定的单图绘制指令。无法证明属于该白名单结构的候选输出不落盘。

## 6. 独立验证与报告语义

候选副本在落盘前执行：

- PDF 魔数、大小、页数和加密状态复核。
- 表单和 JavaScript 状态复核。
- PDF 非 stream 对象和每个 stream 类型的严格结构检查。
- 每一页按任务 DPI 重新渲染，并核对像素尺寸。
- 每页已选原值的目标残留检查。
- 每页重新运行 OCR、规则、精确词和已配置本地文本模型。
- 用户明确复核为保留的命中按任务快照排除。

`passed: true` 表示当前结构白名单通过、没有未复核结果且当前检测链没有
发现残留。`complete: true` 进一步表示策略启用的所有可选本地模型都实际
参与复扫。只提供 OCR 注册表时，规则与 OCR 可以使 `passed` 为真，但默认
策略中的 SiameseUIE/Qwen 未运行会令 `complete` 为假并产生明确诊断。

## 7. 回归样本与结果

`fixtures/pdf/comprehensive.pdf` 只包含合成数据，覆盖：

- 原生文字中的手机号和邮箱。
- 带敏感值的 AcroForm 字段。
- 文本批注和外部链接。
- 整页扫描图中的身份证号和邮箱。
- 不可见文字。
- 敏感元数据、附件和 JavaScript。

真实 PP-OCRv6 small 端到端得到 5 个高置信逻辑命中和 5 个遮罩。导出及
再次独立验证的目标残留均为 0，结构净化通过。外部 PDF 解析检查得到 3 页、
0 个字段、0 个批注、0 个附件、无 JavaScript、无元数据、每页可提取字符数
均为 0；成品逐页渲染后已目视确认遮罩覆盖和页面布局。

样本可通过仓库脚本重建和只输出结构计数的检查器复核：

```bash
python3 evaluation/scripts/generate_pdf_fixture.py fixtures/pdf/comprehensive.pdf
python3 evaluation/scripts/inspect_pdf_fixture.py fixtures/pdf/comprehensive.pdf
```

## 8. 命令

```bash
cargo run -p llamask -- scan-pdf sample.pdf --task task.json \
  --policy config/policies/default.json \
  --runtimes config/runtimes/development-ocr-small.json
cargo run -p llamask -- export-pdf task.json --output sample_已脱敏.pdf \
  --runtimes config/runtimes/development-ocr-small.json
cargo run -p llamask -- verify-pdf task.json sample_已脱敏.pdf \
  --runtimes config/runtimes/development-ocr-small.json
```

扫描、导出和验证都必须提供含同一 OCR id 的注册表；扫描时可用
`--ocr-runtime` 选择非默认运行项。

## 9. 下一阶段

- 双平台捆绑并固定 Poppler 版本，补充恶意、损坏、超大、旋转和非 A4 PDF。
- 在 macOS Preview、Adobe Acrobat 和 Windows 常用阅读器上做真实文件回归。
- 增加逐页预览、缩放、手动画框和页面级安全模式提示。
- 研究可证明删除底层对象的原生文字保留模式。
- 对栅格化页面重建只含已通过复检文本的干净搜索层。
- 在保持安全回退的前提下实现逐页混合导出。
