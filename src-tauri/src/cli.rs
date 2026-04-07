//! CLI dispatcher: parses argv and routes to subcommands without ever
//! touching `tauri::*`. Returns `Some(exit_code)` when a CLI subcommand
//! ran, `None` when the binary should fall through to launching the GUI.
//!
//! Two backends for state-mutating commands:
//!   • **HTTP** when Coach is running. The CLI POSTs to `/api/...` so
//!     the GUI's in-memory state stays consistent and the file gets
//!     persisted by the same code that the Tauri commands use.
//!   • **File** when Coach isn't running. The CLI reads/writes
//!     `~/.coach/settings.json` directly via `Settings::load_from` /
//!     `save_to`.
//!
//! Live state (`coach status`, `coach mode`) is HTTP-only — it has no
//! meaning without a running daemon, so we error cleanly instead of
//! pretending.

use crate::path_install;
use crate::replay;
use crate::settings::{self, CoachRule, EngineMode, ModelConfig, Settings};

const HELP: &str = "\
coach — Claude Code companion (GUI + CLI)

USAGE:
    coach                                  launch the GUI
    coach <command> [args]                 run a CLI subcommand

COMMANDS:
    serve [--port N]                       run the daemon headless (no GUI / no tray)
    status [--json]                        show live state (requires running Coach)
    mode <away|present> [--pid N]          set away/present mode (requires running Coach)

    hooks status                           show Claude Code hook installation status
    hooks install                          install Coach hooks into ~/.claude/settings.json
    hooks uninstall                        remove Coach hooks

    hooks cursor status                    show Cursor Agent hooks (~/.cursor/hooks.json)
    hooks cursor install                   add curl forwarders to Cursor hooks.json
    hooks cursor uninstall                 remove Coach-managed Cursor hook entries

    path install [--dir DIR]               install a `coach` shim on PATH
    path uninstall                         remove the PATH shim
    path status                            show PATH shim status

    config get [<key>]                     read settings
    config set priorities <a,b,c>          replace priorities list
    config set model <provider> <model>    set the LLM model
    config set api-token <provider> <tok>  store an API token
    config set coach-mode <rules|llm>      switch the coach engine
    config set rule <id> <on|off>          enable/disable a rule

    sessions list [--limit N] [--json]     list saved Claude Code sessions
    replay <session-id> [--mode away|present] [--json]

    help, --help, -h                       this message
    version, --version, -V                 print version
";

/// Inspect `std::env::args` and dispatch a CLI subcommand if one is
/// present. Returns `Some(code)` for the process exit code if a CLI
/// subcommand ran; returns `None` if the caller should launch the GUI.
pub fn dispatch() -> Option<i32> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    dispatch_with_args(args)
}

/// Pure entry point used by integration tests. Same logic as `dispatch`
/// but takes the argv vector directly so tests don't have to mutate
/// process state.
pub fn dispatch_with_args(args: Vec<String>) -> Option<i32> {
    if args.is_empty() {
        return None;
    }
    let first = args[0].as_str();
    match first {
        // GUI flags pass through unchanged.
        "--devtools" | "gui" => None,
        "-h" | "--help" | "help" => {
            println!("{HELP}");
            Some(0)
        }
        "-V" | "--version" | "version" => {
            println!("coach {}", env!("CARGO_PKG_VERSION"));
            Some(0)
        }
        "serve" => Some(run(cmd_serve(&args[1..]))),
        "status" => Some(run(cmd_status(&args[1..]))),
        "mode" => Some(run(cmd_mode(&args[1..]))),
        "hooks" => Some(run(cmd_hooks(&args[1..]))),
        "path" => Some(run(cmd_path(&args[1..]))),
        "config" => Some(run(cmd_config(&args[1..]))),
        "sessions" => Some(run(cmd_sessions(&args[1..]))),
        "replay" => Some(run(cmd_replay(&args[1..]))),
        unknown => {
            eprintln!("coach: unknown command '{unknown}'\n\nUse `coach help` for usage.");
            Some(2)
        }
    }
}

/// Print an error to stderr and turn `Result<(), String>` into an exit
/// code. Success → 0, error → 1. Centralised so every subcommand prints
/// errors the same way.
fn run(result: Result<(), String>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("coach: {e}");
            1
        }
    }
}

// ── serve (headless daemon) ─────────────────────────────────────────────

/// Run the daemon without Tauri. Each invocation owns its own
/// multi-thread tokio runtime — server + scanner need to make progress
/// concurrently. Blocks until Ctrl-C or one of the long-running tasks
/// exits.
fn cmd_serve(args: &[String]) -> Result<(), String> {
    let port_override = parse_named_u32(args, "--port")?.map(|p| p as u16);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;
    runtime.block_on(crate::serve(port_override));
    Ok(())
}

// ── status / mode (HTTP-only) ───────────────────────────────────────────

fn cmd_status(args: &[String]) -> Result<(), String> {
    let json_out = args.iter().any(|a| a == "--json");
    let snap = http_get_state()?;

    if json_out {
        println!("{}", serde_json::to_string_pretty(&snap).map_err(|e| e.to_string())?);
        return Ok(());
    }

    let sessions = snap["sessions"].as_array().cloned().unwrap_or_default();
    println!("coach: {} session(s)", sessions.len());
    println!("port:    {}", snap["port"]);
    println!("model:   {} / {}", snap["model"]["provider"], snap["model"]["model"]);
    println!("engine:  {}", snap["coach_mode"]);
    println!("priorities: {}", snap["priorities"]);
    for s in &sessions {
        println!(
            "  pid={} mode={} display={} events={}",
            s["pid"], s["mode"], s["display_name"], s["event_count"]
        );
    }
    Ok(())
}

fn cmd_mode(args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        return Err("usage: coach mode <away|present> [--pid N]".into());
    }
    let mode = match args[0].as_str() {
        "away" => "away",
        "present" => "present",
        other => return Err(format!("invalid mode '{other}' — expected away|present")),
    };
    let pid: Option<u32> = parse_named_u32(&args[1..], "--pid")?;

    let port = configured_port();
    require_server(port)?;
    let body = serde_json::json!({ "mode": mode });
    let url = match pid {
        Some(p) => format!("http://127.0.0.1:{port}/api/sessions/{p}/mode"),
        None => format!("http://127.0.0.1:{port}/api/sessions/mode"),
    };
    http_post_json(&url, &body)?;
    match pid {
        Some(p) => println!("set pid={p} to {mode}"),
        None => println!("set all sessions to {mode}"),
    }
    Ok(())
}

// ── hooks (file mode — same code path as the Tauri command) ─────────────

fn cmd_hooks(args: &[String]) -> Result<(), String> {
    let port = configured_port();
    if args.first().map(String::as_str) == Some("cursor") {
        let verb = args.get(1).map(String::as_str).unwrap_or("status");
        return match verb {
            "status" => {
                let s = settings::check_cursor_hook_status();
                println!("settings file: {}", s.path);
                println!("all installed: {}", s.installed);
                for h in &s.hooks {
                    let mark = if h.installed { "✓" } else { "·" };
                    println!("  {mark} {} → {}", h.event, h.url);
                }
                Ok(())
            }
            "install" => {
                settings::install_cursor_hooks(port)?;
                let s = settings::check_cursor_hook_status();
                println!(
                    "installed {} Cursor hook(s) into {}",
                    s.hooks.iter().filter(|h| h.installed).count(),
                    s.path
                );
                Ok(())
            }
            "uninstall" => {
                settings::uninstall_cursor_hooks()?;
                println!("removed coach Cursor hooks from ~/.cursor/hooks.json");
                Ok(())
            }
            other => Err(format!(
                "usage: coach hooks cursor <status|install|uninstall>; got '{other}'"
            )),
        };
    }

    let verb = args.first().map(String::as_str).unwrap_or("status");
    match verb {
        "status" => {
            let s = settings::check_hook_status(port);
            println!("settings file: {}", s.path);
            println!("all installed: {}", s.installed);
            for h in &s.hooks {
                let mark = if h.installed { "✓" } else { "·" };
                println!("  {mark} {} → {}", h.event, h.url);
            }
            Ok(())
        }
        "install" => {
            settings::install_hooks(port)?;
            let s = settings::check_hook_status(port);
            println!("installed {} hook(s) into {}", s.hooks.iter().filter(|h| h.installed).count(), s.path);
            Ok(())
        }
        "uninstall" => {
            settings::uninstall_hooks(port)?;
            println!("removed coach hooks from ~/.claude/settings.json");
            Ok(())
        }
        other => Err(format!(
            "usage: coach hooks <status|install|uninstall> | hooks cursor <...>; got '{other}'"
        )),
    }
}

// ── path install / uninstall / status ───────────────────────────────────

fn cmd_path(args: &[String]) -> Result<(), String> {
    let verb = args.first().map(String::as_str).unwrap_or("status");
    match verb {
        "install" => {
            let dir_override = parse_named_string(&args[1..], "--dir")?;
            let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
            let dir = match dir_override {
                Some(d) => std::path::PathBuf::from(d),
                None => path_install::default_install_dir()?,
            };
            path_install::install_at(&dir, &exe)?;
            let status = path_install::status_at(&dir, &exe);
            println!("installed: {}", status.install_path);
            if let Some(t) = &status.target {
                println!("target:    {t}");
            }
            if !status.on_path {
                println!();
                #[cfg(windows)]
                {
                    println!("⚠  {} is not on PATH.", dir.display());
                    println!("   Add it via System Properties → Environment Variables, then open a new terminal.");
                }
                #[cfg(not(windows))]
                {
                    println!("⚠  {} is not on $PATH.", dir.display());
                    println!("   Add this to your shell rc:");
                    println!("     export PATH=\"{}:$PATH\"", dir.display());
                }
            }
            Ok(())
        }
        "uninstall" => {
            let status = path_install::uninstall()?;
            println!("removed: {}", status.install_path);
            Ok(())
        }
        "status" => {
            let status = path_install::status()?;
            println!("install path:    {}", status.install_path);
            println!("installed:       {}", status.installed);
            if let Some(t) = &status.target {
                println!("target:          {t}");
            }
            println!("matches running: {}", status.matches_current_exe);
            println!("on $PATH:        {}", status.on_path);
            Ok(())
        }
        other => Err(format!("usage: coach path <install|uninstall|status>; got '{other}'")),
    }
}

// ── config get/set ──────────────────────────────────────────────────────

fn cmd_config(args: &[String]) -> Result<(), String> {
    let verb = args
        .first()
        .ok_or("usage: coach config <get|set> ...")?
        .as_str();
    match verb {
        "get" => cmd_config_get(&args[1..]),
        "set" => cmd_config_set(&args[1..]),
        other => Err(format!("usage: coach config <get|set>; got '{other}'")),
    }
}

fn cmd_config_get(args: &[String]) -> Result<(), String> {
    let s = Settings::load();
    let key = args.first().map(String::as_str);
    match key {
        None | Some("all") => {
            println!("{}", serde_json::to_string_pretty(&s).map_err(|e| e.to_string())?);
        }
        Some("priorities") => {
            for (i, p) in s.priorities.iter().enumerate() {
                println!("{}. {p}", i + 1);
            }
        }
        Some("model") => println!("{} / {}", s.model.provider, s.model.model),
        Some("coach-mode") => println!("{:?}", s.coach_mode),
        Some("port") => println!("{}", s.port),
        Some("rules") => {
            for r in &s.rules {
                println!("{} = {}", r.id, if r.enabled { "on" } else { "off" });
            }
        }
        Some(other) => return Err(format!("unknown config key '{other}'")),
    }
    Ok(())
}

fn cmd_config_set(args: &[String]) -> Result<(), String> {
    let key = args.first().ok_or("usage: coach config set <key> ...")?.as_str();
    let rest = &args[1..];
    match key {
        "priorities" => {
            let val = rest.first().ok_or("usage: coach config set priorities <a,b,c>")?;
            let priorities: Vec<String> = val.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            apply_setting(
                |port| {
                    http_post_json(
                        &format!("http://127.0.0.1:{port}/api/config/priorities"),
                        &serde_json::json!({ "priorities": priorities }),
                    )
                    .map(|_| ())
                },
                || {
                    let mut s = Settings::load();
                    s.priorities = priorities.clone();
                    s.save();
                    Ok(())
                },
            )?;
            println!("priorities = {priorities:?}");
            Ok(())
        }
        "model" => {
            if rest.len() < 2 {
                return Err("usage: coach config set model <provider> <model>".into());
            }
            let model = ModelConfig {
                provider: rest[0].clone(),
                model: rest[1].clone(),
            };
            apply_setting(
                |port| {
                    http_post_json(
                        &format!("http://127.0.0.1:{port}/api/config/model"),
                        &serde_json::to_value(&model).unwrap(),
                    )
                    .map(|_| ())
                },
                || {
                    let mut s = Settings::load();
                    s.model = model.clone();
                    s.save();
                    Ok(())
                },
            )?;
            println!("model = {} / {}", model.provider, model.model);
            Ok(())
        }
        "api-token" => {
            if rest.len() < 2 {
                return Err("usage: coach config set api-token <provider> <token>".into());
            }
            let provider = rest[0].clone();
            let token = rest[1].clone();
            apply_setting(
                |port| {
                    http_post_json(
                        &format!("http://127.0.0.1:{port}/api/config/api-token"),
                        &serde_json::json!({ "provider": provider, "token": token }),
                    )
                    .map(|_| ())
                },
                || {
                    let mut s = Settings::load();
                    if token.is_empty() {
                        s.api_tokens.remove(&provider);
                    } else {
                        s.api_tokens.insert(provider.clone(), token.clone());
                    }
                    s.save();
                    Ok(())
                },
            )?;
            println!("api-token for {provider} updated");
            Ok(())
        }
        "coach-mode" => {
            let val = rest.first().ok_or("usage: coach config set coach-mode <rules|llm>")?;
            let mode = match val.as_str() {
                "rules" => EngineMode::Rules,
                "llm" => EngineMode::Llm,
                other => return Err(format!("invalid coach-mode '{other}' — expected rules|llm")),
            };
            apply_setting(
                |port| {
                    http_post_json(
                        &format!("http://127.0.0.1:{port}/api/config/coach-mode"),
                        &serde_json::json!({ "coach_mode": mode }),
                    )
                    .map(|_| ())
                },
                || {
                    let mut s = Settings::load();
                    s.coach_mode = mode.clone();
                    s.save();
                    Ok(())
                },
            )?;
            println!("coach-mode = {val}");
            Ok(())
        }
        "rule" => {
            if rest.len() < 2 {
                return Err("usage: coach config set rule <id> <on|off>".into());
            }
            let id = rest[0].clone();
            let enabled = match rest[1].as_str() {
                "on" | "true" | "1" => true,
                "off" | "false" | "0" => false,
                other => return Err(format!("invalid rule state '{other}' — expected on|off")),
            };
            // Merge into existing rules so we don't drop other rule entries.
            let mut s = Settings::load();
            let mut found = false;
            for r in &mut s.rules {
                if r.id == id {
                    r.enabled = enabled;
                    found = true;
                }
            }
            if !found {
                s.rules.push(CoachRule { id: id.clone(), enabled });
            }
            let merged_rules = s.rules.clone();
            apply_setting(
                |port| {
                    http_post_json(
                        &format!("http://127.0.0.1:{port}/api/config/rules"),
                        &serde_json::json!({ "rules": merged_rules }),
                    )
                    .map(|_| ())
                },
                || {
                    s.save();
                    Ok(())
                },
            )?;
            println!("rule {id} = {}", if enabled { "on" } else { "off" });
            Ok(())
        }
        other => Err(format!("unknown config key '{other}'")),
    }
}

// ── sessions list / replay (file mode) ──────────────────────────────────

fn cmd_sessions(args: &[String]) -> Result<(), String> {
    let verb = args.first().map(String::as_str).unwrap_or("list");
    match verb {
        "list" => {
            let limit = parse_named_u32(&args[1..], "--limit")?.unwrap_or(50) as usize;
            let json_out = args.iter().any(|a| a == "--json");
            let sessions = replay::list_sessions(limit);
            if json_out {
                println!("{}", serde_json::to_string_pretty(&sessions).map_err(|e| e.to_string())?);
                return Ok(());
            }
            println!("{} saved session(s):", sessions.len());
            for s in &sessions {
                println!(
                    "  {} ({} msgs) — {}",
                    s.id,
                    s.message_count,
                    if s.topic.is_empty() { "(no topic)" } else { &s.topic }
                );
            }
            Ok(())
        }
        other => Err(format!("usage: coach sessions list [--limit N] [--json]; got '{other}'")),
    }
}

fn cmd_replay(args: &[String]) -> Result<(), String> {
    let session_id = args.first().ok_or("usage: coach replay <session-id> [--mode away|present]")?;
    let mode = parse_named_string(&args[1..], "--mode")?.unwrap_or_else(|| "away".to_string());
    let json_out = args.iter().any(|a| a == "--json");

    let priorities = Settings::load().priorities;
    let result = replay::replay_session(session_id, &mode, &priorities)?;

    if json_out {
        println!("{}", serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?);
        return Ok(());
    }

    println!("session: {}", result.session_id);
    println!("topic:   {}", if result.topic.is_empty() { "(none)" } else { &result.topic });
    println!("cwd:     {}", result.cwd);
    println!("messages: {} ({} user / {} assistant)", result.message_count, result.user_message_count, result.assistant_message_count);
    println!("events:  {}", result.event_count);
    if let Some(idx) = result.first_intervention_index {
        println!("first intervention at event {idx}");
    } else {
        println!("no interventions");
    }
    Ok(())
}

// ── helpers: argv parsing ───────────────────────────────────────────────

fn parse_named_u32(args: &[String], name: &str) -> Result<Option<u32>, String> {
    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        if a == name {
            let v = iter.next().ok_or_else(|| format!("{name} requires a value"))?;
            return v
                .parse::<u32>()
                .map(Some)
                .map_err(|e| format!("{name}: {e}"));
        }
    }
    Ok(None)
}

fn parse_named_string(args: &[String], name: &str) -> Result<Option<String>, String> {
    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        if a == name {
            let v = iter.next().ok_or_else(|| format!("{name} requires a value"))?;
            return Ok(Some(v.clone()));
        }
    }
    Ok(None)
}

// ── helpers: HTTP with embedded runtime ─────────────────────────────────

/// Settings-resolved port. Falls back to the default 7700 if no settings
/// file exists. Cheap — `Settings::load_from` is just a file read.
fn configured_port() -> u16 {
    Settings::load().port
}

/// Probe `http://127.0.0.1:{port}/version` with a short timeout. Returns
/// true iff the request succeeds with a 2xx response. Used by the
/// settings commands to decide between HTTP and direct-file mode.
pub fn server_running(port: u16) -> bool {
    runtime().block_on(async {
        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(300))
            .build()
        {
            Ok(c) => c,
            Err(_) => return false,
        };
        client
            .get(format!("http://127.0.0.1:{port}/version"))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    })
}

/// Run `if_server` if Coach is up; otherwise run `if_file` against the
/// settings file directly. Centralises the probe-then-fallback rule for
/// every config setter.
fn apply_setting<F, G>(if_server: F, if_file: G) -> Result<(), String>
where
    F: FnOnce(u16) -> Result<(), String>,
    G: FnOnce() -> Result<(), String>,
{
    let port = configured_port();
    if server_running(port) {
        if_server(port)
    } else {
        if_file()
    }
}

fn require_server(port: u16) -> Result<(), String> {
    if server_running(port) {
        Ok(())
    } else {
        Err(format!(
            "Coach is not running on port {port}. Start the GUI first."
        ))
    }
}

fn http_get_state() -> Result<serde_json::Value, String> {
    let port = configured_port();
    require_server(port)?;
    runtime().block_on(async {
        let client = reqwest::Client::new();
        let resp = client
            .get(format!("http://127.0.0.1:{port}/api/state"))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("server returned {}", resp.status()));
        }
        resp.json::<serde_json::Value>().await.map_err(|e| e.to_string())
    })
}

fn http_post_json(url: &str, body: &serde_json::Value) -> Result<serde_json::Value, String> {
    runtime().block_on(async {
        let client = reqwest::Client::new();
        let resp = client
            .post(url)
            .json(body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("server returned {status}: {body}"));
        }
        resp.json::<serde_json::Value>()
            .await
            .or_else(|_| Ok(serde_json::Value::Null))
    })
}

/// Build a fresh single-thread tokio runtime for one CLI command. Cheap
/// (≈1 ms) and avoids needing a process-wide runtime in the CLI cold path.
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to create tokio runtime for CLI")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Empty argv → fall through to GUI.
    #[test]
    fn no_args_returns_none() {
        assert_eq!(dispatch_with_args(vec![]), None);
    }

    /// `coach --devtools` is the existing GUI flag — keep falling through.
    #[test]
    fn devtools_flag_returns_none() {
        assert_eq!(dispatch_with_args(vec!["--devtools".into()]), None);
    }

    /// `coach gui` is the explicit GUI launcher — keep falling through.
    #[test]
    fn gui_subcommand_returns_none() {
        assert_eq!(dispatch_with_args(vec!["gui".into()]), None);
    }

    /// Help and version subcommands always exit 0 without GUI.
    #[test]
    fn help_returns_zero() {
        assert_eq!(dispatch_with_args(vec!["help".into()]), Some(0));
        assert_eq!(dispatch_with_args(vec!["--help".into()]), Some(0));
        assert_eq!(dispatch_with_args(vec!["-h".into()]), Some(0));
    }

    #[test]
    fn version_returns_zero() {
        assert_eq!(dispatch_with_args(vec!["version".into()]), Some(0));
        assert_eq!(dispatch_with_args(vec!["--version".into()]), Some(0));
        assert_eq!(dispatch_with_args(vec!["-V".into()]), Some(0));
    }

    /// Unknown commands return exit 2 (POSIX convention for usage errors).
    #[test]
    fn unknown_command_returns_two() {
        assert_eq!(dispatch_with_args(vec!["wat".into()]), Some(2));
    }

    /// parse_named_u32 finds the value after the named flag.
    #[test]
    fn parse_named_u32_basic() {
        let args = vec!["--pid".to_string(), "1234".to_string()];
        assert_eq!(parse_named_u32(&args, "--pid").unwrap(), Some(1234));
    }

    #[test]
    fn parse_named_u32_missing_returns_none() {
        let args = vec!["--other".to_string(), "x".to_string()];
        assert_eq!(parse_named_u32(&args, "--pid").unwrap(), None);
    }

    #[test]
    fn parse_named_u32_no_value_errors() {
        let args = vec!["--pid".to_string()];
        assert!(parse_named_u32(&args, "--pid").is_err());
    }
}
