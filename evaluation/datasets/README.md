# 评估集规范

## 记录格式

每行一个 JSON 对象：

```json
{
  "id": "syn-zh-000001",
  "split": "test",
  "language": "zh-CN",
  "text": "联系人：沈晓宁，手机13800000001。",
  "entities": [
    {
      "start": 4,
      "end": 7,
      "label": "PERSON_NAME",
      "text": "沈晓宁"
    }
  ],
  "tags": ["synthetic", "plain_text", "personal"],
  "difficulty": "normal",
  "policy": "default-high-recall"
}
```

`start` 和 `end` 使用 Python/Unicode code point 偏移，`end` 为开区间。进入产品实现时必须显式转换为各文件格式需要的 UTF-16 或 XML 节点偏移。

## 首批标签

| 标签 | 含义 |
| --- | --- |
| `PERSON_NAME` | 自然人姓名 |
| `ORG_NAME` | 组织或机构名称 |
| `ADDRESS` | 地址 |
| `PHONE_NUMBER` | 电话号码 |
| `EMAIL` | 邮箱 |
| `CN_ID_NUMBER` | 中国居民身份证格式号码 |
| `BANK_CARD_NUMBER` | 银行卡格式号码 |
| `FINANCIAL_AMOUNT` | 内部财务金额 |
| `SALARY_AMOUNT` | 工资或薪酬 |
| `BUSINESS_METRIC` | 利润率、报价、产量等内部业务指标 |
| `PROJECT_CODE` | 保密项目代号 |
| `CONTRACT_ID` | 合同或协议编号 |
| `CUSTOMER_NAME` | 在客户名单或保密客户上下文中的名称 |

## 难负例

评估集必须包含外形相似但不应自动脱敏的内容：

- 常见词中包含姓氏或短人名。
- 已公开的价格、客服电话和组织邮箱。
- 无效校验位的证件号和银行卡号。
- 日期、版本号、页码、数量等普通数字。
- “预算”“客户”“手机号”等只有字段名但没有实际值的句子。

模型不能只靠格式或关键词把整句全部判为敏感。
