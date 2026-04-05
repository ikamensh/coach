import { useCoachStore } from "../store/useCoachStore";

const THEMES = [
  { value: "light" as const, label: "Light" },
  { value: "dark" as const, label: "Dark" },
  { value: "system" as const, label: "System" },
];

export function ThemeToggle() {
  const theme = useCoachStore((s) => s.theme);
  const setTheme = useCoachStore((s) => s.setTheme);

  return (
    <div className="flex gap-0.5 bg-zinc-100 dark:bg-zinc-800 rounded-lg p-0.5">
      {THEMES.map((t) => (
        <button
          key={t.value}
          onClick={() => setTheme(t.value)}
          className={`text-xs px-2.5 py-1 rounded-md transition-colors ${
            theme === t.value
              ? "bg-white dark:bg-zinc-700 text-zinc-800 dark:text-zinc-200 shadow-sm"
              : "text-zinc-500 hover:text-zinc-700 dark:hover:text-zinc-300"
          }`}
        >
          {t.label}
        </button>
      ))}
    </div>
  );
}
