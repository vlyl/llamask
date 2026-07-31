# SiameseUIE ONNX sidecar

这是当前默认的 SiameseUIE 本地适配器。它不加载 PyTorch、Transformers
或 ModelScope，运行时只需要 ONNX Runtime、NumPy 和 Rust tokenizers。

## 生成模型

导出和量化仅在开发环境执行：

```bash
.venv-siamese/bin/python \
  evaluation/scripts/export_siamese_uie_onnx.py

.venv-siamese/bin/python \
  evaluation/scripts/quantize_siamese_uie_onnx.py
```

FP32 导出会把正文编码和多提示推理放进同一个动态 ONNX 图：正文只编码
一次，模型权重也只保存一份。导出脚本会用两个不同样例逐位置比较
PyTorch 和 ONNX logits，并要求最终实体完全一致。

## 最小运行环境

开发机上的最小环境按以下方式建立：

```bash
uv venv --python 3.12 .venv-siamese-onnx
uv pip install \
  --python .venv-siamese-onnx/bin/python \
  onnxruntime==1.28.0
uv pip install \
  --python .venv-siamese-onnx/bin/python \
  --no-deps tokenizers==0.22.2
```

`tokenizers` 声明的 Hugging Face Hub 依赖只用于在线仓库能力，本 sidecar
只从固定的本地 `vocab.txt` 创建 `BertWordPieceTokenizer`，因此发布运行
包不带 Hub 客户端。当前开发机实测 site-packages 环境约 104 MiB；携带的
Python 3.12 运行时约 72 MiB。

验证注册表和运行真实扫描：

```bash
target/release/llamask runtimes verify \
  config/runtimes/development-siamese.json

target/release/llamask scan fixtures/text/ai_sample.txt \
  --task task.json \
  --policy config/policies/default.json \
  --runtimes config/runtimes/development-siamese.json
```

## 当前取舍

- 默认：407,056,303 字节的 FP32 单文件 ONNX。旧 360 条冻结预测与
  PyTorch 逐条一致。
- 实验：322,761,115 字节的动态 INT8 ONNX。它在全部 1,440 条测试记录
  中有 26 条实体决策改变，只节省约 20.7% 权重体积，因此不作为首发
  默认。
- 两个版本的 confidence 都不是校准概率，仍以人工复核建议进入任务草稿。
