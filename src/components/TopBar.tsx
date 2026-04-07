import { useEffect, type ReactNode } from "react";
import { useCoachStore } from "../store/useCoachStore";
import { ThemeToggle } from "./ThemeToggle";

interface TopBarProps {
  /** Page title shown next to the back arrow. */
  title?: string;
  /** When provided, shows the back arrow and binds Esc to it. */
  onBack?: () => void;
  /** Page-specific actions rendered just before the persistent nav buttons. */
  rightSlot?: ReactNode;
}

function isEditableTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  const tag = target.tagName;
  return tag === "INPUT" || tag === "TEXTAREA" || target.isContentEditable;
}

function BackArrow() {
  return (
    <svg
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      className="w-4 h-4"
      aria-hidden="true"
    >
      <path d="M19 12H5" />
      <path d="M12 19l-7-7 7-7" />
    </svg>
  );
}

export function TopBar({ title, onBack, rightSlot }: TopBarProps) {
  const view = useCoachStore((s) => s.view);
  const setView = useCoachStore((s) => s.setView);
  const hookStatus = useCoachStore((s) => s.hookStatus);

  useEffect(() => {
    if (!onBack) return;
    const handleKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      // Let Esc cancel input editing first; back nav happens on a second press.
      if (isEditableTarget(document.activeElement)) {
        (document.activeElement as HTMLElement).blur();
        return;
      }
      e.preventDefault();
      onBack();
    };
    window.addEventListener("keydown", handleKey);
    return () => window.removeEventListener("keydown", handleKey);
  }, [onBack]);

  const navButtonClass = (active: boolean) =>
    `text-xs px-2.5 py-1 rounded-md transition-colors flex items-center gap-1.5 ${
      active
        ? "bg-zinc-200 dark:bg-zinc-700 text-zinc-800 dark:text-zinc-200"
        : "bg-zinc-100 dark:bg-zinc-800 text-zinc-500 hover:text-zinc-700 dark:hover:text-zinc-300"
    }`;

  return (
    <div className="flex items-center justify-between flex-shrink-0 gap-2">
      <div className="flex items-center gap-2 min-w-0">
        {onBack && (
          <button
            onClick={onBack}
            aria-label="Back"
            title="Back (Esc)"
            className="text-zinc-500 hover:text-zinc-800 dark:hover:text-zinc-200 hover:bg-zinc-100 dark:hover:bg-zinc-800 rounded-md p-1 flex-shrink-0 transition-colors"
          >
            <BackArrow />
          </button>
        )}
        {title && (
          <h1 className="text-lg font-semibold text-zinc-800 dark:text-zinc-100 truncate">
            {title}
          </h1>
        )}
      </div>
      <div className="flex items-center gap-2 flex-shrink-0">
        {rightSlot}
        <ThemeToggle />
        <button
          onClick={() => setView("hooks")}
          className={navButtonClass(view === "hooks")}
        >
          <span
            className={`w-1.5 h-1.5 rounded-full ${
              hookStatus?.installed ? "bg-emerald-500" : "bg-amber-500"
            }`}
          />
          Hooks
        </button>
        {/* TODO: gate behind import.meta.env.DEV again once we're out of active testing */}
        <button
          onClick={() => setView("dev")}
          className={navButtonClass(view === "dev")}
        >
          Replay
        </button>
        <button
          onClick={() => setView("settings")}
          className={navButtonClass(view === "settings")}
        >
          Settings
        </button>
      </div>
    </div>
  );
}
