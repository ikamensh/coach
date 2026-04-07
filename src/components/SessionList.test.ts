import { describe, it, expect } from "vitest";
import { topTools } from "./SessionList";
import { timeAgo, formatDuration } from "../utils/time";

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

describe("topTools", () => {
  /** Returns tools sorted by count descending, formatted as "Name: count". */
  it("returns top N tools sorted by count", () => {
    const counts = { Write: 14, Bash: 8, Read: 20, Edit: 3 };
    expect(topTools(counts, 3)).toBe("Read: 20, Write: 14, Bash: 8");
  });

  it("returns all tools when fewer than N exist", () => {
    const counts = { Bash: 5 };
    expect(topTools(counts, 3)).toBe("Bash: 5");
  });

  it("returns empty string for empty counts", () => {
    expect(topTools({})).toBe("");
  });

  /** Property: result never contains more entries than requested. */
  it("respects the limit parameter", () => {
    const counts = { A: 1, B: 2, C: 3, D: 4, E: 5 };
    const result = topTools(counts, 2);
    expect(result.split(", ").length).toBe(2);
  });
});

describe("formatDuration", () => {
  /** Compact duration formatting: seconds, minutes, hours, and combinations. */
  it("formats seconds", () => {
    expect(formatDuration(45)).toBe("45s");
  });

  it("formats minutes", () => {
    expect(formatDuration(23 * 60)).toBe("23m");
  });

  it("formats exact hours", () => {
    expect(formatDuration(2 * 3600)).toBe("2h");
  });

  it("formats hours and minutes", () => {
    expect(formatDuration(3600 + 15 * 60)).toBe("1h 15m");
  });

  /** Property: output always contains a time unit character. */
  it("always contains a unit suffix", () => {
    for (const secs of [0, 1, 59, 60, 3599, 3600, 7200, 5400]) {
      expect(formatDuration(secs)).toMatch(/[smh]/);
    }
  });

  /** Property: longer durations produce longer or equal output strings. */
  it("longer input never produces shorter output", () => {
    const short = formatDuration(600);
    const long = formatDuration(6000);
    expect(long.length).toBeGreaterThanOrEqual(short.length);
  });
});
