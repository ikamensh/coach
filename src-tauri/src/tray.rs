use crate::state::{CoachMode, SharedState, EVENT_STATE_UPDATED};
use tauri::{
    image::Image,
    menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem},
    tray::TrayIconBuilder,
    Emitter, Manager,
};

const TRAY_ID: &str = "coach-tray";

/// Generate a small colored circle icon for the system tray.
fn circle_icon(r: u8, g: u8, b: u8) -> Image<'static> {
    const SIZE: u32 = 22;
    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];
    let center = SIZE as f32 / 2.0;
    let radius = center - 2.0;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 - center + 0.5;
            let dy = y as f32 - center + 0.5;
            let dist = (dx * dx + dy * dy).sqrt();
            if dist <= radius {
                let idx = ((y * SIZE + x) * 4) as usize;
                rgba[idx] = r;
                rgba[idx + 1] = g;
                rgba[idx + 2] = b;
                rgba[idx + 3] = 255;
            }
        }
    }
    Image::new_owned(rgba, SIZE, SIZE)
}

/// Update the tray icon to reflect the current default mode.
pub fn update_icon(app: &tauri::AppHandle, mode: &CoachMode) {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let (icon, tooltip) = match mode {
            CoachMode::Present => (circle_icon(16, 185, 129), "Coach — Present"),
            CoachMode::Away => (circle_icon(245, 158, 11), "Coach — Away"),
        };
        let _ = tray.set_icon(Some(icon));
        let _ = tray.set_tooltip(Some(tooltip));
    }
}

fn toggle_all(state: &SharedState, handle: &tauri::AppHandle) {
    let state = state.clone();
    let handle = handle.clone();
    tauri::async_runtime::spawn(async move {
        let mut s = state.write().await;
        let new_mode = match s.default_mode {
            CoachMode::Present => CoachMode::Away,
            CoachMode::Away => CoachMode::Present,
        };
        s.set_all_modes(new_mode);
        update_icon(&handle, &new_mode);
        let _ = handle.emit(EVENT_STATE_UPDATED, s.snapshot());
    });
}

fn show_window(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

pub fn setup(app: &mut tauri::App, state: SharedState) -> Result<(), Box<dyn std::error::Error>> {
    let toggle = MenuItemBuilder::with_id("toggle", "Toggle Present / Away").build(app)?;
    let show = MenuItemBuilder::with_id("show", "Show Window").build(app)?;
    let quit = PredefinedMenuItem::quit(app, Some("Quit"))?;

    let menu = MenuBuilder::new(app)
        .item(&toggle)
        .item(&show)
        .separator()
        .item(&quit)
        .build()?;

    let menu_state = state;

    let _tray = TrayIconBuilder::with_id(TRAY_ID)
        .icon(circle_icon(16, 185, 129))
        .tooltip("Coach — Present")
        .menu(&menu)
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "toggle" => toggle_all(&menu_state, app),
            "show" => show_window(app),
            _ => {}
        })
        .build(app)?;

    Ok(())
}
