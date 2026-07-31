# LlaMask 策略配置模型

文档状态：V0.1

## 1. 目标

策略把“检查什么、何时自动处理、怎样脱敏、是否可恢复”从代码中分离。个人用户可以通过界面配置，企业用户可以导入离线策略文件；两者使用同一个策略模型。

用户不需要直接编辑 JSON 或 YAML。文本格式只用于开发、测试和策略导入导出。

## 2. 配置层级

从低到高：

1. 产品安全默认值。
2. 内置策略模板。
3. 用户策略。
4. 项目策略。
5. 当前任务设置。
6. 单个命中结果的手动修改。

高层配置只覆盖明确设置的字段，未设置字段继续继承低层值。合并后的有效策略带有不可变版本号，供任务复现。

## 3. 策略结构

概念示例：

```yaml
id: personal-and-business
version: 3
name: 个人与业务信息

inputs:
  file_types: [txt, md, docx, xlsx, pptx, pdf, png, jpeg, clipboard]
  include_hidden_content: true
  include_embedded_images: true
  include_metadata: true

detectors:
  PERSON_NAME:
    enabled: true
    auto_apply_threshold: 0.90
  PHONE_NUMBER:
    enabled: true
    auto_apply_threshold: 0.85
  ORGANIZATION:
    enabled: true
    auto_apply_threshold: 0.92
  FINANCIAL_AMOUNT:
    enabled: true
    require_context: true
    auto_apply_threshold: 0.95

actions:
  PERSON_NAME:
    type: stable_placeholder
    template: "[姓名_{index}]"
  PHONE_NUMBER:
    type: partial_mask
    preserve_start: 3
    preserve_end: 4
  default:
    type: typed_placeholder

automation:
  apply_during_scan: true
  review_mode: optional
  allow_direct_batch_export: true
  block_on_verification_failure: true

pdf:
  mode: preserve_with_safe_fallback
  rebuild_clean_text_layer: true

image_mask:
  safety_margin_px: 4
  solid_rgb: [0, 0, 0]
  auto_apply_min_ocr_confidence: 0.90

replacement_mapping:
  consistency_scope: task
  reversible: false
```

## 4. 敏感信息分类

每个分类包含：

- 稳定的内部类型 ID。
- 用户可见名称和说明。
- 启用状态。
- 使用的检测器。
- 检测阈值。
- 自动处理阈值。
- 默认脱敏动作。
- 是否要求上下文。
- 风险等级。

初始顶级分类：

- `PERSONAL`：个人信息。
- `ORGANIZATION`：组织信息。
- `FINANCIAL`：财务信息。
- `BUSINESS`：业务信息。
- `SECRET`：账号、密钥、访问令牌等秘密数据。
- `CUSTOM`：用户自定义信息。

财务和业务类型默认要求上下文，不能仅因内容是数字或金额就自动判定敏感。

## 5. 检测与自动处理阈值

“检测阈值”和“自动处理阈值”分开：

- 达到检测阈值：进入结果列表。
- 达到自动处理阈值：在草稿中自动应用脱敏。
- 低于检测阈值：通常不展示，但可在高召回模式中显示。

确定性校验器可以直接给出高置信度。LLM 的置信度不能与规则置信度简单等价，需要经过评估集校准。

当前文本原型额外实现两个受限字段：

- `exact_terms`：精确敏感词及其实体类型，不接受任意代码或任意正则。
- `allowlist`：按完整命中值取消结果，不做模糊扩展。

二者均限制单值长度、拒绝空值和重复值。任务保存合并后的策略快照，保证
导出后复扫使用与首次扫描相同的词典和白名单。

当前图片原型实现 `image_mask`：

- `safety_margin_px`：OCR 估算框四周增加的像素，范围 0～128。
- `solid_rgb`：不可逆实心遮罩的 RGB 颜色。
- `auto_apply_min_ocr_confidence`：OCR 低于该值时，即使文本检测置信度足够，
  也保持待复核。

这些字段同样进入任务策略快照。当前代码只执行安全默认的 `solid_box`；
像素化和模糊仍保留在产品模型中，但在完成可恢复风险评测前不作为自动
导出动作。

## 6. 复核模式

策略支持：

- `required`：必须打开复核界面后才能导出。
- `optional`：先显示完成汇总，用户可以复核或直接导出。
- `skip`：自动导出，不打开复核界面。

无论选择哪种模式：

- 不覆盖源文件。
- 每个结果都保留检测依据。
- 每个输出文件都执行残留验证。
- 验证失败的文件被拦截。

每个任务命中同时保存 `selected` 和 `reviewed`：

- `selected=true` 表示导出时执行脱敏。
- `reviewed=false` 表示尚未达到自动处理门槛或尚未由用户确认。
- 用户明确保留内容时使用 `selected=false, reviewed=true`。
- 任何 `reviewed=false` 的结果都会阻止导出，不能被批量模式静默跳过。

## 7. 脱敏动作

文本动作：

- `typed_placeholder`
- `stable_placeholder`
- `full_mask`
- `partial_mask`
- `delete`
- `synthetic_alias`
- `encrypted_replacement`

图片动作：

- `solid_box`
- `pixelate`
- `blur`

高敏感图片默认使用 `solid_box`。策略界面应对模糊和像素化显示“可能存在恢复或识别风险”的提示。

## 8. 一致替换

`consistency_scope` 支持：

- `document`
- `task`
- `project`
- `global`

默认是 `task`。映射按“标准化实体值 + 实体类型 + 映射命名空间”生成，避免不同实体类型意外共享代号。

项目级和全局级映射使不同文件之间更容易被关联。产品在启用时必须显示这一影响，并允许用户为不同接收方建立独立命名空间。

## 9. 可逆恢复

`reversible` 默认是 `false`。

开启后：

- 原值与替换值保存在独立加密映射库。
- 策略只保存密钥引用，不保存恢复密钥。
- 用户可以选择恢复整个文件或指定实体。
- 恢复生成新副本，不修改脱敏文件。
- 恢复完成后同样执行结构验证。

可逆模式意味着本机仍保存一份高价值敏感数据。启用前必须显示风险，密钥管理方案在实现前需要独立威胁建模。

## 10. PDF 策略

`pdf.mode` 支持：

- `preserve_with_safe_fallback`：默认。尽量保真，无法安全删除的页面栅格化。
- `force_rasterize`：所有页面安全栅格化。
- `preserve_only`：只允许对象级安全删除；任何页面无法处理时整个文件失败，不静默降级。

导出摘要列出：

- 保持对象结构的页面。
- 被栅格化的页面。
- 是否重建搜索文字层。
- 被删除的附件、批注、表单和元数据类型。

## 11. 策略导入导出

- 启用 `exact_terms` 的本地策略会包含敏感词明文，必须按敏感文件保护；
  对外导出策略时默认排除这些值，只有用户明确选择后才加密导出。
- 可逆映射不与普通策略文件混存。
- 导入前显示来源、版本、将被修改的配置和模型兼容性。
- 企业可以通过离线文件分发策略，但首发不建设中央下发系统。
- 不受信任的策略不能执行任意代码、任意正则或外部命令。
- 正则执行必须有复杂度限制和超时。

## 12. 待原型验证

- 阈值在不同检测器之间如何校准。
- 自然语言规则如何安全转换为受限策略表达式。
- 加密映射库的密钥派生、轮换和备份方式。
- PDF 对象级删除可以可靠覆盖的文件比例。
- 全局一致映射在实际使用中的隐私风险提示是否足够清楚。
