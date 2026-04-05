import { describe, it, expect } from "vitest";
import { timeAgo, projectName } from "./SessionList";

describe("projectName", () => {
  /** Extracts the last path segment as the project name. */
  it("returns last path component from a unix path", () => {
    expect(projectName("/home/user/projects/coach")).toBe("coach");
  });

  it("handles single-segment path", () => {
    expect(projectName("coach")).toBe("coach");
  });

  it("returns 'unknown' for null input", () => {
    expect(projectName(null)).toBe("unknown");
  });

  /** A trailing slash produces an empty last segment; should fall back to full path. */
  it("falls back to full path when last segment is empty", () => {
    expect(projectName("/home/user/")).toBe("/home/user/");
  });
});

describe("timeAgo", () => {
  /**
   * Property: timeAgo always returns a string ending with "ago".
   * This survives changes to thresholds and formatting details.
   */
  it("always ends with 'ago'", () => {
    const now = new Date().toISOString();
    expect(timeAgo(now)).toMatch(/ago$/);
  });

  it("shows seconds for very recent timestamps", () => {
    const fiveSecondsAgo = new Date(Date.now() - 5_000).toISOString();
    expect(timeAgo(fiveSecondsAgo)).toMatch(/^\d+s ago$/);
  });

  it("shows minutes for timestamps a few minutes old", () => {
    const threeMinutesAgo = new Date(Date.now() - 3 * 60_000).toISOString();
    expect(timeAgo(threeMinutesAgo)).toMatch(/^\d+m ago$/);
  });

  it("shows hours for timestamps over an hour old", () => {
    const twoHoursAgo = new Date(Date.now() - 2 * 3_600_000).toISOString();
    expect(timeAgo(twoHoursAgo)).toMatch(/^\d+h ago$/);
  });

  /**
   * Property: more recent timestamps produce smaller or equal numeric prefixes
   * than older timestamps (within the same unit).
   */
  it("older timestamps produce larger numbers", () => {
    const recent = new Date(Date.now() - 10_000).toISOString();
    const older = new Date(Date.now() - 50_000).toISOString();

    const recentNum = parseInt(timeAgo(recent));
    const olderNum = parseInt(timeAgo(older));
    expect(olderNum).toBeGreaterThanOrEqual(recentNum);
  });
});
