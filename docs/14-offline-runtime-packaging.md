# 桌面离线运行包契约

文档状态：V0.1，哈希固定的 OCR/PDF 工具资源打包基线（2026-08-03）

## 1. 本阶段范围

本阶段建立安装包资源契约和可重复的准备流程，但不把未经签名、未经许可证
复核的平台二进制直接提交到 Git 仓库。完成的能力包括：

- 运行注册表可声明 OCR 检测器以及 `pdfinfo`、`pdftoppm` 两个工具。
- 可执行文件、模型、Python 依赖和 PDF 动态库都使用 SHA-256 固定。
- 桌面版优先从 Tauri 资源目录读取 `runtimes/default.json`。
- PDF 扫描、预览、导出和残留复扫都使用同一个已验证工具路径。
- 注册表已经声明某个 PDF 工具时，完整性失败不会回退到系统 PATH。
- 打包器拒绝绝对路径、`..`、符号链接和已有输出目录。

正式下载版仍需生成并验证 Apple Silicon 与 Windows x64 的 OCR/PDF 载荷，
完成许可证清单、代码签名、macOS 公证和真实机器回归后才能发布。

## 2. 资源布局

平台构建输入使用以下逻辑布局；具体 Python 和 Poppler 文件由平台构建任务
填充：

```text
payload/
├── ocr/
│   ├── python/              # 隔离的便携 Python 与依赖
│   ├── sidecar/main.py
│   └── models/
│       ├── det/
│       └── rec/
└── pdf/
    ├── bin/                 # pdfinfo、pdftoppm
    └── lib/                 # macOS 动态库；Windows 可将 DLL 放入 bin
```

示例配方位于：

- `config/runtime-packaging/macos-aarch64.example.json`
- `config/runtime-packaging/windows-x86_64.example.json`

配方只允许相对于载荷根目录的可移植 `/` 路径。`assets` 可以指向文件或
目录；目录会递归展开为逐文件哈希。检测器可执行文件会自动进入资源哈希
列表，PDF 工具可执行文件使用工具自身的 `sha256` 字段固定。

## 3. 生成安装资源

在可信、已经断网的发布构建机上执行：

```bash
python3 scripts/prepare-desktop-runtime.py \
  --recipe config/runtime-packaging/macos-aarch64.example.json \
  --payload /trusted/build/llamask-runtime-macos-aarch64 \
  --output apps/llamask-desktop/src-tauri/runtime-payload
```

Windows x64 使用对应配方和 `python` 命令。输出目录包含复制后的必要文件与
自动生成的 `default.json`。工具不会覆盖已有输出；重建前必须由发布人员明确
移除旧的 `runtime-payload`，避免混合两次构建的文件。

生成后运行：

```bash
cd apps/llamask-desktop
pnpm bundle:offline
```

`tauri.bundle.conf.json` 将整个 `runtime-payload/` 映射到安装后的
`$RESOURCE/runtimes/`。这使用 Tauri 2 官方的
[additional resources](https://v2.tauri.app/develop/resources/) 机制；应用现有
的资源发现逻辑会读取 `$RESOURCE/runtimes/default.json`。WebView 没有资源
文件读取权限，路径解析和进程启动仍只发生在 Rust 端。

## 4. 运行时选择规则

桌面发布构建按以下顺序处理：

1. 如果设置 `LLAMASK_RUNTIME_REGISTRY`，读取明确指定的开发/诊断注册表。
2. 否则读取安装资源中的 `runtimes/default.json`。
3. Debug 构建可回退到仓库开发注册表；Release 构建不使用该路径。

OCR 检测器必须全部通过可执行文件、工作目录和资源哈希校验。PDF 工具允许
独立降级：OCR 正常而 PDF 工具不完整时，图片与 Office 仍可工作，PDF 会以
`PDF_TOOLS_MISSING` 阻断。两个 PDF 工具只要有一个已经在注册表中声明，就
必须成对存在并通过哈希与 `-v` 启动检查。

Core 仍保留 CLI 的兼容行为：未在注册表声明 PDF 工具时，可使用成对的
`LLAMASK_PDFINFO`/`LLAMASK_PDFTOPPM` 覆盖或系统 PATH。桌面 Release 自检
不会把普通系统 PATH 当作可发布的离线运行环境。

## 5. 发布门禁

平台载荷进入 Release 前必须满足：

- PP-OCRv6 small ONNX 模型、便携运行时和所有传递依赖来自固定版本。
- `pdfinfo`、`pdftoppm` 与 Poppler 依赖来自固定版本，并完成许可证复核。
- 生成后的 `default.json` 不含构建机绝对路径。
- `llamask runtimes verify` 对安装资源通过。
- macOS 嵌套可执行文件与动态库完成签名，应用完成公证。
- Windows 安装包在无 Python、无 Poppler、无网络环境中完成扫描和导出。
- 两个平台各执行真实 PDF、图片及含内嵌图片 Office 文件的残留复扫回归。
- Windows 是否需要随包提供固定 WebView2 Runtime 单独冻结；Tauri 官方说明
  固定运行时会显著增加安装包体积，见
  [Windows installer documentation](https://v2.tauri.app/distribute/windows-installer/)。

## 6. 下一增量

下一增量负责实际平台载荷：冻结 Paddle/ONNX Runtime、Poppler 和便携 Python
版本，生成许可证与来源清单，在 macOS/Windows CI 中准备资源并产出未签名
测试安装包。代码签名密钥和公证凭据不进入仓库。
