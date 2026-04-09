use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::Settings;

// ── Claude Code hook detection / installation ──────────────────────────

fn claude_settings_path() -> PathBuf {
    dirs::home_dir()
        .expect("no home directory")
        .join(".claude")
        .join("settings.json")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookEntryStatus {
    pub event: String,
    pub url: String,
    pub installed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookStatus {
    pub installed: bool,
    pub path: String,
    pub hooks: Vec<HookEntryStatus>,
}

fn expected_hook_urls(port: u16) -> Vec<(&'static str, String)> {
    let base = format!("http://localhost:{}", port);
    vec![
        ("PermissionRequest", format!("{}/hook/permission-request", base)),
        ("Stop", format!("{}/hook/stop", base)),
        ("PostToolUse", format!("{}/hook/post-tool-use", base)),
        ("UserPromptSubmit", format!("{}/hook/user-prompt-submit", base)),
        // NOTE: SessionStart used to be in this list — fast `/clear`
        // detection. Removed because Claude Code 2.1.92+ silently drops
        // HTTP hooks for SessionStart (its debug log says
        // "HTTP hooks are not supported for SessionStart"). Installing
        // it produced a misleading ✓ in `coach hooks status` for a hook
        // that would never fire. `/clear` is now detected lazily on the
        // next tool call via the session_id-mismatch path in
        // `state::apply_hook_event`. The HTTP route + handler in
        // server.rs are still wired up because Cursor honours its own
        // `sessionStart` hook — see `cursor_hook_events`.
    ]
}

fn has_http_hook(entries: &[serde_json::Value], url: &str) -> bool {
    entries.iter().any(|entry| is_coach_hook_entry(entry, url))
}

/// Returns true if this hook entry contains an HTTP hook matching the given URL.
fn is_coach_hook_entry(entry: &serde_json::Value, url: &str) -> bool {
    entry
        .get("hooks")
        .and_then(|h| h.as_array())
        .is_some_and(|hooks| {
            hooks.iter().any(|hook| {
                hook.get("type").and_then(|t| t.as_str()) == Some("http")
                    && hook.get("url").and_then(|u| u.as_str()) == Some(url)
            })
        })
}

pub fn check_hook_status(port: u16) -> HookStatus {
    check_hook_status_at(port, &claude_settings_path())
}

pub fn check_hook_status_at(port: u16, path: &std::path::Path) -> HookStatus {
    let expected = expected_hook_urls(port);

    let settings: serde_json::Value = std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::Value::Null);

    let hooks_obj = settings.get("hooks");

    let entries: Vec<HookEntryStatus> = expected
        .iter()
        .map(|(event, url)| {
            let installed = hooks_obj
                .and_then(|h| h.get(*event))
                .and_then(|arr| arr.as_array())
                .is_some_and(|entries| has_http_hook(entries, url));

            HookEntryStatus {
                event: event.to_string(),
                url: url.clone(),
                installed,
            }
        })
        .collect();

    let all_installed = entries.iter().all(|e| e.installed);

    HookStatus {
        installed: all_installed,
        path: path.display().to_string(),
        hooks: entries,
    }
}

/// Merge coach hooks into ~/.claude/settings.json, preserving existing hooks.
pub fn install_hooks(port: u16) -> Result<(), String> {
    install_hooks_at(port, &claude_settings_path())
}

/// Reconcile Coach-managed Claude Code hooks on startup.
///
/// Drives behaviour from the persistent intent flag (`hooks_user_enabled`),
/// not the on-disk presence of hooks. This matters because
/// `auto_uninstall_hooks_on_exit` deletes the on-disk entries on every clean
/// shutdown — without a separate intent flag we'd lose the "user opted in"
/// signal between sessions.
///
/// Behaviour:
///   * If `*user_enabled` is true: install any missing managed hooks.
///   * If `*user_enabled` is false but managed hooks already exist on disk
///     (legacy install or pre-flag user): flip the flag to true and install
///     any missing ones. This is a one-shot migration so existing users keep
///     working without re-clicking Install.
///   * Otherwise: no-op. First-time install stays explicit.
///
/// Returns the names of any newly-added hooks so the caller can log them.
pub fn sync_managed_hooks(port: u16, user_enabled: &mut bool) -> Result<Vec<String>, String> {
    sync_managed_hooks_at(port, &claude_settings_path(), user_enabled)
}

pub fn sync_managed_hooks_at(
    port: u16,
    path: &std::path::Path,
    user_enabled: &mut bool,
) -> Result<Vec<String>, String> {
    let status = check_hook_status_at(port, path);
    let any_installed = status.hooks.iter().any(|h| h.installed);

    // Legacy migration: hooks on disk but no recorded intent → adopt as
    // opted-in. Preserves behaviour for users who installed before the
    // intent flag existed.
    if !*user_enabled && any_installed {
        *user_enabled = true;
    }

    if !*user_enabled {
        return Ok(vec![]);
    }

    let missing: Vec<String> = status
        .hooks
        .iter()
        .filter(|h| !h.installed)
        .map(|h| h.event.clone())
        .collect();
    if missing.is_empty() {
        return Ok(vec![]);
    }
    install_hooks_at(port, path)?;
    Ok(missing)
}

pub fn install_hooks_at(port: u16, path: &std::path::Path) -> Result<(), String> {
    let expected = expected_hook_urls(port);

    let mut settings: serde_json::Value = std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::json!({}));

    let settings_obj = settings
        .as_object_mut()
        .ok_or("Claude settings is not a JSON object")?;

    if !settings_obj.contains_key("hooks") {
        settings_obj.insert("hooks".into(), serde_json::json!({}));
    }

    let hooks_obj = settings_obj
        .get_mut("hooks")
        .and_then(|v| v.as_object_mut())
        .ok_or("hooks is not an object")?;

    for (event, url) in &expected {
        let hook_entry = serde_json::json!({
            "hooks": [{"type": "http", "url": url}]
        });

        if let Some(existing) = hooks_obj.get_mut(*event) {
            if let Some(arr) = existing.as_array_mut() {
                if !has_http_hook(arr, url) {
                    arr.push(hook_entry);
                }
            }
        } else {
            hooks_obj.insert(event.to_string(), serde_json::json!([hook_entry]));
        }
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let json = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())?;

    Ok(())
}

/// Remove coach hooks from ~/.claude/settings.json, preserving everything else.
pub fn uninstall_hooks(port: u16) -> Result<(), String> {
    uninstall_hooks_at(port, &claude_settings_path())
}

pub fn uninstall_hooks_at(port: u16, path: &std::path::Path) -> Result<(), String> {
    let expected = expected_hook_urls(port);

    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut settings: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| e.to_string())?;

    let hooks_obj = settings
        .get_mut("hooks")
        .and_then(|v| v.as_object_mut())
        .ok_or("No hooks object in settings")?;

    for (event, url) in &expected {
        if let Some(arr) = hooks_obj.get_mut(*event).and_then(|v| v.as_array_mut()) {
            arr.retain(|entry| !is_coach_hook_entry(entry, url));
        }
    }

    // Clean up empty event arrays
    let empty_events: Vec<String> = hooks_obj
        .iter()
        .filter(|(_, v)| v.as_array().is_some_and(|a| a.is_empty()))
        .map(|(k, _)| k.clone())
        .collect();
    for key in empty_events {
        hooks_obj.remove(&key);
    }

    let json = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())?;

    Ok(())
}

// ── Cursor Agent hooks (`~/.cursor/hooks.json`, `command` + stdin JSON) ──
//
// Cursor's hook runner silently drops any hook command that mentions `curl`
// directly (verified against cursor-agent 2.4.28 — even prefixing with an
// unrelated `touch` doesn't slip past). We work around it by installing a
// shim script (`coach-cursor-hook.sh`, sibling of hooks.json) and pointing
// every hook entry at it: the shim is what calls curl. Hook command entries
// become `<shim path> <event-slug>`.

fn cursor_hooks_path() -> PathBuf {
    dirs::home_dir()
        .expect("no home directory")
        .join(".cursor")
        .join("hooks.json")
}

/// Shim script Coach drops next to hooks.json so hook entries can avoid
/// mentioning `curl` directly (Cursor's hook runner blocks that).
pub fn cursor_shim_path() -> PathBuf {
    dirs::home_dir()
        .expect("no home directory")
        .join(".cursor")
        .join("coach-cursor-hook.sh")
}

const CURSOR_SHIM_FILENAME: &str = "coach-cursor-hook.sh";

/// (cursor event name, URL slug under `/cursor/hook/`).
const CURSOR_HOOK_EVENTS: &[(&str, &str)] = &[
    ("sessionStart", "session-start"),
    ("beforeSubmitPrompt", "before-submit-prompt"),
    ("beforeShellExecution", "before-shell"),
    ("beforeMCPExecution", "before-mcp"),
    ("afterShellExecution", "after-shell"),
    ("afterMCPExecution", "after-mcp"),
    ("afterFileEdit", "after-file-edit"),
    ("stop", "stop"),
];

/// Body of the shim script for the given Coach port. `--data-binary @-`
/// forwards stdin verbatim and `exec` makes curl's exit code the shim's
/// (cursor treats non-zero as a hook error).
pub fn cursor_shim_script(port: u16) -> String {
    format!(
        "#!/bin/sh\n\
         # Auto-generated by Coach.\n\
         exec curl -sS -X POST \"http://127.0.0.1:{port}/cursor/hook/$1\" \\\n\
              -H \"Content-Type: application/json\" \\\n\
              --data-binary @-\n",
    )
}

pub fn expected_cursor_hook_commands(shim_path: &std::path::Path) -> Vec<(&'static str, String)> {
    let shim = shim_path.display().to_string();
    CURSOR_HOOK_EVENTS
        .iter()
        .map(|(event, slug)| (*event, format!("{shim} {slug}")))
        .collect()
}

fn cursor_command_matches(entry: &serde_json::Value, expected: &str) -> bool {
    entry
        .get("command")
        .and_then(|c| c.as_str())
        .is_some_and(|cmd| cmd == expected)
}

/// A hook entry is "managed by Coach" if its command references our shim
/// script by filename, OR if it's a legacy `curl ... 127.0.0.1:.../cursor/hook/...`
/// entry from a pre-shim install. The legacy match requires both the route
/// path AND a localhost host so an unrelated user hook with `/cursor/hook/`
/// in some other context can't be swept up by reinstall.
fn is_managed_cursor_command(cmd: &str) -> bool {
    if cmd.contains(CURSOR_SHIM_FILENAME) {
        return true;
    }
    cmd.contains("/cursor/hook/")
        && (cmd.contains("127.0.0.1:") || cmd.contains("localhost:"))
}

pub fn check_cursor_hook_status() -> HookStatus {
    check_cursor_hook_status_at(&cursor_hooks_path(), &cursor_shim_path())
}

pub fn check_cursor_hook_status_at(
    hooks_path: &std::path::Path,
    shim_path: &std::path::Path,
) -> HookStatus {
    let expected = expected_cursor_hook_commands(shim_path);

    let settings: serde_json::Value = std::fs::read_to_string(hooks_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::Value::Null);

    let hooks_obj = settings.get("hooks");

    let entries: Vec<HookEntryStatus> = expected
        .iter()
        .map(|(event, cmd)| {
            let installed = hooks_obj
                .and_then(|h| h.get(*event))
                .and_then(|arr| arr.as_array())
                .is_some_and(|entries| entries.iter().any(|e| cursor_command_matches(e, cmd)));

            HookEntryStatus {
                event: event.to_string(),
                url: cmd.clone(),
                installed,
            }
        })
        .collect();

    let all_installed = entries.iter().all(|e| e.installed);

    HookStatus {
        installed: all_installed,
        path: hooks_path.display().to_string(),
        hooks: entries,
    }
}

pub fn install_cursor_hooks(port: u16) -> Result<(), String> {
    install_cursor_hooks_at(port, &cursor_hooks_path(), &cursor_shim_path())
}

pub fn install_cursor_hooks_at(
    port: u16,
    hooks_path: &std::path::Path,
    shim_path: &std::path::Path,
) -> Result<(), String> {
    // 1. Write the shim script first so the executable referenced by
    //    hooks.json is on disk by the time the JSON is written.
    if let Some(parent) = shim_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(shim_path, cursor_shim_script(port)).map_err(|e| e.to_string())?;
    crate::path_install::make_executable(shim_path)?;

    // 2. Merge our hook entries into hooks.json, preserving any existing
    //    user entries (e.g. gleaner-cursor-upload).
    let expected = expected_cursor_hook_commands(shim_path);

    let mut settings: serde_json::Value = std::fs::read_to_string(hooks_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({ "version": 1, "hooks": {} }));

    let settings_obj = settings
        .as_object_mut()
        .ok_or("Cursor hooks.json must be a JSON object")?;

    if !settings_obj.contains_key("version") {
        settings_obj.insert("version".into(), serde_json::json!(1));
    }
    if !settings_obj.contains_key("hooks") {
        settings_obj.insert("hooks".into(), serde_json::json!({}));
    }

    let hooks_obj = settings_obj
        .get_mut("hooks")
        .and_then(|v| v.as_object_mut())
        .ok_or("hooks must be an object")?;

    for (event, cmd) in &expected {
        // Drop any stale Coach-managed entry first (different shim path
        // from a previous install) so we don't accumulate dead commands.
        if let Some(existing) = hooks_obj.get_mut(*event) {
            if let Some(arr) = existing.as_array_mut() {
                arr.retain(|entry| {
                    entry
                        .get("command")
                        .and_then(|c| c.as_str())
                        .map(|c| !is_managed_cursor_command(c))
                        .unwrap_or(true)
                });
            }
        }
        let entry = serde_json::json!({ "command": cmd });
        if let Some(existing) = hooks_obj.get_mut(*event) {
            if let Some(arr) = existing.as_array_mut() {
                if !arr.iter().any(|e| cursor_command_matches(e, cmd)) {
                    arr.push(entry);
                }
            }
        } else {
            hooks_obj.insert((*event).to_string(), serde_json::json!([entry]));
        }
    }

    if let Some(parent) = hooks_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let json = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    std::fs::write(hooks_path, json).map_err(|e| e.to_string())?;

    Ok(())
}

pub fn uninstall_cursor_hooks() -> Result<(), String> {
    uninstall_cursor_hooks_at(&cursor_hooks_path(), &cursor_shim_path())
}

pub fn uninstall_cursor_hooks_at(
    hooks_path: &std::path::Path,
    shim_path: &std::path::Path,
) -> Result<(), String> {
    let content = std::fs::read_to_string(hooks_path).map_err(|e| e.to_string())?;
    let mut settings: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| e.to_string())?;

    let hooks_obj = settings
        .get_mut("hooks")
        .and_then(|v| v.as_object_mut())
        .ok_or("No hooks object in Cursor hooks.json")?;

    for (_event, arr_val) in hooks_obj.iter_mut() {
        if let Some(arr) = arr_val.as_array_mut() {
            arr.retain(|entry| {
                entry
                    .get("command")
                    .and_then(|c| c.as_str())
                    .map(|cmd| !is_managed_cursor_command(cmd))
                    .unwrap_or(true)
            });
        }
    }

    let empty_events: Vec<String> = hooks_obj
        .iter()
        .filter(|(_, v)| v.as_array().is_some_and(|a| a.is_empty()))
        .map(|(k, _)| k.clone())
        .collect();
    for key in empty_events {
        hooks_obj.remove(&key);
    }

    let json = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    std::fs::write(hooks_path, json).map_err(|e| e.to_string())?;

    // Best-effort: remove the shim script. If it's already gone (or
    // shared with something else), don't error.
    let _ = std::fs::remove_file(shim_path);

    Ok(())
}

/// Cursor twin of `sync_managed_hooks`. See that function for the rationale
/// behind the `user_enabled` flag and the legacy migration.
pub fn sync_managed_cursor_hooks(
    port: u16,
    user_enabled: &mut bool,
) -> Result<Vec<String>, String> {
    sync_managed_cursor_hooks_at(port, &cursor_hooks_path(), &cursor_shim_path(), user_enabled)
}

pub fn sync_managed_cursor_hooks_at(
    port: u16,
    hooks_path: &std::path::Path,
    shim_path: &std::path::Path,
    user_enabled: &mut bool,
) -> Result<Vec<String>, String> {
    let status = check_cursor_hook_status_at(hooks_path, shim_path);
    let any_installed = status.hooks.iter().any(|h| h.installed);

    if !*user_enabled && any_installed {
        *user_enabled = true;
    }
    if !*user_enabled {
        return Ok(vec![]);
    }

    let missing: Vec<String> = status
        .hooks
        .iter()
        .filter(|h| !h.installed)
        .map(|h| h.event.clone())
        .collect();
    if missing.is_empty() {
        return Ok(vec![]);
    }
    install_cursor_hooks_at(port, hooks_path, shim_path)?;
    Ok(missing)
}

// ── Exit-time cleanup ────────────────────────────────────────────────────

/// Best-effort hook cleanup on Coach shutdown. Reads `~/.coach/settings.json`
/// from disk so the call site can be a sync shutdown callback that has no
/// access to (or shouldn't be locking) the in-memory `CoachState`.
///
/// No-op if `auto_uninstall_hooks_on_exit` is false. Removes Claude and/or
/// Cursor managed hooks based on the persistent intent flags. Errors are
/// logged but never panic — shutdown must always make progress.
pub fn cleanup_hooks_on_exit() {
    cleanup_hooks_on_exit_at(
        &Settings::load(),
        &claude_settings_path(),
        &cursor_hooks_path(),
        &cursor_shim_path(),
    );
}

/// Path-injectable variant for unit tests.
pub fn cleanup_hooks_on_exit_at(
    settings: &Settings,
    claude_path: &std::path::Path,
    cursor_path: &std::path::Path,
    cursor_shim: &std::path::Path,
) {
    if !settings.auto_uninstall_hooks_on_exit {
        return;
    }
    if settings.hooks_user_enabled && claude_path.exists() {
        match uninstall_hooks_at(settings.port, claude_path) {
            Ok(()) => eprintln!("[coach] cleanup: removed Claude Code hooks"),
            Err(e) => eprintln!("[coach] cleanup: Claude hook removal failed: {e}"),
        }
    }
    if settings.cursor_hooks_user_enabled && cursor_path.exists() {
        match uninstall_cursor_hooks_at(cursor_path, cursor_shim) {
            Ok(()) => eprintln!("[coach] cleanup: removed Cursor hooks"),
            Err(e) => eprintln!("[coach] cleanup: Cursor hook removal failed: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── has_http_hook helper ────────────────────────────────────────────

    /// has_http_hook should find a matching entry when the URL and type
    /// are present in the nested hooks array.
    #[test]
    fn has_http_hook_finds_matching_entry() {
        let entries: Vec<serde_json::Value> = vec![serde_json::json!({
            "hooks": [{"type": "http", "url": "http://localhost:7700/hook/stop"}]
        })];
        assert!(has_http_hook(&entries, "http://localhost:7700/hook/stop"));
    }

    /// has_http_hook should return false when the URL doesn't match,
    /// even if the structure is correct.
    #[test]
    fn has_http_hook_rejects_non_matching_url() {
        let entries: Vec<serde_json::Value> = vec![serde_json::json!({
            "hooks": [{"type": "http", "url": "http://localhost:7700/hook/stop"}]
        })];
        assert!(!has_http_hook(&entries, "http://localhost:9999/hook/stop"));
    }

    /// has_http_hook on an empty array should return false.
    #[test]
    fn has_http_hook_returns_false_for_empty_array() {
        let entries: Vec<serde_json::Value> = vec![];
        assert!(!has_http_hook(&entries, "http://localhost:7700/hook/stop"));
    }

    /// has_http_hook should ignore entries with a non-http type.
    #[test]
    fn has_http_hook_ignores_non_http_types() {
        let entries: Vec<serde_json::Value> = vec![serde_json::json!({
            "hooks": [{"type": "command", "url": "http://localhost:7700/hook/stop"}]
        })];
        assert!(!has_http_hook(&entries, "http://localhost:7700/hook/stop"));
    }

    // ── check_hook_status with path ─────────────────────────────────────

    /// When the settings file doesn't exist, all hooks should report
    /// as not installed.
    #[test]
    fn check_hook_status_reports_not_installed_for_missing_file() {
        let status = check_hook_status_at(7700, std::path::Path::new("/nonexistent/path.json"));
        assert!(!status.installed);
        assert!(status.hooks.iter().all(|h| !h.installed));
        // PermissionRequest, Stop, PostToolUse, UserPromptSubmit
        // (SessionStart was removed — see comment on expected_hook_urls)
        assert_eq!(status.hooks.len(), 4);
    }

    /// After install_hooks_at writes hooks to a temp file, check_hook_status_at
    /// should report all hooks as installed. This tests the install/check roundtrip.
    #[test]
    fn install_then_check_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        install_hooks_at(7700, &path).unwrap();
        let status = check_hook_status_at(7700, &path);

        assert!(status.installed);
        assert!(status.hooks.iter().all(|h| h.installed));
    }

    /// Installing hooks twice should be idempotent — the second call
    /// should not duplicate hook entries.
    #[test]
    fn install_hooks_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        install_hooks_at(7700, &path).unwrap();
        install_hooks_at(7700, &path).unwrap();

        let content: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

        // Each event should have exactly one entry.
        for event in ["PermissionRequest", "Stop", "PostToolUse", "UserPromptSubmit"] {
            let arr = content["hooks"][event].as_array().unwrap();
            assert_eq!(arr.len(), 1, "event {event} should have exactly 1 entry after double install");
        }
    }

    /// Installing hooks should preserve existing content in the settings file.
    #[test]
    fn install_hooks_preserves_existing_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        // Write a settings file with some existing config.
        let existing = serde_json::json!({
            "permissions": {"allow": ["Bash"]},
            "hooks": {
                "Stop": [{"hooks": [{"type": "command", "command": "echo stopped"}]}]
            }
        });
        std::fs::write(&path, serde_json::to_string_pretty(&existing).unwrap()).unwrap();

        install_hooks_at(7700, &path).unwrap();

        let content: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

        // Existing permissions should still be there.
        assert_eq!(content["permissions"]["allow"][0], "Bash");
        // Existing command hook should still be there alongside the new http hook.
        let stop_entries = content["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop_entries.len(), 2, "existing command hook + new http hook");
    }

    // ── uninstall_hooks ────────────────────────────────────────────────

    /// install then uninstall should leave no coach hooks, and
    /// check_hook_status should report not installed.
    #[test]
    fn uninstall_reverses_install() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        install_hooks_at(7700, &path).unwrap();
        uninstall_hooks_at(7700, &path).unwrap();

        let status = check_hook_status_at(7700, &path);
        assert!(!status.installed);
        assert!(status.hooks.iter().all(|h| !h.installed));
    }

    /// Uninstall should preserve other hooks and settings.
    #[test]
    fn uninstall_preserves_other_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let existing = serde_json::json!({
            "permissions": {"allow": ["Bash"]},
            "hooks": {
                "Stop": [{"hooks": [{"type": "command", "command": "echo stopped"}]}]
            }
        });
        std::fs::write(&path, serde_json::to_string_pretty(&existing).unwrap()).unwrap();

        install_hooks_at(7700, &path).unwrap();
        uninstall_hooks_at(7700, &path).unwrap();

        let content: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

        // Permissions untouched.
        assert_eq!(content["permissions"]["allow"][0], "Bash");
        // The user's command hook on Stop should survive.
        let stop_entries = content["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop_entries.len(), 1);
        assert_eq!(stop_entries[0]["hooks"][0]["type"], "command");
    }

    /// Uninstall cleans up empty event arrays (coach-only events get removed).
    #[test]
    fn uninstall_removes_empty_event_arrays() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        install_hooks_at(7700, &path).unwrap();
        uninstall_hooks_at(7700, &path).unwrap();

        let content: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

        let hooks = content["hooks"].as_object().unwrap();
        assert!(hooks.is_empty(), "all coach-only events should be cleaned up");
    }

    // ── sync_managed_hooks ──────────────────────────────────────────────

    /// On a clean settings file with no Coach hooks AND no recorded
    /// intent, sync does nothing — first install must be explicit.
    #[test]
    fn sync_does_nothing_when_unenabled_and_no_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "{}").unwrap();

        let mut user_enabled = false;
        let added = sync_managed_hooks_at(7700, &path, &mut user_enabled).unwrap();
        assert!(added.is_empty());
        assert!(!user_enabled, "no migration when nothing on disk");

        let status = check_hook_status_at(7700, &path);
        assert!(!status.installed);
    }

    /// Legacy migration: hooks were installed by an older Coach (or by a
    /// previous session before the intent flag existed). Sync adopts them
    /// — flips the flag, fills in any missing managed hooks, and reports
    /// the additions.
    #[test]
    fn sync_migrates_legacy_install_and_fills_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let partial = serde_json::json!({
            "hooks": {
                "PostToolUse": [{
                    "hooks": [{"type": "http", "url": "http://localhost:7700/hook/post-tool-use"}]
                }]
            }
        });
        std::fs::write(&path, serde_json::to_string_pretty(&partial).unwrap()).unwrap();

        let mut user_enabled = false;
        let added = sync_managed_hooks_at(7700, &path, &mut user_enabled).unwrap();

        assert!(user_enabled, "legacy install flips intent flag on");
        let mut sorted = added.clone();
        sorted.sort();
        assert_eq!(
            sorted,
            vec![
                "PermissionRequest".to_string(),
                "Stop".to_string(),
                "UserPromptSubmit".to_string(),
            ]
        );

        let status = check_hook_status_at(7700, &path);
        assert!(status.installed);
    }

    /// User clicked Install previously (intent=true). After auto-cleanup
    /// on exit, settings.json has no hooks. Next startup sync must
    /// reinstall them — this is the round-trip the whole feature is
    /// designed around.
    #[test]
    fn sync_reinstalls_after_cleanup_when_intent_true() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        // settings.json doesn't even exist — like after a clean uninstall
        // that removed all coach entries (or after the file was never
        // created).
        let mut user_enabled = true;
        let added = sync_managed_hooks_at(7700, &path, &mut user_enabled).unwrap();
        assert!(!added.is_empty());

        let status = check_hook_status_at(7700, &path);
        assert!(status.installed);
    }

    /// Idempotent: running sync twice returns no additions on the second call.
    #[test]
    fn sync_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let mut user_enabled = true;
        sync_managed_hooks_at(7700, &path, &mut user_enabled).unwrap();
        let added = sync_managed_hooks_at(7700, &path, &mut user_enabled).unwrap();
        assert!(added.is_empty(), "fully-installed state requires no sync");
    }

    /// User's pre-existing non-Coach hooks must survive a sync.
    #[test]
    fn sync_preserves_non_coach_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let mixed = serde_json::json!({
            "hooks": {
                "Stop": [
                    {"hooks": [{"type": "command", "command": "echo user-hook"}]},
                    {"hooks": [{"type": "http", "url": "http://localhost:7700/hook/stop"}]}
                ]
            }
        });
        std::fs::write(&path, serde_json::to_string_pretty(&mixed).unwrap()).unwrap();

        let mut user_enabled = false;
        sync_managed_hooks_at(7700, &path, &mut user_enabled).unwrap();

        let content: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let stop_entries = content["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop_entries.len(), 2);
        let kinds: Vec<&str> = stop_entries
            .iter()
            .map(|e| e["hooks"][0]["type"].as_str().unwrap())
            .collect();
        assert!(kinds.contains(&"command"));
        assert!(kinds.contains(&"http"));
    }

    // ── cleanup_hooks_on_exit_at ────────────────────────────────────────

    /// The whole-cycle property test: install hooks, run the cleanup
    /// (mimicking app shutdown), then sync (mimicking next startup) — the
    /// final on-disk state should match the original install. This is the
    /// regression guard for the "stop hook → HTTP undefined" bug that
    /// motivated the feature.
    #[test]
    fn install_cleanup_sync_roundtrip_matches_original() {
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join("claude_settings.json");
        let cursor_hooks = dir.path().join("cursor_hooks.json");
        let cursor_shim = dir.path().join("coach-cursor-hook.sh");

        // User opts in to both surfaces.
        install_hooks_at(7700, &claude).unwrap();
        install_cursor_hooks_at(7700, &cursor_hooks, &cursor_shim).unwrap();
        let original_claude = std::fs::read_to_string(&claude).unwrap();
        let original_cursor = std::fs::read_to_string(&cursor_hooks).unwrap();

        // Simulate app shutdown with auto-uninstall enabled.
        let settings = Settings {
            auto_uninstall_hooks_on_exit: true,
            hooks_user_enabled: true,
            cursor_hooks_user_enabled: true,
            port: 7700,
            ..Settings::default()
        };
        cleanup_hooks_on_exit_at(&settings, &claude, &cursor_hooks, &cursor_shim);

        // After cleanup, neither file should still contain managed hooks.
        assert!(!check_hook_status_at(7700, &claude).installed);
        assert!(!check_cursor_hook_status_at(&cursor_hooks, &cursor_shim).installed);

        // Simulate next startup: sync re-installs based on intent flags.
        let mut hooks_intent = true;
        let mut cursor_intent = true;
        sync_managed_hooks_at(7700, &claude, &mut hooks_intent).unwrap();
        sync_managed_cursor_hooks_at(7700, &cursor_hooks, &cursor_shim, &mut cursor_intent)
            .unwrap();

        // The end state should be byte-equivalent to the original install.
        // (Well — order-equivalent. Both are written by the same install_*_at
        // path, so the JSON should match exactly.)
        assert_eq!(std::fs::read_to_string(&claude).unwrap(), original_claude);
        assert_eq!(std::fs::read_to_string(&cursor_hooks).unwrap(), original_cursor);
    }

    /// When auto-uninstall is disabled, cleanup is a no-op even with
    /// intent flags set. This is the opt-out path the user requested.
    #[test]
    fn cleanup_is_noop_when_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join("claude_settings.json");
        let cursor_hooks = dir.path().join("cursor_hooks.json");
        let cursor_shim = dir.path().join("coach-cursor-hook.sh");

        install_hooks_at(7700, &claude).unwrap();
        install_cursor_hooks_at(7700, &cursor_hooks, &cursor_shim).unwrap();

        let settings = Settings {
            auto_uninstall_hooks_on_exit: false,
            hooks_user_enabled: true,
            cursor_hooks_user_enabled: true,
            port: 7700,
            ..Settings::default()
        };
        cleanup_hooks_on_exit_at(&settings, &claude, &cursor_hooks, &cursor_shim);

        assert!(check_hook_status_at(7700, &claude).installed);
        assert!(check_cursor_hook_status_at(&cursor_hooks, &cursor_shim).installed);
    }

    /// Cleanup ignores surfaces the user never opted in to. If only Claude
    /// hooks are managed, Cursor hooks.json must not be touched.
    #[test]
    fn cleanup_only_touches_enabled_surfaces() {
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join("claude_settings.json");
        let cursor_hooks = dir.path().join("cursor_hooks.json");
        let cursor_shim = dir.path().join("coach-cursor-hook.sh");

        install_hooks_at(7700, &claude).unwrap();
        install_cursor_hooks_at(7700, &cursor_hooks, &cursor_shim).unwrap();
        let cursor_before = std::fs::read_to_string(&cursor_hooks).unwrap();

        let settings = Settings {
            auto_uninstall_hooks_on_exit: true,
            hooks_user_enabled: true,
            cursor_hooks_user_enabled: false,
            port: 7700,
            ..Settings::default()
        };
        cleanup_hooks_on_exit_at(&settings, &claude, &cursor_hooks, &cursor_shim);

        assert!(!check_hook_status_at(7700, &claude).installed);
        assert_eq!(std::fs::read_to_string(&cursor_hooks).unwrap(), cursor_before);
    }
}
