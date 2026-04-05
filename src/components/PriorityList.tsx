import { useState } from "react";
import { useCoachStore } from "../store/useCoachStore";

export function PriorityList() {
  const priorities = useCoachStore((s) => s.priorities);
  const movePriority = useCoachStore((s) => s.movePriority);
  const removePriority = useCoachStore((s) => s.removePriority);
  const addPriority = useCoachStore((s) => s.addPriority);
  const [input, setInput] = useState("");

  const handleAdd = () => {
    const trimmed = input.trim();
    if (!trimmed) return;
    addPriority(trimmed);
    setInput("");
  };

  return (
    <div>
      <h2 className="text-sm font-medium text-zinc-400 mb-1 uppercase tracking-wide">
        Decision Priorities
      </h2>
      <p className="text-xs text-zinc-400 dark:text-zinc-500 mb-2">
        When away, coach tells Claude to decide using these (highest first)
      </p>

      <ul className="space-y-1">
        {priorities.map((p, i) => (
          <li
            key={i}
            className="flex items-center gap-2 bg-zinc-100 dark:bg-zinc-800/50 rounded-lg px-3 py-1.5 group"
          >
            <span className="text-zinc-400 dark:text-zinc-500 text-sm w-5 text-right">
              {i + 1}.
            </span>
            <span className="flex-1 text-zinc-700 dark:text-zinc-200 text-sm">
              {p}
            </span>
            <div className="flex gap-1 opacity-0 group-hover:opacity-100 transition-opacity">
              <button
                onClick={() => movePriority(i, "up")}
                disabled={i === 0}
                className="text-zinc-400 hover:text-zinc-600 dark:hover:text-zinc-300 disabled:opacity-30 text-xs px-1"
              >
                ▲
              </button>
              <button
                onClick={() => movePriority(i, "down")}
                disabled={i === priorities.length - 1}
                className="text-zinc-400 hover:text-zinc-600 dark:hover:text-zinc-300 disabled:opacity-30 text-xs px-1"
              >
                ▼
              </button>
              <button
                onClick={() => removePriority(i)}
                className="text-zinc-400 hover:text-red-500 text-xs px-1"
              >
                ✕
              </button>
            </div>
          </li>
        ))}
      </ul>

      <div className="flex gap-2 mt-2">
        <input
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && handleAdd()}
          placeholder="Add priority..."
          className="flex-1 bg-zinc-100 dark:bg-zinc-800 border border-zinc-200 dark:border-zinc-700 rounded-lg px-3 py-1.5 text-sm text-zinc-800 dark:text-zinc-200 placeholder-zinc-400 dark:placeholder-zinc-600 focus:outline-none focus:border-zinc-400 dark:focus:border-zinc-500"
        />
        <button
          onClick={handleAdd}
          disabled={!input.trim()}
          className="px-3 py-1.5 bg-zinc-200 dark:bg-zinc-700 hover:bg-zinc-300 dark:hover:bg-zinc-600 disabled:opacity-30 rounded-lg text-sm text-zinc-700 dark:text-zinc-300 transition-colors"
        >
          Add
        </button>
      </div>
    </div>
  );
}
