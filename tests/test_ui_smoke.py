"""Smoke test: launch Coach, verify the UI renders (not blank white).

Run against an already-running Coach:
    uv run --with Pillow python tests/test_ui_smoke.py

Run including launch/teardown:
    uv run --with Pillow python tests/test_ui_smoke.py --launch
"""
import json
import os
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

COACH_BINARY = Path(__file__).resolve().parent.parent / "src-tauri" / "target" / "release" / "coach"
SCREENSHOT_DIR = Path("/tmp/coach_tests")
PORT = 7700
STARTUP_TIMEOUT = 10  # seconds


def wait_for_server(port: int, timeout: float) -> bool:
    """Poll the HTTP API until it responds."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            resp = urllib.request.urlopen(f"http://127.0.0.1:{port}/state", timeout=1)
            resp.read()
            return True
        except Exception:
            time.sleep(0.3)
    return False


def find_coach_window_id() -> int | None:
    """Find the Coach window number via CGWindowList."""
    import Quartz.CoreGraphics as CG  # type: ignore[import-untyped]

    windows = CG.CGWindowListCopyWindowInfo(
        CG.kCGWindowListOptionAll, CG.kCGNullWindowID
    )
    for w in windows:
        owner = w.get("kCGWindowOwnerName", "")
        title = w.get("kCGWindowName", "")
        layer = w.get("kCGWindowLayer", -1)
        if owner.lower() == "coach" and title == "Coach" and layer == 0:
            return w["kCGWindowNumber"]
    return None


def capture_window(window_id: int, path: Path) -> None:
    """Use macOS screencapture to grab a specific window by ID."""
    path.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        ["screencapture", "-x", "-o", "-l", str(window_id), str(path)],
        check=True,
    )


def analyze_image(path: Path) -> dict:
    """Compute pixel statistics for the screenshot."""
    from PIL import Image  # type: ignore[import-untyped]

    img = Image.open(path).convert("RGB")
    w, h = img.size
    pixels = list(img.getdata())
    total = len(pixels)

    white = sum(1 for r, g, b in pixels if r > 240 and g > 240 and b > 240)
    dark = sum(1 for r, g, b in pixels if r < 80 and g < 80 and b < 80)
    colored = sum(
        1 for r, g, b in pixels if max(r, g, b) - min(r, g, b) > 40
    )

    return {
        "size": f"{w}x{h}",
        "total_pixels": total,
        "white_pixels": white,
        "white_pct": round(100 * white / total, 1),
        "dark_pixels": dark,
        "dark_pct": round(100 * dark / total, 1),
        "colored_pixels": colored,
        "colored_pct": round(100 * colored / total, 1),
    }


def check_backend():
    """The HTTP API should return valid JSON with a sessions array."""
    print("--- check: backend responds ---")
    resp = urllib.request.urlopen(f"http://127.0.0.1:{PORT}/state", timeout=2)
    data = json.loads(resp.read())
    assert "sessions" in data and isinstance(data["sessions"], list)
    print(f"  OK: {len(data['sessions'])} session(s)")
    return data


def check_startup_logs(proc):
    """Read stderr lines and verify startup completed."""
    print("--- check: startup logs ---")
    # Non-blocking read of whatever stderr has so far
    import selectors
    sel = selectors.DefaultSelector()
    sel.register(proc.stderr, selectors.EVENT_READ)
    lines = []
    while sel.select(timeout=0.5):
        line = proc.stderr.readline()
        if not line:
            break
        lines.append(line.decode(errors="replace").rstrip())
        if len(lines) > 50:
            break
    sel.close()

    for line in lines:
        print(f"  {line}")

    coach_lines = [l for l in lines if "[coach]" in l]
    assert len(coach_lines) > 0, "No [coach] log lines on stderr"
    assert any("setup: complete" in l for l in coach_lines), (
        "Setup did not complete. Lines:\n" + "\n".join(lines)
    )
    print("  OK: setup complete")


def check_ui_not_blank():
    """The rendered window should contain visible content, not just white."""
    print("--- check: UI not blank ---")
    wid = find_coach_window_id()
    assert wid is not None, "Coach window not found on screen"

    path = SCREENSHOT_DIR / "smoke.png"
    capture_window(wid, path)
    stats = analyze_image(path)

    print(f"  screenshot: {path}")
    print(f"  {json.dumps(stats, indent=2)}")

    ok = stats["white_pct"] < 95 and stats["dark_pixels"] > 50
    if ok:
        print("  OK: UI has visible content")
    else:
        print(f"  FAIL: UI appears blank (white={stats['white_pct']}%, dark_px={stats['dark_pixels']})")
    return ok, stats


def main():
    launch = "--launch" in sys.argv
    proc = None
    failures = []

    try:
        if launch:
            print(f"Launching {COACH_BINARY}")
            assert COACH_BINARY.exists(), f"Binary not found: {COACH_BINARY}"
            proc = subprocess.Popen(
                [str(COACH_BINARY)],
                stderr=subprocess.PIPE,
                stdout=subprocess.PIPE,
            )

        if not wait_for_server(PORT, STARTUP_TIMEOUT):
            print("FAIL: server did not start")
            sys.exit(1)

        time.sleep(3)  # give the window time to render

        # 1. Backend
        try:
            check_backend()
        except Exception as e:
            failures.append(f"backend: {e}")
            print(f"  FAIL: {e}")

        # 2. Startup logs (only if we launched)
        if proc:
            try:
                check_startup_logs(proc)
            except Exception as e:
                failures.append(f"logs: {e}")
                print(f"  FAIL: {e}")

        # 3. UI screenshot
        try:
            ok, stats = check_ui_not_blank()
            if not ok:
                failures.append(f"UI blank: white={stats['white_pct']}%")
        except Exception as e:
            failures.append(f"screenshot: {e}")
            print(f"  FAIL: {e}")

    finally:
        if proc:
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()

    print()
    if failures:
        print(f"FAILED ({len(failures)}):")
        for f in failures:
            print(f"  - {f}")
        sys.exit(1)
    else:
        print("ALL CHECKS PASSED")


if __name__ == "__main__":
    main()
