use std::fs;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::model::{DocumentGraph, DocumentPart, FileKind, Finding, LineEnding, SourceMetadata};
use crate::workflow::WorkflowError;

pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn detect_line_ending(text: &str) -> LineEnding {
    let crlf = text.matches("\r\n").count();
    let without_crlf = text.replace("\r\n", "");
    let lf = without_crlf.matches('\n').count();
    let cr = without_crlf.matches('\r').count();
    let kinds = usize::from(crlf > 0) + usize::from(lf > 0) + usize::from(cr > 0);
    match (kinds, crlf > 0, lf > 0, cr > 0) {
        (0, _, _, _) => LineEnding::None,
        (1, true, _, _) => LineEnding::CrLf,
        (1, _, true, _) => LineEnding::Lf,
        (1, _, _, true) => LineEnding::Cr,
        _ => LineEnding::Mixed,
    }
}

fn document_graph(
    text: String,
    bytes: &[u8],
    source_path: String,
    file_kind: FileKind,
    has_utf8_bom: bool,
) -> DocumentGraph {
    DocumentGraph {
        schema_version: 1,
        offset_unit: "unicode_scalar".to_owned(),
        source: SourceMetadata {
            path: source_path,
            sha256: sha256_hex(bytes),
            size_bytes: bytes.len() as u64,
            file_kind,
            encoding: "utf-8".to_owned(),
            has_utf8_bom,
            line_ending: detect_line_ending(&text),
        },
        parts: vec![DocumentPart {
            id: "part-0001".to_owned(),
            kind: "text".to_owned(),
            locator: "whole_file".to_owned(),
            char_len: text.chars().count(),
            text,
        }],
    }
}

pub fn document_from_text(text: String, source_label: &str, file_kind: FileKind) -> DocumentGraph {
    let bytes = text.as_bytes().to_vec();
    document_graph(text, &bytes, source_label.to_owned(), file_kind, false)
}

pub fn read_document(path: &Path) -> Result<DocumentGraph, WorkflowError> {
    let bytes = fs::read(path)?;
    let has_utf8_bom = bytes.starts_with(&[0xEF, 0xBB, 0xBF]);
    let content = if has_utf8_bom { &bytes[3..] } else { &bytes };
    let text = std::str::from_utf8(content)
        .map_err(|_| WorkflowError::UnsupportedEncoding(path.to_path_buf()))?
        .to_owned();
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let file_kind = match extension.as_str() {
        "txt" => FileKind::Text,
        "md" | "markdown" => FileKind::Markdown,
        _ => return Err(WorkflowError::UnsupportedFileType(extension)),
    };
    let canonical = fs::canonicalize(path)?;
    Ok(document_graph(
        text,
        &bytes,
        canonical.to_string_lossy().into_owned(),
        file_kind,
        has_utf8_bom,
    ))
}

pub fn char_to_byte(text: &str, char_index: usize) -> Option<usize> {
    if char_index == text.chars().count() {
        return Some(text.len());
    }
    text.char_indices().nth(char_index).map(|(index, _)| index)
}

pub fn apply_findings(text: &str, findings: &[Finding]) -> Result<String, WorkflowError> {
    let mut selected: Vec<&Finding> = findings.iter().filter(|item| item.selected).collect();
    selected.sort_by_key(|item| (std::cmp::Reverse(item.start), std::cmp::Reverse(item.end)));

    let mut last_start = usize::MAX;
    let mut output = text.to_owned();
    for finding in selected {
        if finding.end > last_start {
            return Err(WorkflowError::OverlappingFindings);
        }
        let start = char_to_byte(&output, finding.start)
            .ok_or(WorkflowError::InvalidSpan(finding.id.clone()))?;
        let end = char_to_byte(&output, finding.end)
            .ok_or(WorkflowError::InvalidSpan(finding.id.clone()))?;
        if output.get(start..end) != Some(finding.matched_text.as_str()) {
            return Err(WorkflowError::SpanTextMismatch(finding.id.clone()));
        }
        output.replace_range(start..end, &finding.replacement);
        last_start = finding.start;
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use crate::model::{EntityType, Finding};

    use super::apply_findings;

    #[test]
    fn replaces_unicode_scalar_span_without_splitting_utf8() {
        let text = "联系人🙂沈景行，电话13800000001";
        let finding = Finding {
            id: "f-1".to_owned(),
            part_id: "part-0001".to_owned(),
            start: 4,
            end: 7,
            entity_type: EntityType::PersonName,
            matched_text: "沈景行".to_owned(),
            detector: "test".to_owned(),
            confidence: 1.0,
            explanation_code: "TEST".to_owned(),
            selected: true,
            reviewed: true,
            replacement: "[姓名]".to_owned(),
        };
        assert_eq!(
            apply_findings(text, &[finding]).unwrap(),
            "联系人🙂[姓名]，电话13800000001"
        );
    }
}
