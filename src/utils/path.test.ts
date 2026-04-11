import { describe, it, expect } from "vitest";
import { abbreviateCwd, jsonlPath } from "./path";

describe("abbreviateCwd", () => {
  it("replaces the home prefix with ~", () => {
    expect(abbreviateCwd("/Users/alice/work/coach")).toBe("~/work/coach");
  });

  it("returns 'unknown' for null / empty", () => {
    expect(abbreviateCwd(null)).toBe("unknown");
    expect(abbreviateCwd("")).toBe("unknown");
  });

  it("passes non-home paths through unchanged", () => {
    expect(abbreviateCwd("/opt/data")).toBe("/opt/data");
  });
});

describe("jsonlPath", () => {
  const base = {
    client: "claude",
    session_id: "abc-123",
    cwd: "/Users/alice/work",
  };

  it("slugifies every non-alphanumeric byte to '-' (matches scanner.rs mangle_cwd)", () => {
    expect(jsonlPath(base)).toBe(
      "~/.claude/projects/-Users-alice-work/abc-123.jsonl",
    );
  });

  it("mangles underscores and dots the same way the Rust scanner does", () => {
    // Regression for the real bug fixed in Rust — `_` and `.` are NOT
    // preserved (scanner.rs comment: `/tmp/coach_llm_demo` lands under
    // `-tmp-coach-llm-demo`).
    expect(
      jsonlPath({ ...base, cwd: "/tmp/coach_llm_demo.v2" }),
    ).toBe("~/.claude/projects/-tmp-coach-llm-demo-v2/abc-123.jsonl");
  });

  it("returns null for non-Claude clients (Cursor/Codex store elsewhere)", () => {
    expect(jsonlPath({ ...base, client: "cursor" })).toBeNull();
    expect(jsonlPath({ ...base, client: "codex" })).toBeNull();
  });

  it("falls back to bootstrapped_session_id when session_id is empty", () => {
    // Scanner-discovered sessions have session_id="" until the first
    // live hook lands. The JSONL transcript already exists on disk
    // under the id the scanner read at bootstrap time.
    expect(
      jsonlPath({
        ...base,
        session_id: "",
        bootstrapped_session_id: "boot-999",
      }),
    ).toBe("~/.claude/projects/-Users-alice-work/boot-999.jsonl");
  });

  it("returns null when neither id is available", () => {
    expect(
      jsonlPath({
        ...base,
        session_id: "",
        bootstrapped_session_id: null,
      }),
    ).toBeNull();
    expect(jsonlPath({ ...base, session_id: "" })).toBeNull();
  });

  it("prefers the live session_id over the bootstrapped one", () => {
    // Once a real hook lands we key off the canonical id, not the
    // stale bootstrap id from the (possibly /clear'd) older file.
    expect(
      jsonlPath({
        ...base,
        session_id: "live-xyz",
        bootstrapped_session_id: "stale-old",
      }),
    ).toBe("~/.claude/projects/-Users-alice-work/live-xyz.jsonl");
  });

  it("returns null when cwd is unknown (no way to derive the project dir)", () => {
    expect(jsonlPath({ ...base, cwd: null })).toBeNull();
  });

  /**
   * Property: the derived path always ends with `<session_id>.jsonl`
   * and lives under `~/.claude/projects/`. Survives future changes
   * to the mangling rule.
   */
  it("always ends in the session_id's .jsonl under ~/.claude/projects", () => {
    const out = jsonlPath(base)!;
    expect(out).toMatch(/^~\/\.claude\/projects\//);
    expect(out).toMatch(/\/abc-123\.jsonl$/);
  });
});
