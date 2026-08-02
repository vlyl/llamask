# 桌面端 MVP 架构与首个里程碑

文档状态：V0.1，桌面壳与文件导入边界实现基线（2026-08-01）

## 1. 阶段目标

CLI 已证明文本、图片、DOCX、XLSX、PPTX 和 PDF 的扫描、编辑任务、安全
导出与独立复检闭环。桌面端阶段不重写这些处理器，而是让普通用户通过一个
完全离线的界面使用同一套 Rust Core。

首个桌面里程碑只负责建立可信边界：

1. 运行 Tauri 2 桌面窗口和 React/TypeScript 静态前端。
2. 通过系统文件选择器或窗口拖放导入文件。
3. 在 Rust 端规范化路径、检查文件类型和大小上限。
4. 用会话 id 代替绝对路径返回前端。
5. 展示任务列表、支持状态、大小和后续处理步骤。
6. 验证前端构建、Rust 编译和桌面窗口启动。

本里程碑不伪造扫描进度或结果；“开始扫描”在编排器接入前保持禁用。

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

## 6. 下一增量：扫描编排器

下一提交按以下顺序接入：

1. 资源预检：策略、OCR、模型、`pdfinfo` 和 `pdftoppm` 完整性。
2. 以受控后台任务调用 `llamask-core`，不阻塞主窗口。
3. 首先支持 PDF 和 PNG/JPEG，生成统一的页/图片复核摘要。
4. 用事件发送阶段、页码和计数进度；事件不含 OCR 原文。
5. 支持取消；取消后清理临时候选，不留下伪成功文件。
6. 建立只在打开复核页时传输敏感原文的细粒度 command。

扫描编排器稳定后，再依次把 TXT、DOCX、XLSX、PPTX 接到相同任务状态机。

## 7. 后续复核与导出界面

- PDF/图片缩放、逐页导航、检测框与实际遮罩框叠加。
- 增加、移动、缩放、删除遮罩；修改后重新验证坐标。
- 文本/Office 以内容域、类型和上下文分组，不渲染整个可执行文档。
- 支持“一键自动导出”和“打开复核后导出”两条路径。
- 输出位置始终由用户明确选择，不覆盖源文件或已有文件。
- 导出摘要只包含文件名、类型、数量、状态和错误码，不包含敏感原文。

## 8. 验收标准

当前里程碑：

- macOS 上 Tauri 窗口可以启动。
- 文件选择和拖放都能建立任务列表。
- 支持格式、大小超限、不可读文件和重复文件有明确状态。
- 前端响应中不包含绝对路径。
- 未授权的文件系统、shell 和远程内容能力没有启用。
- `pnpm check`、`pnpm build`、Rust 测试和 Clippy 通过。

下一里程碑：

- PDF/图片真实扫描可以后台运行、报告进度并取消。
- 任务从 `queued` 到 `review_required`/`ready_to_export` 由 Rust 状态机推进。
- 敏感任务数据不进入普通日志或浏览器持久存储。
