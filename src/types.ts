export interface Note {
  id: string;
  body: string;
  position: number;
  createdAt: number;
  updatedAt: number;
  cursorStart: number;
  cursorEnd: number;
  scrollTop: number;
  persisted: boolean;
  ordinal: number;
  total: number;
}

export interface InitialState {
  note: Note;
  shortcut: string;
  shortcutLabel: string | null;
  launchAtLogin: boolean;
  fontSize: number;
  theme: "auto" | "dark" | "light";
  glassSettings: GlassSettings;
}

export interface GlassSettings {
  enabled: boolean;
  darkTint: string;
  lightTint: string;
  opacity: number;
}

export interface NoteInput {
  id: string;
  body: string;
  position: number;
  createdAt: number;
  cursorStart: number;
  cursorEnd: number;
  scrollTop: number;
  persisted: boolean;
}

export interface SaveResult {
  persisted: boolean;
  updatedAt: number;
}

export interface PageSummary {
  id: string;
  snippet: string;
  createdAt: number;
  position: number;
}

export interface DeletedPage extends PageSummary {
  deletedAt: number;
}

export interface Neighbors {
  older: Note | null;
  newer: Note | null;
}

export interface DeleteResult {
  note: Note;
  deletedId: string | null;
}

export interface PanelResult {
  external: boolean;
  side: "left" | "right" | "overlay";
}

export interface SummonMetrics {
  count: number;
  p50Micros: number;
  p95Micros: number;
  p99Micros: number;
  visibleCount: number;
  visibleP50Micros: number;
  visibleP95Micros: number;
  visibleP99Micros: number;
  firstInputCount: number;
}

export interface Attachment {
  id: string;
  noteId: string;
  mimeType: string;
  width: number;
  height: number;
  byteSize: number;
  contentHash: string;
  thumbnailUrl: string;
}

export interface ClipboardSettings {
  captureText: boolean;
  captureImages: boolean;
  ignoreDuplicates: boolean;
  ignoreWhitespace: boolean;
  ignoreSensitive: boolean;
  minimumTextLength: number;
  maximumTextLength: number;
  ignoredApplications: string;
}

export interface AiSettings {
  customProgram: string;
  customArguments: string;
  lastProvider: ProviderStatus["id"] | null;
}

export interface ProviderStatus {
  id: "claude" | "codex" | "custom";
  displayName: string;
  available: boolean;
  executable: string | null;
}

export interface ClipboardChange {
  token: string;
  kind: "text" | "image";
}

export interface AiResult {
  markdown: string;
  provider: ProviderStatus["id"];
  action: "summarize" | "organize";
  durationMs: number;
}
