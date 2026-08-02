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
  milestone: string;
}

export interface DesktopCommandError {
  code: string;
  message: string;
}
