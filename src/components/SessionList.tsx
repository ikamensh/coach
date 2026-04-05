import { useCoachStore } from "../store/useCoachStore";

export function projectName(cwd: string | null): string {
  if (!cwd) return "unknown";
  const parts = cwd.split("/");
  return parts[parts.length - 1] || cwd;
}

export function timeAgo(iso: string): string {
  const seconds = Math.floor(
    (Date.now() - new Date(iso).getTime()) / 1000,
  );
  if (seconds < 60) return `${seconds}s ago`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  return `${hours}h ago`;
}

export function SessionList() {
  const sessions = useCoachStore((s) => s.sessions);
  const setSessionMode = useCoachStore((s) => s.setSessionMode);
  const setAllMode = useCoachStore((s) => s.setAllMode);

  const allAway = sessions.length > 0 && sessions.every((s) => s.mode === "away");
  const allPresent = sessions.length > 0 && sessions.every((s) => s.mode === "present");

  return (
    <div>
      <div className="flex items-center justify-between mb-2">
        <h2 className="text-sm font-medium text-zinc-400 dark:text-zinc-400 uppercase tracking-wide">
          Sessions
          {sessions.length > 0 && (
            <span className="ml-2 text-xs font-normal normal-case text-zinc-500">
              ({sessions.length} active)
            </span>
          )}
        </h2>
        {sessions.length > 1 && (
          <div className="flex gap-1">
            <button
              onClick={() => setAllMode("present")}
              disabled={allPresent}
              className="text-xs px-2 py-0.5 rounded bg-emerald-500/10 text-emerald-600 dark:text-emerald-400 hover:bg-emerald-500/20 disabled:opacity-30 transition-colors"
            >
              All Present
            </button>
            <button
              onClick={() => setAllMode("away")}
              disabled={allAway}
              className="text-xs px-2 py-0.5 rounded bg-amber-500/10 text-amber-600 dark:text-amber-400 hover:bg-amber-500/20 disabled:opacity-30 transition-colors"
            >
              All Away
            </button>
          </div>
        )}
      </div>

      {sessions.length === 0 ? (
        <p className="text-xs text-zinc-400 dark:text-zinc-600 italic py-4 text-center">
          No active Claude Code sessions.
          <br />
          Hook events will appear here automatically.
        </p>
      ) : (
        <ul className="space-y-1">
          {sessions.map((session) => (
            <li
              key={session.session_id}
              className="flex items-center gap-3 bg-zinc-100 dark:bg-zinc-800/50 rounded-lg px-3 py-2"
            >
              <div
                className={`w-2 h-2 rounded-full flex-shrink-0 ${
                  session.mode === "away" ? "bg-amber-500" : "bg-emerald-500"
                }`}
              />
              <div className="flex-1 min-w-0">
                <div className="text-sm text-zinc-800 dark:text-zinc-200 font-medium truncate">
                  {projectName(session.cwd)}
                </div>
                <div className="text-xs text-zinc-400 dark:text-zinc-500">
                  {session.event_count} events · {timeAgo(session.last_event)}
                </div>
              </div>
              <button
                onClick={() =>
                  setSessionMode(
                    session.session_id,
                    session.mode === "present" ? "away" : "present",
                  )
                }
                className={`text-xs px-2.5 py-1 rounded-md font-medium transition-colors ${
                  session.mode === "away"
                    ? "bg-amber-500/20 text-amber-600 dark:text-amber-400 hover:bg-amber-500/30"
                    : "bg-emerald-500/20 text-emerald-600 dark:text-emerald-400 hover:bg-emerald-500/30"
                }`}
              >
                {session.mode === "away" ? "Away" : "Present"}
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
