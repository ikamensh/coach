#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // CLI dispatch happens *before* anything from `tauri::*` runs. If a
    // CLI subcommand was invoked, we exit here with its status code and
    // never construct a webview, tray, or single-instance plugin.
    #[cfg(windows)]
    attach_parent_console();

    if let Some(code) = coach_lib::cli::dispatch() {
        std::process::exit(code);
    }
    coach_lib::run()
}

/// On Windows, the GUI binary is built with `windows_subsystem = "windows"`
/// so it has no attached console — `println!` from CLI mode would vanish.
/// Re-attach to the parent console so CLI output reaches the user. No-op
/// on non-Windows targets.
#[cfg(windows)]
fn attach_parent_console() {
    // SAFETY: AttachConsole is safe to call from a single-threaded main
    // before any other window/console activity. ATTACH_PARENT_PROCESS = -1.
    unsafe {
        // Inline the constant so we don't need a windows-sys dep.
        const ATTACH_PARENT_PROCESS: u32 = 0xFFFFFFFF;
        extern "system" {
            fn AttachConsole(dwProcessId: u32) -> i32;
        }
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}
