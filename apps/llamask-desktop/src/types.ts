export type DesktopFileKind =
  | "text"
  | "word"
  | "spreadsheet"
  | "presentation"
  | "pdf"
  | "image"
  | "unknown";

export interface ImportedFile {
  id: string;
  displayName: string;
  extension: string;
  kind: DesktopFileKind;
  sizeBytes: number;
  ready: boolean;
  scanSupported: boolean;
  reasonCode: string;
  duplicate: boolean;
}

export interface DesktopCapabilities {
  appVersion: string;
  coreVersion: string;
  defaultPolicyId: string;
  offlineOnly: boolean;
  maxImportFiles: number;
  supportedExtensions: string[];
  scanExtensions: string[];
  milestone: string;
}

export interface DesktopCommandError {
  code: string;
  message: string;
}

export interface DesktopRuntimeStatus {
  policyReady: boolean;
  runtimeRegistryReady: boolean;
  ocrReady: boolean;
  ocrRuntimeId: string | null;
  pdfToolsReady: boolean;
  scanReady: boolean;
  statusCode: string;
}

export type DesktopScanStatus =
  | "queued"
  | "scanning"
  | "cancelling"
  | "review_required"
  | "ready_to_export"
  | "blocked"
  | "cancelled";

export type DesktopScanStage =
  | "queued"
  | "preflight"
  | "ocr"
  | "scanning_pages"
  | "complete"
  | "cancelling"
  | "blocked"
  | "cancelled";

export interface DesktopScanSummary {
  id: string;
  status: DesktopScanStatus;
  stage: DesktopScanStage;
  completedUnits: number;
  totalUnits: number;
  findingGroups: number;
  unreviewedGroups: number;
  pageCount: number;
  diagnosticCount: number;
  canCancel: boolean;
  errorCode: string | null;
}
