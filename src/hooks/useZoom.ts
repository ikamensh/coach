import { useEffect } from "react";

const STEP = 0.1;
const MIN = 0.5;
const MAX = 2.0;
const KEY = "coach-zoom";

function getZoom(): number {
  const v = localStorage.getItem(KEY);
  return v ? parseFloat(v) : 1.0;
}

function applyZoom(level: number) {
  const clamped = Math.min(MAX, Math.max(MIN, Math.round(level * 10) / 10));
  localStorage.setItem(KEY, String(clamped));
  document.documentElement.style.zoom = String(clamped);
}

export function useZoom() {
  useEffect(() => {
    applyZoom(getZoom());

    function onKey(e: KeyboardEvent) {
      if (!(e.metaKey || e.ctrlKey)) return;

      if (e.key === "=" || e.key === "+") {
        e.preventDefault();
        applyZoom(getZoom() + STEP);
      } else if (e.key === "-") {
        e.preventDefault();
        applyZoom(getZoom() - STEP);
      } else if (e.key === "0") {
        e.preventDefault();
        applyZoom(1.0);
      }
    }

    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);
}
