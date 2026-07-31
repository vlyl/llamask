# Qwen review sidecar

这是 Qwen3.5-4B Q4_K_M 的开发期本地模型适配器，使用固定版本
`llama.cpp` 和 LlaMask sidecar protocol v1。

隐私边界：

- 敏感原文通过标准输入传给 `llama-completion`，不会出现在进程参数中。
- 不启动 `llama-server`，不监听本地端口。
- 使用固定 JSON Schema 约束输出。
- 模型返回的值必须能逐字对齐原文；改写、补全或编造的值会被丢弃。
- Rust 核心还会独立复核 Unicode 位置和原文。

CPU 开发配置：

```bash
target/release/llamask runtimes verify \
  config/runtimes/development-qwen-cpu.json

target/release/llamask scan fixtures/text/qwen_sample.txt \
  --task task.json \
  --policy config/policies/default.json \
  --runtimes config/runtimes/development-qwen-cpu.json
```

当前运行置信度固定为未校准的 `0.80`，低于默认自动处理阈值 `0.90`，
因此结果只供人工确认。

重要限制：冻结评测使用 `llama-server` 按 6 条批量输入，当前 sidecar
一次处理一个文本片段。冻结测试集前 6 条对照中，sidecar 为
Precision 75%、Recall 60%，原批量路径为 Precision 100%、Recall 70%。
两者存在语义漂移，因此当前 sidecar 是实验实现，不能继承原批量评测的
发布结论。

后续需要持久化模型工作进程，并在一次任务中对多个局部片段进行稳定批处理，
随后重跑全部 360 条冻结子集。未达到组合发布门槛前，Qwen 不得默认自动
处理。
