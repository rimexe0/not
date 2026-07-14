export interface RecordedShortcut {
  accelerator: string;
  display: string;
}

const modifierCodes = new Set([
  "AltLeft", "AltRight", "ControlLeft", "ControlRight",
  "MetaLeft", "MetaRight", "ShiftLeft", "ShiftRight",
]);

const keyLabels: Record<string, string> = {
  ArrowDown: "↓",
  ArrowLeft: "←",
  ArrowRight: "→",
  ArrowUp: "↑",
  Backquote: "`",
  Backslash: "\\",
  BracketLeft: "[",
  BracketRight: "]",
  Comma: ",",
  Equal: "=",
  Escape: "Esc",
  Minus: "-",
  Period: ".",
  Quote: "'",
  Semicolon: ";",
  Slash: "/",
  Space: "Space",
};

export function recordShortcut(event: Pick<KeyboardEvent,
  "altKey" | "code" | "ctrlKey" | "key" | "metaKey" | "shiftKey"
>): RecordedShortcut | null {
  if (!event.code || modifierCodes.has(event.code)) return null;

  const modifiers: string[] = [];
  let display = "";
  if (event.metaKey) {
    modifiers.push("Command");
    display += "⌘";
  }
  if (event.ctrlKey) {
    modifiers.push("Control");
    display += "⌃";
  }
  if (event.altKey) {
    modifiers.push("Alt");
    display += "⌥";
  }
  if (event.shiftKey) {
    modifiers.push("Shift");
    display += "⇧";
  }
  if (modifiers.length === 0) return null;

  const producedLabel = event.key.length === 1 && event.key !== " "
    ? event.key.toUpperCase()
    : undefined;
  const keyLabel = producedLabel ?? keyLabels[event.code] ?? event.code
    .replace(/^Key/, "")
    .replace(/^Digit/, "");

  return {
    accelerator: [...modifiers, event.code].join("+"),
    display: `${display}${keyLabel}`,
  };
}

export function formatShortcut(accelerator: string): string {
  const tokens = accelerator.split("+");
  const key = tokens.pop() ?? "";
  let display = "";
  for (const modifier of tokens) {
    if (["Command", "CommandOrControl", "Cmd", "Super"].includes(modifier)) display += "⌘";
    else if (["Control", "Ctrl"].includes(modifier)) display += "⌃";
    else if (["Alt", "Option"].includes(modifier)) display += "⌥";
    else if (modifier === "Shift") display += "⇧";
  }
  return `${display}${keyLabels[key] ?? key.replace(/^Key/, "").replace(/^Digit/, "")}`;
}
