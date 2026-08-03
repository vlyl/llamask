use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use quick_xml::Reader;
use quick_xml::escape::unescape;
use quick_xml::events::{BytesEnd, BytesStart, BytesText, Event};
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
    TaskDiagnostic, XlsxDocumentGraph, XlsxEmbeddedImageTask, XlsxImageResidualFinding,
    XlsxResidualFinding, XlsxSourceMetadata, XlsxTaskDraft, XlsxVerificationReport,
};
use crate::policy::{PolicyConfig, PolicyError};
use crate::sidecar::RuntimeRegistry;
use crate::text::{apply_findings, sha256_hex};
use crate::workflow::{WorkflowError, review_finding, run_detection};

const MAX_XLSX_BYTES: usize = 100 * 1024 * 1024;
const MAX_PACKAGE_ENTRIES: usize = 10_000;
const MAX_ENTRY_BYTES: u64 = 100 * 1024 * 1024;
const MAX_XML_BYTES: u64 = 20 * 1024 * 1024;
const MAX_TOTAL_UNCOMPRESSED_BYTES: u64 = 500 * 1024 * 1024;
const MAX_COMPRESSION_RATIO: u64 = 1_000;

#[derive(Debug, Error)]
pub enum XlsxWorkflowError {
    #[error("XLSX 文件读写失败：{0}")]
    Io(#[from] std::io::Error),
    #[error("XLSX ZIP 结构无效：{0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("策略配置无效：{0}")]
    InvalidPolicy(#[from] PolicyError),
    #[error("文本检测或替换失败：{0}")]
    Text(#[from] WorkflowError),
    #[error("XLSX 文件超过 100 MiB 限制")]
    PackageTooLarge,
    #[error("XLSX 包含过多文件项")]
    TooManyEntries,
    #[error("XLSX 包含不安全或重复的 ZIP 路径：{0}")]
    UnsafeEntryName(String),
    #[error("XLSX 包含加密文件项，当前不能安全处理")]
    EncryptedEntry,
    #[error("XLSX 文件项超过安全解压限制：{0}")]
    EntryTooLarge(String),
    #[error("XLSX 疑似压缩炸弹：{0}")]
    SuspiciousCompression(String),
    #[error("XLSX 使用了不支持的压缩方法：{0}")]
    UnsupportedCompression(String),
    #[error("XLSX 缺少必要文件项：{0}")]
    MissingRequiredEntry(String),
    #[error("XLSX XML 无效：{0}")]
    InvalidXml(String),
    #[error("XLSX 没有可处理的单元格、批注或绘图文字")]
    MissingTextContent,
    #[error("XLSX 任务中的 policy_id 与策略快照不一致")]
    PolicySnapshotMismatch,
    #[error("XLSX 源文件在扫描后发生了变化，请重新扫描")]
    SourceChanged,
    #[error("XLSX 输出路径不能与源文件相同")]
    WouldOverwriteSource,
    #[error("XLSX 输出扩展名必须为 .xlsx")]
    OutputTypeMismatch,
    #[error("输出文件已存在，未执行覆盖：{0}")]
    OutputExists(PathBuf),
    #[error("XLSX 仍有 {0} 个结果尚未复核")]
    UnreviewedFindings(usize),
    #[error("XLSX 命中范围无效：{0}")]
    InvalidFinding(String),
    #[error("同一公式单元格的多个命中必须使用同一个整格替换值：{0}")]
    ConflictingFormulaReplacement(String),
    #[error("工作表名称命中暂不支持自动改名；请明确复核为保留：{0}")]
    SheetRenameUnsupported(String),
    #[error("XLSX 残留复扫失败，发现 {0} 个未处理结果")]
    VerificationFailed(usize),
    #[error("XLSX 包含当前不能安全保真的功能或缓存：{0}")]
    UnsupportedPayload(String),
    #[error("XLSX 包含嵌入图片；请提供 OCR 配置重新扫描后再导出")]
    EmbeddedImagesUnsupported,
    #[error("XLSX 嵌入图片需要 OCR 运行注册表")]
    EmbeddedImageRuntimeRequired,
    #[error("XLSX 嵌入图片任务与源包不一致：{0}")]
    EmbeddedImageTaskMismatch(String),
    #[error("XLSX 包含当前不支持的嵌入图片格式：{0}")]
    UnsupportedEmbeddedImageType(String),
    #[error("XLSX 嵌入图片处理失败：{0}")]
    EmbeddedImage(#[from] ImageWorkflowError),
}

#[derive(Debug)]
struct PackageSummary {
    entry_names: Vec<String>,
    worksheets: usize,
    embedded_images: usize,
    unsupported_embedded_images: Vec<String>,
    unsupported_payloads: Vec<String>,
}

#[derive(Debug, Clone)]
struct CellRecord {
    reference: String,
    cell_type: Option<String>,
    formula: Option<String>,
    value: Option<String>,
    shared_index: Option<usize>,
}

pub fn scan_xlsx_with_policy(
    path: &Path,
    policy: &PolicyConfig,
    runtimes: Option<&RuntimeRegistry>,
    ocr_runtime_id: Option<&str>,
) -> Result<XlsxTaskDraft, XlsxWorkflowError> {
    policy.validate()?;
    let canonical = fs::canonicalize(path)?;
    let bytes = fs::read(&canonical)?;
    let summary = validate_package(&bytes)?;
    let source_sha256 = sha256_hex(&bytes);
    let parts = extract_workbook_parts(&bytes, &summary.entry_names, false)?;
    if parts.is_empty() && summary.embedded_images == 0 {
        return Err(XlsxWorkflowError::MissingTextContent);
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
            "xlsx-scan",
        )?;
        for finding in &mut detection.findings {
            finding.part_id.clone_from(&part.id);
        }
        policy.apply_to_findings(&mut detection.findings);
        if matches!(
            part.kind.as_str(),
            "formula" | "formula_cache" | "sheet_name"
        ) {
            for finding in &mut detection.findings {
                finding.selected = false;
                finding.reviewed = false;
            }
        }
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
        finding.id = format!("xlsx-finding-{:04}", index + 1);
    }
    append_package_diagnostics(
        &summary,
        embedded_images.len(),
        ocr_runtime_id,
        &mut diagnostics,
    );
    deduplicate_diagnostics(&mut diagnostics);

    Ok(XlsxTaskDraft {
        schema_version: 1,
        task_id: format!("xlsx-task-{}", &source_sha256[..12]),
        policy_id: policy.id.clone(),
        policy: policy.clone(),
        document: XlsxDocumentGraph {
            schema_version: 1,
            offset_unit: "unicode_scalar".to_owned(),
            source: XlsxSourceMetadata {
                path: canonical.to_string_lossy().into_owned(),
                sha256: source_sha256,
                size_bytes: bytes.len() as u64,
                package_entries: summary.entry_names.len(),
                worksheets: summary.worksheets,
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

pub fn review_xlsx_finding(
    task: &mut XlsxTaskDraft,
    finding_id: &str,
    selected: bool,
    replacement: Option<&str>,
) -> Result<(), XlsxWorkflowError> {
    validate_xlsx_task(task)?;
    let finding = task
        .findings
        .iter()
        .find(|finding| finding.id == finding_id)
        .ok_or_else(|| XlsxWorkflowError::InvalidFinding(finding_id.to_owned()))?;
    let part = task
        .document
        .parts
        .iter()
        .find(|part| part.id == finding.part_id)
        .ok_or_else(|| XlsxWorkflowError::InvalidFinding(finding_id.to_owned()))?;
    if part.kind == "sheet_name" && selected {
        return Err(XlsxWorkflowError::SheetRenameUnsupported(
            part.locator.clone(),
        ));
    }

    let finding_ids = if let Some(cell_locator) = formula_cell_locator(part) {
        task.findings
            .iter()
            .filter_map(|candidate| {
                let candidate_part = task
                    .document
                    .parts
                    .iter()
                    .find(|part| part.id == candidate.part_id)?;
                (formula_cell_locator(candidate_part) == Some(cell_locator))
                    .then(|| candidate.id.clone())
            })
            .collect::<Vec<_>>()
    } else {
        vec![finding_id.to_owned()]
    };

    let mut updated = task.clone();
    for id in finding_ids {
        review_finding(&mut updated.findings, &id, selected, replacement)?;
    }
    validate_xlsx_task(&updated)?;
    *task = updated;
    Ok(())
}

pub fn render_xlsx_embedded_image_preview(
    task: &XlsxTaskDraft,
    image_number: usize,
) -> Result<ImagePreview, XlsxWorkflowError> {
    validate_xlsx_task(task)?;
    let embedded = image_number
        .checked_sub(1)
        .and_then(|index| task.embedded_images.get(index))
        .ok_or_else(|| {
            XlsxWorkflowError::EmbeddedImageTaskMismatch(format!(
                "内嵌图片编号无效：{image_number}"
            ))
        })?;
    let source_path = fs::canonicalize(&task.document.source.path)?;
    let source_bytes = fs::read(&source_path)?;
    if sha256_hex(&source_bytes) != task.document.source.sha256 {
        return Err(XlsxWorkflowError::SourceChanged);
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
    render_image_task_preview(&preview_task).map_err(XlsxWorkflowError::EmbeddedImage)
}

fn append_package_diagnostics(
    summary: &PackageSummary,
    scanned_images: usize,
    ocr_runtime_id: Option<&str>,
    diagnostics: &mut Vec<TaskDiagnostic>,
) {
    if !summary.unsupported_embedded_images.is_empty() {
        diagnostics.push(TaskDiagnostic {
            severity: DiagnosticSeverity::Error,
            code: "XLSX_EMBEDDED_IMAGE_FORMAT_UNSUPPORTED".to_owned(),
            detector_id: None,
            message: "工作簿包含非 PNG/JPEG 嵌入图片；当前版本将拒绝导出。".to_owned(),
        });
    }
    if summary.embedded_images > scanned_images {
        diagnostics.push(TaskDiagnostic {
            severity: DiagnosticSeverity::Warning,
            code: "XLSX_EMBEDDED_IMAGES_PENDING".to_owned(),
            detector_id: None,
            message: "工作簿包含尚未扫描的图片；请提供 OCR 运行配置后重新扫描。".to_owned(),
        });
    } else if scanned_images > 0 {
        diagnostics.push(TaskDiagnostic {
            severity: DiagnosticSeverity::Info,
            code: "XLSX_EMBEDDED_IMAGES_SCANNED".to_owned(),
            detector_id: ocr_runtime_id.map(str::to_owned),
            message: "嵌入图片已建立可编辑遮罩子任务，并将在导出后独立 OCR 复扫。".to_owned(),
        });
    }
    if !summary.unsupported_payloads.is_empty() {
        diagnostics.push(TaskDiagnostic {
            severity: DiagnosticSeverity::Error,
            code: "XLSX_UNSUPPORTED_PAYLOAD".to_owned(),
            detector_id: None,
            message:
                "工作簿包含当前无法安全验证的宏、外部数据、图表/透视缓存或嵌入对象，将拒绝导出。"
                    .to_owned(),
        });
    }
}

fn validate_package(bytes: &[u8]) -> Result<PackageSummary, XlsxWorkflowError> {
    if bytes.len() > MAX_XLSX_BYTES {
        return Err(XlsxWorkflowError::PackageTooLarge);
    }
    let mut archive = ZipArchive::new(Cursor::new(bytes))?;
    if archive.len() > MAX_PACKAGE_ENTRIES {
        return Err(XlsxWorkflowError::TooManyEntries);
    }
    let mut names = HashSet::new();
    let mut entry_names = Vec::with_capacity(archive.len());
    let mut total_uncompressed = 0_u64;
    let mut worksheets = 0usize;
    let mut embedded_images = 0usize;
    let mut unsupported_embedded_images = Vec::new();
    let mut unsupported_payloads = Vec::new();
    for index in 0..archive.len() {
        let file = archive.by_index(index)?;
        let name = file.name().to_owned();
        if file.enclosed_name().is_none()
            || name.contains('\\')
            || name.contains('\0')
            || !names.insert(name.clone())
        {
            return Err(XlsxWorkflowError::UnsafeEntryName(name));
        }
        if file.encrypted() {
            return Err(XlsxWorkflowError::EncryptedEntry);
        }
        if !file.is_dir()
            && !matches!(
                file.compression(),
                CompressionMethod::Stored | CompressionMethod::Deflated
            )
        {
            return Err(XlsxWorkflowError::UnsupportedCompression(name));
        }
        if file.size() > MAX_ENTRY_BYTES || (name.ends_with(".xml") && file.size() > MAX_XML_BYTES)
        {
            return Err(XlsxWorkflowError::EntryTooLarge(name));
        }
        total_uncompressed = total_uncompressed.saturating_add(file.size());
        if total_uncompressed > MAX_TOTAL_UNCOMPRESSED_BYTES {
            return Err(XlsxWorkflowError::PackageTooLarge);
        }
        if file.size() > 10 * 1024 * 1024
            && file.size() > file.compressed_size().saturating_mul(MAX_COMPRESSION_RATIO)
        {
            return Err(XlsxWorkflowError::SuspiciousCompression(name));
        }
        if is_worksheet(&name) && !file.is_dir() {
            worksheets += 1;
        }
        if is_embedded_media(&name) && !file.is_dir() {
            embedded_images += 1;
            if !is_supported_embedded_image(&name) {
                unsupported_embedded_images.push(name.clone());
            }
        }
        if is_unsupported_payload(&name) && !file.is_dir() {
            unsupported_payloads.push(name.clone());
        }
        entry_names.push(name);
    }
    for required in [
        "[Content_Types].xml",
        "_rels/.rels",
        "xl/workbook.xml",
        "xl/_rels/workbook.xml.rels",
    ] {
        if !names.contains(required) {
            return Err(XlsxWorkflowError::MissingRequiredEntry(required.to_owned()));
        }
    }
    if worksheets == 0 {
        return Err(XlsxWorkflowError::MissingRequiredEntry(
            "xl/worksheets/*.xml".to_owned(),
        ));
    }
    Ok(PackageSummary {
        entry_names,
        worksheets,
        embedded_images,
        unsupported_embedded_images,
        unsupported_payloads,
    })
}

fn is_worksheet(name: &str) -> bool {
    name.starts_with("xl/worksheets/") && name.ends_with(".xml") && !name.contains("/_rels/")
}

fn is_embedded_media(name: &str) -> bool {
    name.starts_with("xl/media/") && !name.ends_with('/')
}

fn embedded_image_extension(name: &str) -> Result<&'static str, XlsxWorkflowError> {
    let extension = name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase());
    match extension.as_deref() {
        Some("png") => Ok("png"),
        Some("jpg" | "jpeg") => Ok("jpg"),
        _ => Err(XlsxWorkflowError::UnsupportedEmbeddedImageType(
            name.to_owned(),
        )),
    }
}

fn is_supported_embedded_image(name: &str) -> bool {
    is_embedded_media(name) && embedded_image_extension(name).is_ok()
}

fn is_unsupported_payload(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with("vbaproject.bin")
        || lower.starts_with("xl/activex/")
        || lower.starts_with("xl/ctrlprops/")
        || lower.starts_with("xl/embeddings/")
        || lower.starts_with("xl/externallinks/")
        || lower == "xl/connections.xml"
        || lower.starts_with("xl/pivotcache/")
        || lower.starts_with("xl/charts/")
        || lower.starts_with("xl/threadedcomments/")
        || lower.starts_with("xl/persons/")
        || lower.starts_with("xl/querytables/")
        || lower.starts_with("xl/slicercaches/")
        || lower.starts_with("_xmlsignatures/")
}

fn scan_embedded_images(
    bytes: &[u8],
    entry_names: &[String],
    source_path: &Path,
    policy: &PolicyConfig,
    runtimes: &RuntimeRegistry,
    ocr_runtime_id: &str,
) -> Result<Vec<XlsxEmbeddedImageTask>, XlsxWorkflowError> {
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
        task.task_id = format!("xlsx-image-{}-{index:05}", &task.source.sha256[..12]);
        task.source.path = format!("xlsx://{}#{entry_name}", source_path.to_string_lossy());
        images.push(XlsxEmbeddedImageTask {
            entry_name: entry_name.clone(),
            task,
        });
    }
    Ok(images)
}

fn extract_workbook_parts(
    bytes: &[u8],
    entry_names: &[String],
    include_shared_storage: bool,
) -> Result<Vec<DocumentPart>, XlsxWorkflowError> {
    let shared = if entry_names
        .iter()
        .any(|name| name == "xl/sharedStrings.xml")
    {
        parse_shared_strings(&read_entry(bytes, "xl/sharedStrings.xml")?)?
    } else {
        Vec::new()
    };
    let mut extracted: Vec<(String, String, String)> = Vec::new();
    let workbook = read_entry(bytes, "xl/workbook.xml")?;
    extracted.extend(extract_workbook_metadata_parts(&workbook)?);

    let mut worksheet_names: Vec<&String> = entry_names
        .iter()
        .filter(|name| is_worksheet(name))
        .collect();
    worksheet_names.sort();
    for name in worksheet_names {
        extracted.extend(extract_worksheet_parts(
            name,
            &read_entry(bytes, name)?,
            &shared,
        )?);
    }

    let mut comment_names: Vec<&String> = entry_names
        .iter()
        .filter(|name| name.starts_with("xl/comments") && name.ends_with(".xml"))
        .collect();
    comment_names.sort();
    for name in comment_names {
        extracted.extend(extract_comment_parts(name, &read_entry(bytes, name)?)?);
    }

    let mut drawing_names: Vec<&String> = entry_names
        .iter()
        .filter(|name| {
            name.starts_with("xl/drawings/")
                && name.ends_with(".xml")
                && !name.contains("/_rels/")
                && !name
                    .rsplit('/')
                    .next()
                    .unwrap_or_default()
                    .starts_with("vml")
        })
        .collect();
    drawing_names.sort();
    for name in drawing_names {
        extracted.extend(extract_drawing_parts(name, &read_entry(bytes, name)?)?);
    }

    if include_shared_storage {
        for (index, text) in shared.iter().enumerate() {
            if !text.is_empty() {
                extracted.push((
                    "shared_string_storage".to_owned(),
                    format!("xl/sharedStrings.xml#si={index:06}"),
                    text.clone(),
                ));
            }
        }
    }

    extracted.sort_by(|left, right| left.1.cmp(&right.1));
    Ok(extracted
        .into_iter()
        .enumerate()
        .filter_map(|(index, (kind, locator, text))| {
            if text.is_empty() {
                return None;
            }
            Some(DocumentPart {
                id: format!("xlsx-part-{:05}", index + 1),
                kind,
                locator,
                char_len: text.chars().count(),
                text,
            })
        })
        .collect())
}

fn read_entry(bytes: &[u8], name: &str) -> Result<Vec<u8>, XlsxWorkflowError> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))?;
    let mut entry = archive.by_name(name)?;
    let mut data = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut data)?;
    Ok(data)
}

fn extract_workbook_metadata_parts(
    xml: &[u8],
) -> Result<Vec<(String, String, String)>, XlsxWorkflowError> {
    let events = parse_xml_events(xml, "xl/workbook.xml")?;
    let mut parts = Vec::new();
    let mut sheet_index = 0usize;
    let mut defined_name_index = 0usize;
    for (index, event) in events.iter().enumerate() {
        match event {
            Event::Start(start) | Event::Empty(start)
                if local_name(start.name().as_ref()) == b"sheet" =>
            {
                sheet_index += 1;
                if let Some(name) = attribute_raw(start, b"name") {
                    parts.push((
                        "sheet_name".to_owned(),
                        format!("xl/workbook.xml#sheet={sheet_index:06}#name"),
                        name,
                    ));
                }
            }
            Event::Start(start) if local_name(start.name().as_ref()) == b"definedName" => {
                defined_name_index += 1;
                let end = matching_end(&events, index, b"definedName", "xl/workbook.xml")?;
                let text = all_text(&events[index + 1..end], "xl/workbook.xml")?;
                if !text.is_empty() {
                    parts.push((
                        "defined_name".to_owned(),
                        format!("xl/workbook.xml#definedName={defined_name_index:06}"),
                        text,
                    ));
                }
            }
            _ => {}
        }
    }
    Ok(parts)
}

fn extract_worksheet_parts(
    entry_name: &str,
    xml: &[u8],
    shared: &[String],
) -> Result<Vec<(String, String, String)>, XlsxWorkflowError> {
    let events = parse_xml_events(xml, entry_name)?;
    let mut parts = Vec::new();
    let mut extra_counts: BTreeMap<Vec<u8>, usize> = BTreeMap::new();
    let extra_names: [&[u8]; 6] = [
        b"oddHeader",
        b"oddFooter",
        b"evenHeader",
        b"evenFooter",
        b"firstHeader",
        b"firstFooter",
    ];
    let mut index = 0usize;
    while index < events.len() {
        match &events[index] {
            Event::Start(start) if local_name(start.name().as_ref()) == b"c" => {
                let end = matching_end(&events, index, b"c", entry_name)?;
                let cell = parse_cell(&events[index..=end], entry_name)?;
                append_cell_parts(entry_name, &cell, shared, &mut parts)?;
                index = end + 1;
            }
            Event::Start(start) if extra_names.contains(&local_name(start.name().as_ref())) => {
                let name = local_name(start.name().as_ref()).to_vec();
                let end = matching_end(&events, index, &name, entry_name)?;
                let text = all_text(&events[index + 1..end], entry_name)?;
                let count = extra_counts.entry(name.clone()).or_default();
                *count += 1;
                if !text.is_empty() {
                    parts.push((
                        "header_footer".to_owned(),
                        format!(
                            "{entry_name}#{}={:06}",
                            String::from_utf8_lossy(&name),
                            *count
                        ),
                        text,
                    ));
                }
                index = end + 1;
            }
            _ => index += 1,
        }
    }
    Ok(parts)
}

fn append_cell_parts(
    entry_name: &str,
    cell: &CellRecord,
    shared: &[String],
    parts: &mut Vec<(String, String, String)>,
) -> Result<(), XlsxWorkflowError> {
    let base = format!("{entry_name}#cell={}", cell.reference);
    if let Some(formula) = &cell.formula {
        if !formula.is_empty() {
            parts.push((
                "formula".to_owned(),
                format!("{base}#formula"),
                formula.clone(),
            ));
        }
        if let Some(value) = &cell.value
            && !value.is_empty()
        {
            parts.push((
                "formula_cache".to_owned(),
                format!("{base}#formula_cache"),
                value.clone(),
            ));
        }
        return Ok(());
    }
    let text = match cell.cell_type.as_deref() {
        Some("s") => {
            let index = cell.shared_index.ok_or_else(|| {
                XlsxWorkflowError::InvalidXml(format!("{entry_name}#{}", cell.reference))
            })?;
            shared.get(index).cloned().ok_or_else(|| {
                XlsxWorkflowError::InvalidXml(format!("{entry_name}#{}", cell.reference))
            })?
        }
        Some("b" | "e") => return Ok(()),
        _ => cell.value.clone().unwrap_or_default(),
    };
    if !text.is_empty() {
        let kind = if matches!(cell.cell_type.as_deref(), None | Some("n")) {
            "number"
        } else {
            "cell"
        };
        parts.push((kind.to_owned(), format!("{base}#value"), text));
    }
    Ok(())
}

fn extract_comment_parts(
    entry_name: &str,
    xml: &[u8],
) -> Result<Vec<(String, String, String)>, XlsxWorkflowError> {
    let events = parse_xml_events(xml, entry_name)?;
    let mut parts = Vec::new();
    let mut index = 0usize;
    while index < events.len() {
        if let Event::Start(start) = &events[index]
            && local_name(start.name().as_ref()) == b"comment"
        {
            let reference = attribute_raw(start, b"ref").unwrap_or_else(|| "unknown".to_owned());
            let end = matching_end(&events, index, b"comment", entry_name)?;
            let text = text_elements(&events[index + 1..end], entry_name)?;
            if !text.is_empty() {
                parts.push((
                    "comment".to_owned(),
                    format!("{entry_name}#comment={reference}"),
                    text,
                ));
            }
            index = end + 1;
        } else {
            index += 1;
        }
    }
    Ok(parts)
}

fn extract_drawing_parts(
    entry_name: &str,
    xml: &[u8],
) -> Result<Vec<(String, String, String)>, XlsxWorkflowError> {
    let events = parse_xml_events(xml, entry_name)?;
    let mut parts = Vec::new();
    let mut text_index = 0usize;
    let mut in_text = false;
    for event in &events {
        match event {
            Event::Start(start) if local_name(start.name().as_ref()) == b"t" => in_text = true,
            Event::End(end) if local_name(end.name().as_ref()) == b"t" => in_text = false,
            Event::Text(_) if in_text => {
                let text = decoded_text(event, entry_name)?;
                text_index += 1;
                if !text.is_empty() {
                    parts.push((
                        "drawing_text".to_owned(),
                        format!("{entry_name}#text={text_index:06}"),
                        text,
                    ));
                }
            }
            Event::CData(_) if in_text => {
                return Err(XlsxWorkflowError::InvalidXml(entry_name.to_owned()));
            }
            _ => {}
        }
    }
    Ok(parts)
}

fn parse_shared_strings(xml: &[u8]) -> Result<Vec<String>, XlsxWorkflowError> {
    let entry_name = "xl/sharedStrings.xml";
    let events = parse_xml_events(xml, entry_name)?;
    let mut strings = Vec::new();
    let mut index = 0usize;
    while index < events.len() {
        if let Event::Start(start) = &events[index]
            && local_name(start.name().as_ref()) == b"si"
        {
            let end = matching_end(&events, index, b"si", entry_name)?;
            strings.push(text_elements(&events[index + 1..end], entry_name)?);
            index = end + 1;
        } else {
            index += 1;
        }
    }
    Ok(strings)
}

fn parse_cell(
    events: &[Event<'static>],
    entry_name: &str,
) -> Result<CellRecord, XlsxWorkflowError> {
    let Event::Start(start) = &events[0] else {
        return Err(XlsxWorkflowError::InvalidXml(entry_name.to_owned()));
    };
    let reference = attribute_raw(start, b"r")
        .ok_or_else(|| XlsxWorkflowError::InvalidXml(entry_name.to_owned()))?;
    let cell_type = attribute_raw(start, b"t");
    let formula = first_element_text(events, b"f", entry_name)?;
    let raw_value = first_element_text(events, b"v", entry_name)?;
    let inline_value = if cell_type.as_deref() == Some("inlineStr") {
        Some(text_elements(events, entry_name)?)
    } else {
        None
    };
    let shared_index = if cell_type.as_deref() == Some("s") {
        raw_value.as_deref().and_then(|value| value.parse().ok())
    } else {
        None
    };
    Ok(CellRecord {
        reference,
        cell_type,
        formula,
        value: inline_value.or(raw_value),
        shared_index,
    })
}

fn first_element_text(
    events: &[Event<'static>],
    element_name: &[u8],
    entry_name: &str,
) -> Result<Option<String>, XlsxWorkflowError> {
    let mut in_target = false;
    for event in events {
        match event {
            Event::Start(start) if local_name(start.name().as_ref()) == element_name => {
                in_target = true;
            }
            Event::End(end) if local_name(end.name().as_ref()) == element_name => break,
            Event::Text(_) if in_target => return Ok(Some(decoded_text(event, entry_name)?)),
            Event::CData(_) if in_target => {
                return Err(XlsxWorkflowError::InvalidXml(entry_name.to_owned()));
            }
            _ => {}
        }
    }
    Ok(None)
}

fn text_elements(events: &[Event<'static>], entry_name: &str) -> Result<String, XlsxWorkflowError> {
    let mut result = String::new();
    let mut in_text = false;
    for event in events {
        match event {
            Event::Start(start) if local_name(start.name().as_ref()) == b"t" => in_text = true,
            Event::End(end) if local_name(end.name().as_ref()) == b"t" => in_text = false,
            Event::Text(_) if in_text => result.push_str(&decoded_text(event, entry_name)?),
            Event::CData(_) if in_text => {
                return Err(XlsxWorkflowError::InvalidXml(entry_name.to_owned()));
            }
            _ => {}
        }
    }
    Ok(result)
}

fn all_text(events: &[Event<'static>], entry_name: &str) -> Result<String, XlsxWorkflowError> {
    let mut result = String::new();
    for event in events {
        match event {
            Event::Text(_) => result.push_str(&decoded_text(event, entry_name)?),
            Event::CData(_) => return Err(XlsxWorkflowError::InvalidXml(entry_name.to_owned())),
            _ => {}
        }
    }
    Ok(result)
}

fn parse_xml_events(
    xml: &[u8],
    entry_name: &str,
) -> Result<Vec<Event<'static>>, XlsxWorkflowError> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut events = Vec::new();
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::DocType(_)) => {
                return Err(XlsxWorkflowError::InvalidXml(entry_name.to_owned()));
            }
            Ok(Event::Eof) => break,
            Ok(event) => events.push(event.into_owned()),
            Err(_) => return Err(XlsxWorkflowError::InvalidXml(entry_name.to_owned())),
        }
        buffer.clear();
    }
    Ok(events)
}

fn serialize_xml_events(
    events: &[Event<'static>],
    entry_name: &str,
) -> Result<Vec<u8>, XlsxWorkflowError> {
    let mut writer = Writer::new(Vec::new());
    for event in events {
        writer
            .write_event(event.clone())
            .map_err(|_| XlsxWorkflowError::InvalidXml(entry_name.to_owned()))?;
    }
    Ok(writer.into_inner())
}

fn matching_end(
    events: &[Event<'static>],
    start_index: usize,
    element_name: &[u8],
    entry_name: &str,
) -> Result<usize, XlsxWorkflowError> {
    let mut depth = 0usize;
    for (index, event) in events.iter().enumerate().skip(start_index) {
        match event {
            Event::Start(start) if local_name(start.name().as_ref()) == element_name => depth += 1,
            Event::End(end) if local_name(end.name().as_ref()) == element_name => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Ok(index);
                }
            }
            _ => {}
        }
    }
    Err(XlsxWorkflowError::InvalidXml(entry_name.to_owned()))
}

fn decoded_text(event: &Event<'static>, entry_name: &str) -> Result<String, XlsxWorkflowError> {
    let Event::Text(text) = event else {
        return Err(XlsxWorkflowError::InvalidXml(entry_name.to_owned()));
    };
    let decoded = text
        .decode()
        .map_err(|_| XlsxWorkflowError::InvalidXml(entry_name.to_owned()))?;
    Ok(unescape(&decoded)
        .map_err(|_| XlsxWorkflowError::InvalidXml(entry_name.to_owned()))?
        .into_owned())
}

fn local_name(name: &[u8]) -> &[u8] {
    name.rsplit(|byte| *byte == b':').next().unwrap_or(name)
}

fn attribute_raw(start: &BytesStart<'_>, local_key: &[u8]) -> Option<String> {
    start
        .attributes()
        .with_checks(false)
        .flatten()
        .find(|attribute| local_name(attribute.key.as_ref()) == local_key)
        .map(|attribute| String::from_utf8_lossy(attribute.value.as_ref()).into_owned())
}

fn validate_xlsx_task(task: &XlsxTaskDraft) -> Result<(), XlsxWorkflowError> {
    task.policy.validate()?;
    if task.policy_id != task.policy.id {
        return Err(XlsxWorkflowError::PolicySnapshotMismatch);
    }
    let parts: HashMap<&str, &DocumentPart> = task
        .document
        .parts
        .iter()
        .map(|part| (part.id.as_str(), part))
        .collect();
    if parts.len() != task.document.parts.len() {
        return Err(XlsxWorkflowError::InvalidFinding(
            "文档部分 id 重复".to_owned(),
        ));
    }
    let mut selected_ranges: BTreeMap<&str, Vec<(usize, usize)>> = BTreeMap::new();
    for finding in &task.findings {
        let Some(part) = parts.get(finding.part_id.as_str()) else {
            return Err(XlsxWorkflowError::InvalidFinding(finding.id.clone()));
        };
        let matched: String = part
            .text
            .chars()
            .skip(finding.start)
            .take(finding.end.saturating_sub(finding.start))
            .collect();
        if finding.start >= finding.end
            || finding.end > part.char_len
            || matched != finding.matched_text
        {
            return Err(XlsxWorkflowError::InvalidFinding(finding.id.clone()));
        }
        if finding.selected {
            if part.kind == "sheet_name" {
                return Err(XlsxWorkflowError::SheetRenameUnsupported(
                    part.locator.clone(),
                ));
            }
            selected_ranges
                .entry(&finding.part_id)
                .or_default()
                .push((finding.start, finding.end));
        }
    }
    for ranges in selected_ranges.values_mut() {
        ranges.sort_unstable();
        if ranges.windows(2).any(|pair| pair[0].1 > pair[1].0) {
            return Err(XlsxWorkflowError::InvalidFinding(
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
            return Err(XlsxWorkflowError::EmbeddedImageTaskMismatch(
                embedded.entry_name.clone(),
            ));
        }
    }
    Ok(())
}

fn findings_by_part(task: &XlsxTaskDraft) -> BTreeMap<&str, Vec<&Finding>> {
    let mut result: BTreeMap<&str, Vec<&Finding>> = BTreeMap::new();
    for finding in task.findings.iter().filter(|finding| finding.selected) {
        result
            .entry(finding.part_id.as_str())
            .or_default()
            .push(finding);
    }
    result
}

fn formula_cell_locator(part: &DocumentPart) -> Option<&str> {
    part.locator
        .strip_suffix("#formula")
        .or_else(|| part.locator.strip_suffix("#formula_cache"))
}

struct RedactionPlan {
    text_by_locator: BTreeMap<String, String>,
    formula_cells: BTreeMap<String, String>,
    defined_names: BTreeSet<String>,
}

fn build_redaction_plan(task: &XlsxTaskDraft) -> Result<RedactionPlan, XlsxWorkflowError> {
    let findings = findings_by_part(task);
    let mut text_by_locator = BTreeMap::new();
    let mut formula_replacements: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut defined_names = BTreeSet::new();
    for part in &task.document.parts {
        let Some(selected) = findings.get(part.id.as_str()) else {
            continue;
        };
        if matches!(part.kind.as_str(), "formula" | "formula_cache") {
            let base = formula_cell_locator(part)
                .ok_or_else(|| XlsxWorkflowError::InvalidFinding(part.id.clone()))?;
            formula_replacements
                .entry(base.to_owned())
                .or_default()
                .extend(selected.iter().map(|finding| finding.replacement.clone()));
        } else if part.kind == "defined_name" {
            defined_names.insert(part.locator.clone());
        } else {
            let owned: Vec<Finding> = selected.iter().map(|finding| (*finding).clone()).collect();
            text_by_locator.insert(part.locator.clone(), apply_findings(&part.text, &owned)?);
        }
    }
    let mut formula_cells = BTreeMap::new();
    for (locator, replacements) in formula_replacements {
        if replacements.len() != 1 {
            return Err(XlsxWorkflowError::ConflictingFormulaReplacement(locator));
        }
        formula_cells.insert(locator, replacements.into_iter().next().unwrap_or_default());
    }
    Ok(RedactionPlan {
        text_by_locator,
        formula_cells,
        defined_names,
    })
}

fn image_group_count(
    images: &[XlsxEmbeddedImageTask],
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
    task: &'a XlsxTaskDraft,
    summary: &PackageSummary,
) -> Result<BTreeMap<&'a str, &'a ImageTaskDraft>, XlsxWorkflowError> {
    if let Some(name) = summary.unsupported_embedded_images.first() {
        return Err(XlsxWorkflowError::UnsupportedEmbeddedImageType(
            name.clone(),
        ));
    }
    if summary.embedded_images > 0 && task.embedded_images.is_empty() {
        return Err(XlsxWorkflowError::EmbeddedImagesUnsupported);
    }
    if summary.embedded_images != task.embedded_images.len()
        || task.document.source.embedded_images != summary.embedded_images
    {
        return Err(XlsxWorkflowError::EmbeddedImageTaskMismatch(
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
        return Err(XlsxWorkflowError::EmbeddedImageTaskMismatch(
            "嵌入图片路径不同".to_owned(),
        ));
    }
    Ok(task
        .embedded_images
        .iter()
        .map(|image| (image.entry_name.as_str(), &image.task))
        .collect())
}

fn retained_shared_indices(
    bytes: &[u8],
    entry_names: &[String],
    plan: &RedactionPlan,
) -> Result<BTreeSet<usize>, XlsxWorkflowError> {
    let mut retained = BTreeSet::new();
    for name in entry_names.iter().filter(|name| is_worksheet(name)) {
        let events = parse_xml_events(&read_entry(bytes, name)?, name)?;
        let mut index = 0usize;
        while index < events.len() {
            if let Event::Start(start) = &events[index]
                && local_name(start.name().as_ref()) == b"c"
            {
                let end = matching_end(&events, index, b"c", name)?;
                let cell = parse_cell(&events[index..=end], name)?;
                let base = format!("{name}#cell={}", cell.reference);
                let value_locator = format!("{base}#value");
                if !plan.formula_cells.contains_key(&base)
                    && !plan.text_by_locator.contains_key(&value_locator)
                    && let Some(shared_index) = cell.shared_index
                {
                    retained.insert(shared_index);
                }
                index = end + 1;
            } else {
                index += 1;
            }
        }
    }
    Ok(retained)
}

fn write_events(
    writer: &mut Writer<Vec<u8>>,
    events: &[Event<'static>],
    entry_name: &str,
) -> Result<(), XlsxWorkflowError> {
    for event in events {
        writer
            .write_event(event.clone())
            .map_err(|_| XlsxWorkflowError::InvalidXml(entry_name.to_owned()))?;
    }
    Ok(())
}

fn prefixed_name(parent: &BytesStart<'_>, local: &str) -> String {
    let qualified_name = parent.name();
    let parent_name = String::from_utf8_lossy(qualified_name.as_ref());
    parent_name
        .split_once(':')
        .map(|(prefix, _)| format!("{prefix}:{local}"))
        .unwrap_or_else(|| local.to_owned())
}

fn cell_start_as_inline(start: &BytesStart<'_>) -> Result<BytesStart<'static>, XlsxWorkflowError> {
    let name = String::from_utf8_lossy(start.name().as_ref()).into_owned();
    let mut rebuilt = BytesStart::new(name);
    for attribute in start.attributes().with_checks(false) {
        let attribute = attribute.map_err(|_| XlsxWorkflowError::InvalidXml("cell".to_owned()))?;
        if local_name(attribute.key.as_ref()) != b"t" {
            rebuilt.push_attribute((attribute.key.as_ref(), attribute.value.as_ref()));
        }
    }
    rebuilt.push_attribute(("t", "inlineStr"));
    Ok(rebuilt.into_owned())
}

fn write_inline_cell(
    writer: &mut Writer<Vec<u8>>,
    start: &BytesStart<'_>,
    replacement: &str,
    entry_name: &str,
) -> Result<(), XlsxWorkflowError> {
    let cell_name = String::from_utf8_lossy(start.name().as_ref()).into_owned();
    let is_name = prefixed_name(start, "is");
    let text_name = prefixed_name(start, "t");
    writer
        .write_event(Event::Start(cell_start_as_inline(start)?))
        .map_err(|_| XlsxWorkflowError::InvalidXml(entry_name.to_owned()))?;
    writer
        .write_event(Event::Start(BytesStart::new(is_name.clone())))
        .map_err(|_| XlsxWorkflowError::InvalidXml(entry_name.to_owned()))?;
    let mut text_start = BytesStart::new(text_name.clone());
    if replacement.starts_with(char::is_whitespace) || replacement.ends_with(char::is_whitespace) {
        text_start.push_attribute(("xml:space", "preserve"));
    }
    writer
        .write_event(Event::Start(text_start))
        .and_then(|_| writer.write_event(Event::Text(BytesText::new(replacement))))
        .and_then(|_| writer.write_event(Event::End(BytesEnd::new(text_name))))
        .and_then(|_| writer.write_event(Event::End(BytesEnd::new(is_name))))
        .and_then(|_| writer.write_event(Event::End(BytesEnd::new(cell_name))))
        .map_err(|_| XlsxWorkflowError::InvalidXml(entry_name.to_owned()))?;
    Ok(())
}

fn transform_worksheet(
    entry_name: &str,
    xml: &[u8],
    plan: &RedactionPlan,
) -> Result<Vec<u8>, XlsxWorkflowError> {
    let events = parse_xml_events(xml, entry_name)?;
    let mut writer = Writer::new(Vec::new());
    let mut extra_counts: BTreeMap<Vec<u8>, usize> = BTreeMap::new();
    let extra_names: [&[u8]; 6] = [
        b"oddHeader",
        b"oddFooter",
        b"evenHeader",
        b"evenFooter",
        b"firstHeader",
        b"firstFooter",
    ];
    let mut index = 0usize;
    while index < events.len() {
        match &events[index] {
            Event::Start(start) if local_name(start.name().as_ref()) == b"c" => {
                let end = matching_end(&events, index, b"c", entry_name)?;
                let cell = parse_cell(&events[index..=end], entry_name)?;
                let base = format!("{entry_name}#cell={}", cell.reference);
                let value_locator = format!("{base}#value");
                let replacement = plan
                    .formula_cells
                    .get(&base)
                    .or_else(|| plan.text_by_locator.get(&value_locator));
                if let Some(replacement) = replacement {
                    write_inline_cell(&mut writer, start, replacement, entry_name)?;
                } else {
                    write_events(&mut writer, &events[index..=end], entry_name)?;
                }
                index = end + 1;
            }
            Event::Start(start) if extra_names.contains(&local_name(start.name().as_ref())) => {
                let name = local_name(start.name().as_ref()).to_vec();
                let end = matching_end(&events, index, &name, entry_name)?;
                let count = extra_counts.entry(name.clone()).or_default();
                *count += 1;
                let locator = format!(
                    "{entry_name}#{}={:06}",
                    String::from_utf8_lossy(&name),
                    *count
                );
                if let Some(replacement) = plan.text_by_locator.get(&locator) {
                    writer
                        .write_event(events[index].clone())
                        .and_then(|_| writer.write_event(Event::Text(BytesText::new(replacement))))
                        .and_then(|_| writer.write_event(events[end].clone()))
                        .map_err(|_| XlsxWorkflowError::InvalidXml(entry_name.to_owned()))?;
                } else {
                    write_events(&mut writer, &events[index..=end], entry_name)?;
                }
                index = end + 1;
            }
            _ => {
                writer
                    .write_event(events[index].clone())
                    .map_err(|_| XlsxWorkflowError::InvalidXml(entry_name.to_owned()))?;
                index += 1;
            }
        }
    }
    Ok(writer.into_inner())
}

fn transform_shared_strings(
    xml: &[u8],
    retained: &BTreeSet<usize>,
) -> Result<Vec<u8>, XlsxWorkflowError> {
    let entry_name = "xl/sharedStrings.xml";
    let events = parse_xml_events(xml, entry_name)?;
    let mut output = Vec::with_capacity(events.len());
    let mut current_si: Option<usize> = None;
    let mut si_index = 0usize;
    let mut in_text = false;
    for event in events {
        match &event {
            Event::Start(start) if local_name(start.name().as_ref()) == b"si" => {
                current_si = Some(si_index);
                si_index += 1;
            }
            Event::End(end) if local_name(end.name().as_ref()) == b"si" => current_si = None,
            Event::Start(start) if local_name(start.name().as_ref()) == b"t" => in_text = true,
            Event::End(end) if local_name(end.name().as_ref()) == b"t" => in_text = false,
            _ => {}
        }
        if matches!(event, Event::Text(_))
            && in_text
            && current_si.is_some_and(|index| !retained.contains(&index))
        {
            output.push(Event::Text(BytesText::new("").into_owned()));
        } else {
            output.push(event);
        }
    }
    serialize_xml_events(&output, entry_name)
}

fn transform_comments(
    entry_name: &str,
    xml: &[u8],
    plan: &RedactionPlan,
) -> Result<Vec<u8>, XlsxWorkflowError> {
    let events = parse_xml_events(xml, entry_name)?;
    let mut writer = Writer::new(Vec::new());
    let mut index = 0usize;
    while index < events.len() {
        match &events[index] {
            Event::Start(start) if local_name(start.name().as_ref()) == b"author" => {
                writer.write_event(events[index].clone())?;
                writer.write_event(Event::Text(BytesText::new("LlaMask")))?;
                index += 1;
                while index < events.len() {
                    if matches!(&events[index], Event::End(end) if local_name(end.name().as_ref()) == b"author")
                    {
                        writer.write_event(events[index].clone())?;
                        index += 1;
                        break;
                    }
                    index += 1;
                }
            }
            Event::Start(start) if local_name(start.name().as_ref()) == b"comment" => {
                let reference =
                    attribute_raw(start, b"ref").unwrap_or_else(|| "unknown".to_owned());
                let end = matching_end(&events, index, b"comment", entry_name)?;
                let locator = format!("{entry_name}#comment={reference}");
                if let Some(replacement) = plan.text_by_locator.get(&locator) {
                    let comment_name = String::from_utf8_lossy(start.name().as_ref()).into_owned();
                    let text_name = prefixed_name(start, "text");
                    let t_name = prefixed_name(start, "t");
                    writer.write_event(events[index].clone())?;
                    writer.write_event(Event::Start(BytesStart::new(text_name.clone())))?;
                    writer.write_event(Event::Start(BytesStart::new(t_name.clone())))?;
                    writer.write_event(Event::Text(BytesText::new(replacement)))?;
                    writer.write_event(Event::End(BytesEnd::new(t_name)))?;
                    writer.write_event(Event::End(BytesEnd::new(text_name)))?;
                    writer.write_event(Event::End(BytesEnd::new(comment_name)))?;
                } else {
                    write_events(&mut writer, &events[index..=end], entry_name)?;
                }
                index = end + 1;
            }
            _ => {
                writer.write_event(events[index].clone())?;
                index += 1;
            }
        }
    }
    Ok(writer.into_inner())
}

fn scrub_drawing_start(start: &BytesStart<'_>) -> Result<BytesStart<'static>, XlsxWorkflowError> {
    let name = String::from_utf8_lossy(start.name().as_ref()).into_owned();
    let mut rebuilt = BytesStart::new(name);
    let qualified_name = start.name();
    let element = local_name(qualified_name.as_ref());
    for attribute in start.attributes().with_checks(false) {
        let attribute =
            attribute.map_err(|_| XlsxWorkflowError::InvalidXml("drawing".to_owned()))?;
        let key = local_name(attribute.key.as_ref());
        if matches!(element, b"cNvPr" | b"docPr") && matches!(key, b"name" | b"descr" | b"title") {
            rebuilt.push_attribute((attribute.key.as_ref(), b"".as_slice()));
        } else {
            rebuilt.push_attribute((attribute.key.as_ref(), attribute.value.as_ref()));
        }
    }
    Ok(rebuilt.into_owned())
}

fn transform_drawing(
    entry_name: &str,
    xml: &[u8],
    plan: &RedactionPlan,
) -> Result<Vec<u8>, XlsxWorkflowError> {
    let events = parse_xml_events(xml, entry_name)?;
    let mut output = Vec::with_capacity(events.len());
    let mut in_text = false;
    let mut text_index = 0usize;
    for event in events {
        match event {
            Event::Start(start) if local_name(start.name().as_ref()) == b"t" => {
                in_text = true;
                output.push(Event::Start(start));
            }
            Event::End(end) if local_name(end.name().as_ref()) == b"t" => {
                in_text = false;
                output.push(Event::End(end));
            }
            Event::Text(_) if in_text => {
                text_index += 1;
                let locator = format!("{entry_name}#text={text_index:06}");
                if let Some(replacement) = plan.text_by_locator.get(&locator) {
                    output.push(Event::Text(BytesText::new(replacement).into_owned()));
                } else {
                    output.push(event);
                }
            }
            Event::Start(start) => output.push(Event::Start(scrub_drawing_start(&start)?)),
            Event::Empty(start) => output.push(Event::Empty(scrub_drawing_start(&start)?)),
            _ => output.push(event),
        }
    }
    serialize_xml_events(&output, entry_name)
}

fn transform_workbook(xml: &[u8], plan: &RedactionPlan) -> Result<Vec<u8>, XlsxWorkflowError> {
    let entry_name = "xl/workbook.xml";
    let events = parse_xml_events(xml, entry_name)?;
    let mut writer = Writer::new(Vec::new());
    let mut index = 0usize;
    let mut defined_name_index = 0usize;
    while index < events.len() {
        if let Event::Start(start) = &events[index]
            && local_name(start.name().as_ref()) == b"definedName"
        {
            defined_name_index += 1;
            let end = matching_end(&events, index, b"definedName", entry_name)?;
            let locator = format!("xl/workbook.xml#definedName={defined_name_index:06}");
            if !plan.defined_names.contains(&locator) {
                write_events(&mut writer, &events[index..=end], entry_name)?;
            }
            index = end + 1;
        } else {
            writer.write_event(events[index].clone())?;
            index += 1;
        }
    }
    Ok(writer.into_inner())
}

fn should_drop_entry(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with("customxml/")
        || lower == "docprops/core.xml"
        || lower == "docprops/app.xml"
        || lower == "docprops/custom.xml"
        || lower.starts_with("docprops/thumbnail.")
        || lower == "xl/calcchain.xml"
        || lower.starts_with("xl/printersettings/")
}

fn should_drop_relationship(start: &BytesStart<'_>) -> bool {
    let target = attribute_raw(start, b"Target")
        .unwrap_or_default()
        .replace('\\', "/")
        .to_ascii_lowercase();
    let relation_type = attribute_raw(start, b"Type")
        .unwrap_or_default()
        .to_ascii_lowercase();
    target.contains("customxml")
        || target.contains("docprops/core.xml")
        || target.contains("docprops/app.xml")
        || target.contains("docprops/custom.xml")
        || target.contains("thumbnail.")
        || target.contains("calcchain.xml")
        || target.contains("printersettings/")
        || relation_type.ends_with("/custom-properties")
        || relation_type.ends_with("/core-properties")
        || relation_type.ends_with("/extended-properties")
        || relation_type.ends_with("/calcchain")
        || relation_type.ends_with("/printersettings")
}

fn sanitize_external_relationship(
    start: &BytesStart<'_>,
    entry_name: &str,
) -> Result<BytesStart<'static>, XlsxWorkflowError> {
    let external = attribute_raw(start, b"TargetMode")
        .is_some_and(|value| value.eq_ignore_ascii_case("External"));
    if !external {
        return Ok(start.clone().into_owned());
    }
    let relation_type = attribute_raw(start, b"Type")
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !relation_type.ends_with("/hyperlink") {
        return Err(XlsxWorkflowError::UnsupportedPayload(format!(
            "{entry_name} 中的外部关系"
        )));
    }
    let name = String::from_utf8_lossy(start.name().as_ref()).into_owned();
    let mut rebuilt = BytesStart::new(name);
    for attribute in start.attributes().with_checks(false) {
        let attribute =
            attribute.map_err(|_| XlsxWorkflowError::InvalidXml(entry_name.to_owned()))?;
        if local_name(attribute.key.as_ref()) == b"Target" {
            rebuilt.push_attribute((attribute.key.as_ref(), b"about:blank".as_slice()));
        } else {
            rebuilt.push_attribute((attribute.key.as_ref(), attribute.value.as_ref()));
        }
    }
    Ok(rebuilt.into_owned())
}

fn scrub_relationships(xml: &[u8], entry_name: &str) -> Result<Vec<u8>, XlsxWorkflowError> {
    let events = parse_xml_events(xml, entry_name)?;
    let mut output = Vec::with_capacity(events.len());
    for event in events {
        match event {
            Event::Empty(start) if local_name(start.name().as_ref()) == b"Relationship" => {
                if !should_drop_relationship(&start) {
                    output.push(Event::Empty(sanitize_external_relationship(
                        &start, entry_name,
                    )?));
                }
            }
            Event::Start(start) if local_name(start.name().as_ref()) == b"Relationship" => {
                if should_drop_relationship(&start) {
                    return Err(XlsxWorkflowError::InvalidXml(entry_name.to_owned()));
                }
                output.push(Event::Start(sanitize_external_relationship(
                    &start, entry_name,
                )?));
            }
            _ => output.push(event),
        }
    }
    serialize_xml_events(&output, entry_name)
}

fn should_drop_content_type(start: &BytesStart<'_>) -> bool {
    let part_name = attribute_raw(start, b"PartName")
        .unwrap_or_default()
        .to_ascii_lowercase();
    part_name.starts_with("/customxml/")
        || part_name == "/docprops/core.xml"
        || part_name == "/docprops/app.xml"
        || part_name == "/docprops/custom.xml"
        || part_name.starts_with("/docprops/thumbnail.")
        || part_name == "/xl/calcchain.xml"
        || part_name.starts_with("/xl/printersettings/")
}

fn scrub_content_types(xml: &[u8]) -> Result<Vec<u8>, XlsxWorkflowError> {
    let entry_name = "[Content_Types].xml";
    let events = parse_xml_events(xml, entry_name)?;
    let mut output = Vec::with_capacity(events.len());
    for event in events {
        match &event {
            Event::Empty(start)
                if local_name(start.name().as_ref()) == b"Override"
                    && should_drop_content_type(start) => {}
            Event::Start(start)
                if local_name(start.name().as_ref()) == b"Override"
                    && should_drop_content_type(start) =>
            {
                return Err(XlsxWorkflowError::InvalidXml(entry_name.to_owned()));
            }
            _ => output.push(event),
        }
    }
    serialize_xml_events(&output, entry_name)
}

fn transform_package_entry(
    name: &str,
    data: &[u8],
    plan: &RedactionPlan,
    retained_shared: &BTreeSet<usize>,
) -> Result<Vec<u8>, XlsxWorkflowError> {
    if is_worksheet(name) {
        return transform_worksheet(name, data, plan);
    }
    if name.starts_with("xl/comments") && name.ends_with(".xml") {
        return transform_comments(name, data, plan);
    }
    if name.starts_with("xl/drawings/")
        && name.ends_with(".xml")
        && !name.contains("/_rels/")
        && !name
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .starts_with("vml")
    {
        return transform_drawing(name, data, plan);
    }
    if name.ends_with(".rels") {
        return scrub_relationships(data, name);
    }
    match name {
        "[Content_Types].xml" => scrub_content_types(data),
        "xl/workbook.xml" => transform_workbook(data, plan),
        "xl/sharedStrings.xml" => transform_shared_strings(data, retained_shared),
        _ => Ok(data.to_vec()),
    }
}

fn redact_embedded_image(
    entry_name: &str,
    source_bytes: &[u8],
    task: &ImageTaskDraft,
    runtimes: &RuntimeRegistry,
) -> Result<Vec<u8>, XlsxWorkflowError> {
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

fn build_redacted_package(
    task: &XlsxTaskDraft,
    source_bytes: &[u8],
    runtimes: Option<&RuntimeRegistry>,
) -> Result<Vec<u8>, XlsxWorkflowError> {
    let summary = validate_package(source_bytes)?;
    if let Some(name) = summary.unsupported_payloads.first() {
        return Err(XlsxWorkflowError::UnsupportedPayload(name.clone()));
    }
    let embedded_tasks = embedded_tasks_by_entry(task, &summary)?;
    let image_runtimes = if embedded_tasks.is_empty() {
        None
    } else {
        Some(runtimes.ok_or(XlsxWorkflowError::EmbeddedImageRuntimeRequired)?)
    };
    let plan = build_redaction_plan(task)?;
    let retained_shared = retained_shared_indices(source_bytes, &summary.entry_names, &plan)?;
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
            transform_package_entry(&name, &data, &plan, &retained_shared)?
        };
        let options = SimpleFileOptions::default()
            .compression_method(compression)
            .unix_permissions(0o644);
        target.start_file(name, options)?;
        target.write_all(&transformed)?;
    }
    Ok(target.finish()?.into_inner())
}

fn relationships_are_scrubbed(xml: &[u8], entry_name: &str) -> Result<bool, XlsxWorkflowError> {
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

fn drawing_metadata_is_scrubbed(xml: &[u8], entry_name: &str) -> Result<bool, XlsxWorkflowError> {
    for event in parse_xml_events(xml, entry_name)? {
        let (Event::Start(start) | Event::Empty(start)) = event else {
            continue;
        };
        if !matches!(local_name(start.name().as_ref()), b"cNvPr" | b"docPr") {
            continue;
        }
        for key in [b"name".as_slice(), b"descr", b"title"] {
            if attribute_raw(&start, key).is_some_and(|value| !value.is_empty()) {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

fn package_metadata_is_scrubbed(bytes: &[u8]) -> Result<bool, XlsxWorkflowError> {
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
        if entry.is_dir() || (!name.ends_with(".xml") && !name.ends_with(".rels")) {
            continue;
        }
        let mut data = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut data)?;
        if name.ends_with(".rels") && !relationships_are_scrubbed(&data, &name)? {
            return Ok(false);
        }
        if name.starts_with("xl/drawings/")
            && !name.contains("/_rels/")
            && !name
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .starts_with("vml")
            && !drawing_metadata_is_scrubbed(&data, &name)?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn append_target_residuals(
    task: &XlsxTaskDraft,
    output_parts: &[DocumentPart],
    accepted_keep_values: &BTreeSet<(EntityType, String)>,
    residuals: &mut Vec<XlsxResidualFinding>,
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
        let mut candidates: Vec<&DocumentPart> = output_by_locator
            .get(source_part.locator.as_str())
            .copied()
            .into_iter()
            .collect();
        if !accepted_keep_values.contains(&(finding.entity_type, finding.matched_text.clone())) {
            candidates.extend(
                output_parts
                    .iter()
                    .filter(|part| part.kind == "shared_string_storage"),
            );
        }
        for output_part in candidates {
            for (byte_start, matched) in output_part.text.match_indices(&finding.matched_text) {
                let start = output_part.text[..byte_start].chars().count();
                let end = start + matched.chars().count();
                let key = (output_part.id.clone(), start, end, finding.entity_type);
                if seen.insert(key) {
                    residuals.push(XlsxResidualFinding {
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
}

fn verify_xlsx_bytes_with_runtimes(
    task: &XlsxTaskDraft,
    bytes: &[u8],
    checked_file: &str,
    runtimes: Option<&RuntimeRegistry>,
) -> Result<XlsxVerificationReport, XlsxWorkflowError> {
    validate_xlsx_task(task)?;
    let summary = validate_package(bytes)?;
    if let Some(name) = summary.unsupported_payloads.first() {
        return Err(XlsxWorkflowError::UnsupportedPayload(name.clone()));
    }
    let output_parts = extract_workbook_parts(bytes, &summary.entry_names, true)?;
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
    let accepted_keep_values: BTreeSet<(EntityType, String)> = accepted_keep
        .iter()
        .map(|(_, entity_type, value)| (*entity_type, value.clone()))
        .collect();
    let mut residual_findings = Vec::new();
    let mut seen = BTreeSet::new();
    append_target_residuals(
        task,
        &output_parts,
        &accepted_keep_values,
        &mut residual_findings,
        &mut seen,
    );
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
            "xlsx-verify",
        )?;
        let checked: BTreeSet<String> = detection.detectors_checked.iter().cloned().collect();
        match &mut checked_intersection {
            Some(intersection) => intersection.retain(|id| checked.contains(id)),
            None => checked_intersection = Some(checked),
        }
        diagnostics.extend(detection.diagnostics);
        for finding in detection.findings {
            let accepted = if part.kind == "shared_string_storage" {
                accepted_keep_values.contains(&(finding.entity_type, finding.matched_text.clone()))
            } else {
                accepted_keep.contains(&(
                    part.locator.clone(),
                    finding.entity_type,
                    finding.matched_text.clone(),
                ))
            };
            if accepted {
                continue;
            }
            let key = (
                part.id.clone(),
                finding.start,
                finding.end,
                finding.entity_type,
            );
            if seen.insert(key) {
                residual_findings.push(XlsxResidualFinding {
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
        Some(runtimes.ok_or(XlsxWorkflowError::EmbeddedImageRuntimeRequired)?)
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
                XlsxImageResidualFinding {
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
            code: "XLSX_METADATA_NOT_SCRUBBED".to_owned(),
            detector_id: None,
            message: "独立校验发现工作簿或 ZIP 元数据尚未完全清理。".to_owned(),
        });
    }
    if !embedded_images_passed {
        diagnostics.push(TaskDiagnostic {
            severity: DiagnosticSeverity::Error,
            code: "XLSX_EMBEDDED_IMAGE_RESIDUALS".to_owned(),
            detector_id: None,
            message: "独立 OCR 复扫在嵌入图片中发现未处理结果。".to_owned(),
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
    let mut detectors_checked = checked_intersection
        .unwrap_or_else(|| BTreeSet::from(["deterministic_rules_v1".to_owned()]));
    detectors_checked.extend(image_detectors_checked);
    let detectors_checked: Vec<String> = detectors_checked.into_iter().collect();
    let complete = text_detectors_complete
        && embedded_images_complete
        && embedded_images_checked == summary.embedded_images;
    Ok(XlsxVerificationReport {
        schema_version: 1,
        passed: unreviewed_findings == 0
            && residual_findings.is_empty()
            && image_residual_findings.is_empty()
            && embedded_images_passed
            && metadata_scrubbed
            && embedded_images_checked == summary.embedded_images,
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

pub fn verify_xlsx_file_with_runtimes(
    task: &XlsxTaskDraft,
    path: &Path,
    runtimes: Option<&RuntimeRegistry>,
) -> Result<XlsxVerificationReport, XlsxWorkflowError> {
    let bytes = fs::read(path)?;
    verify_xlsx_bytes_with_runtimes(task, &bytes, &path.to_string_lossy(), runtimes)
}

pub fn export_xlsx_task_with_runtimes(
    task: &XlsxTaskDraft,
    output: &Path,
    runtimes: Option<&RuntimeRegistry>,
) -> Result<XlsxVerificationReport, XlsxWorkflowError> {
    validate_xlsx_task(task)?;
    if output
        .extension()
        .and_then(|extension| extension.to_str())
        .is_none_or(|extension| !extension.eq_ignore_ascii_case("xlsx"))
    {
        return Err(XlsxWorkflowError::OutputTypeMismatch);
    }
    let unreviewed = task
        .findings
        .iter()
        .filter(|finding| !finding.reviewed)
        .count()
        + image_group_count(&task.embedded_images, |finding| !finding.reviewed);
    if unreviewed > 0 {
        return Err(XlsxWorkflowError::UnreviewedFindings(unreviewed));
    }
    let source_path = fs::canonicalize(&task.document.source.path)?;
    let output_absolute = if output.is_absolute() {
        output.to_path_buf()
    } else {
        std::env::current_dir()?.join(output)
    };
    if source_path == output_absolute {
        return Err(XlsxWorkflowError::WouldOverwriteSource);
    }
    if fs::symlink_metadata(output).is_ok() {
        if fs::canonicalize(output).is_ok_and(|existing| existing == source_path) {
            return Err(XlsxWorkflowError::WouldOverwriteSource);
        }
        return Err(XlsxWorkflowError::OutputExists(output.to_path_buf()));
    }
    let source_bytes = fs::read(&source_path)?;
    if sha256_hex(&source_bytes) != task.document.source.sha256 {
        return Err(XlsxWorkflowError::SourceChanged);
    }
    let output_bytes = build_redacted_package(task, &source_bytes, runtimes)?;
    let report =
        verify_xlsx_bytes_with_runtimes(task, &output_bytes, &output.to_string_lossy(), runtimes)?;
    if !report.passed {
        return Err(XlsxWorkflowError::VerificationFailed(
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
                XlsxWorkflowError::OutputExists(output.to_path_buf())
            }
            _ => XlsxWorkflowError::Io(error.error),
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
        EntityType, Finding, ImageFileKind, ImageSourceMetadata, ImageTaskDraft,
        XlsxEmbeddedImageTask, XlsxTaskDraft,
    };
    use crate::policy::PolicyConfig;
    use crate::text::sha256_hex;

    use super::{
        XlsxWorkflowError, export_xlsx_task_with_runtimes, formula_cell_locator,
        render_xlsx_embedded_image_preview, review_xlsx_finding, scan_xlsx_with_policy,
        verify_xlsx_file_with_runtimes,
    };

    fn fixture_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/xlsx/comprehensive.xlsx")
    }

    fn policy_without_models() -> PolicyConfig {
        let mut policy = PolicyConfig::default();
        policy.detectors.clear();
        policy
    }

    fn review_formula_findings(task: &mut XlsxTaskDraft) {
        let formula_parts: BTreeSet<&str> = task
            .document
            .parts
            .iter()
            .filter(|part| matches!(part.kind.as_str(), "formula" | "formula_cache"))
            .map(|part| part.id.as_str())
            .collect();
        for finding in &mut task.findings {
            if formula_parts.contains(finding.part_id.as_str()) {
                finding.selected = true;
                finding.reviewed = true;
            }
        }
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
    fn comprehensive_fixture_scans_shared_inline_hidden_comment_header_and_formula_content() {
        let task =
            scan_xlsx_with_policy(&fixture_path(), &policy_without_models(), None, None).unwrap();
        assert_eq!(task.document.source.worksheets, 2);
        assert_eq!(task.document.source.embedded_images, 0);
        assert_eq!(task.findings.len(), 10);

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
        for expected in [
            "cell",
            "comment",
            "formula",
            "formula_cache",
            "header_footer",
        ] {
            assert!(finding_kinds.contains(expected));
        }
        assert!(task.document.parts.iter().any(|part| {
            part.locator == "xl/worksheets/sheet2.xml#cell=B3#value"
                && part.text == "hidden.xlsx@example.com"
        }));
        assert!(task.findings.iter().any(|finding| {
            finding.entity_type == EntityType::PhoneNumber && finding.matched_text == "13800000001"
        }));
        assert!(
            task.findings
                .iter()
                .filter(|finding| {
                    task.document
                        .parts
                        .iter()
                        .find(|part| part.id == finding.part_id)
                        .is_some_and(|part| {
                            matches!(part.kind.as_str(), "formula" | "formula_cache")
                        })
                })
                .all(|finding| !finding.selected && !finding.reviewed)
        );
    }

    #[test]
    fn unreviewed_formula_blocks_batch_export() {
        let directory = tempdir().unwrap();
        let output = directory.path().join("blocked.xlsx");
        let task =
            scan_xlsx_with_policy(&fixture_path(), &policy_without_models(), None, None).unwrap();
        assert!(matches!(
            export_xlsx_task_with_runtimes(&task, &output, None),
            Err(XlsxWorkflowError::UnreviewedFindings(2))
        ));
        assert!(!output.exists());
    }

    #[test]
    fn formula_review_is_atomic_across_formula_and_cache() {
        let source = fixture_path();
        let source_before = fs::read(&source).unwrap();
        let mut task =
            scan_xlsx_with_policy(&source, &policy_without_models(), None, None).unwrap();
        let finding_id = task
            .findings
            .iter()
            .find(|finding| {
                task.document
                    .parts
                    .iter()
                    .find(|part| part.id == finding.part_id)
                    .is_some_and(|part| part.kind == "formula")
            })
            .unwrap()
            .id
            .clone();
        let cell_locator = task
            .document
            .parts
            .iter()
            .find(|part| {
                task.findings
                    .iter()
                    .any(|finding| finding.id == finding_id && finding.part_id == part.id)
            })
            .and_then(formula_cell_locator)
            .unwrap()
            .to_owned();

        review_xlsx_finding(&mut task, &finding_id, true, Some("[公式单元格]")).unwrap();

        let synchronized = task
            .findings
            .iter()
            .filter(|finding| {
                task.document
                    .parts
                    .iter()
                    .find(|part| part.id == finding.part_id)
                    .and_then(formula_cell_locator)
                    == Some(cell_locator.as_str())
            })
            .collect::<Vec<_>>();
        assert!(synchronized.len() >= 2);
        assert!(synchronized.iter().all(|finding| finding.reviewed));
        assert!(synchronized.iter().all(|finding| finding.selected));
        assert!(
            synchronized
                .iter()
                .all(|finding| finding.replacement == "[公式单元格]")
        );
        assert_eq!(fs::read(source).unwrap(), source_before);

        assert!(matches!(
            review_xlsx_finding(&mut task, &finding_id, true, Some(&"x".repeat(257))),
            Err(XlsxWorkflowError::Text(
                crate::workflow::WorkflowError::ReplacementTooLong
            ))
        ));
    }

    #[test]
    fn sheet_name_review_can_only_be_explicitly_retained() {
        let mut task =
            scan_xlsx_with_policy(&fixture_path(), &policy_without_models(), None, None).unwrap();
        let part = task
            .document
            .parts
            .iter()
            .find(|part| part.kind == "sheet_name")
            .unwrap()
            .clone();
        let finding_id = "finding-sheet-name".to_owned();
        task.findings.push(Finding {
            id: finding_id.clone(),
            part_id: part.id,
            start: 0,
            end: part.text.chars().count(),
            entity_type: EntityType::CustomerName,
            matched_text: part.text,
            detector: "test".to_owned(),
            confidence: 1.0,
            explanation_code: "TEST_SHEET_NAME".to_owned(),
            selected: false,
            reviewed: false,
            replacement: "[工作表]".to_owned(),
        });
        let before = task.clone();

        assert!(matches!(
            review_xlsx_finding(&mut task, &finding_id, true, Some("[工作表]")),
            Err(XlsxWorkflowError::SheetRenameUnsupported(_))
        ));
        assert_eq!(task, before);

        review_xlsx_finding(&mut task, &finding_id, false, None).unwrap();
        let reviewed = task
            .findings
            .iter()
            .find(|finding| finding.id == finding_id)
            .unwrap();
        assert!(reviewed.reviewed);
        assert!(!reviewed.selected);
    }

    #[test]
    fn fixture_export_redacts_content_preserves_structure_and_scrubs_metadata() {
        let directory = tempdir().unwrap();
        let output = directory.path().join("redacted.xlsx");
        let source = fixture_path();
        let before = fs::read(&source).unwrap();
        let mut task =
            scan_xlsx_with_policy(&source, &policy_without_models(), None, None).unwrap();
        review_formula_findings(&mut task);

        let report = export_xlsx_task_with_runtimes(&task, &output, None).unwrap();
        assert!(report.passed);
        assert!(report.complete);
        assert!(report.metadata_scrubbed);
        assert_eq!(report.target_residual_count, 0);
        assert_eq!(fs::read(&source).unwrap(), before);
        assert!(
            verify_xlsx_file_with_runtimes(&task, &output, None)
                .unwrap()
                .passed
        );

        let output_bytes = fs::read(&output).unwrap();
        let mut output_zip = ZipArchive::new(Cursor::new(&output_bytes)).unwrap();
        let mut sheet1 = String::new();
        output_zip
            .by_name("xl/worksheets/sheet1.xml")
            .unwrap()
            .read_to_string(&mut sheet1)
            .unwrap();
        assert!(sheet1.contains("[手机号]"));
        assert!(sheet1.contains("[邮箱]"));
        assert!(sheet1.contains("[身份证号]"));
        assert!(sheet1.contains("[银行卡号]"));
        assert!(!sheet1.contains("xlsx@example.com"));
        assert!(!sheet1.contains("formula@example.com"));
        assert!(!sheet1.contains("<f>"));

        let mut comments = String::new();
        output_zip
            .by_name("xl/comments1.xml")
            .unwrap()
            .read_to_string(&mut comments)
            .unwrap();
        assert!(comments.contains("LlaMask"));
        assert!(!comments.contains("Sensitive Reviewer"));
        assert!(!comments.contains("comment.xlsx@example.com"));

        let mut workbook = String::new();
        output_zip
            .by_name("xl/workbook.xml")
            .unwrap()
            .read_to_string(&mut workbook)
            .unwrap();
        assert!(workbook.contains("state=\"hidden\""));
        drop(output_zip);

        let mut source_zip = ZipArchive::new(Cursor::new(&before)).unwrap();
        let mut output_zip = ZipArchive::new(Cursor::new(&output_bytes)).unwrap();
        let mut source_styles = Vec::new();
        let mut output_styles = Vec::new();
        source_zip
            .by_name("xl/styles.xml")
            .unwrap()
            .read_to_end(&mut source_styles)
            .unwrap();
        output_zip
            .by_name("xl/styles.xml")
            .unwrap()
            .read_to_end(&mut output_styles)
            .unwrap();
        assert_eq!(output_styles, source_styles);
    }

    #[test]
    fn embedded_image_blocks_export_until_recursive_ocr_is_available() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("with-image.xlsx");
        fs::write(
            &source,
            copy_fixture_with_extra_entry(
                "xl/media/image1.png",
                &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a],
            ),
        )
        .unwrap();
        let mut task =
            scan_xlsx_with_policy(&source, &policy_without_models(), None, None).unwrap();
        review_formula_findings(&mut task);
        assert_eq!(task.document.source.embedded_images, 1);
        assert!(
            task.diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.code == "XLSX_EMBEDDED_IMAGES_PENDING" })
        );

        let output = directory.path().join("blocked.xlsx");
        assert!(matches!(
            export_xlsx_task_with_runtimes(&task, &output, None),
            Err(XlsxWorkflowError::EmbeddedImagesUnsupported)
        ));
        assert!(!output.exists());
    }

    #[test]
    fn embedded_image_preview_revalidates_and_reencodes_the_source() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("with-preview-image.xlsx");
        let mut image_bytes = Vec::new();
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(1, 1, Rgba([12, 34, 56, 255])))
            .write_to(&mut Cursor::new(&mut image_bytes), ImageFormat::Png)
            .unwrap();
        fs::write(
            &source,
            copy_fixture_with_extra_entry("xl/media/image1.png", &image_bytes),
        )
        .unwrap();
        let mut task =
            scan_xlsx_with_policy(&source, &policy_without_models(), None, None).unwrap();
        task.embedded_images.push(XlsxEmbeddedImageTask {
            entry_name: "xl/media/image1.png".to_owned(),
            task: ImageTaskDraft {
                schema_version: 1,
                task_id: "xlsx-preview-test".to_owned(),
                policy_id: task.policy_id.clone(),
                policy: task.policy.clone(),
                source: ImageSourceMetadata {
                    path: "xlsx://test#xl/media/image1.png".to_owned(),
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

        let preview = render_xlsx_embedded_image_preview(&task, 1).unwrap();
        assert_eq!((preview.width, preview.height), (1, 1));
        assert!(preview.png_bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!(matches!(
            render_xlsx_embedded_image_preview(&task, 2),
            Err(XlsxWorkflowError::EmbeddedImageTaskMismatch(_))
        ));
    }

    #[test]
    fn chart_payload_is_never_silently_preserved() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("with-chart.xlsx");
        fs::write(
            &source,
            copy_fixture_with_extra_entry("xl/charts/chart1.xml", b"<chart/>"),
        )
        .unwrap();
        let mut task =
            scan_xlsx_with_policy(&source, &policy_without_models(), None, None).unwrap();
        review_formula_findings(&mut task);
        assert!(
            task.diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.code == "XLSX_UNSUPPORTED_PAYLOAD" })
        );

        let output = directory.path().join("blocked.xlsx");
        assert!(matches!(
            export_xlsx_task_with_runtimes(&task, &output, None),
            Err(XlsxWorkflowError::UnsupportedPayload(_))
        ));
        assert!(!output.exists());
    }

    #[test]
    fn independent_rescan_blocks_a_sensitive_xlsx_replacement() {
        let directory = tempdir().unwrap();
        let output = directory.path().join("blocked.xlsx");
        let mut task =
            scan_xlsx_with_policy(&fixture_path(), &policy_without_models(), None, None).unwrap();
        review_formula_findings(&mut task);
        task.findings
            .iter_mut()
            .find(|finding| finding.entity_type == EntityType::PhoneNumber)
            .unwrap()
            .replacement = "13700000003".to_owned();
        assert!(matches!(
            export_xlsx_task_with_runtimes(&task, &output, None),
            Err(XlsxWorkflowError::VerificationFailed(_))
        ));
        assert!(!output.exists());
    }

    #[test]
    fn verification_report_never_repeats_xlsx_sensitive_values() {
        let task =
            scan_xlsx_with_policy(&fixture_path(), &policy_without_models(), None, None).unwrap();
        let report = verify_xlsx_file_with_runtimes(&task, &fixture_path(), None).unwrap();
        assert!(!report.passed);
        let serialized = serde_json::to_string(&report).unwrap();
        for finding in &task.findings {
            assert!(!serialized.contains(&finding.matched_text));
        }
    }
}
