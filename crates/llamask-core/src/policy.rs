use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model::{EntityType, Finding};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EntityPolicy {
    pub enabled: bool,
    pub auto_apply: bool,
    pub replacement: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DetectorPolicy {
    pub id: String,
    pub enabled: bool,
    pub required: bool,
    pub priority: u8,
    pub min_confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExactTermPolicy {
    pub value: String,
    pub entity_type: EntityType,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConsistencyScope {
    Document,
    Task,
    Project,
    Global,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReplacementMappingPolicy {
    pub scope: ConsistencyScope,
    pub reversible: bool,
    pub key_reference: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImageMaskPolicy {
    /// OCR 文本框四周额外覆盖的像素数。
    pub safety_margin_px: u32,
    /// 实心遮罩颜色，按 RGB 顺序保存。
    pub solid_rgb: [u8; 3],
    /// 低于该置信度的 OCR 行仍会进入任务，但不会自动确认。
    pub auto_apply_min_ocr_confidence: f32,
}

impl Default for ImageMaskPolicy {
    fn default() -> Self {
        Self {
            safety_margin_px: 4,
            solid_rgb: [0, 0, 0],
            auto_apply_min_ocr_confidence: 0.90,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PolicyConfig {
    pub schema_version: u32,
    pub id: String,
    pub auto_apply_min_confidence: f32,
    pub scan_markdown_code: bool,
    pub entities: BTreeMap<EntityType, EntityPolicy>,
    pub detectors: Vec<DetectorPolicy>,
    #[serde(default)]
    pub exact_terms: Vec<ExactTermPolicy>,
    #[serde(default)]
    pub allowlist: Vec<String>,
    #[serde(default)]
    pub image_mask: ImageMaskPolicy,
    pub replacement_mapping: ReplacementMappingPolicy,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PolicyError {
    #[error("只支持策略 schema_version=1")]
    UnsupportedSchema,
    #[error("策略 id 不能为空")]
    EmptyId,
    #[error("自动处理置信度必须位于 0 到 1 之间")]
    InvalidAutoApplyConfidence,
    #[error("检测器 {0} 的置信度必须位于 0 到 1 之间")]
    InvalidDetectorConfidence(String),
    #[error("检测器 id 不能为空")]
    EmptyDetectorId,
    #[error("检测器 id 重复：{0}")]
    DuplicateDetector(String),
    #[error("实体 {0:?} 的替换内容超过 256 个字符")]
    ReplacementTooLong(EntityType),
    #[error("精确敏感词不能为空或超过 256 个字符")]
    InvalidExactTerm,
    #[error("精确敏感词重复：{0}")]
    DuplicateExactTerm(String),
    #[error("白名单值不能为空或超过 256 个字符")]
    InvalidAllowlistValue,
    #[error("白名单值重复：{0}")]
    DuplicateAllowlistValue(String),
    #[error("启用可逆恢复时必须提供非空的 key_reference")]
    MissingKeyReference,
    #[error("当前版本尚未实现可逆恢复，不能静默忽略该配置")]
    ReversibleMappingNotImplemented,
    #[error("当前版本尚未实现 project/global 持久映射")]
    PersistentMappingNotImplemented,
    #[error("图片遮罩安全边距不能超过 128 像素")]
    ImageMarginTooLarge,
    #[error("图片 OCR 自动确认置信度必须位于 0 到 1 之间")]
    InvalidImageOcrConfidence,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        let entities = EntityType::ALL
            .into_iter()
            .map(|entity_type| {
                (
                    entity_type,
                    EntityPolicy {
                        enabled: true,
                        auto_apply: true,
                        replacement: entity_type.placeholder().to_owned(),
                    },
                )
            })
            .collect();
        Self {
            schema_version: 1,
            id: "default-high-recall".to_owned(),
            auto_apply_min_confidence: 0.90,
            scan_markdown_code: true,
            entities,
            detectors: vec![
                DetectorPolicy {
                    id: "siamese_uie".to_owned(),
                    enabled: true,
                    required: false,
                    priority: 50,
                    min_confidence: 0.65,
                },
                DetectorPolicy {
                    id: "qwen_review".to_owned(),
                    enabled: true,
                    required: false,
                    priority: 30,
                    min_confidence: 0.70,
                },
            ],
            exact_terms: Vec::new(),
            allowlist: Vec::new(),
            image_mask: ImageMaskPolicy::default(),
            replacement_mapping: ReplacementMappingPolicy {
                scope: ConsistencyScope::Task,
                reversible: false,
                key_reference: None,
            },
        }
    }
}

impl PolicyConfig {
    pub fn validate(&self) -> Result<(), PolicyError> {
        if self.schema_version != 1 {
            return Err(PolicyError::UnsupportedSchema);
        }
        if self.id.trim().is_empty() {
            return Err(PolicyError::EmptyId);
        }
        if !valid_confidence(self.auto_apply_min_confidence) {
            return Err(PolicyError::InvalidAutoApplyConfidence);
        }
        let mut detector_ids = BTreeSet::new();
        for detector in &self.detectors {
            if detector.id.trim().is_empty() {
                return Err(PolicyError::EmptyDetectorId);
            }
            if !detector_ids.insert(detector.id.as_str()) {
                return Err(PolicyError::DuplicateDetector(detector.id.clone()));
            }
            if !valid_confidence(detector.min_confidence) {
                return Err(PolicyError::InvalidDetectorConfidence(detector.id.clone()));
            }
        }
        for (entity_type, entity) in &self.entities {
            if entity.replacement.chars().count() > 256 {
                return Err(PolicyError::ReplacementTooLong(*entity_type));
            }
        }
        let mut exact_terms = BTreeSet::new();
        for term in &self.exact_terms {
            let length = term.value.chars().count();
            if length == 0 || length > 256 {
                return Err(PolicyError::InvalidExactTerm);
            }
            if !exact_terms.insert((term.entity_type, term.value.as_str())) {
                return Err(PolicyError::DuplicateExactTerm(term.value.clone()));
            }
        }
        let mut allowlist = BTreeSet::new();
        for value in &self.allowlist {
            let length = value.chars().count();
            if length == 0 || length > 256 {
                return Err(PolicyError::InvalidAllowlistValue);
            }
            if !allowlist.insert(value.as_str()) {
                return Err(PolicyError::DuplicateAllowlistValue(value.clone()));
            }
        }
        if self.replacement_mapping.reversible
            && self
                .replacement_mapping
                .key_reference
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
        {
            return Err(PolicyError::MissingKeyReference);
        }
        if self.replacement_mapping.reversible {
            return Err(PolicyError::ReversibleMappingNotImplemented);
        }
        if matches!(
            self.replacement_mapping.scope,
            ConsistencyScope::Project | ConsistencyScope::Global
        ) {
            return Err(PolicyError::PersistentMappingNotImplemented);
        }
        if self.image_mask.safety_margin_px > 128 {
            return Err(PolicyError::ImageMarginTooLarge);
        }
        if !valid_confidence(self.image_mask.auto_apply_min_ocr_confidence) {
            return Err(PolicyError::InvalidImageOcrConfidence);
        }
        Ok(())
    }

    pub fn detector(&self, id: &str) -> Option<&DetectorPolicy> {
        self.detectors.iter().find(|detector| detector.id == id)
    }

    pub fn apply_to_findings(&self, findings: &mut Vec<Finding>) {
        self.filter_findings(findings);
        for finding in findings {
            let entity = self
                .entities
                .get(&finding.entity_type)
                .expect("disabled and missing entities were removed");
            finding.replacement.clone_from(&entity.replacement);
            finding.selected = finding.selected
                && entity.auto_apply
                && finding.confidence >= self.auto_apply_min_confidence;
            finding.reviewed = finding.selected;
        }
    }

    pub fn filter_findings(&self, findings: &mut Vec<Finding>) {
        findings.retain(|finding| {
            !self
                .allowlist
                .iter()
                .any(|value| value == &finding.matched_text)
                && self
                    .entities
                    .get(&finding.entity_type)
                    .is_some_and(|entity| entity.enabled)
        });
    }
}

fn valid_confidence(value: f32) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}

#[cfg(test)]
mod tests {
    use crate::model::{EntityType, Finding};

    use super::{ExactTermPolicy, PolicyConfig, PolicyError};

    #[test]
    fn default_policy_round_trips_and_covers_every_entity() {
        let policy = PolicyConfig::default();
        policy.validate().unwrap();
        let json = serde_json::to_vec_pretty(&policy).unwrap();
        let restored: PolicyConfig = serde_json::from_slice(&json).unwrap();
        assert_eq!(restored, policy);
        assert_eq!(restored.entities.len(), EntityType::ALL.len());
    }

    #[test]
    fn older_policy_json_gets_safe_image_defaults() {
        let mut value = serde_json::to_value(PolicyConfig::default()).unwrap();
        value.as_object_mut().unwrap().remove("image_mask");
        let restored: PolicyConfig = serde_json::from_value(value).unwrap();
        assert_eq!(restored.image_mask.safety_margin_px, 4);
        assert_eq!(restored.image_mask.solid_rgb, [0, 0, 0]);
        assert_eq!(restored.image_mask.auto_apply_min_ocr_confidence, 0.90);
    }

    #[test]
    fn policy_controls_selection_and_replacement() {
        let mut policy = PolicyConfig::default();
        let email = policy.entities.get_mut(&EntityType::Email).unwrap();
        email.auto_apply = false;
        email.replacement = "【邮箱已隐藏】".to_owned();
        let mut findings = vec![Finding {
            id: "f-1".to_owned(),
            part_id: "part-0001".to_owned(),
            start: 0,
            end: 16,
            entity_type: EntityType::Email,
            matched_text: "case@example.com".to_owned(),
            detector: "test".to_owned(),
            confidence: 1.0,
            explanation_code: "TEST".to_owned(),
            selected: true,
            reviewed: false,
            replacement: String::new(),
        }];

        policy.apply_to_findings(&mut findings);
        assert!(!findings[0].selected);
        assert!(!findings[0].reviewed);
        assert_eq!(findings[0].replacement, "【邮箱已隐藏】");
    }

    #[test]
    fn rejects_duplicate_exact_terms_and_allowlist_values() {
        let mut policy = PolicyConfig::default();
        let term = ExactTermPolicy {
            value: "星海计划".to_owned(),
            entity_type: EntityType::ProjectCode,
        };
        policy.exact_terms = vec![term.clone(), term];
        assert_eq!(
            policy.validate(),
            Err(PolicyError::DuplicateExactTerm("星海计划".to_owned()))
        );

        policy.exact_terms.clear();
        policy.allowlist = vec!["公开邮箱".to_owned(), "公开邮箱".to_owned()];
        assert_eq!(
            policy.validate(),
            Err(PolicyError::DuplicateAllowlistValue("公开邮箱".to_owned()))
        );
    }

    #[test]
    fn reversible_policy_requires_a_key_reference() {
        let mut policy = PolicyConfig::default();
        policy.replacement_mapping.reversible = true;
        assert_eq!(policy.validate(), Err(PolicyError::MissingKeyReference));
        policy.replacement_mapping.key_reference = Some("os-keychain:test".to_owned());
        assert_eq!(
            policy.validate(),
            Err(PolicyError::ReversibleMappingNotImplemented)
        );
    }
}
