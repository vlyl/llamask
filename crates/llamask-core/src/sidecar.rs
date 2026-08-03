use std::collections::BTreeSet;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::model::{EntityType, Finding, ImageRect};
use crate::text::char_to_byte;

pub const SIDECAR_PROTOCOL_VERSION: u32 = 1;
const MAX_STDOUT_BYTES: u64 = 1_048_576;
const MAX_STDERR_BYTES: u64 = 8_192;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum DetectorKind {
    InformationExtraction,
    LlmReview,
    Ocr,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum RuntimeToolKind {
    #[serde(rename = "pdfinfo")]
    PdfInfo,
    #[serde(rename = "pdftoppm")]
    PdfToPpm,
}

impl RuntimeToolKind {
    pub fn id(self) -> &'static str {
        match self {
            Self::PdfInfo => "pdfinfo",
            Self::PdfToPpm => "pdftoppm",
        }
    }
}

impl DetectorKind {
    pub fn priority(self) -> u8 {
        match self {
            Self::InformationExtraction => 20,
            Self::LlmReview => 10,
            Self::Ocr => 20,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DetectorRuntime {
    pub id: String,
    pub kind: DetectorKind,
    pub executable: PathBuf,
    #[serde(default)]
    pub working_directory: Option<PathBuf>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub assets: Vec<RuntimeAsset>,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeAsset {
    pub path: PathBuf,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeTool {
    pub kind: RuntimeToolKind,
    pub executable: PathBuf,
    pub sha256: String,
    #[serde(default)]
    pub assets: Vec<RuntimeAsset>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeRegistry {
    pub schema_version: u32,
    pub detectors: Vec<DetectorRuntime>,
    #[serde(default)]
    pub tools: Vec<RuntimeTool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CandidateSpan {
    pub start: usize,
    pub end: usize,
    pub entity_type: EntityType,
    pub matched_text: String,
    pub detector: String,
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SidecarRequest {
    pub protocol_version: u32,
    pub request_id: String,
    pub detector_id: String,
    pub document_id: String,
    pub part_id: String,
    pub offset_unit: String,
    pub text: String,
    pub candidates: Vec<CandidateSpan>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SidecarFinding {
    pub start: usize,
    pub end: usize,
    pub entity_type: EntityType,
    pub matched_text: String,
    pub confidence: f32,
    pub reason_code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SidecarResponse {
    pub protocol_version: u32,
    pub request_id: String,
    pub findings: Vec<SidecarFinding>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OcrSidecarRequest {
    pub protocol_version: u32,
    pub request_id: String,
    pub detector_id: String,
    pub image_path: String,
    pub expected_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OcrSidecarLine {
    pub text: String,
    pub score: f32,
    pub bbox: [u32; 4],
    pub polygon: Vec<[u32; 2]>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OcrSidecarResponse {
    pub protocol_version: u32,
    pub request_id: String,
    pub source_sha256: String,
    pub width: u32,
    pub height: u32,
    pub lines: Vec<OcrSidecarLine>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecognizedLine {
    pub text: String,
    pub score: f32,
    pub bbox: ImageRect,
    pub polygon: Vec<[u32; 2]>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OcrRunResult {
    pub source_sha256: String,
    pub width: u32,
    pub height: u32,
    pub lines: Vec<RecognizedLine>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SidecarRunResult {
    pub findings: Vec<Finding>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Error)]
pub enum SidecarError {
    #[error("模型运行注册表读写失败：{0}")]
    RegistryIo(#[from] std::io::Error),
    #[error("模型运行注册表 JSON 无效：{0}")]
    RegistryJson(#[from] serde_json::Error),
    #[error("只支持模型运行注册表 schema_version=1")]
    UnsupportedRegistrySchema,
    #[error("模型运行项 id 不能为空")]
    EmptyRuntimeId,
    #[error("模型运行项 id 重复：{0}")]
    DuplicateRuntime(String),
    #[error("模型运行项 {0} 没有可执行文件")]
    MissingExecutable(String),
    #[error("模型运行项 {0} 的超时必须位于 100ms 到 10min")]
    InvalidTimeout(String),
    #[error("模型运行项 {0} 包含无效的资源 SHA-256")]
    InvalidAssetHash(String),
    #[error("本地工具重复：{0}")]
    DuplicateTool(String),
    #[error("本地工具 {0} 包含无效的 SHA-256")]
    InvalidToolHash(String),
    #[error("本地模型 {0} 的可执行文件或工作目录不可用")]
    RuntimeUnavailable(String),
    #[error("本地模型 {0} 的必要资源无法读取")]
    AssetUnreadable(String),
    #[error("本地模型 {0} 的必要资源完整性校验失败")]
    AssetHashMismatch(String),
    #[error("本地工具 {0} 不可用")]
    ToolUnavailable(String),
    #[error("本地工具 {0} 的必要资源无法读取")]
    ToolAssetUnreadable(String),
    #[error("本地工具 {0} 的必要资源完整性校验失败")]
    ToolAssetHashMismatch(String),
    #[error("无法启动本地模型 {0}")]
    Spawn(String),
    #[error("无法向本地模型 {0} 发送请求")]
    WriteRequest(String),
    #[error("本地模型 {0} 超时")]
    Timeout(String),
    #[error("本地模型 {0} 异常退出")]
    ProcessFailed(String),
    #[error("本地模型 {0} 返回内容过大")]
    ResponseTooLarge(String),
    #[error("本地模型 {0} 返回内容读取失败")]
    ReadResponse(String),
    #[error("本地模型 {0} 返回的不是有效 JSON")]
    InvalidJson(String),
    #[error("本地模型 {0} 使用了错误的协议版本")]
    ProtocolMismatch(String),
    #[error("本地模型 {0} 返回了错误的 request_id")]
    RequestMismatch(String),
    #[error("本地模型 {0} 返回了无效的置信度")]
    InvalidConfidence(String),
    #[error("本地模型 {0} 返回了越界或空的文本位置")]
    InvalidSpan(String),
    #[error("本地模型 {0} 返回的位置与原文不一致")]
    TextMismatch(String),
    #[error("本地模型 {0} 不是 OCR 运行项")]
    WrongRuntimeKind(String),
    #[error("本地 OCR {0} 返回了错误的源文件哈希")]
    SourceHashMismatch(String),
    #[error("本地 OCR {0} 返回了无效的图片尺寸")]
    InvalidImageDimensions(String),
    #[error("本地 OCR {0} 返回了无效的文本行或坐标")]
    InvalidOcrLine(String),
}

impl RuntimeRegistry {
    pub fn from_path(path: &Path) -> Result<Self, SidecarError> {
        let bytes = fs::read(path)?;
        let mut registry: Self = serde_json::from_slice(&bytes)?;
        let base = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        for runtime in &mut registry.detectors {
            if runtime.executable.is_relative() {
                runtime.executable = base.join(&runtime.executable);
            }
            if let Some(working_directory) = &mut runtime.working_directory
                && working_directory.is_relative()
            {
                *working_directory = base.join(&working_directory);
            }
            for asset in &mut runtime.assets {
                if asset.path.is_relative() {
                    asset.path = base.join(&asset.path);
                }
            }
        }
        for tool in &mut registry.tools {
            if tool.executable.is_relative() {
                tool.executable = base.join(&tool.executable);
            }
            for asset in &mut tool.assets {
                if asset.path.is_relative() {
                    asset.path = base.join(&asset.path);
                }
            }
        }
        registry.validate()?;
        Ok(registry)
    }

    pub fn validate(&self) -> Result<(), SidecarError> {
        if self.schema_version != 1 {
            return Err(SidecarError::UnsupportedRegistrySchema);
        }
        let mut ids = BTreeSet::new();
        for runtime in &self.detectors {
            if runtime.id.trim().is_empty() {
                return Err(SidecarError::EmptyRuntimeId);
            }
            if !ids.insert(runtime.id.as_str()) {
                return Err(SidecarError::DuplicateRuntime(runtime.id.clone()));
            }
            if runtime.executable.as_os_str().is_empty() {
                return Err(SidecarError::MissingExecutable(runtime.id.clone()));
            }
            if !(100..=600_000).contains(&runtime.timeout_ms) {
                return Err(SidecarError::InvalidTimeout(runtime.id.clone()));
            }
            if runtime.assets.iter().any(|asset| {
                asset.sha256.len() != 64
                    || !asset.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
            }) {
                return Err(SidecarError::InvalidAssetHash(runtime.id.clone()));
            }
        }
        let mut tool_kinds = BTreeSet::new();
        for tool in &self.tools {
            let id = tool.kind.id();
            if !tool_kinds.insert(tool.kind) {
                return Err(SidecarError::DuplicateTool(id.to_owned()));
            }
            if tool.executable.as_os_str().is_empty()
                || !valid_sha256(&tool.sha256)
                || tool.assets.iter().any(|asset| !valid_sha256(&asset.sha256))
            {
                return Err(SidecarError::InvalidToolHash(id.to_owned()));
            }
        }
        Ok(())
    }

    pub fn runtime(&self, id: &str) -> Option<&DetectorRuntime> {
        self.detectors.iter().find(|runtime| runtime.id == id)
    }

    pub fn tool(&self, kind: RuntimeToolKind) -> Option<&RuntimeTool> {
        self.tools.iter().find(|tool| tool.kind == kind)
    }

    pub fn verify_detectors(&self) -> Result<(), SidecarError> {
        for runtime in &self.detectors {
            if !runtime.executable.is_file()
                || runtime
                    .working_directory
                    .as_ref()
                    .is_some_and(|directory| !directory.is_dir())
            {
                return Err(SidecarError::RuntimeUnavailable(runtime.id.clone()));
            }
            verify_assets(runtime)?;
        }
        Ok(())
    }

    pub fn verified_tool(&self, kind: RuntimeToolKind) -> Result<Option<&Path>, SidecarError> {
        let Some(tool) = self.tool(kind) else {
            return Ok(None);
        };
        verify_tool(tool)?;
        Ok(Some(&tool.executable))
    }

    pub fn verify_installation(&self) -> Result<(), SidecarError> {
        self.verify_detectors()?;
        for tool in &self.tools {
            verify_tool(tool)?;
        }
        Ok(())
    }
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub fn candidates_from_findings(findings: &[Finding]) -> Vec<CandidateSpan> {
    findings
        .iter()
        .map(|finding| CandidateSpan {
            start: finding.start,
            end: finding.end,
            entity_type: finding.entity_type,
            matched_text: finding.matched_text.clone(),
            detector: finding.detector.clone(),
            confidence: finding.confidence,
        })
        .collect()
}

pub fn invoke(
    runtime: &DetectorRuntime,
    request: &SidecarRequest,
    min_confidence: f32,
) -> Result<SidecarRunResult, SidecarError> {
    if request.protocol_version != SIDECAR_PROTOCOL_VERSION
        || request.offset_unit != "unicode_scalar"
    {
        return Err(SidecarError::ProtocolMismatch(runtime.id.clone()));
    }
    if request.detector_id != runtime.id {
        return Err(SidecarError::RequestMismatch(runtime.id.clone()));
    }
    verify_assets(runtime)?;
    let request_bytes =
        serde_json::to_vec(request).map_err(|_| SidecarError::WriteRequest(runtime.id.clone()))?;
    let stdout = run_process(runtime, &request_bytes)?;
    let response: SidecarResponse = serde_json::from_slice(&stdout)
        .map_err(|_| SidecarError::InvalidJson(runtime.id.clone()))?;
    let warnings = response.warnings.clone();
    let findings = response_to_findings(runtime, request, response, min_confidence)?;
    Ok(SidecarRunResult { findings, warnings })
}

pub fn invoke_ocr(
    runtime: &DetectorRuntime,
    request: &OcrSidecarRequest,
) -> Result<OcrRunResult, SidecarError> {
    if runtime.kind != DetectorKind::Ocr {
        return Err(SidecarError::WrongRuntimeKind(runtime.id.clone()));
    }
    if request.protocol_version != SIDECAR_PROTOCOL_VERSION {
        return Err(SidecarError::ProtocolMismatch(runtime.id.clone()));
    }
    if request.detector_id != runtime.id {
        return Err(SidecarError::RequestMismatch(runtime.id.clone()));
    }
    verify_assets(runtime)?;
    let request_bytes =
        serde_json::to_vec(request).map_err(|_| SidecarError::WriteRequest(runtime.id.clone()))?;
    let stdout = run_process(runtime, &request_bytes)?;
    let response: OcrSidecarResponse = serde_json::from_slice(&stdout)
        .map_err(|_| SidecarError::InvalidJson(runtime.id.clone()))?;
    validate_ocr_response(runtime, request, response)
}

fn run_process(runtime: &DetectorRuntime, request_bytes: &[u8]) -> Result<Vec<u8>, SidecarError> {
    let mut command = Command::new(&runtime.executable);
    command
        .args(&runtime.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("HF_HUB_OFFLINE", "1")
        .env("TRANSFORMERS_OFFLINE", "1")
        .env("MODELSCOPE_OFFLINE", "1")
        .env("PADDLE_PDX_DISABLE_MODEL_SOURCE_CHECK", "True")
        .env("PYTHONNOUSERSITE", "1")
        .env("NO_PROXY", "*")
        .env_remove("HTTP_PROXY")
        .env_remove("HTTPS_PROXY")
        .env_remove("ALL_PROXY");
    if let Some(working_directory) = &runtime.working_directory {
        command.current_dir(working_directory);
    }
    let mut child = command
        .spawn()
        .map_err(|_| SidecarError::Spawn(runtime.id.clone()))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| SidecarError::WriteRequest(runtime.id.clone()))?;
    if stdin
        .write_all(request_bytes)
        .and_then(|_| stdin.write_all(b"\n"))
        .is_err()
    {
        drop(stdin);
        let _ = child.kill();
        let _ = child.wait();
        return Err(SidecarError::WriteRequest(runtime.id.clone()));
    }
    drop(stdin);

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| SidecarError::ReadResponse(runtime.id.clone()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| SidecarError::ReadResponse(runtime.id.clone()))?;
    let stdout_reader = thread::spawn(move || read_limited(stdout, MAX_STDOUT_BYTES));
    let stderr_reader = thread::spawn(move || read_limited(stderr, MAX_STDERR_BYTES));

    let deadline = Instant::now() + Duration::from_millis(runtime.timeout_ms);
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|_| SidecarError::ProcessFailed(runtime.id.clone()))?
        {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(SidecarError::Timeout(runtime.id.clone()));
        }
        thread::sleep(Duration::from_millis(10));
    };

    let stdout = stdout_reader
        .join()
        .map_err(|_| SidecarError::ReadResponse(runtime.id.clone()))?
        .map_err(|_| SidecarError::ReadResponse(runtime.id.clone()))?;
    let _stderr = stderr_reader
        .join()
        .map_err(|_| SidecarError::ReadResponse(runtime.id.clone()))?
        .map_err(|_| SidecarError::ReadResponse(runtime.id.clone()))?;
    if stdout.len() as u64 > MAX_STDOUT_BYTES {
        return Err(SidecarError::ResponseTooLarge(runtime.id.clone()));
    }
    if !status.success() {
        return Err(SidecarError::ProcessFailed(runtime.id.clone()));
    }
    Ok(stdout)
}

fn validate_ocr_response(
    runtime: &DetectorRuntime,
    request: &OcrSidecarRequest,
    response: OcrSidecarResponse,
) -> Result<OcrRunResult, SidecarError> {
    if response.protocol_version != SIDECAR_PROTOCOL_VERSION {
        return Err(SidecarError::ProtocolMismatch(runtime.id.clone()));
    }
    if response.request_id != request.request_id {
        return Err(SidecarError::RequestMismatch(runtime.id.clone()));
    }
    if !response
        .source_sha256
        .eq_ignore_ascii_case(&request.expected_sha256)
    {
        return Err(SidecarError::SourceHashMismatch(runtime.id.clone()));
    }
    if response.width == 0
        || response.height == 0
        || response.width > 20_000
        || response.height > 20_000
        || u64::from(response.width) * u64::from(response.height) > 100_000_000
    {
        return Err(SidecarError::InvalidImageDimensions(runtime.id.clone()));
    }
    if response.lines.len() > 10_000 {
        return Err(SidecarError::InvalidOcrLine(runtime.id.clone()));
    }
    let mut lines = Vec::with_capacity(response.lines.len());
    for line in response.lines {
        let [x0, y0, x1, y1] = line.bbox;
        let invalid_polygon = line.polygon.len() < 4
            || line.polygon.len() > 32
            || line
                .polygon
                .iter()
                .any(|[x, y]| *x > response.width || *y > response.height);
        if line.text.is_empty()
            || line.text.chars().count() > 4_096
            || line.text.contains(['\r', '\n', '\0'])
            || !line.score.is_finite()
            || !(0.0..=1.0).contains(&line.score)
            || x0 >= x1
            || y0 >= y1
            || x1 > response.width
            || y1 > response.height
            || invalid_polygon
        {
            return Err(SidecarError::InvalidOcrLine(runtime.id.clone()));
        }
        lines.push(RecognizedLine {
            text: line.text,
            score: line.score,
            bbox: ImageRect { x0, y0, x1, y1 },
            polygon: line.polygon,
        });
    }
    Ok(OcrRunResult {
        source_sha256: response.source_sha256,
        width: response.width,
        height: response.height,
        lines,
        warnings: response.warnings,
    })
}

fn verify_assets(runtime: &DetectorRuntime) -> Result<(), SidecarError> {
    for asset in &runtime.assets {
        let mut file = fs::File::open(&asset.path)
            .map_err(|_| SidecarError::AssetUnreadable(runtime.id.clone()))?;
        let mut digest = Sha256::new();
        let mut buffer = [0_u8; 1024 * 1024];
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|_| SidecarError::AssetUnreadable(runtime.id.clone()))?;
            if read == 0 {
                break;
            }
            digest.update(&buffer[..read]);
        }
        let actual = format!("{:x}", digest.finalize());
        if !actual.eq_ignore_ascii_case(&asset.sha256) {
            return Err(SidecarError::AssetHashMismatch(runtime.id.clone()));
        }
    }
    Ok(())
}

fn verify_tool(tool: &RuntimeTool) -> Result<(), SidecarError> {
    let id = tool.kind.id().to_owned();
    if !tool.executable.is_file() {
        return Err(SidecarError::ToolUnavailable(id));
    }
    verify_tool_path(&tool.executable, &tool.sha256, tool.kind)?;
    for asset in &tool.assets {
        verify_tool_path(&asset.path, &asset.sha256, tool.kind)?;
    }
    Ok(())
}

fn verify_tool_path(
    path: &Path,
    expected_sha256: &str,
    kind: RuntimeToolKind,
) -> Result<(), SidecarError> {
    let id = kind.id().to_owned();
    let mut file =
        fs::File::open(path).map_err(|_| SidecarError::ToolAssetUnreadable(id.clone()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| SidecarError::ToolAssetUnreadable(id.clone()))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    let actual = format!("{:x}", digest.finalize());
    if !actual.eq_ignore_ascii_case(expected_sha256) {
        return Err(SidecarError::ToolAssetHashMismatch(id));
    }
    Ok(())
}

fn read_limited(reader: impl Read, limit: u64) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    reader.take(limit + 1).read_to_end(&mut output)?;
    Ok(output)
}

pub fn response_to_findings(
    runtime: &DetectorRuntime,
    request: &SidecarRequest,
    response: SidecarResponse,
    min_confidence: f32,
) -> Result<Vec<Finding>, SidecarError> {
    if response.protocol_version != SIDECAR_PROTOCOL_VERSION {
        return Err(SidecarError::ProtocolMismatch(runtime.id.clone()));
    }
    if response.request_id != request.request_id {
        return Err(SidecarError::RequestMismatch(runtime.id.clone()));
    }
    let char_len = request.text.chars().count();
    let mut findings = Vec::new();
    for result in response.findings {
        if !result.confidence.is_finite() || !(0.0..=1.0).contains(&result.confidence) {
            return Err(SidecarError::InvalidConfidence(runtime.id.clone()));
        }
        if result.start >= result.end || result.end > char_len {
            return Err(SidecarError::InvalidSpan(runtime.id.clone()));
        }
        let start = char_to_byte(&request.text, result.start)
            .ok_or_else(|| SidecarError::InvalidSpan(runtime.id.clone()))?;
        let end = char_to_byte(&request.text, result.end)
            .ok_or_else(|| SidecarError::InvalidSpan(runtime.id.clone()))?;
        if request.text.get(start..end) != Some(result.matched_text.as_str()) {
            return Err(SidecarError::TextMismatch(runtime.id.clone()));
        }
        if result.confidence < min_confidence {
            continue;
        }
        findings.push(Finding {
            id: String::new(),
            part_id: request.part_id.clone(),
            start: result.start,
            end: result.end,
            entity_type: result.entity_type,
            matched_text: result.matched_text,
            detector: runtime.id.clone(),
            confidence: result.confidence,
            explanation_code: result.reason_code,
            selected: true,
            reviewed: false,
            replacement: result.entity_type.placeholder().to_owned(),
        });
    }
    Ok(findings)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use tempfile::tempdir;

    use crate::model::EntityType;
    use crate::text::sha256_hex;

    use super::{
        DetectorKind, DetectorRuntime, OcrSidecarLine, OcrSidecarRequest, OcrSidecarResponse,
        RuntimeAsset, RuntimeRegistry, RuntimeTool, RuntimeToolKind, SIDECAR_PROTOCOL_VERSION,
        SidecarError, SidecarFinding, SidecarRequest, SidecarResponse, response_to_findings,
        validate_ocr_response, verify_assets,
    };

    fn runtime() -> DetectorRuntime {
        DetectorRuntime {
            id: "siamese_uie".to_owned(),
            kind: DetectorKind::InformationExtraction,
            executable: PathBuf::from("/unused"),
            working_directory: None,
            args: Vec::new(),
            assets: Vec::new(),
            timeout_ms: 1_000,
        }
    }

    fn request() -> SidecarRequest {
        SidecarRequest {
            protocol_version: SIDECAR_PROTOCOL_VERSION,
            request_id: "request-1".to_owned(),
            detector_id: "siamese_uie".to_owned(),
            document_id: "document-1".to_owned(),
            part_id: "part-0001".to_owned(),
            offset_unit: "unicode_scalar".to_owned(),
            text: "客户🙂星海科技有限公司".to_owned(),
            candidates: Vec::new(),
        }
    }

    fn ocr_runtime() -> DetectorRuntime {
        DetectorRuntime {
            id: "pp_ocr_small".to_owned(),
            kind: DetectorKind::Ocr,
            executable: PathBuf::from("/unused"),
            working_directory: None,
            args: Vec::new(),
            assets: Vec::new(),
            timeout_ms: 1_000,
        }
    }

    fn ocr_request() -> OcrSidecarRequest {
        OcrSidecarRequest {
            protocol_version: SIDECAR_PROTOCOL_VERSION,
            request_id: "ocr-request-1".to_owned(),
            detector_id: "pp_ocr_small".to_owned(),
            image_path: "/unused/example.png".to_owned(),
            expected_sha256: "a".repeat(64),
        }
    }

    fn ocr_response() -> OcrSidecarResponse {
        OcrSidecarResponse {
            protocol_version: SIDECAR_PROTOCOL_VERSION,
            request_id: "ocr-request-1".to_owned(),
            source_sha256: "a".repeat(64),
            width: 100,
            height: 50,
            lines: vec![OcrSidecarLine {
                text: "邮箱case@example.com".to_owned(),
                score: 0.99,
                bbox: [5, 5, 90, 25],
                polygon: vec![[5, 5], [90, 5], [90, 25], [5, 25]],
            }],
            warnings: Vec::new(),
        }
    }

    #[test]
    fn accepts_exact_unicode_spans_and_applies_threshold() {
        let response = SidecarResponse {
            protocol_version: SIDECAR_PROTOCOL_VERSION,
            request_id: "request-1".to_owned(),
            findings: vec![
                SidecarFinding {
                    start: 3,
                    end: 11,
                    entity_type: EntityType::OrgName,
                    matched_text: "星海科技有限公司".to_owned(),
                    confidence: 0.95,
                    reason_code: "MODEL_ORGANIZATION".to_owned(),
                },
                SidecarFinding {
                    start: 3,
                    end: 5,
                    entity_type: EntityType::OrgName,
                    matched_text: "星海".to_owned(),
                    confidence: 0.40,
                    reason_code: "LOW_CONFIDENCE".to_owned(),
                },
            ],
            warnings: Vec::new(),
        };
        let findings = response_to_findings(&runtime(), &request(), response, 0.65).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].matched_text, "星海科技有限公司");
    }

    #[test]
    fn rejects_a_model_value_that_does_not_match_the_source() {
        let response = SidecarResponse {
            protocol_version: SIDECAR_PROTOCOL_VERSION,
            request_id: "request-1".to_owned(),
            findings: vec![SidecarFinding {
                start: 3,
                end: 11,
                entity_type: EntityType::OrgName,
                matched_text: "模型编造的机构".to_owned(),
                confidence: 0.99,
                reason_code: "MODEL_ORGANIZATION".to_owned(),
            }],
            warnings: Vec::new(),
        };
        assert!(matches!(
            response_to_findings(&runtime(), &request(), response, 0.65),
            Err(SidecarError::TextMismatch(_))
        ));
    }

    #[test]
    fn verifies_pinned_runtime_assets_before_model_launch() {
        let directory = tempdir().unwrap();
        let asset_path = directory.path().join("weights.bin");
        fs::write(&asset_path, b"pinned model weights").unwrap();
        let mut runtime = runtime();
        runtime.assets.push(RuntimeAsset {
            path: asset_path.clone(),
            sha256: sha256_hex(b"pinned model weights"),
        });
        verify_assets(&runtime).unwrap();

        fs::write(asset_path, b"modified").unwrap();
        assert!(matches!(
            verify_assets(&runtime),
            Err(SidecarError::AssetHashMismatch(_))
        ));
    }

    #[test]
    fn resolves_and_rechecks_pinned_tool_assets() {
        let directory = tempdir().unwrap();
        let bin = directory.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let executable = bin.join("pdfinfo");
        let dependency = bin.join("libpoppler.fixture");
        fs::write(&executable, b"pinned pdfinfo executable").unwrap();
        fs::write(&dependency, b"pinned poppler dependency").unwrap();
        let registry_path = directory.path().join("default.json");
        let registry_json = serde_json::json!({
            "schema_version": 1,
            "detectors": [],
            "tools": [
                {
                    "kind": "pdfinfo",
                    "executable": "bin/pdfinfo",
                    "sha256": sha256_hex(b"pinned pdfinfo executable"),
                    "assets": [
                        {
                            "path": "bin/libpoppler.fixture",
                            "sha256": sha256_hex(b"pinned poppler dependency")
                        }
                    ]
                }
            ]
        });
        fs::write(
            &registry_path,
            serde_json::to_vec_pretty(&registry_json).unwrap(),
        )
        .unwrap();

        let registry = RuntimeRegistry::from_path(&registry_path).unwrap();
        assert_eq!(
            registry.verified_tool(RuntimeToolKind::PdfInfo).unwrap(),
            Some(executable.as_path())
        );

        fs::write(dependency, b"modified").unwrap();
        assert!(matches!(
            registry.verified_tool(RuntimeToolKind::PdfInfo),
            Err(SidecarError::ToolAssetHashMismatch(_))
        ));
    }

    #[test]
    fn rejects_duplicate_tool_kinds_and_defaults_older_registries() {
        let legacy: RuntimeRegistry =
            serde_json::from_str(r#"{"schema_version":1,"detectors":[]}"#).unwrap();
        assert!(legacy.tools.is_empty());

        let tool = RuntimeTool {
            kind: RuntimeToolKind::PdfToPpm,
            executable: PathBuf::from("/unused/pdftoppm"),
            sha256: "a".repeat(64),
            assets: Vec::new(),
        };
        let registry = RuntimeRegistry {
            schema_version: 1,
            detectors: Vec::new(),
            tools: vec![tool.clone(), tool],
        };
        assert!(matches!(
            registry.validate(),
            Err(SidecarError::DuplicateTool(_))
        ));
    }

    #[test]
    fn validates_ocr_hash_dimensions_and_coordinates() {
        let result = validate_ocr_response(&ocr_runtime(), &ocr_request(), ocr_response()).unwrap();
        assert_eq!(result.width, 100);
        assert_eq!(result.lines[0].bbox.x1, 90);
    }

    #[test]
    fn rejects_ocr_output_outside_the_source_image() {
        let mut response = ocr_response();
        response.lines[0].bbox = [5, 5, 101, 25];
        assert!(matches!(
            validate_ocr_response(&ocr_runtime(), &ocr_request(), response),
            Err(SidecarError::InvalidOcrLine(_))
        ));
    }

    #[test]
    fn rejects_an_ocr_response_for_a_different_source_hash() {
        let mut response = ocr_response();
        response.source_sha256 = "b".repeat(64);
        assert!(matches!(
            validate_ocr_response(&ocr_runtime(), &ocr_request(), response),
            Err(SidecarError::SourceHashMismatch(_))
        ));
    }
}
