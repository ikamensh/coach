//! File-based logging for the GUI app.
//!
//! Strategy: at every app startup, open a fresh log file with a
//! timestamped name (`coach-YYYY-MM-DD_HH-MM-SS-mmm.log`) under
//! `~/Library/Logs/Coach/` (or platform equivalent), point a stable
//! `coach.log` symlink at it, prune older files past `KEEP_LAUNCHES`,
//! then `dup2` the file's fd onto STDERR/STDOUT. From that point on,
//! every existing `eprintln!` / `println!` and any panic message lands
//! in the log file with zero call-site churn.
//!
//! Layout in the log directory:
//!   coach.log                              -> symlink to the most recent
//!   coach-2026-04-07_13-07-22-450.log      most recent launch
//!   coach-2026-04-07_13-05-10-119.log      previous launch
//!   …
//!
//! Each launch lives in its own file so investigating "what happened
//! the time it crashed" is just `tail coach-<that-time>.log`. The
//! symlink lets `tail -F coach.log` follow across restarts.
//!
//! CLI mode is untouched — `init_for_app()` is only called from the GUI
//! `run()` path, after `cli::dispatch()` has returned.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// How many timestamped log files to keep in the directory. Older
/// files are deleted on each startup. 500 covers months of normal
/// restart cadence even for users who relaunch many times a day.
const KEEP_LAUNCHES: usize = 500;

/// Stable name pointed at the most recent launch's file via symlink.
const SYMLINK_NAME: &str = "coach.log";

/// Resolve where Coach's log directory lives.
///
/// macOS:   `~/Library/Logs/Coach/` (Apple's standard place)
/// Linux:   `$XDG_STATE_HOME/coach/` or `~/.local/state/coach/`
/// Other:   `<data_local>/coach/`
pub fn log_dir() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        #[cfg(target_os = "macos")]
        {
            return home.join("Library/Logs/Coach");
        }
        #[cfg(target_os = "linux")]
        {
            let state = std::env::var_os("XDG_STATE_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".local/state"));
            return state.join("coach");
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
}

/// Build the filename for a launch starting at `now`.
/// Millisecond resolution → no in-process collisions.
fn launch_filename(now: chrono::DateTime<chrono::Local>) -> String {
    format!(
        "coach-{}-{:03}.log",
        now.format("%Y-%m-%d_%H-%M-%S"),
        now.timestamp_subsec_millis(),
    )
}

/// Return the timestamped log filenames currently in `dir`, sorted
/// chronologically (oldest first). Filename matching is loose: any
/// `coach-…log` that isn't the symlink counts.
fn list_launch_files(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name();
            let s = name.to_string_lossy();
            if s == SYMLINK_NAME {
                return None;
            }
            if s.starts_with("coach-") && s.ends_with(".log") {
                Some(e.path())
            } else {
                None
            }
        })
        .collect();
    // Filename format is lexicographically sortable by time.
    files.sort();
    Ok(files)
}

/// Delete the oldest launch files in `dir` until at most `keep`
/// remain. No-op if there are fewer than `keep` already.
fn prune_old_logs(dir: &Path, keep: usize) -> std::io::Result<()> {
    let files = list_launch_files(dir)?;
    if files.len() <= keep {
        return Ok(());
    }
    for f in files.iter().take(files.len() - keep) {
        let _ = fs::remove_file(f);
    }
    Ok(())
}

/// Repoint `<dir>/coach.log` at `target_filename` (a name relative to
/// `dir`, not an absolute path — keeps the symlink portable if the
/// directory is moved). Best-effort: failures are not fatal.
#[cfg(unix)]
fn update_symlink(dir: &Path, target_filename: &str) {
    let symlink = dir.join(SYMLINK_NAME);
    // remove + recreate. Race-free in practice since Coach is the
    // only writer in this directory.
    let _ = fs::remove_file(&symlink);
    let _ = std::os::unix::fs::symlink(target_filename, &symlink);
}

#[cfg(not(unix))]
fn update_symlink(_dir: &Path, _target_filename: &str) {
    // Windows: a junction or hardlink would be needed; skip for now.
}

/// Create the directory if needed, open a fresh timestamped log file
/// for this launch, update the `coach.log` symlink, and prune old
/// files. Pure I/O — safe to unit test.
///
/// Returns `(file, full_path)` where `full_path` is the timestamped
/// path the caller can announce.
pub fn prepare_log_file(
    dir: &Path,
    now: chrono::DateTime<chrono::Local>,
    keep: usize,
) -> std::io::Result<(File, PathBuf)> {
    fs::create_dir_all(dir)?;

    let name = launch_filename(now);
    let path = dir.join(&name);
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&path)?;

    update_symlink(dir, &name);
    prune_old_logs(dir, keep)?;

    Ok((file, path))
}

/// Open a fresh timestamped log file under the platform's default
/// log directory and redirect stderr + stdout to it. Thin wrapper
/// over [`init_for_app_in`]. Use this from production code paths
/// only — tests should always go through `init_for_app_in` with an
/// explicit tempdir to avoid polluting `~/Library/Logs/Coach/`.
pub fn init_for_app() -> Option<PathBuf> {
    init_for_app_in(&log_dir())
}

/// Open a fresh timestamped log file under `dir` and redirect the
/// process's stderr + stdout to it. Returns the path so the caller
/// can announce it. On any I/O error, leaves the original stderr/
/// stdout untouched and returns `None`.
///
/// Tests that want to exercise the redirect path pass a tempdir;
/// production callers go through [`init_for_app`] which uses
/// [`log_dir`].
pub fn init_for_app_in(dir: &Path) -> Option<PathBuf> {
    let now = chrono::Local::now();
    let (file, path) = match prepare_log_file(dir, now, KEEP_LAUNCHES) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("[coach] could not open log file under {dir:?}: {e}");
            return None;
        }
    };

    // Banner so the file's first line tells you what version started it.
    let mut banner = &file;
    let _ = writeln!(
        banner,
        "=== Coach v{} starting at {} ===",
        env!("CARGO_PKG_VERSION"),
        now.format("%Y-%m-%d %H:%M:%S%.3f"),
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

    fn ts(s: &str) -> chrono::DateTime<chrono::Local> {
        chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.3f")
            .unwrap()
            .and_local_timezone(chrono::Local)
            .unwrap()
    }

    fn read_to_string(p: &Path) -> String {
        let mut s = String::new();
        File::open(p).unwrap().read_to_string(&mut s).unwrap();
        s
    }

    /// `log_dir()` returns a path under a directory whose name
    /// contains "coach". Sanity check that survives refactors.
    #[test]
    fn log_dir_contains_coach() {
        let p = log_dir();
        let s = p.to_string_lossy().to_lowercase();
        assert!(s.contains("coach"), "expected 'coach' in {p:?}");
    }

    /// Property: launch filenames embed the timestamp at millisecond
    /// resolution and lexicographic sort = chronological sort.
    #[test]
    fn launch_filename_is_lexicographically_sortable_by_time() {
        let a = launch_filename(ts("2026-04-07 13:00:00.000"));
        let b = launch_filename(ts("2026-04-07 13:00:00.001"));
        let c = launch_filename(ts("2026-04-07 13:00:01.000"));
        let d = launch_filename(ts("2026-04-08 09:00:00.000"));
        let mut shuffled = vec![&d, &b, &a, &c];
        shuffled.sort();
        assert_eq!(shuffled, vec![&a, &b, &c, &d]);
    }

    /// `prepare_log_file` creates the directory if it doesn't exist —
    /// first-run on a fresh machine must not need any setup.
    #[test]
    fn prepare_creates_missing_directory() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("a/b/c");
        let (f, path) = prepare_log_file(&dir, ts("2026-04-07 13:00:00.000"), 5).unwrap();
        drop(f);
        assert!(path.exists());
        assert!(path.starts_with(&dir));
    }

    /// Property: each call returns a path that wasn't there before.
    /// Different timestamps → different filenames → no collision.
    #[test]
    fn each_launch_gets_a_unique_path() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path();
        let (f1, p1) =
            prepare_log_file(dir, ts("2026-04-07 13:00:00.000"), 50).unwrap();
        drop(f1);
        let (f2, p2) =
            prepare_log_file(dir, ts("2026-04-07 13:00:00.001"), 50).unwrap();
        drop(f2);
        assert_ne!(p1, p2);
        assert!(p1.exists());
        assert!(p2.exists());
    }

    /// Previous launches are preserved (the whole point of having a
    /// directory of launch files).
    #[test]
    fn previous_launch_files_are_preserved() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path();
        let (mut f1, p1) =
            prepare_log_file(dir, ts("2026-04-07 13:00:00.000"), 50).unwrap();
        writeln!(f1, "launch A").unwrap();
        drop(f1);

        let _ = prepare_log_file(dir, ts("2026-04-07 13:00:01.000"), 50).unwrap();

        assert!(p1.exists());
        assert!(read_to_string(&p1).contains("launch A"));
    }

    /// Property: after K consecutive launches, only the most recent
    /// `keep` files survive — the oldest are deleted, in order.
    #[test]
    fn pruning_keeps_most_recent_n_files() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path();
        let keep = 3;

        let mut paths = Vec::new();
        for i in 0..6 {
            let now = ts(&format!("2026-04-07 13:00:0{i}.000"));
            let (f, p) = prepare_log_file(dir, now, keep).unwrap();
            drop(f);
            paths.push(p);
        }

        let surviving = list_launch_files(dir).unwrap();
        assert_eq!(surviving.len(), keep);
        // The 3 most-recent paths (last in `paths`) must all still exist.
        for p in paths.iter().rev().take(keep) {
            assert!(p.exists(), "expected {p:?} to survive");
        }
        // The 3 oldest must be gone.
        for p in paths.iter().take(paths.len() - keep) {
            assert!(!p.exists(), "expected {p:?} to be pruned");
        }
    }

    /// On Unix, `coach.log` is a symlink to the most recent launch's
    /// filename. Updating to a new launch repoints it.
    #[cfg(unix)]
    #[test]
    fn symlink_points_at_latest_launch() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path();
        let (_, p1) =
            prepare_log_file(dir, ts("2026-04-07 13:00:00.000"), 50).unwrap();
        let symlink = dir.join(SYMLINK_NAME);
        assert!(symlink.exists());
        let target = fs::read_link(&symlink).unwrap();
        assert_eq!(target, PathBuf::from(p1.file_name().unwrap()));

        let (_, p2) =
            prepare_log_file(dir, ts("2026-04-07 13:00:01.000"), 50).unwrap();
        let target2 = fs::read_link(&symlink).unwrap();
        assert_eq!(target2, PathBuf::from(p2.file_name().unwrap()));
    }

    /// `prune_old_logs` ignores the symlink itself when counting —
    /// deleting `coach.log` would lose the convenient stable name
    /// without freeing any space.
    #[cfg(unix)]
    #[test]
    fn prune_does_not_count_or_delete_symlink() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path();

        // Two launches, keep = 1. Should leave 1 timestamped + 1 symlink.
        prepare_log_file(dir, ts("2026-04-07 13:00:00.000"), 1).unwrap();
        prepare_log_file(dir, ts("2026-04-07 13:00:01.000"), 1).unwrap();

        let files = list_launch_files(dir).unwrap();
        assert_eq!(files.len(), 1, "expected 1 surviving launch file");
        assert!(dir.join(SYMLINK_NAME).exists(), "symlink should still exist");
    }
}
