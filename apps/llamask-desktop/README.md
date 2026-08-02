# LlaMask Desktop

这是 Tauri 2 + React/TypeScript 桌面端。当前里程碑已完成最小桌面壳、受限
文件选择、拖放导入、Rust 文件注册表和格式/大小预检，并接入 PDF、PNG、
JPEG 的真实后台扫描、进度状态机与合作式取消。桌面端直接调用
`llamask-core`，不会通过 shell 启动 CLI。

## 开发

```bash
pnpm install --frozen-lockfile
pnpm check
pnpm build
pnpm tauri dev
```

只预览前端布局时可运行 `pnpm dev`。浏览器预览不会读取本地文件路径，文件
选择和拖放导入必须在 Tauri 窗口中测试。

Rust 端测试：

```bash
cargo test -p llamask-desktop
cargo check -p llamask-desktop
```

## 当前安全边界

- WebView 只拥有 `core:default` 权限；系统文件对话框由 Rust 命令打开。
- 不启用文件系统、shell、opener 或网络插件。
- Rust 直接接收系统文件选择和原生拖放结果，规范化路径并保存在进程内注册
  表；前端只取得会话 id、文件名、类型、大小和状态码，不接受可伪造的路径
  参数，也不取得规范化绝对路径。
- 一次最多导入 200 个文件，并按现有 Core 上限预检大小。
- 前端不在 localStorage、日志或 URL 中保存任务和路径。
- 发布构建只加载打包静态资源；CSP 不允许外部站点、脚本或字体。
- 扫描草稿和 OCR 原文只保存在 Rust 进程；普通事件只发送状态、页数和计数。
- OCR 运行文件和权重必须通过注册表 SHA-256 校验；PDF 扫描还要求本地
  `pdfinfo` 和 `pdftoppm` 就绪。

该注册表和扫描草稿目前都是会话内状态，应用退出后清空。下一增量会建立
按需单页复核接口、可编辑遮罩和安全导出；任务安全落盘与恢复仍未启用。
