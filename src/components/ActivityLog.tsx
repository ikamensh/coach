import { useCoachStore } from "../store/useCoachStore";

function projectName(sessionId: string, sessions: { session_id: string; cwd: string | null }[]): string {
  const session = sessions.find((s) => s.session_id === sessionId);
  if (!session?.cwd) return sessionId.slice(0, 8);
  const parts = session.cwd.split("/");
  return parts[parts.length - 1] || sessionId.slice(0, 8);
}

export function ActivityLog() {
  const activityLog = useCoachStore((s) => s.activityLog);
  const sessions = useCoachStore((s) => s.sessions);
  const entries = [...activityLog].reverse();

  return (
    <div className="flex flex-col min-h-0">
      <h2 className="text-sm font-medium text-zinc-400 mb-2 uppercase tracking-wide">
        Activity
      </h2>

      {entries.length === 0 ? (
        <p className="text-xs text-zinc-400 dark:text-zinc-600 italic">
          No hook events yet.
        </p>
      ) : (
        <div className="space-y-0.5 overflow-y-auto flex-1">
          {entries.map((entry, i) => (
            <div
              key={i}
              className="bg-zinc-50 dark:bg-zinc-800/30 rounded px-3 py-1 text-xs"
            >
              <div className="flex items-center gap-2">
                <span className="text-zinc-400 dark:text-zinc-500 tabular-nums">
                  {new Date(entry.timestamp).toLocaleTimeString()}
                </span>
                <span className="text-zinc-500 dark:text-zinc-500">
                  {projectName(entry.session_id, sessions)}
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
    </div>
  );
}
