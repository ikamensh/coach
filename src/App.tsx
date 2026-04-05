import { useEffect } from "react";
import { useCoachStore } from "./store/useCoachStore";
import { useZoom } from "./hooks/useZoom";
import { SessionList } from "./components/SessionList";
import { PriorityList } from "./components/PriorityList";
import { ActivityLog } from "./components/ActivityLog";
import { ThemeToggle } from "./components/ThemeToggle";
import { SettingsPane } from "./components/SettingsPane";
import { HooksPane } from "./components/HooksPane";
import { SessionDetail } from "./components/SessionDetail";

export default function App() {
  const init = useCoachStore((s) => s.init);
  const initialized = useCoachStore((s) => s.initialized);
  const view = useCoachStore((s) => s.view);
  const setView = useCoachStore((s) => s.setView);
  const port = useCoachStore((s) => s.port);
  const model = useCoachStore((s) => s.model);
  const hookStatus = useCoachStore((s) => s.hookStatus);

  useZoom();

  useEffect(() => {
    init();
  }, [init]);

  if (!initialized) {
    return (
      <div className="h-screen flex items-center justify-center bg-white dark:bg-zinc-900 text-zinc-500">
        Loading...
      </div>
    );
  }

  if (view === "settings") {
    return (
      <div className="h-screen flex flex-col bg-white dark:bg-zinc-900 text-zinc-900 dark:text-zinc-100 p-4 overflow-hidden">
        <SettingsPane />
      </div>
    );
  }

  if (view === "session") {
    return (
      <div className="h-screen flex flex-col bg-white dark:bg-zinc-900 text-zinc-900 dark:text-zinc-100 p-4 overflow-hidden">
        <SessionDetail />
      </div>
    );
  }

  if (view === "hooks") {
    return (
      <div className="h-screen flex flex-col bg-white dark:bg-zinc-900 text-zinc-900 dark:text-zinc-100 p-4 overflow-hidden">
        <HooksPane />
      </div>
    );
  }

  return (
    <div className="h-screen flex flex-col bg-white dark:bg-zinc-900 text-zinc-900 dark:text-zinc-100 p-4 gap-4 overflow-hidden">
      <div className="flex items-center justify-between">
        <h1 className="text-lg font-semibold">Coach</h1>
        <div className="flex items-center gap-2">
          <ThemeToggle />
          <button
            onClick={() => setView("hooks")}
            className="text-xs px-2.5 py-1 rounded-md bg-zinc-100 dark:bg-zinc-800 text-zinc-500 hover:text-zinc-700 dark:hover:text-zinc-300 transition-colors flex items-center gap-1.5"
          >
            <span
              className={`w-1.5 h-1.5 rounded-full ${
                hookStatus?.installed
                  ? "bg-emerald-500"
                  : "bg-amber-500"
              }`}
            />
            Hooks
          </button>
          <button
            onClick={() => setView("settings")}
            className="text-xs px-2.5 py-1 rounded-md bg-zinc-100 dark:bg-zinc-800 text-zinc-500 hover:text-zinc-700 dark:hover:text-zinc-300 transition-colors"
          >
            Settings
          </button>
        </div>
      </div>

      <SessionList />
      <PriorityList />

      <div className="flex-1 min-h-0 flex flex-col">
        <ActivityLog />
      </div>

      <div className="text-xs text-zinc-400 dark:text-zinc-600 text-center flex items-center justify-center gap-2">
        <span>v{__APP_VERSION__}</span>
        <span>·</span>
        <span>localhost:{port}</span>
        <span>·</span>
        <span>{model.provider}/{model.model}</span>
      </div>
    </div>
  );
}
