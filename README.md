<p align="center">
  <img src="src-tauri/icons/icon.png" width="180" alt="not logo">
</p>

<h1 align="center">not.</h1>

<p align="center">
  <strong>Press a shortcut. Write something. Swipe it away.</strong>
  <br>
  A tiny, local-first stack of Markdown scratch paper for macOS.
</p>

`not` is not your notes app. It is the piece of paper next to it.

It stays warm and hidden in the background. Summon it, type immediately, then dismiss it without deciding where the thought belongs. Notes form a horizontal stack you can move through with a trackpad or keyboard.

## What it does

- **Appears instantly.** The window is preloaded and hidden instead of being rebuilt every time.
- **Feels like a stack of paper.** Swipe horizontally to move one page at a time, or use `Control + Option + ←/→` to jump quickly.
- **Writes real Markdown.** Source is stored as plain Markdown while headings, emphasis, lists, tasks, quotes, links, code, rules, and images render in place.
- **Keeps the syntax out of the way.** Markdown markers appear when the cursor enters their line and disappear when you leave it.
- **Finds anything.** The Pages panel searches complete note bodies through a local SQLite FTS index.
- **Accepts images inline.** Pasted images live beside the notes locally and appear inside the editor.
- **Captures the clipboard only when visible.** Copied text and images can append after the current caret line while the scratchpad is open. Nothing watches the clipboard while it is hidden.
- **Uses the AI tools you already have.** Summarize or organize a selection—or the whole current page—through an installed Codex CLI, Claude CLI, or custom executable. Results are previewed before they replace anything.
- **Looks like yours.** Choose Auto, Dark, or Light, change the global font size, and experiment with native Liquid Glass plus separate light/dark tint colors.
- **Stays local.** Notes, settings, attachments, full-text search, backups, and deleted pages live on your Mac.

## The interaction

1. Press your global shortcut.
2. Start typing. The first keystroke should never disappear.
3. Swipe for another page.
4. Press `Escape` or the shortcut again to hide it.

The toolbar stays out of sight until the pointer reaches the top edge. Pages, search, AI, deletion, and settings expand from the same window instead of opening more webviews.

Empty pages are deliberately boring: clearing an existing page keeps it, and swiping beyond the newest empty page does not manufacture more blanks.

## Markdown without a preview mode

The editor is both source and preview. `not` never silently rewrites the stored Markdown to make it look rendered.

- Inactive syntax is decorated in place.
- The active or selected line reveals its complete source.
- Task checkboxes are interactive.
- Command-click opens validated `http`, `https`, and `mailto` links.
- Local attachment images render inline.
- Remote Markdown images are never fetched automatically.

Export produces ordinary Markdown and copies local attachments alongside it.

## Privacy by behavior

There is no account, cloud database, analytics pipeline, or background network loop.

- Clipboard capture starts when the window becomes visible and stops when it hides.
- Existing clipboard contents are treated as a baseline, not pasted on launch.
- Likely secrets, duplicates, whitespace, oversized content, and ignored applications can be filtered.
- AI runs only after an explicit Summarize or Organize action.
- Note text is sent over `stdin` directly to the provider executable—never interpolated into a shell command.
- Remote images are not loaded behind your back.

## Performance is part of the product

`not` is built around the warm summon path, not a launch animation.

- The shortcut handler performs no database, filesystem, search, network, or frontend initialization work.
- The handler target is under **1 ms** from receiving the shortcut event to issuing native show/focus.
- Visible presentation targets the next available display frame.
- Hidden CPU should be effectively zero.
- Aggregate hidden memory targets **under 75 MB**, warns at **85 MB**, and fails architecturally at **100 MB**.
- Measurements belong to packaged release builds and report p50, p95, and p99—not one lucky sample.

Experimental native Liquid Glass is being judged against the same budget. Looking good does not exempt it from measurement.

See [performance contract tracking](https://github.com/rimexe0/not/issues/1).

## What it is not

`not` is intentionally not an all-in-one productivity workspace.

It does not currently try to be a calculator, OCR tool, timer suite, knowledge graph, collaborative document system, or permanent home for everything you write. Those can be great features; they are not worth making the first keystroke slower.

The center stays small: summon, write, swipe, retrieve.

## Current status

This is an early macOS-first build. There is no signed public download yet, and the v1 license has not been finalized. Build it from source if you want to experiment now.

### Requirements

- macOS 12 or newer
- Xcode Command Line Tools
- [Rust](https://rustup.rs/)
- Node.js and npm

Liquid Glass requires macOS 26 or newer. Older systems fall back to native macOS vibrancy.

### Build

```sh
git clone https://github.com/rimexe0/not.git
cd not
npm install
npm run tauri build
```

The packaged app is written to:

```text
src-tauri/target/release/bundle/macos/not.app
```

For frontend development:

```sh
npm run dev
```

For the complete desktop development build:

```sh
npm run tauri dev
```

## Under the paper

- [Tauri 2](https://tauri.app/) for the desktop shell
- Rust for persistence, clipboard handling, attachments, AI process execution, backups, and native window behavior
- SQLite + FTS5 for durable notes and full-body search
- CodeMirror 6 for the persistent hybrid Markdown editor
- Vanilla TypeScript and CSS—no frontend framework

One window. One webview. No polling loop.

## Roadmap

- [Validate the packaged performance contract](https://github.com/rimexe0/not/issues/1)
- [Ship a signed and notarized macOS DMG](https://github.com/rimexe0/not/issues/2)
- [Add a safe update and release workflow](https://github.com/rimexe0/not/issues/3)
- [Decide the v1 licensing and trial model](https://github.com/rimexe0/not/issues/4)
- [Design a lazy, permissioned plugin system](https://github.com/rimexe0/not/issues/5)

The plugin system can wait. The scratchpad has to remain tiny first.
