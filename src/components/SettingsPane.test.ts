import { describe, it, expect } from "vitest";
import { PROVIDERS } from "./SettingsPane";

/**
 * The backend's OBSERVER_CAPABLE_PROVIDERS list. Duplicated here so the
 * TS test suite can verify that the static PROVIDERS array stays consistent
 * with backend capability data without needing a live Tauri bridge.
 *
 * Source of truth: coach-core/src/settings/mod.rs → OBSERVER_CAPABLE_PROVIDERS
 */
const BACKEND_OBSERVER_CAPABLE: readonly string[] = [
  "openai",
  "anthropic",
  "google",
];

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

describe("observer-capable provider consistency", () => {
  /**
   * Regression: the frontend must include every observer-capable provider
   * so the model picker can render them. If a backend provider is missing
   * from PROVIDERS, the settings UI silently hides it.
   */
  it("PROVIDERS covers every backend observer-capable provider", () => {
    const frontendIds = PROVIDERS.map((p) => p.id);
    for (const cap of BACKEND_OBSERVER_CAPABLE) {
      expect(frontendIds).toContain(cap);
    }
  });

  /**
   * Regression: at least one provider in PROVIDERS should NOT be
   * observer-capable (e.g. openrouter), so the "(no observer)" suffix
   * and warning path get exercised.
   */
  it("at least one frontend provider is NOT observer-capable", () => {
    const nonObserver = PROVIDERS.filter(
      (p) => !BACKEND_OBSERVER_CAPABLE.includes(p.id),
    );
    expect(nonObserver.length).toBeGreaterThan(0);
  });

  /**
   * The warning message in SettingsPane uses `selectedProvider.label` so
   * it must be a non-empty human-readable string for every non-observer
   * provider.
   */
  it("non-observer providers have a label for the warning message", () => {
    const nonObserver = PROVIDERS.filter(
      (p) => !BACKEND_OBSERVER_CAPABLE.includes(p.id),
    );
    for (const p of nonObserver) {
      expect(p.label).toBeTruthy();
      expect(typeof p.label).toBe("string");
    }
  });

  /**
   * Guard: observer-capable providers all have models, so users can
   * actually select a model after picking them.
   */
  it("every observer-capable provider has at least one model", () => {
    for (const cap of BACKEND_OBSERVER_CAPABLE) {
      const prov = PROVIDERS.find((p) => p.id === cap);
      expect(prov, `missing provider ${cap}`).toBeDefined();
      expect(prov!.models.length).toBeGreaterThan(0);
    }
  });
});
