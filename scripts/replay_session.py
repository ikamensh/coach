#!/usr/bin/env python3
"""Replay a Claude Code session through Coach to find when it would first intervene.

Loads a session JSONL from Claude Code history, reconstructs the hook events
that would have fired, and evaluates them against Coach's intervention logic.

Usage:
    python scripts/replay_session.py <session_id>
    python scripts/replay_session.py <session_id> --mode away
    python scripts/replay_session.py --latest
    python scripts/replay_session.py --list
    python scripts/replay_session.py <session_id> --http http://localhost:7700
"""

from __future__ import annotations

import argparse
import json
import sys
from dataclasses import dataclass, field
from pathlib import Path

CLAUDE_DIR = Path.home() / ".claude"

# ── Data types ───────────────────────────────────────────────────────────


@dataclass
class HookEvent:
    kind: str          # PermissionRequest | PostToolUse | Stop
    tool_name: str
    timestamp: str
    message_index: int
    summary: str       # human-readable one-liner


@dataclass
class Intervention:
    event: HookEvent
    action: str        # auto-approved | blocked
    message: str       # what Coach would inject (empty for passthrough)


# ── Session discovery ────────────────────────────────────────────────────


def find_session(session_id: str) -> Path | None:
    """Find a session JSONL by exact or prefix match."""
    projects = CLAUDE_DIR / "projects"
    if not projects.exists():
        return None
    for project_dir in sorted(projects.iterdir()):
        if not project_dir.is_dir():
            continue
        exact = project_dir / f"{session_id}.jsonl"
        if exact.exists():
            return exact
        for f in project_dir.glob("*.jsonl"):
            if f.stem.startswith(session_id):
                return f
    return None


def list_sessions(limit: int = 20) -> list[dict]:
    """List recent sessions, newest first."""
    projects = CLAUDE_DIR / "projects"
    if not projects.exists():
        return []
    sessions = []
    for project_dir in projects.iterdir():
        if not project_dir.is_dir():
            continue
        for f in project_dir.glob("*.jsonl"):
            topic = _extract_topic(f)
            sessions.append({
                "id": f.stem,
                "project": project_dir.name,
                "mtime": f.stat().st_mtime,
                "size": f.stat().st_size,
                "topic": topic,
            })
    sessions.sort(key=lambda s: s["mtime"], reverse=True)
    return sessions[:limit]


def latest_session() -> Path | None:
    """Return the most recently modified session JSONL."""
    sessions = list_sessions(1)
    if not sessions:
        return None
    return find_session(sessions[0]["id"])


def _extract_topic(path: Path) -> str:
    """Quick topic extraction: first user text, truncated."""
    with open(path) as f:
        for line in f:
            try:
                entry = json.loads(line)
            except json.JSONDecodeError:
                continue
            if entry.get("type") != "user":
                continue
            content = entry.get("message", {}).get("content", "")
            if isinstance(content, str) and content.strip():
                return content.strip()[:80]
            if isinstance(content, list):
                for b in content:
                    if isinstance(b, dict) and b.get("type") == "text":
                        text = (b.get("text") or "").strip()
                        if text:
                            return text[:80]
    return ""


# ── Message parsing ──────────────────────────────────────────────────────


def load_messages(path: Path) -> list[dict]:
    messages = []
    with open(path) as f:
        for line in f:
            stripped = line.strip()
            if not stripped:
                continue
            try:
                messages.append(json.loads(stripped))
            except json.JSONDecodeError:
                continue
    return messages


def _tool_summary(block: dict) -> str:
    """Brief description of a tool_use block's input."""
    inp = block.get("input", {})
    if "command" in inp:
        return inp["command"][:80]
    if "file_path" in inp:
        return inp["file_path"]
    if "pattern" in inp:
        return f'pattern="{inp["pattern"]}"'
    return str(inp)[:80]


def extract_hook_events(messages: list[dict]) -> list[HookEvent]:
    """Walk through a session transcript and reconstruct the hook event sequence.

    Extracts Stop events (end_turn) and PostToolUse observations.
    PermissionRequest is excluded — auto-approving tools is uninteresting
    for evaluating Coach's intervention quality.
    """
    events: list[HookEvent] = []

    for i, msg in enumerate(messages):
        if msg.get("type") != "assistant":
            continue

        ts = msg.get("timestamp", "")
        content = msg.get("message", {}).get("content", [])
        stop_reason = msg.get("message", {}).get("stop_reason", "")

        if not isinstance(content, list):
            continue

        for block in content:
            if not isinstance(block, dict) or block.get("type") != "tool_use":
                continue
            tool = block.get("name", "unknown")
            summary = _tool_summary(block)

            events.append(HookEvent(
                kind="PostToolUse",
                tool_name=tool,
                timestamp=ts,
                message_index=i,
                summary=f"{tool}: {summary}",
            ))

        if stop_reason == "end_turn":
            events.append(HookEvent(
                kind="Stop",
                tool_name="",
                timestamp=ts,
                message_index=i,
                summary="end_turn",
            ))

    return events


# ── Evaluators ───────────────────────────────────────────────────────────


def evaluate_mode_based(
    event: HookEvent,
    mode: str,
    priorities: list[str],
    last_stop_blocked: list[bool],  # mutable — tracks cooldown
) -> Intervention | None:
    """Replicate Coach's current mode-based intervention logic."""
    if mode == "present":
        return None

    if event.kind == "Stop":
        # Simulate 15s cooldown: block first, allow subsequent
        if last_stop_blocked[0]:
            return None  # cooldown — passthrough
        last_stop_blocked[0] = True
        ptext = ", ".join(f"{i+1}. {p}" for i, p in enumerate(priorities))
        msg = (
            f"User is away. Continue working autonomously. "
            f"If you need to make a decision, use these priorities "
            f"(highest first): {ptext}. "
            f"If you were asking whether to proceed — yes, proceed."
        )
        return Intervention(event=event, action="blocked", message=msg)

    return None


def evaluate_via_http(
    event: HookEvent,
    session_id: str,
    cwd: str,
    base_url: str,
) -> Intervention | None:
    """Send the event to a running Coach server and check the response."""
    import urllib.request

    endpoint = {
        "PermissionRequest": "permission-request",
        "PostToolUse": "post-tool-use",
        "Stop": "stop",
    }[event.kind]

    payload = json.dumps({
        "sessionId": session_id,
        "hookEventName": event.kind,
        "toolName": event.tool_name or None,
        "cwd": cwd,
    }).encode()

    url = f"{base_url.rstrip('/')}/hook/{endpoint}"
    req = urllib.request.Request(url, data=payload, method="POST")
    req.add_header("Content-Type", "application/json")

    try:
        resp = urllib.request.urlopen(req, timeout=5)
        body = json.loads(resp.read())
    except Exception as e:
        print(f"    (http error: {e})", file=sys.stderr)
        return None

    hook_output = body.get("hookSpecificOutput")
    if not hook_output:
        return None

    decision = hook_output.get("decision", "")
    if isinstance(decision, dict):
        behavior = decision.get("behavior", "")
        if behavior == "allow":
            return Intervention(event=event, action="auto-approved", message="")
    elif decision == "block":
        ctx = hook_output.get("additionalContext", "")
        return Intervention(event=event, action="blocked", message=ctx)

    return None


# ── Replay ───────────────────────────────────────────────────────────────


def replay(
    session_path: Path,
    mode: str = "away",
    priorities: list[str] | None = None,
    http_url: str | None = None,
    verbose: bool = False,
) -> Intervention | None:
    """Replay a session and return the first intervention (or None)."""
    if priorities is None:
        priorities = ["Code simplicity", "Correctness"]

    messages = load_messages(session_path)
    events = extract_hook_events(messages)

    user_msgs = [m for m in messages if m.get("type") == "user"]
    asst_msgs = [m for m in messages if m.get("type") == "assistant"]
    topic = _extract_topic(session_path)
    session_id = session_path.stem
    cwd = ""
    for m in messages:
        if m.get("cwd"):
            cwd = m["cwd"]
            break

    print(f"Session:  {session_id}")
    print(f"Topic:    {topic}")
    print(f"Messages: {len(messages)} ({len(user_msgs)} user, {len(asst_msgs)} assistant)")
    print(f"Events:   {len(events)} hook events")
    if http_url:
        print(f"Target:   {http_url}")
    else:
        print(f"Mode:     {mode}")
    print()

    last_stop_blocked = [False]  # mutable for cooldown tracking

    for i, event in enumerate(events):
        if http_url:
            intervention = evaluate_via_http(event, session_id, cwd, http_url)
        else:
            intervention = evaluate_mode_based(event, mode, priorities, last_stop_blocked)

        tag = f"[{i+1:3d}/{len(events)}]"
        if intervention:
            print(f"  {tag} {event.kind}({event.summary})")
            print(f"        -> {intervention.action}")
            if intervention.message:
                print(f"        message: {intervention.message[:200]}")
            print(f"\n  First intervention at event {i+1}/{len(events)}")
            if event.timestamp:
                print(f"  Timestamp: {event.timestamp}")
            return intervention

        if verbose:
            print(f"  {tag} {event.kind}({event.summary}) -> passthrough")

    print("  No intervention would have occurred.")
    return None


# ── CLI ──────────────────────────────────────────────────────────────────


def main():
    parser = argparse.ArgumentParser(description="Replay a Claude Code session through Coach.")
    parser.add_argument("session", nargs="?", help="Session ID or prefix")
    parser.add_argument("--list", action="store_true", help="List recent sessions")
    parser.add_argument("--latest", action="store_true", help="Use the most recent session")
    parser.add_argument("--mode", default="away", choices=["away", "present"],
                        help="Coach mode to simulate (default: away)")
    parser.add_argument("--priorities", nargs="*",
                        help="Priorities to inject (default: Code simplicity, Correctness)")
    parser.add_argument("--http", metavar="URL",
                        help="Send events to a running Coach server instead of simulating locally")
    parser.add_argument("-v", "--verbose", action="store_true",
                        help="Show all events, not just the first intervention")
    args = parser.parse_args()

    if args.list:
        sessions = list_sessions(30)
        if not sessions:
            print("No sessions found.")
            return
        for s in sessions:
            sid = s["id"][:12]
            proj = s["project"].replace("-Users-ikamen-", "~/")
            size = s["size"] // 1024
            print(f"  {sid}...  {size:5d}K  {proj:40s}  {s['topic']}")
        return

    if args.latest:
        path = latest_session()
    elif args.session:
        path = find_session(args.session)
    else:
        parser.print_help()
        return

    if not path:
        print(f"Session not found: {args.session or '(latest)'}", file=sys.stderr)
        sys.exit(1)

    replay(
        path,
        mode=args.mode,
        priorities=args.priorities,
        http_url=args.http,
        verbose=args.verbose,
    )


if __name__ == "__main__":
    main()
