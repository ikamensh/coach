//! File-based logging for the GUI app.
//!
//! Strategy: at every app startup, rotate a ring of previous logs and
//! open a fresh `~/Library/Logs/Coach/coach.log` (or platform equivalent),
//! then `dup2` its fd onto STDERR/STDOUT. From that point on, every
//! existing `eprintln!` / `println!` and any panic message lands in the
//! log file with zero call-site churn.
//!
//! Ring layout after N launches:
//!   coach.log     — current launch
//!   coach.log.1   — previous launch
//!   coach.log.2   — two launches ago
//!   …
//!   coach.log.<HISTORY>  — oldest retained
//!
//! Each launch gets its own file so investigating "what happened the
//! time it crashed" is just `tail coach.log.1`.
//!
//! CLI mode is untouched — `init_for_app()` is only called from the GUI
//! `run()` path, after `cli::dispatch()` has returned.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// How many previous launches to keep alongside the current one.
/// 10 covers typical "I crashed on startup, let me try again" loops
/// without burying the original failure.
const HISTORY: usize = 10;

/// Resolve where Coach's log file lives.
///
/// macOS:   `~/Library/Logs/Coach/coach.log` (Apple's standard place)
/// Linux:   `$XDG_STATE_HOME/coach/coach.log` or `~/.local/state/coach/coach.log`
/// Other:   `<data_local>/coach/coach.log`
pub fn log_path() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        #[cfg(target_os = "macos")]
        {
            return home.join("Library/Logs/Coach/coach.log");
        }
        #[cfg(target_os = "linux")]
        {
            let state = std::env::var_os("XDG_STATE_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".local/state"));
            return state.join("coach/coach.log");
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = home;
        }
    }
    dirs::data_local_dir()
        .or_else(dirs::cache_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join("coach")
        .join("coach.log")
}

/// Sibling file `<path>.<n>` — `.1` is the most recent previous launch.
fn numbered_path(path: &Path, n: usize) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(format!(".{n}"));
    PathBuf::from(s)
}

/// Shift the ring by one position. The oldest log (`.<history>`) is
/// dropped, each `.i` becomes `.(i+1)`, and the current log becomes
/// `.1`. After this returns, `path` does not exist and the caller can
/// open a fresh file.
///
/// `history == 0` discards the current log without keeping any history.
/// Missing intermediate files are silently skipped.
fn rotate_ring(path: &Path, history: usize) -> std::io::Result<()> {
    if history > 0 {
        // Drop the oldest if it exists.
        let oldest = numbered_path(path, history);
        if oldest.exists() {
            fs::remove_file(&oldest)?;
        }
        // Shift .i -> .(i+1) from the top down so we never overwrite.
        for i in (1..history).rev() {
            let from = numbered_path(path, i);
            if from.exists() {
                let to = numbered_path(path, i + 1);
                fs::rename(&from, &to)?;
            }
        }
    }
    // Current -> .1 (or just delete if no history is kept).
    if path.exists() {
        if history > 0 {
            fs::rename(path, numbered_path(path, 1))?;
        } else {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

/// Create the parent directory, rotate the ring of previous logs, and
/// open a fresh empty file at `path` for the new launch. Pure I/O, no
/// fd surgery — safe to unit test.
pub fn prepare_log_file(path: &Path, history: usize) -> std::io::Result<File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    rotate_ring(path, history)?;
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
}

/// Open a fresh log file for this launch (rotating the ring of
/// previous launches) and redirect the process's stderr + stdout to
/// it. Returns the path so the caller can announce it. On any I/O
/// error, leaves the original stderr/stdout untouched and returns
/// `None`.
pub fn init_for_app() -> Option<PathBuf> {
    let path = log_path();
    let file = match prepare_log_file(&path, HISTORY) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("[coach] could not open log file {path:?}: {e}");
            return None;
        }
    };

    // Banner so we can tell startups apart in a single tail.
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    let mut banner = &file;
    let _ = writeln!(
        banner,
        "\n=== Coach v{} starting at {} ===",
        env!("CARGO_PKG_VERSION"),
        now,
    );

    redirect_std_to(&file);
    Some(path)
}

#[cfg(unix)]
fn redirect_std_to(file: &File) {
    use std::os::fd::AsRawFd;
    let fd = file.as_raw_fd();
    // SAFETY: dup2 is async-signal-safe and the source fd is valid for
    // the lifetime of `file`. After the call, the kernel keeps the
    // underlying inode open via fds 1 and 2 even when `file` is dropped.
    unsafe {
        libc::dup2(fd, libc::STDERR_FILENO);
        libc::dup2(fd, libc::STDOUT_FILENO);
    }
}

#[cfg(not(unix))]
fn redirect_std_to(_file: &File) {
    // Windows would need SetStdHandle. Out of scope for now — Coach is
    // mac-first and the eprintln! sites still write to the original
    // console on platforms without a redirect.
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use tempfile::tempdir;

    fn read_to_string(p: &Path) -> String {
        let mut s = String::new();
        File::open(p).unwrap().read_to_string(&mut s).unwrap();
        s
    }

    /// `log_path()` returns a path under a directory whose name
    /// contains "coach". Sanity check that survives refactors.
    #[test]
    fn log_path_is_under_a_coach_directory() {
        let p = log_path();
        let s = p.to_string_lossy().to_lowercase();
        assert!(s.contains("coach"), "expected 'coach' in {p:?}");
        assert!(p.file_name().is_some(), "expected a filename in {p:?}");
    }

    /// `prepare_log_file` creates parent directories that don't exist
    /// yet, so first-run on a fresh machine doesn't need any setup.
    #[test]
    fn prepare_creates_missing_parent_dirs() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a/b/c/coach.log");
        let f = prepare_log_file(&path, 5).unwrap();
        drop(f);
        assert!(path.exists());
    }

    /// Property: every call to `prepare_log_file` returns a fresh,
    /// empty file at `path` regardless of what was there before.
    /// This is the "each launch gets its own log" guarantee.
    #[test]
    fn each_call_starts_with_an_empty_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("coach.log");
        {
            let mut f = prepare_log_file(&path, 5).unwrap();
            writeln!(f, "first launch noise").unwrap();
        }
        // Second call must hand back an empty file.
        let f = prepare_log_file(&path, 5).unwrap();
        drop(f);
        assert_eq!(fs::metadata(&path).unwrap().len(), 0);
    }

    /// Property: the previous launch's content is preserved at
    /// `<path>.1` after the next `prepare_log_file` call. The N-th
    /// most recent launch lives at `<path>.<N>`.
    #[test]
    fn previous_launch_moves_to_dot_one() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("coach.log");
        {
            let mut f = prepare_log_file(&path, 5).unwrap();
            writeln!(f, "launch A").unwrap();
        }
        let _ = prepare_log_file(&path, 5).unwrap();
        let one = numbered_path(&path, 1);
        assert!(one.exists(), "expected previous launch at {one:?}");
        assert!(read_to_string(&one).contains("launch A"));
    }

    /// Property: after K consecutive launches, the i-th most recent
    /// launch (1-indexed) lives at `<path>.<i>` for i = 1..min(K, history).
    /// Uses small history to keep the test fast and clear.
    #[test]
    fn ring_preserves_order_across_many_launches() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("coach.log");
        let history = 3;

        // 5 launches; history holds at most 3 previous + 1 current = 4 files.
        for i in 1..=5 {
            let mut f = prepare_log_file(&path, history).unwrap();
            writeln!(f, "launch {i}").unwrap();
        }

        // After launch 5: current="launch 5" was the latest write, but
        // we want a *fresh* file for the next launch, so simulate it.
        let _ = prepare_log_file(&path, history).unwrap();

        // Now: .1 should be "launch 5", .2 = "launch 4", .3 = "launch 3".
        // Earlier launches (1, 2) have aged out.
        for (n, expected) in [(1, "launch 5"), (2, "launch 4"), (3, "launch 3")] {
            let p = numbered_path(&path, n);
            assert!(p.exists(), "expected {p:?} to exist");
            assert!(
                read_to_string(&p).contains(expected),
                ".{n} should contain {expected:?}, got {:?}",
                read_to_string(&p),
            );
        }
        // .4 must NOT exist — we capped at history=3.
        assert!(
            !numbered_path(&path, 4).exists(),
            "ring exceeded its capacity"
        );
    }

    /// `rotate_ring` on a non-existent file is a no-op rather than an
    /// error — first-ever startup must not fail.
    #[test]
    fn rotate_missing_file_is_noop() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nope.log");
        rotate_ring(&path, 5).unwrap();
        assert!(!path.exists());
        assert!(!numbered_path(&path, 1).exists());
    }

    /// Property: with `history == 0`, no `.N` siblings are ever
    /// created. Each launch gets a fresh file and the previous one is
    /// discarded. Useful as a degenerate-but-valid configuration and
    /// guards the off-by-one in the rotation loop.
    #[test]
    fn history_zero_keeps_no_previous_files() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("coach.log");
        {
            let mut f = prepare_log_file(&path, 0).unwrap();
            writeln!(f, "first").unwrap();
        }
        let _ = prepare_log_file(&path, 0).unwrap();
        assert!(!numbered_path(&path, 1).exists());
        assert_eq!(fs::metadata(&path).unwrap().len(), 0);
    }
}
