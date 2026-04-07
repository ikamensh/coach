//! Install/uninstall a `coach` shim on PATH so the same binary that runs
//! the GUI can be invoked from the terminal.
//!
//! On macOS/Linux this is a symlink in `~/.local/bin/coach` pointing at
//! `current_exe()`. On Windows it's a copy to
//! `%LOCALAPPDATA%\Coach\bin\coach.exe` plus a `HKCU\Environment` PATH
//! update.
//!
//! All the file logic is parameterised on `(target_dir, source_exe)` so
//! the unit tests can drive it with tempdirs without ever touching the
//! user's real `~/.local/bin`.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathStatus {
    /// Where the shim would be / is installed.
    pub install_path: String,
    /// True iff a file (or symlink) exists at `install_path`.
    pub installed: bool,
    /// What `install_path` currently points at, if anything. For a symlink
    /// this is `read_link`; for a regular file it's the same as
    /// `install_path`. None if not installed.
    pub target: Option<String>,
    /// True iff `target` resolves to the same inode/file as the
    /// currently-running binary. Lets the UI flag a stale shim after the
    /// app has been updated.
    pub matches_current_exe: bool,
    /// True iff the install dir is on `$PATH`.
    pub on_path: bool,
}

/// Default install dir per platform.
pub fn default_install_dir() -> Result<PathBuf, String> {
    #[cfg(windows)]
    {
        let local = std::env::var("LOCALAPPDATA")
            .map_err(|_| "LOCALAPPDATA not set".to_string())?;
        Ok(PathBuf::from(local).join("Coach").join("bin"))
    }
    #[cfg(not(windows))]
    {
        let home = dirs::home_dir().ok_or("no home directory")?;
        Ok(home.join(".local").join("bin"))
    }
}

/// File name of the shim per platform.
pub fn shim_file_name() -> &'static str {
    #[cfg(windows)]
    {
        "coach.exe"
    }
    #[cfg(not(windows))]
    {
        "coach"
    }
}

/// Resolve where the shim lives inside `dir`.
pub fn shim_path(dir: &Path) -> PathBuf {
    dir.join(shim_file_name())
}

/// Public entry point: install the shim into the default location, pointing
/// at the currently-running binary.
pub fn install() -> Result<PathStatus, String> {
    let dir = default_install_dir()?;
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    install_at(&dir, &exe)?;
    Ok(status_at(&dir, &exe))
}

/// Public entry point: remove the shim from the default location.
pub fn uninstall() -> Result<PathStatus, String> {
    let dir = default_install_dir()?;
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    uninstall_at(&dir)?;
    Ok(status_at(&dir, &exe))
}

/// Public entry point: report current status against the default install dir.
pub fn status() -> Result<PathStatus, String> {
    let dir = default_install_dir()?;
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    Ok(status_at(&dir, &exe))
}

// ── Path-injectable helpers (drive these from tests) ────────────────────

/// Install a shim at `dir/coach[.exe]` pointing at `source_exe`. Idempotent
/// — if a shim already exists it's removed first so the new one always
/// points at `source_exe`.
pub fn install_at(dir: &Path, source_exe: &Path) -> Result<(), String> {
    if !source_exe.exists() {
        return Err(format!(
            "source binary does not exist: {}",
            source_exe.display()
        ));
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let shim = shim_path(dir);

    // Remove any existing shim so we always end up with a fresh link/copy.
    if shim.exists() || shim.symlink_metadata().is_ok() {
        std::fs::remove_file(&shim)
            .map_err(|e| format!("remove existing shim {}: {e}", shim.display()))?;
    }

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(source_exe, &shim)
            .map_err(|e| format!("symlink {} -> {}: {e}", shim.display(), source_exe.display()))?;
    }
    #[cfg(windows)]
    {
        // Windows symlinks need elevation; copy the binary instead.
        std::fs::copy(source_exe, &shim)
            .map_err(|e| format!("copy {} -> {}: {e}", source_exe.display(), shim.display()))?;
    }
    Ok(())
}

/// Remove the shim at `dir/coach[.exe]`. Errors if no shim is present —
/// callers that want idempotent behaviour should check `status_at` first.
pub fn uninstall_at(dir: &Path) -> Result<(), String> {
    let shim = shim_path(dir);
    if !shim.exists() && shim.symlink_metadata().is_err() {
        return Err(format!("no shim installed at {}", shim.display()));
    }
    std::fs::remove_file(&shim)
        .map_err(|e| format!("remove {}: {e}", shim.display()))?;
    Ok(())
}

/// Report status of the shim at `dir/coach[.exe]` relative to `current_exe`.
/// Pure read — never mutates the filesystem.
pub fn status_at(dir: &Path, current_exe: &Path) -> PathStatus {
    let shim = shim_path(dir);
    let installed = shim.exists() || shim.symlink_metadata().is_ok();

    let target: Option<PathBuf> = if installed {
        // For a symlink we want what it points to; otherwise the file itself.
        match std::fs::read_link(&shim) {
            Ok(p) => Some(p),
            Err(_) => Some(shim.clone()),
        }
    } else {
        None
    };

    let matches_current_exe = match (&target, std::fs::canonicalize(current_exe)) {
        (Some(t), Ok(canon_exe)) => std::fs::canonicalize(t)
            .map(|canon_t| canon_t == canon_exe)
            .unwrap_or(false),
        _ => false,
    };

    PathStatus {
        install_path: shim.display().to_string(),
        installed,
        target: target.map(|p| p.display().to_string()),
        matches_current_exe,
        on_path: dir_on_path(dir),
    }
}

/// Mark a file executable on Unix (mode 0o755). No-op on Windows where
/// the executable bit is determined by extension.
#[cfg(unix)]
pub fn make_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)
        .map_err(|e| format!("stat {}: {e}", path.display()))?
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms)
        .map_err(|e| format!("chmod +x {}: {e}", path.display()))
}

#[cfg(not(unix))]
pub fn make_executable(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// True iff `dir` is one of the entries in the current `$PATH`. Compared
/// after canonicalization so symlink and trailing-slash differences don't
/// produce false negatives.
pub fn dir_on_path(dir: &Path) -> bool {
    let Ok(path_var) = std::env::var("PATH") else {
        return false;
    };
    let target = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    std::env::split_paths(&path_var).any(|entry| {
        let canon = std::fs::canonicalize(&entry).unwrap_or(entry);
        canon == target
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a fake "binary" we can symlink to in tests.
    fn fake_exe(dir: &Path) -> PathBuf {
        let path = dir.join("fake-coach");
        std::fs::write(&path, b"#!/bin/sh\necho fake\n").unwrap();
        make_executable(&path).unwrap();
        path
    }

    /// install_at on a clean dir creates the shim and status_at reports it
    /// installed and matching the source.
    #[test]
    fn install_then_status_reports_installed() {
        let tmp = tempfile::tempdir().unwrap();
        let bin_dir = tmp.path().join("bin");
        let exe = fake_exe(tmp.path());

        install_at(&bin_dir, &exe).unwrap();
        let s = status_at(&bin_dir, &exe);

        assert!(s.installed);
        assert!(s.matches_current_exe, "shim must point at the source exe");
        assert!(s.target.is_some());
        assert!(s.install_path.ends_with(shim_file_name()));
    }

    /// install_at twice in a row should not error and should leave a single
    /// shim pointing at the latest source.
    #[test]
    fn install_at_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let bin_dir = tmp.path().join("bin");
        let exe1 = fake_exe(tmp.path());
        let exe2 = {
            let p = tmp.path().join("fake-coach-2");
            std::fs::write(&p, b"v2").unwrap();
            p
        };

        install_at(&bin_dir, &exe1).unwrap();
        install_at(&bin_dir, &exe2).unwrap();

        let s = status_at(&bin_dir, &exe2);
        assert!(s.installed);
        assert!(s.matches_current_exe, "second install should re-point at exe2");
    }

    /// install_at then uninstall_at leaves the dir with no shim, and
    /// status_at reports not installed.
    #[test]
    fn install_then_uninstall_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let bin_dir = tmp.path().join("bin");
        let exe = fake_exe(tmp.path());

        install_at(&bin_dir, &exe).unwrap();
        uninstall_at(&bin_dir).unwrap();

        let s = status_at(&bin_dir, &exe);
        assert!(!s.installed);
        assert!(s.target.is_none());
        assert!(!s.matches_current_exe);
    }

    /// uninstall_at on an empty dir should error so callers know nothing
    /// happened — keeps Rule A.2 (don't paper over surprise) satisfied.
    #[test]
    fn uninstall_at_errors_when_nothing_installed() {
        let tmp = tempfile::tempdir().unwrap();
        let bin_dir = tmp.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        assert!(uninstall_at(&bin_dir).is_err());
    }

    /// install_at must fail loudly if the source binary is missing,
    /// instead of creating a dangling symlink.
    #[test]
    fn install_at_errors_on_missing_source() {
        let tmp = tempfile::tempdir().unwrap();
        let bin_dir = tmp.path().join("bin");
        let missing = tmp.path().join("does-not-exist");
        let err = install_at(&bin_dir, &missing).unwrap_err();
        assert!(err.contains("does not exist"), "got: {err}");
    }

    /// status_at against a directory that doesn't even exist should
    /// report not installed without erroring.
    #[test]
    fn status_at_handles_missing_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let bin_dir = tmp.path().join("nope");
        let exe = fake_exe(tmp.path());
        let s = status_at(&bin_dir, &exe);
        assert!(!s.installed);
        assert!(s.target.is_none());
    }

    /// status_at must detect a stale shim — one that points at a binary
    /// that no longer matches `current_exe`. This is the post-upgrade case
    /// where Coach.app has been replaced and the symlink should be refreshed.
    #[test]
    fn status_at_detects_stale_shim() {
        let tmp = tempfile::tempdir().unwrap();
        let bin_dir = tmp.path().join("bin");
        let old_exe = fake_exe(tmp.path());
        let new_exe = {
            let p = tmp.path().join("new-coach");
            std::fs::write(&p, b"v2").unwrap();
            p
        };

        install_at(&bin_dir, &old_exe).unwrap();
        let s = status_at(&bin_dir, &new_exe);

        assert!(s.installed);
        assert!(
            !s.matches_current_exe,
            "shim points at old_exe but current_exe is new_exe — should be flagged stale"
        );
    }

    /// dir_on_path must be true when the dir is in `$PATH` and false
    /// when it isn't. We mutate `PATH` for the duration of this test.
    #[test]
    fn dir_on_path_detects_membership() {
        let tmp = tempfile::tempdir().unwrap();
        let bin_dir = tmp.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();

        // Save and restore so we don't pollute other tests.
        let saved = std::env::var("PATH").ok();
        // SAFETY: tests are single-threaded with respect to env in this module.
        unsafe {
            std::env::set_var("PATH", bin_dir.display().to_string());
        }
        assert!(dir_on_path(&bin_dir));

        unsafe {
            std::env::set_var("PATH", "/nowhere");
        }
        assert!(!dir_on_path(&bin_dir));

        unsafe {
            match saved {
                Some(v) => std::env::set_var("PATH", v),
                None => std::env::remove_var("PATH"),
            }
        }
    }
}
