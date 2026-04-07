/**
 * Replace `/Users/<name>/` with `~/` so paths are scannable in the UI.
 * Returns "unknown" for null/empty input. Non-home paths pass through.
 */
export function abbreviateCwd(cwd: string | null | undefined): string {
  if (!cwd) return "unknown";
  const home = "/Users/";
  if (cwd.startsWith(home)) {
    const rest = cwd.slice(home.length);
    const slashIdx = rest.indexOf("/");
    if (slashIdx >= 0) return "~" + rest.slice(slashIdx);
    return "~";
  }
  return cwd;
}
