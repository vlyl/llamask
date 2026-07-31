use std::ops::Range;
use std::sync::OnceLock;

use regex::Regex;

use crate::model::{EntityType, FileKind, Finding};
use crate::policy::ExactTermPolicy;

fn regex(pattern: &'static str, slot: &'static OnceLock<Regex>) -> &'static Regex {
    slot.get_or_init(|| Regex::new(pattern).expect("built-in regular expression is valid"))
}

fn byte_to_char(text: &str, byte_index: usize) -> usize {
    text[..byte_index].chars().count()
}

fn context(text: &str, start: usize, end: usize) -> &str {
    let left = text[..start]
        .char_indices()
        .rev()
        .nth(24)
        .map(|(index, _)| index)
        .unwrap_or(0);
    let right = text[end..]
        .char_indices()
        .nth(24)
        .map(|(index, _)| end + index)
        .unwrap_or(text.len());
    &text[left..right]
}

fn explicitly_public(text: &str, value_start: usize) -> bool {
    let left = text[..value_start]
        .char_indices()
        .rev()
        .nth(12)
        .map(|(index, _)| index)
        .unwrap_or(0);
    let value_context = &text[left..value_start];
    ["公开", "官网", "宣传", "公共邮箱", "客服电话", "售后服务"]
        .iter()
        .any(|cue| value_context.contains(cue))
}

fn valid_cn_id(value: &str) -> bool {
    let normalized: String = value
        .chars()
        .filter(|char| char.is_ascii_digit() || matches!(char, 'X' | 'x'))
        .collect();
    if normalized.len() != 18 {
        return false;
    }
    let weights = [7, 9, 10, 5, 8, 4, 2, 1, 6, 3, 7, 9, 10, 5, 8, 4, 2];
    let checks = b"10X98765432";
    let mut sum = 0usize;
    for (byte, weight) in normalized.as_bytes()[..17].iter().zip(weights) {
        if !byte.is_ascii_digit() {
            return false;
        }
        sum += (byte - b'0') as usize * weight;
    }
    normalized.as_bytes()[17].to_ascii_uppercase() == checks[sum % 11]
}

fn valid_luhn(value: &str) -> bool {
    let digits: Vec<u32> = value
        .bytes()
        .filter(|byte| byte.is_ascii_digit())
        .map(|byte| (byte - b'0') as u32)
        .collect();
    if !(12..=19).contains(&digits.len()) {
        return false;
    }
    let parity = digits.len() % 2;
    let total: u32 = digits
        .iter()
        .enumerate()
        .map(|(index, digit)| {
            let mut value = *digit;
            if index % 2 == parity {
                value *= 2;
                if value > 9 {
                    value -= 9;
                }
            }
            value
        })
        .sum();
    total.is_multiple_of(10)
}

fn fenced_code_ranges(text: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = None;
    let mut cursor = 0usize;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            if let Some(open) = start.take() {
                ranges.push(open..cursor + line.chars().count());
            } else {
                start = Some(cursor);
            }
        }
        cursor += line.chars().count();
    }
    if let Some(open) = start {
        ranges.push(open..text.chars().count());
    }
    ranges
}

fn add_finding(
    findings: &mut Vec<Finding>,
    text: &str,
    byte_start: usize,
    byte_end: usize,
    entity_type: EntityType,
    explanation_code: &str,
    review_only: bool,
) {
    let start = byte_to_char(text, byte_start);
    let end = byte_to_char(text, byte_end);
    if findings
        .iter()
        .any(|item| item.start == start && item.end == end && item.entity_type == entity_type)
    {
        return;
    }
    findings.push(Finding {
        id: String::new(),
        part_id: "part-0001".to_owned(),
        start,
        end,
        matched_text: text[byte_start..byte_end].to_owned(),
        detector: "deterministic_rules_v1".to_owned(),
        confidence: if review_only { 0.75 } else { 1.0 },
        explanation_code: explanation_code.to_owned(),
        selected: !review_only,
        reviewed: false,
        replacement: entity_type.placeholder().to_owned(),
        entity_type,
    });
}

pub fn detect_exact_terms(
    text: &str,
    file_kind: &FileKind,
    scan_markdown_code: bool,
    terms: &[ExactTermPolicy],
) -> Vec<Finding> {
    let code_ranges = if *file_kind == FileKind::Markdown {
        fenced_code_ranges(text)
    } else {
        Vec::new()
    };
    let mut findings = Vec::new();
    for term in terms {
        for (byte_start, matched) in text.match_indices(&term.value) {
            let byte_end = byte_start + matched.len();
            let start = byte_to_char(text, byte_start);
            let end = byte_to_char(text, byte_end);
            let inside_code = inside_ranges(start, end, &code_ranges);
            if inside_code && !scan_markdown_code {
                continue;
            }
            add_finding(
                &mut findings,
                text,
                byte_start,
                byte_end,
                term.entity_type,
                if inside_code {
                    "USER_EXACT_TERM_MARKDOWN_CODE_REVIEW"
                } else {
                    "USER_EXACT_TERM"
                },
                inside_code,
            );
            let finding = findings.last_mut().expect("finding was just inserted");
            finding.detector = "policy_exact_terms_v1".to_owned();
        }
    }
    findings.sort_by_key(|item| (item.start, item.end));
    findings
}

fn inside_ranges(start: usize, end: usize, ranges: &[Range<usize>]) -> bool {
    ranges
        .iter()
        .any(|range| start < range.end && range.start < end)
}

pub fn detect(text: &str, file_kind: &FileKind) -> Vec<Finding> {
    detect_with_options(text, file_kind, true)
}

pub fn detect_with_options(
    text: &str,
    file_kind: &FileKind,
    scan_markdown_code: bool,
) -> Vec<Finding> {
    static EMAIL: OnceLock<Regex> = OnceLock::new();
    static PHONE: OnceLock<Regex> = OnceLock::new();
    static CN_ID: OnceLock<Regex> = OnceLock::new();
    static BANK: OnceLock<Regex> = OnceLock::new();
    static AMOUNT: OnceLock<Regex> = OnceLock::new();
    static PERCENT: OnceLock<Regex> = OnceLock::new();
    static ADDRESS: OnceLock<Regex> = OnceLock::new();
    static PROJECT: OnceLock<Regex> = OnceLock::new();
    static PROJECT_INLINE: OnceLock<Regex> = OnceLock::new();
    static CONTRACT: OnceLock<Regex> = OnceLock::new();

    let code_ranges = if *file_kind == FileKind::Markdown {
        fenced_code_ranges(text)
    } else {
        Vec::new()
    };
    let mut findings = Vec::new();

    for found in regex(
        r"(?i)(?:^|[^A-Za-z0-9_.+-])([A-Za-z0-9_.+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,})(?:$|[^A-Za-z0-9_.-])",
        &EMAIL,
    )
    .captures_iter(text)
    {
        let value = found.get(1).expect("capture exists");
        if !explicitly_public(text, value.start()) {
            add_finding(
                &mut findings,
                text,
                value.start(),
                value.end(),
                EntityType::Email,
                "EMAIL_FORMAT",
                false,
            );
        }
    }
    for found in
        regex(r"(?:^|\D)((?:1[3-9])(?:[ \t\r\n-]?\d){9})(?:$|\D)", &PHONE).captures_iter(text)
    {
        let value = found.get(1).expect("capture exists");
        if !explicitly_public(text, value.start()) {
            add_finding(
                &mut findings,
                text,
                value.start(),
                value.end(),
                EntityType::PhoneNumber,
                "CN_MOBILE_FORMAT",
                false,
            );
        }
    }
    for found in regex(
        r"(?:^|\D)((?:\d[ \t\r\n-]?){17}[\dXx])(?:$|[^\dXx])",
        &CN_ID,
    )
    .captures_iter(text)
    {
        let value = found.get(1).expect("capture exists");
        if valid_cn_id(value.as_str()) {
            add_finding(
                &mut findings,
                text,
                value.start(),
                value.end(),
                EntityType::CnIdNumber,
                "CN_ID_CHECKSUM",
                false,
            );
        }
    }
    for found in regex(r"(?:^|\D)((?:\d[ \t\r\n-]?){11,18}\d)(?:$|\D)", &BANK).captures_iter(text) {
        let value = found.get(1).expect("capture exists");
        let normalized: String = value
            .as_str()
            .chars()
            .filter(char::is_ascii_digit)
            .collect();
        let overlaps_cn_id = findings.iter().any(|item| {
            item.entity_type == EntityType::CnIdNumber
                && item.start == byte_to_char(text, value.start())
                && item.end == byte_to_char(text, value.end())
        });
        if valid_luhn(&normalized)
            && !overlaps_cn_id
            && !context(text, value.start(), value.end()).contains("不是银行卡")
        {
            add_finding(
                &mut findings,
                text,
                value.start(),
                value.end(),
                EntityType::BankCardNumber,
                "BANK_CARD_LUHN",
                false,
            );
        }
    }

    for found in regex(r"人民币\d{1,3}(?:,\d{3})*(?:\.\d{1,2})?元", &AMOUNT).find_iter(text) {
        let value_context = context(text, found.start(), found.end());
        let entity_type = if ["工资", "薪酬", "奖金", "税前"]
            .iter()
            .any(|cue| value_context.contains(cue))
        {
            Some(EntityType::SalaryAmount)
        } else if ["底价", "最低报价", "内部报价"]
            .iter()
            .any(|cue| value_context.contains(cue))
        {
            Some(EntityType::BusinessMetric)
        } else if ["内部预算", "预算草案", "未公开预算", "成本", "收入"]
            .iter()
            .any(|cue| value_context.contains(cue))
        {
            Some(EntityType::FinancialAmount)
        } else {
            None
        };
        if let Some(entity_type) = entity_type {
            add_finding(
                &mut findings,
                text,
                found.start(),
                found.end(),
                entity_type,
                "CONTEXTUAL_AMOUNT",
                false,
            );
        }
    }
    for found in regex(r"\d+(?:\.\d+)?%", &PERCENT).find_iter(text) {
        let value_context = context(text, found.start(), found.end());
        if ["未公开", "毛利率", "利润率", "内部"]
            .iter()
            .any(|cue| value_context.contains(cue))
        {
            add_finding(
                &mut findings,
                text,
                found.start(),
                found.end(),
                EntityType::BusinessMetric,
                "CONTEXTUAL_PERCENTAGE",
                false,
            );
        }
    }
    for found in
        regex(r"(?:办公)?地址\s*[:：]\s*([^，。,；;\n]{6,60})", &ADDRESS).captures_iter(text)
    {
        let value = found.get(1).expect("capture exists");
        if [
            "省", "市", "区", "县", "街", "路", "大道", "巷", "号", "栋", "室",
        ]
        .iter()
        .any(|cue| value.as_str().contains(cue))
        {
            add_finding(
                &mut findings,
                text,
                value.start(),
                value.end(),
                EntityType::Address,
                "ADDRESS_FIELD_CONTEXT",
                false,
            );
        }
    }
    for pattern in [
        regex(
            r"(?i)(?:保密)?项目(?:代号|代码| code)\s*[:：]\s*([\p{Han}A-Za-z][\p{Han}A-Za-z0-9_.-]{2,40})",
            &PROJECT,
        ),
        regex(
            r"项目([\p{Han}A-Za-z]{1,12}-\d{4}-[A-Za-z0-9]{1,12})(?:由|，|。|\s)",
            &PROJECT_INLINE,
        ),
    ] {
        for found in pattern.captures_iter(text) {
            let value = found.get(1).expect("capture exists");
            add_finding(
                &mut findings,
                text,
                value.start(),
                value.end(),
                EntityType::ProjectCode,
                "PROJECT_CODE_CONTEXT",
                false,
            );
        }
    }
    for found in regex(
        r"(?i)(?:合同|协议)(?:编号|号)\s*[:：]?\s*([A-Za-z][A-Za-z0-9_.-]{5,50})",
        &CONTRACT,
    )
    .captures_iter(text)
    {
        let value = found.get(1).expect("capture exists");
        add_finding(
            &mut findings,
            text,
            value.start(),
            value.end(),
            EntityType::ContractId,
            "CONTRACT_ID_CONTEXT",
            false,
        );
    }

    findings.retain_mut(|finding| {
        if !inside_ranges(finding.start, finding.end, &code_ranges) {
            return true;
        }
        if !scan_markdown_code {
            return false;
        }
        finding.selected = false;
        finding.confidence = finding.confidence.min(0.75);
        finding.explanation_code = format!("{}_MARKDOWN_CODE_REVIEW", finding.explanation_code);
        true
    });
    findings.sort_by_key(|item| (item.start, item.end));
    for (index, finding) in findings.iter_mut().enumerate() {
        finding.id = format!("finding-{:04}", index + 1);
    }
    findings
}

#[cfg(test)]
mod tests {
    use crate::model::{EntityType, FileKind};
    use crate::policy::ExactTermPolicy;

    use super::{detect, detect_exact_terms, detect_with_options};

    #[test]
    fn finds_high_risk_and_contextual_values() {
        let text = "联系人邮箱case@example.com，手机13800000001；内部预算人民币12,000.00元。";
        let labels: Vec<EntityType> = detect(text, &FileKind::Text)
            .into_iter()
            .map(|item| item.entity_type)
            .collect();
        assert!(labels.contains(&EntityType::Email));
        assert!(labels.contains(&EntityType::PhoneNumber));
        assert!(labels.contains(&EntityType::FinancialAmount));
    }

    #[test]
    fn public_cue_does_not_hide_an_earlier_private_value() {
        let findings = detect(
            "私有邮箱：case@example.com；旧手机号：13900000002，当前公开版本。",
            &FileKind::Text,
        );
        assert!(findings.iter().any(|finding| {
            finding.entity_type == EntityType::Email && finding.matched_text == "case@example.com"
        }));
        assert!(findings.iter().any(|finding| {
            finding.entity_type == EntityType::PhoneNumber && finding.matched_text == "13900000002"
        }));
    }

    #[test]
    fn detects_a_bank_card_split_across_visual_lines() {
        let findings = detect("银行卡号6222020000000001\n63。", &FileKind::Text);
        assert!(findings.iter().any(|finding| {
            finding.entity_type == EntityType::BankCardNumber
                && finding.matched_text == "6222020000000001\n63"
        }));
    }

    #[test]
    fn markdown_code_finding_requires_review() {
        let findings = detect(
            "```\n手机13800000001；内部预算人民币12,000.00元\n```\n",
            &FileKind::Markdown,
        );
        assert_eq!(findings.len(), 2);
        assert!(findings.iter().all(|finding| !finding.selected));
        assert!(
            findings
                .iter()
                .all(|finding| finding.explanation_code.ends_with("MARKDOWN_CODE_REVIEW"))
        );
    }

    #[test]
    fn markdown_code_can_be_excluded_by_policy() {
        let findings = detect_with_options(
            "正文手机13800000001\n```\n手机13900000002\n```\n",
            &FileKind::Markdown,
            false,
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].matched_text, "13800000001");
    }

    #[test]
    fn exact_terms_use_unicode_scalar_offsets_and_markdown_review() {
        let findings = detect_exact_terms(
            "🙂星海计划\n```\n星海计划\n```",
            &FileKind::Markdown,
            true,
            &[ExactTermPolicy {
                value: "星海计划".to_owned(),
                entity_type: EntityType::ProjectCode,
            }],
        );
        assert_eq!(findings.len(), 2);
        assert_eq!((findings[0].start, findings[0].end), (1, 5));
        assert!(findings[0].selected);
        assert!(!findings[1].selected);
        assert_eq!(findings[1].confidence, 0.75);
    }
}
