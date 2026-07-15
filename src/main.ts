import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { SwipeTracker, WheelStepTracker } from "./lib/swipe";
import { formatShortcut, recordShortcut } from "./lib/shortcut";
import { commandMatches } from "./lib/commands";
import { ScratchpadEditor } from "./editor";
import type {
  AiResult,
  AiSettings,
  Attachment,
  ClipboardChange,
  ClipboardSettings,
  DeleteResult,
  DeletedPage,
  GlassSettings,
  InitialState,
  Neighbors,
  Note,
  NoteInput,
  PageSummary,
  PanelResult,
  ProviderStatus,
  SaveResult,
  SummonMetrics,
} from "./types";
import "./style.css";

const app = document.querySelector<HTMLElement>("#app");
if (!app) throw new Error("missing app root");

app.innerHTML = `
  <div class="layout">
    <section class="editor-shell">
      <div class="top-trigger" aria-hidden="true"></div>
      <div class="toolbar" aria-label="Scratchpad controls">
        <div class="toolbar-drag" aria-hidden="true"></div>
        <button type="button" data-action="pages">Pages</button>
        <button type="button" data-action="ai">AI</button>
        <button type="button" class="danger" data-action="delete">Delete</button>
        <button type="button" data-action="settings">Settings</button>
      </div>
      <div class="editor editor-host"></div>
      <pre class="page-preview" aria-hidden="true" hidden></pre>
      <div class="selection-menu" role="menu" hidden><button type="button" data-selection-ai="summarize">Summarize</button><button type="button" data-selection-ai="organize">Organize</button></div>
      <div class="indicator" aria-live="polite"></div>
      <div class="toast" aria-live="polite"><span>Page deleted</span><button type="button" data-action="undo">Undo</button></div>
      <div class="action-toast" aria-live="polite"></div>
      <div class="save-error" role="alert"><span>Couldn’t save this page.</span><button type="button" data-action="retry-save">Retry</button></div>
    </section>
    <div class="command-palette" role="dialog" aria-modal="true" aria-label="Commands" hidden>
      <div class="palette-card">
        <input class="palette-search" type="search" placeholder="Type a command" aria-label="Search commands">
        <div class="palette-list" role="listbox"></div>
      </div>
    </div>
  </div>
`;

const layout = required<HTMLElement>(".layout");
const editorHost = required<HTMLElement>(".editor");
const editorShell = required<HTMLElement>(".editor-shell");
const pagePreview = required<HTMLElement>(".page-preview");
const selectionMenu = required<HTMLElement>(".selection-menu");
const toolbar = required<HTMLElement>(".toolbar");
const topTrigger = required<HTMLElement>(".top-trigger");
const toolbarDrag = required<HTMLElement>(".toolbar-drag");
const indicator = required<HTMLElement>(".indicator");
const toast = required<HTMLElement>(".toast");
const saveError = required<HTMLElement>(".save-error");
const actionToast = required<HTMLElement>(".action-toast");
const commandPalette = required<HTMLElement>(".command-palette");
const paletteSearch = required<HTMLInputElement>(".palette-search");
const paletteList = required<HTMLElement>(".palette-list");
const currentWindow = getCurrentWindow();
const swipe = new SwipeTracker();
const shiftedWheel = new WheelStepTracker();
const editor = new ScratchpadEditor(editorHost, {
  input: handleEditorInput,
  selection: handleEditorSelection,
  scroll: handleEditorScroll,
  beforeInput: handleBeforeInput,
  imagePaste: handleImagePaste,
});

let note: Note;
let shortcut = "CommandOrControl+Shift+Space";
let shortcutDisplay = formatShortcut(shortcut);
let launchAtLogin = true;
let fontSize = 15;
let theme: InitialState["theme"] = "auto";
let glassSettings: GlassSettings = {
  enabled: false,
  darkTint: "#161619",
  lightTint: "#F5F5F7",
  opacity: 28,
};
let saveTimer: ReturnType<typeof setTimeout> | undefined;
let saveQueue: Promise<boolean> = Promise.resolve(true);
let indicatorTimer: ReturnType<typeof setTimeout> | undefined;
let toastTimer: ReturnType<typeof setTimeout> | undefined;
let panelMode: "pages" | "settings" | "ai" | null = null;
let deletedId: string | null = null;
let busy = false;
let pagesRequestId = 0;
let neighborPages: Neighbors | undefined;
let neighborRequestId = 0;
let swipeDirection: -1 | 0 | 1 = 0;
let swipeSettleTimer: ReturnType<typeof setTimeout> | undefined;
let queuedKeyboardNavigation = 0;
let interruptSwipeAnimation: (() => void) | null = null;
let skipActiveSwipeAnimation = false;
let activeSummonSequence = 0;
let quitRetryPending = false;
let toolbarHideTimer: ReturnType<typeof setTimeout> | undefined;
let actionToastTimer: ReturnType<typeof setTimeout> | undefined;
let attachmentRequestId = 0;
let paletteOpen = false;
let paletteSelection = 0;
let providers: ProviderStatus[] = [];
let aiSettings: AiSettings = { customProgram: "", customArguments: "", lastProvider: null };
let aiRequest: { action: "summarize" | "organize"; scope: "selection" | "note"; noteId: string; body: string; from: number; to: number; provider: ProviderStatus["id"] } | null = null;
let aiResult = "";
let aiResultEditor: ScratchpadEditor | null = null;
let clipboardActive = false;
let rememberedCaretLineEnd = 0;
let clipboardAppendQueue: Promise<void> = Promise.resolve();
let scratchpadVisible = false;

function required<T extends Element>(selector: string): T {
  const element = document.querySelector<T>(selector);
  if (!element) throw new Error(`missing ${selector}`);
  return element;
}

function applyFontSize(): void {
  document.documentElement.style.setProperty("--editor-font-size", `${fontSize}px`);
}

function applyTheme(): void {
  document.documentElement.dataset.theme = theme;
  document.documentElement.dataset.material = glassSettings.enabled ? "glass" : "solid";
  document.documentElement.style.setProperty(
    "--glass-dark-background",
    tintBackground(glassSettings.darkTint, glassSettings.opacity),
  );
  document.documentElement.style.setProperty(
    "--glass-light-background",
    tintBackground(glassSettings.lightTint, glassSettings.opacity),
  );
}

function tintBackground(hex: string, opacity: number): string {
  const component = (start: number): number => Number.parseInt(hex.slice(start, start + 2), 16);
  return `rgba(${component(1)}, ${component(3)}, ${component(5)}, ${opacity / 100})`;
}

function showToolbar(): void {
  clearTimeout(toolbarHideTimer);
  toolbarHideTimer = undefined;
  toolbar.classList.add("visible");
}

function scheduleToolbarHide(): void {
  clearTimeout(toolbarHideTimer);
  toolbarHideTimer = setTimeout(() => {
    if (!panelMode) toolbar.classList.remove("visible");
  }, 180);
}

function noteInput(): NoteInput {
  return {
    id: note.id,
    body: editor.value,
    position: note.position,
    createdAt: note.createdAt,
    cursorStart: editor.selectionStart,
    cursorEnd: editor.selectionEnd,
    scrollTop: editor.scrollTop,
    persisted: note.persisted,
  };
}

function mirrorDraft(): void {
  void invoke("update_draft", { input: noteInput() });
}

function mirrorDraftView(): void {
  void invoke("update_draft_view", {
    noteId: note.id,
    cursorStart: editor.selectionStart,
    cursorEnd: editor.selectionEnd,
    scrollTop: editor.scrollTop,
  });
}

function scheduleSave(): void {
  clearTimeout(saveTimer);
  saveTimer = setTimeout(() => void flushSave(), 250);
}

function flushSave(): Promise<boolean> {
  clearTimeout(saveTimer);
  saveTimer = undefined;
  const input = noteInput();
  saveQueue = saveQueue.then(async () => {
    try {
      const result = await invoke<SaveResult>("save_note", { input });
      saveError.classList.remove("visible");
      if (note.id === input.id) {
        const becamePersisted = !note.persisted && result.persisted;
        const changedEmptyState = note.body.trim() === "" !== (input.body.trim() === "");
        note.persisted = result.persisted;
        note.updatedAt = result.updatedAt;
        note.body = editor.value;
        note.cursorStart = editor.selectionStart;
        note.cursorEnd = editor.selectionEnd;
        note.scrollTop = editor.scrollTop;
        if (becamePersisted || changedEmptyState) scheduleNeighborPreload();
      }
      return true;
    } catch {
      showStorageError("Couldn’t save this page.");
      return false;
    }
  }, () => false);
  return saveQueue;
}

function showStorageError(message: string): void {
  const label = saveError.querySelector("span");
  if (label) label.textContent = message;
  saveError.classList.add("visible");
}

async function retryStorageOperation(): Promise<void> {
  if (quitRetryPending) {
    await quitSafely();
  } else {
    await flushSave();
  }
}

function displayNote(next: Note): void {
  note = next;
  selectionMenu.hidden = true;
  editor.setAttachments([]);
  editor.setValue(next.body, next.cursorStart, next.cursorEnd, next.scrollTop);
  mirrorDraft();
  requestAnimationFrame(() => {
    editor.focus({ preventScroll: true });
  });
  showIndicator();
  scheduleNeighborPreload();
  if (scratchpadVisible) void renderAttachments(next.id);
}

async function renderAttachments(noteId: string): Promise<void> {
  if (!scratchpadVisible) return;
  const requestId = ++attachmentRequestId;
  try {
    const attachments = await invoke<Attachment[]>("list_attachments", { noteId });
    if (!scratchpadVisible || requestId !== attachmentRequestId || note.id !== noteId) return;
    editor.setAttachments(attachments);
  } catch {
    if (requestId === attachmentRequestId) editor.setAttachments([]);
  }
}

function showActionToast(message: string): void {
  actionToast.textContent = message;
  actionToast.classList.add("visible");
  clearTimeout(actionToastTimer);
  actionToastTimer = setTimeout(() => actionToast.classList.remove("visible"), 2600);
}

function showIndicator(): void {
  indicator.textContent = `${note.ordinal} / ${note.total}`;
  indicator.classList.add("visible");
  clearTimeout(indicatorTimer);
  indicatorTimer = setTimeout(() => indicator.classList.remove("visible"), 850);
}

function scheduleNeighborPreload(): void {
  neighborPages = undefined;
  const noteId = note.id;
  const requestId = ++neighborRequestId;
  setTimeout(async () => {
    const result = await invoke<Neighbors>("neighbors", { noteId });
    if (requestId !== neighborRequestId || note.id !== noteId) return;
    neighborPages = result;
    if (swipeDirection !== 0 && !busy) renderSwipePreview();
  }, 0);
}

async function navigate(direction: -1 | 1, rapid = false, skipAnimation = false): Promise<void> {
  if (panelMode) return;
  if (busy) {
    if (rapid) {
      queuedKeyboardNavigation = Math.max(-50, Math.min(50, queuedKeyboardNavigation + direction));
      skipActiveSwipeAnimation = true;
      interruptSwipeAnimation?.();
    }
    return;
  }
  skipActiveSwipeAnimation = false;
  busy = true;
  try {
    if (swipeDirection === 0) {
      swipeDirection = direction;
      renderSwipePreview(0);
      await nextFrame();
    }
    const target = direction === 1 ? neighborPages?.newer : neighborPages?.older;
    const navigation = flushSave().then((saved) => {
      if (!saved) throw new Error("save failed");
      return invoke<Note>("navigate", { noteId: note.id, direction });
    });

    if (target) {
      pagePreview.textContent = target.body;
      await completeSwipe(navigation, direction, !skipAnimation);
      return;
    }

    const next = await navigation;
    if (next.id === note.id) {
      await cancelSwipe();
      return;
    }
    pagePreview.textContent = next.body;
    await completeSwipe(Promise.resolve(next), direction, !skipAnimation);
  } catch {
    cleanupSwipe();
    editor.focus({ preventScroll: true });
  } finally {
    busy = false;
    if (queuedKeyboardNavigation !== 0 && !panelMode) {
      const queuedDirection = Math.sign(queuedKeyboardNavigation) as -1 | 1;
      queuedKeyboardNavigation -= queuedDirection;
      void navigate(queuedDirection, true, true);
    }
  }
}

async function createNewNote(): Promise<void> {
  if (busy) return;
  if (panelMode) await closePanel();
  if (!note.persisted && editor.value.trim() === "") {
    editor.focus({ preventScroll: true });
    return;
  }
  busy = true;
  try {
    if (!await flushSave()) return;
    cleanupSwipe();
    displayNote(await invoke<Note>("new_note"));
  } finally {
    busy = false;
  }
}

function renderSwipePreview(distance?: number): void {
  const width = editorShell.clientWidth;
  const target = swipeDirection === 1 ? neighborPages?.newer : neighborPages?.older;
  const rawDistance = distance ?? Number(editor.dataset.swipeDistance ?? 0);
  const travel = Math.round(Math.min(rawDistance, width));
  editor.dataset.swipeDistance = String(travel);
  editor.style.transition = "none";
  pagePreview.style.transition = "none";

  if (target === null) {
    const resisted = Math.round(travel * 0.18);
    editor.style.transform = `translate3d(${swipeDirection === 1 ? -resisted : resisted}px, 0, 0)`;
    pagePreview.hidden = true;
    return;
  }

  pagePreview.textContent = target?.body ?? "";
  pagePreview.hidden = false;
  if (swipeDirection === 1) {
    editor.style.transform = `translate3d(${-travel}px, 0, 0)`;
    pagePreview.style.transform = `translate3d(${width - travel}px, 0, 0)`;
  } else {
    editor.style.transform = `translate3d(${travel}px, 0, 0)`;
    pagePreview.style.transform = `translate3d(${-width + travel}px, 0, 0)`;
  }
}

async function finishSwipe(): Promise<void> {
  clearTimeout(swipeSettleTimer);
  const threshold = Math.min(90, editorShell.clientWidth * 0.22);
  const direction = swipe.finish(threshold);
  if (direction !== 0) {
    await navigate(direction);
    return;
  }
  busy = true;
  try {
    await cancelSwipe();
  } finally {
    busy = false;
  }
}

async function completeSwipe(nextNote: Promise<Note>, direction: -1 | 1, animate: boolean): Promise<void> {
  if (!animate || skipActiveSwipeAnimation) {
    const next = await nextNote;
    cleanupSwipe();
    displayNote(next);
    return;
  }
  const travel = Number(editor.dataset.swipeDistance ?? 0);
  const velocity = swipe.completionVelocity(direction);
  swipe.suppressMomentum(direction);
  const width = editorShell.clientWidth;
  const remaining = Math.max(1, width - travel);
  const duration = Math.round(Math.min(240, Math.max(120, remaining / Math.max(velocity, 0.8))));
  const initialSlope = velocity * duration / remaining;
  const firstControlY = Math.min(1, initialSlope / 3);
  const timing = `${duration}ms cubic-bezier(0.3333, ${firstControlY}, 0.6667, 1)`;
  editor.style.transition = `transform ${timing}`;
  pagePreview.style.transition = `transform ${timing}`;
  await nextFrame();
  if (skipActiveSwipeAnimation) {
    const next = await nextNote;
    cleanupSwipe();
    displayNote(next);
    return;
  }
  const animation = waitForTransformTransition(pagePreview, duration + 40);
  const interrupted = new Promise<void>((resolve) => {
    interruptSwipeAnimation = resolve;
  });
  editor.style.transform = `translate3d(${direction === 1 ? -width : width}px, 0, 0)`;
  pagePreview.style.transform = "translate3d(0, 0, 0)";
  const [next] = await Promise.all([nextNote, Promise.race([animation, interrupted])]);
  interruptSwipeAnimation = null;
  swipe.suppressMomentum(direction);
  cleanupSwipe();
  displayNote(next);
}

async function cancelSwipe(): Promise<void> {
  const width = editorShell.clientWidth;
  const timing = "140ms cubic-bezier(0.22, 1, 0.36, 1)";
  editor.style.transition = `transform ${timing}`;
  pagePreview.style.transition = `transform ${timing}`;
  await nextFrame();
  editor.style.transform = "translate3d(0, 0, 0)";
  pagePreview.style.transform = `translate3d(${swipeDirection === 1 ? width : -width}px, 0, 0)`;
  await delay(140);
  cleanupSwipe();
  editor.focus({ preventScroll: true });
}

function cleanupSwipe(): void {
  clearTimeout(swipeSettleTimer);
  swipeDirection = 0;
  delete editor.dataset.swipeDistance;
  editor.style.transition = "none";
  pagePreview.style.transition = "none";
  editor.style.transform = "translate3d(0, 0, 0)";
  pagePreview.style.transform = "translate3d(0, 0, 0)";
  pagePreview.hidden = true;
  interruptSwipeAnimation = null;
}

function nextFrame(): Promise<void> {
  return new Promise((resolve) => requestAnimationFrame(() => resolve()));
}

function delay(milliseconds: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

function waitForTransformTransition(element: HTMLElement, fallbackMs: number): Promise<void> {
  return new Promise((resolve) => {
    const fallback = setTimeout(finish, fallbackMs);
    element.addEventListener("transitionend", onTransitionEnd);

    function onTransitionEnd(event: TransitionEvent): void {
      if (event.propertyName === "transform") finish();
    }

    function finish(): void {
      clearTimeout(fallback);
      element.removeEventListener("transitionend", onTransitionEnd);
      resolve();
    }
  });
}

async function deleteCurrent(): Promise<void> {
  if (busy) return;
  busy = true;
  try {
    if (!await flushSave()) return;
    const result = await invoke<DeleteResult>("delete_note", { noteId: note.id });
    deletedId = result.deletedId;
    displayNote(result.note);
    if (deletedId) {
      toast.classList.add("visible");
      clearTimeout(toastTimer);
      toastTimer = setTimeout(() => toast.classList.remove("visible"), 5000);
    }
    if (panelMode === "pages") await renderPages("");
  } finally {
    busy = false;
  }
}

async function undoDelete(): Promise<void> {
  if (!deletedId) return;
  const restored = await invoke<Note>("restore_note", { noteId: deletedId });
  deletedId = null;
  toast.classList.remove("visible");
  displayNote(restored);
}

async function openPanel(mode: "pages" | "settings" | "ai"): Promise<void> {
  if (panelMode === mode) return;
  if (panelMode) await closePanel();
  if (!await flushSave()) return;
  const panelWidthLogical = mode === "ai" ? 380 : 300;
  layout.style.setProperty("--panel-width", `${panelWidthLogical}px`);
  const geometry = await invoke<PanelResult>("set_panel", { open: true, panelWidthLogical });
  panelMode = mode;
  layout.classList.toggle("panel-left", geometry.side === "left");
  showToolbar();

  const panel = document.createElement("aside");
  panel.className = `panel panel-${geometry.side}`;
  const title = mode === "pages" ? "Pages" : mode === "settings" ? "Settings" : "AI";
  panel.innerHTML = `<div class="panel-header"><h2>${title}</h2><button class="panel-close" type="button" aria-label="Close">×</button></div><div class="panel-content"></div>`;
  layout.append(panel);
  panel.addEventListener("pointerdown", (event) => {
    const target = event.target as HTMLElement;
    if (target.closest("button, input, textarea, select, a, label, .cm-editor, [contenteditable=true]")) return;
    startDragging(event);
  });
  panel.querySelector(".panel-close")?.addEventListener("click", () => void closePanel());
  if (mode === "pages") await renderPages("");
  else if (mode === "settings") await renderSettings();
  else await renderAiPanel();
}

async function closePanel(): Promise<void> {
  if (!panelMode) return;
  panelMode = null;
  destroyAiResultEditor();
  if (aiRequest && !aiResult) void invoke("cancel_ai");
  aiRequest = null;
  aiResult = "";
  layout.querySelector(".panel")?.remove();
  layout.classList.remove("panel-left");
  await invoke("set_panel", { open: false, panelWidthLogical: null });
  layout.style.removeProperty("--panel-width");
  clearTimeout(toolbarHideTimer);
  toolbar.classList.remove("visible");
  editor.focus({ preventScroll: true });
}

async function renderPages(query: string): Promise<void> {
  const content = layout.querySelector<HTMLElement>(".panel-content");
  if (!content || panelMode !== "pages") return;
  const requestId = ++pagesRequestId;
  const pages = await invoke<PageSummary[]>("list_pages", { query });
  if (requestId !== pagesRequestId || panelMode !== "pages") return;
  content.replaceChildren();
  const search = document.createElement("input");
  search.className = "search";
  search.type = "search";
  search.placeholder = "Search pages";
  search.value = query;
  content.append(search);
  const list = document.createElement("div");
  list.className = "page-list";
  content.append(list);

  if (pages.length === 0) {
    list.innerHTML = `<div class="empty">No matching pages.</div>`;
  } else {
    for (const page of pages) {
      const button = document.createElement("button");
      button.className = "page-item";
      button.type = "button";
      const snippet = document.createElement("div");
      snippet.className = "page-snippet";
      appendHighlighted(snippet, page.snippet || "Empty page", query);
      const date = document.createElement("small");
      date.textContent = new Date(page.createdAt).toLocaleString();
      button.append(snippet, date);
      button.addEventListener("click", async () => {
        if (!await flushSave()) return;
        const selected = await invoke<Note>("select_note", { noteId: page.id });
        await closePanel();
        displayNote(selected);
      });
      list.append(button);
    }
  }

  let searchTimer: ReturnType<typeof setTimeout> | undefined;
  search.addEventListener("input", () => {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(() => void renderPages(search.value), 100);
  });
  requestAnimationFrame(() => search.focus());
}

function appendHighlighted(target: HTMLElement, text: string, query: string): void {
  const terms = query.trim().split(/\s+/).filter(Boolean);
  if (terms.length === 0) {
    target.textContent = text;
    return;
  }
  const pattern = new RegExp(`(${terms.map(escapeRegExp).join("|")})`, "gi");
  for (const part of text.split(pattern)) {
    if (terms.some((term) => term.toLowerCase() === part.toLowerCase())) {
      const mark = document.createElement("mark");
      mark.textContent = part;
      target.append(mark);
    } else {
      target.append(document.createTextNode(part));
    }
  }
}

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

async function renderSettings(): Promise<void> {
  const content = layout.querySelector<HTMLElement>(".panel-content");
  if (!content || panelMode !== "settings") return;
  const [metrics, trash, clipboardConfig, aiConfig, providerStatuses] = await Promise.all([
    invoke<SummonMetrics>("summon_metrics"),
    invoke<DeletedPage[]>("list_deleted"),
    invoke<ClipboardSettings>("clipboard_settings"),
    invoke<AiSettings>("ai_settings"),
    invoke<ProviderStatus[]>("detect_ai_providers"),
  ]);
  providers = providerStatuses;
  aiSettings = aiConfig;
  const providerSummary = providerStatuses
    .map((provider) => `${provider.available ? "●" : "○"} ${provider.displayName}`)
    .join(" · ");
  content.innerHTML = `
    <div class="setting"><strong>Command palette</strong><small>Press ⌘⇧P for export and page navigation.</small></div>
    <label class="setting">Global shortcut<input class="setting-input shortcut-recorder" data-setting="shortcut" value="${escapeAttribute(shortcutDisplay)}" readonly><small>Click, then press a key combination.</small><small class="setting-error"></small></label>
    <label class="setting-row setting"><span>Theme</span><select class="setting-select" data-setting="theme"><option value="auto" ${theme === "auto" ? "selected" : ""}>Auto</option><option value="dark" ${theme === "dark" ? "selected" : ""}>Dark</option><option value="light" ${theme === "light" ? "selected" : ""}>Light</option></select></label>
    <div class="glass-settings setting-section">
      <label class="setting-row setting"><strong>Liquid Glass · experimental</strong><input type="checkbox" data-glass="enabled" ${glassSettings.enabled ? "checked" : ""}></label>
      <div class="glass-controls" ${glassSettings.enabled ? "" : "hidden"}>
        <small>Glass can be combined with any color theme. Auto follows macOS. Set opacity to 0% for no color tint.</small>
        <label class="setting-row setting"><span>Dark tint</span><input class="glass-color" type="color" data-glass="darkTint" value="${escapeAttribute(glassSettings.darkTint)}"></label>
        <label class="setting-row setting"><span>Light tint</span><input class="glass-color" type="color" data-glass="lightTint" value="${escapeAttribute(glassSettings.lightTint)}"></label>
        <label class="setting-row setting"><span>Opacity <output data-glass-opacity>${glassSettings.opacity}%</output></span><input class="glass-opacity" type="range" min="0" max="100" step="1" data-glass="opacity" value="${glassSettings.opacity}"></label>
      </div>
    </div>
    <label class="setting-row setting"><span>Font size</span><select class="setting-select" data-setting="font-size">${fontSizeOptions()}</select></label>
    <label class="setting-row setting"><span>Launch at login</span><input type="checkbox" data-setting="autostart" ${launchAtLogin ? "checked" : ""}></label>
    <div class="setting"><button class="panel-action" type="button" data-setting="export">Export Markdown</button><small class="export-result"></small></div>
    <div class="setting-section"><strong>Visible clipboard capture</strong><small>Clipboard changes append to the open note only while not is visible.</small>
      <label class="setting-row setting"><span>Capture text</span><input type="checkbox" data-clipboard="captureText" ${clipboardConfig.captureText ? "checked" : ""}></label>
      <label class="setting-row setting"><span>Capture images</span><input type="checkbox" data-clipboard="captureImages" ${clipboardConfig.captureImages ? "checked" : ""}></label>
      <label class="setting-row setting"><span>Ignore duplicates</span><input type="checkbox" data-clipboard="ignoreDuplicates" ${clipboardConfig.ignoreDuplicates ? "checked" : ""}></label>
      <label class="setting-row setting"><span>Ignore whitespace</span><input type="checkbox" data-clipboard="ignoreWhitespace" ${clipboardConfig.ignoreWhitespace ? "checked" : ""}></label>
      <label class="setting-row setting"><span>Filter likely secrets</span><input type="checkbox" data-clipboard="ignoreSensitive" ${clipboardConfig.ignoreSensitive ? "checked" : ""}></label>
      <label class="setting setting-compact">Text length<input class="setting-input" type="number" min="1" max="100000" data-clipboard="minimumTextLength" value="${clipboardConfig.minimumTextLength}"><span>to</span><input class="setting-input" type="number" min="1" max="1000000" data-clipboard="maximumTextLength" value="${clipboardConfig.maximumTextLength}"></label>
      <label class="setting">Ignored app bundle IDs<textarea class="setting-input settings-textarea" data-clipboard="ignoredApplications" placeholder="com.example.app, com.other.app">${escapeHtml(clipboardConfig.ignoredApplications)}</textarea><small>Comma or newline separated.</small></label>
    </div>
    <div class="setting-section"><strong>AI providers</strong><small>${escapeHtml(providerSummary)}</small>
      <label class="setting">Custom executable<input class="setting-input" data-ai="customProgram" value="${escapeAttribute(aiConfig.customProgram)}" placeholder="/path/to/program"></label>
      <label class="setting">Custom arguments<input class="setting-input" data-ai="customArguments" value="${escapeAttribute(aiConfig.customArguments)}" placeholder="--flag value"></label>
      <small>Selected note text is sent to the provider only when you explicitly run an AI command.</small>
    </div>
    <div class="setting"><strong>Warm summon handler</strong><div class="metrics">${metrics.count} samples\np50 ${metrics.p50Micros} µs\np95 ${metrics.p95Micros} µs\np99 ${metrics.p99Micros} µs</div></div>
    <div class="setting"><strong>Summon to webview frame</strong><div class="metrics">${metrics.visibleCount} samples\np50 ${formatMillis(metrics.visibleP50Micros)} ms\np95 ${formatMillis(metrics.visibleP95Micros)} ms\np99 ${formatMillis(metrics.visibleP99Micros)} ms\nfirst inputs captured ${metrics.firstInputCount}</div></div>
    <div class="setting"><strong>Trash · 7 days</strong><div class="trash-list"></div></div>
  `;

  const shortcutInput = content.querySelector<HTMLInputElement>("[data-setting=shortcut]");
  const shortcutError = content.querySelector<HTMLElement>(".setting-error");
  let recordingShortcut = false;
  let savingShortcut = false;
  shortcutInput?.addEventListener("focus", () => {
    recordingShortcut = true;
    void invoke("set_shortcut_recording", { recording: true });
    shortcutInput.value = "Press shortcut…";
    if (shortcutError) shortcutError.textContent = "";
  });
  shortcutInput?.addEventListener("blur", () => {
    void invoke("set_shortcut_recording", { recording: false });
    if (!recordingShortcut) return;
    recordingShortcut = false;
    shortcutInput.value = shortcutDisplay;
  });
  shortcutInput?.addEventListener("keydown", async (event) => {
    event.preventDefault();
    event.stopPropagation();
    if (savingShortcut) return;
    if (event.key === "Escape" && !event.metaKey && !event.ctrlKey && !event.altKey && !event.shiftKey) {
      recordingShortcut = false;
      shortcutInput.value = shortcutDisplay;
      shortcutInput.blur();
      return;
    }

    const recorded = recordShortcut(event);
    if (!recorded) {
      shortcutInput.value = event.metaKey || event.ctrlKey || event.altKey || event.shiftKey
        ? "Press another key…"
        : "Include a modifier…";
      return;
    }

    shortcutInput.value = recorded.display;
    savingShortcut = true;
    try {
      shortcut = await invoke<string>("set_shortcut", {
        shortcut: recorded.accelerator,
        label: recorded.display,
      });
      shortcutDisplay = recorded.display;
      recordingShortcut = false;
      shortcutInput.blur();
    } catch (error) {
      shortcutInput.value = "Try another shortcut…";
      if (shortcutError) shortcutError.textContent = String(error);
    } finally {
      savingShortcut = false;
    }
  });
  content.querySelector<HTMLInputElement>("[data-setting=autostart]")?.addEventListener("change", async (event) => {
    const target = event.currentTarget as HTMLInputElement;
    launchAtLogin = await invoke<boolean>("set_autostart", { enabled: target.checked });
    target.checked = launchAtLogin;
  });
  content.querySelector<HTMLSelectElement>("[data-setting=font-size]")?.addEventListener("change", async (event) => {
    const target = event.currentTarget as HTMLSelectElement;
    const previous = fontSize;
    fontSize = Number(target.value);
    applyFontSize();
    try {
      fontSize = await invoke<number>("set_font_size", { value: fontSize });
      target.value = String(fontSize);
      applyFontSize();
    } catch {
      fontSize = previous;
      target.value = String(previous);
      applyFontSize();
    }
  });
  content.querySelector<HTMLSelectElement>("[data-setting=theme]")?.addEventListener("change", async (event) => {
    const target = event.currentTarget as HTMLSelectElement;
    const previous = theme;
    theme = target.value as InitialState["theme"];
    applyTheme();
    try {
      theme = await invoke<InitialState["theme"]>("set_theme", { value: theme });
      target.value = theme;
      applyTheme();
    } catch {
      theme = previous;
      target.value = previous;
      applyTheme();
    }
  });
  const saveGlassSettings = async (): Promise<void> => {
    const darkTint = content.querySelector<HTMLInputElement>("[data-glass=darkTint]");
    const lightTint = content.querySelector<HTMLInputElement>("[data-glass=lightTint]");
    const opacity = content.querySelector<HTMLInputElement>("[data-glass=opacity]");
    const enabled = content.querySelector<HTMLInputElement>("[data-glass=enabled]");
    if (!darkTint || !lightTint || !opacity || !enabled) return;
    const previous = glassSettings;
    glassSettings = {
      enabled: enabled.checked,
      darkTint: darkTint.value,
      lightTint: lightTint.value,
      opacity: Number(opacity.value),
    };
    applyTheme();
    try {
      glassSettings = await invoke<GlassSettings>("set_glass_settings", { settings: glassSettings });
      darkTint.value = glassSettings.darkTint;
      lightTint.value = glassSettings.lightTint;
      opacity.value = String(glassSettings.opacity);
      enabled.checked = glassSettings.enabled;
    } catch {
      glassSettings = previous;
      darkTint.value = previous.darkTint;
      lightTint.value = previous.lightTint;
      opacity.value = String(previous.opacity);
      enabled.checked = previous.enabled;
      applyTheme();
    }
    const output = content.querySelector<HTMLOutputElement>("[data-glass-opacity]");
    if (output) output.value = `${glassSettings.opacity}%`;
  };
  content.querySelector<HTMLInputElement>("[data-glass=enabled]")?.addEventListener("change", (event) => {
    const enabled = (event.currentTarget as HTMLInputElement).checked;
    const controls = content.querySelector<HTMLElement>(".glass-controls");
    if (controls) controls.hidden = !enabled;
    void saveGlassSettings();
  });
  content.querySelector<HTMLInputElement>("[data-glass=darkTint]")?.addEventListener("change", () => void saveGlassSettings());
  content.querySelector<HTMLInputElement>("[data-glass=lightTint]")?.addEventListener("change", () => void saveGlassSettings());
  content.querySelector<HTMLInputElement>("[data-glass=opacity]")?.addEventListener("input", (event) => {
    const output = content.querySelector<HTMLOutputElement>("[data-glass-opacity]");
    if (output) output.value = `${(event.currentTarget as HTMLInputElement).value}%`;
  });
  content.querySelector<HTMLInputElement>("[data-glass=opacity]")?.addEventListener("change", () => void saveGlassSettings());
  content.querySelector("[data-setting=export]")?.addEventListener("click", async () => {
    const path = await invoke<string>("export_notes");
    const result = content.querySelector<HTMLElement>(".export-result");
    if (result) result.textContent = path;
  });

  const saveClipboardSettings = async (): Promise<void> => {
    const checked = (key: string): boolean => content.querySelector<HTMLInputElement>(`[data-clipboard=${key}]`)?.checked ?? false;
    const number = (key: string): number => Number(content.querySelector<HTMLInputElement>(`[data-clipboard=${key}]`)?.value ?? 0);
    const value: ClipboardSettings = {
      captureText: checked("captureText"),
      captureImages: checked("captureImages"),
      ignoreDuplicates: checked("ignoreDuplicates"),
      ignoreWhitespace: checked("ignoreWhitespace"),
      ignoreSensitive: checked("ignoreSensitive"),
      minimumTextLength: number("minimumTextLength"),
      maximumTextLength: number("maximumTextLength"),
      ignoredApplications: content.querySelector<HTMLTextAreaElement>("[data-clipboard=ignoredApplications]")?.value ?? "",
    };
    Object.assign(clipboardConfig, await invoke<ClipboardSettings>("set_clipboard_settings", { settings: value }));
    if (clipboardActive) {
      stopClipboardWatcher();
      startClipboardWatcher();
    }
  };
  content.querySelectorAll<HTMLInputElement | HTMLTextAreaElement>("[data-clipboard]").forEach((input) => {
    input.addEventListener("change", () => void saveClipboardSettings());
  });

  const saveAiSettings = async (): Promise<void> => {
    const value: AiSettings = {
      customProgram: content.querySelector<HTMLInputElement>("[data-ai=customProgram]")?.value ?? "",
      customArguments: content.querySelector<HTMLInputElement>("[data-ai=customArguments]")?.value ?? "",
      lastProvider: aiSettings.lastProvider,
    };
    aiSettings = await invoke<AiSettings>("set_ai_settings", { settings: value });
  };
  content.querySelectorAll<HTMLInputElement>("[data-ai]").forEach((input) => {
    input.addEventListener("change", () => void saveAiSettings());
  });

  const trashList = content.querySelector<HTMLElement>(".trash-list");
  if (trashList) {
    if (trash.length === 0) trashList.innerHTML = `<div class="empty">Trash is empty.</div>`;
    for (const item of trash) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "page-item";
      button.textContent = item.snippet || "Empty page";
      button.addEventListener("click", async () => {
        displayNote(await invoke<Note>("restore_note", { noteId: item.id }));
        await closePanel();
      });
      trashList.append(button);
    }
  }
}

function fontSizeOptions(): string {
  return Array.from({ length: 18 }, (_, index) => index + 11)
    .map((size) => `<option value="${size}" ${size === fontSize ? "selected" : ""}>${size} px</option>`)
    .join("");
}

function escapeAttribute(value: string): string {
  return value.replaceAll("&", "&amp;").replaceAll('"', "&quot;").replaceAll("<", "&lt;");
}

function escapeHtml(value: string): string {
  return escapeAttribute(value).replaceAll(">", "&gt;");
}

function formatMillis(micros: number): string {
  return (micros / 1000).toFixed(2);
}

interface PaletteCommand {
  label: string;
  keywords: string;
  disabled?: boolean;
  run: () => void | Promise<void>;
}

async function openAi(action?: "summarize" | "organize", scope: "selection" | "note" = "note"): Promise<void> {
  selectionMenu.hidden = true;
  const from = scope === "selection" ? editor.selectionStart : 0;
  const to = scope === "selection" ? editor.selectionEnd : editor.value.length;
  const body = editor.value.slice(from, to);
  if (!body.trim()) { showActionToast(scope === "selection" ? "Select some text first" : "This page is empty"); return; }
  aiRequest = action ? { action, scope, noteId: note.id, body: editor.value, from, to, provider: "custom" } : null;
  await openPanel("ai");
}

async function renderAiPanel(): Promise<void> {
  const content = layout.querySelector<HTMLElement>(".panel-content");
  if (!content || panelMode !== "ai") return;
  [providers, aiSettings] = await Promise.all([
    invoke<ProviderStatus[]>("detect_ai_providers"),
    invoke<AiSettings>("ai_settings"),
  ]);
  const available = providers.filter((provider) => provider.available);
  if (!aiRequest) {
    content.innerHTML = `<div class="ai-action-picker"><p>Use AI on the current note.</p><button class="panel-action" data-ai-action="summarize">Summarize</button><button class="panel-action" data-ai-action="organize">Organize</button></div>`;
    content.querySelectorAll<HTMLElement>("[data-ai-action]").forEach((button) => button.addEventListener("click", () => void beginAi(button.dataset.aiAction as "summarize" | "organize", "note", available)));
    return;
  }
  await beginAi(aiRequest.action, aiRequest.scope, available);
}

async function beginAi(action: "summarize" | "organize", scope: "selection" | "note", available = providers.filter((provider) => provider.available)): Promise<void> {
  const content = layout.querySelector<HTMLElement>(".panel-content");
  if (!content || panelMode !== "ai") return;
  if (available.length === 0) {
    content.innerHTML = `<div class="empty">No AI provider was found. Configure a custom command in Settings.</div>`;
    return;
  }
  const remembered = available.find((provider) => provider.id === aiSettings.lastProvider);
  const provider = remembered ?? (available.length === 1 ? available[0] : null);
  if (!provider) {
    content.innerHTML = `<div class="setting"><strong>Choose a provider</strong><small>This choice is remembered.</small></div><div class="provider-list"></div>`;
    const list = content.querySelector<HTMLElement>(".provider-list");
    for (const option of available) {
      const button = document.createElement("button");
      button.className = "panel-action";
      button.textContent = option.displayName;
      button.addEventListener("click", async () => {
        aiSettings.lastProvider = option.id;
        aiSettings = await invoke<AiSettings>("set_ai_settings", { settings: aiSettings });
        await runAiPreview(action, scope, option);
      });
      list?.append(button);
    }
    return;
  }
  await runAiPreview(action, scope, provider);
}

async function runAiPreview(action: "summarize" | "organize", scope: "selection" | "note", provider: ProviderStatus): Promise<void> {
  const content = layout.querySelector<HTMLElement>(".panel-content");
  if (!content || panelMode !== "ai") return;
  const from = scope === "selection" ? editor.selectionStart : 0;
  const to = scope === "selection" ? editor.selectionEnd : editor.value.length;
  const source = editor.value.slice(from, to);
  if (!source.trim()) { content.innerHTML = `<div class="empty">Nothing selected.</div>`; return; }
  aiRequest = { action, scope, noteId: note.id, body: editor.value, from, to, provider: provider.id };
  aiResult = "";
  destroyAiResultEditor();
  content.innerHTML = `<div class="ai-status"><strong>${action === "summarize" ? "Summarizing" : "Organizing"} with ${escapeHtml(provider.displayName)}</strong><small>The selected ${scope === "selection" ? "text" : "note"} is being sent to this provider.</small><button class="panel-action" data-ai-cancel>Cancel</button></div>`;
  content.querySelector("[data-ai-cancel]")?.addEventListener("click", () => void invoke("cancel_ai"));
  try {
    const result = await invoke<AiResult>("run_ai", { provider: provider.id, action, body: source });
    if (!aiRequest || panelMode !== "ai") return;
    aiResult = result.markdown;
    renderAiResult(provider);
  } catch (error) {
    if (panelMode !== "ai") return;
    destroyAiResultEditor();
    content.innerHTML = `<div class="ai-status"><strong>AI failed</strong><small>${escapeHtml(String(error))}</small><button class="panel-action" data-ai-retry>Retry</button><button class="panel-action" data-ai-discard>Discard</button></div>`;
    content.querySelector("[data-ai-retry]")?.addEventListener("click", () => void retryAi());
    content.querySelector("[data-ai-discard]")?.addEventListener("click", () => void closePanel());
  }
}

function renderAiResult(provider: ProviderStatus): void {
  const content = layout.querySelector<HTMLElement>(".panel-content");
  if (!content || !aiRequest) return;
  destroyAiResultEditor();
  const conflicted = note.id !== aiRequest.noteId || editor.value !== aiRequest.body;
  content.innerHTML = `<div class="ai-result"><small>Result from ${escapeHtml(provider.displayName)}</small><div class="editor ai-result-editor" aria-label="Editable AI result"></div><small class="ai-conflict">${conflicted ? "The source changed. Retry before inserting." : ""}</small><div class="ai-result-actions"><button class="panel-action" data-ai-insert ${conflicted ? "disabled" : ""}>Insert</button><button class="panel-action" data-ai-retry>Retry</button><button class="panel-action" data-ai-discard>Discard</button></div></div>`;
  const resultHost = content.querySelector<HTMLElement>(".ai-result-editor");
  if (resultHost) {
    const preview = new ScratchpadEditor(resultHost, {
      input: () => { aiResult = preview.value; },
      selection: () => {},
      scroll: () => {},
      beforeInput: () => {},
      imagePaste: () => false,
    });
    aiResultEditor = preview;
    preview.setValue(aiResult, 0, 0, 0);
  }
  content.querySelector("[data-ai-insert]")?.addEventListener("click", () => void insertAiResult());
  content.querySelector("[data-ai-retry]")?.addEventListener("click", () => void retryAi());
  content.querySelector("[data-ai-discard]")?.addEventListener("click", () => void closePanel());
}

function destroyAiResultEditor(): void {
  aiResultEditor?.destroy();
  aiResultEditor = null;
}

async function retryAi(): Promise<void> {
  if (!aiRequest) return;
  const status = providers.find((provider) => provider.id === aiRequest?.provider);
  if (status) await runAiPreview(aiRequest.action, aiRequest.scope, status);
}

async function insertAiResult(): Promise<void> {
  if (!aiRequest || note.id !== aiRequest.noteId || editor.value !== aiRequest.body) return;
  editor.replaceRange(aiRequest.from, aiRequest.to, aiResult);
  await closePanel();
  mirrorDraft();
  scheduleSave();
}

function paletteCommands(): PaletteCommand[] {
  return [
    { label: "Open Pages", keywords: "search list notes", run: () => openPanel("pages") },
    { label: "Open Settings", keywords: "preferences", run: () => openPanel("settings") },
    { label: "Previous Page", keywords: "older back", run: () => navigate(-1) },
    { label: "Next Page", keywords: "newer forward", run: () => navigate(1) },
    { label: "Delete Current Page", keywords: "trash remove", run: deleteCurrent },
    {
      label: "Export Markdown",
      keywords: "backup save file",
      run: async () => showActionToast(`Exported to ${await invoke<string>("export_notes")}`),
    },
  ];
}

async function openCommandPalette(): Promise<void> {
  if (panelMode) await closePanel();
  paletteOpen = true;
  paletteSelection = 0;
  commandPalette.hidden = false;
  paletteSearch.value = "";
  renderCommandPalette();
  paletteSearch.focus();
}

function closeCommandPalette(): void {
  if (!paletteOpen) return;
  paletteOpen = false;
  commandPalette.hidden = true;
  editor.focus({ preventScroll: true });
}

function filteredPaletteCommands(): PaletteCommand[] {
  return paletteCommands().filter((command) => commandMatches(command.label, command.keywords, paletteSearch.value));
}

function renderCommandPalette(): void {
  const commands = filteredPaletteCommands();
  paletteSelection = Math.min(paletteSelection, Math.max(0, commands.length - 1));
  paletteList.replaceChildren();
  commands.forEach((command, index) => {
    const button = document.createElement("button");
    button.type = "button";
    button.role = "option";
    button.textContent = command.label;
    button.disabled = command.disabled ?? false;
    button.classList.toggle("selected", index === paletteSelection);
    button.addEventListener("pointermove", () => {
      paletteSelection = index;
      renderCommandPalette();
    });
    button.addEventListener("click", () => void executePaletteCommand(command));
    paletteList.append(button);
  });
}

async function executePaletteCommand(command: PaletteCommand): Promise<void> {
  if (command.disabled) return;
  closeCommandPalette();
  await command.run();
}

async function hideWindow(): Promise<void> {
  stopClipboardWatcher();
  if (!await flushSave()) return;
  closeCommandPalette();
  if (panelMode) await closePanel();
  scratchpadVisible = false;
  releaseHiddenResources();
  await invoke("save_window_state");
  await invoke("hide_window");
}

function releaseHiddenResources(): void {
  attachmentRequestId += 1;
  editor.setAttachments([]);
  neighborRequestId += 1;
  neighborPages = undefined;
  queuedKeyboardNavigation = 0;
  pagePreview.textContent = "";
  cleanupSwipe();
  destroyAiResultEditor();
}

function handleEditorInput(): void {
  mirrorDraft();
  scheduleSave();
  if (panelMode === "ai" && aiResult && aiRequest) {
    const provider = providers.find((item) => item.id === aiRequest?.provider);
    if (provider) renderAiResult(provider);
  }
}

function handleImagePaste(event: ClipboardEvent): boolean {
  const hasImage = Array.from(event.clipboardData?.items ?? []).some((item) => item.type.startsWith("image/"));
  if (!hasImage || busy) return false;
  busy = true;
  void invoke<Note>("paste_clipboard_image", { input: noteInput() })
    .then((next) => {
      displayNote(next);
      showActionToast("Image attached");
    })
    .catch((error) => showActionToast(`Image paste failed: ${String(error)}`))
    .finally(() => { busy = false; });
  return true;
}

function handleEditorSelection(): void {
  mirrorDraftView();
  scheduleSave();
  rememberedCaretLineEnd = editor.caretLineEnd();
  if (editor.selectionStart === editor.selectionEnd || panelMode || paletteOpen) {
    selectionMenu.hidden = true;
    return;
  }
  const coordinates = editor.selectionRect();
  if (!coordinates) return;
  const shell = editorShell.getBoundingClientRect();
  selectionMenu.style.left = `${Math.max(8, Math.min(shell.width - selectionMenu.offsetWidth - 8, coordinates.left - shell.left))}px`;
  selectionMenu.style.top = `${Math.max(8, coordinates.top - shell.top - 40)}px`;
  selectionMenu.hidden = false;
}

function handleEditorScroll(): void {
  mirrorDraftView();
  scheduleSave();
  selectionMenu.hidden = true;
}

function handleBeforeInput(): void {
  if (activeSummonSequence === 0) return;
  const sequence = activeSummonSequence;
  activeSummonSequence = 0;
  void invoke("record_first_input", { sequence });
}

editor.dom.addEventListener("wheel", (event) => {
  if (panelMode) return;
  if (event.shiftKey) {
    event.preventDefault();
    const direction = shiftedWheel.push(event.deltaX, event.deltaY);
    if (direction === 0) return;
    swipe.reset();
    if (swipeDirection !== 0 && !busy) cleanupSwipe();
    void navigate(direction, true);
    return;
  }
  const horizontal = Math.abs(event.deltaX) > Math.abs(event.deltaY) * 1.2;
  const update = swipe.push(event.deltaX, event.deltaY);
  if (!horizontal) return;
  event.preventDefault();
  if (busy || update.direction === 0) return;
  swipeDirection = update.direction;
  renderSwipePreview(update.distance);
  clearTimeout(swipeSettleTimer);
  swipeSettleTimer = setTimeout(() => void finishSwipe(), 36);
}, { passive: false });

selectionMenu.addEventListener("pointerdown", (event) => event.preventDefault());
selectionMenu.addEventListener("click", (event) => {
  const action = (event.target as HTMLElement).closest<HTMLElement>("[data-selection-ai]")?.dataset.selectionAi as "summarize" | "organize" | undefined;
  if (action) void openAi(action, "selection");
});

topTrigger.addEventListener("pointerenter", showToolbar);
toolbar.addEventListener("pointerenter", showToolbar);
toolbar.addEventListener("pointerleave", scheduleToolbarHide);
editorShell.addEventListener("pointermove", (event) => {
  const top = editorShell.getBoundingClientRect().top;
  if (event.clientY - top <= 16) showToolbar();
});
editorShell.addEventListener("pointerleave", scheduleToolbarHide);
const startDragging = (event: PointerEvent): void => {
  if (event.button === 0) void invoke("start_window_drag");
};
topTrigger.addEventListener("pointerdown", startDragging);
toolbarDrag.addEventListener("pointerdown", startDragging);

layout.addEventListener("click", (event) => {
  const action = (event.target as HTMLElement).closest<HTMLElement>("[data-action]")?.dataset.action;
  if (action === "pages") void (panelMode === "pages" ? closePanel() : openPanel("pages"));
  if (action === "ai") void (panelMode === "ai" ? closePanel() : openAi());
  if (action === "settings") void (panelMode === "settings" ? closePanel() : openPanel("settings"));
  if (action === "delete") void deleteCurrent();
  if (action === "undo") void undoDelete();
  if (action === "retry-save") void retryStorageOperation();
});

commandPalette.addEventListener("pointerdown", (event) => {
  if (event.target === commandPalette) closeCommandPalette();
});
paletteSearch.addEventListener("input", () => {
  paletteSelection = 0;
  renderCommandPalette();
});
paletteSearch.addEventListener("keydown", (event) => {
  const commands = filteredPaletteCommands();
  if (event.key === "ArrowDown") {
    event.preventDefault();
    paletteSelection = Math.min(commands.length - 1, paletteSelection + 1);
    renderCommandPalette();
  } else if (event.key === "ArrowUp") {
    event.preventDefault();
    paletteSelection = Math.max(0, paletteSelection - 1);
    renderCommandPalette();
  } else if (event.key === "Enter") {
    event.preventDefault();
    const command = commands[paletteSelection];
    if (command) void executePaletteCommand(command);
  } else if (event.key === "Escape") {
    event.preventDefault();
    closeCommandPalette();
  }
});

window.addEventListener("keydown", (event) => {
  if (paletteOpen) return;
  if (event.key === "Escape") {
    event.preventDefault();
    if (panelMode) void closePanel(); else void hideWindow();
    return;
  }
  if (event.metaKey && event.shiftKey && event.key.toLowerCase() === "p") {
    event.preventDefault();
    void openCommandPalette();
  } else if (event.metaKey && !event.shiftKey && !event.ctrlKey && !event.altKey && event.key.toLowerCase() === "t") {
    event.preventDefault();
    void createNewNote();
  } else if (event.metaKey && !event.shiftKey && !event.ctrlKey && !event.altKey && event.key.toLowerCase() === "w") {
    event.preventDefault();
    void hideWindow();
  } else if (event.metaKey && event.key.toLowerCase() === "k") {
    event.preventDefault();
    void openPanel("pages");
  } else if (event.metaKey && event.key === ",") {
    event.preventDefault();
    void openPanel("settings");
  } else if (event.ctrlKey && event.altKey && event.key === "ArrowLeft") {
    event.preventDefault();
    void navigate(-1, true);
  } else if (event.ctrlKey && event.altKey && event.key === "ArrowRight") {
    event.preventDefault();
    void navigate(1, true);
  } else if (event.metaKey && event.shiftKey && event.key === "Backspace") {
    event.preventDefault();
    void deleteCurrent();
  }
});

window.addEventListener("focus", () => {
  if (!panelMode && !paletteOpen) editor.focus({ preventScroll: true });
});
window.addEventListener("blur", () => void flushSave());

let geometryTimer: ReturnType<typeof setTimeout> | undefined;
const persistGeometry = (): void => {
  clearTimeout(geometryTimer);
  geometryTimer = setTimeout(() => void invoke("save_window_state"), 200);
};
void currentWindow.onMoved(persistGeometry);
void currentWindow.onResized(persistGeometry);
void listen("shortcut-hide", () => void hideWindow());
void listen<number>("shortcut-show", ({ payload: sequence }) => {
  activeSummonSequence = sequence;
  scratchpadVisible = true;
  startClipboardWatcher();
  void renderAttachments(note.id);
  requestAnimationFrame(() => void invoke("record_visible_frame", { sequence }));
});
void listen<ClipboardChange>("clipboard-change", ({ payload }) => {
  clipboardAppendQueue = clipboardAppendQueue.then(async () => {
    if (!clipboardActive || !await flushSave()) return;
    const input = noteInput();
    const expectedUpdatedAt = note.updatedAt;
    const caretLineEnd = rememberedCaretLineEnd;
    try {
      const next = await invoke<Note>("append_clipboard_change", { token: payload.token, input, expectedUpdatedAt, caretLineEnd });
      applyClipboardNote(next, payload.kind);
    } catch (error) {
      showActionToast(`Clipboard capture skipped: ${String(error)}`);
    }
  });
});

function startClipboardWatcher(): void {
  if (clipboardActive) return;
  clipboardActive = true;
  void invoke("start_visible_clipboard").catch(() => { clipboardActive = false; });
}

function stopClipboardWatcher(): void {
  if (!clipboardActive) return;
  clipboardActive = false;
  void invoke("stop_visible_clipboard");
}

function applyClipboardNote(next: Note, kind: ClipboardChange["kind"]): void {
  if (next.id !== note.id) return;
  note = next;
  editor.setAttachments([]);
  editor.applyExternalValue(next.body, next.cursorStart, next.cursorEnd, next.scrollTop);
  mirrorDraft();
  void renderAttachments(next.id);
  scheduleNeighborPreload();
  showActionToast(kind === "image" ? "Image appended" : "Clipboard text appended");
}
async function quitSafely(): Promise<void> {
  if (!await flushSave()) return;
  try {
    await invoke("quit_app");
  } catch {
    quitRetryPending = true;
    showStorageError("Couldn’t create a safety backup before quitting.");
  }
}

void listen("request-quit", () => void quitSafely());

async function start(): Promise<void> {
  const initial = await invoke<InitialState>("load_initial_state");
  shortcut = initial.shortcut;
  shortcutDisplay = initial.shortcutLabel ?? formatShortcut(shortcut);
  launchAtLogin = initial.launchAtLogin;
  fontSize = initial.fontSize;
  theme = initial.theme;
  glassSettings = initial.glassSettings;
  applyFontSize();
  applyTheme();
  displayNote(initial.note);
  await invoke("mark_ready");
}

void start();
