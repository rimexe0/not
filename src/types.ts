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
  launchAtLogin: boolean;
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
}
