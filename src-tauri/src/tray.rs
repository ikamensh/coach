use crate::state::{CoachMode, SharedState};
use tauri::{
    menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Emitter, Manager,
};

fn toggle_all(state: &SharedState, handle: &tauri::AppHandle) {
    let state = state.clone();
    let handle = handle.clone();
    tauri::async_runtime::spawn(async move {
        let mut s = state.write().await;
        let new_mode = match s.default_mode {
            CoachMode::Present => CoachMode::Away,
            CoachMode::Away => CoachMode::Present,
        };
        s.default_mode = new_mode.clone();
        for session in s.sessions.values_mut() {
            session.mode = new_mode.clone();
        }
        let _ = handle.emit("coach-state-updated", s.snapshot());
    });
}

pub fn setup(app: &mut tauri::App, state: SharedState) -> Result<(), Box<dyn std::error::Error>> {
    let toggle = MenuItemBuilder::with_id("toggle", "Toggle All Present / Away").build(app)?;
    let show = MenuItemBuilder::with_id("show", "Show Window").build(app)?;
    let quit = PredefinedMenuItem::quit(app, Some("Quit"))?;

    let menu = MenuBuilder::new(app)
        .item(&toggle)
        .item(&show)
        .separator()
        .item(&quit)
        .build()?;

    let tray_state = state.clone();
    let menu_state = state;

    let _tray = TrayIconBuilder::new()
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("Coach")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_tray_icon_event(move |tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_all(&tray_state, tray.app_handle());
            }
        })
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "toggle" => toggle_all(&menu_state, app),
            "show" => {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }
            _ => {}
        })
        .build(app)?;

    Ok(())
}
