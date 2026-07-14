import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { SwipeTracker } from "./lib/swipe";
import type {
  DeleteResult,
  DeletedPage,
  InitialState,
  Neighbors,
  Note,
  NoteInput,
  PageSummary,
  PanelResult,
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
        <button type="button" class="danger" data-action="delete">Delete</button>
        <button type="button" data-action="settings">Settings</button>
      </div>
      <textarea class="editor" aria-label="Scratchpad" spellcheck="true"></textarea>
      <pre class="page-preview" aria-hidden="true" hidden></pre>
      <div class="indicator" aria-live="polite"></div>
      <div class="toast" aria-live="polite"><span>Page deleted</span><button type="button" data-action="undo">Undo</button></div>
      <div class="save-error" role="alert"><span>Couldn’t save this page.</span><button type="button" data-action="retry-save">Retry</button></div>
    </section>
  </div>
`;

const layout = required<HTMLElement>(".layout");
const editor = required<HTMLTextAreaElement>(".editor");
const editorShell = required<HTMLElement>(".editor-shell");
const pagePreview = required<HTMLElement>(".page-preview");
const toolbar = required<HTMLElement>(".toolbar");
const topTrigger = required<HTMLElement>(".top-trigger");
const toolbarDrag = required<HTMLElement>(".toolbar-drag");
const indicator = required<HTMLElement>(".indicator");
const toast = required<HTMLElement>(".toast");
const saveError = required<HTMLElement>(".save-error");
const currentWindow = getCurrentWindow();
const swipe = new SwipeTracker();

let note: Note;
let shortcut = "CommandOrControl+Shift+Space";
let launchAtLogin = true;
let saveTimer: ReturnType<typeof setTimeout> | undefined;
let saveQueue: Promise<boolean> = Promise.resolve(true);
let indicatorTimer: ReturnType<typeof setTimeout> | undefined;
let toastTimer: ReturnType<typeof setTimeout> | undefined;
let panelMode: "pages" | "settings" | null = null;
let deletedId: string | null = null;
let busy = false;
let pagesRequestId = 0;
let neighborPages: Neighbors | undefined;
let neighborRequestId = 0;
let swipeDirection: -1 | 0 | 1 = 0;
let swipeSettleTimer: ReturnType<typeof setTimeout> | undefined;
let activeSummonSequence = 0;
let quitRetryPending = false;

function required<T extends Element>(selector: string): T {
  const element = document.querySelector<T>(selector);
  if (!element) throw new Error(`missing ${selector}`);
  return element;
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
  editor.value = next.body;
  editor.setSelectionRange(next.cursorStart, next.cursorEnd);
  editor.scrollTop = next.scrollTop;
  mirrorDraft();
  requestAnimationFrame(() => {
    editor.focus({ preventScroll: true });
  });
  showIndicator();
  scheduleNeighborPreload();
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

async function navigate(direction: -1 | 1): Promise<void> {
  if (busy || panelMode) return;
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
      await completeSwipe(navigation, direction);
      return;
    }

    const next = await navigation;
    if (next.id === note.id) {
      await cancelSwipe();
      return;
    }
    pagePreview.textContent = next.body;
    await completeSwipe(Promise.resolve(next), direction);
  } catch {
    cleanupSwipe();
    editor.focus({ preventScroll: true });
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

async function completeSwipe(nextNote: Promise<Note>, direction: -1 | 1): Promise<void> {
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
  const animation = waitForTransformTransition(pagePreview, duration + 40);
  editor.style.transform = `translate3d(${direction === 1 ? -width : width}px, 0, 0)`;
  pagePreview.style.transform = "translate3d(0, 0, 0)";
  const [next] = await Promise.all([nextNote, animation]);
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

async function openPanel(mode: "pages" | "settings"): Promise<void> {
  if (panelMode === mode) return;
  if (panelMode) await closePanel();
  if (!await flushSave()) return;
  const geometry = await invoke<PanelResult>("set_panel", { open: true });
  panelMode = mode;
  layout.classList.toggle("panel-left", geometry.side === "left");
  toolbar.classList.add("visible");

  const panel = document.createElement("aside");
  panel.className = `panel panel-${geometry.side}`;
  panel.innerHTML = `<div class="panel-header"><h2>${mode === "pages" ? "Pages" : "Settings"}</h2><button class="panel-close" type="button" aria-label="Close">×</button></div><div class="panel-content"></div>`;
  layout.append(panel);
  panel.querySelector(".panel-close")?.addEventListener("click", () => void closePanel());
  if (mode === "pages") await renderPages("");
  else await renderSettings();
}

async function closePanel(): Promise<void> {
  if (!panelMode) return;
  panelMode = null;
  layout.querySelector(".panel")?.remove();
  layout.classList.remove("panel-left");
  await invoke("set_panel", { open: false });
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
  const [metrics, trash] = await Promise.all([
    invoke<SummonMetrics>("summon_metrics"),
    invoke<DeletedPage[]>("list_deleted"),
  ]);
  content.innerHTML = `
    <label class="setting">Global shortcut<input class="setting-input" data-setting="shortcut" value="${escapeAttribute(shortcut)}"></label>
    <label class="setting-row setting"><span>Launch at login</span><input type="checkbox" data-setting="autostart" ${launchAtLogin ? "checked" : ""}></label>
    <div class="setting"><button class="panel-action" type="button" data-setting="export">Export Markdown</button><small class="export-result"></small></div>
    <div class="setting"><strong>Warm summon handler</strong><div class="metrics">${metrics.count} samples\np50 ${metrics.p50Micros} µs\np95 ${metrics.p95Micros} µs\np99 ${metrics.p99Micros} µs</div></div>
    <div class="setting"><strong>Summon to webview frame</strong><div class="metrics">${metrics.visibleCount} samples\np50 ${formatMillis(metrics.visibleP50Micros)} ms\np95 ${formatMillis(metrics.visibleP95Micros)} ms\np99 ${formatMillis(metrics.visibleP99Micros)} ms\nfirst inputs captured ${metrics.firstInputCount}</div></div>
    <div class="setting"><strong>Trash · 7 days</strong><div class="trash-list"></div></div>
  `;

  const shortcutInput = content.querySelector<HTMLInputElement>("[data-setting=shortcut]");
  shortcutInput?.addEventListener("change", async () => {
    shortcut = await invoke<string>("set_shortcut", { shortcut: shortcutInput.value });
    shortcutInput.value = shortcut;
  });
  content.querySelector<HTMLInputElement>("[data-setting=autostart]")?.addEventListener("change", async (event) => {
    const target = event.currentTarget as HTMLInputElement;
    launchAtLogin = await invoke<boolean>("set_autostart", { enabled: target.checked });
    target.checked = launchAtLogin;
  });
  content.querySelector("[data-setting=export]")?.addEventListener("click", async () => {
    const path = await invoke<string>("export_notes");
    const result = content.querySelector<HTMLElement>(".export-result");
    if (result) result.textContent = path;
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

function escapeAttribute(value: string): string {
  return value.replaceAll("&", "&amp;").replaceAll('"', "&quot;").replaceAll("<", "&lt;");
}

function formatMillis(micros: number): string {
  return (micros / 1000).toFixed(2);
}

async function hideWindow(): Promise<void> {
  if (!await flushSave()) return;
  if (panelMode) await closePanel();
  await invoke("hide_window");
}

editor.addEventListener("input", () => {
  mirrorDraft();
  scheduleSave();
});
editor.addEventListener("select", () => {
  mirrorDraftView();
  scheduleSave();
});
editor.addEventListener("scroll", () => {
  mirrorDraftView();
  scheduleSave();
}, { passive: true });
editor.addEventListener("beforeinput", () => {
  if (activeSummonSequence === 0) return;
  const sequence = activeSummonSequence;
  activeSummonSequence = 0;
  void invoke("record_first_input", { sequence });
});
editor.addEventListener("wheel", (event) => {
  if (panelMode) return;
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

topTrigger.addEventListener("pointerenter", () => toolbar.classList.add("visible"));
toolbar.addEventListener("pointerleave", () => {
  if (!panelMode) toolbar.classList.remove("visible");
});
const startDragging = (event: PointerEvent): void => {
  if (event.button === 0) void invoke("start_window_drag");
};
topTrigger.addEventListener("pointerdown", startDragging);
toolbarDrag.addEventListener("pointerdown", startDragging);

layout.addEventListener("click", (event) => {
  const action = (event.target as HTMLElement).closest<HTMLElement>("[data-action]")?.dataset.action;
  if (action === "pages") void openPanel("pages");
  if (action === "settings") void openPanel("settings");
  if (action === "delete") void deleteCurrent();
  if (action === "undo") void undoDelete();
  if (action === "retry-save") void retryStorageOperation();
});

window.addEventListener("keydown", (event) => {
  if (event.key === "Escape") {
    event.preventDefault();
    if (panelMode) void closePanel(); else void hideWindow();
    return;
  }
  if (event.metaKey && event.key.toLowerCase() === "k") {
    event.preventDefault();
    void openPanel("pages");
  } else if (event.metaKey && event.key === ",") {
    event.preventDefault();
    void openPanel("settings");
  } else if (event.ctrlKey && event.altKey && event.key === "ArrowLeft") {
    event.preventDefault();
    void navigate(-1);
  } else if (event.ctrlKey && event.altKey && event.key === "ArrowRight") {
    event.preventDefault();
    void navigate(1);
  } else if (event.metaKey && event.shiftKey && event.key === "Backspace") {
    event.preventDefault();
    void deleteCurrent();
  }
});

window.addEventListener("focus", () => editor.focus({ preventScroll: true }));

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
  requestAnimationFrame(() => void invoke("record_visible_frame", { sequence }));
});
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
  launchAtLogin = initial.launchAtLogin;
  displayNote(initial.note);
  await invoke("mark_ready");
}

void start();
