import { Component, useEffect, type ReactNode } from "react";
import { useCoachStore } from "./store/useCoachStore";
import { useZoom } from "./hooks/useZoom";
import { SessionList } from "./components/SessionList";
import { PriorityList } from "./components/PriorityList";
import { ActivityLog } from "./components/ActivityLog";
import { ThemeToggle } from "./components/ThemeToggle";
import { SettingsPane } from "./components/SettingsPane";
import { HooksPane } from "./components/HooksPane";
import { SessionDetail } from "./components/SessionDetail";
import { DevPane } from "./components/DevPane";

function ErrorDisplay({ title, error }: { title: string; error: string }) {
  return (
    <div className="h-screen flex items-center justify-center bg-white dark:bg-zinc-900 p-6">
      <div className="max-w-lg w-full">
        <div className="text-red-600 dark:text-red-400 font-semibold text-sm mb-2">
          {title}
        </div>
        <pre className="text-xs text-red-500 dark:text-red-400 bg-red-50 dark:bg-red-950 border border-red-200 dark:border-red-800 rounded-md p-3 whitespace-pre-wrap break-words font-mono">
          {error}
        </pre>
      </div>
    </div>
  );
}

class ErrorBoundary extends Component<
  { children: ReactNode },
  { error: string | null }
> {
  state = { error: null as string | null };

  static getDerivedStateFromError(error: Error) {
    return { error: `${error.message}\n\n${error.stack}` };
  }

  render() {
    if (this.state.error) {
      return (
        <ErrorDisplay title="Render error" error={this.state.error} />
      );
    }
    return this.props.children;
  }
}

function VersionFooter() {
  return (
    <div className="text-[10px] text-zinc-400 dark:text-zinc-600 text-center pt-2">
      v{__APP_VERSION__}
    </div>
  );
}

function AppInner() {
  const init = useCoachStore((s) => s.init);
  const initialized = useCoachStore((s) => s.initialized);
  const initError = useCoachStore((s) => s.initError);
  const view = useCoachStore((s) => s.view);
  const setView = useCoachStore((s) => s.setView);
  const port = useCoachStore((s) => s.port);
  const model = useCoachStore((s) => s.model);
  const hookStatus = useCoachStore((s) => s.hookStatus);

  useZoom();

  useEffect(() => {
    init();
  }, [init]);

  if (initError) {
    return <ErrorDisplay title="Init failed" error={initError} />;
  }

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
        <VersionFooter />
      </div>
    );
  }

  if (view === "session") {
    return (
      <div className="h-screen flex flex-col bg-white dark:bg-zinc-900 text-zinc-900 dark:text-zinc-100 p-4 overflow-hidden">
        <SessionDetail />
        <VersionFooter />
      </div>
    );
  }

  if (view === "hooks") {
    return (
      <div className="h-screen flex flex-col bg-white dark:bg-zinc-900 text-zinc-900 dark:text-zinc-100 p-4 overflow-hidden">
        <HooksPane />
        <VersionFooter />
      </div>
    );
  }

  if (view === "dev") {
    return (
      <div className="h-screen flex flex-col bg-white dark:bg-zinc-900 text-zinc-900 dark:text-zinc-100 p-4 overflow-hidden">
        <DevPane />
        <VersionFooter />
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
          {import.meta.env.DEV && (
            <button
              onClick={() => setView("dev")}
              className="text-xs px-2.5 py-1 rounded-md bg-zinc-100 dark:bg-zinc-800 text-zinc-500 hover:text-zinc-700 dark:hover:text-zinc-300 transition-colors"
            >
              Replay
            </button>
          )}
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

export default function App() {
  return (
    <ErrorBoundary>
      <AppInner />
    </ErrorBoundary>
  );
}
