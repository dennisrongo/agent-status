//! Menubar tray icon with a click-to-toggle dropdown window positioned
//! under the tray icon.

use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, LogicalPosition, Manager, Rect, WebviewWindow,
};
use tauri_plugin_positioner::{Position, WindowExt};

pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let show = MenuItem::with_id(app, "show", "Show / Hide", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;

    // Monochrome template icon → macOS tints it to match the menu bar
    // (light/dark) and keeps the transparent background, unlike the filled
    // app icon. Falls back to the window icon if decoding ever fails.
    let tray_icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))
        .unwrap_or_else(|_| app.default_window_icon().unwrap().clone());

    TrayIconBuilder::with_id("main-tray")
        .icon(tray_icon)
        .icon_as_template(true)
        .tooltip("Agent Usage Monitor")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "quit" => app.exit(0),
            // Menu-driven open has no click point; fall back to the cached
            // tray location inside `toggle_window`.
            "show" => toggle_window(app, None),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            tauri_plugin_positioner::on_tray_event(tray.app_handle(), &event);
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                rect,
                ..
            } = event
            {
                // The icon's bounding rect (physical pixels, top-left origin)
                // is the anchor: it tells us which display the menu bar is on
                // and exactly where the icon sits. Unlike the cursor `position`
                // field, it's converted into the same coordinate space the
                // window APIs use.
                toggle_window(tray.app_handle(), Some(rect));
            }
        })
        .build(app)?;

    Ok(())
}

fn toggle_window(app: &AppHandle, tray_rect: Option<Rect>) {
    let Some(window) = app.get_webview_window("main") else { return };
    if window.is_visible().unwrap_or(false) {
        let _ = window.hide();
        return;
    }

    position_dropdown(&window, tray_rect);
    let _ = window.show();
    // macOS can re-place a window onto a different monitor when it becomes
    // visible, undoing the position we set while it was hidden. Re-assert it
    // now that it's on-screen so the dropdown reliably lands on the display
    // whose menu bar was clicked.
    position_dropdown(&window, tray_rect);
    let _ = window.set_focus();
    refresh_on_open(app);
}

/// Hang the dropdown directly off the tray icon: top edge just below the menu
/// bar, right edge aligned with the icon's right edge so it extends down-left.
/// Anchoring the right edge (rather than centering) keeps the window on-screen,
/// since menu-bar icons sit near the top-right corner.
///
/// All math is done in **logical points** — the only coordinate space that's
/// uniform across displays of different scale (e.g. a 2x Retina laptop next to
/// a 1x external monitor). The result is sent as a `LogicalPosition`, which the
/// OS places directly without reinterpreting it against the window's current
/// display scale; that's what makes this correct on a mixed-DPI setup.
///
/// `tray_rect` is the icon's bounding rect in physical pixels (top-left global
/// origin). When it's `None` (the "Show / Hide" menu item, which carries no
/// geometry) we defer to the positioner plugin's cached tray location.
fn position_dropdown(window: &WebviewWindow, tray_rect: Option<Rect>) {
    let Some(rect) = tray_rect else {
        let _ = window.move_window_constrained(Position::TrayCenter);
        return;
    };

    // Icon geometry in physical pixels, at the icon display's scale.
    let pos = rect.position.to_physical::<f64>(1.0);
    let size = rect.size.to_physical::<f64>(1.0);

    // Converting the physical icon position to logical points requires the
    // scale of the display the icon is on — which we recover below.
    let Some(scale) = icon_display_scale(window, pos.x, pos.y) else {
        let _ = window.move_window_constrained(Position::TrayCenter);
        return;
    };

    let icon_right = (pos.x + size.width) / scale;
    // The icon spans the menu bar height, so its bottom edge is exactly where a
    // menu-bar dropdown should hang from — on this display, not the primary's.
    let menubar_bottom = (pos.y + size.height) / scale;

    // Window width in logical points (constant regardless of current display).
    let win_scale = window.scale_factor().unwrap_or(1.0).max(0.01);
    let win_w = window.outer_size().map(|s| s.width as f64).unwrap_or(0.0) / win_scale;

    let _ = window.set_position(LogicalPosition::new(icon_right - win_w, menubar_bottom));
}

/// Scale factor of the display the tray icon sits on.
///
/// The icon position arrives in physical pixels, but with mixed-DPI displays a
/// physical coordinate is ambiguous — a point on a 1x display can also be a
/// valid halved point on a 2x one. We reinterpret it in each monitor's scale,
/// keep the monitors whose logical bounds contain it, and pick the one where
/// the icon sits closest to the right edge (where menu-bar icons live). This
/// resolves the ambiguity for real icon positions without native screen calls.
///
/// `monitor_from_point` can't be used here: it hit-tests in logical points
/// while our coordinate is physical, so on a scaled display it picks wrong.
fn icon_display_scale(window: &WebviewWindow, phys_x: f64, phys_y: f64) -> Option<f64> {
    let mut best: Option<(f64, f64)> = None; // (scale, distance from right edge)
    for m in window.available_monitors().ok()? {
        let s = m.scale_factor();
        if s <= 0.0 {
            continue;
        }
        let (left, top) = (m.position().x as f64 / s, m.position().y as f64 / s);
        let (w, h) = (m.size().width as f64 / s, m.size().height as f64 / s);
        let (cx, cy) = (phys_x / s, phys_y / s);
        if cx >= left && cx <= left + w && cy >= top && cy <= top + h {
            let dist_right = (left + w) - cx;
            if best.map_or(true, |(_, d)| dist_right < d) {
                best = Some((s, dist_right));
            }
        }
    }
    best.map(|(s, _)| s)
}

/// Pull fresh data the moment the dropdown opens, so the numbers are current
/// without waiting for the next interval tick.
pub fn refresh_on_open(app: &AppHandle) {
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        match crate::commands::usage::collect(&handle).await {
            Ok(snapshot) => {
                let _ = handle.emit("usage-updated", &snapshot);
            }
            Err(e) => tracing::warn!("refresh-on-open failed: {e}"),
        }
    });
}
