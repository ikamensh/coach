import { useCoachStore } from "../store/useCoachStore";
import { formatDuration } from "../utils/time";
import { abbreviateCwd } from "../utils/path";
import { OwlIcon } from "./OwlIcon";
import { CursorIcon } from "./CursorIcon";
import { useState } from "react";
import { ActivityBar, ActivityLegend } from "./ActivityBar";

/** Top N tools by count, formatted like "Write: 14, Bash: 8". */
export function topTools(toolCounts: Record<string, number>, n = 3): string {
  return Object.entries(toolCounts)
    .sort(([, a], [, b]) => b - a)
    .slice(0, n)
    .map(([name, count]) => `${name}: ${count}`)
    .join(", ");
}

export function SessionList() {
  const sessions = useCoachStore((s) => s.sessions);
  const setSessionMode = useCoachStore((s) => s.setSessionMode);
  const setAllMode = useCoachStore((s) => s.setAllMode);
  const openSession = useCoachStore((s) => s.openSession);
  const [hovered, setHovered] = useState<string | null>(null);

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
          No active sessions.
          <br />
          Hook events will appear here automatically.
        </p>
      ) : (
        <ul className="space-y-1">
          {sessions.map((session) => {
            const tint = session.mode === "away" ? "#a1a1aa" : "#e8743c";
            return (
            <li
              key={session.pid}
              onClick={() => openSession(session.pid)}
              className="flex items-start gap-3 bg-zinc-100 dark:bg-zinc-800/50 rounded-lg px-3 py-2 cursor-pointer hover:bg-zinc-200 dark:hover:bg-zinc-800 transition-colors"
            >
              <div className="flex flex-col items-center flex-shrink-0 mt-0.5">
                {session.client === "cursor" ? (
                  <CursorIcon size={26} color={tint} />
                ) : (
                  <OwlIcon size={26} color={tint} />
                )}
                {session.active_agents > 0 && session.active_agents <= 6 && (
                  <div className="flex flex-wrap justify-center gap-0 mt-0.5" style={{ maxWidth: 26 }}>
                    {Array.from({ length: session.active_agents }).map((_, i) => (
                      <OwlIcon key={i} size={8} color={tint} />
                    ))}
                  </div>
                )}
                {session.active_agents > 6 && (
                  <div className="flex items-center gap-0.5 mt-0.5">
                    <OwlIcon size={8} color={tint} />
                    <span className="text-zinc-400" style={{ fontSize: 8, lineHeight: 1 }}>
                      x{session.active_agents}
                    </span>
                  </div>
                )}
              </div>
              <div className="flex-1 min-w-0">
                <div className="flex items-center justify-between gap-2">
                  <div className="flex items-center gap-1.5 min-w-0">
                    <div className="text-sm text-zinc-800 dark:text-zinc-200 font-medium truncate">
                      {session.coach_session_title ?? session.display_name}
                    </div>
                    {session.is_worktree && (
                      <span className="flex-shrink-0 text-xs px-1.5 py-0.5 rounded bg-orange-500/15 text-orange-600 dark:text-orange-400">
                        ⎇ worktree
                      </span>
                    )}
                  </div>
                  <button
                    onClick={(e) => {
                      e.stopPropagation();
                      setSessionMode(
                        session.pid,
                        session.mode === "present" ? "away" : "present",
                      );
                    }}
                    className={`text-xs px-2.5 py-0.5 rounded-md font-medium transition-colors flex-shrink-0 ${
                      session.mode === "away"
                        ? "bg-amber-500/20 text-amber-600 dark:text-amber-400 hover:bg-amber-500/30"
                        : "bg-emerald-500/20 text-emerald-600 dark:text-emerald-400 hover:bg-emerald-500/30"
                    }`}
                  >
                    {session.mode === "away" ? "Away" : "Present"}
                  </button>
                </div>
                <div className="text-xs text-zinc-400 dark:text-zinc-500 font-mono truncate">
                  {abbreviateCwd(session.cwd)}
                </div>
                <div className="text-xs text-zinc-400 dark:text-zinc-500">
                  {formatDuration(session.duration_secs)} · {session.event_count} events
                  {Object.keys(session.tool_counts).length > 0 && (
                    <span> · {topTools(session.tool_counts)}</span>
                  )}
                </div>
                <ActivityBar entries={session.activity} hovered={hovered} setHovered={setHovered} />
              </div>
            </li>
            );
          })}
        </ul>
      )}
      {sessions.length > 0 && <ActivityLegend hovered={hovered} setHovered={setHovered} />}
    </div>
  );
}
