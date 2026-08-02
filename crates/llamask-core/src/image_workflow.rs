use std::collections::BTreeSet;
use std::fs;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits, Rgba, RgbaImage};
use tempfile::Builder;
use thiserror::Error;

use crate::model::{
    DiagnosticSeverity, EntityType, FileKind, ImageFileKind, ImageFinding, ImageRect,
    ImageResidualFinding, ImageSourceMetadata, ImageTaskDraft, ImageVerificationReport,
    TaskDiagnostic,
};
use crate::policy::{PolicyConfig, PolicyError};
use crate::sidecar::{
    DetectorRuntime, OcrRunResult, OcrSidecarRequest, RuntimeRegistry, SIDECAR_PROTOCOL_VERSION,
    SidecarError, invoke_ocr,
};
use crate::text::sha256_hex;
use crate::workflow::{WorkflowError, run_detection};

const MAX_IMAGE_BYTES: usize = 50 * 1024 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 20_000;
const MAX_IMAGE_PIXELS: u64 = 100_000_000;
const MAX_DECODE_ALLOC: u64 = 512 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum ImageWorkflowError {
    #[error("图片文件读写失败：{0}")]
    Io(#[from] std::io::Error),
    #[error("图片解码或编码失败：{0}")]
    Image(#[from] image::ImageError),
    #[error("策略配置无效：{0}")]
    InvalidPolicy(#[from] PolicyError),
    #[error("文本检测失败：{0}")]
    TextDetection(#[from] WorkflowError),
    #[error("本地 OCR 失败：{0}")]
    Ocr(#[from] SidecarError),
    #[error("只支持 PNG 和 JPEG 图片")]
    UnsupportedImageType,
    #[error("图片超过 50 MiB 限制")]
    ImageTooLarge,
    #[error("图片尺寸超过安全限制")]
    ImageDimensionsTooLarge,
    #[error("找不到 OCR 运行项：{0}")]
    OcrRuntimeUnavailable(String),
    #[error("OCR 返回的图片尺寸与核心解码结果不一致")]
    OcrDimensionsMismatch,
    #[error("源图片在扫描后发生了变化，请重新扫描")]
    SourceChanged,
    #[error("任务中的 policy_id 与策略快照不一致")]
    PolicySnapshotMismatch,
    #[error("任务中的 OCR 运行项与本次配置不一致")]
    OcrRuntimeMismatch,
    #[error("遮罩矩形无效或超出图片边界：{0}")]
    InvalidMaskRect(String),
    #[error("输出路径不能与源文件相同")]
    WouldOverwriteSource,
    #[error("输出文件已存在，未执行覆盖：{0}")]
    OutputExists(PathBuf),
    #[error("输出扩展名必须与源图片格式一致")]
    OutputTypeMismatch,
    #[error("仍有 {0} 个结果尚未复核")]
    UnreviewedFindings(usize),
    #[error("图片残留复扫失败，发现 {0} 个未处理结果")]
    VerificationFailed(usize),
}

struct DecodedImage {
    bytes: Vec<u8>,
    rgba: RgbaImage,
    kind: ImageFileKind,
}

pub fn scan_image_with_policy(
    path: &Path,
    policy: &PolicyConfig,
    runtimes: &RuntimeRegistry,
    ocr_runtime_id: &str,
) -> Result<ImageTaskDraft, ImageWorkflowError> {
    policy.validate()?;
    let canonical = fs::canonicalize(path)?;
    let decoded = decode_image(&canonical)?;
    let width = decoded.rgba.width();
    let height = decoded.rgba.height();
    let source_sha256 = sha256_hex(&decoded.bytes);
    let runtime = runtimes
        .runtime(ocr_runtime_id)
        .ok_or_else(|| ImageWorkflowError::OcrRuntimeUnavailable(ocr_runtime_id.to_owned()))?;
    let ocr = run_ocr(runtime, &canonical, &source_sha256)?;
    if ocr.width != width || ocr.height != height {
        return Err(ImageWorkflowError::OcrDimensionsMismatch);
    }

    let layout = ocr_text_layout(&ocr);
    let mut findings = Vec::new();
    let mut diagnostics = ocr_diagnostics(runtime, &ocr);
    if !layout.text.is_empty() {
        let mut detection = run_detection(
            &layout.text,
            &FileKind::Text,
            &source_sha256,
            "ocr-page-0001",
            policy,
            Some(runtimes),
            "image-scan",
        )?;
        diagnostics.append(&mut detection.diagnostics);
        policy.apply_to_findings(&mut detection.findings);
        for (group_index, finding) in detection.findings.into_iter().enumerate() {
            let group_id = format!("image-group-{:04}", group_index + 1);
            for line_range in layout
                .lines
                .iter()
                .filter(|line| finding.start < line.end && line.start < finding.end)
            {
                let line = &ocr.lines[line_range.line_index];
                let text_start = finding.start.max(line_range.start) - line_range.start;
                let text_end = finding.end.min(line_range.end) - line_range.start;
                let line_fragment = scalar_slice(&line.text, text_start, text_end);
                let auto_ocr = line.score >= policy.image_mask.auto_apply_min_ocr_confidence;
                let selected = finding.selected && auto_ocr;
                let reviewed = finding.reviewed && auto_ocr;
                let finding_rect = approximate_finding_rect(
                    line.bbox,
                    &line.polygon,
                    &line.text,
                    text_start,
                    text_end,
                );
                findings.push(ImageFinding {
                    id: String::new(),
                    group_id: group_id.clone(),
                    line_index: line_range.line_index,
                    text_start,
                    text_end,
                    entity_type: finding.entity_type,
                    matched_text: finding.matched_text.clone(),
                    line_fragment,
                    recognized_line: line.text.clone(),
                    detector: finding.detector.clone(),
                    confidence: finding.confidence.min(line.score),
                    ocr_confidence: line.score,
                    explanation_code: finding.explanation_code.clone(),
                    ocr_rect: line.bbox,
                    mask_rect: expand_text_rect(
                        finding_rect,
                        line.bbox,
                        policy.image_mask.safety_margin_px,
                        width,
                        height,
                    ),
                    selected,
                    reviewed,
                });
            }
        }
    }
    findings.sort_by_key(|finding| (finding.line_index, finding.entity_type));
    for (index, finding) in findings.iter_mut().enumerate() {
        finding.id = format!("image-finding-{:04}", index + 1);
    }
    deduplicate_diagnostics(&mut diagnostics);

    Ok(ImageTaskDraft {
        schema_version: 1,
        task_id: format!("image-task-{}", &source_sha256[..12]),
        policy_id: policy.id.clone(),
        policy: policy.clone(),
        source: ImageSourceMetadata {
            path: canonical.to_string_lossy().into_owned(),
            sha256: source_sha256,
            size_bytes: decoded.bytes.len() as u64,
            file_kind: decoded.kind,
            width,
            height,
        },
        ocr_runtime_id: ocr_runtime_id.to_owned(),
        findings,
        diagnostics,
        contains_sensitive_plaintext: true,
    })
}

pub fn export_image_task_with_runtimes(
    task: &ImageTaskDraft,
    output: &Path,
    runtimes: &RuntimeRegistry,
) -> Result<ImageVerificationReport, ImageWorkflowError> {
    validate_task(task, runtimes)?;
    let unreviewed = group_count(&task.findings, |finding| !finding.reviewed);
    if unreviewed > 0 {
        return Err(ImageWorkflowError::UnreviewedFindings(unreviewed));
    }
    ensure_output_type(output, task.source.file_kind)?;
    let output_absolute = absolute_output(output)?;
    let source = fs::canonicalize(&task.source.path)?;
    if output_absolute == source {
        return Err(ImageWorkflowError::WouldOverwriteSource);
    }
    if fs::symlink_metadata(output).is_ok() {
        if fs::canonicalize(output).is_ok_and(|existing| existing == source) {
            return Err(ImageWorkflowError::WouldOverwriteSource);
        }
        return Err(ImageWorkflowError::OutputExists(output.to_path_buf()));
    }

    let mut decoded = current_source(task)?;
    validate_findings(task)?;
    paint_masks(
        &mut decoded.rgba,
        &task.findings,
        task.policy.image_mask.solid_rgb,
    );
    let redacted_bytes = encode_image(&decoded.rgba, task.source.file_kind)?;

    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let suffix = match task.source.file_kind {
        ImageFileKind::Png => ".png",
        ImageFileKind::Jpeg => ".jpg",
    };
    let mut temporary = Builder::new()
        .prefix(".llamask-verify-")
        .suffix(suffix)
        .tempfile_in(parent)?;
    temporary.write_all(&redacted_bytes)?;
    temporary.as_file().sync_all()?;
    let report = verify_image_path(
        task,
        temporary.path(),
        &redacted_bytes,
        &output.to_string_lossy(),
        runtimes,
    )?;
    if !report.passed {
        return Err(ImageWorkflowError::VerificationFailed(
            report.unreviewed_findings + report.residual_findings.len(),
        ));
    }
    temporary
        .persist_noclobber(output)
        .map_err(|error| match error.error.kind() {
            std::io::ErrorKind::AlreadyExists => {
                ImageWorkflowError::OutputExists(output.to_path_buf())
            }
            _ => ImageWorkflowError::Io(error.error),
        })?;
    Ok(report)
}

pub fn verify_image_file_with_runtimes(
    task: &ImageTaskDraft,
    path: &Path,
    runtimes: &RuntimeRegistry,
) -> Result<ImageVerificationReport, ImageWorkflowError> {
    validate_task(task, runtimes)?;
    let decoded = decode_image(path)?;
    if decoded.kind != task.source.file_kind
        || decoded.rgba.width() != task.source.width
        || decoded.rgba.height() != task.source.height
    {
        return Err(ImageWorkflowError::OutputTypeMismatch);
    }
    verify_image_path(
        task,
        &fs::canonicalize(path)?,
        &decoded.bytes,
        &path.to_string_lossy(),
        runtimes,
    )
}

fn verify_image_path(
    task: &ImageTaskDraft,
    path: &Path,
    bytes: &[u8],
    checked_file: &str,
    runtimes: &RuntimeRegistry,
) -> Result<ImageVerificationReport, ImageWorkflowError> {
    let runtime = runtimes
        .runtime(&task.ocr_runtime_id)
        .ok_or_else(|| ImageWorkflowError::OcrRuntimeUnavailable(task.ocr_runtime_id.clone()))?;
    let output_sha256 = sha256_hex(bytes);
    let ocr = run_ocr(runtime, path, &output_sha256)?;
    if ocr.width != task.source.width || ocr.height != task.source.height {
        return Err(ImageWorkflowError::OcrDimensionsMismatch);
    }

    let unreviewed_findings = group_count(&task.findings, |finding| !finding.reviewed);
    let accepted_keep: BTreeSet<(EntityType, String)> = task
        .findings
        .iter()
        .filter(|finding| finding.reviewed && !finding.selected)
        .map(|finding| (finding.entity_type, finding.matched_text.clone()))
        .collect();
    let layout = ocr_text_layout(&ocr);
    let mut residual_findings = Vec::new();
    let mut seen = BTreeSet::new();
    let mut selected_targets = BTreeSet::new();
    for finding in task.findings.iter().filter(|finding| finding.selected) {
        selected_targets.insert((finding.entity_type, finding.matched_text.as_str()));
    }
    for (entity_type, matched_text) in selected_targets {
        if let Some(byte_start) = layout.text.find(matched_text) {
            let start = layout.text[..byte_start].chars().count();
            if let Some(line_index) = layout.line_index_at(start) {
                let line = &ocr.lines[line_index];
                if seen.insert((line_index, entity_type, line.bbox)) {
                    residual_findings.push(ImageResidualFinding {
                        line_index,
                        entity_type,
                        detector: "target_value_check".to_owned(),
                        explanation_code: "SELECTED_VALUE_REMAINS".to_owned(),
                        ocr_rect: line.bbox,
                    });
                }
            }
        }
    }
    let target_residual_count = residual_findings.len();
    let mut diagnostics = ocr_diagnostics(runtime, &ocr);
    let mut detectors_checked = vec![runtime.id.clone()];
    if !layout.text.is_empty() {
        let mut detection = run_detection(
            &layout.text,
            &FileKind::Text,
            &output_sha256,
            "ocr-page-0001",
            &task.policy,
            Some(runtimes),
            "image-verify",
        )?;
        diagnostics.append(&mut detection.diagnostics);
        detectors_checked.append(&mut detection.detectors_checked);
        for finding in detection.findings {
            if accepted_keep.contains(&(finding.entity_type, finding.matched_text)) {
                continue;
            }
            if let Some(line_index) = layout.line_index_at(finding.start) {
                let line = &ocr.lines[line_index];
                if seen.insert((line_index, finding.entity_type, line.bbox)) {
                    residual_findings.push(ImageResidualFinding {
                        line_index,
                        entity_type: finding.entity_type,
                        detector: finding.detector,
                        explanation_code: finding.explanation_code,
                        ocr_rect: line.bbox,
                    });
                }
            }
        }
    }
    deduplicate_diagnostics(&mut diagnostics);
    detectors_checked.sort();
    detectors_checked.dedup();
    let complete = task
        .policy
        .detectors
        .iter()
        .filter(|detector| detector.enabled)
        .all(|detector| {
            ocr.lines.is_empty() || detectors_checked.iter().any(|id| id == &detector.id)
        });
    Ok(ImageVerificationReport {
        schema_version: 1,
        passed: unreviewed_findings == 0 && residual_findings.is_empty(),
        complete,
        checked_file: checked_file.to_owned(),
        sha256: output_sha256,
        selected_findings: group_count(&task.findings, |finding| finding.selected),
        unreviewed_findings,
        target_residual_count,
        residual_findings,
        detectors_checked,
        diagnostics,
    })
}

fn run_ocr(
    runtime: &DetectorRuntime,
    path: &Path,
    expected_sha256: &str,
) -> Result<OcrRunResult, ImageWorkflowError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        fs::canonicalize(path)?
    };
    Ok(invoke_ocr(
        runtime,
        &OcrSidecarRequest {
            protocol_version: SIDECAR_PROTOCOL_VERSION,
            request_id: format!("ocr-{}", &expected_sha256[..12]),
            detector_id: runtime.id.clone(),
            image_path: absolute.to_string_lossy().into_owned(),
            expected_sha256: expected_sha256.to_owned(),
        },
    )?)
}

fn decode_image(path: &Path) -> Result<DecodedImage, ImageWorkflowError> {
    let bytes = fs::read(path)?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(ImageWorkflowError::ImageTooLarge);
    }
    let format = image::guess_format(&bytes)?;
    let kind = match format {
        ImageFormat::Png => ImageFileKind::Png,
        ImageFormat::Jpeg => ImageFileKind::Jpeg,
        _ => return Err(ImageWorkflowError::UnsupportedImageType),
    };
    let mut reader = ImageReader::with_format(Cursor::new(&bytes), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    let mut decoder = reader.into_decoder()?;
    let orientation = decoder.orientation()?;
    let mut image = DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
    if u64::from(image.width()) * u64::from(image.height()) > MAX_IMAGE_PIXELS {
        return Err(ImageWorkflowError::ImageDimensionsTooLarge);
    }
    Ok(DecodedImage {
        bytes,
        rgba: image.into_rgba8(),
        kind,
    })
}

fn encode_image(image: &RgbaImage, kind: ImageFileKind) -> Result<Vec<u8>, ImageWorkflowError> {
    let dynamic = DynamicImage::ImageRgba8(image.clone());
    let mut output = Vec::new();
    match kind {
        ImageFileKind::Png => dynamic.write_to(&mut Cursor::new(&mut output), ImageFormat::Png)?,
        ImageFileKind::Jpeg => {
            let rgb = DynamicImage::ImageRgb8(dynamic.into_rgb8());
            JpegEncoder::new_with_quality(&mut output, 95).encode_image(&rgb)?;
        }
    }
    Ok(output)
}

fn current_source(task: &ImageTaskDraft) -> Result<DecodedImage, ImageWorkflowError> {
    let decoded = decode_image(Path::new(&task.source.path))?;
    if sha256_hex(&decoded.bytes) != task.source.sha256
        || decoded.kind != task.source.file_kind
        || decoded.rgba.width() != task.source.width
        || decoded.rgba.height() != task.source.height
    {
        return Err(ImageWorkflowError::SourceChanged);
    }
    Ok(decoded)
}

fn validate_task(
    task: &ImageTaskDraft,
    runtimes: &RuntimeRegistry,
) -> Result<(), ImageWorkflowError> {
    task.policy.validate()?;
    if task.policy_id != task.policy.id {
        return Err(ImageWorkflowError::PolicySnapshotMismatch);
    }
    if runtimes.runtime(&task.ocr_runtime_id).is_none() {
        return Err(ImageWorkflowError::OcrRuntimeMismatch);
    }
    validate_findings(task)
}

fn validate_findings(task: &ImageTaskDraft) -> Result<(), ImageWorkflowError> {
    for finding in &task.findings {
        if !valid_rect(finding.ocr_rect, task.source.width, task.source.height)
            || !valid_rect(finding.mask_rect, task.source.width, task.source.height)
            || finding.line_index > 10_000
            || finding.text_start >= finding.text_end
            || finding.text_end > finding.recognized_line.chars().count()
            || finding
                .recognized_line
                .chars()
                .skip(finding.text_start)
                .take(finding.text_end - finding.text_start)
                .collect::<String>()
                != finding.line_fragment
            || finding.group_id.is_empty()
            || finding.matched_text.is_empty()
            || finding.line_fragment.is_empty()
            || finding.recognized_line.is_empty()
            || !finding.confidence.is_finite()
            || !(0.0..=1.0).contains(&finding.confidence)
            || !finding.ocr_confidence.is_finite()
            || !(0.0..=1.0).contains(&finding.ocr_confidence)
        {
            return Err(ImageWorkflowError::InvalidMaskRect(finding.id.clone()));
        }
    }
    Ok(())
}

#[derive(Debug)]
struct OcrLineRange {
    line_index: usize,
    start: usize,
    end: usize,
}

#[derive(Debug)]
struct OcrTextLayout {
    text: String,
    lines: Vec<OcrLineRange>,
}

impl OcrTextLayout {
    fn line_index_at(&self, offset: usize) -> Option<usize> {
        self.lines
            .iter()
            .find(|line| line.start <= offset && offset < line.end)
            .map(|line| line.line_index)
    }
}

fn ocr_text_layout(ocr: &OcrRunResult) -> OcrTextLayout {
    let mut text = String::new();
    let mut lines = Vec::with_capacity(ocr.lines.len());
    let mut cursor = 0usize;
    for (line_index, line) in ocr.lines.iter().enumerate() {
        if line_index > 0 {
            text.push('\n');
            cursor += 1;
        }
        let start = cursor;
        text.push_str(&line.text);
        cursor += line.text.chars().count();
        lines.push(OcrLineRange {
            line_index,
            start,
            end: cursor,
        });
    }
    OcrTextLayout { text, lines }
}

fn scalar_slice(text: &str, start: usize, end: usize) -> String {
    text.chars().skip(start).take(end - start).collect()
}

fn group_count(findings: &[ImageFinding], predicate: impl Fn(&ImageFinding) -> bool) -> usize {
    findings
        .iter()
        .filter(|finding| predicate(finding))
        .map(|finding| finding.group_id.as_str())
        .collect::<BTreeSet<_>>()
        .len()
}

fn valid_rect(rect: ImageRect, width: u32, height: u32) -> bool {
    rect.x0 < rect.x1 && rect.y0 < rect.y1 && rect.x1 <= width && rect.y1 <= height
}

/// OCR engines return line boxes rather than glyph boxes. Proportional fonts can
/// otherwise leave the first or last glyph outside a character-ratio estimate,
/// so add roughly one glyph of padding along the writing direction.
fn expand_text_rect(
    rect: ImageRect,
    line_rect: ImageRect,
    margin: u32,
    width: u32,
    height: u32,
) -> ImageRect {
    let line_width = line_rect.x1.saturating_sub(line_rect.x0);
    let line_height = line_rect.y1.saturating_sub(line_rect.y0);
    let (x_margin, y_margin) = if line_width >= line_height {
        (
            margin.saturating_add(line_height.saturating_add(1) / 2),
            margin,
        )
    } else {
        (
            margin,
            margin.saturating_add(line_width.saturating_add(1) / 2),
        )
    };
    ImageRect {
        x0: rect.x0.saturating_sub(x_margin),
        y0: rect.y0.saturating_sub(y_margin),
        x1: rect.x1.saturating_add(x_margin).min(width),
        y1: rect.y1.saturating_add(y_margin).min(height),
    }
}

fn approximate_finding_rect(
    line_rect: ImageRect,
    polygon: &[[u32; 2]],
    text: &str,
    start: usize,
    end: usize,
) -> ImageRect {
    let chars: Vec<char> = text.chars().collect();
    if start >= end || end > chars.len() || chars.len() <= 1 || polygon.len() != 4 {
        return line_rect;
    }
    let width = line_rect.x1 - line_rect.x0;
    let height = line_rect.y1 - line_rect.y0;
    let top_skew = polygon[0][1].abs_diff(polygon[1][1]);
    let side_skew = polygon[0][0].abs_diff(polygon[3][0]);
    let horizontal = width >= height.saturating_mul(2)
        && top_skew <= height.saturating_div(4).max(4)
        && side_skew <= height.saturating_div(4).max(4);
    let vertical = height >= width.saturating_mul(2)
        && top_skew <= width.saturating_div(4).max(4)
        && side_skew <= width.saturating_div(4).max(4);
    if !horizontal && !vertical {
        return line_rect;
    }
    let total: f64 = chars.iter().copied().map(character_width_weight).sum();
    let before: f64 = chars[..start]
        .iter()
        .copied()
        .map(character_width_weight)
        .sum();
    let through: f64 = chars[..end]
        .iter()
        .copied()
        .map(character_width_weight)
        .sum();
    if total <= 0.0 {
        return line_rect;
    }
    if horizontal {
        let x0 = line_rect.x0 + (f64::from(width) * before / total).floor() as u32;
        let x1 = line_rect.x0 + (f64::from(width) * through / total).ceil() as u32;
        ImageRect {
            x0,
            y0: line_rect.y0,
            x1: x1.max(x0 + 1).min(line_rect.x1),
            y1: line_rect.y1,
        }
    } else {
        let y0 = line_rect.y0 + (f64::from(height) * before / total).floor() as u32;
        let y1 = line_rect.y0 + (f64::from(height) * through / total).ceil() as u32;
        ImageRect {
            x0: line_rect.x0,
            y0,
            x1: line_rect.x1,
            y1: y1.max(y0 + 1).min(line_rect.y1),
        }
    }
}

fn character_width_weight(value: char) -> f64 {
    if value.is_ascii() {
        if value.is_ascii_punctuation() {
            0.75
        } else {
            1.0
        }
    } else {
        1.5
    }
}

fn paint_masks(image: &mut RgbaImage, findings: &[ImageFinding], color: [u8; 3]) {
    let pixel = Rgba([color[0], color[1], color[2], 255]);
    for finding in findings.iter().filter(|finding| finding.selected) {
        for y in finding.mask_rect.y0..finding.mask_rect.y1 {
            for x in finding.mask_rect.x0..finding.mask_rect.x1 {
                image.put_pixel(x, y, pixel);
            }
        }
    }
}

fn ensure_output_type(path: &Path, kind: ImageFileKind) -> Result<(), ImageWorkflowError> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase);
    let matches = match kind {
        ImageFileKind::Png => extension.as_deref() == Some("png"),
        ImageFileKind::Jpeg => matches!(extension.as_deref(), Some("jpg" | "jpeg")),
    };
    if matches {
        Ok(())
    } else {
        Err(ImageWorkflowError::OutputTypeMismatch)
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

fn ocr_diagnostics(runtime: &DetectorRuntime, ocr: &OcrRunResult) -> Vec<TaskDiagnostic> {
    let mut diagnostics = Vec::new();
    if ocr.lines.is_empty() {
        diagnostics.push(TaskDiagnostic {
            severity: DiagnosticSeverity::Warning,
            code: "OCR_NO_TEXT_FOUND".to_owned(),
            detector_id: Some(runtime.id.clone()),
            message: "OCR 未识别到文字；如图片肉眼可见文字，需要人工复核。".to_owned(),
        });
    }
    if !ocr.warnings.is_empty() {
        diagnostics.push(TaskDiagnostic {
            severity: DiagnosticSeverity::Warning,
            code: "OCR_RUNTIME_WARNING".to_owned(),
            detector_id: Some(runtime.id.clone()),
            message: "本地 OCR 返回了运行提示；详细内容未写入任务，避免泄露原文。".to_owned(),
        });
    }
    diagnostics
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
    use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
    use tempfile::tempdir;

    use super::{approximate_finding_rect, decode_image, expand_text_rect, valid_rect};
    use crate::model::ImageRect;

    #[test]
    fn safety_margin_is_clamped_to_image_bounds() {
        let rect = expand_text_rect(
            ImageRect {
                x0: 2,
                y0: 3,
                x1: 98,
                y1: 49,
            },
            ImageRect {
                x0: 2,
                y0: 3,
                x1: 98,
                y1: 49,
            },
            4,
            100,
            50,
        );
        assert_eq!(
            rect,
            ImageRect {
                x0: 0,
                y0: 0,
                x1: 100,
                y1: 50,
            }
        );
        assert!(valid_rect(rect, 100, 50));
    }

    #[test]
    fn text_mask_adds_one_glyph_of_horizontal_safety() {
        let rect = expand_text_rect(
            ImageRect {
                x0: 100,
                y0: 40,
                x1: 300,
                y1: 70,
            },
            ImageRect {
                x0: 20,
                y0: 40,
                x1: 300,
                y1: 70,
            },
            4,
            400,
            100,
        );
        assert_eq!(
            rect,
            ImageRect {
                x0: 81,
                y0: 36,
                x1: 319,
                y1: 74,
            }
        );
    }

    #[test]
    fn mixed_chinese_and_digits_map_to_a_precise_sub_rectangle() {
        let rect = approximate_finding_rect(
            ImageRect {
                x0: 63,
                y0: 54,
                x1: 786,
                y1: 82,
            },
            &[[63, 54], [786, 54], [786, 82], [63, 82]],
            "沈景行的居民身份证号码为110105198003150020，仅限本",
            12,
            30,
        );
        assert!(rect.x0 >= 360 && rect.x0 <= 390);
        assert!(rect.x1 >= 670 && rect.x1 <= 700);
        assert_eq!(rect.y0, 54);
        assert_eq!(rect.y1, 82);
    }

    #[test]
    fn png_transparency_survives_core_decoding() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("transparent.png");
        let image = RgbaImage::from_pixel(2, 2, Rgba([10, 20, 30, 40]));
        DynamicImage::ImageRgba8(image)
            .save_with_format(&path, ImageFormat::Png)
            .unwrap();

        let decoded = decode_image(&path).unwrap();
        assert_eq!(decoded.rgba.get_pixel(0, 0).0, [10, 20, 30, 40]);
    }
}
