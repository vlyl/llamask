use std::collections::{BTreeMap, HashSet};
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use llamask_core::model::{DocumentPart, EntityType, Finding, ImageFinding};
use llamask_core::sidecar::DetectorKind;
use llamask_core::{
    DocxTaskDraft, DocxWorkflowError, ImageRect, ImageTaskDraft, ImageWorkflowError, PdfTaskDraft,
    PdfWorkflowError, PolicyConfig, PptxTaskDraft, PptxWorkflowError, RuntimeRegistry, TaskDraft,
    WorkflowError, XlsxTaskDraft, XlsxWorkflowError, add_manual_image_mask,
    export_docx_task_with_runtimes, export_image_task_with_runtimes, export_pdf_task_with_runtimes,
    export_pptx_task_with_runtimes, export_task_with_runtimes, export_xlsx_task_with_runtimes,
    remove_manual_image_group, render_docx_embedded_image_preview, render_image_task_preview,
    render_pdf_task_page_preview, render_pptx_embedded_image_preview, render_task_with_runtimes,
    render_xlsx_embedded_image_preview, review_docx_finding, review_image_group,
    review_pptx_finding, review_text_finding, review_xlsx_finding,
    scan_docx_with_policy_and_images, scan_image_with_policy, scan_path_with_policy,
    scan_pdf_with_policy_and_progress, scan_pptx_with_policy_and_images, scan_text_with_policy,
    scan_xlsx_with_policy, update_image_mask,
};
use serde::Serialize;
use tauri::{AppHandle, DragDropEvent, Emitter, Manager, State, WindowEvent};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_dialog::DialogExt;

const MAX_IMPORT_FILES: usize = 200;
const MAX_TEXT_BYTES: u64 = 2 * 1024 * 1024;
const MAX_IMAGE_BYTES: u64 = 50 * 1024 * 1024;
const MAX_DOCUMENT_BYTES: u64 = 100 * 1024 * 1024;
const SUPPORTED_EXTENSIONS: &[&str] = &[
    "txt", "md", "docx", "xlsx", "pptx", "pdf", "png", "jpg", "jpeg",
];
const FILES_IMPORTED_EVENT: &str = "desktop-files-imported";
const SCAN_PROGRESS_EVENT: &str = "desktop-scan-progress";
const BATCH_EXPORT_COMPLETE_EVENT: &str = "desktop-batch-export-complete";
const RUNTIME_REGISTRY_ENV: &str = "LLAMASK_RUNTIME_REGISTRY";
const TEXT_REVIEW_CONTEXT_CHARS: usize = 80;

#[derive(Default)]
struct DesktopState {
    next_id: AtomicU64,
    files: Mutex<BTreeMap<String, RegisteredFile>>,
    scans: Mutex<BTreeMap<String, ScanRecord>>,
    runtime: Mutex<Option<Arc<RuntimeContext>>>,
    worker_running: AtomicBool,
}

#[derive(Debug, Clone)]
struct RegisteredFile {
    source: RegisteredSource,
    kind: DesktopFileKind,
    ready: bool,
    scan_supported: bool,
}

#[derive(Debug, Clone)]
enum RegisteredSource {
    File(PathBuf),
    Clipboard(String),
}

struct RuntimeContext {
    registry: RuntimeRegistry,
    ocr_runtime_id: String,
    pdf_tools_ready: bool,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum DesktopFileKind {
    Text,
    Word,
    Spreadsheet,
    Presentation,
    Pdf,
    Image,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum DesktopSourceKind {
    File,
    Clipboard,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum DesktopOutputKind {
    File,
    Clipboard,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct ImportedFile {
    id: String,
    display_name: String,
    extension: String,
    kind: DesktopFileKind,
    source_kind: DesktopSourceKind,
    size_bytes: u64,
    ready: bool,
    scan_supported: bool,
    reason_code: String,
    duplicate: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopCapabilities {
    app_version: String,
    core_version: String,
    default_policy_id: String,
    offline_only: bool,
    max_import_files: usize,
    supported_extensions: Vec<&'static str>,
    scan_extensions: Vec<&'static str>,
    milestone: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopCommandError {
    code: &'static str,
    message: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopImportEvent {
    files: Vec<ImportedFile>,
    error: Option<DesktopCommandError>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct DesktopRuntimeStatus {
    policy_ready: bool,
    runtime_registry_ready: bool,
    ocr_ready: bool,
    ocr_runtime_id: Option<String>,
    pdf_tools_ready: bool,
    scan_ready: bool,
    status_code: String,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum DesktopScanStatus {
    Queued,
    Scanning,
    Cancelling,
    ReviewRequired,
    ReadyToExport,
    Exporting,
    Complete,
    ExportFailed,
    Blocked,
    Cancelled,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum DesktopScanStage {
    Queued,
    Preflight,
    Ocr,
    ScanningPages,
    DetectingText,
    Complete,
    Exporting,
    Failed,
    Cancelling,
    Blocked,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct DesktopScanSummary {
    id: String,
    status: DesktopScanStatus,
    stage: DesktopScanStage,
    completed_units: usize,
    total_units: usize,
    finding_groups: usize,
    unreviewed_groups: usize,
    page_count: usize,
    diagnostic_count: usize,
    can_cancel: bool,
    error_code: Option<String>,
    output_name: Option<String>,
    output_kind: Option<DesktopOutputKind>,
    verification_complete: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct DesktopReviewFinding {
    finding_id: String,
    group_id: String,
    entity_type: String,
    confidence: f32,
    ocr_confidence: f32,
    mask_rect: ImageRect,
    selected: bool,
    reviewed: bool,
    manual: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct DesktopReviewPage {
    id: String,
    page_number: usize,
    page_count: usize,
    width: u32,
    height: u32,
    image_data_url: String,
    findings: Vec<DesktopReviewFinding>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct DesktopReviewMutation {
    summary: DesktopScanSummary,
    findings: Vec<DesktopReviewFinding>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct DesktopTextReviewFinding {
    finding_id: String,
    entity_type: String,
    confidence: f32,
    section_label: Option<String>,
    can_apply: bool,
    review_note: Option<String>,
    context_before: String,
    matched_text: String,
    context_after: String,
    replacement: String,
    selected: bool,
    reviewed: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct DesktopTextReview {
    id: String,
    total_characters: usize,
    embedded_image_count: usize,
    unreviewed_image_groups: usize,
    findings: Vec<DesktopTextReviewFinding>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct DesktopTextReviewMutation {
    summary: DesktopScanSummary,
    findings: Vec<DesktopTextReviewFinding>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct DesktopBatchExportStart {
    started: bool,
    attempted: usize,
    skipped: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct DesktopBatchExportSummary {
    attempted: usize,
    succeeded: usize,
    failed: usize,
    skipped: usize,
    complete_verifications: usize,
    basic_verifications: usize,
}

struct ScanCandidate {
    id: String,
    source: RegisteredSource,
    kind: DesktopFileKind,
    cancel_requested: Arc<AtomicBool>,
}

struct BatchExportCandidate {
    id: String,
    task: StoredScanTask,
    output: PathBuf,
    output_name: String,
}

#[derive(Clone)]
enum StoredScanTask {
    Text(Box<TaskDraft>),
    Image(Box<ImageTaskDraft>),
    Pdf(Box<PdfTaskDraft>),
    Docx(Box<DocxTaskDraft>),
    Xlsx(Box<XlsxTaskDraft>),
    Pptx(Box<PptxTaskDraft>),
}

#[derive(Debug, Clone, Copy, Default)]
struct ScanMetrics {
    finding_groups: usize,
    unreviewed_groups: usize,
    page_count: usize,
    diagnostic_count: usize,
}

struct ScanRecord {
    status: DesktopScanStatus,
    stage: DesktopScanStage,
    completed_units: usize,
    total_units: usize,
    cancel_requested: Arc<AtomicBool>,
    task: Option<StoredScanTask>,
    error_code: Option<&'static str>,
    output_name: Option<String>,
    output_kind: Option<DesktopOutputKind>,
    verification_complete: bool,
}

struct RuntimeLoadFailure {
    code: &'static str,
}

impl DesktopCommandError {
    fn new(code: &'static str, message: &'static str) -> Self {
        Self { code, message }
    }
}

impl StoredScanTask {
    fn metrics(&self) -> ScanMetrics {
        match self {
            Self::Text(task) => text_task_metrics(task),
            Self::Image(task) => image_task_metrics(task),
            Self::Pdf(task) => pdf_task_metrics(task),
            Self::Docx(task) => docx_task_metrics(task),
            Self::Xlsx(task) => xlsx_task_metrics(task),
            Self::Pptx(task) => pptx_task_metrics(task),
        }
    }

    fn page_count(&self) -> usize {
        match self {
            Self::Text(_) => 1,
            Self::Image(_) => 1,
            Self::Pdf(task) => task.pages.len(),
            Self::Docx(task) => task.embedded_images.len(),
            Self::Xlsx(task) => task.embedded_images.len(),
            Self::Pptx(task) => task.embedded_images.len(),
        }
    }

    fn page_task(&self, page_number: usize) -> Option<&ImageTaskDraft> {
        match self {
            Self::Text(_) => None,
            Self::Image(task) if page_number == 1 => Some(task),
            Self::Pdf(task) => task
                .pages
                .iter()
                .find(|page| page.page_number == page_number)
                .map(|page| &page.task),
            Self::Docx(task) => page_number
                .checked_sub(1)
                .and_then(|index| task.embedded_images.get(index))
                .map(|embedded| &embedded.task),
            Self::Xlsx(task) => page_number
                .checked_sub(1)
                .and_then(|index| task.embedded_images.get(index))
                .map(|embedded| &embedded.task),
            Self::Pptx(task) => page_number
                .checked_sub(1)
                .and_then(|index| task.embedded_images.get(index))
                .map(|embedded| &embedded.task),
            _ => None,
        }
    }

    fn page_task_mut(&mut self, page_number: usize) -> Option<&mut ImageTaskDraft> {
        match self {
            Self::Text(_) => None,
            Self::Image(task) if page_number == 1 => Some(task),
            Self::Pdf(task) => task
                .pages
                .iter_mut()
                .find(|page| page.page_number == page_number)
                .map(|page| &mut page.task),
            Self::Docx(task) => page_number
                .checked_sub(1)
                .and_then(|index| task.embedded_images.get_mut(index))
                .map(|embedded| &mut embedded.task),
            Self::Xlsx(task) => page_number
                .checked_sub(1)
                .and_then(|index| task.embedded_images.get_mut(index))
                .map(|embedded| &mut embedded.task),
            Self::Pptx(task) => page_number
                .checked_sub(1)
                .and_then(|index| task.embedded_images.get_mut(index))
                .map(|embedded| &mut embedded.task),
            _ => None,
        }
    }
}

impl ScanRecord {
    fn queued(cancel_requested: Arc<AtomicBool>) -> Self {
        Self {
            status: DesktopScanStatus::Queued,
            stage: DesktopScanStage::Queued,
            completed_units: 0,
            total_units: 0,
            cancel_requested,
            task: None,
            error_code: None,
            output_name: None,
            output_kind: None,
            verification_complete: false,
        }
    }

    fn summary(&self, id: &str) -> DesktopScanSummary {
        let metrics = self
            .task
            .as_ref()
            .map(StoredScanTask::metrics)
            .unwrap_or_default();
        DesktopScanSummary {
            id: id.to_owned(),
            status: self.status,
            stage: self.stage,
            completed_units: self.completed_units,
            total_units: self.total_units,
            finding_groups: metrics.finding_groups,
            unreviewed_groups: metrics.unreviewed_groups,
            page_count: metrics.page_count,
            diagnostic_count: metrics.diagnostic_count,
            can_cancel: matches!(
                self.status,
                DesktopScanStatus::Queued
                    | DesktopScanStatus::Scanning
                    | DesktopScanStatus::Cancelling
            ),
            error_code: self.error_code.map(str::to_owned),
            output_name: self.output_name.clone(),
            output_kind: self.output_kind,
            verification_complete: self.verification_complete,
        }
    }

    fn finish(&mut self, task: StoredScanTask) {
        let metrics = task.metrics();
        self.status = if metrics.unreviewed_groups > 0 {
            DesktopScanStatus::ReviewRequired
        } else {
            DesktopScanStatus::ReadyToExport
        };
        self.stage = DesktopScanStage::Complete;
        self.completed_units = metrics.page_count.max(1);
        self.total_units = metrics.page_count.max(1);
        self.task = Some(task);
        self.error_code = None;
        self.output_name = None;
        self.output_kind = None;
        self.verification_complete = false;
    }

    fn refresh_after_review(&mut self) {
        let Some(task) = self.task.as_ref() else {
            return;
        };
        let metrics = task.metrics();
        self.status = if metrics.unreviewed_groups > 0 {
            DesktopScanStatus::ReviewRequired
        } else {
            DesktopScanStatus::ReadyToExport
        };
        self.stage = DesktopScanStage::Complete;
        self.error_code = None;
        self.output_name = None;
        self.output_kind = None;
        self.verification_complete = false;
    }

    fn begin_export(&mut self) {
        self.status = DesktopScanStatus::Exporting;
        self.stage = DesktopScanStage::Exporting;
        self.error_code = None;
        self.output_name = None;
        self.output_kind = None;
        self.verification_complete = false;
    }

    fn finish_export(&mut self, output_name: String, verification_complete: bool) {
        self.status = DesktopScanStatus::Complete;
        self.stage = DesktopScanStage::Complete;
        self.error_code = None;
        self.output_name = Some(output_name);
        self.output_kind = Some(DesktopOutputKind::File);
        self.verification_complete = verification_complete;
    }

    fn finish_clipboard_export(&mut self, verification_complete: bool) {
        self.status = DesktopScanStatus::Complete;
        self.stage = DesktopScanStage::Complete;
        self.error_code = None;
        self.output_name = None;
        self.output_kind = Some(DesktopOutputKind::Clipboard);
        self.verification_complete = verification_complete;
    }

    fn fail_export(&mut self, code: &'static str) {
        self.status = DesktopScanStatus::ExportFailed;
        self.stage = DesktopScanStage::Failed;
        self.error_code = Some(code);
        self.output_name = None;
        self.output_kind = None;
        self.verification_complete = false;
    }

    fn cancel(&mut self) {
        self.status = DesktopScanStatus::Cancelled;
        self.stage = DesktopScanStage::Cancelled;
        self.task = None;
        self.error_code = None;
        self.output_name = None;
        self.output_kind = None;
        self.verification_complete = false;
    }

    fn block(&mut self, code: &'static str) {
        self.status = DesktopScanStatus::Blocked;
        self.stage = DesktopScanStage::Blocked;
        self.task = None;
        self.error_code = Some(code);
        self.output_name = None;
        self.output_kind = None;
        self.verification_complete = false;
    }
}

#[tauri::command]
fn desktop_capabilities() -> DesktopCapabilities {
    DesktopCapabilities {
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        core_version: env!("CARGO_PKG_VERSION").to_owned(),
        default_policy_id: PolicyConfig::default().id,
        offline_only: true,
        max_import_files: MAX_IMPORT_FILES,
        supported_extensions: SUPPORTED_EXTENSIONS.to_vec(),
        scan_extensions: vec![
            "txt", "md", "docx", "xlsx", "pptx", "pdf", "png", "jpg", "jpeg",
        ],
        milestone: "batch-pptx-xlsx-docx-clipboard-text-image-pdf-review-export",
    }
}

#[tauri::command]
fn desktop_runtime_status(app: AppHandle, state: State<'_, DesktopState>) -> DesktopRuntimeStatus {
    runtime_status(&app, state.inner())
}

#[tauri::command]
async fn pick_files(
    app: AppHandle,
    state: State<'_, DesktopState>,
) -> Result<Vec<ImportedFile>, DesktopCommandError> {
    let selected = app
        .dialog()
        .file()
        .set_title("选择需要脱敏的文件")
        .add_filter("LlaMask 支持的文件", SUPPORTED_EXTENSIONS)
        .blocking_pick_files()
        .unwrap_or_default();
    let paths = selected
        .into_iter()
        .map(|path| {
            path.into_path().map_err(|_| {
                DesktopCommandError::new("FILE_PATH_INVALID", "所选文件路径无法安全读取。")
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    register_files(state.inner(), paths)
}

#[tauri::command]
fn import_clipboard_text(
    app: AppHandle,
    state: State<'_, DesktopState>,
) -> Result<ImportedFile, DesktopCommandError> {
    let text = app.clipboard().read_text().map_err(|_| {
        DesktopCommandError::new("CLIPBOARD_READ_FAILED", "无法读取系统剪贴板中的纯文本。")
    })?;
    register_clipboard_text(state.inner(), text)
}

fn register_clipboard_text(
    state: &DesktopState,
    text: String,
) -> Result<ImportedFile, DesktopCommandError> {
    if text.trim().is_empty() {
        return Err(DesktopCommandError::new(
            "CLIPBOARD_EMPTY",
            "剪贴板中没有可处理的纯文本。",
        ));
    }
    let size_bytes = text.len() as u64;
    if size_bytes > MAX_TEXT_BYTES {
        return Err(DesktopCommandError::new(
            "CLIPBOARD_TOO_LARGE",
            "剪贴板文本超过 2 MiB 安全处理上限。",
        ));
    }

    let mut registry = state.files.lock().map_err(|_| task_state_error())?;
    if let Some((existing_id, _)) = registry.iter().find(|(_, registered)| {
        matches!(&registered.source, RegisteredSource::Clipboard(existing) if existing == &text)
    }) {
        return Ok(clipboard_imported_file(
            existing_id.clone(),
            size_bytes,
            true,
        ));
    }
    if registry.len() >= MAX_IMPORT_FILES {
        return Err(DesktopCommandError::new(
            "TOO_MANY_FILES",
            "当前任务最多包含 200 个文件或剪贴板文本。",
        ));
    }
    let id = format!(
        "clipboard-{:08}",
        state.next_id.fetch_add(1, Ordering::Relaxed) + 1
    );
    registry.insert(
        id.clone(),
        RegisteredFile {
            source: RegisteredSource::Clipboard(text),
            kind: DesktopFileKind::Text,
            ready: true,
            scan_supported: true,
        },
    );
    Ok(clipboard_imported_file(id, size_bytes, false))
}

fn register_files(
    state: &DesktopState,
    paths: Vec<PathBuf>,
) -> Result<Vec<ImportedFile>, DesktopCommandError> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    if paths.len() > MAX_IMPORT_FILES {
        return Err(DesktopCommandError::new(
            "TOO_MANY_FILES",
            "一次最多导入 200 个文件。",
        ));
    }

    let mut registry = state.files.lock().map_err(|_| {
        DesktopCommandError::new("TASK_STATE_UNAVAILABLE", "本地任务状态暂时不可用。")
    })?;
    if registry.len().saturating_add(paths.len()) > MAX_IMPORT_FILES {
        return Err(DesktopCommandError::new(
            "TOO_MANY_FILES",
            "当前任务最多包含 200 个文件或剪贴板文本。",
        ));
    }
    let mut imported = Vec::with_capacity(paths.len());
    for original in paths {
        let id = format!(
            "file-{:08}",
            state.next_id.fetch_add(1, Ordering::Relaxed) + 1
        );
        let display_name = display_name(&original);
        let canonical = match fs::canonicalize(&original) {
            Ok(path) => path,
            Err(_) => {
                imported.push(unreadable_file(id, display_name, &original));
                continue;
            }
        };
        let metadata = match fs::metadata(&canonical) {
            Ok(metadata) if metadata.is_file() => metadata,
            _ => {
                imported.push(unreadable_file(id, display_name, &original));
                continue;
            }
        };
        if let Some((existing_id, _)) = registry
            .iter()
            .find(|(_, registered)| {
                matches!(&registered.source, RegisteredSource::File(path) if path == &canonical)
            })
        {
            let mut candidate = inspect_file(
                existing_id.clone(),
                display_name,
                &canonical,
                metadata.len(),
            );
            candidate.duplicate = true;
            candidate.reason_code = "ALREADY_IMPORTED".to_owned();
            imported.push(candidate);
            continue;
        }
        let candidate = inspect_file(id.clone(), display_name, &canonical, metadata.len());
        registry.insert(
            id,
            RegisteredFile {
                source: RegisteredSource::File(canonical),
                kind: candidate.kind,
                ready: candidate.ready,
                scan_supported: candidate.scan_supported,
            },
        );
        imported.push(candidate);
    }
    Ok(imported)
}

#[tauri::command]
fn remove_registered_file(
    state: State<'_, DesktopState>,
    id: String,
) -> Result<bool, DesktopCommandError> {
    let mut scans = state.scans.lock().map_err(|_| task_state_error())?;
    if scans
        .get(&id)
        .is_some_and(|record| record.status == DesktopScanStatus::Exporting)
    {
        return Err(DesktopCommandError::new(
            "EXPORT_ACTIVE",
            "安全导出完成前不能移除该文件。",
        ));
    }
    if let Some(record) = scans.remove(&id) {
        record.cancel_requested.store(true, Ordering::Release);
    }
    drop(scans);
    let mut registry = state.files.lock().map_err(|_| {
        DesktopCommandError::new("TASK_STATE_UNAVAILABLE", "本地任务状态暂时不可用。")
    })?;
    Ok(registry.remove(&id).is_some())
}

#[tauri::command]
fn clear_registered_files(state: State<'_, DesktopState>) -> Result<usize, DesktopCommandError> {
    let mut scans = state.scans.lock().map_err(|_| {
        DesktopCommandError::new("TASK_STATE_UNAVAILABLE", "本地任务状态暂时不可用。")
    })?;
    if scans
        .values()
        .any(|record| record.status == DesktopScanStatus::Exporting)
    {
        return Err(DesktopCommandError::new(
            "EXPORT_ACTIVE",
            "安全导出完成前不能清空当前任务。",
        ));
    }
    for record in scans.values() {
        record.cancel_requested.store(true, Ordering::Release);
    }
    scans.clear();
    drop(scans);

    let mut registry = state.files.lock().map_err(|_| {
        DesktopCommandError::new("TASK_STATE_UNAVAILABLE", "本地任务状态暂时不可用。")
    })?;
    let count = registry.len();
    registry.clear();
    Ok(count)
}

#[tauri::command]
fn scan_task_summaries(
    state: State<'_, DesktopState>,
) -> Result<Vec<DesktopScanSummary>, DesktopCommandError> {
    let scans = state.scans.lock().map_err(|_| {
        DesktopCommandError::new("TASK_STATE_UNAVAILABLE", "本地任务状态暂时不可用。")
    })?;
    Ok(scans
        .iter()
        .map(|(id, record)| record.summary(id))
        .collect())
}

#[tauri::command]
fn start_registered_scans(
    app: AppHandle,
    state: State<'_, DesktopState>,
) -> Result<Vec<DesktopScanSummary>, DesktopCommandError> {
    if state.worker_running.swap(true, Ordering::AcqRel) {
        return Err(DesktopCommandError::new(
            "LOCAL_WORK_ACTIVE",
            "当前扫描或安全导出任务尚未结束。",
        ));
    }
    let files = state.files.lock().map_err(|_| {
        state.worker_running.store(false, Ordering::Release);
        DesktopCommandError::new("TASK_STATE_UNAVAILABLE", "本地任务状态暂时不可用。")
    })?;
    let candidate_files = files
        .iter()
        .filter(|(_, file)| file.ready && file.scan_supported)
        .map(|(id, file)| (id.clone(), file.source.clone(), file.kind))
        .collect::<Vec<_>>();
    drop(files);

    let mut scans = state.scans.lock().map_err(|_| {
        state.worker_running.store(false, Ordering::Release);
        DesktopCommandError::new("TASK_STATE_UNAVAILABLE", "本地任务状态暂时不可用。")
    })?;
    let mut candidates = Vec::new();
    for (id, source, kind) in candidate_files {
        if scans.get(&id).is_some_and(|record| {
            !matches!(
                record.status,
                DesktopScanStatus::Blocked | DesktopScanStatus::Cancelled
            )
        }) {
            continue;
        }
        let cancel_requested = Arc::new(AtomicBool::new(false));
        scans.insert(id.clone(), ScanRecord::queued(cancel_requested.clone()));
        candidates.push(ScanCandidate {
            id,
            source,
            kind,
            cancel_requested,
        });
    }
    let summaries = candidates
        .iter()
        .filter_map(|candidate| {
            scans
                .get(&candidate.id)
                .map(|record| record.summary(&candidate.id))
        })
        .collect::<Vec<_>>();
    drop(scans);

    if candidates.is_empty() {
        state.worker_running.store(false, Ordering::Release);
        return Err(DesktopCommandError::new(
            "NO_SCANNABLE_FILES",
            "请先导入剪贴板文本、TXT、Markdown、DOCX、XLSX、PPTX、PDF、PNG 或 JPEG。",
        ));
    }

    let batch_app = app.clone();
    tauri::async_runtime::spawn(async move {
        let worker_app = batch_app.clone();
        let result =
            tauri::async_runtime::spawn_blocking(move || run_scan_batch(worker_app, candidates))
                .await;
        if result.is_err() {
            block_active_scans(&batch_app, "SCAN_WORKER_FAILED");
        }
        batch_app
            .state::<DesktopState>()
            .worker_running
            .store(false, Ordering::Release);
    });

    Ok(summaries)
}

#[tauri::command]
fn cancel_scan(
    app: AppHandle,
    state: State<'_, DesktopState>,
    id: String,
) -> Result<bool, DesktopCommandError> {
    let summary = {
        let mut scans = state.scans.lock().map_err(|_| {
            DesktopCommandError::new("TASK_STATE_UNAVAILABLE", "本地任务状态暂时不可用。")
        })?;
        let Some(record) = scans.get_mut(&id) else {
            return Ok(false);
        };
        if !matches!(
            record.status,
            DesktopScanStatus::Queued | DesktopScanStatus::Scanning | DesktopScanStatus::Cancelling
        ) {
            return Ok(false);
        }
        record.cancel_requested.store(true, Ordering::Release);
        if record.status == DesktopScanStatus::Queued {
            record.cancel();
        } else {
            record.status = DesktopScanStatus::Cancelling;
            record.stage = DesktopScanStage::Cancelling;
        }
        record.summary(&id)
    };
    let _ = app.emit(SCAN_PROGRESS_EVENT, summary);
    Ok(true)
}

#[tauri::command]
async fn review_scan_page(
    state: State<'_, DesktopState>,
    id: String,
    page_number: usize,
) -> Result<DesktopReviewPage, DesktopCommandError> {
    let task = {
        let scans = state.scans.lock().map_err(|_| task_state_error())?;
        let record = scans.get(&id).ok_or_else(scan_not_found_error)?;
        if matches!(
            record.status,
            DesktopScanStatus::Queued
                | DesktopScanStatus::Scanning
                | DesktopScanStatus::Cancelling
                | DesktopScanStatus::Exporting
                | DesktopScanStatus::Blocked
                | DesktopScanStatus::Cancelled
        ) {
            return Err(DesktopCommandError::new(
                "REVIEW_NOT_READY",
                "当前文件尚未进入可复核状态。",
            ));
        }
        record.task.clone().ok_or_else(scan_not_found_error)?
    };
    let page_count = task.page_count();
    let preview_task = task.clone();
    let preview = tauri::async_runtime::spawn_blocking(move || match &preview_task {
        StoredScanTask::Image(task) if page_number == 1 => {
            render_image_task_preview(task).map_err(|_| "IMAGE_PREVIEW_FAILED")
        }
        StoredScanTask::Pdf(task) => {
            render_pdf_task_page_preview(task, page_number).map_err(|_| "PDF_PREVIEW_FAILED")
        }
        StoredScanTask::Docx(task) => render_docx_embedded_image_preview(task, page_number)
            .map_err(|_| "DOCX_IMAGE_PREVIEW_FAILED"),
        StoredScanTask::Xlsx(task) => render_xlsx_embedded_image_preview(task, page_number)
            .map_err(|_| "XLSX_IMAGE_PREVIEW_FAILED"),
        StoredScanTask::Pptx(task) => render_pptx_embedded_image_preview(task, page_number)
            .map_err(|_| "PPTX_IMAGE_PREVIEW_FAILED"),
        _ => Err("REVIEW_PAGE_INVALID"),
    })
    .await
    .map_err(|_| DesktopCommandError::new("PREVIEW_WORKER_FAILED", "本地预览任务异常结束。"))?
    .map_err(preview_error)?;
    let page_task = task
        .page_task(page_number)
        .ok_or_else(|| DesktopCommandError::new("REVIEW_PAGE_INVALID", "复核页码无效。"))?;
    Ok(DesktopReviewPage {
        id,
        page_number,
        page_count,
        width: preview.width,
        height: preview.height,
        image_data_url: format!(
            "data:image/png;base64,{}",
            BASE64_STANDARD.encode(preview.png_bytes)
        ),
        findings: review_findings(&page_task.findings),
    })
}

#[tauri::command]
fn set_review_group(
    app: AppHandle,
    state: State<'_, DesktopState>,
    id: String,
    page_number: usize,
    group_id: String,
    selected: bool,
) -> Result<DesktopReviewMutation, DesktopCommandError> {
    apply_review_mutation(&app, state.inner(), &id, page_number, |task| {
        review_image_group(task, &group_id, selected)
    })
}

#[tauri::command]
fn update_review_mask(
    app: AppHandle,
    state: State<'_, DesktopState>,
    id: String,
    page_number: usize,
    finding_id: String,
    mask_rect: ImageRect,
) -> Result<DesktopReviewMutation, DesktopCommandError> {
    apply_review_mutation(&app, state.inner(), &id, page_number, |task| {
        update_image_mask(task, &finding_id, mask_rect)
    })
}

#[tauri::command]
fn add_review_mask(
    app: AppHandle,
    state: State<'_, DesktopState>,
    id: String,
    page_number: usize,
    mask_rect: ImageRect,
) -> Result<DesktopReviewMutation, DesktopCommandError> {
    apply_review_mutation(&app, state.inner(), &id, page_number, |task| {
        add_manual_image_mask(task, mask_rect).map(|_| ())
    })
}

#[tauri::command]
fn remove_review_mask(
    app: AppHandle,
    state: State<'_, DesktopState>,
    id: String,
    page_number: usize,
    group_id: String,
) -> Result<DesktopReviewMutation, DesktopCommandError> {
    apply_review_mutation(&app, state.inner(), &id, page_number, |task| {
        remove_manual_image_group(task, &group_id)
    })
}

#[tauri::command]
fn review_text_scan(
    state: State<'_, DesktopState>,
    id: String,
) -> Result<DesktopTextReview, DesktopCommandError> {
    let review = {
        let scans = state.scans.lock().map_err(|_| task_state_error())?;
        let record = scans.get(&id).ok_or_else(scan_not_found_error)?;
        ensure_review_ready(record)?;
        match record.task.as_ref() {
            Some(StoredScanTask::Text(task)) => DesktopTextReview {
                id,
                total_characters: task.document.parts.iter().map(|part| part.char_len).sum(),
                embedded_image_count: 0,
                unreviewed_image_groups: 0,
                findings: text_review_findings(task)?,
            },
            Some(StoredScanTask::Docx(task)) => DesktopTextReview {
                id,
                total_characters: task.document.parts.iter().map(|part| part.char_len).sum(),
                embedded_image_count: task.embedded_images.len(),
                unreviewed_image_groups: docx_unreviewed_image_groups(task),
                findings: docx_text_review_findings(task)?,
            },
            Some(StoredScanTask::Xlsx(task)) => DesktopTextReview {
                id,
                total_characters: task.document.parts.iter().map(|part| part.char_len).sum(),
                embedded_image_count: task.embedded_images.len(),
                unreviewed_image_groups: xlsx_unreviewed_image_groups(task),
                findings: xlsx_text_review_findings(task)?,
            },
            Some(StoredScanTask::Pptx(task)) => DesktopTextReview {
                id,
                total_characters: task.document.parts.iter().map(|part| part.char_len).sum(),
                embedded_image_count: task.embedded_images.len(),
                unreviewed_image_groups: pptx_unreviewed_image_groups(task),
                findings: pptx_text_review_findings(task)?,
            },
            _ => {
                return Err(DesktopCommandError::new(
                    "TEXT_REVIEW_UNAVAILABLE",
                    "当前文件不支持文本复核。",
                ));
            }
        }
    };
    Ok(review)
}

#[tauri::command]
fn set_text_review_finding(
    app: AppHandle,
    state: State<'_, DesktopState>,
    id: String,
    finding_id: String,
    selected: bool,
    replacement: Option<String>,
) -> Result<DesktopTextReviewMutation, DesktopCommandError> {
    let mutation = {
        let mut scans = state.scans.lock().map_err(|_| task_state_error())?;
        let record = scans.get_mut(&id).ok_or_else(scan_not_found_error)?;
        ensure_review_ready(record)?;
        let findings = match record.task.as_mut() {
            Some(StoredScanTask::Text(task)) => {
                review_text_finding(task, &finding_id, selected, replacement.as_deref())
                    .map_err(text_review_error)?;
                text_review_findings(task)?
            }
            Some(StoredScanTask::Docx(task)) => {
                review_docx_finding(task, &finding_id, selected, replacement.as_deref())
                    .map_err(docx_review_error)?;
                docx_text_review_findings(task)?
            }
            Some(StoredScanTask::Xlsx(task)) => {
                review_xlsx_finding(task, &finding_id, selected, replacement.as_deref())
                    .map_err(xlsx_review_error)?;
                xlsx_text_review_findings(task)?
            }
            Some(StoredScanTask::Pptx(task)) => {
                review_pptx_finding(task, &finding_id, selected, replacement.as_deref())
                    .map_err(pptx_review_error)?;
                pptx_text_review_findings(task)?
            }
            _ => {
                return Err(DesktopCommandError::new(
                    "TEXT_REVIEW_UNAVAILABLE",
                    "当前文件不支持文本复核。",
                ));
            }
        };
        record.refresh_after_review();
        DesktopTextReviewMutation {
            summary: record.summary(&id),
            findings,
        }
    };
    let _ = app.emit(SCAN_PROGRESS_EVENT, mutation.summary.clone());
    Ok(mutation)
}

#[tauri::command]
fn choose_and_export_scan(
    app: AppHandle,
    state: State<'_, DesktopState>,
    id: String,
) -> Result<bool, DesktopCommandError> {
    if state.worker_running.load(Ordering::Acquire) {
        return Err(DesktopCommandError::new(
            "LOCAL_WORK_ACTIVE",
            "当前扫描或安全导出任务尚未结束。",
        ));
    }
    let (default_name, extension, file_kind) = {
        let files = state.files.lock().map_err(|_| task_state_error())?;
        let file = files.get(&id).ok_or_else(scan_not_found_error)?;
        let RegisteredSource::File(path) = &file.source else {
            return Err(DesktopCommandError::new(
                "CLIPBOARD_USE_COPY_ACTION",
                "剪贴板任务请使用“复制脱敏结果”。",
            ));
        };
        let (name, extension) = export_name_and_extension(path)?;
        (name, extension, file.kind)
    };
    let extensions = [extension.as_str()];
    let selected = app
        .dialog()
        .file()
        .set_title("保存脱敏副本")
        .set_file_name(default_name)
        .add_filter("脱敏副本", &extensions)
        .blocking_save_file();
    let Some(selected) = selected else {
        return Ok(false);
    };
    let output = selected
        .into_path()
        .map_err(|_| DesktopCommandError::new("OUTPUT_PATH_INVALID", "输出路径无法安全使用。"))?;
    if state.worker_running.swap(true, Ordering::AcqRel) {
        return Err(DesktopCommandError::new(
            "LOCAL_WORK_ACTIVE",
            "当前扫描或安全导出任务尚未结束。",
        ));
    }
    let context = match runtime_context(&app, state.inner()) {
        Ok(context) => Some(context),
        Err(_)
            if matches!(
                file_kind,
                DesktopFileKind::Text
                    | DesktopFileKind::Word
                    | DesktopFileKind::Spreadsheet
                    | DesktopFileKind::Presentation
            ) =>
        {
            None
        }
        Err(failure) => {
            state.worker_running.store(false, Ordering::Release);
            return Err(DesktopCommandError::new(
                failure.code,
                "本地运行环境尚未通过完整性检查。",
            ));
        }
    };
    let task = {
        let mut scans = state.scans.lock().map_err(|_| {
            state.worker_running.store(false, Ordering::Release);
            task_state_error()
        })?;
        let record = scans.get_mut(&id).ok_or_else(|| {
            state.worker_running.store(false, Ordering::Release);
            scan_not_found_error()
        })?;
        if !matches!(
            record.status,
            DesktopScanStatus::ReadyToExport
                | DesktopScanStatus::ExportFailed
                | DesktopScanStatus::Complete
        ) {
            state.worker_running.store(false, Ordering::Release);
            return Err(DesktopCommandError::new(
                "EXPORT_NOT_READY",
                "请先完成所有待复核结果。",
            ));
        }
        let task = record.task.clone().ok_or_else(|| {
            state.worker_running.store(false, Ordering::Release);
            scan_not_found_error()
        })?;
        record.begin_export();
        task
    };
    emit_scan_summary(&app, &id);

    let export_app = app.clone();
    let export_id = id.clone();
    tauri::async_runtime::spawn(async move {
        let worker_app = export_app.clone();
        let output_name = output
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("redacted-output")
            .to_owned();
        let result = tauri::async_runtime::spawn_blocking(move || {
            export_stored_task(&task, &output, context.as_deref(), None)
        })
        .await;
        match result {
            Ok(Ok(verification_complete)) => update_scan(&worker_app, &export_id, |record| {
                record.finish_export(output_name, verification_complete)
            }),
            Ok(Err(code)) => {
                update_scan(&worker_app, &export_id, |record| record.fail_export(code))
            }
            Err(_) => update_scan(&worker_app, &export_id, |record| {
                record.fail_export("EXPORT_WORKER_FAILED")
            }),
        }
        worker_app
            .state::<DesktopState>()
            .worker_running
            .store(false, Ordering::Release);
    });
    Ok(true)
}

#[tauri::command]
fn choose_and_export_batch(
    app: AppHandle,
    state: State<'_, DesktopState>,
) -> Result<DesktopBatchExportStart, DesktopCommandError> {
    let selected = app
        .dialog()
        .file()
        .set_title("选择批量脱敏副本目录")
        .blocking_pick_folder();
    let Some(selected) = selected else {
        return Ok(DesktopBatchExportStart {
            started: false,
            attempted: 0,
            skipped: 0,
        });
    };
    let directory = selected
        .into_path()
        .map_err(|_| DesktopCommandError::new("OUTPUT_PATH_INVALID", "输出目录无法安全使用。"))?;
    if !directory.is_dir() {
        return Err(DesktopCommandError::new(
            "OUTPUT_PATH_INVALID",
            "请选择已经存在的本地输出目录。",
        ));
    }
    if state.worker_running.swap(true, Ordering::AcqRel) {
        return Err(DesktopCommandError::new(
            "LOCAL_WORK_ACTIVE",
            "当前扫描或安全导出任务尚未结束。",
        ));
    }

    let (context, runtime_failure_code) = match runtime_context(&app, state.inner()) {
        Ok(context) => (Some(context), None),
        Err(failure) => (None, Some(failure.code)),
    };
    let mut candidates = Vec::new();
    let mut changed_ids = Vec::new();
    let mut reserved = HashSet::new();
    let mut skipped = 0usize;
    let mut initial_failed = 0usize;
    {
        let files = state.files.lock().map_err(|_| {
            state.worker_running.store(false, Ordering::Release);
            task_state_error()
        })?;
        let mut scans = state.scans.lock().map_err(|_| {
            state.worker_running.store(false, Ordering::Release);
            task_state_error()
        })?;
        for (id, file) in files.iter() {
            let Some(record) = scans.get_mut(id) else {
                skipped += 1;
                continue;
            };
            let RegisteredSource::File(source) = &file.source else {
                skipped += 1;
                continue;
            };
            if !batch_export_status_is_eligible(record.status) {
                skipped += 1;
                continue;
            }
            let Some(task) = record.task.clone() else {
                skipped += 1;
                continue;
            };
            match batch_output_path(&directory, source, &mut reserved) {
                Ok((output, output_name)) => {
                    record.begin_export();
                    changed_ids.push(id.clone());
                    candidates.push(BatchExportCandidate {
                        id: id.clone(),
                        task,
                        output,
                        output_name,
                    });
                }
                Err(error) => {
                    record.fail_export(error.code);
                    changed_ids.push(id.clone());
                    initial_failed += 1;
                }
            }
        }
    }

    let attempted = candidates.len() + initial_failed;
    if attempted == 0 {
        state.worker_running.store(false, Ordering::Release);
        return Err(DesktopCommandError::new(
            "BATCH_EXPORT_EMPTY",
            "没有已完成自动确认或人工复核的文件可以批量导出。",
        ));
    }
    for id in &changed_ids {
        emit_scan_summary(&app, id);
    }

    let batch_app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut summary = DesktopBatchExportSummary {
            attempted,
            succeeded: 0,
            failed: initial_failed,
            skipped,
            complete_verifications: 0,
            basic_verifications: 0,
        };
        for candidate in candidates {
            let worker_app = batch_app.clone();
            let export_id = candidate.id.clone();
            let output_name = candidate.output_name;
            let task = candidate.task;
            let output = candidate.output;
            let export_context = context.clone();
            let result = tauri::async_runtime::spawn_blocking(move || {
                export_stored_task(
                    &task,
                    &output,
                    export_context.as_deref(),
                    runtime_failure_code,
                )
            })
            .await;
            match result {
                Ok(Ok(verification_complete)) => {
                    summary.succeeded += 1;
                    if verification_complete {
                        summary.complete_verifications += 1;
                    } else {
                        summary.basic_verifications += 1;
                    }
                    update_scan(&worker_app, &export_id, |record| {
                        record.finish_export(output_name, verification_complete)
                    });
                }
                Ok(Err(code)) => {
                    summary.failed += 1;
                    update_scan(&worker_app, &export_id, |record| record.fail_export(code));
                }
                Err(_) => {
                    summary.failed += 1;
                    update_scan(&worker_app, &export_id, |record| {
                        record.fail_export("EXPORT_WORKER_FAILED")
                    });
                }
            }
        }
        let _ = batch_app.emit(BATCH_EXPORT_COMPLETE_EVENT, summary);
        batch_app
            .state::<DesktopState>()
            .worker_running
            .store(false, Ordering::Release);
    });

    Ok(DesktopBatchExportStart {
        started: true,
        attempted,
        skipped,
    })
}

fn batch_export_status_is_eligible(status: DesktopScanStatus) -> bool {
    matches!(
        status,
        DesktopScanStatus::ReadyToExport
            | DesktopScanStatus::ExportFailed
            | DesktopScanStatus::Complete
    )
}

fn export_stored_task(
    task: &StoredScanTask,
    output: &Path,
    context: Option<&RuntimeContext>,
    runtime_failure_code: Option<&'static str>,
) -> Result<bool, &'static str> {
    match task {
        StoredScanTask::Text(task) => {
            export_task_with_runtimes(task, output, context.map(|context| &context.registry))
                .map(|report| report.complete)
                .map_err(|error| text_export_error_code(&error))
        }
        StoredScanTask::Image(task) => {
            let context =
                context.ok_or(runtime_failure_code.unwrap_or("RUNTIME_REGISTRY_MISSING"))?;
            export_image_task_with_runtimes(task, output, &context.registry)
                .map(|report| report.complete)
                .map_err(|error| image_export_error_code(&error))
        }
        StoredScanTask::Pdf(task) => {
            let context =
                context.ok_or(runtime_failure_code.unwrap_or("RUNTIME_REGISTRY_MISSING"))?;
            export_pdf_task_with_runtimes(task, output, &context.registry)
                .map(|report| report.complete)
                .map_err(|error| pdf_export_error_code(&error))
        }
        StoredScanTask::Docx(task) => {
            export_docx_task_with_runtimes(task, output, context.map(|context| &context.registry))
                .map(|report| report.complete)
                .map_err(|error| docx_export_error_code(&error))
        }
        StoredScanTask::Xlsx(task) => {
            export_xlsx_task_with_runtimes(task, output, context.map(|context| &context.registry))
                .map(|report| report.complete)
                .map_err(|error| xlsx_export_error_code(&error))
        }
        StoredScanTask::Pptx(task) => {
            export_pptx_task_with_runtimes(task, output, context.map(|context| &context.registry))
                .map(|report| report.complete)
                .map_err(|error| pptx_export_error_code(&error))
        }
    }
}

#[tauri::command]
fn copy_redacted_clipboard(
    app: AppHandle,
    state: State<'_, DesktopState>,
    id: String,
) -> Result<bool, DesktopCommandError> {
    if state.worker_running.swap(true, Ordering::AcqRel) {
        return Err(DesktopCommandError::new(
            "LOCAL_WORK_ACTIVE",
            "当前扫描或安全导出任务尚未结束。",
        ));
    }
    let is_clipboard = state
        .files
        .lock()
        .map_err(|_| {
            state.worker_running.store(false, Ordering::Release);
            task_state_error()
        })?
        .get(&id)
        .is_some_and(|file| matches!(&file.source, RegisteredSource::Clipboard(_)));
    if !is_clipboard {
        state.worker_running.store(false, Ordering::Release);
        return Err(DesktopCommandError::new(
            "CLIPBOARD_TASK_REQUIRED",
            "当前任务不是剪贴板文本。",
        ));
    }
    let context = runtime_context(&app, state.inner()).ok();
    let task = {
        let mut scans = state.scans.lock().map_err(|_| {
            state.worker_running.store(false, Ordering::Release);
            task_state_error()
        })?;
        let record = scans.get_mut(&id).ok_or_else(|| {
            state.worker_running.store(false, Ordering::Release);
            scan_not_found_error()
        })?;
        if !matches!(
            record.status,
            DesktopScanStatus::ReadyToExport
                | DesktopScanStatus::ExportFailed
                | DesktopScanStatus::Complete
        ) {
            state.worker_running.store(false, Ordering::Release);
            return Err(DesktopCommandError::new(
                "EXPORT_NOT_READY",
                "请先完成所有待复核结果。",
            ));
        }
        let Some(StoredScanTask::Text(task)) = record.task.clone() else {
            state.worker_running.store(false, Ordering::Release);
            return Err(scan_not_found_error());
        };
        record.begin_export();
        task
    };
    emit_scan_summary(&app, &id);

    let copy_app = app.clone();
    let copy_id = id.clone();
    tauri::async_runtime::spawn(async move {
        let worker_app = copy_app.clone();
        let result = tauri::async_runtime::spawn_blocking(move || {
            render_task_with_runtimes(&task, context.as_deref().map(|context| &context.registry))
                .map(|(text, report)| (text, report.complete))
                .map_err(|error| text_export_error_code(&error))
        })
        .await;
        match result {
            Ok(Ok((text, verification_complete))) => {
                if worker_app.clipboard().write_text(text).is_ok() {
                    update_scan(&worker_app, &copy_id, |record| {
                        record.finish_clipboard_export(verification_complete)
                    });
                } else {
                    update_scan(&worker_app, &copy_id, |record| {
                        record.fail_export("CLIPBOARD_WRITE_FAILED")
                    });
                }
            }
            Ok(Err(code)) => {
                update_scan(&worker_app, &copy_id, |record| record.fail_export(code));
            }
            Err(_) => update_scan(&worker_app, &copy_id, |record| {
                record.fail_export("EXPORT_WORKER_FAILED")
            }),
        }
        worker_app
            .state::<DesktopState>()
            .worker_running
            .store(false, Ordering::Release);
    });
    Ok(true)
}

fn apply_review_mutation(
    app: &AppHandle,
    state: &DesktopState,
    id: &str,
    page_number: usize,
    update: impl FnOnce(&mut ImageTaskDraft) -> Result<(), ImageWorkflowError>,
) -> Result<DesktopReviewMutation, DesktopCommandError> {
    let mutation = {
        let mut scans = state.scans.lock().map_err(|_| task_state_error())?;
        let record = scans.get_mut(id).ok_or_else(scan_not_found_error)?;
        if !matches!(
            record.status,
            DesktopScanStatus::ReviewRequired
                | DesktopScanStatus::ReadyToExport
                | DesktopScanStatus::ExportFailed
                | DesktopScanStatus::Complete
        ) {
            return Err(DesktopCommandError::new(
                "REVIEW_NOT_READY",
                "当前文件尚未进入可复核状态。",
            ));
        }
        let task = record.task.as_mut().ok_or_else(scan_not_found_error)?;
        let page_task = task
            .page_task_mut(page_number)
            .ok_or_else(|| DesktopCommandError::new("REVIEW_PAGE_INVALID", "复核页码无效。"))?;
        update(page_task).map_err(review_error)?;
        let findings = review_findings(&page_task.findings);
        record.refresh_after_review();
        DesktopReviewMutation {
            summary: record.summary(id),
            findings,
        }
    };
    let _ = app.emit(SCAN_PROGRESS_EVENT, mutation.summary.clone());
    Ok(mutation)
}

fn ensure_review_ready(record: &ScanRecord) -> Result<(), DesktopCommandError> {
    if matches!(
        record.status,
        DesktopScanStatus::ReviewRequired
            | DesktopScanStatus::ReadyToExport
            | DesktopScanStatus::ExportFailed
            | DesktopScanStatus::Complete
    ) {
        Ok(())
    } else {
        Err(DesktopCommandError::new(
            "REVIEW_NOT_READY",
            "当前文件尚未进入可复核状态。",
        ))
    }
}

fn emit_scan_summary(app: &AppHandle, id: &str) {
    let summary = {
        let state = app.state::<DesktopState>();
        let Ok(scans) = state.scans.lock() else {
            return;
        };
        scans.get(id).map(|record| record.summary(id))
    };
    if let Some(summary) = summary {
        let _ = app.emit(SCAN_PROGRESS_EVENT, summary);
    }
}

fn review_findings(findings: &[ImageFinding]) -> Vec<DesktopReviewFinding> {
    findings
        .iter()
        .map(|finding| DesktopReviewFinding {
            finding_id: finding.id.clone(),
            group_id: finding.group_id.clone(),
            entity_type: if finding.manual {
                "MANUAL_MASK".to_owned()
            } else {
                entity_type_code(finding.entity_type).to_owned()
            },
            confidence: finding.confidence,
            ocr_confidence: finding.ocr_confidence,
            mask_rect: finding.mask_rect,
            selected: finding.selected,
            reviewed: finding.reviewed,
            manual: finding.manual,
        })
        .collect()
}

fn text_review_findings(
    task: &TaskDraft,
) -> Result<Vec<DesktopTextReviewFinding>, DesktopCommandError> {
    document_text_review_findings(
        &task.document.parts,
        &task.findings,
        TextReviewPresentation::Plain,
    )
}

fn docx_text_review_findings(
    task: &DocxTaskDraft,
) -> Result<Vec<DesktopTextReviewFinding>, DesktopCommandError> {
    document_text_review_findings(
        &task.document.parts,
        &task.findings,
        TextReviewPresentation::Docx,
    )
}

fn xlsx_text_review_findings(
    task: &XlsxTaskDraft,
) -> Result<Vec<DesktopTextReviewFinding>, DesktopCommandError> {
    document_text_review_findings(
        &task.document.parts,
        &task.findings,
        TextReviewPresentation::Xlsx,
    )
}

fn pptx_text_review_findings(
    task: &PptxTaskDraft,
) -> Result<Vec<DesktopTextReviewFinding>, DesktopCommandError> {
    document_text_review_findings(
        &task.document.parts,
        &task.findings,
        TextReviewPresentation::Pptx,
    )
}

#[derive(Clone, Copy)]
enum TextReviewPresentation {
    Plain,
    Docx,
    Xlsx,
    Pptx,
}

fn document_text_review_findings(
    parts: &[DocumentPart],
    findings: &[Finding],
    presentation: TextReviewPresentation,
) -> Result<Vec<DesktopTextReviewFinding>, DesktopCommandError> {
    findings
        .iter()
        .map(|finding| {
            let (part_index, part) = parts
                .iter()
                .enumerate()
                .find(|(_, part)| part.id == finding.part_id)
                .ok_or_else(text_review_data_error)?;
            let characters = part.text.chars().collect::<Vec<_>>();
            if finding.start > finding.end || finding.end > characters.len() {
                return Err(text_review_data_error());
            }
            let matched_text = characters[finding.start..finding.end]
                .iter()
                .collect::<String>();
            if matched_text != finding.matched_text {
                return Err(text_review_data_error());
            }
            let part_number = parts[..=part_index]
                .iter()
                .filter(|candidate| candidate.kind == part.kind)
                .count();
            let (section_label, can_apply, review_note) = match presentation {
                TextReviewPresentation::Plain => (None, true, None),
                TextReviewPresentation::Docx => (
                    Some(docx_section_label(&part.kind, part_number)),
                    true,
                    None,
                ),
                TextReviewPresentation::Xlsx => xlsx_review_presentation(part, part_number),
                TextReviewPresentation::Pptx => pptx_review_presentation(part, part_number),
            };
            let before_start = finding.start.saturating_sub(TEXT_REVIEW_CONTEXT_CHARS);
            let after_end = (finding.end + TEXT_REVIEW_CONTEXT_CHARS).min(characters.len());
            Ok(DesktopTextReviewFinding {
                finding_id: finding.id.clone(),
                entity_type: entity_type_code(finding.entity_type).to_owned(),
                confidence: finding.confidence,
                section_label,
                can_apply,
                review_note,
                context_before: characters[before_start..finding.start].iter().collect(),
                matched_text,
                context_after: characters[finding.end..after_end].iter().collect(),
                replacement: finding.replacement.clone(),
                selected: finding.selected,
                reviewed: finding.reviewed,
            })
        })
        .collect()
}

fn docx_section_label(kind: &str, part_number: usize) -> String {
    let label = match kind {
        "body" => "正文",
        "header" => "页眉",
        "footer" => "页脚",
        "footnote" => "脚注",
        "endnote" => "尾注",
        "comment" => "批注",
        "glossary" => "术语库",
        "chart" => "图表",
        "diagram" => "图示",
        _ => "文档内容",
    };
    format!("{label} · 内容 {part_number}")
}

fn xlsx_review_presentation(
    part: &DocumentPart,
    part_number: usize,
) -> (Option<String>, bool, Option<String>) {
    let worksheet = safe_locator_ordinal(&part.locator, "xl/worksheets/sheet");
    let cell = safe_locator_reference(&part.locator, "#cell=");
    let label = match part.kind.as_str() {
        "cell" | "number" | "formula" | "formula_cache" => {
            let value_kind = match part.kind.as_str() {
                "number" => "数值",
                "formula" => "公式",
                "formula_cache" => "公式缓存",
                _ => "文本",
            };
            match (worksheet, cell) {
                (Some(sheet), Some(cell)) => {
                    format!("工作表 {sheet} · 单元格 {cell} · {value_kind}")
                }
                (_, Some(cell)) => format!("单元格 {cell} · {value_kind}"),
                _ => format!("{value_kind} · 内容 {part_number}"),
            }
        }
        "comment" => {
            let comment = safe_locator_ordinal(&part.locator, "xl/comments");
            let cell = safe_locator_reference(&part.locator, "#comment=");
            match (comment, cell) {
                (Some(comment), Some(cell)) => format!("批注 {comment} · 单元格 {cell}"),
                _ => format!("批注 · 内容 {part_number}"),
            }
        }
        "header_footer" => {
            let area = if part.locator.contains("Footer") {
                "页脚"
            } else {
                "页眉"
            };
            worksheet.map_or_else(
                || format!("{area} · 内容 {part_number}"),
                |sheet| format!("工作表 {sheet} · {area}"),
            )
        }
        "drawing_text" => safe_locator_ordinal(&part.locator, "xl/drawings/drawing").map_or_else(
            || format!("绘图文字 · 内容 {part_number}"),
            |drawing| format!("绘图 {drawing} · 文字 {part_number}"),
        ),
        "sheet_name" => safe_marker_ordinal(&part.locator, "#sheet=").map_or_else(
            || format!("工作表名称 · 内容 {part_number}"),
            |sheet| format!("工作表 {sheet} · 名称"),
        ),
        "defined_name" => safe_marker_ordinal(&part.locator, "#definedName=").map_or_else(
            || format!("定义名称 · 内容 {part_number}"),
            |name| format!("定义名称 {name}"),
        ),
        _ => format!("工作簿内容 · 内容 {part_number}"),
    };
    let can_apply = part.kind != "sheet_name";
    let review_note = match part.kind.as_str() {
        "sheet_name" => Some("工作表名称当前不能安全自动改名，只能明确保留。".to_owned()),
        "formula" | "formula_cache" => {
            Some("此决定会同步到同一单元格的公式和缓存；应用后整格转为普通文本。".to_owned())
        }
        "defined_name" => Some("应用处理会删除整个定义名称。".to_owned()),
        _ => None,
    };
    (Some(label), can_apply, review_note)
}

fn pptx_review_presentation(
    part: &DocumentPart,
    part_number: usize,
) -> (Option<String>, bool, Option<String>) {
    let paragraph = safe_marker_ordinal(&part.locator, "#p");
    let text_node = safe_marker_ordinal(&part.locator, "#t");
    let position = paragraph
        .map(|number| format!("段落 {number}"))
        .or_else(|| text_node.map(|number| format!("文本 {number}")))
        .unwrap_or_else(|| format!("内容 {part_number}"));
    let label = match part.kind.as_str() {
        "slide" => safe_locator_ordinal(&part.locator, "ppt/slides/slide").map_or_else(
            || format!("幻灯片 · {position}"),
            |slide| format!("幻灯片 {slide} · {position}"),
        ),
        "notes" => safe_locator_ordinal(&part.locator, "ppt/notesSlides/notesSlide").map_or_else(
            || format!("演讲者备注 · {position}"),
            |slide| format!("幻灯片 {slide} · 备注 · {position}"),
        ),
        "comment" => safe_locator_ordinal(&part.locator, "ppt/comments/comment")
            .or_else(|| safe_locator_ordinal(&part.locator, "ppt/comments/comments"))
            .map_or_else(
                || format!("批注 · {position}"),
                |comment| format!("批注 {comment} · {position}"),
            ),
        "slide_master" => safe_locator_ordinal(&part.locator, "ppt/slideMasters/slideMaster")
            .map_or_else(
                || format!("幻灯片母版 · {position}"),
                |master| format!("幻灯片母版 {master} · {position}"),
            ),
        "slide_layout" => safe_locator_ordinal(&part.locator, "ppt/slideLayouts/slideLayout")
            .map_or_else(
                || format!("幻灯片版式 · {position}"),
                |layout| format!("幻灯片版式 {layout} · {position}"),
            ),
        "notes_master" => safe_locator_ordinal(&part.locator, "ppt/notesMasters/notesMaster")
            .map_or_else(
                || format!("备注母版 · {position}"),
                |master| format!("备注母版 {master} · {position}"),
            ),
        "handout_master" => safe_locator_ordinal(&part.locator, "ppt/handoutMasters/handoutMaster")
            .map_or_else(
                || format!("讲义母版 · {position}"),
                |master| format!("讲义母版 {master} · {position}"),
            ),
        "diagram" => format!("关系图 · {position}"),
        _ => format!("演示文稿内容 · {position}"),
    };
    let review_note = matches!(
        part.kind.as_str(),
        "slide_master" | "slide_layout" | "notes_master" | "handout_master"
    )
    .then(|| "母版或版式内容可能影响多张页面；替换会保留现有文本样式。".to_owned());
    (Some(label), true, review_note)
}

fn safe_locator_ordinal(locator: &str, prefix: &str) -> Option<usize> {
    let suffix = locator.strip_prefix(prefix)?;
    let digits = suffix
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    (!digits.is_empty()).then(|| digits.parse().ok()).flatten()
}

fn safe_marker_ordinal(locator: &str, marker: &str) -> Option<usize> {
    let suffix = locator.split_once(marker)?.1;
    let digits = suffix
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    (!digits.is_empty()).then(|| digits.parse().ok()).flatten()
}

fn safe_locator_reference(locator: &str, marker: &str) -> Option<String> {
    let value = locator.split_once(marker)?.1.split('#').next()?;
    (value.len() <= 20
        && !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '$'))
    .then(|| value.to_owned())
}

fn text_review_data_error() -> DesktopCommandError {
    DesktopCommandError::new(
        "TEXT_REVIEW_DATA_INVALID",
        "文本复核范围与当前任务不一致，请重新扫描。",
    )
}

fn entity_type_code(entity_type: EntityType) -> &'static str {
    match entity_type {
        EntityType::PersonName => "PERSON_NAME",
        EntityType::OrgName => "ORG_NAME",
        EntityType::Address => "ADDRESS",
        EntityType::PhoneNumber => "PHONE_NUMBER",
        EntityType::Email => "EMAIL",
        EntityType::CnIdNumber => "CN_ID_NUMBER",
        EntityType::BankCardNumber => "BANK_CARD_NUMBER",
        EntityType::FinancialAmount => "FINANCIAL_AMOUNT",
        EntityType::SalaryAmount => "SALARY_AMOUNT",
        EntityType::BusinessMetric => "BUSINESS_METRIC",
        EntityType::ProjectCode => "PROJECT_CODE",
        EntityType::ContractId => "CONTRACT_ID",
        EntityType::CustomerName => "CUSTOMER_NAME",
    }
}

fn export_name_and_extension(source: &Path) -> Result<(String, String), DesktopCommandError> {
    let stem = source
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            DesktopCommandError::new("OUTPUT_NAME_INVALID", "无法生成安全的副本文件名。")
        })?;
    let extension = normalized_extension(source);
    if extension.is_empty() {
        return Err(DesktopCommandError::new(
            "OUTPUT_NAME_INVALID",
            "无法生成安全的副本文件名。",
        ));
    }
    Ok((format!("{stem}_redacted.{extension}"), extension))
}

fn batch_output_path(
    directory: &Path,
    source: &Path,
    reserved: &mut HashSet<PathBuf>,
) -> Result<(PathBuf, String), DesktopCommandError> {
    let stem = source
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            DesktopCommandError::new("OUTPUT_NAME_INVALID", "无法生成安全的副本文件名。")
        })?;
    let extension = normalized_extension(source);
    if extension.is_empty() {
        return Err(DesktopCommandError::new(
            "OUTPUT_NAME_INVALID",
            "无法生成安全的副本文件名。",
        ));
    }
    for ordinal in 1..=10_000usize {
        let name = if ordinal == 1 {
            format!("{stem}_redacted.{extension}")
        } else {
            format!("{stem}_redacted_{ordinal}.{extension}")
        };
        let output = directory.join(&name);
        if !output.exists() && reserved.insert(output.clone()) {
            return Ok((output, name));
        }
    }
    Err(DesktopCommandError::new(
        "OUTPUT_NAME_UNAVAILABLE",
        "输出目录中没有可用的安全副本文件名。",
    ))
}

fn task_state_error() -> DesktopCommandError {
    DesktopCommandError::new("TASK_STATE_UNAVAILABLE", "本地任务状态暂时不可用。")
}

fn scan_not_found_error() -> DesktopCommandError {
    DesktopCommandError::new("SCAN_NOT_FOUND", "找不到对应的本地扫描任务。")
}

fn preview_error(code: &'static str) -> DesktopCommandError {
    DesktopCommandError::new(code, "无法安全生成当前复核页预览。")
}

fn review_error(error: ImageWorkflowError) -> DesktopCommandError {
    match error {
        ImageWorkflowError::InvalidMaskRect(_) => {
            DesktopCommandError::new("MASK_RECT_INVALID", "遮罩范围无效或超出页面边界。")
        }
        ImageWorkflowError::FindingNotFound(_) | ImageWorkflowError::GroupNotFound(_) => {
            DesktopCommandError::new("REVIEW_RESULT_NOT_FOUND", "找不到对应的复核结果。")
        }
        ImageWorkflowError::DetectedGroupCannotBeRemoved(_) => DesktopCommandError::new(
            "DETECTED_RESULT_CANNOT_BE_REMOVED",
            "自动检测结果请标记为保留原文；只有手动遮罩可以删除。",
        ),
        _ => DesktopCommandError::new("REVIEW_UPDATE_FAILED", "复核修改未能安全保存。"),
    }
}

fn text_review_error(error: WorkflowError) -> DesktopCommandError {
    match error {
        WorkflowError::FindingNotFound(_) => {
            DesktopCommandError::new("REVIEW_RESULT_NOT_FOUND", "找不到对应的文本复核结果。")
        }
        WorkflowError::ReplacementTooLong => {
            DesktopCommandError::new("REPLACEMENT_TOO_LONG", "替换内容不能超过 256 个字符。")
        }
        WorkflowError::InvalidPolicy(_) | WorkflowError::PolicySnapshotMismatch => {
            DesktopCommandError::new("POLICY_INVALID", "任务策略快照无效，请重新扫描。")
        }
        _ => DesktopCommandError::new("REVIEW_UPDATE_FAILED", "文本复核修改未能安全保存。"),
    }
}

fn docx_review_error(error: DocxWorkflowError) -> DesktopCommandError {
    match error {
        DocxWorkflowError::TextDetection(error) => text_review_error(error),
        DocxWorkflowError::InvalidFinding(_) | DocxWorkflowError::EmbeddedImageTaskMismatch(_) => {
            DesktopCommandError::new("REVIEW_DATA_INVALID", "DOCX 复核数据无效，请重新扫描。")
        }
        DocxWorkflowError::InvalidPolicy(_) | DocxWorkflowError::PolicySnapshotMismatch => {
            DesktopCommandError::new("POLICY_INVALID", "任务策略快照无效，请重新扫描。")
        }
        _ => DesktopCommandError::new("REVIEW_UPDATE_FAILED", "DOCX 复核修改未能安全保存。"),
    }
}

fn xlsx_review_error(error: XlsxWorkflowError) -> DesktopCommandError {
    match error {
        XlsxWorkflowError::Text(error) => text_review_error(error),
        XlsxWorkflowError::SheetRenameUnsupported(_) => DesktopCommandError::new(
            "XLSX_SHEET_RENAME_UNSUPPORTED",
            "工作表名称当前不能安全自动改名，请选择保留原文。",
        ),
        XlsxWorkflowError::InvalidFinding(_)
        | XlsxWorkflowError::EmbeddedImageTaskMismatch(_)
        | XlsxWorkflowError::ConflictingFormulaReplacement(_) => {
            DesktopCommandError::new("REVIEW_DATA_INVALID", "XLSX 复核数据无效，请重新扫描。")
        }
        XlsxWorkflowError::InvalidPolicy(_) | XlsxWorkflowError::PolicySnapshotMismatch => {
            DesktopCommandError::new("POLICY_INVALID", "任务策略快照无效，请重新扫描。")
        }
        _ => DesktopCommandError::new("REVIEW_UPDATE_FAILED", "XLSX 复核修改未能安全保存。"),
    }
}

fn pptx_review_error(error: PptxWorkflowError) -> DesktopCommandError {
    match error {
        PptxWorkflowError::TextDetection(error) => text_review_error(error),
        PptxWorkflowError::InvalidFinding(_) | PptxWorkflowError::EmbeddedImageTaskMismatch(_) => {
            DesktopCommandError::new("REVIEW_DATA_INVALID", "PPTX 复核数据无效，请重新扫描。")
        }
        PptxWorkflowError::InvalidPolicy(_) | PptxWorkflowError::PolicySnapshotMismatch => {
            DesktopCommandError::new("POLICY_INVALID", "任务策略快照无效，请重新扫描。")
        }
        _ => DesktopCommandError::new("REVIEW_UPDATE_FAILED", "PPTX 复核修改未能安全保存。"),
    }
}

fn run_scan_batch(app: AppHandle, candidates: Vec<ScanCandidate>) {
    let (context, runtime_failure_code) =
        match runtime_context(&app, app.state::<DesktopState>().inner()) {
            Ok(context) => (Some(context), None),
            Err(failure) => (None, Some(failure.code)),
        };
    let policy = PolicyConfig::default();

    for candidate in candidates {
        if candidate.cancel_requested.load(Ordering::Acquire) {
            update_scan(&app, &candidate.id, ScanRecord::cancel);
            continue;
        }
        if matches!(
            candidate.kind,
            DesktopFileKind::Image | DesktopFileKind::Pdf
        ) && context.is_none()
        {
            update_scan(&app, &candidate.id, |record| {
                record.block(runtime_failure_code.unwrap_or("RUNTIME_REGISTRY_MISSING"))
            });
            continue;
        }
        if candidate.kind == DesktopFileKind::Pdf
            && !context
                .as_ref()
                .is_some_and(|context| context.pdf_tools_ready)
        {
            update_scan(&app, &candidate.id, |record| {
                record.block("PDF_TOOLS_MISSING")
            });
            continue;
        }

        update_scan(&app, &candidate.id, |record| {
            record.status = DesktopScanStatus::Scanning;
            record.stage = DesktopScanStage::Preflight;
            record.completed_units = 0;
            record.total_units = 0;
            record.task = None;
            record.error_code = None;
            record.output_name = None;
            record.output_kind = None;
            record.verification_complete = false;
        });

        match candidate.kind {
            DesktopFileKind::Text => {
                scan_text_candidate(&app, &candidate, context.as_deref(), &policy)
            }
            DesktopFileKind::Word => {
                scan_docx_candidate(&app, &candidate, context.as_deref(), &policy)
            }
            DesktopFileKind::Spreadsheet => {
                scan_xlsx_candidate(&app, &candidate, context.as_deref(), &policy)
            }
            DesktopFileKind::Presentation => {
                scan_pptx_candidate(&app, &candidate, context.as_deref(), &policy)
            }
            DesktopFileKind::Image => scan_image_candidate(
                &app,
                &candidate,
                context.as_deref().expect("image runtime checked above"),
                &policy,
            ),
            DesktopFileKind::Pdf => scan_pdf_candidate(
                &app,
                &candidate,
                context.as_deref().expect("PDF runtime checked above"),
                &policy,
            ),
            _ => update_scan(&app, &candidate.id, |record| {
                record.block("SCAN_FORMAT_NOT_AVAILABLE")
            }),
        }
    }
}

fn scan_text_candidate(
    app: &AppHandle,
    candidate: &ScanCandidate,
    context: Option<&RuntimeContext>,
    policy: &PolicyConfig,
) {
    update_scan(app, &candidate.id, |record| {
        record.stage = DesktopScanStage::DetectingText;
        record.total_units = 1;
    });
    let runtimes = context.map(|context| &context.registry);
    let result = match &candidate.source {
        RegisteredSource::File(path) => scan_path_with_policy(path, policy, runtimes),
        RegisteredSource::Clipboard(text) => scan_text_with_policy(text.clone(), policy, runtimes),
    };
    if candidate.cancel_requested.load(Ordering::Acquire) {
        update_scan(app, &candidate.id, ScanRecord::cancel);
        return;
    }
    match result {
        Ok(task) => update_scan(app, &candidate.id, |record| {
            record.finish(StoredScanTask::Text(Box::new(task)))
        }),
        Err(error) => update_scan(app, &candidate.id, |record| {
            record.block(text_error_code(&error))
        }),
    }
}

fn scan_docx_candidate(
    app: &AppHandle,
    candidate: &ScanCandidate,
    context: Option<&RuntimeContext>,
    policy: &PolicyConfig,
) {
    let Some(path) = registered_source_path(&candidate.source) else {
        update_scan(app, &candidate.id, |record| {
            record.block("SCAN_SOURCE_INVALID")
        });
        return;
    };
    update_scan(app, &candidate.id, |record| {
        record.stage = DesktopScanStage::DetectingText;
        record.total_units = 1;
    });
    let result = scan_docx_with_policy_and_images(
        path,
        policy,
        context.map(|context| &context.registry),
        context.map(|context| context.ocr_runtime_id.as_str()),
    );
    if candidate.cancel_requested.load(Ordering::Acquire) {
        update_scan(app, &candidate.id, ScanRecord::cancel);
        return;
    }
    match result {
        Ok(task) => {
            if let Some(code) = docx_blocking_diagnostic(&task) {
                update_scan(app, &candidate.id, |record| record.block(code));
            } else {
                update_scan(app, &candidate.id, |record| {
                    record.finish(StoredScanTask::Docx(Box::new(task)))
                });
            }
        }
        Err(error) => update_scan(app, &candidate.id, |record| {
            record.block(docx_error_code(&error))
        }),
    }
}

fn scan_xlsx_candidate(
    app: &AppHandle,
    candidate: &ScanCandidate,
    context: Option<&RuntimeContext>,
    policy: &PolicyConfig,
) {
    let Some(path) = registered_source_path(&candidate.source) else {
        update_scan(app, &candidate.id, |record| {
            record.block("SCAN_SOURCE_INVALID")
        });
        return;
    };
    update_scan(app, &candidate.id, |record| {
        record.stage = DesktopScanStage::DetectingText;
        record.total_units = 1;
    });
    let result = scan_xlsx_with_policy(
        path,
        policy,
        context.map(|context| &context.registry),
        context.map(|context| context.ocr_runtime_id.as_str()),
    );
    if candidate.cancel_requested.load(Ordering::Acquire) {
        update_scan(app, &candidate.id, ScanRecord::cancel);
        return;
    }
    match result {
        Ok(task) => {
            if let Some(code) = xlsx_blocking_diagnostic(&task) {
                update_scan(app, &candidate.id, |record| record.block(code));
            } else {
                update_scan(app, &candidate.id, |record| {
                    record.finish(StoredScanTask::Xlsx(Box::new(task)))
                });
            }
        }
        Err(error) => update_scan(app, &candidate.id, |record| {
            record.block(xlsx_error_code(&error))
        }),
    }
}

fn scan_pptx_candidate(
    app: &AppHandle,
    candidate: &ScanCandidate,
    context: Option<&RuntimeContext>,
    policy: &PolicyConfig,
) {
    let Some(path) = registered_source_path(&candidate.source) else {
        update_scan(app, &candidate.id, |record| {
            record.block("SCAN_SOURCE_INVALID")
        });
        return;
    };
    update_scan(app, &candidate.id, |record| {
        record.stage = DesktopScanStage::DetectingText;
        record.total_units = 1;
    });
    let result = scan_pptx_with_policy_and_images(
        path,
        policy,
        context.map(|context| &context.registry),
        context.map(|context| context.ocr_runtime_id.as_str()),
    );
    if candidate.cancel_requested.load(Ordering::Acquire) {
        update_scan(app, &candidate.id, ScanRecord::cancel);
        return;
    }
    match result {
        Ok(task) => {
            if let Some(code) = pptx_blocking_diagnostic(&task) {
                update_scan(app, &candidate.id, |record| record.block(code));
            } else {
                update_scan(app, &candidate.id, |record| {
                    record.finish(StoredScanTask::Pptx(Box::new(task)))
                });
            }
        }
        Err(error) => update_scan(app, &candidate.id, |record| {
            record.block(pptx_error_code(&error))
        }),
    }
}

fn scan_image_candidate(
    app: &AppHandle,
    candidate: &ScanCandidate,
    context: &RuntimeContext,
    policy: &PolicyConfig,
) {
    let Some(path) = registered_source_path(&candidate.source) else {
        update_scan(app, &candidate.id, |record| {
            record.block("SCAN_SOURCE_INVALID")
        });
        return;
    };
    update_scan(app, &candidate.id, |record| {
        record.stage = DesktopScanStage::Ocr;
        record.total_units = 1;
    });
    let result = scan_image_with_policy(path, policy, &context.registry, &context.ocr_runtime_id);
    if candidate.cancel_requested.load(Ordering::Acquire) {
        update_scan(app, &candidate.id, ScanRecord::cancel);
        return;
    }
    match result {
        Ok(task) => update_scan(app, &candidate.id, |record| {
            record.finish(StoredScanTask::Image(Box::new(task)))
        }),
        Err(error) => update_scan(app, &candidate.id, |record| {
            record.block(image_error_code(&error))
        }),
    }
}

fn scan_pdf_candidate(
    app: &AppHandle,
    candidate: &ScanCandidate,
    context: &RuntimeContext,
    policy: &PolicyConfig,
) {
    let Some(path) = registered_source_path(&candidate.source) else {
        update_scan(app, &candidate.id, |record| {
            record.block("SCAN_SOURCE_INVALID")
        });
        return;
    };
    let progress_app = app.clone();
    let progress_id = candidate.id.clone();
    let cancel_requested = candidate.cancel_requested.clone();
    let result = scan_pdf_with_policy_and_progress(
        path,
        policy,
        &context.registry,
        &context.ocr_runtime_id,
        move |progress| {
            update_scan(&progress_app, &progress_id, |record| {
                record.stage = DesktopScanStage::ScanningPages;
                record.completed_units = progress.completed_pages;
                record.total_units = progress.total_pages;
            });
            !cancel_requested.load(Ordering::Acquire)
        },
    );
    if candidate.cancel_requested.load(Ordering::Acquire) {
        update_scan(app, &candidate.id, ScanRecord::cancel);
        return;
    }
    match result {
        Ok(task) => update_scan(app, &candidate.id, |record| {
            record.finish(StoredScanTask::Pdf(Box::new(task)))
        }),
        Err(PdfWorkflowError::ScanCancelled) => update_scan(app, &candidate.id, ScanRecord::cancel),
        Err(error) => update_scan(app, &candidate.id, |record| {
            record.block(pdf_error_code(&error))
        }),
    }
}

fn update_scan(app: &AppHandle, id: &str, update: impl FnOnce(&mut ScanRecord)) {
    let summary = {
        let state = app.state::<DesktopState>();
        let Ok(mut scans) = state.scans.lock() else {
            return;
        };
        let Some(record) = scans.get_mut(id) else {
            return;
        };
        update(record);
        record.summary(id)
    };
    let _ = app.emit(SCAN_PROGRESS_EVENT, summary);
}

fn block_active_scans(app: &AppHandle, code: &'static str) {
    let summaries = {
        let state = app.state::<DesktopState>();
        let Ok(mut scans) = state.scans.lock() else {
            return;
        };
        scans
            .iter_mut()
            .filter_map(|(id, record)| {
                if matches!(
                    record.status,
                    DesktopScanStatus::Queued
                        | DesktopScanStatus::Scanning
                        | DesktopScanStatus::Cancelling
                ) {
                    record.block(code);
                    Some(record.summary(id))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
    };
    for summary in summaries {
        let _ = app.emit(SCAN_PROGRESS_EVENT, summary);
    }
}

fn runtime_status(app: &AppHandle, state: &DesktopState) -> DesktopRuntimeStatus {
    let policy_ready = PolicyConfig::default().validate().is_ok();
    match runtime_context(app, state) {
        Ok(context) => DesktopRuntimeStatus {
            policy_ready,
            runtime_registry_ready: true,
            ocr_ready: true,
            ocr_runtime_id: Some(context.ocr_runtime_id.clone()),
            pdf_tools_ready: context.pdf_tools_ready,
            scan_ready: policy_ready,
            status_code: if context.pdf_tools_ready {
                "READY"
            } else {
                "PDF_TOOLS_MISSING"
            }
            .to_owned(),
        },
        Err(failure) => DesktopRuntimeStatus {
            policy_ready,
            runtime_registry_ready: false,
            ocr_ready: false,
            ocr_runtime_id: None,
            pdf_tools_ready: pdf_tools_available(),
            scan_ready: false,
            status_code: failure.code.to_owned(),
        },
    }
}

fn runtime_context(
    app: &AppHandle,
    state: &DesktopState,
) -> Result<Arc<RuntimeContext>, RuntimeLoadFailure> {
    if let Ok(runtime) = state.runtime.lock()
        && let Some(context) = runtime.as_ref()
    {
        return Ok(context.clone());
    }

    let path = runtime_registry_path(app).ok_or(RuntimeLoadFailure {
        code: "RUNTIME_REGISTRY_MISSING",
    })?;
    let registry = RuntimeRegistry::from_path(&path).map_err(|_| RuntimeLoadFailure {
        code: "RUNTIME_REGISTRY_INVALID",
    })?;
    registry
        .verify_installation()
        .map_err(|_| RuntimeLoadFailure {
            code: "RUNTIME_ASSET_INVALID",
        })?;
    let ocr_runtime_id = registry
        .detectors
        .iter()
        .find(|runtime| runtime.kind == DetectorKind::Ocr)
        .map(|runtime| runtime.id.clone())
        .ok_or(RuntimeLoadFailure {
            code: "OCR_RUNTIME_MISSING",
        })?;
    let context = Arc::new(RuntimeContext {
        registry,
        ocr_runtime_id,
        pdf_tools_ready: pdf_tools_available(),
    });
    let mut cached = state.runtime.lock().map_err(|_| RuntimeLoadFailure {
        code: "TASK_STATE_UNAVAILABLE",
    })?;
    *cached = Some(context.clone());
    Ok(context)
}

fn runtime_registry_path(app: &AppHandle) -> Option<PathBuf> {
    if let Some(path) = env::var_os(RUNTIME_REGISTRY_ENV) {
        return Some(PathBuf::from(path));
    }
    if let Ok(resource_dir) = app.path().resource_dir() {
        let bundled = resource_dir.join("runtimes").join("default.json");
        if bundled.is_file() {
            return Some(bundled);
        }
    }
    #[cfg(debug_assertions)]
    {
        let development = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../config/runtimes/development-ocr-small.json");
        if development.is_file() {
            return Some(development);
        }
    }
    None
}

fn pdf_tools_available() -> bool {
    command_available("LLAMASK_PDFINFO", "pdfinfo")
        && command_available("LLAMASK_PDFTOPPM", "pdftoppm")
}

fn command_available(variable: &str, fallback: &str) -> bool {
    let executable = env::var_os(variable).unwrap_or_else(|| OsString::from(fallback));
    Command::new(executable)
        .arg("-v")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn image_task_metrics(task: &ImageTaskDraft) -> ScanMetrics {
    let mut groups = BTreeMap::<&str, bool>::new();
    for finding in &task.findings {
        groups
            .entry(finding.group_id.as_str())
            .and_modify(|reviewed| *reviewed &= finding.reviewed)
            .or_insert(finding.reviewed);
    }
    ScanMetrics {
        finding_groups: groups.len(),
        unreviewed_groups: groups.values().filter(|reviewed| !**reviewed).count(),
        page_count: 1,
        diagnostic_count: task.diagnostics.len(),
    }
}

fn text_task_metrics(task: &TaskDraft) -> ScanMetrics {
    ScanMetrics {
        finding_groups: task.findings.len(),
        unreviewed_groups: task
            .findings
            .iter()
            .filter(|finding| !finding.reviewed)
            .count(),
        page_count: 1,
        diagnostic_count: task.diagnostics.len(),
    }
}

fn pdf_task_metrics(task: &PdfTaskDraft) -> ScanMetrics {
    let mut groups = BTreeMap::<(usize, &str), bool>::new();
    let mut diagnostic_count = task.diagnostics.len();
    for page in &task.pages {
        diagnostic_count += page.task.diagnostics.len();
        for finding in &page.task.findings {
            groups
                .entry((page.page_number, finding.group_id.as_str()))
                .and_modify(|reviewed| *reviewed &= finding.reviewed)
                .or_insert(finding.reviewed);
        }
    }
    ScanMetrics {
        finding_groups: groups.len(),
        unreviewed_groups: groups.values().filter(|reviewed| !**reviewed).count(),
        page_count: task.pages.len(),
        diagnostic_count,
    }
}

fn docx_task_metrics(task: &DocxTaskDraft) -> ScanMetrics {
    let image_groups = docx_image_group_states(task);
    ScanMetrics {
        finding_groups: task.findings.len() + image_groups.len(),
        unreviewed_groups: task
            .findings
            .iter()
            .filter(|finding| !finding.reviewed)
            .count()
            + image_groups.values().filter(|reviewed| !**reviewed).count(),
        page_count: task.embedded_images.len(),
        diagnostic_count: task.diagnostics.len()
            + task
                .embedded_images
                .iter()
                .map(|embedded| embedded.task.diagnostics.len())
                .sum::<usize>(),
    }
}

fn docx_image_group_states(task: &DocxTaskDraft) -> BTreeMap<(usize, &str), bool> {
    let mut groups = BTreeMap::new();
    for (image_index, embedded) in task.embedded_images.iter().enumerate() {
        for finding in &embedded.task.findings {
            groups
                .entry((image_index, finding.group_id.as_str()))
                .and_modify(|reviewed| *reviewed &= finding.reviewed)
                .or_insert(finding.reviewed);
        }
    }
    groups
}

fn docx_unreviewed_image_groups(task: &DocxTaskDraft) -> usize {
    docx_image_group_states(task)
        .values()
        .filter(|reviewed| !**reviewed)
        .count()
}

fn docx_blocking_diagnostic(task: &DocxTaskDraft) -> Option<&'static str> {
    task.diagnostics
        .iter()
        .find_map(|diagnostic| match diagnostic.code.as_str() {
            "DOCX_EMBEDDED_IMAGE_FORMAT_UNSUPPORTED" => Some("DOCX_EMBEDDED_IMAGE_UNSUPPORTED"),
            "DOCX_EMBEDDED_IMAGES_PENDING" => Some("OCR_RUNTIME_MISSING"),
            "DOCX_EMBEDDED_OBJECTS_UNSUPPORTED" => Some("DOCX_EMBEDDED_OBJECTS_UNSUPPORTED"),
            "DOCX_ACTIVE_CONTENT_UNSUPPORTED" => Some("DOCX_ACTIVE_CONTENT_UNSUPPORTED"),
            _ => None,
        })
}

fn xlsx_task_metrics(task: &XlsxTaskDraft) -> ScanMetrics {
    let image_groups = xlsx_image_group_states(task);
    ScanMetrics {
        finding_groups: task.findings.len() + image_groups.len(),
        unreviewed_groups: task
            .findings
            .iter()
            .filter(|finding| !finding.reviewed)
            .count()
            + image_groups.values().filter(|reviewed| !**reviewed).count(),
        page_count: task.embedded_images.len(),
        diagnostic_count: task.diagnostics.len()
            + task
                .embedded_images
                .iter()
                .map(|embedded| embedded.task.diagnostics.len())
                .sum::<usize>(),
    }
}

fn xlsx_image_group_states(task: &XlsxTaskDraft) -> BTreeMap<(usize, &str), bool> {
    let mut groups = BTreeMap::new();
    for (image_index, embedded) in task.embedded_images.iter().enumerate() {
        for finding in &embedded.task.findings {
            groups
                .entry((image_index, finding.group_id.as_str()))
                .and_modify(|reviewed| *reviewed &= finding.reviewed)
                .or_insert(finding.reviewed);
        }
    }
    groups
}

fn xlsx_unreviewed_image_groups(task: &XlsxTaskDraft) -> usize {
    xlsx_image_group_states(task)
        .values()
        .filter(|reviewed| !**reviewed)
        .count()
}

fn xlsx_blocking_diagnostic(task: &XlsxTaskDraft) -> Option<&'static str> {
    task.diagnostics
        .iter()
        .find_map(|diagnostic| match diagnostic.code.as_str() {
            "XLSX_EMBEDDED_IMAGE_FORMAT_UNSUPPORTED" => Some("XLSX_EMBEDDED_IMAGE_UNSUPPORTED"),
            "XLSX_EMBEDDED_IMAGES_PENDING" => Some("OCR_RUNTIME_MISSING"),
            "XLSX_UNSUPPORTED_PAYLOAD" => Some("XLSX_UNSUPPORTED_PAYLOAD"),
            _ => None,
        })
}

fn image_error_code(error: &ImageWorkflowError) -> &'static str {
    match error {
        ImageWorkflowError::ImageTooLarge | ImageWorkflowError::ImageDimensionsTooLarge => {
            "IMAGE_LIMIT_EXCEEDED"
        }
        ImageWorkflowError::UnsupportedImageType => "UNSUPPORTED_IMAGE",
        ImageWorkflowError::OcrRuntimeUnavailable(_) => "OCR_RUNTIME_MISSING",
        ImageWorkflowError::Ocr(_) => "OCR_FAILED",
        ImageWorkflowError::InvalidPolicy(_) => "POLICY_INVALID",
        ImageWorkflowError::Io(_) | ImageWorkflowError::Image(_) => "IMAGE_READ_FAILED",
        _ => "IMAGE_SCAN_FAILED",
    }
}

fn pptx_task_metrics(task: &PptxTaskDraft) -> ScanMetrics {
    let image_groups = pptx_image_group_states(task);
    ScanMetrics {
        finding_groups: task.findings.len() + image_groups.len(),
        unreviewed_groups: task
            .findings
            .iter()
            .filter(|finding| !finding.reviewed)
            .count()
            + image_groups.values().filter(|reviewed| !**reviewed).count(),
        page_count: task.embedded_images.len(),
        diagnostic_count: task.diagnostics.len()
            + task
                .embedded_images
                .iter()
                .map(|embedded| embedded.task.diagnostics.len())
                .sum::<usize>(),
    }
}

fn pptx_image_group_states(task: &PptxTaskDraft) -> BTreeMap<(usize, &str), bool> {
    let mut groups = BTreeMap::new();
    for (image_index, embedded) in task.embedded_images.iter().enumerate() {
        for finding in &embedded.task.findings {
            groups
                .entry((image_index, finding.group_id.as_str()))
                .and_modify(|reviewed| *reviewed &= finding.reviewed)
                .or_insert(finding.reviewed);
        }
    }
    groups
}

fn pptx_unreviewed_image_groups(task: &PptxTaskDraft) -> usize {
    pptx_image_group_states(task)
        .values()
        .filter(|reviewed| !**reviewed)
        .count()
}

fn pptx_blocking_diagnostic(task: &PptxTaskDraft) -> Option<&'static str> {
    task.diagnostics
        .iter()
        .find_map(|diagnostic| match diagnostic.code.as_str() {
            "PPTX_EMBEDDED_IMAGE_FORMAT_UNSUPPORTED" => Some("PPTX_EMBEDDED_IMAGE_UNSUPPORTED"),
            "PPTX_EMBEDDED_IMAGES_PENDING" => Some("OCR_RUNTIME_MISSING"),
            "PPTX_EMBEDDED_OBJECTS_UNSUPPORTED" => Some("PPTX_EMBEDDED_OBJECTS_UNSUPPORTED"),
            "PPTX_ACTIVE_CONTENT_UNSUPPORTED" => Some("PPTX_ACTIVE_CONTENT_UNSUPPORTED"),
            _ => None,
        })
}

fn text_error_code(error: &WorkflowError) -> &'static str {
    match error {
        WorkflowError::UnsupportedEncoding(_) => "TEXT_ENCODING_UNSUPPORTED",
        WorkflowError::UnsupportedFileType(_) => "UNSUPPORTED_TEXT_TYPE",
        WorkflowError::RequiredDetectorUnavailable(_) => "MODEL_RUNTIME_REQUIRED",
        WorkflowError::InvalidPolicy(_) => "POLICY_INVALID",
        WorkflowError::Io(_) => "TEXT_READ_FAILED",
        _ => "TEXT_SCAN_FAILED",
    }
}

fn docx_error_code(error: &DocxWorkflowError) -> &'static str {
    match error {
        DocxWorkflowError::PackageTooLarge
        | DocxWorkflowError::TooManyEntries
        | DocxWorkflowError::EntryTooLarge(_)
        | DocxWorkflowError::SuspiciousCompression(_) => "DOCX_LIMIT_EXCEEDED",
        DocxWorkflowError::EncryptedEntry => "ENCRYPTED_DOCX_UNSUPPORTED",
        DocxWorkflowError::UnsupportedCompression(_)
        | DocxWorkflowError::UnsafeEntryName(_)
        | DocxWorkflowError::MissingRequiredEntry(_)
        | DocxWorkflowError::InvalidXml(_)
        | DocxWorkflowError::MissingTextContent
        | DocxWorkflowError::Zip(_) => "INVALID_DOCX",
        DocxWorkflowError::TextDetection(error) => text_error_code(error),
        DocxWorkflowError::EmbeddedImage(error) => image_error_code(error),
        DocxWorkflowError::InvalidPolicy(_) => "POLICY_INVALID",
        DocxWorkflowError::Io(_) => "DOCX_READ_FAILED",
        _ => "DOCX_SCAN_FAILED",
    }
}

fn xlsx_error_code(error: &XlsxWorkflowError) -> &'static str {
    match error {
        XlsxWorkflowError::PackageTooLarge
        | XlsxWorkflowError::TooManyEntries
        | XlsxWorkflowError::EntryTooLarge(_)
        | XlsxWorkflowError::SuspiciousCompression(_) => "XLSX_LIMIT_EXCEEDED",
        XlsxWorkflowError::EncryptedEntry => "ENCRYPTED_XLSX_UNSUPPORTED",
        XlsxWorkflowError::UnsupportedCompression(_)
        | XlsxWorkflowError::UnsafeEntryName(_)
        | XlsxWorkflowError::MissingRequiredEntry(_)
        | XlsxWorkflowError::InvalidXml(_)
        | XlsxWorkflowError::MissingTextContent
        | XlsxWorkflowError::Zip(_) => "INVALID_XLSX",
        XlsxWorkflowError::Text(error) => text_error_code(error),
        XlsxWorkflowError::EmbeddedImage(error) => image_error_code(error),
        XlsxWorkflowError::InvalidPolicy(_) => "POLICY_INVALID",
        XlsxWorkflowError::Io(_) => "XLSX_READ_FAILED",
        _ => "XLSX_SCAN_FAILED",
    }
}

fn pptx_error_code(error: &PptxWorkflowError) -> &'static str {
    match error {
        PptxWorkflowError::PackageTooLarge
        | PptxWorkflowError::TooManyEntries
        | PptxWorkflowError::EntryTooLarge(_)
        | PptxWorkflowError::SuspiciousCompression(_) => "PPTX_LIMIT_EXCEEDED",
        PptxWorkflowError::EncryptedEntry => "ENCRYPTED_PPTX_UNSUPPORTED",
        PptxWorkflowError::UnsupportedCompression(_)
        | PptxWorkflowError::UnsafeEntryName(_)
        | PptxWorkflowError::MissingRequiredEntry(_)
        | PptxWorkflowError::InvalidXml(_)
        | PptxWorkflowError::MissingTextContent
        | PptxWorkflowError::Zip(_) => "INVALID_PPTX",
        PptxWorkflowError::TextDetection(error) => text_error_code(error),
        PptxWorkflowError::EmbeddedImage(error) => image_error_code(error),
        PptxWorkflowError::InvalidPolicy(_) => "POLICY_INVALID",
        PptxWorkflowError::Io(_) => "PPTX_READ_FAILED",
        _ => "PPTX_SCAN_FAILED",
    }
}

fn text_export_error_code(error: &WorkflowError) -> &'static str {
    match error {
        WorkflowError::OutputExists(_) => "OUTPUT_EXISTS",
        WorkflowError::WouldOverwriteSource => "OUTPUT_CONFLICT",
        WorkflowError::UnreviewedFindings(_) => "REVIEW_REQUIRED",
        WorkflowError::VerificationFailed(_) => "VERIFICATION_FAILED",
        WorkflowError::SourceChanged => "SOURCE_CHANGED",
        WorkflowError::UnsupportedEncoding(_) => "TEXT_ENCODING_UNSUPPORTED",
        WorkflowError::RequiredDetectorUnavailable(_) => "MODEL_RUNTIME_REQUIRED",
        WorkflowError::InvalidPolicy(_) | WorkflowError::PolicySnapshotMismatch => "POLICY_INVALID",
        WorkflowError::InvalidSpan(_)
        | WorkflowError::SpanTextMismatch(_)
        | WorkflowError::OverlappingFindings
        | WorkflowError::FindingNotFound(_)
        | WorkflowError::ReplacementTooLong => "REVIEW_DATA_INVALID",
        WorkflowError::Io(_) => "OUTPUT_WRITE_FAILED",
        _ => "TEXT_EXPORT_FAILED",
    }
}

fn docx_export_error_code(error: &DocxWorkflowError) -> &'static str {
    match error {
        DocxWorkflowError::OutputExists(_) => "OUTPUT_EXISTS",
        DocxWorkflowError::WouldOverwriteSource => "OUTPUT_CONFLICT",
        DocxWorkflowError::UnreviewedFindings(_) => "REVIEW_REQUIRED",
        DocxWorkflowError::VerificationFailed(_) => "VERIFICATION_FAILED",
        DocxWorkflowError::SourceChanged => "SOURCE_CHANGED",
        DocxWorkflowError::EmbeddedImagesUnsupported
        | DocxWorkflowError::EmbeddedImageRuntimeRequired => "OCR_RUNTIME_MISSING",
        DocxWorkflowError::UnsupportedEmbeddedImageType(_) => "DOCX_EMBEDDED_IMAGE_UNSUPPORTED",
        DocxWorkflowError::EmbeddedObjectsUnsupported => "DOCX_EMBEDDED_OBJECTS_UNSUPPORTED",
        DocxWorkflowError::ActiveContentUnsupported => "DOCX_ACTIVE_CONTENT_UNSUPPORTED",
        DocxWorkflowError::ExternalRelationshipUnsupported(_) => {
            "DOCX_EXTERNAL_RELATIONSHIP_UNSUPPORTED"
        }
        DocxWorkflowError::InvalidPolicy(_) | DocxWorkflowError::PolicySnapshotMismatch => {
            "POLICY_INVALID"
        }
        DocxWorkflowError::InvalidFinding(_) | DocxWorkflowError::EmbeddedImageTaskMismatch(_) => {
            "REVIEW_DATA_INVALID"
        }
        DocxWorkflowError::TextDetection(error) => text_export_error_code(error),
        DocxWorkflowError::EmbeddedImage(error) => image_export_error_code(error),
        DocxWorkflowError::Io(_) | DocxWorkflowError::Zip(_) => "OUTPUT_WRITE_FAILED",
        _ => "DOCX_EXPORT_FAILED",
    }
}

fn xlsx_export_error_code(error: &XlsxWorkflowError) -> &'static str {
    match error {
        XlsxWorkflowError::OutputExists(_) => "OUTPUT_EXISTS",
        XlsxWorkflowError::WouldOverwriteSource => "OUTPUT_CONFLICT",
        XlsxWorkflowError::OutputTypeMismatch => "OUTPUT_TYPE_INVALID",
        XlsxWorkflowError::UnreviewedFindings(_) => "REVIEW_REQUIRED",
        XlsxWorkflowError::VerificationFailed(_) => "VERIFICATION_FAILED",
        XlsxWorkflowError::SourceChanged => "SOURCE_CHANGED",
        XlsxWorkflowError::EmbeddedImagesUnsupported
        | XlsxWorkflowError::EmbeddedImageRuntimeRequired => "OCR_RUNTIME_MISSING",
        XlsxWorkflowError::UnsupportedEmbeddedImageType(_) => "XLSX_EMBEDDED_IMAGE_UNSUPPORTED",
        XlsxWorkflowError::UnsupportedPayload(_) => "XLSX_UNSUPPORTED_PAYLOAD",
        XlsxWorkflowError::SheetRenameUnsupported(_) => "XLSX_SHEET_RENAME_UNSUPPORTED",
        XlsxWorkflowError::InvalidPolicy(_) | XlsxWorkflowError::PolicySnapshotMismatch => {
            "POLICY_INVALID"
        }
        XlsxWorkflowError::InvalidFinding(_)
        | XlsxWorkflowError::EmbeddedImageTaskMismatch(_)
        | XlsxWorkflowError::ConflictingFormulaReplacement(_) => "REVIEW_DATA_INVALID",
        XlsxWorkflowError::Text(error) => text_export_error_code(error),
        XlsxWorkflowError::EmbeddedImage(error) => image_export_error_code(error),
        XlsxWorkflowError::Io(_) | XlsxWorkflowError::Zip(_) => "OUTPUT_WRITE_FAILED",
        _ => "XLSX_EXPORT_FAILED",
    }
}

fn pptx_export_error_code(error: &PptxWorkflowError) -> &'static str {
    match error {
        PptxWorkflowError::OutputExists(_) => "OUTPUT_EXISTS",
        PptxWorkflowError::WouldOverwriteSource => "OUTPUT_CONFLICT",
        PptxWorkflowError::OutputTypeMismatch => "OUTPUT_TYPE_INVALID",
        PptxWorkflowError::UnreviewedFindings(_) => "REVIEW_REQUIRED",
        PptxWorkflowError::VerificationFailed(_) => "VERIFICATION_FAILED",
        PptxWorkflowError::SourceChanged => "SOURCE_CHANGED",
        PptxWorkflowError::EmbeddedImagesUnsupported
        | PptxWorkflowError::EmbeddedImageRuntimeRequired => "OCR_RUNTIME_MISSING",
        PptxWorkflowError::UnsupportedEmbeddedImageType(_) => "PPTX_EMBEDDED_IMAGE_UNSUPPORTED",
        PptxWorkflowError::EmbeddedObjectsUnsupported => "PPTX_EMBEDDED_OBJECTS_UNSUPPORTED",
        PptxWorkflowError::ActiveContentUnsupported => "PPTX_ACTIVE_CONTENT_UNSUPPORTED",
        PptxWorkflowError::ExternalRelationshipUnsupported(_) => {
            "PPTX_EXTERNAL_RELATIONSHIP_UNSUPPORTED"
        }
        PptxWorkflowError::InvalidPolicy(_) | PptxWorkflowError::PolicySnapshotMismatch => {
            "POLICY_INVALID"
        }
        PptxWorkflowError::InvalidFinding(_) | PptxWorkflowError::EmbeddedImageTaskMismatch(_) => {
            "REVIEW_DATA_INVALID"
        }
        PptxWorkflowError::TextDetection(error) => text_export_error_code(error),
        PptxWorkflowError::EmbeddedImage(error) => image_export_error_code(error),
        PptxWorkflowError::Io(_) | PptxWorkflowError::Zip(_) => "OUTPUT_WRITE_FAILED",
        _ => "PPTX_EXPORT_FAILED",
    }
}

fn image_export_error_code(error: &ImageWorkflowError) -> &'static str {
    match error {
        ImageWorkflowError::OutputExists(_) => "OUTPUT_EXISTS",
        ImageWorkflowError::WouldOverwriteSource => "OUTPUT_CONFLICT",
        ImageWorkflowError::OutputTypeMismatch => "OUTPUT_TYPE_INVALID",
        ImageWorkflowError::UnreviewedFindings(_) => "REVIEW_REQUIRED",
        ImageWorkflowError::VerificationFailed(_) => "VERIFICATION_FAILED",
        ImageWorkflowError::SourceChanged => "SOURCE_CHANGED",
        ImageWorkflowError::OcrRuntimeUnavailable(_) => "OCR_RUNTIME_MISSING",
        ImageWorkflowError::Ocr(_) => "OCR_FAILED",
        ImageWorkflowError::InvalidPolicy(_) | ImageWorkflowError::PolicySnapshotMismatch => {
            "POLICY_INVALID"
        }
        ImageWorkflowError::InvalidMaskRect(_)
        | ImageWorkflowError::FindingNotFound(_)
        | ImageWorkflowError::GroupNotFound(_)
        | ImageWorkflowError::DetectedGroupCannotBeRemoved(_) => "REVIEW_DATA_INVALID",
        ImageWorkflowError::Io(_) | ImageWorkflowError::Image(_) => "OUTPUT_WRITE_FAILED",
        _ => "IMAGE_EXPORT_FAILED",
    }
}

fn pdf_error_code(error: &PdfWorkflowError) -> &'static str {
    match error {
        PdfWorkflowError::PdfTooLarge
        | PdfWorkflowError::TooManyPages
        | PdfWorkflowError::RenderedPixelsTooLarge => "PDF_LIMIT_EXCEEDED",
        PdfWorkflowError::InvalidPdf | PdfWorkflowError::InvalidToolOutput => "INVALID_PDF",
        PdfWorkflowError::EncryptedPdfUnsupported => "ENCRYPTED_PDF_UNSUPPORTED",
        PdfWorkflowError::ToolUnavailable(_) => "PDF_TOOLS_MISSING",
        PdfWorkflowError::ToolFailed(_)
        | PdfWorkflowError::ToolTimeout(_)
        | PdfWorkflowError::ToolOutputTooLarge(_) => "PDF_TOOL_FAILED",
        PdfWorkflowError::InvalidPolicy(_) => "POLICY_INVALID",
        PdfWorkflowError::ImageWorkflow(error) => image_error_code(error),
        PdfWorkflowError::Io(_) | PdfWorkflowError::Image(_) => "PDF_READ_FAILED",
        PdfWorkflowError::ScanCancelled => "SCAN_CANCELLED",
        _ => "PDF_SCAN_FAILED",
    }
}

fn pdf_export_error_code(error: &PdfWorkflowError) -> &'static str {
    match error {
        PdfWorkflowError::OutputExists(_) => "OUTPUT_EXISTS",
        PdfWorkflowError::WouldOverwriteSource => "OUTPUT_CONFLICT",
        PdfWorkflowError::OutputTypeMismatch => "OUTPUT_TYPE_INVALID",
        PdfWorkflowError::UnreviewedFindings(_) => "REVIEW_REQUIRED",
        PdfWorkflowError::VerificationFailed(_) | PdfWorkflowError::OutputPageMismatch => {
            "VERIFICATION_FAILED"
        }
        PdfWorkflowError::SourceChanged => "SOURCE_CHANGED",
        PdfWorkflowError::ToolUnavailable(_) => "PDF_TOOLS_MISSING",
        PdfWorkflowError::ToolFailed(_)
        | PdfWorkflowError::ToolTimeout(_)
        | PdfWorkflowError::ToolOutputTooLarge(_) => "PDF_TOOL_FAILED",
        PdfWorkflowError::ImageWorkflow(error) => image_export_error_code(error),
        PdfWorkflowError::InvalidPolicy(_) | PdfWorkflowError::PolicySnapshotMismatch => {
            "POLICY_INVALID"
        }
        PdfWorkflowError::Io(_) | PdfWorkflowError::Image(_) => "OUTPUT_WRITE_FAILED",
        _ => "PDF_EXPORT_FAILED",
    }
}

fn inspect_file(id: String, display_name: String, path: &Path, size_bytes: u64) -> ImportedFile {
    let extension = normalized_extension(path);
    let (kind, limit) = extension_profile(&extension);
    let (ready, scan_supported, reason_code) = if kind == DesktopFileKind::Unknown {
        (false, false, "UNSUPPORTED_EXTENSION")
    } else if size_bytes > limit {
        (false, false, "FILE_TOO_LARGE")
    } else if matches!(
        kind,
        DesktopFileKind::Text
            | DesktopFileKind::Word
            | DesktopFileKind::Spreadsheet
            | DesktopFileKind::Presentation
            | DesktopFileKind::Pdf
            | DesktopFileKind::Image
    ) {
        (true, true, "READY")
    } else {
        (true, false, "SCAN_NOT_AVAILABLE")
    };
    ImportedFile {
        id,
        display_name,
        extension,
        kind,
        source_kind: DesktopSourceKind::File,
        size_bytes,
        ready,
        scan_supported,
        reason_code: reason_code.to_owned(),
        duplicate: false,
    }
}

fn unreadable_file(id: String, display_name: String, path: &Path) -> ImportedFile {
    ImportedFile {
        id,
        display_name,
        extension: normalized_extension(path),
        kind: DesktopFileKind::Unknown,
        source_kind: DesktopSourceKind::File,
        size_bytes: 0,
        ready: false,
        scan_supported: false,
        reason_code: "FILE_UNREADABLE".to_owned(),
        duplicate: false,
    }
}

fn clipboard_imported_file(id: String, size_bytes: u64, duplicate: bool) -> ImportedFile {
    ImportedFile {
        id,
        display_name: "剪贴板文本".to_owned(),
        extension: "txt".to_owned(),
        kind: DesktopFileKind::Text,
        source_kind: DesktopSourceKind::Clipboard,
        size_bytes,
        ready: true,
        scan_supported: true,
        reason_code: if duplicate {
            "ALREADY_IMPORTED"
        } else {
            "READY"
        }
        .to_owned(),
        duplicate,
    }
}

fn registered_source_path(source: &RegisteredSource) -> Option<&Path> {
    match source {
        RegisteredSource::File(path) => Some(path),
        RegisteredSource::Clipboard(_) => None,
    }
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("未命名文件")
        .to_owned()
}

fn normalized_extension(path: &Path) -> String {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default()
}

fn extension_profile(extension: &str) -> (DesktopFileKind, u64) {
    match extension {
        "txt" | "md" => (DesktopFileKind::Text, MAX_TEXT_BYTES),
        "docx" => (DesktopFileKind::Word, MAX_DOCUMENT_BYTES),
        "xlsx" => (DesktopFileKind::Spreadsheet, MAX_DOCUMENT_BYTES),
        "pptx" => (DesktopFileKind::Presentation, MAX_DOCUMENT_BYTES),
        "pdf" => (DesktopFileKind::Pdf, MAX_DOCUMENT_BYTES),
        "png" | "jpg" | "jpeg" => (DesktopFileKind::Image, MAX_IMAGE_BYTES),
        _ => (DesktopFileKind::Unknown, 0),
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(DesktopState::default())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_dialog::init())
        .on_window_event(|window, event| {
            let WindowEvent::DragDrop(DragDropEvent::Drop { paths, .. }) = event else {
                return;
            };
            let result = register_files(window.state::<DesktopState>().inner(), paths.clone());
            let payload = match result {
                Ok(files) => DesktopImportEvent { files, error: None },
                Err(error) => DesktopImportEvent {
                    files: Vec::new(),
                    error: Some(error),
                },
            };
            let _ = window.emit(FILES_IMPORTED_EVENT, payload);
        })
        .invoke_handler(tauri::generate_handler![
            desktop_capabilities,
            desktop_runtime_status,
            pick_files,
            import_clipboard_text,
            remove_registered_file,
            clear_registered_files,
            scan_task_summaries,
            start_registered_scans,
            cancel_scan,
            review_scan_page,
            set_review_group,
            update_review_mask,
            add_review_mask,
            remove_review_mask,
            review_text_scan,
            set_text_review_finding,
            choose_and_export_scan,
            choose_and_export_batch,
            copy_redacted_clipboard
        ])
        .run(tauri::generate_context!())
        .expect("LlaMask desktop runtime failed");
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use tempfile::tempdir;

    use llamask_core::model::{DocumentPart, EntityType, ImageFinding, ImageRect};
    use llamask_core::{
        PolicyConfig, scan_docx_with_policy, scan_pptx_with_policy, scan_text_with_policy,
        scan_xlsx_with_policy,
    };

    use super::{
        DesktopBatchExportSummary, DesktopFileKind, DesktopOutputKind, DesktopScanStage,
        DesktopScanStatus, DesktopSourceKind, DesktopState, MAX_TEXT_BYTES, ScanRecord,
        batch_export_status_is_eligible, batch_output_path, docx_task_metrics,
        docx_text_review_findings, export_name_and_extension, inspect_file, pptx_task_metrics,
        pptx_text_review_findings, register_clipboard_text, register_files, review_findings,
        text_review_findings, xlsx_review_presentation, xlsx_task_metrics,
        xlsx_text_review_findings,
    };

    fn docx_fixture_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../fixtures/docx/comprehensive.docx")
    }

    fn xlsx_fixture_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../fixtures/xlsx/comprehensive.xlsx")
    }

    fn pptx_fixture_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../fixtures/pptx/comprehensive.pptx")
    }

    #[test]
    fn classifies_supported_pdf_without_returning_its_path() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("synthetic.pdf");
        fs::write(&path, b"%PDF-1.4\n").unwrap();
        let candidate = inspect_file("file-1".to_owned(), "synthetic.pdf".to_owned(), &path, 9);
        assert_eq!(candidate.kind, DesktopFileKind::Pdf);
        assert!(candidate.ready);
        assert!(candidate.scan_supported);
        assert_eq!(candidate.reason_code, "READY");
    }

    #[test]
    fn classifies_text_as_scannable_without_ocr() {
        let candidate = inspect_file(
            "file-2".to_owned(),
            "notes.txt".to_owned(),
            Path::new("notes.txt"),
            12,
        );
        assert_eq!(candidate.kind, DesktopFileKind::Text);
        assert!(candidate.ready);
        assert!(candidate.scan_supported);
        assert_eq!(candidate.reason_code, "READY");
    }

    #[test]
    fn classifies_docx_as_scannable_without_exposing_its_path() {
        let candidate = inspect_file(
            "file-3".to_owned(),
            "proposal.docx".to_owned(),
            Path::new("/private/customer/proposal.docx"),
            512,
        );
        let json = serde_json::to_string(&candidate).unwrap();

        assert_eq!(candidate.kind, DesktopFileKind::Word);
        assert!(candidate.ready);
        assert!(candidate.scan_supported);
        assert_eq!(candidate.reason_code, "READY");
        assert!(!json.contains("/private/customer"));
    }

    #[test]
    fn classifies_xlsx_as_scannable_without_exposing_its_path() {
        let candidate = inspect_file(
            "file-4".to_owned(),
            "finance.xlsx".to_owned(),
            Path::new("/private/customer/finance.xlsx"),
            1024,
        );
        let json = serde_json::to_string(&candidate).unwrap();

        assert_eq!(candidate.kind, DesktopFileKind::Spreadsheet);
        assert!(candidate.ready);
        assert!(candidate.scan_supported);
        assert_eq!(candidate.reason_code, "READY");
        assert!(!json.contains("/private/customer"));
    }

    #[test]
    fn classifies_pptx_as_scannable_without_exposing_its_path() {
        let candidate = inspect_file(
            "file-5".to_owned(),
            "briefing.pptx".to_owned(),
            Path::new("/private/customer/briefing.pptx"),
            2048,
        );
        let json = serde_json::to_string(&candidate).unwrap();

        assert_eq!(candidate.kind, DesktopFileKind::Presentation);
        assert!(candidate.ready);
        assert!(candidate.scan_supported);
        assert_eq!(candidate.reason_code, "READY");
        assert!(!json.contains("/private/customer"));
    }

    #[test]
    fn rejects_unknown_extensions_before_scanning() {
        let candidate = inspect_file(
            "file-3".to_owned(),
            "archive.bin".to_owned(),
            Path::new("archive.bin"),
            12,
        );
        assert_eq!(candidate.kind, DesktopFileKind::Unknown);
        assert!(!candidate.ready);
        assert!(!candidate.scan_supported);
        assert_eq!(candidate.reason_code, "UNSUPPORTED_EXTENSION");
    }

    #[test]
    fn registry_reuses_an_opaque_id_for_duplicate_paths() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("customer.txt");
        fs::write(&path, b"local only").unwrap();
        let state = DesktopState::default();

        let first = register_files(&state, vec![path.clone()]).unwrap();
        let duplicate = register_files(&state, vec![path]).unwrap();

        assert_eq!(first[0].id, "file-00000001");
        assert!(!first[0].duplicate);
        assert_eq!(duplicate[0].id, first[0].id);
        assert!(duplicate[0].duplicate);
        assert_eq!(duplicate[0].reason_code, "ALREADY_IMPORTED");
    }

    #[test]
    fn clipboard_registration_is_memory_only_and_deduplicated() {
        let state = DesktopState::default();
        let sensitive_text = "联系人张三，邮箱 case@example.com".to_owned();

        let first = register_clipboard_text(&state, sensitive_text.clone()).unwrap();
        let duplicate = register_clipboard_text(&state, sensitive_text.clone()).unwrap();
        let json = serde_json::to_string(&first).unwrap();

        assert_eq!(first.id, "clipboard-00000001");
        assert_eq!(first.source_kind, DesktopSourceKind::Clipboard);
        assert!(!first.duplicate);
        assert_eq!(duplicate.id, first.id);
        assert!(duplicate.duplicate);
        assert!(!json.contains(&sensitive_text));
        assert!(!json.contains("case@example.com"));
        assert!(!json.contains("sourcePath"));
    }

    #[test]
    fn clipboard_registration_rejects_empty_and_oversized_text() {
        let state = DesktopState::default();

        let empty = register_clipboard_text(&state, " \n\t".to_owned()).unwrap_err();
        let oversized =
            register_clipboard_text(&state, "x".repeat(MAX_TEXT_BYTES as usize + 1)).unwrap_err();

        assert_eq!(empty.code, "CLIPBOARD_EMPTY");
        assert_eq!(oversized.code, "CLIPBOARD_TOO_LARGE");
        assert!(state.files.lock().unwrap().is_empty());
    }

    #[test]
    fn webview_capability_does_not_grant_direct_clipboard_access() {
        let capability = include_str!("../capabilities/main-capability.json");
        let value: serde_json::Value = serde_json::from_str(capability).unwrap();

        assert_eq!(value["permissions"], serde_json::json!(["core:default"]));
        assert!(!capability.contains("clipboard-manager:allow"));
    }

    #[test]
    fn clipboard_export_summary_exposes_only_its_destination_kind() {
        let mut record = ScanRecord::queued(Arc::new(AtomicBool::new(false)));
        record.finish_clipboard_export(true);
        let summary = record.summary("clipboard-00000042");
        let json = serde_json::to_string(&summary).unwrap();

        assert_eq!(summary.status, DesktopScanStatus::Complete);
        assert_eq!(summary.output_kind, Some(DesktopOutputKind::Clipboard));
        assert!(summary.output_name.is_none());
        assert!(summary.verification_complete);
        assert!(!json.contains("matchedText"));
    }

    #[test]
    fn cancellation_summary_contains_no_path_or_sensitive_text() {
        let cancel_requested = Arc::new(AtomicBool::new(false));
        let mut record = ScanRecord::queued(cancel_requested.clone());
        cancel_requested.store(true, Ordering::Release);
        record.cancel();
        let summary = record.summary("file-00000042");
        let json = serde_json::to_string(&summary).unwrap();

        assert_eq!(summary.status, DesktopScanStatus::Cancelled);
        assert_eq!(summary.stage, DesktopScanStage::Cancelled);
        assert!(!summary.can_cancel);
        assert!(!json.contains("/Users/"));
        assert!(!json.contains("matched_text"));
        assert!(!json.contains("recognized_line"));
    }

    #[test]
    fn review_payload_exposes_geometry_but_not_ocr_plaintext() {
        let findings = review_findings(&[ImageFinding {
            id: "finding-1".to_owned(),
            group_id: "group-1".to_owned(),
            line_index: 0,
            text_start: 0,
            text_end: 6,
            entity_type: EntityType::PersonName,
            matched_text: "敏感姓名".to_owned(),
            line_fragment: "敏感姓名".to_owned(),
            recognized_line: "敏感姓名在这里".to_owned(),
            detector: "test".to_owned(),
            confidence: 0.98,
            ocr_confidence: 0.99,
            explanation_code: "TEST".to_owned(),
            ocr_rect: ImageRect {
                x0: 1,
                y0: 2,
                x1: 30,
                y1: 20,
            },
            mask_rect: ImageRect {
                x0: 0,
                y0: 1,
                x1: 32,
                y1: 22,
            },
            manual: false,
            selected: true,
            reviewed: false,
        }]);
        let json = serde_json::to_string(&findings).unwrap();

        assert!(json.contains("maskRect"));
        assert!(json.contains("PERSON_NAME"));
        assert!(!json.contains("敏感姓名"));
        assert!(!json.contains("recognizedLine"));
        assert!(!json.contains("matchedText"));
    }

    #[test]
    fn text_review_payload_is_bounded_and_omits_the_source_path() {
        let prefix = "前".repeat(120);
        let suffix = "后".repeat(120);
        let task = scan_text_with_policy(
            format!("{prefix}邮箱 case@example.com{suffix}"),
            &PolicyConfig::default(),
            None,
        )
        .unwrap();
        let findings = text_review_findings(&task).unwrap();
        let json = serde_json::to_string(&findings).unwrap();

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].context_before.chars().count(), 80);
        assert_eq!(findings[0].context_after.chars().count(), 80);
        assert_eq!(findings[0].matched_text, "case@example.com");
        assert!(findings[0].can_apply);
        assert!(findings[0].review_note.is_none());
        assert!(!json.contains("stdin://clipboard"));
        assert!(!json.contains("sourcePath"));
    }

    #[test]
    fn docx_review_payload_uses_safe_section_labels_without_ooxml_locators() {
        let mut policy = PolicyConfig::default();
        policy.detectors.clear();
        let task = scan_docx_with_policy(&docx_fixture_path(), &policy, None).unwrap();
        let findings = docx_text_review_findings(&task).unwrap();
        let metrics = docx_task_metrics(&task);
        let json = serde_json::to_string(&findings).unwrap();

        assert_eq!(metrics.finding_groups, findings.len());
        assert_eq!(metrics.page_count, 0);
        assert!(
            findings
                .iter()
                .all(|finding| finding.section_label.is_some())
        );
        assert!(json.contains("正文 · 内容 1"));
        assert!(!json.contains("word/document.xml"));
        assert!(!json.contains("comprehensive.docx"));
        assert!(!json.contains("sourcePath"));
    }

    #[test]
    fn xlsx_review_payload_uses_cell_labels_without_package_locators() {
        let mut policy = PolicyConfig::default();
        policy.detectors.clear();
        let task = scan_xlsx_with_policy(&xlsx_fixture_path(), &policy, None, None).unwrap();
        let findings = xlsx_text_review_findings(&task).unwrap();
        let metrics = xlsx_task_metrics(&task);
        let json = serde_json::to_string(&findings).unwrap();

        assert_eq!(metrics.finding_groups, findings.len());
        assert_eq!(metrics.page_count, 0);
        assert!(
            findings
                .iter()
                .all(|finding| finding.section_label.is_some())
        );
        assert!(findings.iter().any(|finding| {
            finding
                .section_label
                .as_deref()
                .is_some_and(|label| label.starts_with("工作表 1 · 单元格"))
        }));
        assert!(findings.iter().any(|finding| {
            finding
                .review_note
                .as_deref()
                .is_some_and(|note| note.contains("公式和缓存"))
        }));
        assert!(!json.contains("xl/worksheets"));
        assert!(!json.contains("#cell="));
        assert!(!json.contains("comprehensive.xlsx"));
        assert!(!json.contains("sourcePath"));
    }

    #[test]
    fn xlsx_sheet_name_review_is_safe_and_retain_only() {
        let part = DocumentPart {
            id: "part-sheet-name".to_owned(),
            kind: "sheet_name".to_owned(),
            locator: "xl/workbook.xml#sheet=000002#name".to_owned(),
            text: "客户机密项目".to_owned(),
            char_len: 6,
        };

        let (label, can_apply, note) = xlsx_review_presentation(&part, 1);

        assert_eq!(label.as_deref(), Some("工作表 2 · 名称"));
        assert!(!can_apply);
        assert!(note.is_some_and(|note| note.contains("只能明确保留")));
        assert!(!label.unwrap().contains("客户机密项目"));
    }

    #[test]
    fn pptx_review_payload_uses_safe_story_labels_without_package_locators() {
        let mut policy = PolicyConfig::default();
        policy.detectors.clear();
        let task = scan_pptx_with_policy(&pptx_fixture_path(), &policy, None).unwrap();
        let findings = pptx_text_review_findings(&task).unwrap();
        let metrics = pptx_task_metrics(&task);
        let json = serde_json::to_string(&findings).unwrap();

        assert_eq!(metrics.finding_groups, findings.len());
        assert_eq!(metrics.page_count, 0);
        assert!(
            findings
                .iter()
                .all(|finding| finding.section_label.is_some() && finding.can_apply)
        );
        assert!(findings.iter().any(|finding| {
            finding
                .section_label
                .as_deref()
                .is_some_and(|label| label.starts_with("幻灯片 1 · 段落"))
        }));
        assert!(findings.iter().any(|finding| {
            finding
                .review_note
                .as_deref()
                .is_some_and(|note| note.contains("母版或版式"))
        }));
        assert!(!json.contains("ppt/slides"));
        assert!(!json.contains("ppt/slideMasters"));
        assert!(!json.contains("#p"));
        assert!(!json.contains("comprehensive.pptx"));
        assert!(!json.contains("sourcePath"));
    }

    #[test]
    fn export_name_preserves_the_original_extension() {
        let (name, extension) = export_name_and_extension(Path::new("report.final.pdf")).unwrap();
        assert_eq!(name, "report.final_redacted.pdf");
        assert_eq!(extension, "pdf");
    }

    #[test]
    fn batch_output_names_never_overwrite_or_collide() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("report_redacted.pdf"), b"existing").unwrap();
        let mut reserved = HashSet::new();

        let (first_path, first_name) = batch_output_path(
            directory.path(),
            Path::new("/source-a/report.pdf"),
            &mut reserved,
        )
        .unwrap();
        let (second_path, second_name) = batch_output_path(
            directory.path(),
            Path::new("/source-b/report.pdf"),
            &mut reserved,
        )
        .unwrap();

        assert_eq!(first_name, "report_redacted_2.pdf");
        assert_eq!(second_name, "report_redacted_3.pdf");
        assert_ne!(first_path, second_path);
        assert!(!first_path.exists());
        assert!(!second_path.exists());
    }

    #[test]
    fn batch_export_never_silently_accepts_unreviewed_tasks() {
        assert!(batch_export_status_is_eligible(
            DesktopScanStatus::ReadyToExport
        ));
        assert!(batch_export_status_is_eligible(
            DesktopScanStatus::ExportFailed
        ));
        assert!(batch_export_status_is_eligible(DesktopScanStatus::Complete));
        assert!(!batch_export_status_is_eligible(
            DesktopScanStatus::ReviewRequired
        ));
        assert!(!batch_export_status_is_eligible(DesktopScanStatus::Blocked));
        assert!(!batch_export_status_is_eligible(
            DesktopScanStatus::Cancelled
        ));
    }

    #[test]
    fn batch_export_summary_contains_only_aggregate_counts() {
        let summary = DesktopBatchExportSummary {
            attempted: 4,
            succeeded: 2,
            failed: 2,
            skipped: 1,
            complete_verifications: 1,
            basic_verifications: 1,
        };
        let json = serde_json::to_string(&summary).unwrap();

        assert_eq!(
            json,
            r#"{"attempted":4,"succeeded":2,"failed":2,"skipped":1,"completeVerifications":1,"basicVerifications":1}"#
        );
        assert!(!json.contains("path"));
        assert!(!json.contains("name"));
        assert!(!json.contains("finding"));
    }
}
