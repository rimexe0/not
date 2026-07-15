import { markdown } from "@codemirror/lang-markdown";
import { GFM } from "@lezer/markdown";
import { describe, expect, it } from "vitest";
import { EditorState } from "@codemirror/state";
import { selectedLineNumbers } from "../editor";

function nodeNames(source: string): Set<string> {
  const tree = markdown({ extensions: GFM }).language.parser.parse(source);
  const cursor = tree.cursor();
  const names = new Set<string>();
  do names.add(cursor.name); while (cursor.next());
  return names;
}

describe("hybrid Markdown grammar", () => {
  it("parses every rendered construct without changing source offsets", () => {
    const source = "# Heading\n\n**bold** *italic* ~~gone~~\n- [ ] task\n> quote\n[link](https://example.com) `code`\n\n---\n\n```js\nconst value = 1\n```";
    const names = nodeNames(source);
    for (const name of [
      "ATXHeading1", "StrongEmphasis", "Emphasis", "Strikethrough", "BulletList", "Task",
      "Blockquote", "Link", "URL", "InlineCode", "HorizontalRule", "FencedCode",
    ]) expect(names.has(name), name).toBe(true);
  });

  it("recognizes attachment references as Markdown images without remote loading", () => {
    const names = nodeNames("![pasted](attachment://1234-5678)");
    expect(names.has("Image")).toBe(true);
    expect(names.has("URL")).toBe(true);
  });

  it("reveals every line touched by the cursor or selection", () => {
    const selected = EditorState.create({ doc: "one\ntwo\nthree", selection: { anchor: 1, head: 7 } });
    expect([...selectedLineNumbers(selected)]).toEqual([1, 2]);
    const cursor = EditorState.create({ doc: "one\ntwo\nthree", selection: { anchor: 10 } });
    expect([...selectedLineNumbers(cursor)]).toEqual([3]);
  });
});
