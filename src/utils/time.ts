/** Relative time, e.g. "5s ago", "12m ago", "2h ago". */
export function timeAgo(iso: string): string {
  const seconds = Math.floor((Date.now() - new Date(iso).getTime()) / 1000);
  if (seconds < 60) return `${seconds}s ago`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  return `${hours}h ago`;
}

/** Compact duration: "23m", "1h 15m", "2h". */
export function formatDuration(secs: number): string {
  if (secs < 60) return `${secs}s`;
  const minutes = Math.floor(secs / 60);
  const hours = Math.floor(minutes / 60);
  const remainMinutes = minutes % 60;
  if (hours === 0) return `${minutes}m`;
  if (remainMinutes === 0) return `${hours}h`;
  return `${hours}h ${remainMinutes}m`;
}

/** Wall-clock time, e.g. "14:23". */
export function formatTime(iso: string): string {
  return new Date(iso).toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
  });
}
