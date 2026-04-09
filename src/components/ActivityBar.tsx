import { useEffect, useMemo, useState } from "react";
import type { ActivityEntry } from "../store/useCoachStore";

const FOUR_HOURS_SECS = 4 * 60 * 60;
const MAX_CHIPS = 60;
const TICK_MS = 5_000;

/**
 * Logarithmic opacity decay for an activity chip. Returns 1 at age 0,
 * smoothly fades to 0 by `maxSeconds`. Logarithmic curve means very
 * recent events stay vivid for a few seconds, then progressively
 * darker as they age.
 */
export function activityOpacity(
  ageSeconds: number,
  maxSeconds: number = FOUR_HOURS_SECS,
): number {
  if (ageSeconds <= 0) return 1;
  if (ageSeconds >= maxSeconds) return 0;
  return 1 - Math.log1p(ageSeconds) / Math.log1p(maxSeconds);
}

/**
 * Pick a chip color from an activity entry. User prompts are the major
 * lifecycle event and get a vivid yellow that pops against tool noise.
 * Coach interventions (blocks, auto-approvals) get alert colors; tool
 * calls are categorized by tool family; everything else falls back to
 * neutral.
 */
export function activityColor(entry: ActivityEntry): string {
  if (entry.hook_event === "UserPromptSubmit") return "rgb(250 204 21)"; // yellow-400
  if (entry.action.includes("blocked")) return "rgb(239 68 68)"; // red-500
  if (entry.action.includes("auto-approved")) return "rgb(245 158 11)"; // amber-500

  const tool = entry.detail ?? "";
  switch (tool) {
    case "Bash":
      return "rgb(249 115 22)"; // orange-500
    case "Read":
      return "rgb(59 130 246)"; // blue-500
    case "Grep":
    case "Glob":
      return "rgb(168 85 247)"; // purple-500
    case "Write":
    case "Edit":
    case "MultiEdit":
      return "rgb(16 185 129)"; // emerald-500
    case "Task":
      return "rgb(99 102 241)"; // indigo-500
    case "WebFetch":
    case "WebSearch":
      return "rgb(6 182 212)"; // cyan-500
  }
  if (entry.hook_event === "Observer") return "rgb(139 92 246)"; // violet-500
  return "rgb(113 113 122)"; // zinc-500
}

export const LEGEND_ENTRIES = [
  { key: "prompt", color: "rgb(250 204 21)", label: "Prompt" },
  { key: "blocked", color: "rgb(239 68 68)", label: "Blocked" },
  { key: "approved", color: "rgb(245 158 11)", label: "Approved" },
  { key: "bash", color: "rgb(249 115 22)", label: "Bash" },
  { key: "read", color: "rgb(59 130 246)", label: "Read" },
  { key: "search", color: "rgb(168 85 247)", label: "Search" },
  { key: "write", color: "rgb(16 185 129)", label: "Write" },
  { key: "task", color: "rgb(99 102 241)", label: "Task" },
  { key: "web", color: "rgb(6 182 212)", label: "Web" },
  { key: "observer", color: "rgb(139 92 246)", label: "Observer" },
  { key: "other", color: "rgb(113 113 122)", label: "Other" },
];

export function activityCategory(entry: ActivityEntry): string {
  if (entry.hook_event === "UserPromptSubmit") return "prompt";
  if (entry.action.includes("blocked")) return "blocked";
  if (entry.action.includes("auto-approved")) return "approved";
  const tool = entry.detail ?? "";
  switch (tool) {
    case "Bash": return "bash";
    case "Read": return "read";
    case "Grep": case "Glob": return "search";
    case "Write": case "Edit": case "MultiEdit": return "write";
    case "Task": return "task";
    case "WebFetch": case "WebSearch": return "web";
  }
  if (entry.hook_event === "Observer") return "observer";
  return "other";
}

/** UserPromptSubmit is a major lifecycle event — render it as a wider,
 * full-height "spike" so it stands out above the tool-call chip stream. */
function isMajor(entry: ActivityEntry): boolean {
  return entry.hook_event === "UserPromptSubmit";
}

function chipLabel(entry: ActivityEntry): string {
  const tool = entry.detail ? ` ${entry.detail}` : "";
  return `${entry.hook_event}${tool} — ${entry.action}`;
}

/**
 * A horizontal queue of recent events for one session, with logarithmic fade.
 * Takes the session's own activity entries (oldest-first); newer chips render
 * on the right.
 */
export function ActivityBar({
  entries,
  hovered,
  setHovered,
}: {
  entries: ActivityEntry[];
  hovered: string | null;
  setHovered: (cat: string | null) => void;
}) {
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), TICK_MS);
    return () => clearInterval(id);
  }, []);

  const chips = useMemo(() => entries.slice(-MAX_CHIPS), [entries]);

  if (chips.length === 0) {
    return (
      <div className="h-4 mt-1.5 rounded bg-zinc-200/40 dark:bg-zinc-700/30" />
    );
  }

  return (
    <div
      className="flex items-end gap-[2px] mt-1.5 h-4 overflow-hidden"
      aria-label="recent activity"
    >
      {chips.map((entry, i) => {
        const age = (now - new Date(entry.timestamp).getTime()) / 1000;
        const opacity = activityOpacity(age);
        if (opacity <= 0) return null;
        const major = isMajor(entry);
        return (
          <span
            key={`${entry.timestamp}-${i}`}
            title={chipLabel(entry)}
            className={
              major
                ? "block w-[7px] h-4 rounded-[1px] ring-1 ring-yellow-300/60 dark:ring-yellow-200/40"
                : "block w-[5px] h-2.5 rounded-[1px]"
            }
            style={{
              backgroundColor: activityColor(entry),
              opacity: hovered && activityCategory(entry) !== hovered ? opacity * 0.2 : opacity,
            }}
            onMouseEnter={() => setHovered(activityCategory(entry))}
            onMouseLeave={() => setHovered(null)}
          />
        );
      })}
    </div>
  );
}

export function ActivityLegend({
  hovered,
  setHovered,
}: {
  hovered: string | null;
  setHovered: (cat: string | null) => void;
}) {
  return (
    <div className="flex flex-wrap gap-x-3 gap-y-1 mt-3 px-1">
      {LEGEND_ENTRIES.map(({ key, color, label }) => (
        <div
          key={key}
          className={`flex items-center gap-1 text-[10px] cursor-default transition-opacity ${
            hovered === null
              ? "text-zinc-500"
              : hovered === key
                ? "text-zinc-800 dark:text-zinc-200"
                : "text-zinc-500 opacity-30"
          }`}
          onMouseEnter={() => setHovered(key)}
          onMouseLeave={() => setHovered(null)}
        >
          <span
            className="block w-2 h-2 rounded-[1px] flex-shrink-0"
            style={{ backgroundColor: color }}
          />
          {label}
        </div>
      ))}
    </div>
  );
}
