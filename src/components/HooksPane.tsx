import { useCoachStore } from "../store/useCoachStore";

const HOOK_DESCRIPTIONS: Record<string, string> = {
  PermissionRequest: "Auto-approves tool use when you're away",
  Stop: "When away, tells Claude to keep working with your priorities",
  PostToolUse: "Tracks session activity (observation only)",
  UserPromptSubmit: "Marks user turns on the activity timeline",
  SessionStart: "Detects /clear, /resume, /compact instantly so the session row swaps to the new conversation without waiting for the next tool call",
};

export function HooksPane() {
  const hookStatus = useCoachStore((s) => s.hookStatus);
  const setView = useCoachStore((s) => s.setView);
  const installHooks = useCoachStore((s) => s.installHooks);
  const uninstallHooks = useCoachStore((s) => s.uninstallHooks);

  return (
    <div className="flex flex-col gap-4 h-full">
      <div className="flex items-center justify-between">
        <h1 className="text-lg font-semibold text-zinc-800 dark:text-zinc-100">
          Hook Setup
        </h1>
        <button
          onClick={() => setView("main")}
          className="text-sm text-zinc-500 hover:text-zinc-700 dark:hover:text-zinc-300"
        >
          Back
        </button>
      </div>

      <p className="text-sm text-zinc-500 dark:text-zinc-400">
        Coach uses{" "}
        <span className="font-medium text-zinc-700 dark:text-zinc-300">
          Claude Code hooks
        </span>{" "}
        (HTTP type) to monitor and guide sessions. Hooks are added to your
        Claude Code user settings — existing hooks are preserved.
      </p>

      {hookStatus && (
        <>
          <section>
            <h2 className="text-sm font-medium text-zinc-400 mb-2 uppercase tracking-wide">
              Settings File
            </h2>
            <p className="text-xs font-mono text-zinc-500 dark:text-zinc-400 bg-zinc-100 dark:bg-zinc-800 px-3 py-2 rounded">
              {hookStatus.path}
            </p>
          </section>

          <section>
            <h2 className="text-sm font-medium text-zinc-400 mb-2 uppercase tracking-wide">
              Hooks
            </h2>
            <div className="space-y-3">
              {hookStatus.hooks.map((hook) => (
                <div key={hook.event} className="flex flex-col gap-0.5">
                  <div className="flex items-center gap-2">
                    <span
                      className={`w-2 h-2 rounded-full flex-shrink-0 ${
                        hook.installed
                          ? "bg-emerald-500"
                          : "bg-zinc-300 dark:bg-zinc-600"
                      }`}
                    />
                    <span className="text-sm font-medium text-zinc-700 dark:text-zinc-300">
                      {hook.event}
                    </span>
                  </div>
                  <p className="text-xs text-zinc-400 dark:text-zinc-500 ml-4">
                    {HOOK_DESCRIPTIONS[hook.event] ?? hook.url}
                  </p>
                </div>
              ))}
            </div>
          </section>

          {!hookStatus.installed && (
            <button
              onClick={installHooks}
              className="px-4 py-2 text-sm font-medium bg-emerald-500/20 text-emerald-600 dark:text-emerald-400 rounded-lg hover:bg-emerald-500/30 transition-colors"
            >
              Install Hooks
            </button>
          )}

          {hookStatus.installed && (
            <div className="flex items-center justify-between">
              <p className="text-sm text-emerald-600 dark:text-emerald-400">
                All hooks installed.
              </p>
              <button
                onClick={uninstallHooks}
                className="px-3 py-1.5 text-sm font-medium text-zinc-500 dark:text-zinc-400 hover:text-red-600 dark:hover:text-red-400 rounded-lg hover:bg-red-500/10 transition-colors"
              >
                Uninstall
              </button>
            </div>
          )}
        </>
      )}
    </div>
  );
}
