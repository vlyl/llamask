use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use llamask_core::PolicyConfig;
use serde::Serialize;
use tauri::{DragDropEvent, Emitter, Manager, State, WindowEvent};
use tauri_plugin_dialog::DialogExt;

const MAX_IMPORT_FILES: usize = 200;
const MAX_TEXT_BYTES: u64 = 2 * 1024 * 1024;
const MAX_IMAGE_BYTES: u64 = 50 * 1024 * 1024;
const MAX_DOCUMENT_BYTES: u64 = 100 * 1024 * 1024;
const SUPPORTED_EXTENSIONS: &[&str] = &[
    "txt", "md", "docx", "xlsx", "pptx", "pdf", "png", "jpg", "jpeg",
];
const FILES_IMPORTED_EVENT: &str = "desktop-files-imported";

#[derive(Default)]
struct DesktopState {
    next_id: AtomicU64,
    files: Mutex<BTreeMap<String, PathBuf>>,
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

impl DesktopCommandError {
    fn new(code: &'static str, message: &'static str) -> Self {
        Self { code, message }
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
        milestone: "desktop-shell-file-import",
    }
}

#[tauri::command]
async fn pick_files(
    app: tauri::AppHandle,
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
        if let Some((existing_id, _)) = registry.iter().find(|(_, path)| *path == &canonical) {
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
        registry.insert(id, canonical);
        imported.push(candidate);
    }
    Ok(imported)
}

#[tauri::command]
fn remove_registered_file(
    state: State<'_, DesktopState>,
    id: String,
) -> Result<bool, DesktopCommandError> {
    let mut registry = state.files.lock().map_err(|_| {
        DesktopCommandError::new("TASK_STATE_UNAVAILABLE", "本地任务状态暂时不可用。")
    })?;
    Ok(registry.remove(&id).is_some())
}

#[tauri::command]
fn clear_registered_files(state: State<'_, DesktopState>) -> Result<usize, DesktopCommandError> {
    let mut registry = state.files.lock().map_err(|_| {
        DesktopCommandError::new("TASK_STATE_UNAVAILABLE", "本地任务状态暂时不可用。")
    })?;
    let count = registry.len();
    registry.clear();
    Ok(count)
}

fn inspect_file(id: String, display_name: String, path: &Path, size_bytes: u64) -> ImportedFile {
    let extension = normalized_extension(path);
    let (kind, limit) = extension_profile(&extension);
    let (ready, reason_code) = if kind == DesktopFileKind::Unknown {
        (false, "UNSUPPORTED_EXTENSION")
    } else if size_bytes > limit {
        (false, "FILE_TOO_LARGE")
    } else {
        (true, "READY")
    };
    ImportedFile {
        id,
        display_name,
        extension,
        kind,
        size_bytes,
        ready,
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
            pick_files,
            remove_registered_file,
            clear_registered_files
        ])
        .run(tauri::generate_context!())
        .expect("LlaMask desktop runtime failed");
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use tempfile::tempdir;

    use super::{DesktopFileKind, DesktopState, inspect_file, register_files};

    #[test]
    fn classifies_supported_pdf_without_returning_its_path() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("synthetic.pdf");
        fs::write(&path, b"%PDF-1.4\n").unwrap();
        let candidate = inspect_file("file-1".to_owned(), "synthetic.pdf".to_owned(), &path, 9);
        assert_eq!(candidate.kind, DesktopFileKind::Pdf);
        assert!(candidate.ready);
        assert_eq!(candidate.reason_code, "READY");
    }

    #[test]
    fn rejects_unknown_extensions_before_scanning() {
        let candidate = inspect_file(
            "file-2".to_owned(),
            "archive.bin".to_owned(),
            Path::new("archive.bin"),
            12,
        );
        assert_eq!(candidate.kind, DesktopFileKind::Unknown);
        assert!(!candidate.ready);
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
}
