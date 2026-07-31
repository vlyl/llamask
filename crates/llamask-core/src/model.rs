use serde::{Deserialize, Serialize};

use crate::policy::PolicyConfig;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    Text,
    Markdown,
    Clipboard,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LineEnding {
    None,
    Lf,
    CrLf,
    Cr,
    Mixed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceMetadata {
    pub path: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub file_kind: FileKind,
    pub encoding: String,
    pub has_utf8_bom: bool,
    pub line_ending: LineEnding,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DocumentPart {
    pub id: String,
    pub kind: String,
    pub locator: String,
    pub text: String,
    pub char_len: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DocumentGraph {
    pub schema_version: u32,
    pub offset_unit: String,
    pub source: SourceMetadata,
    pub parts: Vec<DocumentPart>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EntityType {
    PersonName,
    OrgName,
    Address,
    PhoneNumber,
    Email,
    CnIdNumber,
    BankCardNumber,
    FinancialAmount,
    SalaryAmount,
    BusinessMetric,
    ProjectCode,
    ContractId,
    CustomerName,
}

impl EntityType {
    pub const ALL: [Self; 13] = [
        Self::PersonName,
        Self::OrgName,
        Self::Address,
        Self::PhoneNumber,
        Self::Email,
        Self::CnIdNumber,
        Self::BankCardNumber,
        Self::FinancialAmount,
        Self::SalaryAmount,
        Self::BusinessMetric,
        Self::ProjectCode,
        Self::ContractId,
        Self::CustomerName,
    ];

    pub fn placeholder(&self) -> &'static str {
        match self {
            Self::PersonName => "[姓名]",
            Self::OrgName => "[机构]",
            Self::Address => "[地址]",
            Self::PhoneNumber => "[手机号]",
            Self::Email => "[邮箱]",
            Self::CnIdNumber => "[身份证号]",
            Self::BankCardNumber => "[银行卡号]",
            Self::FinancialAmount => "[财务金额]",
            Self::SalaryAmount => "[薪酬金额]",
            Self::BusinessMetric => "[业务指标]",
            Self::ProjectCode => "[项目代号]",
            Self::ContractId => "[合同编号]",
            Self::CustomerName => "[客户名称]",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskDiagnostic {
    pub severity: DiagnosticSeverity,
    pub code: String,
    pub detector_id: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Finding {
    pub id: String,
    pub part_id: String,
    pub start: usize,
    pub end: usize,
    pub entity_type: EntityType,
    pub matched_text: String,
    pub detector: String,
    pub confidence: f32,
    pub explanation_code: String,
    pub selected: bool,
    #[serde(default)]
    pub reviewed: bool,
    pub replacement: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskDraft {
    pub schema_version: u32,
    pub task_id: String,
    pub policy_id: String,
    #[serde(default)]
    pub policy: PolicyConfig,
    pub document: DocumentGraph,
    pub findings: Vec<Finding>,
    #[serde(default)]
    pub diagnostics: Vec<TaskDiagnostic>,
    pub contains_sensitive_plaintext: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResidualFinding {
    pub start: usize,
    pub end: usize,
    pub entity_type: EntityType,
    pub detector: String,
    pub explanation_code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerificationReport {
    pub schema_version: u32,
    pub passed: bool,
    pub complete: bool,
    pub checked_file: String,
    pub sha256: String,
    pub selected_findings: usize,
    pub unreviewed_findings: usize,
    pub target_residual_count: usize,
    pub residual_findings: Vec<ResidualFinding>,
    pub detectors_checked: Vec<String>,
    pub diagnostics: Vec<TaskDiagnostic>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImageFileKind {
    Png,
    Jpeg,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ImageRect {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImageSourceMetadata {
    pub path: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub file_kind: ImageFileKind,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImageFinding {
    pub id: String,
    /// 同一逻辑命中跨越多条 OCR 行时，各矩形共享同一个 group_id。
    pub group_id: String,
    pub line_index: usize,
    /// 在 OCR 文本行中的 Unicode scalar 起止位置。
    pub text_start: usize,
    pub text_end: usize,
    pub entity_type: EntityType,
    /// 完整逻辑命中；跨行时可能包含换行符。
    pub matched_text: String,
    /// 当前遮罩矩形对应的本行片段。
    pub line_fragment: String,
    pub recognized_line: String,
    pub detector: String,
    pub confidence: f32,
    pub ocr_confidence: f32,
    pub explanation_code: String,
    pub ocr_rect: ImageRect,
    /// 导出时实际覆盖的矩形。用户可在任务文件或后续复核界面中修改。
    pub mask_rect: ImageRect,
    pub selected: bool,
    #[serde(default)]
    pub reviewed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImageTaskDraft {
    pub schema_version: u32,
    pub task_id: String,
    pub policy_id: String,
    pub policy: PolicyConfig,
    pub source: ImageSourceMetadata,
    pub ocr_runtime_id: String,
    pub findings: Vec<ImageFinding>,
    #[serde(default)]
    pub diagnostics: Vec<TaskDiagnostic>,
    pub contains_sensitive_plaintext: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImageResidualFinding {
    pub line_index: usize,
    pub entity_type: EntityType,
    pub detector: String,
    pub explanation_code: String,
    pub ocr_rect: ImageRect,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImageVerificationReport {
    pub schema_version: u32,
    pub passed: bool,
    pub complete: bool,
    pub checked_file: String,
    pub sha256: String,
    pub selected_findings: usize,
    pub unreviewed_findings: usize,
    pub target_residual_count: usize,
    pub residual_findings: Vec<ImageResidualFinding>,
    pub detectors_checked: Vec<String>,
    pub diagnostics: Vec<TaskDiagnostic>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DocxSourceMetadata {
    pub path: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub package_entries: usize,
    pub embedded_images: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DocxDocumentGraph {
    pub schema_version: u32,
    pub offset_unit: String,
    pub source: DocxSourceMetadata,
    pub parts: Vec<DocumentPart>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DocxEmbeddedImageTask {
    /// OOXML package path, for example `word/media/image1.png`.
    pub entry_name: String,
    pub task: ImageTaskDraft,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DocxTaskDraft {
    pub schema_version: u32,
    pub task_id: String,
    pub policy_id: String,
    pub policy: PolicyConfig,
    pub document: DocxDocumentGraph,
    pub findings: Vec<Finding>,
    /// Embedded PNG/JPEG images are independent editable image subtasks.
    #[serde(default)]
    pub embedded_images: Vec<DocxEmbeddedImageTask>,
    #[serde(default)]
    pub diagnostics: Vec<TaskDiagnostic>,
    pub contains_sensitive_plaintext: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DocxResidualFinding {
    pub part_id: String,
    pub start: usize,
    pub end: usize,
    pub entity_type: EntityType,
    pub detector: String,
    pub explanation_code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DocxImageResidualFinding {
    pub entry_name: String,
    pub line_index: usize,
    pub entity_type: EntityType,
    pub detector: String,
    pub explanation_code: String,
    pub ocr_rect: ImageRect,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DocxVerificationReport {
    pub schema_version: u32,
    pub passed: bool,
    pub complete: bool,
    pub checked_file: String,
    pub sha256: String,
    pub selected_findings: usize,
    pub unreviewed_findings: usize,
    pub target_residual_count: usize,
    pub residual_findings: Vec<DocxResidualFinding>,
    #[serde(default)]
    pub image_residual_findings: Vec<DocxImageResidualFinding>,
    pub detectors_checked: Vec<String>,
    pub package_entries_checked: usize,
    pub metadata_scrubbed: bool,
    pub embedded_images_checked: usize,
    pub diagnostics: Vec<TaskDiagnostic>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct XlsxSourceMetadata {
    pub path: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub package_entries: usize,
    pub worksheets: usize,
    pub embedded_images: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct XlsxDocumentGraph {
    pub schema_version: u32,
    pub offset_unit: String,
    pub source: XlsxSourceMetadata,
    pub parts: Vec<DocumentPart>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct XlsxEmbeddedImageTask {
    /// OOXML package path, for example `xl/media/image1.png`.
    pub entry_name: String,
    pub task: ImageTaskDraft,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct XlsxTaskDraft {
    pub schema_version: u32,
    pub task_id: String,
    pub policy_id: String,
    pub policy: PolicyConfig,
    pub document: XlsxDocumentGraph,
    pub findings: Vec<Finding>,
    #[serde(default)]
    pub embedded_images: Vec<XlsxEmbeddedImageTask>,
    #[serde(default)]
    pub diagnostics: Vec<TaskDiagnostic>,
    pub contains_sensitive_plaintext: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct XlsxResidualFinding {
    pub part_id: String,
    pub start: usize,
    pub end: usize,
    pub entity_type: EntityType,
    pub detector: String,
    pub explanation_code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct XlsxImageResidualFinding {
    pub entry_name: String,
    pub line_index: usize,
    pub entity_type: EntityType,
    pub detector: String,
    pub explanation_code: String,
    pub ocr_rect: ImageRect,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct XlsxVerificationReport {
    pub schema_version: u32,
    pub passed: bool,
    pub complete: bool,
    pub checked_file: String,
    pub sha256: String,
    pub selected_findings: usize,
    pub unreviewed_findings: usize,
    pub target_residual_count: usize,
    pub residual_findings: Vec<XlsxResidualFinding>,
    pub image_residual_findings: Vec<XlsxImageResidualFinding>,
    pub detectors_checked: Vec<String>,
    pub package_entries_checked: usize,
    pub metadata_scrubbed: bool,
    pub embedded_images_checked: usize,
    pub diagnostics: Vec<TaskDiagnostic>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PptxSourceMetadata {
    pub path: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub package_entries: usize,
    pub slides: usize,
    pub embedded_images: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PptxDocumentGraph {
    pub schema_version: u32,
    pub offset_unit: String,
    pub source: PptxSourceMetadata,
    pub parts: Vec<DocumentPart>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PptxEmbeddedImageTask {
    /// OOXML package path, for example `ppt/media/image1.png`.
    pub entry_name: String,
    pub task: ImageTaskDraft,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PptxTaskDraft {
    pub schema_version: u32,
    pub task_id: String,
    pub policy_id: String,
    pub policy: PolicyConfig,
    pub document: PptxDocumentGraph,
    pub findings: Vec<Finding>,
    #[serde(default)]
    pub embedded_images: Vec<PptxEmbeddedImageTask>,
    #[serde(default)]
    pub diagnostics: Vec<TaskDiagnostic>,
    pub contains_sensitive_plaintext: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PptxResidualFinding {
    pub part_id: String,
    pub start: usize,
    pub end: usize,
    pub entity_type: EntityType,
    pub detector: String,
    pub explanation_code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PptxImageResidualFinding {
    pub entry_name: String,
    pub line_index: usize,
    pub entity_type: EntityType,
    pub detector: String,
    pub explanation_code: String,
    pub ocr_rect: ImageRect,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PptxVerificationReport {
    pub schema_version: u32,
    pub passed: bool,
    pub complete: bool,
    pub checked_file: String,
    pub sha256: String,
    pub selected_findings: usize,
    pub unreviewed_findings: usize,
    pub target_residual_count: usize,
    pub residual_findings: Vec<PptxResidualFinding>,
    pub image_residual_findings: Vec<PptxImageResidualFinding>,
    pub detectors_checked: Vec<String>,
    pub package_entries_checked: usize,
    pub metadata_scrubbed: bool,
    pub embedded_images_checked: usize,
    pub diagnostics: Vec<TaskDiagnostic>,
}
