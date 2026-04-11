use coach_core::state::{self, CoachMode, SharedState};
use coach_core::EventEmitter;
use std::sync::Arc;
use tauri::{
    image::Image,
    menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem},
    tray::TrayIconBuilder,
    Manager,
};

const TRAY_ID: &str = "coach-tray";

/// Generate an iris-style tray icon: black pupil, colored iris with
/// concentric rings perturbed by radial fibers, dark limbus outline.
/// Rendered at 2x retina resolution with anti-aliased edges.
fn iris_icon(r: u8, g: u8, b: u8) -> Image<'static> {
    const SIZE: u32 = 44;
    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];
    let center = SIZE as f32 / 2.0;
    let outer = center - 1.5;
    let pupil = outer * 0.30;

    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 - center + 0.5;
            let dy = y as f32 - center + 0.5;
            let dist = (dx * dx + dy * dy).sqrt();
            if dist > outer + 1.0 {
                continue;
            }

            let idx = ((y * SIZE + x) * 4) as usize;

            // Anti-aliased outer edge and pupil edge.
            let outer_alpha = (outer + 0.5 - dist).clamp(0.0, 1.0);
            let pupil_alpha = (pupil + 0.5 - dist).clamp(0.0, 1.0);

            // Iris pattern (relevant outside pupil).
            let n = ((dist - pupil) / (outer - pupil)).clamp(0.0, 1.0);
            let angle = dy.atan2(dx);
            // Two-frequency angular fibers — gives the iris an irregular,
            // organic look instead of perfectly concentric rings.
            let fibers = (angle * 13.0).sin() * 0.20 + (angle * 7.0 + 1.7).cos() * 0.12;
            let rings = ((n * 6.0 + fibers * 1.8).sin() * 0.5 + 0.5) * 0.45 + 0.45;
            // Darken near limbus for a defined outer edge.
            let limbus = 1.0 - ((n - 0.85).max(0.0) * 6.5).min(0.7);
            let bright = (rings * limbus).clamp(0.0, 1.0);

            // Iris color, then blend pupil (black) over it via pupil_alpha.
            let inv_pupil = 1.0 - pupil_alpha;
            rgba[idx] = (r as f32 * bright * inv_pupil) as u8;
            rgba[idx + 1] = (g as f32 * bright * inv_pupil) as u8;
            rgba[idx + 2] = (b as f32 * bright * inv_pupil) as u8;
            rgba[idx + 3] = (outer_alpha * 255.0) as u8;
        }
    }
    Image::new_owned(rgba, SIZE, SIZE)
}

/// Update the tray icon to reflect the current default mode.
pub fn update_icon(app: &tauri::AppHandle, mode: &CoachMode) {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let (icon, tooltip) = match mode {
            CoachMode::Present => (iris_icon(16, 185, 129), "Coach — Present"),
            CoachMode::Away => (iris_icon(245, 158, 11), "Coach — Away"),
        };
        let _ = tray.set_icon(Some(icon));
        let _ = tray.set_tooltip(Some(tooltip));
    }
}

fn toggle_all(state: &SharedState, handle: &tauri::AppHandle) {
    let state = state.clone();
    let emitter = handle.state::<Arc<dyn EventEmitter>>().inner().clone();
    let handle = handle.clone();
    tauri::async_runtime::spawn(async move {
        let new_mode = state::mutate(&state, &emitter, |s| {
            let new_mode = match s.default_mode {
                CoachMode::Present => CoachMode::Away,
                CoachMode::Away => CoachMode::Present,
            };
            s.set_all_modes(new_mode);
            new_mode
        })
        .await;
        update_icon(&handle, &new_mode);
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
        .icon(iris_icon(16, 185, 129))
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
