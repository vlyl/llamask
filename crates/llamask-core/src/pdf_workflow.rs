use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, GenericImageView, ImageFormat};
use tempfile::{NamedTempFile, TempDir, tempdir};
use thiserror::Error;

use crate::image_workflow::{
    ImageWorkflowError, encode_preview_png, export_image_task_with_runtimes,
    scan_image_with_policy, verify_image_file_with_runtimes,
};
use crate::model::{
    DiagnosticSeverity, ImageFileKind, ImageFinding, ImagePreview, PdfPageTask, PdfResidualFinding,
    PdfSourceMetadata, PdfTaskDraft, PdfVerificationReport, TaskDiagnostic,
};
use crate::policy::{PolicyConfig, PolicyError};
use crate::sidecar::RuntimeRegistry;
use crate::text::sha256_hex;

const PDF_TASK_SCHEMA_VERSION: u32 = 1;
const DEFAULT_RASTER_DPI: u32 = 200;
const MAX_PDF_BYTES: usize = 100 * 1024 * 1024;
const MAX_PDF_PAGES: usize = 200;
const MAX_TOTAL_RENDERED_PIXELS: u64 = 500_000_000;
const PDFINFO_TIMEOUT: Duration = Duration::from_secs(30);
const PDF_RENDER_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_TOOL_STDOUT_BYTES: usize = 1024 * 1024;
const MAX_TOOL_STDERR_BYTES: usize = 16 * 1024;

const FORBIDDEN_OUTPUT_NAMES: &[&[u8]] = &[
    b"/AA",
    b"/AcroForm",
    b"/Annots",
    b"/EmbeddedFiles",
    b"/Encrypt",
    b"/Filespec",
    b"/Info",
    b"/JavaScript",
    b"/JS",
    b"/Metadata",
    b"/Names",
    b"/ObjStm",
    b"/OCProperties",
    b"/OpenAction",
    b"/RichMedia",
    b"/Sig",
    b"/XFA",
];

const ALLOWED_OUTPUT_NAMES: &[&[u8]] = &[
    b"/BitsPerComponent",
    b"/Catalog",
    b"/ColorSpace",
    b"/Contents",
    b"/Count",
    b"/DCTDecode",
    b"/DeviceRGB",
    b"/Filter",
    b"/Height",
    b"/Image",
    b"/Im0",
    b"/Interpolate",
    b"/Kids",
    b"/Length",
    b"/MediaBox",
    b"/Page",
    b"/Pages",
    b"/Parent",
    b"/Resources",
    b"/Root",
    b"/Size",
    b"/Subtype",
    b"/Type",
    b"/Width",
    b"/XObject",
];

#[derive(Debug, Error)]
pub enum PdfWorkflowError {
    #[error("PDF 文件读写失败：{0}")]
    Io(#[from] std::io::Error),
    #[error("PDF 页面图片处理失败：{0}")]
    Image(#[from] image::ImageError),
    #[error("PDF 页面 OCR 或打码失败：{0}")]
    ImageWorkflow(#[from] ImageWorkflowError),
    #[error("策略配置无效：{0}")]
    InvalidPolicy(#[from] PolicyError),
    #[error("PDF 文件超过 100 MiB 限制")]
    PdfTooLarge,
    #[error("文件不是有效的 PDF")]
    InvalidPdf,
    #[error("PDF 页数超过 {MAX_PDF_PAGES} 页限制")]
    TooManyPages,
    #[error("PDF 页面总像素超过安全限制")]
    RenderedPixelsTooLarge,
    #[error("加密 PDF 当前不能安全处理，请先在受信任环境中解密副本")]
    EncryptedPdfUnsupported,
    #[error("缺少本地 PDF 工具：{0}")]
    ToolUnavailable(String),
    #[error("本地 PDF 工具执行失败：{0}")]
    ToolFailed(String),
    #[error("本地 PDF 工具执行超时：{0}")]
    ToolTimeout(String),
    #[error("本地 PDF 工具返回内容过大：{0}")]
    ToolOutputTooLarge(String),
    #[error("PDF 工具返回了无法解析的结构信息")]
    InvalidToolOutput,
    #[error("PDF 任务中的 policy_id 与策略快照不一致")]
    PolicySnapshotMismatch,
    #[error("PDF 任务页结构无效")]
    InvalidPageTask,
    #[error("PDF 源文件在扫描后发生了变化，请重新扫描")]
    SourceChanged,
    #[error("PDF 输出路径不能与源文件相同")]
    WouldOverwriteSource,
    #[error("PDF 输出扩展名必须为 .pdf")]
    OutputTypeMismatch,
    #[error("输出文件已存在，未执行覆盖：{0}")]
    OutputExists(PathBuf),
    #[error("PDF 输出页数或页面尺寸与任务不一致")]
    OutputPageMismatch,
    #[error("PDF 仍有 {0} 个结果尚未复核")]
    UnreviewedFindings(usize),
    #[error("PDF 安全结构或残留复扫失败，发现 {0} 个未处理结果")]
    VerificationFailed(usize),
    #[error("PDF 扫描已取消")]
    ScanCancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PdfScanProgress {
    pub completed_pages: usize,
    pub total_pages: usize,
}

#[derive(Debug)]
struct PdfInfo {
    pages: usize,
    encrypted: bool,
    form: Option<String>,
    javascript: bool,
}

#[derive(Debug)]
struct ToolOutput {
    stdout: Vec<u8>,
}

pub fn scan_pdf_with_policy(
    path: &Path,
    policy: &PolicyConfig,
    runtimes: &RuntimeRegistry,
    ocr_runtime_id: &str,
) -> Result<PdfTaskDraft, PdfWorkflowError> {
    scan_pdf_with_policy_and_progress(path, policy, runtimes, ocr_runtime_id, |_| true)
}

pub fn scan_pdf_with_policy_and_progress(
    path: &Path,
    policy: &PolicyConfig,
    runtimes: &RuntimeRegistry,
    ocr_runtime_id: &str,
    mut on_progress: impl FnMut(PdfScanProgress) -> bool,
) -> Result<PdfTaskDraft, PdfWorkflowError> {
    policy.validate()?;
    let canonical = fs::canonicalize(path)?;
    let source_bytes = read_pdf_limited(&canonical)?;
    let source_sha256 = sha256_hex(&source_bytes);
    let work = tempdir()?;
    let staged_pdf = stage_pdf(&work, &source_bytes)?;
    let info = inspect_pdf(&staged_pdf, work.path())?;
    validate_source_info(&info)?;

    if !on_progress(PdfScanProgress {
        completed_pages: 0,
        total_pages: info.pages,
    }) {
        return Err(PdfWorkflowError::ScanCancelled);
    }

    let mut diagnostics = source_diagnostics(&source_bytes, &info);
    let mut pages = Vec::with_capacity(info.pages);
    let mut total_pixels = 0_u64;
    for page_number in 1..=info.pages {
        let rendered = render_page(&staged_pdf, page_number, DEFAULT_RASTER_DPI, work.path())?;
        let (width, height) = image::image_dimensions(&rendered)?;
        total_pixels = total_pixels
            .checked_add(u64::from(width) * u64::from(height))
            .ok_or(PdfWorkflowError::RenderedPixelsTooLarge)?;
        if total_pixels > MAX_TOTAL_RENDERED_PIXELS {
            return Err(PdfWorkflowError::RenderedPixelsTooLarge);
        }
        let mut task = scan_image_with_policy(&rendered, policy, runtimes, ocr_runtime_id)?;
        task.task_id = format!("pdf-page-{page_number:04}-{}", &source_sha256[..12]);
        task.source.path = format!("pdf://{}#page={page_number}", canonical.to_string_lossy());
        pages.push(PdfPageTask { page_number, task });
        if !on_progress(PdfScanProgress {
            completed_pages: page_number,
            total_pages: info.pages,
        }) {
            return Err(PdfWorkflowError::ScanCancelled);
        }
    }
    deduplicate_diagnostics(&mut diagnostics);

    Ok(PdfTaskDraft {
        schema_version: PDF_TASK_SCHEMA_VERSION,
        task_id: format!("pdf-task-{}", &source_sha256[..12]),
        policy_id: policy.id.clone(),
        policy: policy.clone(),
        source: PdfSourceMetadata {
            path: canonical.to_string_lossy().into_owned(),
            sha256: source_sha256,
            size_bytes: source_bytes.len() as u64,
            pages: info.pages,
            raster_dpi: DEFAULT_RASTER_DPI,
        },
        ocr_runtime_id: ocr_runtime_id.to_owned(),
        pages,
        diagnostics,
        contains_sensitive_plaintext: true,
    })
}

pub fn render_pdf_task_page_preview(
    task: &PdfTaskDraft,
    page_number: usize,
) -> Result<ImagePreview, PdfWorkflowError> {
    task.policy.validate()?;
    if task.policy_id != task.policy.id {
        return Err(PdfWorkflowError::PolicySnapshotMismatch);
    }
    let page = task
        .pages
        .iter()
        .find(|page| page.page_number == page_number)
        .ok_or(PdfWorkflowError::InvalidPageTask)?;
    let source = fs::canonicalize(&task.source.path)?;
    let source_bytes = read_pdf_limited(&source)?;
    if sha256_hex(&source_bytes) != task.source.sha256 {
        return Err(PdfWorkflowError::SourceChanged);
    }
    let work = tempdir()?;
    let staged_pdf = stage_pdf(&work, &source_bytes)?;
    let info = inspect_pdf(&staged_pdf, work.path())?;
    validate_source_info(&info)?;
    if info.pages != task.source.pages || page_number == 0 || page_number > info.pages {
        return Err(PdfWorkflowError::SourceChanged);
    }
    let rendered = render_page(
        &staged_pdf,
        page_number,
        task.source.raster_dpi,
        work.path(),
    )?;
    let image = image::load_from_memory_with_format(&fs::read(rendered)?, ImageFormat::Png)?;
    let (width, height) = image.dimensions();
    if width != page.task.source.width || height != page.task.source.height {
        return Err(PdfWorkflowError::SourceChanged);
    }
    let png_bytes = encode_preview_png(image)?;
    Ok(ImagePreview {
        width,
        height,
        png_bytes,
    })
}

pub fn export_pdf_task_with_runtimes(
    task: &PdfTaskDraft,
    output: &Path,
    runtimes: &RuntimeRegistry,
) -> Result<PdfVerificationReport, PdfWorkflowError> {
    validate_task(task, runtimes)?;
    ensure_pdf_extension(output)?;
    let source = fs::canonicalize(&task.source.path)?;
    let output_absolute = absolute_output(output)?;
    if source == output_absolute {
        return Err(PdfWorkflowError::WouldOverwriteSource);
    }
    if fs::symlink_metadata(output).is_ok() {
        if fs::canonicalize(output).is_ok_and(|existing| existing == source) {
            return Err(PdfWorkflowError::WouldOverwriteSource);
        }
        return Err(PdfWorkflowError::OutputExists(output.to_path_buf()));
    }
    let unreviewed = pdf_group_count(task, |finding| !finding.reviewed);
    if unreviewed > 0 {
        return Err(PdfWorkflowError::UnreviewedFindings(unreviewed));
    }

    let source_bytes = read_pdf_limited(&source)?;
    if sha256_hex(&source_bytes) != task.source.sha256 {
        return Err(PdfWorkflowError::SourceChanged);
    }
    let work = tempdir()?;
    let staged_pdf = stage_pdf(&work, &source_bytes)?;
    let info = inspect_pdf(&staged_pdf, work.path())?;
    validate_source_info(&info)?;
    if info.pages != task.source.pages {
        return Err(PdfWorkflowError::SourceChanged);
    }

    let mut redacted_pages = Vec::with_capacity(task.pages.len());
    for page in &task.pages {
        let rendered = render_page(
            &staged_pdf,
            page.page_number,
            task.source.raster_dpi,
            work.path(),
        )?;
        let mut image_task = page.task.clone();
        image_task.source.path = rendered.to_string_lossy().into_owned();
        let redacted = work
            .path()
            .join(format!("redacted-page-{:04}.png", page.page_number));
        export_image_task_with_runtimes(&image_task, &redacted, runtimes)?;
        redacted_pages.push(redacted);
    }

    let output_bytes = build_image_only_pdf(&redacted_pages, task.source.raster_dpi)?;
    let report = verify_pdf_bytes(task, &output_bytes, &output.to_string_lossy(), runtimes)?;
    if !report.passed {
        return Err(PdfWorkflowError::VerificationFailed(
            report.unreviewed_findings
                + report.residual_findings.len()
                + usize::from(!report.structure_sanitized),
        ));
    }

    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(&output_bytes)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist_noclobber(output)
        .map_err(|error| match error.error.kind() {
            std::io::ErrorKind::AlreadyExists => {
                PdfWorkflowError::OutputExists(output.to_path_buf())
            }
            _ => PdfWorkflowError::Io(error.error),
        })?;
    Ok(report)
}

pub fn verify_pdf_file_with_runtimes(
    task: &PdfTaskDraft,
    path: &Path,
    runtimes: &RuntimeRegistry,
) -> Result<PdfVerificationReport, PdfWorkflowError> {
    validate_task(task, runtimes)?;
    let bytes = read_pdf_limited(path)?;
    verify_pdf_bytes(task, &bytes, &path.to_string_lossy(), runtimes)
}

fn verify_pdf_bytes(
    task: &PdfTaskDraft,
    bytes: &[u8],
    checked_file: &str,
    runtimes: &RuntimeRegistry,
) -> Result<PdfVerificationReport, PdfWorkflowError> {
    let work = tempdir()?;
    let staged_pdf = stage_pdf(&work, bytes)?;
    let info = inspect_pdf(&staged_pdf, work.path())?;
    if info.encrypted || info.pages != task.pages.len() {
        return Err(PdfWorkflowError::OutputPageMismatch);
    }
    let structure_sanitized = safe_image_only_structure(bytes)
        && info
            .form
            .as_deref()
            .is_none_or(|value| value.eq_ignore_ascii_case("none"))
        && !info.javascript;

    let mut diagnostics = Vec::new();
    if !structure_sanitized {
        diagnostics.push(TaskDiagnostic {
            severity: DiagnosticSeverity::Error,
            code: "PDF_UNSAFE_STRUCTURE_REMAINS".to_owned(),
            detector_id: Some("pdf-structure".to_owned()),
            message: "输出仍包含非页面图像对象、交互结构或无法验证的 PDF 流。".to_owned(),
        });
    }
    let mut residual_findings = Vec::new();
    let mut detectors_checked = BTreeSet::new();
    let mut pages_passed = true;
    let mut complete = true;
    let mut target_residual_count = 0;
    for page in &task.pages {
        let rendered = render_page(
            &staged_pdf,
            page.page_number,
            task.source.raster_dpi,
            work.path(),
        )?;
        let report = verify_image_file_with_runtimes(&page.task, &rendered, runtimes)?;
        pages_passed &= report.passed;
        complete &= report.complete;
        target_residual_count += report.target_residual_count;
        detectors_checked.extend(report.detectors_checked);
        diagnostics.extend(report.diagnostics);
        residual_findings.extend(report.residual_findings.into_iter().map(|finding| {
            PdfResidualFinding {
                page_number: page.page_number,
                line_index: finding.line_index,
                entity_type: finding.entity_type,
                detector: finding.detector,
                explanation_code: finding.explanation_code,
                ocr_rect: finding.ocr_rect,
            }
        }));
    }
    deduplicate_diagnostics(&mut diagnostics);
    let unreviewed_findings = pdf_group_count(task, |finding| !finding.reviewed);
    Ok(PdfVerificationReport {
        schema_version: PDF_TASK_SCHEMA_VERSION,
        passed: structure_sanitized
            && pages_passed
            && unreviewed_findings == 0
            && residual_findings.is_empty(),
        complete: complete && structure_sanitized,
        checked_file: checked_file.to_owned(),
        sha256: sha256_hex(bytes),
        selected_findings: pdf_group_count(task, |finding| finding.selected),
        unreviewed_findings,
        target_residual_count,
        residual_findings,
        detectors_checked: detectors_checked.into_iter().collect(),
        pages_checked: task.pages.len(),
        rasterized_pages: task.pages.len(),
        structure_sanitized,
        diagnostics,
    })
}

fn read_pdf_limited(path: &Path) -> Result<Vec<u8>, PdfWorkflowError> {
    let metadata = fs::metadata(path)?;
    if metadata.len() > MAX_PDF_BYTES as u64 {
        return Err(PdfWorkflowError::PdfTooLarge);
    }
    let bytes = fs::read(path)?;
    if bytes.len() > MAX_PDF_BYTES {
        return Err(PdfWorkflowError::PdfTooLarge);
    }
    if !bytes.starts_with(b"%PDF-") {
        return Err(PdfWorkflowError::InvalidPdf);
    }
    Ok(bytes)
}

fn stage_pdf(work: &TempDir, bytes: &[u8]) -> Result<PathBuf, PdfWorkflowError> {
    let path = work.path().join("source.pdf");
    fs::write(&path, bytes)?;
    Ok(path)
}

fn validate_source_info(info: &PdfInfo) -> Result<(), PdfWorkflowError> {
    if info.encrypted {
        return Err(PdfWorkflowError::EncryptedPdfUnsupported);
    }
    if info.pages == 0 {
        return Err(PdfWorkflowError::InvalidPdf);
    }
    if info.pages > MAX_PDF_PAGES {
        return Err(PdfWorkflowError::TooManyPages);
    }
    Ok(())
}

fn inspect_pdf(path: &Path, cache_directory: &Path) -> Result<PdfInfo, PdfWorkflowError> {
    let executable = tool_from_env("LLAMASK_PDFINFO", "pdfinfo");
    let output = run_tool(
        "pdfinfo",
        &executable,
        &[path.as_os_str().to_owned()],
        PDFINFO_TIMEOUT,
        cache_directory,
    )?;
    parse_pdfinfo(&output.stdout)
}

fn parse_pdfinfo(bytes: &[u8]) -> Result<PdfInfo, PdfWorkflowError> {
    let text = String::from_utf8_lossy(bytes);
    let mut pages = None;
    let mut encrypted = None;
    let mut form = None;
    let mut javascript = false;
    for line in text.lines() {
        let Some((label, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match label.trim() {
            "Pages" => pages = value.parse::<usize>().ok(),
            "Encrypted" => encrypted = Some(value.to_ascii_lowercase().starts_with("yes")),
            "Form" => form = Some(value.to_owned()),
            "JavaScript" => javascript = value.eq_ignore_ascii_case("yes"),
            _ => {}
        }
    }
    Ok(PdfInfo {
        pages: pages.ok_or(PdfWorkflowError::InvalidToolOutput)?,
        encrypted: encrypted.ok_or(PdfWorkflowError::InvalidToolOutput)?,
        form,
        javascript,
    })
}

fn render_page(
    path: &Path,
    page_number: usize,
    dpi: u32,
    work_directory: &Path,
) -> Result<PathBuf, PdfWorkflowError> {
    let executable = tool_from_env("LLAMASK_PDFTOPPM", "pdftoppm");
    let prefix = work_directory.join(format!("page-{page_number:04}"));
    let page = page_number.to_string();
    let dpi = dpi.to_string();
    let args = [
        OsString::from("-f"),
        OsString::from(&page),
        OsString::from("-l"),
        OsString::from(&page),
        OsString::from("-singlefile"),
        OsString::from("-png"),
        OsString::from("-r"),
        OsString::from(&dpi),
        path.as_os_str().to_owned(),
        prefix.as_os_str().to_owned(),
    ];
    run_tool(
        "pdftoppm",
        &executable,
        &args,
        PDF_RENDER_TIMEOUT,
        work_directory,
    )?;
    let output = prefix.with_extension("png");
    if !output.is_file() {
        return Err(PdfWorkflowError::ToolFailed("pdftoppm".to_owned()));
    }
    Ok(output)
}

fn tool_from_env(variable: &str, fallback: &str) -> OsString {
    std::env::var_os(variable).unwrap_or_else(|| OsString::from(fallback))
}

fn run_tool(
    name: &str,
    executable: &OsStr,
    args: &[OsString],
    timeout: Duration,
    cache_directory: &Path,
) -> Result<ToolOutput, PdfWorkflowError> {
    let mut child = Command::new(executable)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("XDG_CACHE_HOME", cache_directory)
        .env("NO_PROXY", "*")
        .env_remove("HTTP_PROXY")
        .env_remove("HTTPS_PROXY")
        .env_remove("ALL_PROXY")
        .spawn()
        .map_err(|_| PdfWorkflowError::ToolUnavailable(name.to_owned()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| PdfWorkflowError::ToolFailed(name.to_owned()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| PdfWorkflowError::ToolFailed(name.to_owned()))?;
    let stdout_reader = thread::spawn(move || read_capped(stdout, MAX_TOOL_STDOUT_BYTES));
    let stderr_reader = thread::spawn(move || read_capped(stderr, MAX_TOOL_STDERR_BYTES));

    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|_| PdfWorkflowError::ToolFailed(name.to_owned()))?
        {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(PdfWorkflowError::ToolTimeout(name.to_owned()));
        }
        thread::sleep(Duration::from_millis(10));
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| PdfWorkflowError::ToolFailed(name.to_owned()))??;
    let _stderr = stderr_reader
        .join()
        .map_err(|_| PdfWorkflowError::ToolFailed(name.to_owned()))??;
    if stdout.len() > MAX_TOOL_STDOUT_BYTES {
        return Err(PdfWorkflowError::ToolOutputTooLarge(name.to_owned()));
    }
    if !status.success() {
        return Err(PdfWorkflowError::ToolFailed(name.to_owned()));
    }
    Ok(ToolOutput { stdout })
}

fn read_capped(mut reader: impl Read, limit: usize) -> std::io::Result<Vec<u8>> {
    let mut kept = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        if kept.len() <= limit {
            let remaining = limit.saturating_add(1).saturating_sub(kept.len());
            kept.extend_from_slice(&buffer[..read.min(remaining)]);
        }
    }
    Ok(kept)
}

fn source_diagnostics(bytes: &[u8], info: &PdfInfo) -> Vec<TaskDiagnostic> {
    let mut diagnostics = vec![
        TaskDiagnostic {
            severity: DiagnosticSeverity::Info,
            code: "PDF_FULL_RASTER_MODE".to_owned(),
            detector_id: Some("pdf-structure".to_owned()),
            message: "安全模式会栅格化全部页面；版面尽量保持一致，但文字搜索、复制、矢量编辑和表单交互不会保留。".to_owned(),
        },
        TaskDiagnostic {
            severity: DiagnosticSeverity::Info,
            code: "PDF_CONTAINER_DATA_REMOVED".to_owned(),
            detector_id: Some("pdf-structure".to_owned()),
            message: "导出会从零构造 PDF，不复制元数据、隐藏文字、增量历史或非页面载荷。".to_owned(),
        },
    ];
    if info
        .form
        .as_deref()
        .is_some_and(|value| !value.eq_ignore_ascii_case("none"))
    {
        diagnostics.push(structure_warning(
            "PDF_FORMS_FLATTENED",
            "检测到表单；可见外观会进入页面图像，字段结构和值不会复制。",
        ));
    }
    if info.javascript
        || contains_pdf_name(bytes, b"/JavaScript")
        || contains_pdf_name(bytes, b"/JS")
    {
        diagnostics.push(structure_warning(
            "PDF_ACTIVE_CONTENT_REMOVED",
            "检测到脚本或主动内容；导出时会移除。",
        ));
    }
    if contains_pdf_name(bytes, b"/Annots") {
        diagnostics.push(structure_warning(
            "PDF_ANNOTATIONS_FLATTENED",
            "检测到批注或链接；仅保留渲染时可见外观，不保留交互内容。",
        ));
    }
    if contains_pdf_name(bytes, b"/EmbeddedFiles") || contains_pdf_name(bytes, b"/Filespec") {
        diagnostics.push(structure_warning(
            "PDF_ATTACHMENTS_REMOVED",
            "检测到附件；导出时不会复制。",
        ));
    }
    if contains_pdf_name(bytes, b"/Sig") {
        diagnostics.push(structure_warning(
            "PDF_SIGNATURE_REMOVED",
            "检测到数字签名；安全栅格化副本不会保留签名及其有效性。",
        ));
    }
    diagnostics
}

fn structure_warning(code: &str, message: &str) -> TaskDiagnostic {
    TaskDiagnostic {
        severity: DiagnosticSeverity::Warning,
        code: code.to_owned(),
        detector_id: Some("pdf-structure".to_owned()),
        message: message.to_owned(),
    }
}

fn validate_task(task: &PdfTaskDraft, runtimes: &RuntimeRegistry) -> Result<(), PdfWorkflowError> {
    task.policy.validate()?;
    if task.schema_version != PDF_TASK_SCHEMA_VERSION || task.policy_id != task.policy.id {
        return Err(PdfWorkflowError::PolicySnapshotMismatch);
    }
    if task.source.raster_dpi != DEFAULT_RASTER_DPI
        || task.source.pages != task.pages.len()
        || task.pages.is_empty()
        || task.pages.len() > MAX_PDF_PAGES
        || runtimes.runtime(&task.ocr_runtime_id).is_none()
    {
        return Err(PdfWorkflowError::InvalidPageTask);
    }
    for (index, page) in task.pages.iter().enumerate() {
        if page.page_number != index + 1
            || page.task.policy_id != task.policy_id
            || page.task.policy != task.policy
            || page.task.ocr_runtime_id != task.ocr_runtime_id
            || page.task.source.file_kind != ImageFileKind::Png
            || page.task.source.width == 0
            || page.task.source.height == 0
        {
            return Err(PdfWorkflowError::InvalidPageTask);
        }
    }
    Ok(())
}

fn ensure_pdf_extension(path: &Path) -> Result<(), PdfWorkflowError> {
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
    {
        Ok(())
    } else {
        Err(PdfWorkflowError::OutputTypeMismatch)
    }
}

fn absolute_output(path: &Path) -> Result<PathBuf, std::io::Error> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let Some(file_name) = absolute.file_name() else {
        return Ok(absolute);
    };
    let parent = absolute.parent().unwrap_or_else(|| Path::new("."));
    match fs::canonicalize(parent) {
        Ok(parent) => Ok(parent.join(file_name)),
        Err(_) => Ok(absolute),
    }
}

fn pdf_group_count(task: &PdfTaskDraft, predicate: impl Fn(&ImageFinding) -> bool) -> usize {
    let mut groups = BTreeSet::new();
    for page in &task.pages {
        for finding in &page.task.findings {
            if predicate(finding) {
                groups.insert((page.page_number, finding.group_id.as_str()));
            }
        }
    }
    groups.len()
}

fn build_image_only_pdf(pages: &[PathBuf], dpi: u32) -> Result<Vec<u8>, PdfWorkflowError> {
    if pages.is_empty() || dpi == 0 {
        return Err(PdfWorkflowError::InvalidPdf);
    }
    let mut encoded_pages = Vec::with_capacity(pages.len());
    for page in pages {
        let image = image::open(page)?;
        let dimensions = image.dimensions();
        let mut jpeg = Vec::new();
        JpegEncoder::new_with_quality(&mut jpeg, 95)
            .encode_image(&DynamicImage::ImageRgb8(image.into_rgb8()))?;
        encoded_pages.push((dimensions.0, dimensions.1, jpeg));
    }
    build_pdf_objects(&encoded_pages, dpi)
}

fn build_pdf_objects(pages: &[(u32, u32, Vec<u8>)], dpi: u32) -> Result<Vec<u8>, PdfWorkflowError> {
    let object_count = 2 + pages.len() * 3;
    let mut objects = vec![Vec::new(); object_count];
    objects[0] = b"<< /Type /Catalog /Pages 2 0 R >>".to_vec();
    let kids = (0..pages.len())
        .map(|index| format!("{} 0 R", 3 + index * 3))
        .collect::<Vec<_>>()
        .join(" ");
    objects[1] = format!("<< /Type /Pages /Count {} /Kids [{kids}] >>", pages.len()).into_bytes();

    for (index, (width, height, jpeg)) in pages.iter().enumerate() {
        let page_object = 3 + index * 3;
        let image_object = page_object + 1;
        let content_object = page_object + 2;
        let width_points = f64::from(*width) * 72.0 / f64::from(dpi);
        let height_points = f64::from(*height) * 72.0 / f64::from(dpi);
        objects[page_object - 1] = format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width_points:.6} {height_points:.6}] /Resources << /XObject << /Im0 {image_object} 0 R >> >> /Contents {content_object} 0 R >>"
        )
        .into_bytes();
        let mut image_object_bytes = format!(
            "<< /Type /XObject /Subtype /Image /Width {width} /Height {height} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Interpolate true /Filter /DCTDecode /Length {} >>\nstream\n",
            jpeg.len()
        )
        .into_bytes();
        image_object_bytes.extend_from_slice(jpeg);
        image_object_bytes.extend_from_slice(b"\nendstream");
        objects[image_object - 1] = image_object_bytes;
        let content = format!("q\n{width_points:.6} 0 0 {height_points:.6} 0 0 cm\n/Im0 Do\nQ\n");
        let mut content_object_bytes =
            format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
        content_object_bytes.extend_from_slice(content.as_bytes());
        content_object_bytes.extend_from_slice(b"endstream");
        objects[content_object - 1] = content_object_bytes;
    }

    let mut output = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for (index, object) in objects.iter().enumerate() {
        offsets.push(output.len());
        writeln!(&mut output, "{} 0 obj", index + 1)?;
        output.extend_from_slice(object);
        output.extend_from_slice(b"\nendobj\n");
    }
    let xref = output.len();
    write!(&mut output, "xref\n0 {}\n", objects.len() + 1)?;
    output.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        writeln!(&mut output, "{offset:010} 00000 n ")?;
    }
    write!(
        &mut output,
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objects.len() + 1
    )?;
    Ok(output)
}

fn safe_image_only_structure(bytes: &[u8]) -> bool {
    let Some(structure) = bytes_without_streams(bytes) else {
        return false;
    };
    FORBIDDEN_OUTPUT_NAMES
        .iter()
        .all(|name| !contains_pdf_name(&structure, name))
        && only_allowed_pdf_names(&structure)
        && contains_pdf_name(&structure, b"/Catalog")
        && contains_pdf_name(&structure, b"/Pages")
        && contains_pdf_name(&structure, b"/Image")
}

fn only_allowed_pdf_names(bytes: &[u8]) -> bool {
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] != b'/' {
            cursor += 1;
            continue;
        }
        let start = cursor;
        cursor += 1;
        while cursor < bytes.len() && !token_boundary(Some(bytes[cursor])) {
            cursor += 1;
        }
        let name = &bytes[start..cursor];
        if !ALLOWED_OUTPUT_NAMES.contains(&name) {
            return false;
        }
    }
    true
}

fn bytes_without_streams(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut output = Vec::with_capacity(bytes.len().min(1024 * 1024));
    let mut cursor = 0;
    while let Some(relative) = find_subslice(&bytes[cursor..], b"stream") {
        let stream = cursor + relative;
        if !token_boundary(bytes.get(stream.wrapping_sub(1)).copied())
            || !token_boundary(bytes.get(stream + 6).copied())
        {
            output.extend_from_slice(&bytes[cursor..stream + 6]);
            cursor = stream + 6;
            continue;
        }
        let header = &bytes[cursor..stream];
        output.extend_from_slice(header);
        let length = parse_direct_stream_length(header)?;
        let data_start = if bytes.get(stream + 6..stream + 8) == Some(b"\r\n") {
            stream + 8
        } else if bytes.get(stream + 6) == Some(&b'\n') {
            stream + 7
        } else {
            return None;
        };
        let after_data = data_start.checked_add(length)?;
        if after_data > bytes.len() {
            return None;
        }
        if !valid_stream(header, &bytes[data_start..after_data]) {
            return None;
        }
        let mut endstream = after_data;
        if bytes.get(endstream..endstream + 2) == Some(b"\r\n") {
            endstream += 2;
        } else if bytes.get(endstream) == Some(&b'\n') {
            endstream += 1;
        }
        if bytes.get(endstream..endstream + 9) != Some(b"endstream") {
            return None;
        }
        output.extend_from_slice(b"stream endstream");
        cursor = endstream + 9;
    }
    output.extend_from_slice(&bytes[cursor..]);
    Some(output)
}

fn valid_stream(header: &[u8], data: &[u8]) -> bool {
    if contains_pdf_name(header, b"/Image") {
        return contains_pdf_name(header, b"/DCTDecode")
            && contains_pdf_name(header, b"/DeviceRGB")
            && data.starts_with(&[0xff, 0xd8])
            && data.ends_with(&[0xff, 0xd9]);
    }
    let Ok(content) = std::str::from_utf8(data) else {
        return false;
    };
    let tokens = content.split_ascii_whitespace().collect::<Vec<_>>();
    tokens.len() == 11
        && tokens[0] == "q"
        && positive_number(tokens[1])
        && tokens[2..4] == ["0", "0"]
        && positive_number(tokens[4])
        && tokens[5..7] == ["0", "0"]
        && tokens[7..] == ["cm", "/Im0", "Do", "Q"]
}

fn positive_number(value: &str) -> bool {
    value
        .parse::<f64>()
        .is_ok_and(|number| number.is_finite() && number > 0.0)
}

fn parse_direct_stream_length(header: &[u8]) -> Option<usize> {
    let start = header
        .windows(b"/Length".len())
        .rposition(|window| window == b"/Length")?
        + b"/Length".len();
    let suffix = &header[start..];
    let first = suffix.iter().position(|byte| !byte.is_ascii_whitespace())?;
    let digits = suffix[first..]
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .copied()
        .collect::<Vec<_>>();
    if digits.is_empty() {
        return None;
    }
    std::str::from_utf8(&digits).ok()?.parse().ok()
}

fn contains_pdf_name(bytes: &[u8], name: &[u8]) -> bool {
    bytes
        .windows(name.len())
        .enumerate()
        .any(|(index, window)| {
            window == name
                && token_boundary(bytes.get(index.wrapping_sub(1)).copied())
                && token_boundary(bytes.get(index + name.len()).copied())
        })
}

fn token_boundary(value: Option<u8>) -> bool {
    value.is_none_or(|byte| {
        byte.is_ascii_whitespace()
            || matches!(byte, b'<' | b'>' | b'[' | b']' | b'(' | b')' | b'/' | b'%')
    })
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn deduplicate_diagnostics(diagnostics: &mut Vec<TaskDiagnostic>) {
    let mut seen = BTreeSet::new();
    diagnostics.retain(|item| {
        seen.insert((
            item.code.clone(),
            item.detector_id.clone(),
            item.message.clone(),
        ))
    });
}

#[cfg(test)]
mod tests {
    use super::{
        build_pdf_objects, bytes_without_streams, parse_pdfinfo, safe_image_only_structure,
    };

    #[test]
    fn parses_poppler_summary_without_retaining_metadata() {
        let info = parse_pdfinfo(
            b"Title: secret\nPages: 3\nEncrypted: no\nForm: AcroForm\nJavaScript: yes\n",
        )
        .unwrap();
        assert_eq!(info.pages, 3);
        assert!(!info.encrypted);
        assert_eq!(info.form.as_deref(), Some("AcroForm"));
        assert!(info.javascript);
    }

    #[test]
    fn generated_pdf_contains_only_page_images() {
        let bytes = build_pdf_objects(&[(2, 3, vec![0xff, 0xd8, 0xff, 0xd9])], 200).unwrap();
        assert!(bytes.starts_with(b"%PDF-1.4"));
        assert!(safe_image_only_structure(&bytes));
        let structure = bytes_without_streams(&bytes).unwrap();
        assert!(!structure.windows(4).any(|window| window == b"/JS "));
    }

    #[test]
    fn rejects_interactive_or_indirect_length_structures() {
        let active = b"%PDF-1.4\n1 0 obj << /Type /Catalog /OpenAction 2 0 R >> endobj";
        assert!(!safe_image_only_structure(active));
        let indirect = b"%PDF-1.4\n1 0 obj << /Length 2 0 R >> stream\nabc\nendstream\nendobj";
        assert!(bytes_without_streams(indirect).is_none());
    }
}
