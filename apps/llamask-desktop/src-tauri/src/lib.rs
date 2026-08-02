use std::collections::BTreeMap;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use llamask_core::model::{EntityType, ImageFinding};
use llamask_core::sidecar::DetectorKind;
use llamask_core::{
    ImageRect, ImageTaskDraft, ImageWorkflowError, PdfTaskDraft, PdfWorkflowError, PolicyConfig,
    RuntimeRegistry, add_manual_image_mask, export_image_task_with_runtimes,
    export_pdf_task_with_runtimes, remove_manual_image_group, render_image_task_preview,
    render_pdf_task_page_preview, review_image_group, scan_image_with_policy,
    scan_pdf_with_policy_and_progress, update_image_mask,
};
use serde::Serialize;
use tauri::{AppHandle, DragDropEvent, Emitter, Manager, State, WindowEvent};
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
const RUNTIME_REGISTRY_ENV: &str = "LLAMASK_RUNTIME_REGISTRY";

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
    path: PathBuf,
    kind: DesktopFileKind,
    ready: bool,
    scan_supported: bool,
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

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct ImportedFile {
    id: String,
    display_name: String,
    extension: String,
    kind: DesktopFileKind,
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

struct ScanCandidate {
    id: String,
    path: PathBuf,
    kind: DesktopFileKind,
    cancel_requested: Arc<AtomicBool>,
}

#[derive(Clone)]
enum StoredScanTask {
    Image(Box<ImageTaskDraft>),
    Pdf(Box<PdfTaskDraft>),
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
            Self::Image(task) => image_task_metrics(task),
            Self::Pdf(task) => pdf_task_metrics(task),
        }
    }

    fn page_count(&self) -> usize {
        match self {
            Self::Image(_) => 1,
            Self::Pdf(task) => task.pages.len(),
        }
    }

    fn page_task(&self, page_number: usize) -> Option<&ImageTaskDraft> {
        match self {
            Self::Image(task) if page_number == 1 => Some(task),
            Self::Pdf(task) => task
                .pages
                .iter()
                .find(|page| page.page_number == page_number)
                .map(|page| &page.task),
            _ => None,
        }
    }

    fn page_task_mut(&mut self, page_number: usize) -> Option<&mut ImageTaskDraft> {
        match self {
            Self::Image(task) if page_number == 1 => Some(task),
            Self::Pdf(task) => task
                .pages
                .iter_mut()
                .find(|page| page.page_number == page_number)
                .map(|page| &mut page.task),
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
        self.verification_complete = false;
    }

    fn begin_export(&mut self) {
        self.status = DesktopScanStatus::Exporting;
        self.stage = DesktopScanStage::Exporting;
        self.error_code = None;
        self.output_name = None;
        self.verification_complete = false;
    }

    fn finish_export(&mut self, output_name: String, verification_complete: bool) {
        self.status = DesktopScanStatus::Complete;
        self.stage = DesktopScanStage::Complete;
        self.error_code = None;
        self.output_name = Some(output_name);
        self.verification_complete = verification_complete;
    }

    fn fail_export(&mut self, code: &'static str) {
        self.status = DesktopScanStatus::ExportFailed;
        self.stage = DesktopScanStage::Failed;
        self.error_code = Some(code);
        self.output_name = None;
        self.verification_complete = false;
    }

    fn cancel(&mut self) {
        self.status = DesktopScanStatus::Cancelled;
        self.stage = DesktopScanStage::Cancelled;
        self.task = None;
        self.error_code = None;
        self.output_name = None;
        self.verification_complete = false;
    }

    fn block(&mut self, code: &'static str) {
        self.status = DesktopScanStatus::Blocked;
        self.stage = DesktopScanStage::Blocked;
        self.task = None;
        self.error_code = Some(code);
        self.output_name = None;
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
        scan_extensions: vec!["pdf", "png", "jpg", "jpeg"],
        milestone: "image-pdf-review-export",
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
            .find(|(_, registered)| registered.path == canonical)
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
                path: canonical,
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
        .map(|(id, file)| (id.clone(), file.path.clone(), file.kind))
        .collect::<Vec<_>>();
    drop(files);

    let mut scans = state.scans.lock().map_err(|_| {
        state.worker_running.store(false, Ordering::Release);
        DesktopCommandError::new("TASK_STATE_UNAVAILABLE", "本地任务状态暂时不可用。")
    })?;
    let mut candidates = Vec::new();
    for (id, path, kind) in candidate_files {
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
            path,
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
            "请先导入 PDF、PNG 或 JPEG 文件。",
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
    let (default_name, extension) = {
        let files = state.files.lock().map_err(|_| task_state_error())?;
        let file = files.get(&id).ok_or_else(scan_not_found_error)?;
        export_name_and_extension(&file.path)?
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
        Ok(context) => context,
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
        let result = tauri::async_runtime::spawn_blocking(move || match &task {
            StoredScanTask::Image(task) => {
                export_image_task_with_runtimes(task, &output, &context.registry)
                    .map(|report| report.complete)
                    .map_err(|error| image_export_error_code(&error))
            }
            StoredScanTask::Pdf(task) => {
                export_pdf_task_with_runtimes(task, &output, &context.registry)
                    .map(|report| report.complete)
                    .map_err(|error| pdf_export_error_code(&error))
            }
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

fn run_scan_batch(app: AppHandle, candidates: Vec<ScanCandidate>) {
    let context = match runtime_context(&app, app.state::<DesktopState>().inner()) {
        Ok(context) => context,
        Err(failure) => {
            for candidate in candidates {
                if candidate.cancel_requested.load(Ordering::Acquire) {
                    update_scan(&app, &candidate.id, ScanRecord::cancel);
                } else {
                    update_scan(&app, &candidate.id, |record| record.block(failure.code));
                }
            }
            return;
        }
    };
    let policy = PolicyConfig::default();

    for candidate in candidates {
        if candidate.cancel_requested.load(Ordering::Acquire) {
            update_scan(&app, &candidate.id, ScanRecord::cancel);
            continue;
        }
        if candidate.kind == DesktopFileKind::Pdf && !context.pdf_tools_ready {
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
            record.verification_complete = false;
        });

        match candidate.kind {
            DesktopFileKind::Image => scan_image_candidate(&app, &candidate, &context, &policy),
            DesktopFileKind::Pdf => scan_pdf_candidate(&app, &candidate, &context, &policy),
            _ => update_scan(&app, &candidate.id, |record| {
                record.block("SCAN_FORMAT_NOT_AVAILABLE")
            }),
        }
    }
}

fn scan_image_candidate(
    app: &AppHandle,
    candidate: &ScanCandidate,
    context: &RuntimeContext,
    policy: &PolicyConfig,
) {
    update_scan(app, &candidate.id, |record| {
        record.stage = DesktopScanStage::Ocr;
        record.total_units = 1;
    });
    let result = scan_image_with_policy(
        &candidate.path,
        policy,
        &context.registry,
        &context.ocr_runtime_id,
    );
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
    let progress_app = app.clone();
    let progress_id = candidate.id.clone();
    let cancel_requested = candidate.cancel_requested.clone();
    let result = scan_pdf_with_policy_and_progress(
        &candidate.path,
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
    } else if matches!(kind, DesktopFileKind::Pdf | DesktopFileKind::Image) {
        (true, true, "READY")
    } else {
        (true, false, "SCAN_NOT_AVAILABLE")
    };
    ImportedFile {
        id,
        display_name,
        extension,
        kind,
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
        size_bytes: 0,
        ready: false,
        scan_supported: false,
        reason_code: "FILE_UNREADABLE".to_owned(),
        duplicate: false,
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
            choose_and_export_scan
        ])
        .run(tauri::generate_context!())
        .expect("LlaMask desktop runtime failed");
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use tempfile::tempdir;

    use llamask_core::model::{EntityType, ImageFinding, ImageRect};

    use super::{
        DesktopFileKind, DesktopScanStage, DesktopScanStatus, DesktopState, ScanRecord,
        export_name_and_extension, inspect_file, register_files, review_findings,
    };

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
    fn keeps_future_formats_importable_but_not_scannable() {
        let candidate = inspect_file(
            "file-2".to_owned(),
            "notes.txt".to_owned(),
            Path::new("notes.txt"),
            12,
        );
        assert_eq!(candidate.kind, DesktopFileKind::Text);
        assert!(candidate.ready);
        assert!(!candidate.scan_supported);
        assert_eq!(candidate.reason_code, "SCAN_NOT_AVAILABLE");
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
    fn export_name_preserves_the_original_extension() {
        let (name, extension) = export_name_and_extension(Path::new("report.final.pdf")).unwrap();
        assert_eq!(name, "report.final_redacted.pdf");
        assert_eq!(extension, "pdf");
    }
}
