import { useMemo } from "react";
import { useCoachStore } from "../store/useCoachStore";
import { formatDuration, formatTime, timeAgo } from "../utils/time";
import { TopBar } from "./TopBar";

/// Compact integer formatter — 1234 → "1.2k", 12345 → "12.3k", < 1000 → as-is.
/// Used for token counts where exact precision adds noise.
function formatTokens(n: number): string {
  if (n < 1000) return n.toString();
  if (n < 1_000_000) return `${(n / 1000).toFixed(1)}k`;
  return `${(n / 1_000_000).toFixed(1)}M`;
}

/// Latency formatter — sub-second in ms, otherwise seconds with one decimal.
function formatLatency(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  return `${(ms / 1000).toFixed(1)}s`;
}


export function SessionDetail() {
  const sessions = useCoachStore((s) => s.sessions);
  const selectedPid = useCoachStore((s) => s.selectedPid);
  const setSessionMode = useCoachStore((s) => s.setSessionMode);
  const setView = useCoachStore((s) => s.setView);

  const session = sessions.find((s) => s.pid === selectedPid);

  // Newest-first for the timeline display.
  const sessionEntries = useMemo(
    () => (session ? [...session.activity].reverse() : []),
    [session],
  );

  if (!session) {
    return (
      <div className="flex flex-col gap-4 h-full">
        <TopBar title="Session" onBack={() => setView("main")} />
        <p className="text-sm text-zinc-400 dark:text-zinc-500 italic text-center py-8">
          Session no longer active.
        </p>
      </div>
    );
  }

  const toolEntries = Object.entries(session.tool_counts).sort(
    ([, a], [, b]) => b - a,
  );
  const maxToolCount = toolEntries.length > 0 ? toolEntries[0][1] : 0;
  const otherCwds = session.cwd_history.filter((p) => p !== session.cwd);

  return (
    <div className="flex flex-col gap-4 h-full overflow-y-auto">
      <TopBar
        title={session.display_name}
        onBack={() => setView("main")}
        rightSlot={
          <button
            onClick={() =>
              setSessionMode(
                session.pid,
                session.mode === "present" ? "away" : "present",
              )
            }
            className={`text-xs px-2.5 py-1 rounded-md font-medium transition-colors flex-shrink-0 ${
              session.mode === "away"
                ? "bg-amber-500/20 text-amber-600 dark:text-amber-400 hover:bg-amber-500/30"
                : "bg-emerald-500/20 text-emerald-600 dark:text-emerald-400 hover:bg-emerald-500/30"
            }`}
          >
            {session.mode === "away" ? "Away" : "Present"}
          </button>
        }
      />

      {/* Overview */}
      <section>
        <h2 className="text-sm font-medium text-zinc-400 mb-2 uppercase tracking-wide">
          Overview
        </h2>
        <div className="space-y-1 text-xs">
          <div className="font-mono text-zinc-600 dark:text-zinc-400 truncate">
            {session.cwd}
          </div>
          <div className="font-mono text-zinc-400 dark:text-zinc-600">
            {session.session_id.slice(0, 12)}
          </div>
          <div className="text-zinc-500 dark:text-zinc-400">
            Started {formatTime(session.started_at)} · {timeAgo(session.started_at)} · {formatDuration(session.duration_secs)}
          </div>
          {otherCwds.length > 0 && (
            <div className="text-zinc-400 dark:text-zinc-500">
              Also worked in:{" "}
              {otherCwds.map((p, i) => (
                <span key={i} className="font-mono">
                  {i > 0 && ", "}
                  {p}
                </span>
              ))}
            </div>
          )}
        </div>
      </section>

      {/* Coach panel — visible whenever the LLM coach has done anything on
          this session, including just errors. Hidden only when truly idle. */}
      {(session.coach_calls > 0 ||
        session.coach_errors > 0 ||
        session.coach_last_assessment) && (
        <section>
          <div className="flex items-baseline justify-between mb-2">
            <h2 className="text-sm font-medium text-zinc-400 uppercase tracking-wide">
              Coach
            </h2>
            <span className="text-[10px] text-zinc-400 dark:text-zinc-500 font-mono uppercase">
              {session.coach_chain_kind === "empty"
                ? "idle"
                : session.coach_chain_kind}
            </span>
          </div>

          <div className="bg-amber-50 dark:bg-amber-500/5 border border-amber-200/60 dark:border-amber-500/20 rounded px-3 py-2 space-y-2">
            {/* Activity row: calls, errors, message count */}
            <div className="grid grid-cols-3 gap-2 text-xs">
              <div>
                <div className="text-[10px] uppercase tracking-wide text-zinc-400 dark:text-zinc-500">
                  Calls
                </div>
                <div className="tabular-nums text-zinc-700 dark:text-zinc-200">
                  {session.coach_calls}
                  {session.coach_errors > 0 && (
                    <span className="text-red-500 dark:text-red-400 ml-1">
                      · {session.coach_errors} err
                    </span>
                  )}
                </div>
              </div>
              <div>
                <div className="text-[10px] uppercase tracking-wide text-zinc-400 dark:text-zinc-500">
                  Chain
                </div>
                <div className="tabular-nums text-zinc-700 dark:text-zinc-200">
                  {session.coach_chain_messages} msgs
                </div>
              </div>
              <div>
                <div className="text-[10px] uppercase tracking-wide text-zinc-400 dark:text-zinc-500">
                  Last call
                </div>
                <div className="tabular-nums text-zinc-700 dark:text-zinc-200">
                  {session.coach_last_called_at
                    ? timeAgo(session.coach_last_called_at)
                    : "—"}
                  {session.coach_last_latency_ms !== null && (
                    <span className="text-zinc-400 dark:text-zinc-500 ml-1">
                      · {formatLatency(session.coach_last_latency_ms)}
                    </span>
                  )}
                </div>
              </div>
            </div>

            {/* Tokens row: last call vs cumulative. Only when we have any. */}
            {(session.coach_last_usage ||
              session.coach_total_usage.input_tokens > 0) && (
              <div className="grid grid-cols-2 gap-2 text-xs border-t border-amber-200/40 dark:border-amber-500/10 pt-2">
                <div>
                  <div className="text-[10px] uppercase tracking-wide text-zinc-400 dark:text-zinc-500">
                    Last call tokens
                  </div>
                  <div className="tabular-nums text-zinc-700 dark:text-zinc-200">
                    {session.coach_last_usage ? (
                      <>
                        {formatTokens(session.coach_last_usage.input_tokens)} in
                        {" / "}
                        {formatTokens(session.coach_last_usage.output_tokens)} out
                        {session.coach_last_usage.cached_input_tokens > 0 && (
                          <span className="text-zinc-400 dark:text-zinc-500 ml-1">
                            · {formatTokens(session.coach_last_usage.cached_input_tokens)} cached
                          </span>
                        )}
                      </>
                    ) : (
                      "—"
                    )}
                  </div>
                </div>
                <div>
                  <div className="text-[10px] uppercase tracking-wide text-zinc-400 dark:text-zinc-500">
                    Cumulative tokens
                  </div>
                  <div className="tabular-nums text-zinc-700 dark:text-zinc-200">
                    {formatTokens(session.coach_total_usage.input_tokens)} in
                    {" / "}
                    {formatTokens(session.coach_total_usage.output_tokens)} out
                    {session.coach_total_usage.cached_input_tokens > 0 && (
                      <span className="text-zinc-400 dark:text-zinc-500 ml-1">
                        · {formatTokens(session.coach_total_usage.cached_input_tokens)} cached
                      </span>
                    )}
                  </div>
                </div>
              </div>
            )}

            {/* Last assessment text — the thing the user actually wants to read. */}
            {session.coach_last_assessment && (
              <div className="border-t border-amber-200/40 dark:border-amber-500/10 pt-2">
                <div className="text-[10px] uppercase tracking-wide text-zinc-400 dark:text-zinc-500 mb-1">
                  Last assessment
                </div>
                <div className="text-xs text-zinc-700 dark:text-zinc-300 whitespace-pre-wrap">
                  {session.coach_last_assessment}
                </div>
              </div>
            )}
          </div>
        </section>
      )}

      {/* Tools */}
      {toolEntries.length > 0 && (
        <section>
          <h2 className="text-sm font-medium text-zinc-400 mb-2 uppercase tracking-wide">
            Tools
          </h2>
          <div className="space-y-1">
            {toolEntries.map(([name, count]) => (
              <div key={name} className="flex items-center gap-2 text-xs">
                <span className="w-20 text-zinc-600 dark:text-zinc-400 truncate text-right">
                  {name}
                </span>
                <div className="flex-1 h-3 bg-zinc-100 dark:bg-zinc-800 rounded overflow-hidden">
                  <div
                    className="h-full bg-emerald-500/40 dark:bg-emerald-500/30 rounded"
                    style={{ width: `${(count / maxToolCount) * 100}%` }}
                  />
                </div>
                <span className="w-8 text-zinc-400 dark:text-zinc-500 tabular-nums text-right">
                  {count}
                </span>
              </div>
            ))}
          </div>
        </section>
      )}

      {/* Coach Activity */}
      {session.stop_count > 0 && (
        <section>
          <h2 className="text-sm font-medium text-zinc-400 mb-2 uppercase tracking-wide">
            Coach Activity
          </h2>
          <div className="text-xs text-zinc-600 dark:text-zinc-400">
            Stops blocked: {session.stop_blocked_count} of {session.stop_count}
          </div>
        </section>
      )}

      {/* Timeline */}
      <section className="flex-1 min-h-0 flex flex-col">
        <h2 className="text-sm font-medium text-zinc-400 mb-2 uppercase tracking-wide">
          Timeline
        </h2>
        {sessionEntries.length === 0 ? (
          <p className="text-xs text-zinc-400 dark:text-zinc-600 italic">
            No events recorded yet.
          </p>
        ) : (
          <div className="space-y-0.5 overflow-y-auto flex-1">
            {sessionEntries.map((entry, i) => (
              <div
                key={i}
                className="bg-zinc-50 dark:bg-zinc-800/30 rounded px-3 py-1 text-xs"
              >
                <div className="flex items-center gap-2">
                  <span className="text-zinc-400 dark:text-zinc-500 tabular-nums">
                    {new Date(entry.timestamp).toLocaleTimeString()}
                  </span>
                  <span className="text-zinc-600 dark:text-zinc-400 font-medium">
                    {entry.hook_event}
                  </span>
                  <span
                    className={
                      entry.action.includes("auto-approved")
                        ? "text-amber-600 dark:text-amber-400"
                        : entry.action.includes("blocked")
                          ? "text-red-500 dark:text-red-400"
                          : "text-zinc-400 dark:text-zinc-500"
                    }
                  >
                    {entry.action}
                  </span>
                </div>
                {entry.detail && (
                  <div className="text-zinc-400 dark:text-zinc-600 mt-0.5 truncate">
                    {entry.detail}
                  </div>
                )}
              </div>
            ))}
          </div>
        )}
      </section>
    </div>
  );
}
