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

/**
 * Derive the `~/.claude/projects/...jsonl` transcript path for a Claude
 * Code session. Mirrors the Rust scanner's `mangle_cwd` — Claude Code
 * slugifies the launch cwd by replacing every non-alphanumeric byte
 * with `-`, so `/Users/alice/work_2024` lives under
 * `-Users-alice-work-2024`.
 *
 * Falls back to `bootstrapped_session_id` when `session_id` is empty
 * — scanner-discovered sessions don't get a real `session_id` until
 * the first live hook lands, but the JSONL transcript is already on
 * disk under the id the scanner read at bootstrap time.
 *
 * Returns null when the path can't be formed: non-Claude client
 * (Cursor/Codex use different storage), missing cwd, or neither id
 * available.
 */
export function jsonlPath(session: {
  client: string;
  session_id: string;
  bootstrapped_session_id?: string | null;
  cwd: string | null;
}): string | null {
  if (session.client !== "claude") return null;
  const sid = session.session_id || session.bootstrapped_session_id || "";
  if (!sid) return null;
  if (!session.cwd) return null;
  const mangled = session.cwd.replace(/[^a-zA-Z0-9]/g, "-");
  return `~/.claude/projects/${mangled}/${sid}.jsonl`;
}
