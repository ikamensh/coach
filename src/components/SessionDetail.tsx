import { useMemo } from "react";
import { useCoachStore } from "../store/useCoachStore";
import { formatDuration, formatTime, timeAgo } from "../utils/time";
import { TopBar } from "./TopBar";

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
