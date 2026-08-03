# LlaMask

[English](README.md) | **简体中文**

[![CI](https://github.com/vlyl/llamask/actions/workflows/ci.yml/badge.svg)](https://github.com/vlyl/llamask/actions/workflows/ci.yml)

> 当前版本：`v0.1.0-alpha.1` 开发者预览。它用于验证脱敏核心、格式适配器和
> 桌面壳，尚未提供面向普通用户的签名安装包，也不包含可直接使用的模型权重。

LlaMask 是一个面向个人与组织的本地离线数据脱敏工具。它在不上传文件、不依赖云端服务的前提下，识别文本、Office 文档和图片中的敏感信息，供用户复核后生成脱敏副本。

项目已完成首轮模型评测，并进入可运行原型开发。当前文本、图片、DOCX、
XLSX、PPTX 和 PDF 纵向切片已经跑通：UTF-8 TXT/Markdown/标准输入、PNG/JPEG、
Office 文档的可见/隐藏文字域、备注、批注、母版及嵌入图片均可执行规则和
本地模型扫描；PDF 可逐页 OCR、编辑遮罩并以全页安全栅格化模式导出。所有
格式均生成可编辑任务草稿、安全副本，并在落盘前独立复扫残留。

## 核心原则

- 完全离线：安装完成后无需联网，运行过程中不产生任何网络请求。
- 原件安全：不覆盖源文件，只生成新副本。
- 高召回优先：宁可多提示可疑内容，也尽量避免遗漏敏感信息。
- 人机协同：高置信度结果可自动处理，模糊结果交由用户确认。
- 可解释：每个检测结果都应说明类型、来源和判定依据。
- 渐进学习：通过本地词典、规则、白名单和用户反馈适应使用习惯。
- 格式保真：尽量保持原文档的排版、公式、样式和可编辑性。

## 设计文档

- [产品需求文档](docs/01-product-requirements.md)
- [技术架构](docs/02-technical-architecture.md)
- [MVP 开发计划](docs/03-mvp-roadmap.md)
- [产品决策记录](docs/04-product-decisions.md)
- [策略配置模型](docs/05-policy-model.md)
- [本地模型选型报告](docs/06-model-selection.md)
- [模型评测与首发冻结结论](docs/07-model-benchmark-results.md)
- [策略与本地模型进程协议](docs/08-policy-and-sidecar-protocol.md)
- [DOCX 纵向切片与安全边界](docs/09-docx-vertical-slice.md)
- [XLSX 纵向切片与安全边界](docs/10-xlsx-vertical-slice.md)
- [PPTX 纵向切片与安全边界](docs/11-pptx-vertical-slice.md)
- [PDF 纵向切片与安全边界](docs/12-pdf-vertical-slice.md)
- [桌面端 MVP 架构与里程碑](docs/13-desktop-mvp-architecture.md)
- [桌面离线运行包契约](docs/14-offline-runtime-packaging.md)

## 当前确定的首发范围

- 面向个人用户与企业用户的完全离线桌面应用。
- 同时支持 Windows 和 macOS。
- 支持 TXT、Markdown、剪贴板文本、DOCX、XLSX、PPTX、PDF 和常见图片。
- 使用规则、敏感词典、校验算法、中文信息抽取、OCR/版面模型和 4B 本地多模态语言模型组合检测。
- 自动应用策略并生成可编辑的脱敏草稿，允许用户撤销、修改和补充。
- 允许按照策略跳过人工复核并批量导出，失败文件单独拦截。
- 脱敏方法、自动处理阈值和敏感信息范围均可配置。
- PDF 保真优先；无法确认安全删除时，对受影响页面使用安全栅格化兜底。
- 一致替换的作用域和可逆恢复均可配置，默认采用任务内一致且不可逆。
- 提供扫描、草稿复核、生成副本和二次校验的完整闭环。
- 用户反馈只保存在本机，MVP 阶段不进行模型权重训练。

详细范围与验收条件以产品需求文档为准。

## 当前可运行原型

需要 Rust 1.97 或更高版本。在项目目录执行：

```bash
cargo run -p llamask -- scan sample.txt --task task.json
cargo run -p llamask -- export task.json --output sample_已脱敏.txt \
  --runtimes config/runtimes/development-siamese.json
cargo run -p llamask -- verify task.json sample_已脱敏.txt \
  --runtimes config/runtimes/development-siamese.json
```

`task.json` 是可编辑的复核草稿。`selected` 控制是否脱敏，`replacement`
控制替换内容；模型低置信结果的 `reviewed` 默认为 `false`，用户确认处理或
保留后需要改为 `true`，否则导出会被拦截。原型不会覆盖原文件或已有输出
文件；扫描后原文件发生变化时，导出会被拒绝。

策略还支持精确敏感词和白名单：

```json
{
  "exact_terms": [
    { "value": "星海计划", "entity_type": "PROJECT_CODE" }
  ],
  "allowlist": ["public@example.com"]
}
```

这两个字段加入完整策略 JSON 使用。精确敏感词使用原文精确匹配；白名单
只取消与整个命中值完全相同的结果。

剪贴板兼容流程可以从标准输入建立任务，再把经过复扫的脱敏文本写到标准
输出：

```bash
cargo run -p llamask -- scan-stdin --task clipboard-task.json
cargo run -p llamask -- render clipboard-task.json
```

命令行不会后台监听或自动读取系统剪贴板；桌面界面后续只在用户主动操作时
调用同一套入口。

PNG/JPEG 图片流程使用本地 PP-OCRv6 small。普通规则模式约需 31 MB OCR
权重；带人名、机构 AI 识别的轻量组合再加载 SiameseUIE。图片任务中的
`mask_rect` 是可手动修改的最终像素矩形，`selected`/`reviewed` 控制处理与
复核状态：

```bash
cargo run -p llamask -- scan-image sample.png \
  --task image-task.json \
  --runtimes config/runtimes/development-image-small.json

cargo run -p llamask -- export-image image-task.json \
  --output sample_已脱敏.png \
  --runtimes config/runtimes/development-image-small.json

cargo run -p llamask -- verify-image image-task.json sample_已脱敏.png \
  --runtimes config/runtimes/development-image-small.json
```

核心会按 OCR 文本行估算命中片段矩形，并增加策略中的安全边距；跨行号码
共享 `group_id` 并生成多个矩形。旋转或无法可靠细分的文字回退为整行框。
导出使用实心色块，PNG 无损重编码并保留透明通道、JPEG 质量 95；手机照片
的 EXIF 方向先烧录到像素，再去除源图片元数据；
候选副本必须通过再次 OCR 和规则/可选模型复扫后才会原子落盘。输出格式
必须与输入一致，且不会覆盖原件或已有文件。

DOCX 流程扫描正文、表格、页眉页脚、脚注、尾注、批注、图表文字、
修订删除文字和字段代码。跨多个 Word run 的一个命中会合并检测，再仅修改
相交的文本节点；未命中的 XML 和媒体 entry 不从纯文本重建。导出同时清理
作者、批注/修订身份、文档变量、自定义属性、外部超链接目标、缩略图以及
ZIP 注释和时间戳：

```bash
cargo run -p llamask -- scan-docx sample.docx \
  --task docx-task.json

cargo run -p llamask -- export-docx docx-task.json \
  --output sample_已脱敏.docx

cargo run -p llamask -- verify-docx docx-task.json sample_已脱敏.docx
```

DOCX 中的 PNG/JPEG 嵌入图片已经递归接入现有图片管线。扫描时提供含 OCR
运行项的注册表，任务草稿会在 `embedded_images` 中保存逐图可编辑的遮罩
子任务；导出时逐图去元数据重编码、替换 `word/media`，并对候选 DOCX 中
的每张图片再次 OCR 复扫：

```bash
cargo run -p llamask -- scan-docx sample.docx \
  --task docx-task.json \
  --runtimes config/runtimes/development-ocr-small.json
cargo run -p llamask -- export-docx docx-task.json \
  --output sample_已脱敏.docx \
  --runtimes config/runtimes/development-ocr-small.json
```

未提供 OCR 运行配置时仍可取得文字扫描草稿，但含图片文档会在导出时安全
阻断并要求重新扫描。GIF、TIFF、SVG 等尚未支持的媒体格式，以及宏、
ActiveX、OLE/嵌入对象仍会阻断。验证报告不包含残留原值；
`complete: false` 表示策略启用的可选本地模型没有全部参与复扫。

XLSX 流程覆盖普通字符串、共享字符串、内联字符串、数字单元格、公式及
缓存值、传统批注、页眉页脚、隐藏行列/工作表、定义名称、DrawingML 文字
和 PNG/JPEG 嵌入图片。公式中的命中默认强制人工复核；确认处理后会删除
公式并把整个单元格替换为普通文本，避免敏感字面量藏在公式或缓存中：

```bash
cargo run -p llamask -- scan-xlsx sample.xlsx \
  --task xlsx-task.json \
  --runtimes config/runtimes/development-ocr-small.json

cargo run -p llamask -- export-xlsx xlsx-task.json \
  --output sample_已脱敏.xlsx \
  --runtimes config/runtimes/development-ocr-small.json

cargo run -p llamask -- verify-xlsx xlsx-task.json sample_已脱敏.xlsx \
  --runtimes config/runtimes/development-ocr-small.json
```

选中的共享字符串单元格会转成保留原样式的内联字符串，失去引用的共享
字符串原值会清空；批注作者和 Office/ZIP 隐私元数据也会清理。工作表名称
暂不自动改名，命中后必须明确复核为保留。图表、透视缓存、外部数据连接、
宏、ActiveX、嵌入对象和非 PNG/JPEG 媒体目前会安全阻断，避免生成看似
成功但仍可能含残留的文件。详见 XLSX 安全边界文档。

PPTX 流程覆盖页面文本框、表格、组合形状、跨 run 文字、演讲者备注、现代
批注和回复、隐藏页、幻灯片母版、版式、SmartArt 文字和 PNG/JPEG 嵌入
图片。替换直接发生在相交的 `a:t` 节点中，保留页面结构和 run 样式：

```bash
cargo run -p llamask -- scan-pptx sample.pptx \
  --task pptx-task.json \
  --runtimes config/runtimes/development-ocr-small.json

cargo run -p llamask -- export-pptx pptx-task.json \
  --output sample_已脱敏.pptx \
  --runtimes config/runtimes/development-ocr-small.json

cargo run -p llamask -- verify-pptx pptx-task.json sample_已脱敏.pptx \
  --runtimes config/runtimes/development-ocr-small.json
```

导出会中性化批注作者和时间、清理绘图描述与自定义数据，并把外部超链接
改为 `about:blank`。图表/缓存、嵌入工作簿、外部数据、宏、ActiveX、3D
模型、音视频和非 PNG/JPEG 媒体当前会安全阻断。详见 PPTX 安全边界文档。

PDF 首个安全基线使用 Poppler 在固定 200 DPI 下渲染全部页面，再复用图片
OCR 和可编辑 `mask_rect` 任务。导出不会复制源 PDF 的对象树，而是把打码
后的页面作为 JPEG 图像从零构造新 PDF，因此表单、批注、链接、附件、脚本、
隐藏文字、元数据和增量历史均不会进入副本：

```bash
cargo run -p llamask -- scan-pdf sample.pdf \
  --task pdf-task.json \
  --runtimes config/runtimes/development-ocr-small.json

cargo run -p llamask -- export-pdf pdf-task.json \
  --output sample_已脱敏.pdf \
  --runtimes config/runtimes/development-ocr-small.json

cargo run -p llamask -- verify-pdf pdf-task.json sample_已脱敏.pdf \
  --runtimes config/runtimes/development-ocr-small.json
```

该模式保持页面视觉外观和顺序，但不保留文字搜索/复制、矢量编辑、表单和
链接交互。输出必须再次通过逐页 OCR、目标值检查和严格的仅图像 PDF 结构
验证才会原子落盘。当前需要本机或安装包提供 `pdfinfo`、`pdftoppm`；可用
`LLAMASK_PDFINFO`、`LLAMASK_PDFTOPPM` 指向捆绑版本。加密 PDF、超过
100 MiB、超过 200 页或渲染后超过 5 亿像素的文件会安全拒绝。详见 PDF
安全边界文档。

生成和验证策略：

```bash
cargo run -p llamask -- policy init my-policy.json
cargo run -p llamask -- policy validate my-policy.json
cargo run -p llamask -- runtimes verify config/runtimes/development-siamese.json
```

扫描时可通过 `--policy` 选择策略，通过 `--runtimes` 选择本地模型运行
注册表。模型未配置或可选模型失败时，任务草稿会保存明确的降级提示；
策略将模型设为 `required` 时则直接拦截扫描。导出和验证再次传入同一运行
注册表时，会在替换后重新运行规则与模型；报告中的 `complete` 表示全部
已启用模型是否都参与了复扫，报告不会回显残留原文。

真实文本模型组合可以使用：

```bash
target/release/llamask scan sample.txt \
  --task task.json \
  --policy config/policies/default.json \
  --runtimes config/runtimes/development-text-models-cpu.json
```

当前任务草稿会包含原文、OCR 文本和命中值，应视为敏感文件妥善保管。真实
SiameseUIE 和 Qwen Q4 已有开发 sidecar。SiameseUIE 默认路径已改为纯
ONNX 运行，不再依赖 PyTorch、Transformers 或 ModelScope；Qwen 的单记录
路径相对冻结批量评测仍存在语义漂移，所以两者暂时都以未校准结果进入
复核。图片轻量组合当前只接入 OCR、规则和 SiameseUIE；Qwen 要等持久模型
进程完成后再进入图片默认链路。仓库内的 mock sidecar 只用于协议测试。
图形界面、PDF 对象保留/干净搜索层模式仍属于后续纵向切片。Office 与 PDF
适配器的下一个发布门槛是双平台 Microsoft Office/LibreOffice/Keynote 与
系统 PDF 阅读器真实文件回归，以及继续扩展 PNG/JPEG 之外的安全媒体支持。

桌面端阶段已经开始：`apps/llamask-desktop` 提供 Tauri 2 + React/TypeScript
最小窗口、受限文件选择权限、拖放导入、Rust 会话路径注册表、格式/大小预检
和任务列表界面。TXT、Markdown、DOCX、XLSX、PPTX、PDF、PNG 和 JPEG 已接入真实
`llamask-core` 后台扫描；敏感扫描草稿只保存在 Rust 内存。图片/PDF 复核只
按需取得受限尺寸的重编码页面预览和几何框；文本复核只取得命中内容及前后
各 80 个 Unicode 字符，不返回源路径或整份文档。DOCX 文字结果只额外提供
“正文、页眉、批注”等非敏感内容域标签，OOXML 定位仍留在 Rust 任务内；
内嵌 PNG/JPEG 会在复核源包哈希后进入同一个可编辑遮罩界面。剪贴板纯文本现已支持由
用户主动读取、使用同一有界文本界面扫描和复核，并且只有独立残留复扫通过后
才写回系统剪贴板；原文只保存在 Rust 会话内存，不进入进度事件或应用存储。
用户可以编辑文本替换值、接受或保留结果、移动/缩放遮罩、新增/删除手动遮罩，
并通过原生保存对话框调用安全导出。DOCX 的文字与内嵌图片必须全部复核后
才能导出，导出复用 Core 的最小 OOXML 修改和残留验证。XLSX 复核只展示
工作表序号、单元格坐标和内容类型；同一单元格的公式与缓存原子同步复核，
工作表名称当前只能明确保留。内嵌图片复用哈希校验后的遮罩流程，导出继续
使用 Core 的工作簿最小修改与残留复扫。PPTX 复核只展示幻灯片、备注、批注、
母版、版式和关系图等安全内容域及序号；内嵌图片复用源哈希校验后的遮罩流程。
图表、外部数据、嵌入对象、主动内容和不支持媒体在复核前安全阻断，导出复用
Core 的演示文稿最小修改与独立残留复扫。批量导出现在只需选择一次原生输出
目录，按顺序处理已自动确认或人工复核的文件，自动避让重名并隔离单文件失败；
待复核文件会跳过，完成摘要只包含聚合计数。离线运行包契约现在会对 OCR
可执行文件、模型、`pdfinfo`、`pdftoppm` 和依赖逐文件固定 SHA-256，并由
平台配方安全生成 Tauri 资源目录；已声明工具校验失败时不会回退。下一发布
门槛是真实 Apple Silicon/Windows x64 载荷、许可证清单、签名和真机回归。
开发和安全边界见桌面端架构文档。
