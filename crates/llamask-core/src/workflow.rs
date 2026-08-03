use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use tempfile::NamedTempFile;
use thiserror::Error;

use crate::detect;
use crate::model::{
    DiagnosticSeverity, DocumentGraph, EntityType, FileKind, Finding, ResidualFinding,
    TaskDiagnostic, TaskDraft, VerificationReport,
};
use crate::policy::{DetectorPolicy, PolicyConfig, PolicyError};
use crate::sidecar::{
    RuntimeRegistry, SIDECAR_PROTOCOL_VERSION, SidecarRequest, candidates_from_findings, invoke,
};
use crate::text::{apply_findings, document_from_text, read_document, sha256_hex};

#[derive(Debug, Error)]
pub enum WorkflowError {
    #[error("文件读写失败：{0}")]
    Io(#[from] std::io::Error),
    #[error("任务文件格式无效：{0}")]
    Json(#[from] serde_json::Error),
    #[error("暂不支持此文件类型：{0}")]
    UnsupportedFileType(String),
    #[error("文件不是有效 UTF-8，当前原型暂不支持：{0}")]
    UnsupportedEncoding(PathBuf),
    #[error("源文件在扫描后发生了变化，请重新扫描")]
    SourceChanged,
    #[error("输出路径不能与源文件相同")]
    WouldOverwriteSource,
    #[error("输出文件已存在，未执行覆盖：{0}")]
    OutputExists(PathBuf),
    #[error("命中范围无效：{0}")]
    InvalidSpan(String),
    #[error("命中原文与当前文件不一致：{0}")]
    SpanTextMismatch(String),
    #[error("选中的命中范围互相重叠")]
    OverlappingFindings,
    #[error("残留复扫失败，发现 {0} 个未处理结果")]
    VerificationFailed(usize),
    #[error("仍有 {0} 个结果尚未复核")]
    UnreviewedFindings(usize),
    #[error("文档没有可处理的文本部分")]
    MissingTextPart,
    #[error("策略配置无效：{0}")]
    InvalidPolicy(#[from] PolicyError),
    #[error("必需的本地模型不可用：{0}")]
    RequiredDetectorUnavailable(String),
    #[error("任务中的 policy_id 与策略快照不一致")]
    PolicySnapshotMismatch,
    #[error("找不到文本复核结果：{0}")]
    FindingNotFound(String),
    #[error("替换内容不能超过 256 个字符")]
    ReplacementTooLong,
}

pub fn scan_path(path: &Path) -> Result<TaskDraft, WorkflowError> {
    scan_path_with_policy(path, &PolicyConfig::default(), None)
}

pub(crate) struct DetectionRun {
    pub(crate) findings: Vec<Finding>,
    pub(crate) diagnostics: Vec<TaskDiagnostic>,
    pub(crate) detectors_checked: Vec<String>,
}

fn merge_policy_findings(findings: &mut Vec<Finding>, mut explicit: Vec<Finding>) {
    explicit.sort_by_key(|finding| std::cmp::Reverse(finding.end - finding.start));
    for candidate in explicit {
        if findings.iter().any(|existing| {
            existing.detector == "policy_exact_terms_v1" && overlaps(existing, &candidate)
        }) {
            continue;
        }
        findings.retain(|existing| !overlaps(existing, &candidate));
        findings.push(candidate);
    }
}

pub(crate) fn run_detection(
    text: &str,
    file_kind: &FileKind,
    document_id: &str,
    part_id: &str,
    policy: &PolicyConfig,
    runtimes: Option<&RuntimeRegistry>,
    phase: &str,
) -> Result<DetectionRun, WorkflowError> {
    let mut findings = detect::detect_with_options(text, file_kind, policy.scan_markdown_code);
    let exact_findings = detect::detect_exact_terms(
        text,
        file_kind,
        policy.scan_markdown_code,
        &policy.exact_terms,
    );
    merge_policy_findings(&mut findings, exact_findings);
    policy.filter_findings(&mut findings);

    let mut diagnostics = Vec::new();
    let mut detectors_checked = vec!["deterministic_rules_v1".to_owned()];
    if !policy.exact_terms.is_empty() {
        detectors_checked.push("policy_exact_terms_v1".to_owned());
    }
    let mut enabled_detectors: Vec<&DetectorPolicy> = policy
        .detectors
        .iter()
        .filter(|detector| detector.enabled)
        .collect();
    enabled_detectors.sort_by_key(|detector| std::cmp::Reverse(detector.priority));
    for detector in enabled_detectors {
        let Some(runtime) = runtimes.and_then(|registry| registry.runtime(&detector.id)) else {
            if detector.required {
                return Err(WorkflowError::RequiredDetectorUnavailable(
                    detector.id.clone(),
                ));
            }
            diagnostics.push(model_diagnostic(
                "MODEL_RUNTIME_NOT_CONFIGURED",
                &detector.id,
                "本地模型运行项未配置，本次检测已使用其余检测器。",
            ));
            continue;
        };
        let request = SidecarRequest {
            protocol_version: SIDECAR_PROTOCOL_VERSION,
            request_id: format!("{document_id}-{phase}-{}", detector.id),
            detector_id: detector.id.clone(),
            document_id: document_id.to_owned(),
            part_id: part_id.to_owned(),
            offset_unit: "unicode_scalar".to_owned(),
            text: text.to_owned(),
            candidates: candidates_from_findings(&findings),
        };
        match invoke(runtime, &request, detector.min_confidence) {
            Ok(result) => {
                merge_model_findings(&mut findings, result.findings);
                detectors_checked.push(detector.id.clone());
                diagnostics.extend(
                    result
                        .warnings
                        .iter()
                        .take(16)
                        .map(|warning| sidecar_warning_diagnostic(&detector.id, warning)),
                );
            }
            Err(_) if detector.required => {
                return Err(WorkflowError::RequiredDetectorUnavailable(
                    detector.id.clone(),
                ));
            }
            Err(_) => diagnostics.push(model_diagnostic(
                "MODEL_RUNTIME_FAILED",
                &detector.id,
                "本地模型未成功完成，本次检测已使用其余检测器。",
            )),
        }
    }
    policy.filter_findings(&mut findings);
    findings.sort_by_key(|finding| (finding.start, finding.end));
    for (index, finding) in findings.iter_mut().enumerate() {
        finding.id = format!("finding-{:04}", index + 1);
    }
    detectors_checked.sort();
    Ok(DetectionRun {
        findings,
        diagnostics,
        detectors_checked,
    })
}

fn scan_document_with_policy(
    document: DocumentGraph,
    policy: &PolicyConfig,
    runtimes: Option<&RuntimeRegistry>,
) -> Result<TaskDraft, WorkflowError> {
    policy.validate()?;
    let part = document
        .parts
        .first()
        .ok_or(WorkflowError::MissingTextPart)?;
    let task_id = format!("task-{}", &document.source.sha256[..12]);
    let mut detection = run_detection(
        &part.text,
        &document.source.file_kind,
        &document.source.sha256,
        &part.id,
        policy,
        runtimes,
        "scan",
    )?;
    policy.apply_to_findings(&mut detection.findings);

    Ok(TaskDraft {
        schema_version: 3,
        task_id,
        policy_id: policy.id.clone(),
        policy: policy.clone(),
        document,
        findings: detection.findings,
        diagnostics: detection.diagnostics,
        contains_sensitive_plaintext: true,
    })
}

pub fn scan_path_with_policy(
    path: &Path,
    policy: &PolicyConfig,
    runtimes: Option<&RuntimeRegistry>,
) -> Result<TaskDraft, WorkflowError> {
    let document = read_document(path)?;
    scan_document_with_policy(document, policy, runtimes)
}

pub fn scan_text_with_policy(
    text: String,
    policy: &PolicyConfig,
    runtimes: Option<&RuntimeRegistry>,
) -> Result<TaskDraft, WorkflowError> {
    let document = document_from_text(text, "stdin://clipboard", FileKind::Clipboard);
    scan_document_with_policy(document, policy, runtimes)
}

pub fn review_text_finding(
    task: &mut TaskDraft,
    finding_id: &str,
    selected: bool,
    replacement: Option<&str>,
) -> Result<(), WorkflowError> {
    validate_task(task)?;
    review_finding(&mut task.findings, finding_id, selected, replacement)
}

pub(crate) fn review_finding(
    findings: &mut [Finding],
    finding_id: &str,
    selected: bool,
    replacement: Option<&str>,
) -> Result<(), WorkflowError> {
    let finding = findings
        .iter_mut()
        .find(|finding| finding.id == finding_id)
        .ok_or_else(|| WorkflowError::FindingNotFound(finding_id.to_owned()))?;
    if let Some(replacement) = replacement {
        if replacement.chars().count() > 256 {
            return Err(WorkflowError::ReplacementTooLong);
        }
        finding.replacement = replacement.to_owned();
    }
    finding.selected = selected;
    finding.reviewed = true;
    Ok(())
}

fn sidecar_warning_diagnostic(detector_id: &str, warning: &str) -> TaskDiagnostic {
    match warning {
        "SIAMESE_UIE_CONFIDENCE_IS_NOT_CALIBRATED" | "QWEN_CONFIDENCE_IS_NOT_CALIBRATED" => {
            model_diagnostic(
                "MODEL_CONFIDENCE_UNCALIBRATED",
                detector_id,
                "该模型的置信度尚未完成产品校准，默认结果需要人工确认。",
            )
        }
        _ => model_diagnostic(
            "MODEL_RUNTIME_WARNING",
            detector_id,
            "本地模型返回了运行提示；为避免原文进入日志，详细内容未保存。",
        ),
    }
}

fn model_diagnostic(code: &str, detector_id: &str, message: &str) -> TaskDiagnostic {
    TaskDiagnostic {
        severity: DiagnosticSeverity::Warning,
        code: code.to_owned(),
        detector_id: Some(detector_id.to_owned()),
        message: message.to_owned(),
    }
}

fn overlaps(left: &Finding, right: &Finding) -> bool {
    left.start < right.end && right.start < left.end
}

fn merge_model_findings(findings: &mut Vec<Finding>, mut incoming: Vec<Finding>) {
    incoming.sort_by(|left, right| {
        right
            .confidence
            .total_cmp(&left.confidence)
            .then_with(|| (right.end - right.start).cmp(&(left.end - left.start)))
    });
    for candidate in incoming {
        if findings
            .iter()
            .any(|existing| overlaps(existing, &candidate))
        {
            continue;
        }
        findings.push(candidate);
    }
}

fn current_source_bytes(task: &TaskDraft) -> Result<Vec<u8>, WorkflowError> {
    if task.document.source.file_kind == FileKind::Clipboard {
        let part = task
            .document
            .parts
            .first()
            .ok_or(WorkflowError::MissingTextPart)?;
        let bytes = encode_output(task, &part.text);
        if sha256_hex(&bytes) != task.document.source.sha256 {
            return Err(WorkflowError::SourceChanged);
        }
        return Ok(bytes);
    }
    let bytes = fs::read(&task.document.source.path)?;
    if sha256_hex(&bytes) != task.document.source.sha256 {
        return Err(WorkflowError::SourceChanged);
    }
    Ok(bytes)
}

fn encode_output(task: &TaskDraft, text: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(text.len() + 3);
    if task.document.source.has_utf8_bom {
        bytes.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
    }
    bytes.extend_from_slice(text.as_bytes());
    bytes
}

fn target_residuals(task: &TaskDraft, text: &str) -> Vec<ResidualFinding> {
    let mut residuals = Vec::new();
    let mut seen = BTreeSet::new();
    for finding in task.findings.iter().filter(|finding| finding.selected) {
        for (byte_start, matched) in text.match_indices(&finding.matched_text) {
            let start = text[..byte_start].chars().count();
            let end = start + matched.chars().count();
            if seen.insert((start, end, finding.entity_type)) {
                residuals.push(ResidualFinding {
                    start,
                    end,
                    entity_type: finding.entity_type,
                    detector: "target_value_check".to_owned(),
                    explanation_code: "SELECTED_VALUE_REMAINS".to_owned(),
                });
            }
        }
    }
    residuals
}

fn verify_text_with_runtimes(
    task: &TaskDraft,
    text: &str,
    checked_file: &str,
    bytes: &[u8],
    runtimes: Option<&RuntimeRegistry>,
) -> Result<VerificationReport, WorkflowError> {
    let unreviewed_findings = task
        .findings
        .iter()
        .filter(|finding| !finding.reviewed)
        .count();
    let mut residual_findings = target_residuals(task, text);
    let target_residual_count = residual_findings.len();
    let mut seen: BTreeSet<(usize, usize, EntityType)> = residual_findings
        .iter()
        .map(|finding| (finding.start, finding.end, finding.entity_type))
        .collect();
    let accepted_keep: BTreeSet<(EntityType, String)> = task
        .findings
        .iter()
        .filter(|finding| finding.reviewed && !finding.selected)
        .map(|finding| (finding.entity_type, finding.matched_text.clone()))
        .collect();
    let output_sha256 = sha256_hex(bytes);
    let detection = run_detection(
        text,
        &task.document.source.file_kind,
        &output_sha256,
        "part-0001",
        &task.policy,
        runtimes,
        "verify",
    )?;
    for finding in detection.findings {
        let value_key = (finding.entity_type, finding.matched_text.clone());
        if accepted_keep.contains(&value_key) {
            continue;
        }
        if seen.insert((finding.start, finding.end, finding.entity_type)) {
            residual_findings.push(ResidualFinding {
                start: finding.start,
                end: finding.end,
                entity_type: finding.entity_type,
                detector: finding.detector,
                explanation_code: finding.explanation_code,
            });
        }
    }
    residual_findings.sort_by_key(|finding| (finding.start, finding.end, finding.entity_type));
    let complete = task
        .policy
        .detectors
        .iter()
        .filter(|detector| detector.enabled)
        .all(|detector| detection.detectors_checked.contains(&detector.id));
    Ok(VerificationReport {
        schema_version: 2,
        passed: unreviewed_findings == 0 && residual_findings.is_empty(),
        complete,
        checked_file: checked_file.to_owned(),
        sha256: output_sha256,
        selected_findings: task
            .findings
            .iter()
            .filter(|finding| finding.selected)
            .count(),
        unreviewed_findings,
        target_residual_count,
        residual_findings,
        detectors_checked: detection.detectors_checked,
        diagnostics: detection.diagnostics,
    })
}

fn validate_task(task: &TaskDraft) -> Result<(), WorkflowError> {
    task.policy.validate()?;
    if task.policy_id != task.policy.id {
        return Err(WorkflowError::PolicySnapshotMismatch);
    }
    Ok(())
}

pub fn export_task_with_runtimes(
    task: &TaskDraft,
    output: &Path,
    runtimes: Option<&RuntimeRegistry>,
) -> Result<VerificationReport, WorkflowError> {
    validate_task(task)?;
    let unreviewed = task
        .findings
        .iter()
        .filter(|finding| !finding.reviewed)
        .count();
    if unreviewed > 0 {
        return Err(WorkflowError::UnreviewedFindings(unreviewed));
    }
    let output_absolute = if output.is_absolute() {
        output.to_path_buf()
    } else {
        std::env::current_dir()?.join(output)
    };
    if task.document.source.file_kind != FileKind::Clipboard {
        let source = fs::canonicalize(&task.document.source.path)?;
        if source == output_absolute {
            return Err(WorkflowError::WouldOverwriteSource);
        }
    }
    if fs::symlink_metadata(output).is_ok() {
        if task.document.source.file_kind != FileKind::Clipboard {
            let source = fs::canonicalize(&task.document.source.path)?;
            if fs::canonicalize(output).is_ok_and(|existing| existing == source) {
                return Err(WorkflowError::WouldOverwriteSource);
            }
        }
        return Err(WorkflowError::OutputExists(output.to_path_buf()));
    }
    current_source_bytes(task)?;
    let part = task
        .document
        .parts
        .first()
        .ok_or(WorkflowError::MissingTextPart)?;
    let redacted = apply_findings(&part.text, &task.findings)?;
    let bytes = encode_output(task, &redacted);
    let report =
        verify_text_with_runtimes(task, &redacted, &output.to_string_lossy(), &bytes, runtimes)?;
    if !report.passed {
        return Err(WorkflowError::VerificationFailed(
            report.unreviewed_findings + report.residual_findings.len(),
        ));
    }

    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist_noclobber(output)
        .map_err(|error| match error.error.kind() {
            std::io::ErrorKind::AlreadyExists => WorkflowError::OutputExists(output.to_path_buf()),
            _ => WorkflowError::Io(error.error),
        })?;
    Ok(report)
}

pub fn export_task(task: &TaskDraft, output: &Path) -> Result<VerificationReport, WorkflowError> {
    export_task_with_runtimes(task, output, None)
}

pub fn render_task_with_runtimes(
    task: &TaskDraft,
    runtimes: Option<&RuntimeRegistry>,
) -> Result<(String, VerificationReport), WorkflowError> {
    validate_task(task)?;
    let unreviewed = task
        .findings
        .iter()
        .filter(|finding| !finding.reviewed)
        .count();
    if unreviewed > 0 {
        return Err(WorkflowError::UnreviewedFindings(unreviewed));
    }
    current_source_bytes(task)?;
    let part = task
        .document
        .parts
        .first()
        .ok_or(WorkflowError::MissingTextPart)?;
    let redacted = apply_findings(&part.text, &task.findings)?;
    let bytes = encode_output(task, &redacted);
    let report = verify_text_with_runtimes(task, &redacted, "stdout", &bytes, runtimes)?;
    if !report.passed {
        if report.unreviewed_findings > 0 {
            return Err(WorkflowError::UnreviewedFindings(
                report.unreviewed_findings,
            ));
        }
        return Err(WorkflowError::VerificationFailed(
            report.residual_findings.len(),
        ));
    }
    Ok((redacted, report))
}

pub fn verify_file_with_runtimes(
    task: &TaskDraft,
    path: &Path,
    runtimes: Option<&RuntimeRegistry>,
) -> Result<VerificationReport, WorkflowError> {
    validate_task(task)?;
    let bytes = fs::read(path)?;
    let content = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        &bytes[3..]
    } else {
        &bytes
    };
    let text = std::str::from_utf8(content)
        .map_err(|_| WorkflowError::UnsupportedEncoding(path.to_path_buf()))?;
    verify_text_with_runtimes(task, text, &path.to_string_lossy(), &bytes, runtimes)
}

pub fn verify_file(task: &TaskDraft, path: &Path) -> Result<VerificationReport, WorkflowError> {
    verify_file_with_runtimes(task, path, None)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use crate::model::EntityType;
    use crate::policy::{ExactTermPolicy, PolicyConfig};

    use super::{
        WorkflowError, export_task, render_task_with_runtimes, review_text_finding, scan_path,
        scan_path_with_policy, scan_text_with_policy, verify_file,
    };

    #[test]
    fn text_review_updates_selection_and_replacement_without_changing_source() {
        let mut task = scan_text_with_policy(
            "联系邮箱 case@example.com".to_owned(),
            &PolicyConfig::default(),
            None,
        )
        .unwrap();
        let finding_id = task.findings[0].id.clone();
        let source = task.document.parts[0].text.clone();

        review_text_finding(&mut task, &finding_id, true, Some("[私人邮箱]")).unwrap();

        assert!(task.findings[0].selected);
        assert!(task.findings[0].reviewed);
        assert_eq!(task.findings[0].replacement, "[私人邮箱]");
        assert_eq!(task.document.parts[0].text, source);
    }

    #[test]
    fn text_review_rejects_unknown_findings_and_oversized_replacements() {
        let mut task = scan_text_with_policy(
            "联系邮箱 case@example.com".to_owned(),
            &PolicyConfig::default(),
            None,
        )
        .unwrap();
        assert!(matches!(
            review_text_finding(&mut task, "missing", false, None),
            Err(WorkflowError::FindingNotFound(_))
        ));
        let finding_id = task.findings[0].id.clone();
        assert!(matches!(
            review_text_finding(&mut task, &finding_id, true, Some(&"x".repeat(257))),
            Err(WorkflowError::ReplacementTooLong)
        ));
    }

    #[test]
    fn scan_export_verify_preserves_source() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("sample.txt");
        let output = directory.path().join("sample_已脱敏.txt");
        fs::write(&source, "手机：13800000001").unwrap();
        let before = fs::read(&source).unwrap();
        let task = scan_path(&source).unwrap();
        let report = export_task(&task, &output).unwrap();
        assert!(report.passed);
        assert_eq!(fs::read(&source).unwrap(), before);
        assert_eq!(fs::read_to_string(&output).unwrap(), "手机：[手机号]");
        assert!(verify_file(&task, &output).unwrap().passed);
    }

    #[test]
    fn never_overwrites_source_or_existing_output() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("sample.txt");
        let output = directory.path().join("existing.txt");
        fs::write(&source, "手机：13800000001").unwrap();
        fs::write(&output, "keep me").unwrap();
        let task = scan_path(&source).unwrap();

        assert!(matches!(
            export_task(&task, &source),
            Err(WorkflowError::WouldOverwriteSource)
        ));
        assert!(matches!(
            export_task(&task, &output),
            Err(WorkflowError::OutputExists(_))
        ));
        assert_eq!(fs::read_to_string(&source).unwrap(), "手机：13800000001");
        assert_eq!(fs::read_to_string(&output).unwrap(), "keep me");
    }

    #[test]
    fn rejects_a_source_that_changed_after_scan() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("sample.txt");
        let output = directory.path().join("redacted.txt");
        fs::write(&source, "手机：13800000001").unwrap();
        let task = scan_path(&source).unwrap();
        fs::write(&source, "手机：13900000002").unwrap();

        assert!(matches!(
            export_task(&task, &output),
            Err(WorkflowError::SourceChanged)
        ));
        assert!(!output.exists());
    }

    #[test]
    fn honors_manual_selection_and_replacement_changes() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("sample.txt");
        let output = directory.path().join("redacted.txt");
        fs::write(&source, "邮箱：case@example.com，手机：13800000001").unwrap();
        let mut task = scan_path(&source).unwrap();
        task.findings[0].selected = false;
        task.findings[1].replacement = "【已隐藏】".to_owned();

        export_task(&task, &output).unwrap();
        assert_eq!(
            fs::read_to_string(&output).unwrap(),
            "邮箱：case@example.com，手机：【已隐藏】"
        );
    }

    #[test]
    fn blocks_an_unreviewed_finding() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("sample.txt");
        let output = directory.path().join("redacted.txt");
        fs::write(&source, "手机：13800000001").unwrap();
        let mut task = scan_path(&source).unwrap();
        task.findings[0].selected = false;
        task.findings[0].reviewed = false;

        assert!(matches!(
            export_task(&task, &output),
            Err(WorkflowError::UnreviewedFindings(1))
        ));
        assert!(!output.exists());
    }

    #[test]
    fn independent_rescan_blocks_sensitive_replacement_content() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("sample.txt");
        let output = directory.path().join("redacted.txt");
        fs::write(&source, "手机：13800000001").unwrap();
        let mut task = scan_path(&source).unwrap();
        task.findings[0].replacement = "13900000002".to_owned();

        assert!(matches!(
            export_task(&task, &output),
            Err(WorkflowError::VerificationFailed(1))
        ));
        assert!(!output.exists());
    }

    #[test]
    fn exact_terms_and_allowlist_are_applied_before_export() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("sample.txt");
        fs::write(&source, "公开邮箱case@example.com，内部代号星海计划").unwrap();
        let mut policy = PolicyConfig::default();
        policy.detectors.clear();
        policy.exact_terms.push(ExactTermPolicy {
            value: "星海计划".to_owned(),
            entity_type: EntityType::ProjectCode,
        });
        policy.allowlist.push("case@example.com".to_owned());

        let task = scan_path_with_policy(&source, &policy, None).unwrap();
        assert_eq!(task.findings.len(), 1);
        assert_eq!(task.findings[0].matched_text, "星海计划");
        assert_eq!(task.findings[0].detector, "policy_exact_terms_v1");
        assert!(task.findings[0].reviewed);
    }

    #[test]
    fn stdin_text_can_render_without_a_source_file() {
        let mut policy = PolicyConfig::default();
        policy.detectors.clear();
        let task =
            scan_text_with_policy("邮箱：case@example.com".to_owned(), &policy, None).unwrap();
        let (rendered, report) = render_task_with_runtimes(&task, None).unwrap();
        assert_eq!(rendered, "邮箱：[邮箱]");
        assert!(report.passed);
        assert!(report.complete);
    }

    #[test]
    fn verification_report_does_not_repeat_sensitive_values() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("sample.txt");
        fs::write(&source, "手机：13800000001").unwrap();
        let task = scan_path(&source).unwrap();
        let report = verify_file(&task, &source).unwrap();
        let serialized = serde_json::to_string(&report).unwrap();
        assert!(!report.passed);
        assert_eq!(report.target_residual_count, 1);
        assert!(!serialized.contains("13800000001"));
    }

    #[test]
    fn optional_models_degrade_with_explicit_diagnostics() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("sample.txt");
        fs::write(&source, "手机：13800000001").unwrap();

        let task = scan_path_with_policy(&source, &PolicyConfig::default(), None).unwrap();
        assert_eq!(task.findings.len(), 1);
        assert_eq!(task.diagnostics.len(), 2);
        assert!(
            task.diagnostics
                .iter()
                .all(|item| item.code == "MODEL_RUNTIME_NOT_CONFIGURED")
        );
    }

    #[test]
    fn a_required_missing_model_blocks_scanning() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("sample.txt");
        fs::write(&source, "手机：13800000001").unwrap();
        let mut policy = PolicyConfig::default();
        policy.detectors[0].required = true;

        assert!(matches!(
            scan_path_with_policy(&source, &policy, None),
            Err(WorkflowError::RequiredDetectorUnavailable(id)) if id == "siamese_uie"
        ));
    }
}
