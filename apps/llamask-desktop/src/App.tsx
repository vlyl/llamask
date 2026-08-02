import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useCallback, useEffect, useMemo, useState } from "react";

import {
  ChevronRightIcon,
  CloseIcon,
  FileIcon,
  FolderIcon,
  LockIcon,
  PlusIcon,
  SettingsIcon,
  ShieldIcon,
  TrashIcon,
} from "./icons";
import type {
  DesktopCapabilities,
  DesktopCommandError,
  DesktopFileKind,
  ImportedFile,
} from "./types";

const browserCapabilities: DesktopCapabilities = {
  appVersion: "0.1.0",
  coreVersion: "0.1.0",
  defaultPolicyId: "default-high-recall",
  offlineOnly: true,
  maxImportFiles: 200,
  supportedExtensions: [
    "txt",
    "md",
    "docx",
    "xlsx",
    "pptx",
    "pdf",
    "png",
    "jpg",
    "jpeg",
  ],
  milestone: "browser-preview",
};

const kindLabels: Record<DesktopFileKind, string> = {
  text: "文本",
  word: "Word",
  spreadsheet: "Excel",
  presentation: "PowerPoint",
  pdf: "PDF",
  image: "图片",
  unknown: "不支持",
};

const reasonLabels: Record<string, string> = {
  READY: "待扫描",
  ALREADY_IMPORTED: "已在任务中",
  UNSUPPORTED_EXTENSION: "暂不支持该格式",
  FILE_TOO_LARGE: "超过安全大小限制",
  FILE_UNREADABLE: "文件无法读取",
};

interface DesktopImportEvent {
  files: ImportedFile[];
  error: DesktopCommandError | null;
}

function isTauriRuntime() {
  return typeof window !== "undefined" && Boolean(window.__TAURI_INTERNALS__);
}

function formatBytes(value: number) {
  if (value < 1024) return `${value} B`;
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KB`;
  return `${(value / 1024 / 1024).toFixed(1)} MB`;
}

function normalizeError(error: unknown): DesktopCommandError {
  if (typeof error === "object" && error !== null) {
    const candidate = error as Partial<DesktopCommandError>;
    if (typeof candidate.message === "string") {
      return {
        code: candidate.code ?? "DESKTOP_COMMAND_FAILED",
        message: candidate.message,
      };
    }
  }
  return {
    code: "DESKTOP_COMMAND_FAILED",
    message: "本地操作未完成，请重试。",
  };
}

function App() {
  const [capabilities, setCapabilities] = useState(browserCapabilities);
  const [files, setFiles] = useState<ImportedFile[]>([]);
  const [busy, setBusy] = useState(false);
  const [dragging, setDragging] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const tauriRuntime = isTauriRuntime();

  useEffect(() => {
    if (!tauriRuntime) return;
    invoke<DesktopCapabilities>("desktop_capabilities")
      .then(setCapabilities)
      .catch((error: unknown) => setNotice(normalizeError(error).message));
  }, [tauriRuntime]);

  const mergeImportedFiles = useCallback((imported: ImportedFile[]) => {
    const duplicateCount = imported.filter((file) => file.duplicate).length;
    setFiles((current) => {
      const byId = new Map(current.map((file) => [file.id, file]));
      for (const file of imported) {
        if (!file.duplicate) byId.set(file.id, file);
      }
      return [...byId.values()];
    });
    if (duplicateCount > 0) {
      setNotice(`${duplicateCount} 个文件已经在当前任务中。`);
    }
  }, []);

  useEffect(() => {
    if (!tauriRuntime) return;
    let active = true;
    const unlisten: Array<() => void> = [];
    listen<DesktopImportEvent>("desktop-files-imported", (event) => {
      if (event.payload.error) {
        setNotice(event.payload.error.message);
        return;
      }
      mergeImportedFiles(event.payload.files);
    })
      .then((dispose) => {
        if (active) unlisten.push(dispose);
        else dispose();
      })
      .catch(() => setNotice("拖放导入未启用，仍可通过文件选择器导入。"));
    getCurrentWindow()
      .onDragDropEvent((event) => {
        if (event.payload.type === "over") setDragging(true);
        if (event.payload.type === "leave") setDragging(false);
        if (event.payload.type === "drop") {
          setDragging(false);
        }
      })
      .then((dispose) => {
        if (active) unlisten.push(dispose);
        else dispose();
      })
      .catch(() => setNotice("拖放监听未启用，仍可通过文件选择器导入。"));
    return () => {
      active = false;
      unlisten.forEach((dispose) => dispose());
    };
  }, [mergeImportedFiles, tauriRuntime]);

  const chooseFiles = async () => {
    if (!tauriRuntime) {
      setNotice("浏览器预览不读取本地路径；请在 Tauri 窗口中运行。");
      return;
    }
    setBusy(true);
    setNotice(null);
    try {
      const imported = await invoke<ImportedFile[]>("pick_files");
      mergeImportedFiles(imported);
    } catch (error) {
      setNotice(normalizeError(error).message);
    } finally {
      setBusy(false);
    }
  };

  const removeFile = async (id: string) => {
    if (tauriRuntime) {
      try {
        await invoke<boolean>("remove_registered_file", { id });
      } catch (error) {
        setNotice(normalizeError(error).message);
        return;
      }
    }
    setFiles((current) => current.filter((file) => file.id !== id));
  };

  const clearFiles = async () => {
    if (tauriRuntime) {
      try {
        await invoke<number>("clear_registered_files");
      } catch (error) {
        setNotice(normalizeError(error).message);
        return;
      }
    }
    setFiles([]);
  };

  const summary = useMemo(() => {
    const ready = files.filter((file) => file.ready).length;
    const blocked = files.length - ready;
    const bytes = files.reduce((total, file) => total + file.sizeBytes, 0);
    return { ready, blocked, bytes };
  }, [files]);

  return (
    <div className="app-shell">
      <aside className="sidebar">
        <div className="brand">
          <span className="brand-mark">
            <ShieldIcon width={22} height={22} />
          </span>
          <span>
            <strong>LlaMask</strong>
            <small>本地数据脱敏</small>
          </span>
        </div>

        <nav className="main-nav" aria-label="主要功能">
          <button className="nav-item active" type="button">
            <FolderIcon />
            <span>脱敏任务</span>
            {files.length > 0 && <em>{files.length}</em>}
          </button>
          <button className="nav-item" type="button" disabled>
            <FileIcon />
            <span>策略管理</span>
            <small>即将接入</small>
          </button>
          <button className="nav-item" type="button" disabled>
            <SettingsIcon />
            <span>运行环境</span>
          </button>
        </nav>

        <div className="workflow-card">
          <p className="eyebrow">当前流程</p>
          <ol className="workflow-steps">
            <li className={files.length > 0 ? "done" : "active"}>
              <span>1</span>
              <div>
                <strong>导入文件</strong>
                <small>{files.length > 0 ? "已建立本地引用" : "选择或拖入文件"}</small>
              </div>
            </li>
            <li>
              <span>2</span>
              <div>
                <strong>扫描识别</strong>
                <small>下一里程碑</small>
              </div>
            </li>
            <li>
              <span>3</span>
              <div>
                <strong>复核调整</strong>
                <small>等待扫描结果</small>
              </div>
            </li>
            <li>
              <span>4</span>
              <div>
                <strong>安全导出</strong>
                <small>独立残留复检</small>
              </div>
            </li>
          </ol>
        </div>

        <div className="offline-card">
          <LockIcon />
          <div>
            <strong>完全离线</strong>
            <span>文件不会离开此设备</span>
          </div>
          <i aria-label="离线模式已启用" />
        </div>
      </aside>

      <main className="workspace">
        <header className="workspace-header">
          <div>
            <p className="eyebrow">桌面端 MVP · 文件导入</p>
            <h1>新建脱敏任务</h1>
            <p>导入文件后，LlaMask 将在本机完成识别、复核与安全导出。</p>
          </div>
          <div className="header-actions">
            <span className="runtime-chip">
              <span /> Core {capabilities.coreVersion}
            </span>
            <button
              className="primary-button compact"
              type="button"
              onClick={() => void chooseFiles()}
              disabled={busy}
            >
              <PlusIcon />
              添加文件
            </button>
          </div>
        </header>

        {notice && (
          <div className="notice" role="status">
            <span>{notice}</span>
            <button type="button" onClick={() => setNotice(null)} aria-label="关闭提示">
              <CloseIcon />
            </button>
          </div>
        )}

        <section
          className={`drop-zone ${dragging ? "dragging" : ""} ${files.length > 0 ? "compact" : ""}`}
          aria-label="文件导入区"
        >
          <div className="drop-icon">
            <FolderIcon width={30} height={30} />
          </div>
          <div>
            <h2>{dragging ? "松开即可加入任务" : "拖入需要脱敏的文件"}</h2>
            <p>
              支持 DOCX、XLSX、PPTX、PDF、图片、TXT 和 Markdown，单次最多 {capabilities.maxImportFiles} 个
            </p>
          </div>
          <button className="secondary-button" type="button" onClick={() => void chooseFiles()} disabled={busy}>
            {busy ? "正在读取…" : "选择文件"}
          </button>
        </section>

        {files.length > 0 ? (
          <section className="task-panel">
            <div className="panel-header">
              <div>
                <h2>任务文件</h2>
                <span>{summary.ready} 个可扫描 · {summary.blocked} 个需要处理</span>
              </div>
              <button className="text-button danger" type="button" onClick={() => void clearFiles()}>
                <TrashIcon /> 清空
              </button>
            </div>

            <div className="file-list">
              {files.map((file) => (
                <article className={`file-row ${file.ready ? "" : "blocked"}`} key={file.id}>
                  <div className={`file-badge kind-${file.kind}`}>
                    {file.extension ? file.extension.slice(0, 4).toUpperCase() : "?"}
                  </div>
                  <div className="file-copy">
                    <strong title={file.displayName}>{file.displayName}</strong>
                    <span>{kindLabels[file.kind]} · {formatBytes(file.sizeBytes)}</span>
                  </div>
                  <span className={`status-pill ${file.ready ? "ready" : "blocked"}`}>
                    {reasonLabels[file.reasonCode] ?? "需要检查"}
                  </span>
                  <button
                    className="icon-button"
                    type="button"
                    onClick={() => void removeFile(file.id)}
                    aria-label={`移除 ${file.displayName}`}
                  >
                    <CloseIcon />
                  </button>
                </article>
              ))}
            </div>
          </section>
        ) : (
          <section className="privacy-explainer">
            <div>
              <ShieldIcon />
              <h3>原件安全</h3>
              <p>仅生成新副本，不覆盖源文件或已有输出。</p>
            </div>
            <div>
              <LockIcon />
              <h3>本地识别</h3>
              <p>规则、OCR 和本地模型都在设备上运行。</p>
            </div>
            <div>
              <FileIcon />
              <h3>独立复检</h3>
              <p>副本通过残留验证后才会保存到目标位置。</p>
            </div>
          </section>
        )}

        <footer className="action-bar">
          <div className="task-summary">
            <span>{files.length} 个文件</span>
            <span>{formatBytes(summary.bytes)}</span>
            <span>策略：{capabilities.defaultPolicyId}</span>
          </div>
          <div className="next-action">
            <span>当前提交先完成安全导入边界</span>
            <button className="primary-button" type="button" disabled>
              开始扫描
              <ChevronRightIcon />
            </button>
          </div>
        </footer>
      </main>
    </div>
  );
}

export default App;
