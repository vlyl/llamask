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
  DesktopRuntimeStatus,
  DesktopScanSummary,
  ImportedFile,
} from "./types";

const browserCapabilities: DesktopCapabilities = {
  appVersion: "0.1.0-alpha.1",
  coreVersion: "0.1.0-alpha.1",
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
  scanExtensions: ["pdf", "png", "jpg", "jpeg"],
  milestone: "browser-preview",
};

const browserRuntimeStatus: DesktopRuntimeStatus = {
  policyReady: true,
  runtimeRegistryReady: false,
  ocrReady: false,
  ocrRuntimeId: null,
  pdfToolsReady: false,
  scanReady: false,
  statusCode: "BROWSER_PREVIEW",
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
  SCAN_NOT_AVAILABLE: "后续增量接入",
  UNSUPPORTED_EXTENSION: "暂不支持该格式",
  FILE_TOO_LARGE: "超过安全大小限制",
  FILE_UNREADABLE: "文件无法读取",
};

const scanStatusLabels: Record<DesktopScanSummary["status"], string> = {
  queued: "等待扫描",
  scanning: "正在扫描",
  cancelling: "正在取消",
  review_required: "需要复核",
  ready_to_export: "可安全导出",
  blocked: "扫描已阻断",
  cancelled: "已取消",
};

const scanErrorLabels: Record<string, string> = {
  RUNTIME_REGISTRY_MISSING: "未找到本地 OCR 运行配置",
  RUNTIME_REGISTRY_INVALID: "本地 OCR 运行配置无效",
  RUNTIME_ASSET_INVALID: "OCR 模型或运行文件完整性校验失败",
  OCR_RUNTIME_MISSING: "本地 OCR 未安装",
  PDF_TOOLS_MISSING: "PDF 本地工具未安装",
  PDF_LIMIT_EXCEEDED: "PDF 超过安全处理限制",
  INVALID_PDF: "PDF 结构无效",
  ENCRYPTED_PDF_UNSUPPORTED: "加密 PDF 需要先生成可信解密副本",
  PDF_TOOL_FAILED: "PDF 本地工具执行失败",
  PDF_READ_FAILED: "PDF 无法安全读取",
  IMAGE_LIMIT_EXCEEDED: "图片超过安全处理限制",
  UNSUPPORTED_IMAGE: "图片格式不受支持",
  OCR_FAILED: "本地 OCR 执行失败",
  IMAGE_READ_FAILED: "图片无法安全读取",
  POLICY_INVALID: "默认脱敏策略无效",
  SCAN_WORKER_FAILED: "本地扫描任务异常结束",
  IMAGE_SCAN_FAILED: "图片扫描未完成",
  PDF_SCAN_FAILED: "PDF 扫描未完成",
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

function scanDetail(scan: DesktopScanSummary) {
  if (scan.errorCode) {
    return scanErrorLabels[scan.errorCode] ?? "扫描被安全阻断，请检查运行环境。";
  }
  if (scan.status === "scanning" && scan.totalUnits > 0) {
    return `${scan.completedUnits}/${scan.totalUnits} ${scan.totalUnits > 1 ? "页" : "步"}`;
  }
  if (scan.status === "review_required") {
    return `${scan.findingGroups} 个结果 · ${scan.unreviewedGroups} 个待复核`;
  }
  if (scan.status === "ready_to_export") {
    return `${scan.findingGroups} 个结果已完成自动确认`;
  }
  return scanStatusLabels[scan.status];
}

function scanPillClass(scan: DesktopScanSummary | undefined, file: ImportedFile) {
  if (!scan) return file.ready && file.scanSupported ? "ready" : "blocked";
  if (["blocked", "cancelled"].includes(scan.status)) return "blocked";
  if (["review_required", "ready_to_export"].includes(scan.status)) return "complete";
  return "scanning";
}

function App() {
  const [capabilities, setCapabilities] = useState(browserCapabilities);
  const [runtimeStatus, setRuntimeStatus] = useState(browserRuntimeStatus);
  const [files, setFiles] = useState<ImportedFile[]>([]);
  const [scans, setScans] = useState<Map<string, DesktopScanSummary>>(new Map());
  const [busy, setBusy] = useState(false);
  const [scanStarting, setScanStarting] = useState(false);
  const [dragging, setDragging] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const tauriRuntime = isTauriRuntime();

  useEffect(() => {
    if (!tauriRuntime) return;
    invoke<DesktopCapabilities>("desktop_capabilities")
      .then(setCapabilities)
      .catch((error: unknown) => setNotice(normalizeError(error).message));
    invoke<DesktopRuntimeStatus>("desktop_runtime_status")
      .then(setRuntimeStatus)
      .catch(() =>
        setRuntimeStatus({
          ...browserRuntimeStatus,
          statusCode: "RUNTIME_STATUS_FAILED",
        }),
      );
    invoke<DesktopScanSummary[]>("scan_task_summaries")
      .then((summaries) =>
        setScans(new Map(summaries.map((summary) => [summary.id, summary]))),
      )
      .catch(() => undefined);
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
    listen<DesktopScanSummary>("desktop-scan-progress", (event) => {
      setScans((current) => {
        const next = new Map(current);
        next.set(event.payload.id, event.payload);
        return next;
      });
    })
      .then((dispose) => {
        if (active) unlisten.push(dispose);
        else dispose();
      })
      .catch(() => setNotice("扫描状态监听未启用，请重新启动应用。"));
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
    setScans((current) => {
      const next = new Map(current);
      next.delete(id);
      return next;
    });
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
    setScans(new Map());
  };

  const startScanning = async () => {
    if (!tauriRuntime) {
      setNotice("浏览器预览不会读取或扫描本地文件；请在 Tauri 窗口中运行。");
      return;
    }
    setScanStarting(true);
    setNotice(null);
    try {
      const summaries = await invoke<DesktopScanSummary[]>("start_registered_scans");
      setScans((current) => {
        const next = new Map(current);
        for (const summary of summaries) next.set(summary.id, summary);
        return next;
      });
    } catch (error) {
      setNotice(normalizeError(error).message);
    } finally {
      setScanStarting(false);
    }
  };

  const cancelScanning = async (id: string) => {
    if (!tauriRuntime) return;
    try {
      await invoke<boolean>("cancel_scan", { id });
    } catch (error) {
      setNotice(normalizeError(error).message);
    }
  };

  const summary = useMemo(() => {
    const ready = files.filter((file) => file.ready).length;
    const blocked = files.length - ready;
    const scannable = files.filter((file) => file.ready && file.scanSupported).length;
    const active = [...scans.values()].filter((scan) => scan.canCancel).length;
    const completed = [...scans.values()].filter((scan) =>
      ["review_required", "ready_to_export"].includes(scan.status),
    ).length;
    const bytes = files.reduce((total, file) => total + file.sizeBytes, 0);
    return { ready, blocked, scannable, active, completed, bytes };
  }, [files, scans]);

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
            <li className={summary.active > 0 ? "active" : summary.completed > 0 ? "done" : ""}>
              <span>2</span>
              <div>
                <strong>扫描识别</strong>
                <small>
                  {summary.active > 0
                    ? `${summary.active} 个文件正在本机处理`
                    : summary.completed > 0
                      ? `${summary.completed} 个文件扫描完成`
                      : "PDF 与图片已接入"}
                </small>
              </div>
            </li>
            <li className={summary.completed > 0 ? "active" : ""}>
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
            <p className="eyebrow">桌面端 MVP · 本地扫描</p>
            <h1>新建脱敏任务</h1>
            <p>导入文件后，LlaMask 将在本机完成识别、复核与安全导出。</p>
          </div>
          <div className="header-actions">
            <span className={`runtime-chip ${runtimeStatus.scanReady ? "" : "warning"}`}>
              <span /> Core {capabilities.coreVersion}
              {runtimeStatus.scanReady ? ` · OCR ${runtimeStatus.ocrRuntimeId}` : " · OCR 未就绪"}
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
              可导入 DOCX、XLSX、PPTX、PDF、图片、TXT 和 Markdown；当前扫描增量支持{" "}
              {capabilities.scanExtensions.map((extension) => extension.toUpperCase()).join("、")}，单次最多{" "}
              {capabilities.maxImportFiles} 个
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
                <span>
                  {summary.scannable} 个可立即扫描 · {summary.active} 个处理中 · {summary.blocked} 个导入受阻
                </span>
              </div>
              <button className="text-button danger" type="button" onClick={() => void clearFiles()}>
                <TrashIcon /> 清空
              </button>
            </div>

            <div className="file-list">
              {files.map((file) => {
                const scan = scans.get(file.id);
                return (
                  <article
                    className={`file-row ${file.ready ? "" : "blocked"} ${scan?.canCancel ? "active" : ""}`}
                    key={file.id}
                  >
                    <div className={`file-badge kind-${file.kind}`}>
                      {file.extension ? file.extension.slice(0, 4).toUpperCase() : "?"}
                    </div>
                    <div className="file-copy">
                      <strong title={file.displayName}>{file.displayName}</strong>
                      <span>
                        {kindLabels[file.kind]} · {formatBytes(file.sizeBytes)}
                        {scan ? ` · ${scanDetail(scan)}` : ""}
                      </span>
                    </div>
                    <span className={`status-pill ${scanPillClass(scan, file)}`}>
                      {scan
                        ? scanStatusLabels[scan.status]
                        : reasonLabels[file.reasonCode] ?? "需要检查"}
                    </span>
                    <button
                      className="icon-button"
                      type="button"
                      onClick={() =>
                        scan?.canCancel
                          ? void cancelScanning(file.id)
                          : void removeFile(file.id)
                      }
                      aria-label={`${scan?.canCancel ? "取消扫描" : "移除"} ${file.displayName}`}
                    >
                      <CloseIcon />
                    </button>
                  </article>
                );
              })}
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
            <span>
              {!runtimeStatus.scanReady
                ? "请先完成本地 OCR 资源安装与完整性校验"
                : summary.scannable === 0
                  ? "请导入 PDF、PNG 或 JPEG"
                  : "扫描任务和敏感结果只保存在 Rust 进程内"}
            </span>
            <button
              className="primary-button"
              type="button"
              onClick={() => void startScanning()}
              disabled={
                scanStarting ||
                summary.active > 0 ||
                summary.scannable === 0 ||
                !runtimeStatus.scanReady
              }
            >
              {scanStarting ? "正在启动…" : summary.active > 0 ? "扫描进行中" : "开始扫描"}
              <ChevronRightIcon />
            </button>
          </div>
        </footer>
      </main>
    </div>
  );
}

export default App;
