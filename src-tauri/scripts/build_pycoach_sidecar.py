#!/usr/bin/env python3
"""Build the pycoach Python sidecar as a single-file binary suitable for
embedding in the Tauri bundle as an `externalBin`.

Output:
  src-tauri/binaries/pycoach-<rustc-host-triple>[.exe]

Tauri's bundler appends the host triple to externalBin entries when
matching files on disk and strips the suffix in the produced bundle, so
at runtime the sidecar lives next to the main `Coach` executable as
plain `pycoach` (or `pycoach.exe` on Windows). The runtime resolver in
`src-tauri/src/pycoach.rs` probes both names.

Requirements:
  * `uv` on PATH (manages pycoach's venv + installs PyInstaller into it)
  * `rustc` on PATH (used to detect the host triple)
  * The pycoach checkout at ../../pycoach (sibling of this repo)

Usage:
  python src-tauri/scripts/build_pycoach_sidecar.py

After running, build the desktop app with the sidecar wired in:
  npm run tauri build -- -f pycoach -c src-tauri/tauri.conf.pycoach.json

Cross-platform: this script runs unmodified on Linux, macOS, and Windows
(no shell required). PyInstaller itself does not cross-compile, so each
target platform needs its own builder.

Notes:
  * The first build is slow (resolves the dep tree); subsequent builds
    re-use PyInstaller's cache and are much faster.
  * If a built binary fails at runtime with ModuleNotFoundError, add
    the missing module to the `--collect-submodules` / `--copy-metadata`
    list below.
"""

from __future__ import annotations

import os
import platform
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

SCRIPT_PATH = Path(__file__).resolve()
SRC_TAURI_DIR = SCRIPT_PATH.parent.parent
COACH_DIR = SRC_TAURI_DIR.parent
WORKSPACE_DIR = COACH_DIR.parent
PYCOACH_DIR = WORKSPACE_DIR / "pycoach"

EXE_SUFFIX = ".exe" if platform.system() == "Windows" else ""


def die(msg: str) -> None:
    print(f"error: {msg}", file=sys.stderr)
    sys.exit(1)


def require(tool: str) -> None:
    if shutil.which(tool) is None:
        die(f"`{tool}` is required on PATH")


def host_triple() -> str:
    out = subprocess.check_output(["rustc", "-vV"], text=True)
    for line in out.splitlines():
        if line.startswith("host:"):
            return line.split(":", 1)[1].strip()
    die("could not parse host triple from `rustc -vV`")
    raise SystemExit  # for the type checker; die() already exits


def main() -> int:
    if not PYCOACH_DIR.is_dir():
        die(f"pycoach checkout not found at {PYCOACH_DIR}")
    require("uv")
    require("rustc")

    triple = host_triple()
    out_dir = SRC_TAURI_DIR / "binaries"
    out_dir.mkdir(parents=True, exist_ok=True)
    out_bin = out_dir / f"pycoach-{triple}{EXE_SUFFIX}"

    print("[build_pycoach] syncing pycoach venv via uv")
    subprocess.run(
        ["uv", "sync"],
        cwd=PYCOACH_DIR,
        check=True,
        stdout=subprocess.DEVNULL,
    )

    print("[build_pycoach] installing pyinstaller into pycoach venv")
    subprocess.run(
        ["uv", "pip", "install", "--quiet", "pyinstaller"],
        cwd=PYCOACH_DIR,
        check=True,
    )

    # PyInstaller's `--name pycoach` produces `pycoach` on Unix and
    # `pycoach.exe` on Windows. The entry script just calls into pycoach.cli.
    entry_fd, entry_path_str = tempfile.mkstemp(suffix=".py", prefix="pycoach_entry_")
    os.close(entry_fd)
    entry_path = Path(entry_path_str)
    entry_path.write_text(
        "import sys\n"
        "from pycoach.cli import main\n"
        "raise SystemExit(main())\n"
    )

    work_root = out_dir / "_pyinstaller"
    try:
        if work_root.exists():
            shutil.rmtree(work_root)
        work_root.mkdir(parents=True)

        print("[build_pycoach] running pyinstaller (slow on first build)")
        # Why excluded:
        #   logfire — pydantic plugin that calls inspect.getsource() at
        #     import time. Frozen binaries have no source on disk, so it
        #     crashes the interpreter on startup. Pycoach doesn't actually
        #     use logfire; it's pulled in as an optional pydantic-ai
        #     integration. Excluding it makes the pydantic plugin loader
        #     skip it cleanly.
        # Why --recursive-copy-metadata:
        #   pydantic-ai (and several of its deps) call
        #   importlib.metadata.version("pkg") at import time. PyInstaller
        #   doesn't include distribution metadata by default — recursive
        #   copy grabs the package and its entire dependency tree's
        #   metadata in one go.
        subprocess.run(
            [
                "uv", "run", "pyinstaller",
                "--onefile",
                "--console",
                "--name", "pycoach",
                "--distpath", str(work_root / "dist"),
                "--workpath", str(work_root / "build"),
                "--specpath", str(work_root / "spec"),
                "--collect-submodules", "pycoach",
                "--collect-submodules", "pydantic_ai",
                "--recursive-copy-metadata", "pydantic-ai",
                "--recursive-copy-metadata", "pydantic-ai-slim",
                "--recursive-copy-metadata", "fastapi",
                "--recursive-copy-metadata", "uvicorn",
                "--exclude-module", "logfire",
                str(entry_path),
            ],
            cwd=PYCOACH_DIR,
            check=True,
        )

        built = work_root / "dist" / f"pycoach{EXE_SUFFIX}"
        if not built.is_file():
            die(f"pyinstaller did not produce {built}")

        if out_bin.exists():
            out_bin.unlink()
        shutil.move(str(built), str(out_bin))
    finally:
        entry_path.unlink(missing_ok=True)
        if work_root.exists():
            shutil.rmtree(work_root, ignore_errors=True)

    size_mb = out_bin.stat().st_size / 1024 / 1024
    print()
    print(f"[build_pycoach] built sidecar: {out_bin}")
    print(f"[build_pycoach] size: {size_mb:.0f} MB")
    print()
    print("next: build the bundled app with the sidecar overlay:")
    print("  npm run tauri build -- -f pycoach -c src-tauri/tauri.conf.pycoach.json")
    return 0


if __name__ == "__main__":
    sys.exit(main())
