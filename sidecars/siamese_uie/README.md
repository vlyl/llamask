# SiameseUIE sidecar

这是 SiameseUIE Chinese base 的旧 PyTorch/ModelScope 开发适配器，已经
实现 LlaMask sidecar protocol v1。当前默认运行路径见
`sidecars/siamese_uie_onnx/`。

运行前提：

- `.venv-siamese` 中固定版本的 Python 3.12、PyTorch、Transformers 和
  ModelScope。
- 本地模型目录 `evaluation/models/cache/siamese-uie-chinese-base`。
- 不从网络下载任何模型或代码。

验证安装：

```bash
cargo run -p llamask -- runtimes verify \
  config/runtimes/development-siamese-pytorch.json
```

真实文本扫描：

```bash
cargo run -p llamask -- scan fixtures/text/ai_sample.txt \
  --task task.json \
  --policy config/policies/default.json \
  --runtimes config/runtimes/development-siamese-pytorch.json
```

当前模型只返回人名、机构和客户名称。原模型接口没有提供校准后的概率，
适配器暂时写入保守的运行置信度 `0.85`，默认策略阈值为 `0.90`，因此命中
默认进入人工确认。用户明确降低策略阈值后才会自动选择。

这不是默认或发布运行包。当前环境约 1.3GB，且 ModelScope 加载路径允许
受信任的模型代码；它只保留用于重新导出和回归对照。默认开发配置已经使用
固定 FP32 ONNX 图，发布前仍需在 Windows x64 与 Apple Silicon 上验证。
