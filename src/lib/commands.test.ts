import { describe, expect, it } from "vitest";
import { commandMatches } from "./commands";

describe("commandMatches", () => {
  it("matches every query term across labels and keywords", () => {
    expect(commandMatches("Export Markdown", "backup save file", "markdown backup")).toBe(true);
    expect(commandMatches("Summarize with Claude Code", "ai summary claude", "claude summary")).toBe(true);
  });

  it("rejects partial matches missing a term", () => {
    expect(commandMatches("Open Pages", "search list notes", "search clipboard")).toBe(false);
  });
});
