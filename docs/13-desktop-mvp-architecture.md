# 桌面端 MVP 架构与复核导出里程碑

文档状态：V0.3，PDF/图片复核与安全导出实现基线（2026-08-02）

## 1. 阶段目标

CLI 已证明文本、图片、DOCX、XLSX、PPTX 和 PDF 的扫描、编辑任务、安全
导出与独立复检闭环。桌面端阶段不重写这些处理器，而是让普通用户通过一个
完全离线的界面使用同一套 Rust Core。

首个桌面里程碑负责建立可信导入边界：

1. 运行 Tauri 2 桌面窗口和 React/TypeScript 静态前端。
2. 通过系统文件选择器或窗口拖放导入文件。
3. 在 Rust 端规范化路径、检查文件类型和大小上限。
4. 用会话 id 代替绝对路径返回前端。
5. 展示任务列表、支持状态、大小和后续处理步骤。
6. 验证前端构建、Rust 编译和桌面窗口启动。

该里程碑不伪造扫描进度或结果。第二个里程碑现已让“开始扫描”调用真实
`llamask-core` PDF/图片扫描，并继续沿用同一条可信路径边界。

## 2. 技术选择

| 层 | 选择 | 理由 |
| --- | --- | --- |
| 桌面容器 | Tauri 2 | 复用 Rust Core、安装体积可控、具有显式 capability 和 CSP |
| 前端 | React 19 + TypeScript | 适合任务列表、复核状态和复杂页面预览交互 |
| 构建 | Vite | 输出静态 SPA，开发反馈快，不引入服务端渲染 |
| 包管理 | pnpm | 锁文件可复现，工作目录依赖隔离清晰 |
| 后端调用 | Tauri command IPC | 不监听端口，不通过 shell 拼装 CLI 参数 |
| 业务核心 | `llamask-core` | CLI 和桌面端共享同一检测、安全导出与验证实现 |

桌面 crate 是 Cargo workspace 成员，直接以路径依赖链接 `llamask-core`。

## 3. 信任边界

```text
系统文件选择器 / 窗口拖放
              │ 用户明确选择的路径
              ▼
        Tauri Rust 命令层
        ├─ canonicalize
        ├─ file metadata
        ├─ format / size gate
        └─ session path registry
              │ id + 文件名 + 类型 + 大小 + 状态码
              ▼
          React WebView
```

关键约束：

- 系统文件选择器由 Rust 命令打开；原生窗口事件直接接收拖放结果。WebView
  不向命令提交路径，也不获得规范化绝对路径。
- WebView 只有 `core:default` 权限；不授予对话框、文件系统、shell、opener
  或远程 URL 权限。
- 自定义 Rust command 默认只返回稳定错误码和不含路径的中文说明。
- 前端不使用 localStorage、IndexedDB 或 URL 参数保存文件和任务。
- 任务注册表位于 Rust 进程内，退出即清除。后续持久任务必须进入专用权限
  临时目录并使用受控文件权限。
- WebView 只加载打包静态资源。CSP 只允许本地资源、Tauri IPC 和开发期的
  loopback Vite 地址；不加载 CDN 字体、脚本、遥测或远程图片。

## 4. 当前 IPC

### `desktop_capabilities`

返回应用/Core 版本、默认策略 id、支持扩展名、单次文件上限和离线状态。

### `pick_files()`

Rust 打开系统多文件选择器，用户确认后直接接收路径。前端不能把任意路径作为
参数提交。Rust 对每个路径：

- 规范化并确认是普通文件。
- 只按文件名、扩展名、大小建立导入摘要。
- TXT/Markdown 上限 2 MiB；图片 50 MiB；Office/PDF 100 MiB。
- 识别同一会话内的重复路径。
- 将绝对路径保留在 Rust 注册表，前端只收到会话 id。

单次请求最多 200 个文件。不可读、超限和不支持格式作为单文件状态返回，
不会把操作系统错误或路径写进界面日志。

窗口拖放由 Rust 原生 `WindowEvent` 进入同一注册函数，再通过
`desktop-files-imported` 事件向前端发送相同摘要；事件不含路径。

### `remove_registered_file(id)` / `clear_registered_files()`

同步清除 Rust 注册表和界面任务。不存在的 id 不暴露注册表内容。

### `desktop_runtime_status()`

在 Rust 端验证默认策略、本地运行注册表、OCR 可执行文件和权重 SHA-256，
并检查 `pdfinfo`/`pdftoppm`。开发构建可使用仓库内 OCR 配置；安装构建只从
应用资源目录读取 `runtimes/default.json`，也可由明确的
`LLAMASK_RUNTIME_REGISTRY` 覆盖。前端只取得就绪状态和稳定状态码，不取得
模型或工具路径。

### `start_registered_scans()` / `cancel_scan(id)`

一次启动当前会话内全部可扫描 PDF、PNG 和 JPEG。Rust 后台工作线程按顺序
处理文件，避免并发启动大量 OCR 进程；扫描任务草稿始终保存在 Rust 进程
内。取消采用合作式语义：PDF 在页边界终止并由临时目录自动清理，图片在
当前 OCR 调用返回后丢弃结果，不会生成伪成功任务。

### `scan_task_summaries()` / `desktop-scan-progress`

返回或推送任务 id、状态、阶段、页数、进度、逻辑命中数、待复核数、诊断
数量和稳定错误码。普通事件不包含绝对路径、OCR 原文、命中原值或任务草稿。

### `review_scan_page(id, page_number)`

仅在用户明确打开复核页时调用。Rust 重新验证源文件哈希，并返回当前页最长
边不超过 2400 像素的重编码 PNG、原始坐标系尺寸和遮罩几何信息。响应不含
绝对路径、OCR 行文本或命中原值；前端不持久化预览。

### 复核修改 commands

`set_review_group`、`update_review_mask`、`add_review_mask` 和
`remove_review_mask` 只接收会话 id、页码、任务内结果 id 和数值坐标。Rust
Core 校验矩形边界、结果归属和状态转换。自动检测结果不能被物理删除，只能
明确标记为“应用遮罩”或“保留原文”；只有不含 OCR 原文的手动几何遮罩可以
删除。

### `choose_and_export_scan(id)`

Rust 打开原生保存对话框，WebView 不提交或取得输出路径。导出在受控后台任务
中调用既有 `llamask-core` 图片/PDF 导出器，拒绝覆盖源文件和已有输出。候选
副本必须通过独立 OCR、目标值、策略和 PDF 安全结构复扫后才原子落盘；普通
状态事件只返回输出文件名、完成度和稳定错误码。

## 5. 后续任务状态机

```text
queued
  ├─ scanning ── review_required ── ready_to_export
  │                  └────────────── ready_to_export
  ├─ blocked
  └─ cancelled

ready_to_export ── exporting ── verifying ── complete
                         └─────────────── failed
```

状态只能由 Rust 编排器推进；WebView 发送用户意图，不能自行把文件标记为
“安全”或“完成”。`passed` 与 `complete` 保持当前 Core 语义，界面分别展示，
不能把可选模型未运行隐藏成完整验证。

## 6. 已实现增量：扫描编排器

当前实现按以下顺序接入：

1. 资源预检：策略、OCR、模型、`pdfinfo` 和 `pdftoppm` 完整性。
2. 以受控后台任务调用 `llamask-core`，不阻塞主窗口。
3. 首先支持 PDF 和 PNG/JPEG，生成统一的页/图片复核摘要。
4. 用事件发送阶段、页码和计数进度；事件不含 OCR 原文。
5. 支持取消；取消后清理临时候选，不留下伪成功文件。
6. 为复核 command 保留独立的按需数据边界，由下一节的实现继续收紧。

当前取消不会强制杀死正在响应的单次 OCR 子进程，而是在安全检查点结束并
丢弃结果；后续持久 sidecar 会加入请求级中断。扫描编排器稳定后，再依次把
TXT、DOCX、XLSX、PPTX 接到相同任务状态机。

## 7. 已实现增量：复核与导出界面

- PDF/图片支持逐页导航、检测框和实际遮罩框叠加。
- 支持接受或保留自动结果、移动/缩放矩形、精确坐标修改，以及新增/删除手动遮罩。
- 自动确认完成的文件可以从任务列表直接导出；待复核文件在全部确认后导出。
- 输出位置始终由用户通过原生对话框选择，不覆盖源文件或已有文件。
- 导出状态只包含文件名、数量、完成度和错误码，不包含敏感原文。
- 文本/Office 复核将以内容域、类型和上下文分组，不渲染整个可执行文档。

## 8. 验收标准

当前里程碑：

- macOS 上 Tauri 窗口可以启动。
- 文件选择和拖放都能建立任务列表。
- 支持格式、大小超限、不可读文件和重复文件有明确状态。
- 前端响应中不包含绝对路径。
- 未授权的文件系统、shell 和远程内容能力没有启用。
- `pnpm check`、`pnpm build`、Rust 测试和 Clippy 通过。

当前扫描里程碑：

- PDF/图片真实扫描可以后台运行、报告进度并取消。
- 任务从 `queued` 到 `review_required`/`ready_to_export` 由 Rust 状态机推进。
- 敏感任务数据不进入普通日志或浏览器持久存储。

当前复核与导出里程碑：

- PDF/图片复核页只按需读取单页预览和细粒度结果。
- 用户可以增删改遮罩，并把修改后的安全坐标写回 Rust 任务。
- 自动导出与复核后导出共用同一个残留验证门禁。

下一里程碑：

- 把 TXT、DOCX、XLSX、PPTX 和用户主动读取的剪贴板接到同一桌面状态机。
- 增加批量输出目录、逐文件失败隔离和不含敏感原文的完成摘要。
- 捆绑并验证双平台 OCR/PDF 工具与模型运行包。
