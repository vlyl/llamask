use std::collections::BTreeMap;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use llamask_core::sidecar::DetectorKind;
use llamask_core::{
    ImageTaskDraft, ImageWorkflowError, PdfTaskDraft, PdfWorkflowError, PolicyConfig,
    RuntimeRegistry, scan_image_with_policy, scan_pdf_with_policy_and_progress,
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
    batch_running: AtomicBool,
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
}

struct ScanCandidate {
    id: String,
    path: PathBuf,
    kind: DesktopFileKind,
    cancel_requested: Arc<AtomicBool>,
}

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
    }

    fn cancel(&mut self) {
        self.status = DesktopScanStatus::Cancelled;
        self.stage = DesktopScanStage::Cancelled;
        self.task = None;
        self.error_code = None;
    }

    fn block(&mut self, code: &'static str) {
        self.status = DesktopScanStatus::Blocked;
        self.stage = DesktopScanStage::Blocked;
        self.task = None;
        self.error_code = Some(code);
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
        milestone: "image-pdf-background-scan",
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
    if let Some(record) = state
        .scans
        .lock()
        .map_err(|_| {
            DesktopCommandError::new("TASK_STATE_UNAVAILABLE", "本地任务状态暂时不可用。")
        })?
        .remove(&id)
    {
        record.cancel_requested.store(true, Ordering::Release);
    }
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
    if state.batch_running.swap(true, Ordering::AcqRel) {
        return Err(DesktopCommandError::new(
            "SCAN_BATCH_ACTIVE",
            "当前扫描批次尚未结束。",
        ));
    }

    let files = state.files.lock().map_err(|_| {
        state.batch_running.store(false, Ordering::Release);
        DesktopCommandError::new("TASK_STATE_UNAVAILABLE", "本地任务状态暂时不可用。")
    })?;
    let candidate_files = files
        .iter()
        .filter(|(_, file)| file.ready && file.scan_supported)
        .map(|(id, file)| (id.clone(), file.path.clone(), file.kind))
        .collect::<Vec<_>>();
    drop(files);

    let mut scans = state.scans.lock().map_err(|_| {
        state.batch_running.store(false, Ordering::Release);
        DesktopCommandError::new("TASK_STATE_UNAVAILABLE", "本地任务状态暂时不可用。")
    })?;
    let mut candidates = Vec::new();
    for (id, path, kind) in candidate_files {
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
        state.batch_running.store(false, Ordering::Release);
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
            .batch_running
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
            cancel_scan
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

    use super::{
        DesktopFileKind, DesktopScanStage, DesktopScanStatus, DesktopState, ScanRecord,
        inspect_file, register_files,
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
}
