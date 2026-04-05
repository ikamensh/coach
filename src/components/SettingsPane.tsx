import { useState } from "react";
import { useCoachStore } from "../store/useCoachStore";
import type { TokenSource } from "../store/useCoachStore";

export const PROVIDERS = [
  {
    id: "google",
    label: "Google AI",
    envVar: "GEMINI_API_KEY",
    models: ["gemini-2.5-flash", "gemini-2.5-pro", "gemini-3-flash-preview"],
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
  source,
  envVar,
}: {
  provider: (typeof PROVIDERS)[number];
  source: TokenSource;
  envVar?: string;
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
          {"$"}{envVar ?? provider.envVar}
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
          title={
            source === "user"
              ? "Remove override, fall back to env if available"
              : undefined
          }
        >
          Clear
        </button>
      )}
    </div>
  );
}

export function SettingsPane() {
  const model = useCoachStore((s) => s.model);
  const tokenStatus = useCoachStore((s) => s.tokenStatus);
  const modelError = useCoachStore((s) => s.modelError);
  const modelValidating = useCoachStore((s) => s.modelValidating);
  const setModel = useCoachStore((s) => s.setModel);
  const setView = useCoachStore((s) => s.setView);

  const selectedProvider = PROVIDERS.find((p) => p.id === model.provider);
  const models = selectedProvider?.models ?? [];
  const activeSource = tokenStatus[model.provider]?.source ?? "none";

  return (
    <div className="flex flex-col gap-4 h-full">
      <div className="flex items-center justify-between">
        <h1 className="text-lg font-semibold text-zinc-800 dark:text-zinc-100">
          Settings
        </h1>
        <button
          onClick={() => setView("main")}
          className="text-sm text-zinc-500 hover:text-zinc-700 dark:hover:text-zinc-300"
        >
          Back
        </button>
      </div>

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
              </option>
            ))}
          </select>
          <input
            list={`models-${model.provider}`}
            value={model.model}
            onChange={(e) =>
              setModel({ provider: model.provider, model: e.target.value })
            }
            placeholder="Model ID"
            className="flex-1 bg-zinc-100 dark:bg-zinc-800 border border-zinc-200 dark:border-zinc-700 rounded-lg px-3 py-1.5 text-sm text-zinc-800 dark:text-zinc-200 focus:outline-none font-mono"
          />
          <datalist id={`models-${model.provider}`}>
            {models.map((m) => (
              <option key={m} value={m} />
            ))}
          </datalist>
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
          {PROVIDERS.map((provider) => {
            const status = tokenStatus[provider.id];
            const source: TokenSource = status?.source ?? "none";
            return (
              <div key={provider.id}>
                <div className="text-sm font-medium text-zinc-700 dark:text-zinc-300 mb-1">
                  {provider.label}
                </div>
                <TokenInput provider={provider} source={source} envVar={status?.env_var} />
              </div>
            );
          })}
        </div>
      </section>
    </div>
  );
}
