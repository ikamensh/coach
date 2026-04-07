import { useState } from "react";
import { useCoachStore } from "../store/useCoachStore";
import { TopBar } from "./TopBar";

const HOOK_DESCRIPTIONS: Record<string, string> = {
  PermissionRequest: "Auto-approves tool use when you're away",
  Stop: "When away, tells Claude to keep working with your priorities",
  PostToolUse: "Tracks session activity (observation only)",
  UserPromptSubmit: "Marks user turns on the activity timeline",
  SessionStart: "Detects /clear, /resume, /compact instantly so the session row swaps to the new conversation without waiting for the next tool call",
};

const CURSOR_HOOK_DESCRIPTIONS: Record<string, string> = {
  sessionStart: "Session boundaries (new agent run / conversation)",
  beforeSubmitPrompt: "User prompt turns on the activity timeline",
  beforeShellExecution: "Auto-approves shell when you're away",
  beforeMCPExecution: "Auto-approves MCP tool use when you're away",
  afterShellExecution: "Tracks shell activity (rules + observer)",
  afterMCPExecution: "Tracks MCP tool activity",
  afterFileEdit: "Tracks edits (rules + observer)",
  stop: "When away, stop / continue with your priorities",
};

export function HooksPane() {
  const hookStatus = useCoachStore((s) => s.hookStatus);
  const cursorHookStatus = useCoachStore((s) => s.cursorHookStatus);
  const pathStatus = useCoachStore((s) => s.pathStatus);
  const setView = useCoachStore((s) => s.setView);
  const installHooks = useCoachStore((s) => s.installHooks);
  const uninstallHooks = useCoachStore((s) => s.uninstallHooks);
  const installCursorHooks = useCoachStore((s) => s.installCursorHooks);
  const uninstallCursorHooks = useCoachStore((s) => s.uninstallCursorHooks);
  const installPath = useCoachStore((s) => s.installPath);
  const uninstallPath = useCoachStore((s) => s.uninstallPath);
  const [pathError, setPathError] = useState<string | null>(null);

  const handleInstallPath = async () => {
    setPathError(null);
    try {
      await installPath();
    } catch (e) {
      setPathError(String(e));
    }
  };

  const handleUninstallPath = async () => {
    setPathError(null);
    try {
      await uninstallPath();
    } catch (e) {
      setPathError(String(e));
    }
  };

  return (
    <div className="flex flex-col gap-4 h-full overflow-y-auto">
      <TopBar title="Setup" onBack={() => setView("main")} />

      <p className="text-sm text-zinc-500 dark:text-zinc-400">
        Coach can integrate with{" "}
        <span className="font-medium text-zinc-700 dark:text-zinc-300">
          Claude Code
        </span>{" "}
        (HTTP hooks in <code className="font-mono text-xs">~/.claude/settings.json</code>) and{" "}
        <span className="font-medium text-zinc-700 dark:text-zinc-300">
          Cursor Agent
        </span>{" "}
        (a small shim script{" "}
        <code className="font-mono text-xs">~/.cursor/coach-cursor-hook.sh</code>{" "}
        referenced from <code className="font-mono text-xs">~/.cursor/hooks.json</code> —
        Cursor's hook runner blocks raw <code className="font-mono text-xs">curl</code>{" "}
        commands). Existing entries are preserved.
      </p>

      {hookStatus && (
        <>
          <section>
            <h2 className="text-sm font-medium text-zinc-400 mb-2 uppercase tracking-wide">
              Claude Code — file
            </h2>
            <p className="text-xs font-mono text-zinc-500 dark:text-zinc-400 bg-zinc-100 dark:bg-zinc-800 px-3 py-2 rounded">
              {hookStatus.path}
            </p>
          </section>

          <section>
            <h2 className="text-sm font-medium text-zinc-400 mb-2 uppercase tracking-wide">
              Claude Code — hooks
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
              Install Claude hooks
            </button>
          )}

          {hookStatus.installed && (
            <div className="flex items-center justify-between">
              <p className="text-sm text-emerald-600 dark:text-emerald-400">
                All Claude hooks installed.
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

      {cursorHookStatus && (
        <div className="border-t border-zinc-200 dark:border-zinc-800 pt-4 mt-2">
          <section>
            <h2 className="text-sm font-medium text-zinc-400 mb-2 uppercase tracking-wide">
              Cursor Agent — file
            </h2>
            <p className="text-xs font-mono text-zinc-500 dark:text-zinc-400 bg-zinc-100 dark:bg-zinc-800 px-3 py-2 rounded">
              {cursorHookStatus.path}
            </p>
          </section>

          <section className="mt-3">
            <h2 className="text-sm font-medium text-zinc-400 mb-2 uppercase tracking-wide">
              Cursor Agent — hooks
            </h2>
            <div className="space-y-3">
              {cursorHookStatus.hooks.map((hook) => (
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
                  <p className="text-xs text-zinc-400 dark:text-zinc-500 ml-4 break-all">
                    {CURSOR_HOOK_DESCRIPTIONS[hook.event] ?? hook.url}
                  </p>
                </div>
              ))}
            </div>
          </section>

          {!cursorHookStatus.installed && (
            <button
              onClick={installCursorHooks}
              className="mt-3 px-4 py-2 text-sm font-medium bg-emerald-500/20 text-emerald-600 dark:text-emerald-400 rounded-lg hover:bg-emerald-500/30 transition-colors"
            >
              Install Cursor hooks
            </button>
          )}

          {cursorHookStatus.installed && (
            <div className="flex items-center justify-between mt-3">
              <p className="text-sm text-emerald-600 dark:text-emerald-400">
                All Cursor hooks installed.
              </p>
              <button
                onClick={uninstallCursorHooks}
                className="px-3 py-1.5 text-sm font-medium text-zinc-500 dark:text-zinc-400 hover:text-red-600 dark:hover:text-red-400 rounded-lg hover:bg-red-500/10 transition-colors"
              >
                Uninstall
              </button>
            </div>
          )}
        </div>
      )}

      <div className="border-t border-zinc-200 dark:border-zinc-800 pt-4 mt-2">
        <h2 className="text-sm font-medium text-zinc-400 mb-2 uppercase tracking-wide">
          CLI on PATH
        </h2>
        <p className="text-xs text-zinc-500 dark:text-zinc-400 mb-3">
          Install a <code className="font-mono">coach</code> shim on your{" "}
          <code className="font-mono">$PATH</code> so the same binary that runs
          this app can be invoked from a terminal:{" "}
          <code className="font-mono">coach hooks install</code>,{" "}
          <code className="font-mono">coach mode away</code>, etc.
        </p>

        {pathStatus && (
          <div className="space-y-2 mb-3">
            <p className="text-xs font-mono text-zinc-500 dark:text-zinc-400 bg-zinc-100 dark:bg-zinc-800 px-3 py-2 rounded">
              {pathStatus.install_path}
            </p>
            <div className="flex flex-wrap gap-x-4 gap-y-1 text-xs text-zinc-500 dark:text-zinc-400">
              <span className="flex items-center gap-1.5">
                <span
                  className={`w-2 h-2 rounded-full ${
                    pathStatus.installed ? "bg-emerald-500" : "bg-zinc-300 dark:bg-zinc-600"
                  }`}
                />
                {pathStatus.installed ? "installed" : "not installed"}
              </span>
              {pathStatus.installed && !pathStatus.matches_current_exe && (
                <span className="text-amber-600 dark:text-amber-400">
                  stale — points at a different binary
                </span>
              )}
              <span className="flex items-center gap-1.5">
                <span
                  className={`w-2 h-2 rounded-full ${
                    pathStatus.on_path ? "bg-emerald-500" : "bg-amber-500"
                  }`}
                />
                {pathStatus.on_path ? "directory on $PATH" : "directory NOT on $PATH"}
              </span>
            </div>
          </div>
        )}

        {pathError && (
          <p className="text-xs text-red-500 mb-2">{pathError}</p>
        )}

        <div className="flex gap-2">
          {(!pathStatus?.installed || !pathStatus.matches_current_exe) && (
            <button
              onClick={handleInstallPath}
              className="px-4 py-2 text-sm font-medium bg-emerald-500/20 text-emerald-600 dark:text-emerald-400 rounded-lg hover:bg-emerald-500/30 transition-colors"
            >
              {pathStatus?.installed ? "Reinstall" : "Install on PATH"}
            </button>
          )}
          {pathStatus?.installed && (
            <button
              onClick={handleUninstallPath}
              className="px-3 py-1.5 text-sm font-medium text-zinc-500 dark:text-zinc-400 hover:text-red-600 dark:hover:text-red-400 rounded-lg hover:bg-red-500/10 transition-colors"
            >
              Uninstall
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
