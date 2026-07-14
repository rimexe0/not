import { describe, expect, it } from "vitest";
import { formatShortcut, recordShortcut } from "./shortcut";

describe("shortcut recorder", () => {
  it("records the physical key while displaying the produced character", () => {
    expect(recordShortcut({
      altKey: false,
      code: "Backquote",
      ctrlKey: false,
      key: "§",
      metaKey: true,
      shiftKey: false,
    })).toEqual({ accelerator: "Command+Backquote", display: "⌘§" });
  });

  it("records modifier combinations in a stable order", () => {
    expect(recordShortcut({
      altKey: true,
      code: "KeyK",
      ctrlKey: true,
      key: "k",
      metaKey: true,
      shiftKey: true,
    })).toEqual({ accelerator: "Command+Control+Alt+Shift+KeyK", display: "⌘⌃⌥⇧K" });
  });

  it("waits for a non-modifier key and rejects unmodified keys", () => {
    expect(recordShortcut({
      altKey: false,
      code: "MetaLeft",
      ctrlKey: false,
      key: "Meta",
      metaKey: true,
      shiftKey: false,
    })).toBeNull();
    expect(recordShortcut({
      altKey: false,
      code: "KeyA",
      ctrlKey: false,
      key: "a",
      metaKey: false,
      shiftKey: false,
    })).toBeNull();
  });

  it("formats persisted accelerators for the settings UI", () => {
    expect(formatShortcut("CommandOrControl+Shift+Space")).toBe("⌘⇧Space");
    expect(formatShortcut("Command+Backquote")).toBe("⌘`");
  });
});
