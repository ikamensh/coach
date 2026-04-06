import { useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useCoachStore } from "../store/useCoachStore";

interface SavedSession {
  id: string;
  project: string;
  mtime: number;
  size: number;
  topic: string;
  message_count: number;
  user_message_count: number;
  assistant_message_count: number;
}

interface ReplayEvent {
  index: number;
  kind: string;
  tool_name: string;
  timestamp: string;
  summary: string;
  action: string | null;
  message: string | null;
}

interface ReplayResult {
  session_id: string;
  topic: string;
  cwd: string;
  message_count: number;
  user_message_count: number;
  assistant_message_count: number;
  event_count: number;
  events: ReplayEvent[];
  first_intervention_index: number | null;
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes}B`;
  const kb = Math.floor(bytes / 1024);
  if (kb < 1024) return `${kb}K`;
  return `${(kb / 1024).toFixed(1)}M`;
}

function formatDate(epoch: number): string {
  const d = new Date(epoch * 1000);
  const now = new Date();
  const diffMs = now.getTime() - d.getTime();
  const diffDays = Math.floor(diffMs / 86400000);

  const time = d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  if (diffDays === 0) return `Today ${time}`;
  if (diffDays === 1) return `Yesterday ${time}`;
  if (diffDays < 7) return `${diffDays}d ago`;
  return d.toLocaleDateString([], { month: "short", day: "numeric" });
}

function abbreviateProject(project: string): string {
  return project.replace(/-Users-ikamen-/, "~/").replace(/-/g, "/");
}

function SessionBrowser({
  onSelect,
}: {
  onSelect: (id: string) => void;
}) {
  const [sessions, setSessions] = useState<SavedSession[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const result = await invoke<SavedSession[]>("list_saved_sessions", {
        limit: 50,
      });
      setSessions(result);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  // Load on first render
  if (sessions === null && !loading) {
    load();
  }

  if (loading) {
    return (
      <p className="text-xs text-zinc-400 dark:text-zinc-500 italic py-8 text-center">
        Scanning sessions...
      </p>
    );
  }

  if (error) {
    return (
      <div className="text-xs text-red-500 py-4 text-center">
        {error}
        <button onClick={load} className="ml-2 underline">
          Retry
        </button>
      </div>
    );
  }

  if (!sessions || sessions.length === 0) {
    return (
      <p className="text-xs text-zinc-400 dark:text-zinc-500 italic py-8 text-center">
        No saved sessions found in ~/.claude/projects/
      </p>
    );
  }

  return (
    <div className="space-y-0.5 overflow-y-auto flex-1">
      {sessions.map((s) => (
        <div
          key={s.id}
          onClick={() => onSelect(s.id)}
          className="bg-zinc-50 dark:bg-zinc-800/30 rounded px-3 py-2 cursor-pointer hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors"
        >
          <div className="flex items-center justify-between gap-2">
            <span className="text-xs text-zinc-700 dark:text-zinc-300 truncate flex-1">
              {s.topic || s.id.slice(0, 12)}
            </span>
            <span className="text-xs text-zinc-400 dark:text-zinc-500 flex-shrink-0">
              {formatDate(s.mtime)}
            </span>
          </div>
          <div className="flex items-center gap-3 text-xs text-zinc-400 dark:text-zinc-500 mt-0.5">
            <span className="font-mono truncate">
              {abbreviateProject(s.project)}
            </span>
            <span className="flex-shrink-0">
              {s.message_count} msgs ({s.user_message_count}u/{s.assistant_message_count}a)
            </span>
            <span className="flex-shrink-0">{formatSize(s.size)}</span>
          </div>
        </div>
      ))}
    </div>
  );
}

function ReplayView({
  sessionId,
  onBack,
}: {
  sessionId: string;
  onBack: () => void;
}) {
  const [result, setResult] = useState<ReplayResult | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [expandedEvent, setExpandedEvent] = useState<number | null>(null);

  const run = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const r = await invoke<ReplayResult>("replay_session", {
        sessionId,
      });
      setResult(r);
      // Auto-expand first intervention
      if (r.first_intervention_index !== null) {
        setExpandedEvent(r.first_intervention_index);
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, [sessionId]);

  if (!result && !loading && !error) {
    run();
  }

  if (loading) {
    return (
      <div className="flex flex-col gap-4 h-full">
        <Header
          title={`Replaying ${sessionId.slice(0, 12)}...`}
          onBack={onBack}
        />
        <p className="text-xs text-zinc-400 dark:text-zinc-500 italic py-8 text-center">
          Parsing session and evaluating events...
        </p>
      </div>
    );
  }

  if (error) {
    return (
      <div className="flex flex-col gap-4 h-full">
        <Header title="Replay Error" onBack={onBack} />
        <p className="text-xs text-red-500 py-4 text-center">{error}</p>
      </div>
    );
  }

  if (!result) return null;

  const interventionIdx = result.first_intervention_index;

  return (
    <div className="flex flex-col gap-3 h-full overflow-hidden">
      <Header title="Replay" onBack={onBack} />

      {/* Session info */}
      <section className="flex-shrink-0">
        <div className="text-xs space-y-0.5">
          <div className="text-zinc-700 dark:text-zinc-300 font-medium">
            {result.topic || result.session_id.slice(0, 12)}
          </div>
          {result.cwd && (
            <div className="font-mono text-zinc-400 dark:text-zinc-500 truncate">
              {result.cwd}
            </div>
          )}
          <div className="text-zinc-400 dark:text-zinc-500">
            {result.message_count} messages ({result.user_message_count}u/
            {result.assistant_message_count}a) · {result.event_count} hook
            events
          </div>
          {interventionIdx !== null ? (
            <div className="text-amber-600 dark:text-amber-400 font-medium">
              First intervention at event {interventionIdx + 1}/
              {result.event_count}
            </div>
          ) : (
            <div className="text-emerald-600 dark:text-emerald-400">
              No intervention would occur
            </div>
          )}
        </div>
      </section>

      {/* Event timeline */}
      <section className="flex-1 min-h-0 flex flex-col">
        <h2 className="text-sm font-medium text-zinc-400 mb-1.5 uppercase tracking-wide flex-shrink-0">
          Events
        </h2>

        {result.events.length === 0 ? (
          <p className="text-xs text-zinc-400 dark:text-zinc-600 italic">
            No hook events extracted.
          </p>
        ) : (
          <div className="space-y-0.5 overflow-y-auto flex-1">
            {result.events.map((ev) => {
              const isIntervention = ev.action !== null;
              const isFirst = ev.index === interventionIdx;
              const expanded = expandedEvent === ev.index;

              return (
                <div
                  key={ev.index}
                  onClick={() =>
                    setExpandedEvent(expanded ? null : ev.index)
                  }
                  className={`rounded px-3 py-1 text-xs cursor-pointer transition-colors ${
                    isFirst
                      ? "bg-amber-50 dark:bg-amber-900/20 ring-1 ring-amber-300 dark:ring-amber-700"
                      : isIntervention
                        ? "bg-red-50 dark:bg-red-900/10"
                        : "bg-zinc-50 dark:bg-zinc-800/30 hover:bg-zinc-100 dark:hover:bg-zinc-800"
                  }`}
                >
                  <div className="flex items-center gap-2">
                    <span className="text-zinc-400 dark:text-zinc-600 tabular-nums w-8 text-right flex-shrink-0">
                      {ev.index + 1}
                    </span>
                    <span
                      className={`font-medium flex-shrink-0 ${
                        ev.kind === "Stop"
                          ? "text-violet-600 dark:text-violet-400"
                          : "text-zinc-600 dark:text-zinc-400"
                      }`}
                    >
                      {ev.kind}
                    </span>
                    <span className="text-zinc-500 dark:text-zinc-500 truncate">
                      {ev.summary}
                    </span>
                    {isIntervention && (
                      <span
                        className={`flex-shrink-0 font-medium ${
                          ev.action === "blocked"
                            ? "text-red-500 dark:text-red-400"
                            : "text-amber-600 dark:text-amber-400"
                        }`}
                      >
                        {ev.action}
                      </span>
                    )}
                    {isFirst && (
                      <span className="flex-shrink-0 text-amber-500 dark:text-amber-400">
                        ← first
                      </span>
                    )}
                  </div>

                  {expanded && ev.message && (
                    <div className="mt-1.5 ml-10 p-2 bg-zinc-100 dark:bg-zinc-800 rounded text-zinc-600 dark:text-zinc-400 whitespace-pre-wrap break-words">
                      <div className="text-zinc-400 dark:text-zinc-500 text-[10px] uppercase tracking-wider mb-1">
                        Coach message
                      </div>
                      {ev.message}
                    </div>
                  )}

                  {expanded && !ev.message && !isIntervention && (
                    <div className="mt-1 ml-10 text-zinc-400 dark:text-zinc-600 italic">
                      Passthrough — no intervention
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        )}
      </section>
    </div>
  );
}

function Header({
  title,
  onBack,
}: {
  title: string;
  onBack: () => void;
}) {
  return (
    <div className="flex items-center justify-between flex-shrink-0">
      <div className="flex items-center gap-2 min-w-0">
        <button
          onClick={onBack}
          className="text-sm text-zinc-500 hover:text-zinc-700 dark:hover:text-zinc-300 flex-shrink-0"
        >
          Back
        </button>
        <h1 className="text-lg font-semibold text-zinc-800 dark:text-zinc-100 truncate">
          {title}
        </h1>
      </div>
    </div>
  );
}

export function DevPane() {
  const setView = useCoachStore((s) => s.setView);
  const [selectedId, setSelectedId] = useState<string | null>(null);

  if (selectedId) {
    return (
      <ReplayView
        sessionId={selectedId}
        onBack={() => setSelectedId(null)}
      />
    );
  }

  return (
    <div className="flex flex-col gap-3 h-full overflow-hidden">
      <div className="flex items-center justify-between flex-shrink-0">
        <div className="flex items-center gap-2">
          <button
            onClick={() => setView("main")}
            className="text-sm text-zinc-500 hover:text-zinc-700 dark:hover:text-zinc-300"
          >
            Back
          </button>
          <h1 className="text-lg font-semibold text-zinc-800 dark:text-zinc-100">
            Session Replay
          </h1>
        </div>
      </div>

      <h2 className="text-sm font-medium text-zinc-400 uppercase tracking-wide flex-shrink-0">
        Saved Sessions
      </h2>

      <SessionBrowser onSelect={setSelectedId} />
    </div>
  );
}
