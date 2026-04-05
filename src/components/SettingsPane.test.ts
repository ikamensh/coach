import { describe, it, expect } from "vitest";
import { PROVIDERS } from "./SettingsPane";

describe("PROVIDERS", () => {
  /** Every provider entry has the required shape for the settings UI to render. */
  it("each provider has id, label, envVar, and non-empty models", () => {
    for (const p of PROVIDERS) {
      expect(p.id).toBeTruthy();
      expect(p.label).toBeTruthy();
      expect(p.envVar).toBeTruthy();
      expect(p.models.length).toBeGreaterThan(0);
    }
  });

  /** Provider IDs must be unique so they can serve as keys. */
  it("has unique provider IDs", () => {
    const ids = PROVIDERS.map((p) => p.id);
    expect(new Set(ids).size).toBe(ids.length);
  });

  /**
   * Within each provider, model IDs should be unique so the datalist
   * and selection logic works correctly.
   */
  it("has no duplicate model IDs within a provider", () => {
    for (const p of PROVIDERS) {
      expect(new Set(p.models).size).toBe(p.models.length);
    }
  });

  /**
   * Every envVar follows the convention of uppercase with _API_KEY suffix.
   * This is a structural property the rest of the code relies on.
   */
  it("envVar follows UPPER_CASE_API_KEY pattern", () => {
    for (const p of PROVIDERS) {
      expect(p.envVar).toMatch(/^[A-Z][A-Z0-9_]*_API_KEY$/);
    }
  });
});
