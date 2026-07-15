import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import { defaultHighlightStyle, syntaxHighlighting, syntaxTree } from "@codemirror/language";
import { markdown, markdownKeymap } from "@codemirror/lang-markdown";
import { EditorSelection, EditorState, StateEffect, StateField, type Extension, Transaction } from "@codemirror/state";
import {
  Decoration,
  type DecorationSet,
  EditorView,
  keymap,
  WidgetType,
} from "@codemirror/view";
import { GFM } from "@lezer/markdown";
import { invoke } from "@tauri-apps/api/core";
import type { Attachment } from "./types";

interface EditorCallbacks {
  input: () => void;
  selection: () => void;
  scroll: () => void;
  beforeInput: () => void;
  imagePaste: (event: ClipboardEvent) => boolean;
}

const setAttachments = StateEffect.define<Map<string, Attachment>>();

const attachmentsField = StateField.define<Map<string, Attachment>>({
  create: () => new Map(),
  update(value, transaction) {
    for (const effect of transaction.effects) if (effect.is(setAttachments)) return effect.value;
    return value;
  },
});

export function selectedLineNumbers(state: EditorState): Set<number> {
  const lines = new Set<number>();
  for (const range of state.selection.ranges) {
    let line = state.doc.lineAt(range.from);
    const end = state.doc.lineAt(range.to).number;
    while (line.number <= end) {
      lines.add(line.number);
      if (line.number === state.doc.lines) break;
      line = state.doc.line(line.number + 1);
    }
  }
  return lines;
}

function inactiveRange(state: EditorState, from: number, to: number, active: Set<number>): boolean {
  return !active.has(state.doc.lineAt(from).number) && !active.has(state.doc.lineAt(Math.max(from, to - 1)).number);
}

class AttachmentWidget extends WidgetType {
  constructor(readonly attachment: Attachment, readonly sourceFrom: number) { super(); }

  eq(other: AttachmentWidget): boolean {
    return other.attachment.id === this.attachment.id && other.sourceFrom === this.sourceFrom;
  }

  toDOM(view: EditorView): HTMLElement {
    const wrapper = document.createElement("span");
    wrapper.className = "md-image-widget";
    const image = document.createElement("img");
    image.alt = "Pasted image";
    image.width = this.attachment.width;
    image.height = this.attachment.height;
    image.decoding = "async";
    const placeholderHeight = Math.min(260, Math.max(80, 320 * this.attachment.height / Math.max(1, this.attachment.width)));
    wrapper.style.minHeight = `${placeholderHeight}px`;
    wrapper.append(image);
    let loaded = false;
    const load = (): void => {
      if (loaded) return;
      loaded = true;
      image.src = this.attachment.thumbnailUrl;
    };
    image.addEventListener("load", () => { wrapper.style.minHeight = ""; }, { once: true });
    const observer = new IntersectionObserver((entries) => {
      if (entries.some((entry) => entry.isIntersecting)) {
        load();
        observer.disconnect();
      }
    }, { root: view.scrollDOM, rootMargin: "320px 0px" });
    observer.observe(wrapper);
    const initialCheck = requestAnimationFrame(() => {
      const widget = wrapper.getBoundingClientRect();
      const viewport = view.scrollDOM.getBoundingClientRect();
      if (widget.bottom >= viewport.top - 320 && widget.top <= viewport.bottom + 320) load();
    });
    wrapper.addEventListener("click", () => view.dispatch({ selection: { anchor: this.sourceFrom }, scrollIntoView: true }));
    (wrapper as HTMLElement & { cleanup?: () => void }).cleanup = () => {
      observer.disconnect();
      cancelAnimationFrame(initialCheck);
      image.removeAttribute("src");
    };
    return wrapper;
  }

  destroy(dom: HTMLElement): void {
    (dom as HTMLElement & { cleanup?: () => void }).cleanup?.();
    dom.replaceChildren();
  }

  ignoreEvent(): boolean { return false; }
}

class TaskWidget extends WidgetType {
  constructor(readonly checked: boolean, readonly from: number, readonly to: number) { super(); }

  eq(other: TaskWidget): boolean { return other.checked === this.checked && other.from === this.from; }

  toDOM(view: EditorView): HTMLElement {
    const input = document.createElement("input");
    input.type = "checkbox";
    input.className = "md-task-checkbox";
    input.checked = this.checked;
    input.addEventListener("change", () => view.dispatch({
      changes: { from: this.from, to: this.to, insert: input.checked ? "[x]" : "[ ]" },
    }));
    return input;
  }

  ignoreEvent(): boolean { return false; }
}

function decorations(state: EditorState): DecorationSet {
  const active = selectedLineNumbers(state);
  const attachments = state.field(attachmentsField);
  const ranges: Array<{ from: number; to: number; decoration: Decoration }> = [];
  const imageRanges: Array<{ from: number; to: number }> = [];
  const markerNames = new Set([
    "HeaderMark", "EmphasisMark", "StrikethroughMark", "QuoteMark", "ListMark", "LinkMark", "CodeMark", "CodeInfo",
  ]);

  for (let lineNumber = 1; lineNumber <= state.doc.lines; lineNumber += 1) {
    if (active.has(lineNumber)) continue;
    const line = state.doc.line(lineNumber);
    const imagePattern = /!\[([^\]]*)\]\(attachment:\/\/([a-zA-Z0-9-]+)\)/g;
    for (const match of line.text.matchAll(imagePattern)) {
      const attachmentId = match[2];
      if (!attachmentId || match.index === undefined) continue;
      const attachment = attachments.get(attachmentId);
      if (!attachment) continue;
      const from = line.from + match.index;
      const to = from + match[0].length;
      imageRanges.push({ from, to });
      ranges.push({ from, to, decoration: Decoration.replace({ widget: new AttachmentWidget(attachment, from) }) });
    }
  }

  syntaxTree(state).iterate({
    enter(node) {
      const name = node.name;
      if (name === "Image" && imageRanges.some((range) => range.from === node.from && range.to === node.to)) return false;
      if (/^ATXHeading[1-6]$/.test(name)) {
        const level = name.at(-1);
        ranges.push({ from: node.from, to: node.to, decoration: Decoration.mark({ class: `md-heading md-h${level}` }) });
      } else if (name === "Emphasis") {
        ranges.push({ from: node.from, to: node.to, decoration: Decoration.mark({ class: "md-emphasis" }) });
      } else if (name === "StrongEmphasis") {
        ranges.push({ from: node.from, to: node.to, decoration: Decoration.mark({ class: "md-strong" }) });
      } else if (name === "Strikethrough") {
        ranges.push({ from: node.from, to: node.to, decoration: Decoration.mark({ class: "md-strike" }) });
      } else if (name === "InlineCode") {
        ranges.push({ from: node.from, to: node.to, decoration: Decoration.mark({ class: "md-inline-code" }) });
      } else if (name === "FencedCode" || name === "CodeBlock") {
        ranges.push({ from: node.from, to: node.to, decoration: Decoration.mark({ class: "md-code-block" }) });
      } else if (name === "Blockquote") {
        ranges.push({ from: node.from, to: node.to, decoration: Decoration.mark({ class: "md-quote" }) });
      } else if (name === "URL" && ["Link", "Image"].includes(node.node.parent?.name ?? "") && inactiveRange(state, node.from, node.to, active)) {
        ranges.push({ from: node.from, to: node.to, decoration: Decoration.replace({}) });
      } else if (markerNames.has(name) && inactiveRange(state, node.from, node.to, active)) {
        ranges.push({ from: node.from, to: node.to, decoration: Decoration.replace({}) });
      }
    },
  });

  for (let lineNumber = 1; lineNumber <= state.doc.lines; lineNumber += 1) {
    const line = state.doc.line(lineNumber);
    if (active.has(lineNumber)) continue;
    const text = line.text;
    const task = /^(\s*(?:[-+*]|\d+[.)])\s+)\[([ xX])\]/.exec(text);
    if (task?.[1] && task[2]) {
      const markerOffset = task[1].length;
      const from = line.from + markerOffset;
      ranges.push({ from, to: from + 3, decoration: Decoration.replace({ widget: new TaskWidget(task[2].toLowerCase() === "x", from, from + 3) }) });
    }
    const linkPattern = /\[([^\]]+)\]\(((?:https?:\/\/|mailto:)[^\s)]+)\)/g;
    for (const match of text.matchAll(linkPattern)) {
      if (match.index === undefined || !match[1] || !match[2]) continue;
      const from = line.from + match.index + 1;
      ranges.push({ from, to: from + match[1].length, decoration: Decoration.mark({ class: "md-link", attributes: { "data-href": match[2] } }) });
    }
    if (/^\s*(?:---+|___+|\*\*\*+)\s*$/.test(text)) {
      ranges.push({ from: line.from, to: line.to, decoration: Decoration.mark({ class: "md-rule" }) });
    }
  }

  return Decoration.set(ranges.map((range) => range.decoration.range(range.from, range.to)), true);
}

const hybridMarkdown = EditorView.decorations.compute(["doc", "selection", attachmentsField], decorations);

export class ScratchpadEditor {
  readonly view: EditorView;
  readonly dom: HTMLElement;
  private readonly extensions: Extension;

  constructor(parent: HTMLElement, callbacks: EditorCallbacks) {
    this.dom = parent;
    this.extensions = [
          history(),
          markdown({ extensions: GFM, completeHTMLTags: false }),
          syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
          keymap.of([...markdownKeymap, ...defaultKeymap, ...historyKeymap]),
          EditorView.lineWrapping,
          EditorView.contentAttributes.of({ spellcheck: "true", "aria-label": "Scratchpad" }),
          attachmentsField,
          hybridMarkdown,
          EditorView.updateListener.of((update) => {
            if (update.docChanged) callbacks.input();
            if (update.selectionSet) callbacks.selection();
          }),
          EditorView.domEventHandlers({
            beforeinput: () => { callbacks.beforeInput(); return false; },
            paste: (event) => callbacks.imagePaste(event),
            mousedown: (event) => {
              const target = (event.target as HTMLElement).closest<HTMLElement>(".md-link[data-href]");
              if (!target || !event.metaKey) return false;
              event.preventDefault();
              void invoke("open_external", { url: target.dataset.href });
              return true;
            },
          }),
        ];
    this.view = new EditorView({ parent, state: EditorState.create({ extensions: this.extensions }) });
    this.view.scrollDOM.addEventListener("scroll", callbacks.scroll, { passive: true });
  }

  get value(): string { return this.view.state.doc.toString(); }
  get selectionStart(): number { return this.view.state.selection.main.from; }
  get selectionEnd(): number { return this.view.state.selection.main.to; }
  get scrollTop(): number { return this.view.scrollDOM.scrollTop; }
  set scrollTop(value: number) { this.view.scrollDOM.scrollTop = value; }
  get style(): CSSStyleDeclaration { return this.dom.style; }
  get dataset(): DOMStringMap { return this.dom.dataset; }

  setValue(value: string, from: number, to: number, scrollTop: number): void {
    const start = Math.max(0, Math.min(value.length, from));
    const end = Math.max(start, Math.min(value.length, to));
    this.view.setState(EditorState.create({ doc: value, selection: EditorSelection.single(start, end), extensions: this.extensions }));
    requestAnimationFrame(() => { this.view.scrollDOM.scrollTop = scrollTop; });
  }

  applyExternalValue(value: string, from: number, to: number, scrollTop: number): void {
    const start = Math.max(0, Math.min(value.length, from));
    const end = Math.max(start, Math.min(value.length, to));
    this.view.dispatch({
      changes: { from: 0, to: this.view.state.doc.length, insert: value },
      selection: EditorSelection.single(start, end),
      annotations: Transaction.addToHistory.of(true),
    });
    requestAnimationFrame(() => { this.view.scrollDOM.scrollTop = scrollTop; });
  }

  setSelectionRange(from: number, to: number): void {
    const start = Math.max(0, Math.min(this.view.state.doc.length, from));
    const end = Math.max(start, Math.min(this.view.state.doc.length, to));
    this.view.dispatch({ selection: EditorSelection.single(start, end), scrollIntoView: true });
  }

  replaceRange(from: number, to: number, value: string): void {
    this.view.dispatch({ changes: { from, to, insert: value }, selection: { anchor: from + value.length } });
  }

  setAttachments(attachments: Attachment[]): void {
    this.view.dispatch({ effects: setAttachments.of(new Map(attachments.map((attachment) => [attachment.id, attachment]))) });
  }

  destroy(): void {
    this.view.destroy();
  }

  focus(options?: FocusOptions): void { this.view.focus(); if (options?.preventScroll) return; }
  caretLineEnd(): number { return this.view.state.doc.lineAt(this.selectionEnd).to; }
  selectionRect(): DOMRect | null {
    const coordinates = this.view.coordsAtPos(this.selectionEnd);
    return coordinates ? new DOMRect(coordinates.left, coordinates.top, coordinates.right - coordinates.left, coordinates.bottom - coordinates.top) : null;
  }
}
