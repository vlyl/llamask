# LlaMask Desktop

这是 Tauri 2 + React/TypeScript 桌面端。当前里程碑完成最小桌面壳、受限文件
选择权限、拖放导入、Rust 文件注册表和格式/大小预检；扫描、复核和导出会在
后续增量中直接调用 `llamask-core`，不会通过 shell 启动 CLI。

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

该注册表目前是会话内状态，应用退出后清空。任务安全落盘、恢复和加密临时
目录会在扫描编排器接入时统一实现。
