import { useState } from "react";
import { useCoachStore } from "../store/useCoachStore";
import type { TokenSource, TokenStatus } from "../store/useCoachStore";
import { TopBar } from "./TopBar";

export const PROVIDERS = [
  {
    id: "google",
    label: "Google AI",
    envVar: "GEMINI_API_KEY",
    models: ["gemini-3.1-flash", "gemini-3.1-pro", "gemini-3.0-flash", "gemini-2.5-flash"],
  },
  {
    id: "anthropic",
    label: "Anthropic",
    envVar: "ANTHROPIC_API_KEY",
    models: ["claude-haiku-4-5", "claude-sonnet-4-6", "claude-opus-4-6"],
  },
  {
    id: "openai",
    label: "OpenAI",
    envVar: "OPENAI_API_KEY",
    models: ["gpt-5.4", "gpt-5.4-mini", "gpt-5.4-nano"],
  },
  {
    id: "openrouter",
    label: "OpenRouter",
    envVar: "OPENROUTER_API_KEY",
    models: ["qwen/qwen3.5-397b-a17b", "moonshotai/kimi-k2.5", "xiaomi/mimo-v2-pro"],
  },
];

const SOURCE_BADGE: Record<TokenSource, { label: string; className: string }> =
  {
    user: {
      label: "user",
      className:
        "bg-emerald-500/15 text-emerald-600 dark:text-emerald-400",
    },
    env: {
      label: "env",
      className:
        "bg-blue-500/15 text-blue-600 dark:text-blue-400",
    },
    none: {
      label: "not set",
      className:
        "bg-zinc-500/15 text-zinc-500 dark:text-zinc-500",
    },
  };

function TokenInput({
  provider,
  status,
}: {
  provider: (typeof PROVIDERS)[number];
  status: TokenStatus | undefined;
}) {
  const setApiToken = useCoachStore((s) => s.setApiToken);
  const [editing, setEditing] = useState(false);
  const [value, setValue] = useState("");

  const handleSave = () => {
    setApiToken(provider.id, value);
    setEditing(false);
    setValue("");
  };

  const handleClear = () => {
    setApiToken(provider.id, "");
  };

  const source: TokenSource = status?.source ?? "none";
  const badge = SOURCE_BADGE[source];

  if (editing) {
    return (
      <div className="flex gap-2">
        <input
          type="password"
          value={value}
          onChange={(e) => setValue(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && handleSave()}
          placeholder={provider.envVar}
          autoFocus
          className="flex-1 bg-zinc-100 dark:bg-zinc-800 border border-zinc-200 dark:border-zinc-700 rounded px-2 py-1 text-sm text-zinc-800 dark:text-zinc-200 placeholder-zinc-400 dark:placeholder-zinc-600 focus:outline-none focus:border-zinc-400 dark:focus:border-zinc-500 font-mono"
        />
        <button
          onClick={handleSave}
          disabled={!value.trim()}
          className="text-xs px-2 py-1 bg-emerald-500/20 text-emerald-600 dark:text-emerald-400 rounded hover:bg-emerald-500/30 disabled:opacity-30"
        >
          Save
        </button>
        <button
          onClick={() => {
            setEditing(false);
            setValue("");
          }}
          className="text-xs px-2 py-1 text-zinc-500 hover:text-zinc-700 dark:hover:text-zinc-300"
        >
          Cancel
        </button>
      </div>
    );
  }

  return (
    <div className="flex items-center gap-2">
      <span
        className={`text-[10px] px-1.5 py-0.5 rounded font-medium ${badge.className}`}
      >
        {badge.label}
      </span>
      {source === "env" && (
        <span className="text-xs text-zinc-400 dark:text-zinc-500 font-mono">
          {"$"}{status?.env_var ?? provider.envVar}
        </span>
      )}
      <div className="flex-1" />
      <button
        onClick={() => setEditing(true)}
        className="text-xs text-zinc-500 hover:text-zinc-700 dark:hover:text-zinc-300"
      >
        {source === "user" ? "Change" : "Override"}
      </button>
      {source === "user" && (
        <button
          onClick={handleClear}
          className="text-xs text-zinc-500 hover:text-red-500"
          title="Remove override, fall back to env if available"
        >
          Clear
        </button>
      )}
    </div>
  );
}

const RULE_LABELS: Record<string, { label: string; description: string }> = {
  outdated_models: {
    label: "Outdated LLM models",
    description:
      "Detect when code uses outdated model identifiers (gemini-2.0-flash, gpt-4o, claude-3-*) and suggest current versions",
  },
};

export function SettingsPane() {
  const model = useCoachStore((s) => s.model);
  const tokenStatus = useCoachStore((s) => s.tokenStatus);
  const modelError = useCoachStore((s) => s.modelError);
  const modelValidating = useCoachStore((s) => s.modelValidating);
  const setModel = useCoachStore((s) => s.setModel);
  const setView = useCoachStore((s) => s.setView);
  const engineMode = useCoachStore((s) => s.engineMode);
  const rules = useCoachStore((s) => s.rules);
  const setEngineMode = useCoachStore((s) => s.setEngineMode);
  const toggleRule = useCoachStore((s) => s.toggleRule);

  const observerCapableProviders = useCoachStore(
    (s) => s.observerCapableProviders,
  );

  const selectedProvider = PROVIDERS.find((p) => p.id === model.provider);
  const models = selectedProvider?.models ?? [];
  const activeSource = tokenStatus[model.provider]?.source ?? "none";
  const providerIsObserverCapable = observerCapableProviders.includes(
    model.provider,
  );

  return (
    <div className="flex flex-col gap-4 h-full">
      <TopBar title="Settings" onBack={() => setView("main")} />

      {/* Model Selection */}
      <section>
        <h2 className="text-sm font-medium text-zinc-400 mb-2 uppercase tracking-wide">
          Coach Model
        </h2>
        <div className="flex gap-2">
          <select
            value={model.provider}
            onChange={(e) =>
              setModel({
                provider: e.target.value,
                model:
                  PROVIDERS.find((p) => p.id === e.target.value)?.models[0] ??
                  "",
              })
            }
            className="flex-1 bg-zinc-100 dark:bg-zinc-800 border border-zinc-200 dark:border-zinc-700 rounded-lg px-3 py-1.5 text-sm text-zinc-800 dark:text-zinc-200 focus:outline-none"
          >
            {PROVIDERS.map((p) => (
              <option key={p.id} value={p.id}>
                {p.label}
                {!observerCapableProviders.includes(p.id)
                  ? " (no observer)"
                  : ""}
              </option>
            ))}
          </select>
          <select
            value={model.model}
            onChange={(e) =>
              setModel({ provider: model.provider, model: e.target.value })
            }
            className="flex-1 bg-zinc-100 dark:bg-zinc-800 border border-zinc-200 dark:border-zinc-700 rounded-lg px-3 py-1.5 text-sm text-zinc-800 dark:text-zinc-200 focus:outline-none font-mono"
          >
            {models.map((m) => (
              <option key={m} value={m}>
                {m}
              </option>
            ))}
          </select>
        </div>
        {activeSource === "none" && (
          <p className="text-xs text-amber-600 dark:text-amber-400 mt-1">
            No API token for {selectedProvider?.label} — set one below or export{" "}
            <code className="font-mono">{selectedProvider?.envVar}</code>
          </p>
        )}
        {modelValidating && (
          <p className="text-xs text-zinc-400 dark:text-zinc-500 mt-1">
            Validating model...
          </p>
        )}
        {modelError && !modelValidating && (
          <p className="text-xs text-red-600 dark:text-red-400 mt-1">
            {modelError}
          </p>
        )}
        {!providerIsObserverCapable && engineMode === "llm" && (
          <p
            className="text-xs text-amber-600 dark:text-amber-400 mt-1"
            data-testid="observer-warning"
          >
            {selectedProvider?.label ?? model.provider} does not support LLM
            observer sessions. Switch to a supported provider or use Rules
            engine mode.
          </p>
        )}
      </section>

      {/* Coach Engine */}
      <section>
        <h2 className="text-sm font-medium text-zinc-400 mb-2 uppercase tracking-wide">
          Coach Engine
        </h2>
        <div className="flex gap-1">
          <button
            onClick={() => setEngineMode("rules")}
            className={`flex-1 text-xs px-3 py-1.5 rounded-lg font-medium transition-colors ${
              engineMode === "rules"
                ? "bg-emerald-500/20 text-emerald-600 dark:text-emerald-400"
                : "bg-zinc-100 dark:bg-zinc-800 text-zinc-500 hover:text-zinc-700 dark:hover:text-zinc-300"
            }`}
          >
            Rules
          </button>
          <button
            onClick={() => setEngineMode("llm")}
            className={`flex-1 text-xs px-3 py-1.5 rounded-lg font-medium transition-colors ${
              engineMode === "llm"
                ? "bg-violet-500/20 text-violet-600 dark:text-violet-400"
                : "bg-zinc-100 dark:bg-zinc-800 text-zinc-500 hover:text-zinc-700 dark:hover:text-zinc-300"
            }`}
          >
            LLM
          </button>
        </div>
        <p className="text-xs text-zinc-400 dark:text-zinc-500 mt-1.5">
          {engineMode === "rules"
            ? "Pattern-matching only. No LLM calls, zero latency."
            : "Uses the coach model to evaluate context. Requires API token."}
        </p>
      </section>

      {/* Rules */}
      <section>
        <h2 className="text-sm font-medium text-zinc-400 mb-2 uppercase tracking-wide">
          Rules
        </h2>
        <div className="space-y-2">
          {rules.map((rule) => {
            const meta = RULE_LABELS[rule.id];
            return (
              <label
                key={rule.id}
                className="flex items-start gap-2.5 cursor-pointer group"
              >
                <input
                  type="checkbox"
                  checked={rule.enabled}
                  onChange={() => toggleRule(rule.id)}
                  className="mt-0.5 accent-emerald-500"
                />
                <div className="flex-1 min-w-0">
                  <div className="text-sm text-zinc-700 dark:text-zinc-300 font-medium group-hover:text-zinc-900 dark:group-hover:text-zinc-100 transition-colors">
                    {meta?.label ?? rule.id}
                  </div>
                  {meta?.description && (
                    <div className="text-xs text-zinc-400 dark:text-zinc-500 mt-0.5">
                      {meta.description}
                    </div>
                  )}
                </div>
              </label>
            );
          })}
          {rules.length === 0 && (
            <p className="text-xs text-zinc-400 dark:text-zinc-500 italic">
              No rules configured.
            </p>
          )}
        </div>
      </section>

      {/* API Tokens */}
      <section className="flex-1">
        <h2 className="text-sm font-medium text-zinc-400 mb-1 uppercase tracking-wide">
          API Tokens
        </h2>
        <p className="text-xs text-zinc-400 dark:text-zinc-500 mb-3">
          User overrides saved to ~/.coach/settings.json. Environment variables
          are detected automatically.
        </p>
        <div className="space-y-3">
          {PROVIDERS.map((provider) => (
            <div key={provider.id}>
              <div className="text-sm font-medium text-zinc-700 dark:text-zinc-300 mb-1">
                {provider.label}
              </div>
              <TokenInput provider={provider} status={tokenStatus[provider.id]} />
            </div>
          ))}
        </div>
      </section>
    </div>
  );
}
