import { describe, it, expect } from "vitest";
import { activityOpacity, activityColor } from "./ActivityBar";

describe("activityOpacity", () => {
  /** A brand new event should be fully opaque. */
  it("returns 1 at age 0", () => {
    expect(activityOpacity(0)).toBe(1);
  });

  /** Events older than the window must be invisible. */
  it("returns 0 at the maximum age (4h)", () => {
    expect(activityOpacity(4 * 60 * 60)).toBe(0);
  });

  it("returns 0 well past the maximum age", () => {
    expect(activityOpacity(10 * 60 * 60)).toBe(0);
  });

  /**
   * Property: monotone non-increasing — older events never appear
   * brighter than newer events.
   */
  it("opacity never increases with age", () => {
    const samples = [0, 1, 5, 30, 60, 300, 1800, 3600, 7200, 14400];
    for (let i = 1; i < samples.length; i++) {
      const prev = activityOpacity(samples[i - 1]);
      const curr = activityOpacity(samples[i]);
      expect(curr).toBeLessThanOrEqual(prev);
    }
  });

  /** Property: opacity is always within [0, 1]. */
  it("opacity stays in [0, 1] across all ages", () => {
    for (const age of [-10, 0, 1, 1000, 14400, 100000]) {
      const o = activityOpacity(age);
      expect(o).toBeGreaterThanOrEqual(0);
      expect(o).toBeLessThanOrEqual(1);
    }
  });

  /**
   * Logarithmic — not linear. A 1-minute-old event should still be
   * substantially visible (linear would put it at ~0.996); confirm the
   * curve actually decays meaningfully early on.
   */
  it("decays meaningfully within the first minute", () => {
    const oneMinute = activityOpacity(60);
    expect(oneMinute).toBeLessThan(0.7);
    expect(oneMinute).toBeGreaterThan(0.4);
  });

  /** A 1h-old event should be faded but still faintly visible. */
  it("1h-old events are dim but not gone", () => {
    const oneHour = activityOpacity(3600);
    expect(oneHour).toBeGreaterThan(0);
    expect(oneHour).toBeLessThan(0.25);
  });
});

describe("activityColor", () => {
  /** Coach interventions stand out from ordinary tool noise. */
  it("uses red for blocked actions", () => {
    expect(
      activityColor({
        timestamp: "",
        hook_event: "Stop",
        action: "blocked — user away",
        detail: null,
      }),
    ).toBe("rgb(239 68 68)");
  });

  /** Tool family colors are stable so the visual stays readable. */
  it("colors Bash distinctly from Read", () => {
    const bash = activityColor({
      timestamp: "",
      hook_event: "PostToolUse",
      action: "observed",
      detail: "Bash",
    });
    const read = activityColor({
      timestamp: "",
      hook_event: "PostToolUse",
      action: "observed",
      detail: "Read",
    });
    expect(bash).not.toBe(read);
  });

  /** Unknown tools/events fall back to neutral zinc. */
  it("falls back to neutral for unknown tools", () => {
    expect(
      activityColor({
        timestamp: "",
        hook_event: "PostToolUse",
        action: "observed",
        detail: "SomeNewTool",
      }),
    ).toBe("rgb(113 113 122)");
  });

  /** UserPromptSubmit is the major lifecycle event — it must dominate
   * any other classification (color is yellow even if e.g. "blocked"
   * appears in the action by some future fluke). */
  it("uses yellow for UserPromptSubmit regardless of action text", () => {
    expect(
      activityColor({
        timestamp: "",
        hook_event: "UserPromptSubmit",
        action: "user spoke",
        detail: "anything",
      }),
    ).toBe("rgb(250 204 21)");
  });
});
