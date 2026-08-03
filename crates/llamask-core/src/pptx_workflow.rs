use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use quick_xml::Reader;
use quick_xml::escape::unescape;
use quick_xml::events::{BytesStart, BytesText, Event};
use quick_xml::writer::Writer;
use tempfile::{NamedTempFile, tempdir};
use thiserror::Error;
use zip::CompressionMethod;
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

use crate::image_workflow::{
    ImageWorkflowError, export_image_task_with_runtimes, render_image_task_preview,
    scan_image_with_policy, verify_image_file_with_runtimes,
};
use crate::model::{
    DiagnosticSeverity, DocumentPart, EntityType, FileKind, Finding, ImagePreview, ImageTaskDraft,
    PptxDocumentGraph, PptxEmbeddedImageTask, PptxImageResidualFinding, PptxResidualFinding,
    PptxSourceMetadata, PptxTaskDraft, PptxVerificationReport, TaskDiagnostic,
};
use crate::policy::{PolicyConfig, PolicyError};
use crate::sidecar::RuntimeRegistry;
use crate::text::sha256_hex;
use crate::workflow::{WorkflowError, review_finding, run_detection};

const MAX_PPTX_BYTES: usize = 100 * 1024 * 1024;
const MAX_PACKAGE_ENTRIES: usize = 10_000;
const MAX_ENTRY_BYTES: u64 = 100 * 1024 * 1024;
const MAX_XML_BYTES: u64 = 20 * 1024 * 1024;
const MAX_TOTAL_UNCOMPRESSED_BYTES: u64 = 500 * 1024 * 1024;
const MAX_COMPRESSION_RATIO: u64 = 1_000;

#[derive(Debug, Error)]
pub enum PptxWorkflowError {
    #[error("PPTX 文件读写失败：{0}")]
    Io(#[from] std::io::Error),
    #[error("PPTX ZIP 结构无效：{0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("策略配置无效：{0}")]
    InvalidPolicy(#[from] PolicyError),
    #[error("文本检测失败：{0}")]
    TextDetection(#[from] WorkflowError),
    #[error("PPTX 文件超过 100 MiB 限制")]
    PackageTooLarge,
    #[error("PPTX 包含过多文件项")]
    TooManyEntries,
    #[error("PPTX 包含不安全或重复的 ZIP 路径：{0}")]
    UnsafeEntryName(String),
    #[error("PPTX 包含加密文件项，当前不能安全处理")]
    EncryptedEntry,
    #[error("PPTX 文件项超过安全解压限制：{0}")]
    EntryTooLarge(String),
    #[error("PPTX 疑似压缩炸弹：{0}")]
    SuspiciousCompression(String),
    #[error("PPTX 使用了不支持的压缩方法：{0}")]
    UnsupportedCompression(String),
    #[error("PPTX 缺少必要文件项：{0}")]
    MissingRequiredEntry(String),
    #[error("PPTX XML 无效：{0}")]
    InvalidXml(String),
    #[error("PPTX 没有可处理的文本内容")]
    MissingTextContent,
    #[error("PPTX 任务中的 policy_id 与策略快照不一致")]
    PolicySnapshotMismatch,
    #[error("PPTX 源文件在扫描后发生了变化，请重新扫描")]
    SourceChanged,
    #[error("PPTX 输出路径不能与源文件相同")]
    WouldOverwriteSource,
    #[error("PPTX 输出扩展名必须为 .pptx")]
    OutputTypeMismatch,
    #[error("输出文件已存在，未执行覆盖：{0}")]
    OutputExists(PathBuf),
    #[error("PPTX 仍有 {0} 个结果尚未复核")]
    UnreviewedFindings(usize),
    #[error("PPTX 命中范围无效：{0}")]
    InvalidFinding(String),
    #[error("PPTX 残留复扫失败，发现 {0} 个未处理结果")]
    VerificationFailed(usize),
    #[error("PPTX 包含当前无法安全验证的图表、嵌入对象或外部数据，已拒绝导出")]
    EmbeddedObjectsUnsupported,
    #[error("PPTX 包含嵌入图片；当前文字切片尚未递归执行图片 OCR，已拒绝导出")]
    EmbeddedImagesUnsupported,
    #[error("PPTX 嵌入图片需要 OCR 运行注册表才能扫描、导出和复核")]
    EmbeddedImageRuntimeRequired,
    #[error("PPTX 嵌入图片任务与源包不一致：{0}")]
    EmbeddedImageTaskMismatch(String),
    #[error("PPTX 包含当前不支持的嵌入图片格式：{0}")]
    UnsupportedEmbeddedImageType(String),
    #[error("PPTX 嵌入图片处理失败：{0}")]
    EmbeddedImage(#[from] ImageWorkflowError),
    #[error("PPTX 包含宏、ActiveX 或其他主动内容，当前版本拒绝导出")]
    ActiveContentUnsupported,
    #[error("PPTX 包含无法安全保留的外部关系：{0}")]
    ExternalRelationshipUnsupported(String),
}

#[derive(Debug)]
struct PackageSummary {
    entry_names: Vec<String>,
    slides: usize,
    embedded_images: usize,
    unsupported_embedded_images: Vec<String>,
    embedded_objects: usize,
    active_content: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum XmlGrouping {
    Paragraph,
    ParagraphAndTextNode,
}

#[derive(Debug)]
struct StoryEntry {
    name: String,
    kind: String,
    grouping: XmlGrouping,
}

#[derive(Debug)]
struct TextSlot {
    event_index: usize,
    start: usize,
    end: usize,
}

pub fn scan_pptx_with_policy(
    path: &Path,
    policy: &PolicyConfig,
    runtimes: Option<&RuntimeRegistry>,
) -> Result<PptxTaskDraft, PptxWorkflowError> {
    scan_pptx_with_policy_and_images(path, policy, runtimes, None)
}

pub fn scan_pptx_with_policy_and_images(
    path: &Path,
    policy: &PolicyConfig,
    runtimes: Option<&RuntimeRegistry>,
    ocr_runtime_id: Option<&str>,
) -> Result<PptxTaskDraft, PptxWorkflowError> {
    policy.validate()?;
    let canonical = fs::canonicalize(path)?;
    let bytes = fs::read(&canonical)?;
    let summary = validate_package(&bytes)?;
    let source_sha256 = sha256_hex(&bytes);
    let parts = extract_document_parts(&bytes, &summary.entry_names)?;
    if parts.is_empty() && summary.embedded_images == 0 {
        return Err(PptxWorkflowError::MissingTextContent);
    }

    let embedded_images = match (runtimes, ocr_runtime_id) {
        (Some(runtimes), Some(ocr_runtime_id)) => scan_embedded_images(
            &bytes,
            &summary.entry_names,
            &canonical,
            policy,
            runtimes,
            ocr_runtime_id,
        )?,
        _ => Vec::new(),
    };

    let mut findings = Vec::new();
    let mut diagnostics = Vec::new();
    for part in &parts {
        let mut detection = run_detection(
            &part.text,
            &FileKind::Text,
            &source_sha256,
            &part.id,
            policy,
            runtimes,
            "pptx-scan",
        )?;
        for finding in &mut detection.findings {
            finding.part_id.clone_from(&part.id);
        }
        policy.apply_to_findings(&mut detection.findings);
        findings.append(&mut detection.findings);
        diagnostics.append(&mut detection.diagnostics);
    }
    findings.sort_by(|left, right| {
        left.part_id
            .cmp(&right.part_id)
            .then_with(|| left.start.cmp(&right.start))
            .then_with(|| left.end.cmp(&right.end))
    });
    for (index, finding) in findings.iter_mut().enumerate() {
        finding.id = format!("pptx-finding-{:04}", index + 1);
    }
    if !summary.unsupported_embedded_images.is_empty() {
        diagnostics.push(TaskDiagnostic {
            severity: DiagnosticSeverity::Error,
            code: "PPTX_EMBEDDED_IMAGE_FORMAT_UNSUPPORTED".to_owned(),
            detector_id: None,
            message: "文档包含非 PNG/JPEG 嵌入图片；当前版本无法安全重编码和 OCR，将拒绝导出。"
                .to_owned(),
        });
    }
    if summary.embedded_images > embedded_images.len() {
        diagnostics.push(TaskDiagnostic {
            severity: DiagnosticSeverity::Warning,
            code: "PPTX_EMBEDDED_IMAGES_PENDING".to_owned(),
            detector_id: None,
            message: "文档包含尚未扫描的嵌入图片；请提供 OCR 运行配置后重新扫描。".to_owned(),
        });
    } else if !embedded_images.is_empty() {
        diagnostics.push(TaskDiagnostic {
            severity: DiagnosticSeverity::Info,
            code: "PPTX_EMBEDDED_IMAGES_SCANNED".to_owned(),
            detector_id: ocr_runtime_id.map(str::to_owned),
            message: "嵌入图片已建立可编辑遮罩子任务；导出时会逐图打码并独立 OCR 复扫。".to_owned(),
        });
    }
    if summary.embedded_objects > 0 {
        diagnostics.push(TaskDiagnostic {
            severity: DiagnosticSeverity::Error,
            code: "PPTX_EMBEDDED_OBJECTS_UNSUPPORTED".to_owned(),
            detector_id: None,
            message: "文档包含图表/缓存、嵌入对象、外部数据、3D/墨迹或签名；当前版本无法证明这些载荷内部无残留，将拒绝导出。".to_owned(),
        });
    }
    if summary.active_content > 0 {
        diagnostics.push(TaskDiagnostic {
            severity: DiagnosticSeverity::Error,
            code: "PPTX_ACTIVE_CONTENT_UNSUPPORTED".to_owned(),
            detector_id: None,
            message: "文档包含宏或 ActiveX；当前版本不会执行或复制这类主动内容，将拒绝导出。"
                .to_owned(),
        });
    }
    deduplicate_diagnostics(&mut diagnostics);

    Ok(PptxTaskDraft {
        schema_version: 2,
        task_id: format!("pptx-task-{}", &source_sha256[..12]),
        policy_id: policy.id.clone(),
        policy: policy.clone(),
        document: PptxDocumentGraph {
            schema_version: 1,
            offset_unit: "unicode_scalar".to_owned(),
            source: PptxSourceMetadata {
                path: canonical.to_string_lossy().into_owned(),
                sha256: source_sha256,
                size_bytes: bytes.len() as u64,
                package_entries: summary.entry_names.len(),
                slides: summary.slides,
                embedded_images: summary.embedded_images,
            },
            parts,
        },
        findings,
        embedded_images,
        diagnostics,
        contains_sensitive_plaintext: true,
    })
}

pub fn review_pptx_finding(
    task: &mut PptxTaskDraft,
    finding_id: &str,
    selected: bool,
    replacement: Option<&str>,
) -> Result<(), PptxWorkflowError> {
    validate_pptx_task(task)?;
    let mut updated = task.clone();
    review_finding(&mut updated.findings, finding_id, selected, replacement)?;
    validate_pptx_task(&updated)?;
    *task = updated;
    Ok(())
}

pub fn render_pptx_embedded_image_preview(
    task: &PptxTaskDraft,
    image_number: usize,
) -> Result<ImagePreview, PptxWorkflowError> {
    validate_pptx_task(task)?;
    let embedded = image_number
        .checked_sub(1)
        .and_then(|index| task.embedded_images.get(index))
        .ok_or_else(|| {
            PptxWorkflowError::EmbeddedImageTaskMismatch(format!(
                "内嵌图片编号无效：{image_number}"
            ))
        })?;
    let source_path = fs::canonicalize(&task.document.source.path)?;
    let source_bytes = fs::read(&source_path)?;
    if sha256_hex(&source_bytes) != task.document.source.sha256 {
        return Err(PptxWorkflowError::SourceChanged);
    }
    let summary = validate_package(&source_bytes)?;
    embedded_tasks_by_entry(task, &summary)?;

    let mut archive = ZipArchive::new(Cursor::new(source_bytes))?;
    let mut entry = archive.by_name(&embedded.entry_name)?;
    let mut image_bytes = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut image_bytes)?;
    let extension = embedded_image_extension(&embedded.entry_name)?;
    let temporary = tempdir()?;
    let image_path = temporary.path().join(format!("preview.{extension}"));
    fs::write(&image_path, image_bytes)?;
    let mut preview_task = embedded.task.clone();
    preview_task.source.path = image_path.to_string_lossy().into_owned();
    render_image_task_preview(&preview_task).map_err(PptxWorkflowError::EmbeddedImage)
}

fn validate_package(bytes: &[u8]) -> Result<PackageSummary, PptxWorkflowError> {
    if bytes.len() > MAX_PPTX_BYTES {
        return Err(PptxWorkflowError::PackageTooLarge);
    }
    let mut archive = ZipArchive::new(Cursor::new(bytes))?;
    if archive.len() > MAX_PACKAGE_ENTRIES {
        return Err(PptxWorkflowError::TooManyEntries);
    }
    let mut names = HashSet::new();
    let mut entry_names = Vec::with_capacity(archive.len());
    let mut total_uncompressed = 0_u64;
    let mut slides = 0usize;
    let mut embedded_images = 0usize;
    let mut unsupported_embedded_images = Vec::new();
    let mut embedded_objects = 0usize;
    let mut active_content = 0usize;
    for index in 0..archive.len() {
        let file = archive.by_index(index)?;
        let name = file.name().to_owned();
        if file.enclosed_name().is_none()
            || name.contains('\\')
            || name.contains('\0')
            || !names.insert(name.clone())
        {
            return Err(PptxWorkflowError::UnsafeEntryName(name));
        }
        if file.encrypted() {
            return Err(PptxWorkflowError::EncryptedEntry);
        }
        if !file.is_dir()
            && !matches!(
                file.compression(),
                CompressionMethod::Stored | CompressionMethod::Deflated
            )
        {
            return Err(PptxWorkflowError::UnsupportedCompression(name));
        }
        if file.size() > MAX_ENTRY_BYTES || (name.ends_with(".xml") && file.size() > MAX_XML_BYTES)
        {
            return Err(PptxWorkflowError::EntryTooLarge(name));
        }
        total_uncompressed = total_uncompressed.saturating_add(file.size());
        if total_uncompressed > MAX_TOTAL_UNCOMPRESSED_BYTES {
            return Err(PptxWorkflowError::PackageTooLarge);
        }
        if file.size() > 10 * 1024 * 1024
            && file.size() > file.compressed_size().saturating_mul(MAX_COMPRESSION_RATIO)
        {
            return Err(PptxWorkflowError::SuspiciousCompression(name));
        }
        if is_slide(&name) && !file.is_dir() {
            slides += 1;
        }
        if is_embedded_media(&name) && !file.is_dir() {
            embedded_images += 1;
            if !is_supported_embedded_image(&name) {
                unsupported_embedded_images.push(name.clone());
            }
        }
        if is_unsupported_payload(&name) && !file.is_dir() {
            embedded_objects += 1;
        }
        if is_active_content(&name) && !file.is_dir() {
            active_content += 1;
        }
        entry_names.push(name);
    }
    for required in [
        "[Content_Types].xml",
        "_rels/.rels",
        "ppt/presentation.xml",
        "ppt/_rels/presentation.xml.rels",
    ] {
        if !names.contains(required) {
            return Err(PptxWorkflowError::MissingRequiredEntry(required.to_owned()));
        }
    }
    if slides == 0 {
        return Err(PptxWorkflowError::MissingRequiredEntry(
            "ppt/slides/slide*.xml".to_owned(),
        ));
    }
    Ok(PackageSummary {
        entry_names,
        slides,
        embedded_images,
        unsupported_embedded_images,
        embedded_objects,
        active_content,
    })
}

fn is_slide(name: &str) -> bool {
    name.starts_with("ppt/slides/slide") && name.ends_with(".xml") && !name.contains("/_rels/")
}

fn is_unsupported_payload(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with("ppt/embeddings/")
        || lower.starts_with("ppt/charts/")
        || lower.starts_with("ppt/externallinks/")
        || lower.starts_with("ppt/model3d/")
        || lower.starts_with("ppt/ink/")
        || lower.starts_with("_xmlsignatures/")
}

fn scan_embedded_images(
    bytes: &[u8],
    entry_names: &[String],
    source_path: &Path,
    policy: &PolicyConfig,
    runtimes: &RuntimeRegistry,
    ocr_runtime_id: &str,
) -> Result<Vec<PptxEmbeddedImageTask>, PptxWorkflowError> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))?;
    let temporary = tempdir()?;
    let mut images = Vec::new();
    for (index, entry_name) in entry_names
        .iter()
        .filter(|name| is_supported_embedded_image(name))
        .enumerate()
    {
        let mut entry = archive.by_name(entry_name)?;
        let mut image_bytes = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut image_bytes)?;
        let extension = embedded_image_extension(entry_name)?;
        let image_path = temporary
            .path()
            .join(format!("embedded-{index:05}.{extension}"));
        fs::write(&image_path, &image_bytes)?;
        let mut task = scan_image_with_policy(&image_path, policy, runtimes, ocr_runtime_id)?;
        task.task_id = format!("pptx-image-{}-{index:05}", &task.source.sha256[..12]);
        task.source.path = format!("pptx://{}#{entry_name}", source_path.to_string_lossy());
        images.push(PptxEmbeddedImageTask {
            entry_name: entry_name.clone(),
            task,
        });
    }
    Ok(images)
}

fn extract_document_parts(
    bytes: &[u8],
    entry_names: &[String],
) -> Result<Vec<DocumentPart>, PptxWorkflowError> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))?;
    let mut parts = Vec::new();
    let mut part_index = 0usize;
    for story in story_entries(entry_names) {
        let mut file = archive.by_name(&story.name)?;
        let mut xml = Vec::with_capacity(file.size() as usize);
        file.read_to_end(&mut xml)?;
        let extracted = extract_story_parts(&story, &xml)?;
        for (locator, text) in extracted {
            if text.is_empty() {
                continue;
            }
            part_index += 1;
            parts.push(DocumentPart {
                id: format!("pptx-part-{part_index:05}"),
                kind: story.kind.clone(),
                locator,
                char_len: text.chars().count(),
                text,
            });
        }
    }
    Ok(parts)
}

fn story_entries(entry_names: &[String]) -> Vec<StoryEntry> {
    let mut entries: Vec<StoryEntry> = entry_names
        .iter()
        .filter_map(|name| story_entry(name))
        .collect();
    entries.sort_by(|left, right| {
        story_priority(&left.name)
            .cmp(&story_priority(&right.name))
            .then_with(|| left.name.cmp(&right.name))
    });
    entries
}

fn story_entry(name: &str) -> Option<StoryEntry> {
    let (kind, grouping) = if is_slide(name) {
        ("slide", XmlGrouping::Paragraph)
    } else if name.starts_with("ppt/notesSlides/notesSlide") && name.ends_with(".xml") {
        ("notes", XmlGrouping::Paragraph)
    } else if name.starts_with("ppt/slideMasters/slideMaster") && name.ends_with(".xml") {
        ("slide_master", XmlGrouping::Paragraph)
    } else if name.starts_with("ppt/slideLayouts/slideLayout") && name.ends_with(".xml") {
        ("slide_layout", XmlGrouping::Paragraph)
    } else if name.starts_with("ppt/notesMasters/notesMaster") && name.ends_with(".xml") {
        ("notes_master", XmlGrouping::Paragraph)
    } else if name.starts_with("ppt/handoutMasters/handoutMaster") && name.ends_with(".xml") {
        ("handout_master", XmlGrouping::Paragraph)
    } else if name.starts_with("ppt/comments/") && name.ends_with(".xml") {
        ("comment", XmlGrouping::ParagraphAndTextNode)
    } else if name.starts_with("ppt/diagrams/") && name.ends_with(".xml") {
        ("diagram", XmlGrouping::ParagraphAndTextNode)
    } else {
        return None;
    };
    Some(StoryEntry {
        name: name.to_owned(),
        kind: kind.to_owned(),
        grouping,
    })
}

fn story_priority(name: &str) -> u8 {
    if name.starts_with("ppt/slides/") {
        0
    } else if name.starts_with("ppt/notesSlides/") {
        1
    } else if name.starts_with("ppt/comments/") {
        2
    } else if name.starts_with("ppt/slideMasters/") {
        3
    } else if name.starts_with("ppt/slideLayouts/") {
        4
    } else if name.starts_with("ppt/notesMasters/") {
        5
    } else if name.starts_with("ppt/handoutMasters/") {
        6
    } else {
        7
    }
}

fn extract_story_parts(
    story: &StoryEntry,
    xml: &[u8],
) -> Result<Vec<(String, String)>, PptxWorkflowError> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut parts = Vec::new();
    let mut paragraph_index = 0usize;
    let mut text_node_index = 0usize;
    let mut in_paragraph = false;
    let mut in_text_node = false;
    let mut paragraph_text = String::new();
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) if local_name(event.name().as_ref()) == b"p" => {
                in_paragraph = true;
                paragraph_text.clear();
            }
            Ok(Event::End(event)) if local_name(event.name().as_ref()) == b"p" => {
                paragraph_index += 1;
                if !paragraph_text.is_empty() {
                    parts.push((
                        format!("{}#p{paragraph_index:06}", story.name),
                        paragraph_text.clone(),
                    ));
                }
                in_paragraph = false;
                paragraph_text.clear();
            }
            Ok(Event::Start(event)) if is_text_element(event.name().as_ref()) => {
                in_text_node = true;
            }
            Ok(Event::End(event)) if is_text_element(event.name().as_ref()) => {
                in_text_node = false;
            }
            Ok(Event::Text(event)) if in_text_node => {
                let decoded = event
                    .decode()
                    .map_err(|_| PptxWorkflowError::InvalidXml(story.name.clone()))?;
                let text = unescape(&decoded)
                    .map_err(|_| PptxWorkflowError::InvalidXml(story.name.clone()))?
                    .into_owned();
                if in_paragraph {
                    paragraph_text.push_str(&text);
                } else if story.grouping == XmlGrouping::ParagraphAndTextNode && !text.is_empty() {
                    text_node_index += 1;
                    parts.push((format!("{}#t{text_node_index:06}", story.name), text));
                }
            }
            Ok(Event::CData(_)) if in_text_node => {
                return Err(PptxWorkflowError::InvalidXml(story.name.clone()));
            }
            Ok(Event::DocType(_)) => {
                return Err(PptxWorkflowError::InvalidXml(story.name.clone()));
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(_) => return Err(PptxWorkflowError::InvalidXml(story.name.clone())),
        }
        buffer.clear();
    }
    Ok(parts)
}

fn is_text_element(name: &[u8]) -> bool {
    matches!(local_name(name), b"t" | b"text")
}

fn is_embedded_media(name: &str) -> bool {
    name.starts_with("ppt/media/") && !name.ends_with('/')
}

fn is_supported_embedded_image(name: &str) -> bool {
    is_embedded_media(name) && embedded_image_extension(name).is_ok()
}

fn embedded_image_extension(name: &str) -> Result<&'static str, PptxWorkflowError> {
    let extension = name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase());
    match extension.as_deref() {
        Some("png") => Ok("png"),
        Some("jpg" | "jpeg") => Ok("jpg"),
        _ => Err(PptxWorkflowError::UnsupportedEmbeddedImageType(
            name.to_owned(),
        )),
    }
}

fn is_active_content(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with("vbaproject.bin")
        || lower.starts_with("ppt/activex/")
        || lower.starts_with("ppt/ctrlprops/")
}

fn local_name(name: &[u8]) -> &[u8] {
    name.rsplit(|byte| *byte == b':').next().unwrap_or(name)
}

fn parse_xml_events(
    xml: &[u8],
    entry_name: &str,
) -> Result<Vec<Event<'static>>, PptxWorkflowError> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut events = Vec::new();
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::DocType(_)) => {
                return Err(PptxWorkflowError::InvalidXml(entry_name.to_owned()));
            }
            Ok(Event::Eof) => break,
            Ok(event) => events.push(event.into_owned()),
            Err(_) => return Err(PptxWorkflowError::InvalidXml(entry_name.to_owned())),
        }
        buffer.clear();
    }
    Ok(events)
}

fn serialize_xml_events(
    events: &[Event<'static>],
    entry_name: &str,
) -> Result<Vec<u8>, PptxWorkflowError> {
    let mut writer = Writer::new(Vec::new());
    for event in events {
        writer
            .write_event(event.clone())
            .map_err(|_| PptxWorkflowError::InvalidXml(entry_name.to_owned()))?;
    }
    Ok(writer.into_inner())
}

fn decoded_text(event: &Event<'static>, entry_name: &str) -> Result<String, PptxWorkflowError> {
    let Event::Text(text) = event else {
        return Err(PptxWorkflowError::InvalidXml(entry_name.to_owned()));
    };
    let decoded = text
        .decode()
        .map_err(|_| PptxWorkflowError::InvalidXml(entry_name.to_owned()))?;
    unescape(&decoded)
        .map(|value| value.into_owned())
        .map_err(|_| PptxWorkflowError::InvalidXml(entry_name.to_owned()))
}

fn char_slice(value: &str, start: usize, end: usize) -> Option<&str> {
    if start > end {
        return None;
    }
    let byte_start = value
        .char_indices()
        .nth(start)
        .map(|(index, _)| index)
        .unwrap_or(value.len());
    let byte_end = value
        .char_indices()
        .nth(end)
        .map(|(index, _)| index)
        .unwrap_or(value.len());
    (value[..byte_start].chars().count() == start && value[..byte_end].chars().count() == end)
        .then_some(&value[byte_start..byte_end])
}

fn replace_char_range(value: &str, start: usize, end: usize, replacement: &str) -> Option<String> {
    let prefix = char_slice(value, 0, start)?;
    let suffix = char_slice(value, end, value.chars().count())?;
    Some(format!("{prefix}{replacement}{suffix}"))
}

fn validate_pptx_task(task: &PptxTaskDraft) -> Result<(), PptxWorkflowError> {
    task.policy.validate()?;
    if task.policy_id != task.policy.id {
        return Err(PptxWorkflowError::PolicySnapshotMismatch);
    }
    let parts: HashMap<&str, &DocumentPart> = task
        .document
        .parts
        .iter()
        .map(|part| (part.id.as_str(), part))
        .collect();
    if parts.len() != task.document.parts.len() {
        return Err(PptxWorkflowError::InvalidFinding(
            "文档部分 id 重复".to_owned(),
        ));
    }
    let mut selected_ranges: BTreeMap<&str, Vec<(usize, usize)>> = BTreeMap::new();
    for finding in &task.findings {
        let Some(part) = parts.get(finding.part_id.as_str()) else {
            return Err(PptxWorkflowError::InvalidFinding(finding.id.clone()));
        };
        if finding.start >= finding.end
            || finding.end > part.char_len
            || char_slice(&part.text, finding.start, finding.end)
                != Some(finding.matched_text.as_str())
        {
            return Err(PptxWorkflowError::InvalidFinding(finding.id.clone()));
        }
        if finding.selected {
            selected_ranges
                .entry(&finding.part_id)
                .or_default()
                .push((finding.start, finding.end));
        }
    }
    for ranges in selected_ranges.values_mut() {
        ranges.sort_unstable();
        if ranges.windows(2).any(|pair| pair[0].1 > pair[1].0) {
            return Err(PptxWorkflowError::InvalidFinding(
                "选中范围相互重叠".to_owned(),
            ));
        }
    }
    let mut image_entries = BTreeSet::new();
    for embedded in &task.embedded_images {
        if !is_supported_embedded_image(&embedded.entry_name)
            || !image_entries.insert(embedded.entry_name.as_str())
            || embedded.task.policy_id != task.policy_id
            || embedded.task.policy != task.policy
        {
            return Err(PptxWorkflowError::EmbeddedImageTaskMismatch(
                embedded.entry_name.clone(),
            ));
        }
    }
    Ok(())
}

fn image_group_count(
    images: &[PptxEmbeddedImageTask],
    predicate: impl Fn(&crate::model::ImageFinding) -> bool,
) -> usize {
    images
        .iter()
        .flat_map(|image| {
            image
                .task
                .findings
                .iter()
                .filter(|finding| predicate(finding))
                .map(move |finding| (image.entry_name.as_str(), finding.group_id.as_str()))
        })
        .collect::<BTreeSet<_>>()
        .len()
}

fn embedded_tasks_by_entry<'a>(
    task: &'a PptxTaskDraft,
    summary: &PackageSummary,
) -> Result<BTreeMap<&'a str, &'a ImageTaskDraft>, PptxWorkflowError> {
    if let Some(name) = summary.unsupported_embedded_images.first() {
        return Err(PptxWorkflowError::UnsupportedEmbeddedImageType(
            name.clone(),
        ));
    }
    if summary.embedded_images > 0 && task.embedded_images.is_empty() {
        return Err(PptxWorkflowError::EmbeddedImagesUnsupported);
    }
    if summary.embedded_images != task.embedded_images.len()
        || task.document.source.embedded_images != summary.embedded_images
    {
        return Err(PptxWorkflowError::EmbeddedImageTaskMismatch(
            "嵌入图片数量不同".to_owned(),
        ));
    }
    let expected: BTreeSet<&str> = summary
        .entry_names
        .iter()
        .filter(|name| is_supported_embedded_image(name))
        .map(String::as_str)
        .collect();
    let actual: BTreeSet<&str> = task
        .embedded_images
        .iter()
        .map(|image| image.entry_name.as_str())
        .collect();
    if expected != actual {
        return Err(PptxWorkflowError::EmbeddedImageTaskMismatch(
            "嵌入图片路径不同".to_owned(),
        ));
    }
    Ok(task
        .embedded_images
        .iter()
        .map(|image| (image.entry_name.as_str(), &image.task))
        .collect())
}

fn findings_by_locator(task: &PptxTaskDraft) -> BTreeMap<String, Vec<Finding>> {
    let part_locators: HashMap<&str, &str> = task
        .document
        .parts
        .iter()
        .map(|part| (part.id.as_str(), part.locator.as_str()))
        .collect();
    let mut result: BTreeMap<String, Vec<Finding>> = BTreeMap::new();
    for finding in task.findings.iter().filter(|finding| finding.selected) {
        if let Some(locator) = part_locators.get(finding.part_id.as_str()) {
            result
                .entry((*locator).to_owned())
                .or_default()
                .push(finding.clone());
        }
    }
    result
}

fn apply_findings_to_slots(
    events: &mut [Event<'static>],
    slots: &[TextSlot],
    findings: &[Finding],
    entry_name: &str,
) -> Result<(), PptxWorkflowError> {
    let mut ordered = findings.to_vec();
    ordered.sort_by_key(|finding| (finding.start, finding.end));
    if ordered.windows(2).any(|pair| pair[0].end > pair[1].start) {
        return Err(PptxWorkflowError::InvalidFinding(
            "选中范围相互重叠".to_owned(),
        ));
    }
    for finding in ordered.iter().rev() {
        let mut inserted = false;
        for slot in slots {
            if finding.start >= slot.end || finding.end <= slot.start {
                continue;
            }
            let current = decoded_text(&events[slot.event_index], entry_name)?;
            let local_start = finding.start.max(slot.start) - slot.start;
            let local_end = finding.end.min(slot.end) - slot.start;
            let replacement = if !inserted && finding.start >= slot.start {
                inserted = true;
                finding.replacement.as_str()
            } else {
                ""
            };
            let updated = replace_char_range(&current, local_start, local_end, replacement)
                .ok_or_else(|| PptxWorkflowError::InvalidFinding(finding.id.clone()))?;
            events[slot.event_index] = Event::Text(BytesText::new(&updated).into_owned());
            if updated.starts_with(char::is_whitespace) || updated.ends_with(char::is_whitespace) {
                ensure_xml_space(events, slot.event_index);
            }
        }
        if !inserted {
            return Err(PptxWorkflowError::InvalidFinding(finding.id.clone()));
        }
    }
    Ok(())
}

fn ensure_xml_space(events: &mut [Event<'static>], text_index: usize) {
    let Some(Event::Start(start)) = text_index
        .checked_sub(1)
        .and_then(|index| events.get_mut(index))
    else {
        return;
    };
    let already_present = start
        .attributes()
        .with_checks(false)
        .flatten()
        .any(|attribute| attribute.key.as_ref() == b"xml:space");
    if !already_present {
        start.push_attribute(("xml:space", "preserve"));
    }
}

fn redact_story_xml(
    story: &StoryEntry,
    xml: &[u8],
    by_locator: &BTreeMap<String, Vec<Finding>>,
) -> Result<Vec<u8>, PptxWorkflowError> {
    let mut events = parse_xml_events(xml, &story.name)?;
    let mut paragraph_index = 0usize;
    let mut text_node_index = 0usize;
    let mut in_paragraph = false;
    let mut in_text_node = false;
    let mut slots = Vec::new();
    let mut char_count = 0usize;
    for index in 0..events.len() {
        match &events[index] {
            Event::Start(event) if local_name(event.name().as_ref()) == b"p" => {
                in_paragraph = true;
                slots.clear();
                char_count = 0;
            }
            Event::End(event) if local_name(event.name().as_ref()) == b"p" => {
                paragraph_index += 1;
                let locator = format!("{}#p{paragraph_index:06}", story.name);
                if let Some(findings) = by_locator.get(&locator) {
                    apply_findings_to_slots(&mut events, &slots, findings, story.name.as_str())?;
                }
                in_paragraph = false;
                slots.clear();
                char_count = 0;
            }
            Event::Start(event) if is_text_element(event.name().as_ref()) => {
                in_text_node = true;
            }
            Event::End(event) if is_text_element(event.name().as_ref()) => {
                in_text_node = false;
            }
            Event::Text(_) if in_text_node => {
                let text = decoded_text(&events[index], &story.name)?;
                if in_paragraph {
                    let length = text.chars().count();
                    slots.push(TextSlot {
                        event_index: index,
                        start: char_count,
                        end: char_count + length,
                    });
                    char_count += length;
                } else if story.grouping == XmlGrouping::ParagraphAndTextNode && !text.is_empty() {
                    text_node_index += 1;
                    let locator = format!("{}#t{text_node_index:06}", story.name);
                    if let Some(findings) = by_locator.get(&locator) {
                        let length = text.chars().count();
                        apply_findings_to_slots(
                            &mut events,
                            &[TextSlot {
                                event_index: index,
                                start: 0,
                                end: length,
                            }],
                            findings,
                            story.name.as_str(),
                        )?;
                    }
                }
            }
            Event::CData(_) if in_text_node => {
                return Err(PptxWorkflowError::InvalidXml(story.name.clone()));
            }
            _ => {}
        }
    }
    scrub_presentation_metadata_attributes(&mut events)?;
    serialize_xml_events(&events, &story.name)
}

fn scrub_presentation_metadata_attributes(
    events: &mut [Event<'static>],
) -> Result<(), PptxWorkflowError> {
    for event in events {
        match event {
            Event::Start(start) | Event::Empty(start) => {
                let element_name = local_name(start.name().as_ref()).to_vec();
                let mut cleaned = start.to_owned();
                cleaned.clear_attributes();
                for attribute in start.attributes().with_checks(false) {
                    let attribute = attribute.map_err(|_| {
                        PptxWorkflowError::InvalidXml("Presentation XML 属性".to_owned())
                    })?;
                    let key = local_name(attribute.key.as_ref());
                    if element_name == b"author" && key == b"name" {
                        cleaned.push_attribute((attribute.key.as_ref(), b"LlaMask".as_slice()));
                        continue;
                    }
                    if key == b"author" {
                        cleaned.push_attribute((attribute.key.as_ref(), b"LlaMask".as_slice()));
                        continue;
                    }
                    if key == b"initials" {
                        cleaned.push_attribute((attribute.key.as_ref(), b"LM".as_slice()));
                        continue;
                    }
                    if matches!(key, b"date" | b"created" | b"modified") {
                        cleaned.push_attribute((
                            attribute.key.as_ref(),
                            b"2000-01-01T00:00:00Z".as_slice(),
                        ));
                        continue;
                    }
                    if matches!(element_name.as_slice(), b"docPr" | b"cNvPr") && key == b"name" {
                        cleaned.push_attribute((attribute.key.as_ref(), b"".as_slice()));
                        continue;
                    }
                    let is_drawing_metadata =
                        matches!(element_name.as_slice(), b"docPr" | b"cNvPr")
                            && matches!(key, b"name" | b"title" | b"descr");
                    let is_named_container =
                        matches!(element_name.as_slice(), b"cSld" | b"custShow" | b"section")
                            && key == b"name";
                    let is_identity_metadata = matches!(key, b"email" | b"userId" | b"providerId");
                    if !is_drawing_metadata && !is_named_container && !is_identity_metadata {
                        cleaned.push_attribute(attribute.to_owned());
                    }
                }
                *start = cleaned;
            }
            _ => {}
        }
    }
    Ok(())
}

fn scrub_core_properties(xml: &[u8]) -> Result<Vec<u8>, PptxWorkflowError> {
    let entry_name = "docProps/core.xml";
    let events = parse_xml_events(xml, entry_name)?;
    let mut writer = Writer::new(Vec::new());
    let mut depth = 0usize;
    let mut skip_depth = 0usize;
    for event in events {
        if skip_depth > 0 {
            match event {
                Event::Start(_) => skip_depth += 1,
                Event::End(_) => skip_depth -= 1,
                _ => {}
            }
            continue;
        }
        match &event {
            Event::Start(_) => {
                if depth == 1 {
                    skip_depth = 1;
                } else {
                    depth += 1;
                    writer.write_event(event.clone())?;
                }
            }
            Event::End(_) => {
                writer.write_event(event.clone())?;
                depth = depth.saturating_sub(1);
            }
            Event::Empty(_) if depth == 1 => {}
            _ => writer.write_event(event.clone())?,
        }
    }
    Ok(writer.into_inner())
}

fn scrub_app_properties(xml: &[u8]) -> Result<Vec<u8>, PptxWorkflowError> {
    let entry_name = "docProps/app.xml";
    let events = parse_xml_events(xml, entry_name)?;
    let targets = [
        b"Manager".as_slice(),
        b"Company",
        b"Template",
        b"HyperlinkBase",
        b"HeadingPairs",
        b"TitlesOfParts",
    ];
    let mut writer = Writer::new(Vec::new());
    let mut sensitive_stack = Vec::new();
    for event in events {
        match &event {
            Event::Start(start) => {
                let inherited = sensitive_stack.last().copied().unwrap_or(false);
                let sensitive = inherited || targets.contains(&local_name(start.name().as_ref()));
                sensitive_stack.push(sensitive);
                writer.write_event(event.clone())?;
            }
            Event::End(_) => {
                writer.write_event(event.clone())?;
                sensitive_stack.pop();
            }
            Event::Text(_) | Event::CData(_)
                if sensitive_stack.last().copied().unwrap_or(false) => {}
            _ => writer.write_event(event.clone())?,
        }
    }
    Ok(writer.into_inner())
}

fn scrub_presentation_xml(xml: &[u8], entry_name: &str) -> Result<Vec<u8>, PptxWorkflowError> {
    let mut events = parse_xml_events(xml, entry_name)?;
    scrub_presentation_metadata_attributes(&mut events)?;
    serialize_xml_events(&events, entry_name)
}

fn attribute_raw(start: &BytesStart<'_>, local_key: &[u8]) -> Option<String> {
    start
        .attributes()
        .with_checks(false)
        .flatten()
        .find(|attribute| local_name(attribute.key.as_ref()) == local_key)
        .map(|attribute| String::from_utf8_lossy(attribute.value.as_ref()).into_owned())
}

fn should_drop_relationship(start: &BytesStart<'_>) -> bool {
    let target = attribute_raw(start, b"Target")
        .unwrap_or_default()
        .to_ascii_lowercase();
    let relation_type = attribute_raw(start, b"Type")
        .unwrap_or_default()
        .to_ascii_lowercase();
    target.contains("customxml/")
        || target.contains("customdata/")
        || target.contains("printersettings/")
        || target.contains("tags/")
        || target.ends_with("docprops/custom.xml")
        || target.contains("thumbnail.")
        || relation_type.ends_with("/custom-properties")
        || relation_type.ends_with("/thumbnail")
        || relation_type.ends_with("/customdata")
        || relation_type.ends_with("/printersettings")
        || relation_type.ends_with("/tags")
}

fn sanitize_external_relationship(
    start: &BytesStart<'_>,
) -> Result<BytesStart<'static>, PptxWorkflowError> {
    let target_mode = attribute_raw(start, b"TargetMode").unwrap_or_default();
    if !target_mode.eq_ignore_ascii_case("External") {
        return Ok(start.to_owned());
    }
    let relation_type = attribute_raw(start, b"Type").unwrap_or_default();
    if !relation_type.to_ascii_lowercase().ends_with("/hyperlink") {
        return Err(PptxWorkflowError::ExternalRelationshipUnsupported(
            relation_type,
        ));
    }
    let mut cleaned = start.to_owned();
    cleaned.clear_attributes();
    for attribute in start.attributes().with_checks(false) {
        let attribute =
            attribute.map_err(|_| PptxWorkflowError::InvalidXml("关系 XML 属性".to_owned()))?;
        if local_name(attribute.key.as_ref()) == b"Target" {
            cleaned.push_attribute((attribute.key.as_ref(), b"about:blank".as_slice()));
        } else {
            cleaned.push_attribute(attribute.to_owned());
        }
    }
    Ok(cleaned.into_owned())
}

fn scrub_relationships(xml: &[u8], entry_name: &str) -> Result<Vec<u8>, PptxWorkflowError> {
    let events = parse_xml_events(xml, entry_name)?;
    let mut writer = Writer::new(Vec::new());
    let mut skip_depth = 0usize;
    for event in events {
        if skip_depth > 0 {
            match event {
                Event::Start(_) => skip_depth += 1,
                Event::End(_) => skip_depth -= 1,
                _ => {}
            }
            continue;
        }
        match event {
            Event::Start(start) if local_name(start.name().as_ref()) == b"Relationship" => {
                if should_drop_relationship(&start) {
                    skip_depth = 1;
                } else {
                    writer.write_event(Event::Start(sanitize_external_relationship(&start)?))?;
                }
            }
            Event::Empty(start) if local_name(start.name().as_ref()) == b"Relationship" => {
                if !should_drop_relationship(&start) {
                    writer.write_event(Event::Empty(sanitize_external_relationship(&start)?))?;
                }
            }
            _ => writer.write_event(event)?,
        }
    }
    Ok(writer.into_inner())
}

fn should_drop_content_type(start: &BytesStart<'_>) -> bool {
    let part_name = attribute_raw(start, b"PartName")
        .unwrap_or_default()
        .to_ascii_lowercase();
    part_name.starts_with("/customxml/")
        || part_name.starts_with("/ppt/customdata/")
        || part_name.starts_with("/ppt/printersettings/")
        || part_name.starts_with("/ppt/tags/")
        || part_name == "/docprops/custom.xml"
        || part_name.contains("thumbnail.")
}

fn scrub_content_types(xml: &[u8]) -> Result<Vec<u8>, PptxWorkflowError> {
    let entry_name = "[Content_Types].xml";
    let events = parse_xml_events(xml, entry_name)?;
    let mut writer = Writer::new(Vec::new());
    let mut skip_depth = 0usize;
    for event in events {
        if skip_depth > 0 {
            match event {
                Event::Start(_) => skip_depth += 1,
                Event::End(_) => skip_depth -= 1,
                _ => {}
            }
            continue;
        }
        match &event {
            Event::Start(start)
                if local_name(start.name().as_ref()) == b"Override"
                    && should_drop_content_type(start) =>
            {
                skip_depth = 1;
            }
            Event::Empty(start)
                if local_name(start.name().as_ref()) == b"Override"
                    && should_drop_content_type(start) => {}
            _ => writer.write_event(event.clone())?,
        }
    }
    Ok(writer.into_inner())
}

fn should_drop_entry(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with("customxml/")
        || lower.starts_with("ppt/customdata/")
        || lower.starts_with("ppt/printersettings/")
        || lower.starts_with("ppt/tags/")
        || lower == "docprops/custom.xml"
        || (lower.starts_with("docprops/thumbnail.") && !lower.ends_with('/'))
}

fn is_presentation_metadata_xml(name: &str) -> bool {
    matches!(
        name,
        "ppt/presentation.xml"
            | "ppt/authors.xml"
            | "ppt/commentAuthors.xml"
            | "ppt/presProps.xml"
            | "ppt/viewProps.xml"
    )
}

fn transform_package_entry(
    name: &str,
    data: &[u8],
    by_locator: &BTreeMap<String, Vec<Finding>>,
) -> Result<Vec<u8>, PptxWorkflowError> {
    if let Some(story) = story_entry(name) {
        return redact_story_xml(&story, data, by_locator);
    }
    if name.ends_with(".rels") {
        return scrub_relationships(data, name);
    }
    if is_presentation_metadata_xml(name) {
        return scrub_presentation_xml(data, name);
    }
    match name {
        "[Content_Types].xml" => scrub_content_types(data),
        "docProps/core.xml" => scrub_core_properties(data),
        "docProps/app.xml" => scrub_app_properties(data),
        _ => Ok(data.to_vec()),
    }
}

fn build_redacted_package(
    task: &PptxTaskDraft,
    source_bytes: &[u8],
    runtimes: Option<&RuntimeRegistry>,
) -> Result<Vec<u8>, PptxWorkflowError> {
    let summary = validate_package(source_bytes)?;
    if summary.embedded_objects > 0 {
        return Err(PptxWorkflowError::EmbeddedObjectsUnsupported);
    }
    if summary.active_content > 0 {
        return Err(PptxWorkflowError::ActiveContentUnsupported);
    }
    let embedded_tasks = embedded_tasks_by_entry(task, &summary)?;
    let image_runtimes = if embedded_tasks.is_empty() {
        None
    } else {
        Some(runtimes.ok_or(PptxWorkflowError::EmbeddedImageRuntimeRequired)?)
    };
    let by_locator = findings_by_locator(task);
    let mut source = ZipArchive::new(Cursor::new(source_bytes))?;
    let mut target = ZipWriter::new(Cursor::new(Vec::new()));
    for index in 0..source.len() {
        let mut entry = source.by_index(index)?;
        let name = entry.name().to_owned();
        if entry.is_dir() || should_drop_entry(&name) {
            continue;
        }
        let compression = entry.compression();
        let mut data = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut data)?;
        let transformed = if let Some(image_task) = embedded_tasks.get(name.as_str()) {
            redact_embedded_image(
                &name,
                &data,
                image_task,
                image_runtimes.expect("image runtimes checked above"),
            )?
        } else {
            transform_package_entry(&name, &data, &by_locator)?
        };
        let options = SimpleFileOptions::default()
            .compression_method(compression)
            .unix_permissions(0o644);
        target.start_file(name, options)?;
        target.write_all(&transformed)?;
    }
    Ok(target.finish()?.into_inner())
}

fn redact_embedded_image(
    entry_name: &str,
    source_bytes: &[u8],
    task: &ImageTaskDraft,
    runtimes: &RuntimeRegistry,
) -> Result<Vec<u8>, PptxWorkflowError> {
    let temporary = tempdir()?;
    let extension = embedded_image_extension(entry_name)?;
    let input = temporary.path().join(format!("source.{extension}"));
    let output = temporary.path().join(format!("redacted.{extension}"));
    fs::write(&input, source_bytes)?;
    let mut file_task = task.clone();
    file_task.source.path = input.to_string_lossy().into_owned();
    export_image_task_with_runtimes(&file_task, &output, runtimes)?;
    Ok(fs::read(output)?)
}

fn xml_contains_nonempty_text(xml: &[u8], entry_name: &str) -> Result<bool, PptxWorkflowError> {
    Ok(parse_xml_events(xml, entry_name)?.iter().any(|event| {
        matches!(event, Event::CData(value) if !value.is_empty())
            || matches!(event, Event::Text(value) if value.decode().is_ok_and(|text| !text.trim().is_empty()))
    }))
}

fn app_properties_are_scrubbed(xml: &[u8]) -> Result<bool, PptxWorkflowError> {
    let entry_name = "docProps/app.xml";
    let targets = [
        b"Manager".as_slice(),
        b"Company",
        b"Template",
        b"HyperlinkBase",
        b"HeadingPairs",
        b"TitlesOfParts",
    ];
    let events = parse_xml_events(xml, entry_name)?;
    let mut sensitive_stack = Vec::new();
    for event in events {
        match event {
            Event::Start(start) => {
                let inherited = sensitive_stack.last().copied().unwrap_or(false);
                sensitive_stack
                    .push(inherited || targets.contains(&local_name(start.name().as_ref())));
            }
            Event::End(_) => {
                sensitive_stack.pop();
            }
            Event::Text(text) if sensitive_stack.last().copied().unwrap_or(false) => {
                let decoded = text
                    .decode()
                    .map_err(|_| PptxWorkflowError::InvalidXml(entry_name.to_owned()))?;
                if !decoded.trim().is_empty() {
                    return Ok(false);
                }
            }
            Event::CData(value)
                if sensitive_stack.last().copied().unwrap_or(false) && !value.is_empty() =>
            {
                return Ok(false);
            }
            _ => {}
        }
    }
    Ok(true)
}

fn presentation_metadata_attributes_are_scrubbed(
    xml: &[u8],
    entry_name: &str,
) -> Result<bool, PptxWorkflowError> {
    for event in parse_xml_events(xml, entry_name)? {
        let (Event::Start(start) | Event::Empty(start)) = event else {
            continue;
        };
        let element_name = local_name(start.name().as_ref()).to_vec();
        for attribute in start.attributes().with_checks(false) {
            let attribute =
                attribute.map_err(|_| PptxWorkflowError::InvalidXml(entry_name.to_owned()))?;
            let key = local_name(attribute.key.as_ref());
            let value = String::from_utf8_lossy(attribute.value.as_ref());
            if (element_name == b"author" && key == b"name" && value != "LlaMask")
                || (key == b"author" && value != "LlaMask")
                || (key == b"initials" && value != "LM")
                || (matches!(key, b"date" | b"created" | b"modified")
                    && value != "2000-01-01T00:00:00Z")
                || matches!(key, b"email" | b"userId" | b"providerId")
                || (matches!(element_name.as_slice(), b"docPr" | b"cNvPr")
                    && matches!(key, b"name" | b"title" | b"descr")
                    && !value.is_empty())
                || (matches!(element_name.as_slice(), b"cSld" | b"custShow" | b"section")
                    && key == b"name")
            {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

fn relationships_are_scrubbed(xml: &[u8], entry_name: &str) -> Result<bool, PptxWorkflowError> {
    for event in parse_xml_events(xml, entry_name)? {
        let (Event::Start(start) | Event::Empty(start)) = event else {
            continue;
        };
        if local_name(start.name().as_ref()) != b"Relationship" {
            continue;
        }
        if should_drop_relationship(&start) {
            return Ok(false);
        }
        if attribute_raw(&start, b"TargetMode")
            .is_some_and(|value| value.eq_ignore_ascii_case("External"))
            && attribute_raw(&start, b"Target").as_deref() != Some("about:blank")
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn package_metadata_is_scrubbed(bytes: &[u8]) -> Result<bool, PptxWorkflowError> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))?;
    if !archive.comment().is_empty() {
        return Ok(false);
    }
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let name = entry.name().to_owned();
        if !entry.comment().is_empty() || should_drop_entry(&name) {
            return Ok(false);
        }
        if entry.is_dir() || !name.ends_with(".xml") && !name.ends_with(".rels") {
            continue;
        }
        let mut data = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut data)?;
        if name == "docProps/core.xml" && xml_contains_nonempty_text(&data, &name)? {
            return Ok(false);
        }
        if name == "docProps/app.xml" && !app_properties_are_scrubbed(&data)? {
            return Ok(false);
        }
        if (story_entry(&name).is_some() || is_presentation_metadata_xml(&name))
            && !presentation_metadata_attributes_are_scrubbed(&data, &name)?
        {
            return Ok(false);
        }
        if name.ends_with(".rels") && !relationships_are_scrubbed(&data, &name)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn append_target_residuals(
    task: &PptxTaskDraft,
    output_parts: &[DocumentPart],
    residuals: &mut Vec<PptxResidualFinding>,
    seen: &mut BTreeSet<(String, usize, usize, EntityType)>,
) {
    let output_by_locator: HashMap<&str, &DocumentPart> = output_parts
        .iter()
        .map(|part| (part.locator.as_str(), part))
        .collect();
    let source_by_id: HashMap<&str, &DocumentPart> = task
        .document
        .parts
        .iter()
        .map(|part| (part.id.as_str(), part))
        .collect();
    for finding in task.findings.iter().filter(|finding| finding.selected) {
        let Some(source_part) = source_by_id.get(finding.part_id.as_str()) else {
            continue;
        };
        let Some(output_part) = output_by_locator.get(source_part.locator.as_str()) else {
            continue;
        };
        for (byte_start, matched) in output_part.text.match_indices(&finding.matched_text) {
            let start = output_part.text[..byte_start].chars().count();
            let end = start + matched.chars().count();
            let key = (output_part.id.clone(), start, end, finding.entity_type);
            if seen.insert(key) {
                residuals.push(PptxResidualFinding {
                    part_id: output_part.id.clone(),
                    start,
                    end,
                    entity_type: finding.entity_type,
                    detector: "target_value_check".to_owned(),
                    explanation_code: "SELECTED_VALUE_REMAINS".to_owned(),
                });
            }
        }
    }
}

fn verify_pptx_bytes_with_runtimes(
    task: &PptxTaskDraft,
    bytes: &[u8],
    checked_file: &str,
    runtimes: Option<&RuntimeRegistry>,
) -> Result<PptxVerificationReport, PptxWorkflowError> {
    validate_pptx_task(task)?;
    let summary = validate_package(bytes)?;
    let output_parts = extract_document_parts(bytes, &summary.entry_names)?;
    let unreviewed_findings = task
        .findings
        .iter()
        .filter(|finding| !finding.reviewed)
        .count()
        + image_group_count(&task.embedded_images, |finding| !finding.reviewed);
    let source_by_id: HashMap<&str, &DocumentPart> = task
        .document
        .parts
        .iter()
        .map(|part| (part.id.as_str(), part))
        .collect();
    let accepted_keep: BTreeSet<(String, EntityType, String)> = task
        .findings
        .iter()
        .filter(|finding| finding.reviewed && !finding.selected)
        .filter_map(|finding| {
            source_by_id.get(finding.part_id.as_str()).map(|part| {
                (
                    part.locator.clone(),
                    finding.entity_type,
                    finding.matched_text.clone(),
                )
            })
        })
        .collect();
    let mut residual_findings = Vec::new();
    let mut seen = BTreeSet::new();
    append_target_residuals(task, &output_parts, &mut residual_findings, &mut seen);
    let mut target_residual_count = residual_findings.len();
    let output_sha256 = sha256_hex(bytes);
    let mut diagnostics = Vec::new();
    let mut checked_intersection: Option<BTreeSet<String>> = None;
    for part in &output_parts {
        let detection = run_detection(
            &part.text,
            &FileKind::Text,
            &output_sha256,
            &part.id,
            &task.policy,
            runtimes,
            "pptx-verify",
        )?;
        let checked: BTreeSet<String> = detection.detectors_checked.iter().cloned().collect();
        match &mut checked_intersection {
            Some(intersection) => intersection.retain(|id| checked.contains(id)),
            None => checked_intersection = Some(checked),
        }
        diagnostics.extend(detection.diagnostics);
        for finding in detection.findings {
            if accepted_keep.contains(&(
                part.locator.clone(),
                finding.entity_type,
                finding.matched_text.clone(),
            )) {
                continue;
            }
            let key = (
                part.id.clone(),
                finding.start,
                finding.end,
                finding.entity_type,
            );
            if seen.insert(key) {
                residual_findings.push(PptxResidualFinding {
                    part_id: part.id.clone(),
                    start: finding.start,
                    end: finding.end,
                    entity_type: finding.entity_type,
                    detector: finding.detector,
                    explanation_code: finding.explanation_code,
                });
            }
        }
    }
    let text_detectors_complete = task
        .policy
        .detectors
        .iter()
        .filter(|detector| detector.enabled)
        .all(|detector| {
            output_parts.is_empty()
                || checked_intersection
                    .as_ref()
                    .is_some_and(|checked| checked.contains(&detector.id))
        });

    let embedded_tasks = embedded_tasks_by_entry(task, &summary)?;
    let image_runtimes = if embedded_tasks.is_empty() {
        None
    } else {
        Some(runtimes.ok_or(PptxWorkflowError::EmbeddedImageRuntimeRequired)?)
    };
    let mut image_residual_findings = Vec::new();
    let mut embedded_images_checked = 0usize;
    let mut embedded_images_passed = true;
    let mut embedded_images_complete = true;
    let mut image_detectors_checked = BTreeSet::new();
    if let Some(image_runtimes) = image_runtimes {
        let temporary = tempdir()?;
        let mut archive = ZipArchive::new(Cursor::new(bytes))?;
        for (index, (entry_name, image_task)) in embedded_tasks.iter().enumerate() {
            let mut entry = archive.by_name(entry_name)?;
            let mut image_bytes = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut image_bytes)?;
            let extension = embedded_image_extension(entry_name)?;
            let image_path = temporary
                .path()
                .join(format!("verify-{index:05}.{extension}"));
            fs::write(&image_path, image_bytes)?;
            let report = verify_image_file_with_runtimes(image_task, &image_path, image_runtimes)?;
            embedded_images_checked += 1;
            embedded_images_passed &= report.passed;
            embedded_images_complete &= report.complete;
            target_residual_count += report.target_residual_count;
            diagnostics.extend(report.diagnostics);
            image_detectors_checked.extend(report.detectors_checked);
            image_residual_findings.extend(report.residual_findings.into_iter().map(|finding| {
                PptxImageResidualFinding {
                    entry_name: (*entry_name).to_owned(),
                    line_index: finding.line_index,
                    entity_type: finding.entity_type,
                    detector: finding.detector,
                    explanation_code: finding.explanation_code,
                    ocr_rect: finding.ocr_rect,
                }
            }));
        }
    }
    let metadata_scrubbed = package_metadata_is_scrubbed(bytes)?;
    if !metadata_scrubbed {
        diagnostics.push(TaskDiagnostic {
            severity: DiagnosticSeverity::Error,
            code: "PPTX_METADATA_NOT_SCRUBBED".to_owned(),
            detector_id: None,
            message: "独立校验发现文档或 ZIP 元数据尚未完全清理。".to_owned(),
        });
    }
    if !embedded_images_passed {
        diagnostics.push(TaskDiagnostic {
            severity: DiagnosticSeverity::Error,
            code: "PPTX_EMBEDDED_IMAGE_RESIDUALS".to_owned(),
            detector_id: None,
            message: "独立 OCR 复扫在嵌入图片中发现未处理结果。".to_owned(),
        });
    }
    if summary.embedded_objects > 0 || summary.active_content > 0 {
        diagnostics.push(TaskDiagnostic {
            severity: DiagnosticSeverity::Error,
            code: "PPTX_UNSUPPORTED_PAYLOAD_REMAINS".to_owned(),
            detector_id: None,
            message: "文档仍包含当前版本无法安全验证的图表、嵌入对象、外部数据或主动内容。"
                .to_owned(),
        });
    }
    deduplicate_diagnostics(&mut diagnostics);
    residual_findings.sort_by(|left, right| {
        left.part_id
            .cmp(&right.part_id)
            .then_with(|| left.start.cmp(&right.start))
            .then_with(|| left.end.cmp(&right.end))
            .then_with(|| left.entity_type.cmp(&right.entity_type))
    });
    image_residual_findings.sort_by(|left, right| {
        left.entry_name
            .cmp(&right.entry_name)
            .then_with(|| left.line_index.cmp(&right.line_index))
            .then_with(|| left.entity_type.cmp(&right.entity_type))
    });
    let mut detectors_checked: BTreeSet<String> = checked_intersection
        .unwrap_or_else(|| BTreeSet::from(["deterministic_rules_v1".to_owned()]))
        .into_iter()
        .collect();
    detectors_checked.extend(image_detectors_checked);
    let detectors_checked: Vec<String> = detectors_checked.into_iter().collect();
    let complete = text_detectors_complete
        && embedded_images_complete
        && embedded_images_checked == summary.embedded_images
        && summary.embedded_objects == 0
        && summary.active_content == 0;
    Ok(PptxVerificationReport {
        schema_version: 2,
        passed: unreviewed_findings == 0
            && residual_findings.is_empty()
            && image_residual_findings.is_empty()
            && embedded_images_passed
            && metadata_scrubbed
            && embedded_images_checked == summary.embedded_images
            && summary.embedded_objects == 0
            && summary.active_content == 0,
        complete,
        checked_file: checked_file.to_owned(),
        sha256: output_sha256,
        selected_findings: task
            .findings
            .iter()
            .filter(|finding| finding.selected)
            .count()
            + image_group_count(&task.embedded_images, |finding| finding.selected),
        unreviewed_findings,
        target_residual_count,
        residual_findings,
        image_residual_findings,
        detectors_checked,
        package_entries_checked: summary.entry_names.len(),
        metadata_scrubbed,
        embedded_images_checked,
        diagnostics,
    })
}

pub fn verify_pptx_file_with_runtimes(
    task: &PptxTaskDraft,
    path: &Path,
    runtimes: Option<&RuntimeRegistry>,
) -> Result<PptxVerificationReport, PptxWorkflowError> {
    let bytes = fs::read(path)?;
    verify_pptx_bytes_with_runtimes(task, &bytes, &path.to_string_lossy(), runtimes)
}

pub fn export_pptx_task_with_runtimes(
    task: &PptxTaskDraft,
    output: &Path,
    runtimes: Option<&RuntimeRegistry>,
) -> Result<PptxVerificationReport, PptxWorkflowError> {
    validate_pptx_task(task)?;
    if output
        .extension()
        .and_then(|extension| extension.to_str())
        .is_none_or(|extension| !extension.eq_ignore_ascii_case("pptx"))
    {
        return Err(PptxWorkflowError::OutputTypeMismatch);
    }
    let unreviewed = task
        .findings
        .iter()
        .filter(|finding| !finding.reviewed)
        .count()
        + image_group_count(&task.embedded_images, |finding| !finding.reviewed);
    if unreviewed > 0 {
        return Err(PptxWorkflowError::UnreviewedFindings(unreviewed));
    }
    let source_path = fs::canonicalize(&task.document.source.path)?;
    let output_absolute = if output.is_absolute() {
        output.to_path_buf()
    } else {
        std::env::current_dir()?.join(output)
    };
    if source_path == output_absolute {
        return Err(PptxWorkflowError::WouldOverwriteSource);
    }
    if fs::symlink_metadata(output).is_ok() {
        if fs::canonicalize(output).is_ok_and(|existing| existing == source_path) {
            return Err(PptxWorkflowError::WouldOverwriteSource);
        }
        return Err(PptxWorkflowError::OutputExists(output.to_path_buf()));
    }
    let source_bytes = fs::read(&source_path)?;
    if sha256_hex(&source_bytes) != task.document.source.sha256 {
        return Err(PptxWorkflowError::SourceChanged);
    }
    let output_bytes = build_redacted_package(task, &source_bytes, runtimes)?;
    let report =
        verify_pptx_bytes_with_runtimes(task, &output_bytes, &output.to_string_lossy(), runtimes)?;
    if !report.passed {
        return Err(PptxWorkflowError::VerificationFailed(
            report.unreviewed_findings
                + report.residual_findings.len()
                + report.image_residual_findings.len()
                + usize::from(!report.metadata_scrubbed),
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
                PptxWorkflowError::OutputExists(output.to_path_buf())
            }
            _ => PptxWorkflowError::Io(error.error),
        })?;
    Ok(report)
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
    use std::collections::BTreeSet;
    use std::fs;
    use std::io::{Cursor, Read, Write};
    use std::path::PathBuf;

    use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
    use tempfile::tempdir;
    use zip::write::SimpleFileOptions;
    use zip::{ZipArchive, ZipWriter};

    use crate::model::{
        EntityType, ImageFileKind, ImageSourceMetadata, ImageTaskDraft, PptxEmbeddedImageTask,
        PptxTaskDraft,
    };
    use crate::policy::PolicyConfig;
    use crate::text::sha256_hex;

    use super::{
        PptxWorkflowError, export_pptx_task_with_runtimes, render_pptx_embedded_image_preview,
        review_pptx_finding, scan_pptx_with_policy, verify_pptx_file_with_runtimes,
    };

    fn fixture_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/pptx/comprehensive.pptx")
    }

    fn policy_without_models() -> PolicyConfig {
        let mut policy = PolicyConfig::default();
        policy.detectors.clear();
        policy
    }

    fn review_all(task: &mut PptxTaskDraft) {
        for finding in &mut task.findings {
            finding.selected = true;
            finding.reviewed = true;
        }
    }

    fn read_entry(bytes: &[u8], name: &str) -> Vec<u8> {
        let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut entry = archive.by_name(name).unwrap();
        let mut data = Vec::new();
        entry.read_to_end(&mut data).unwrap();
        data
    }

    fn copy_fixture_with_extra_entry(name: &str, data: &[u8]) -> Vec<u8> {
        let original = fs::read(fixture_path()).unwrap();
        let mut source = ZipArchive::new(Cursor::new(&original)).unwrap();
        let mut target = ZipWriter::new(Cursor::new(Vec::new()));
        for index in 0..source.len() {
            let mut entry = source.by_index(index).unwrap();
            if entry.is_dir() {
                continue;
            }
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            target
                .start_file(entry.name(), SimpleFileOptions::default())
                .unwrap();
            target.write_all(&bytes).unwrap();
        }
        target
            .start_file(name, SimpleFileOptions::default())
            .unwrap();
        target.write_all(data).unwrap();
        target.finish().unwrap().into_inner()
    }

    #[test]
    fn comprehensive_fixture_scans_runs_tables_notes_comments_hidden_master_and_layout() {
        let task = scan_pptx_with_policy(&fixture_path(), &policy_without_models(), None).unwrap();
        assert_eq!(task.document.source.slides, 3);
        assert_eq!(task.document.source.embedded_images, 0);

        let finding_kinds: BTreeSet<&str> = task
            .findings
            .iter()
            .map(|finding| {
                task.document
                    .parts
                    .iter()
                    .find(|part| part.id == finding.part_id)
                    .unwrap()
                    .kind
                    .as_str()
            })
            .collect();
        for expected in ["slide", "notes", "comment", "slide_master", "slide_layout"] {
            assert!(finding_kinds.contains(expected), "missing {expected}");
        }
        for expected in [
            "13800000001",
            "pptx@example.com",
            "notes.pptx@example.com",
            "comment.pptx@example.com",
            "reply.pptx@example.com",
            "hidden.pptx@example.com",
            "master.pptx@example.com",
            "13700000003",
        ] {
            assert!(
                task.findings
                    .iter()
                    .any(|finding| finding.matched_text == expected),
                "missing {expected}"
            );
        }
        assert_eq!(
            task.findings
                .iter()
                .filter(|finding| finding.entity_type == EntityType::PhoneNumber
                    && finding.matched_text == "13800000001")
                .count(),
            1
        );
    }

    #[test]
    fn text_review_updates_only_the_requested_finding() {
        let source = fixture_path();
        let source_before = fs::read(&source).unwrap();
        let mut task = scan_pptx_with_policy(&source, &policy_without_models(), None).unwrap();
        let finding_id = task.findings[0].id.clone();
        let untouched_id = task.findings[1].id.clone();
        let untouched_before = task.findings[1].clone();

        review_pptx_finding(&mut task, &finding_id, true, Some("[演示文稿内容]")).unwrap();

        let reviewed = task
            .findings
            .iter()
            .find(|finding| finding.id == finding_id)
            .unwrap();
        assert!(reviewed.reviewed);
        assert!(reviewed.selected);
        assert_eq!(reviewed.replacement, "[演示文稿内容]");
        assert_eq!(
            task.findings
                .iter()
                .find(|finding| finding.id == untouched_id)
                .unwrap(),
            &untouched_before
        );
        assert_eq!(fs::read(source).unwrap(), source_before);

        assert!(matches!(
            review_pptx_finding(&mut task, &finding_id, true, Some(&"x".repeat(257))),
            Err(PptxWorkflowError::TextDetection(
                crate::workflow::WorkflowError::ReplacementTooLong
            ))
        ));
    }

    #[test]
    fn export_redacts_structural_text_scrubs_links_and_authors_and_preserves_theme() {
        let mut task =
            scan_pptx_with_policy(&fixture_path(), &policy_without_models(), None).unwrap();
        review_all(&mut task);
        let temporary = tempdir().unwrap();
        let output = temporary.path().join("redacted.pptx");
        let report = export_pptx_task_with_runtimes(&task, &output, None).unwrap();
        assert!(report.passed);
        assert!(report.complete);
        assert!(report.metadata_scrubbed);
        assert!(report.residual_findings.is_empty());

        let source = fs::read(fixture_path()).unwrap();
        let redacted = fs::read(&output).unwrap();
        assert_eq!(
            read_entry(&source, "ppt/theme/theme1.xml"),
            read_entry(&redacted, "ppt/theme/theme1.xml")
        );
        let slide3 = String::from_utf8(read_entry(&redacted, "ppt/slides/slide3.xml")).unwrap();
        assert!(slide3.contains("show=\"0\""));
        assert!(!slide3.contains("hidden.pptx@example.com"));
        let master =
            String::from_utf8(read_entry(&redacted, "ppt/slideMasters/slideMaster1.xml")).unwrap();
        assert!(!master.contains("master.pptx@example.com"));
        let relationships =
            String::from_utf8(read_entry(&redacted, "ppt/slides/_rels/slide1.xml.rels")).unwrap();
        assert!(relationships.contains("about:blank"));
        assert!(!relationships.contains("link.pptx@example.com"));
        let authors = String::from_utf8(read_entry(&redacted, "ppt/authors.xml")).unwrap();
        assert!(authors.contains("LlaMask"));
        assert!(!authors.contains("Sensitive Reviewer"));

        let verified = verify_pptx_file_with_runtimes(&task, &output, None).unwrap();
        assert_eq!(verified, report);
        let serialized = serde_json::to_string(&report).unwrap();
        assert!(!serialized.contains("pptx@example.com"));
        assert!(!serialized.contains("13800000001"));
    }

    #[test]
    fn embedded_image_without_ocr_task_is_blocked() {
        let png = b"\x89PNG\r\n\x1a\n";
        let bytes = copy_fixture_with_extra_entry("ppt/media/image1.png", png);
        let temporary = tempdir().unwrap();
        let input = temporary.path().join("image.pptx");
        fs::write(&input, bytes).unwrap();
        let mut task = scan_pptx_with_policy(&input, &policy_without_models(), None).unwrap();
        review_all(&mut task);
        let error =
            export_pptx_task_with_runtimes(&task, &temporary.path().join("redacted.pptx"), None)
                .unwrap_err();
        assert!(matches!(
            error,
            PptxWorkflowError::EmbeddedImagesUnsupported
        ));
    }

    #[test]
    fn embedded_image_preview_revalidates_and_reencodes_the_source() {
        let temporary = tempdir().unwrap();
        let source = temporary.path().join("with-preview-image.pptx");
        let mut image_bytes = Vec::new();
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(1, 1, Rgba([12, 34, 56, 255])))
            .write_to(&mut Cursor::new(&mut image_bytes), ImageFormat::Png)
            .unwrap();
        fs::write(
            &source,
            copy_fixture_with_extra_entry("ppt/media/image1.png", &image_bytes),
        )
        .unwrap();
        let mut task = scan_pptx_with_policy(&source, &policy_without_models(), None).unwrap();
        task.embedded_images.push(PptxEmbeddedImageTask {
            entry_name: "ppt/media/image1.png".to_owned(),
            task: ImageTaskDraft {
                schema_version: 1,
                task_id: "pptx-preview-test".to_owned(),
                policy_id: task.policy_id.clone(),
                policy: task.policy.clone(),
                source: ImageSourceMetadata {
                    path: "pptx://test#ppt/media/image1.png".to_owned(),
                    sha256: sha256_hex(&image_bytes),
                    size_bytes: image_bytes.len() as u64,
                    file_kind: ImageFileKind::Png,
                    width: 1,
                    height: 1,
                },
                ocr_runtime_id: "pp_ocr_small".to_owned(),
                findings: Vec::new(),
                diagnostics: Vec::new(),
                contains_sensitive_plaintext: true,
            },
        });

        let preview = render_pptx_embedded_image_preview(&task, 1).unwrap();
        assert_eq!((preview.width, preview.height), (1, 1));
        assert!(preview.png_bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!(matches!(
            render_pptx_embedded_image_preview(&task, 2),
            Err(PptxWorkflowError::EmbeddedImageTaskMismatch(_))
        ));
    }

    #[test]
    fn chart_payload_is_blocked_fail_closed() {
        let bytes = copy_fixture_with_extra_entry(
            "ppt/charts/chart1.xml",
            br#"<?xml version="1.0"?><c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart"/>"#,
        );
        let temporary = tempdir().unwrap();
        let input = temporary.path().join("chart.pptx");
        fs::write(&input, bytes).unwrap();
        let mut task = scan_pptx_with_policy(&input, &policy_without_models(), None).unwrap();
        review_all(&mut task);
        let error =
            export_pptx_task_with_runtimes(&task, &temporary.path().join("redacted.pptx"), None)
                .unwrap_err();
        assert!(matches!(
            error,
            PptxWorkflowError::EmbeddedObjectsUnsupported
        ));
    }

    #[test]
    fn replacement_that_is_still_sensitive_fails_independent_rescan() {
        let mut task =
            scan_pptx_with_policy(&fixture_path(), &policy_without_models(), None).unwrap();
        review_all(&mut task);
        task.findings
            .iter_mut()
            .find(|finding| finding.matched_text == "13800000001")
            .unwrap()
            .replacement = "13900000099".to_owned();
        let temporary = tempdir().unwrap();
        let output = temporary.path().join("redacted.pptx");
        let error = export_pptx_task_with_runtimes(&task, &output, None).unwrap_err();
        assert!(matches!(error, PptxWorkflowError::VerificationFailed(_)));
        assert!(!output.exists());
    }
}
