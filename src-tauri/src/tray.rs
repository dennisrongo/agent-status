//! Menubar tray icon with a click-to-toggle dropdown window and a small hover
//! popover that previews the top usage meters, both positioned under the icon.

use std::sync::Mutex;

use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, LogicalPosition, Manager, Rect, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder,
};
use tauri_plugin_positioner::{Position, WindowExt};

use crate::scanner::UsageSnapshot;
use crate::state::AppState;

/// Tray icon id, reused to look the icon back up later.
const TRAY_ID: &str = "main-tray";

/// Label of the hover popover window. It loads the same bundle as the "main"
/// dropdown; the frontend renders the compact popover when it sees this label.
const HOVER_LABEL: &str = "hover";

/// Logical width of the hover popover. Its height is fit to content by the
/// frontend; the width stays fixed so the right-edge anchor under the icon holds.
const HOVER_WIDTH: f64 = 300.0;

pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let show = MenuItem::with_id(app, "show", "Show / Hide", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;

    // Monochrome template icon → macOS tints it to match the menu bar
    // (light/dark) and keeps the transparent background, unlike the filled
    // app icon. Falls back to the window icon if decoding ever fails.
    let tray_icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))
        .unwrap_or_else(|_| app.default_window_icon().unwrap().clone());

    TrayIconBuilder::with_id(TRAY_ID)
        .icon(tray_icon)
        .icon_as_template(true)
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
            match event {
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    rect,
                    ..
                } => {
                    // The icon's bounding rect (physical pixels, top-left origin)
                    // is the anchor: it tells us which display the menu bar is on
                    // and exactly where the icon sits. Unlike the cursor `position`
                    // field, it's converted into the same coordinate space the
                    // window APIs use.
                    hide_hover_popover(tray.app_handle());
                    toggle_window(tray.app_handle(), Some(rect));
                }
                // Hovering previews the top usage meters in a small popover, so a
                // glance gives current numbers without opening the full dropdown.
                TrayIconEvent::Enter { rect, .. } => {
                    show_hover_popover(tray.app_handle(), rect)
                }
                TrayIconEvent::Leave { .. } => hide_hover_popover(tray.app_handle()),
                _ => {}
            }
        })
        .build(app)?;

    build_hover_window(app)?;

    Ok(())
}

/// Pre-create the hover popover (hidden) so it's already loaded and listening
/// for `usage-updated` events by the time the user first hovers the icon.
/// Borderless, non-focusing, always-on-top — a passive preview, never the key
/// window, so hovering never steals focus from whatever the user is typing in.
fn build_hover_window(app: &AppHandle) -> tauri::Result<()> {
    if app.get_webview_window(HOVER_LABEL).is_some() {
        return Ok(());
    }
    WebviewWindowBuilder::new(app, HOVER_LABEL, WebviewUrl::App("index.html".into()))
        .title("Agent Usage")
        .inner_size(HOVER_WIDTH, 150.0)
        .resizable(false)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .shadow(true)
        .focused(false)
        .visible(false)
        .build()?;
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

/// Show the hover popover under the tray icon and refresh its data.
///
/// The popover persists across hovers, so it already shows the last reading the
/// instant it appears; we also broadcast the cached snapshot (covers the very
/// first hover, before any collect) and kick off an on-demand refresh whose
/// fresh snapshot is pushed to every window. Refreshing only on hover keeps the
/// app idle while nothing is on screen.
fn show_hover_popover(app: &AppHandle, rect: Rect) {
    // Don't compete with the full dropdown when it's already open — the same
    // meters (and more) are already on screen.
    if app
        .get_webview_window("main")
        .and_then(|w| w.is_visible().ok())
        .unwrap_or(false)
    {
        return;
    }
    let Some(win) = app.get_webview_window(HOVER_LABEL) else {
        return;
    };

    // Tell the popover which provider to preview before it renders. Pushed on
    // every show, so a change made in Settings takes effect on the next hover
    // without any settings-sync plumbing in the popover.
    let _ = app.emit("hover-provider", tooltip_provider(app));

    if let Some(snapshot) = cached_snapshot(app) {
        let _ = app.emit("usage-updated", &snapshot);
    }

    position_dropdown(&win, Some(rect));
    let _ = win.show();
    // macOS can re-place a window onto another monitor when it becomes visible;
    // re-assert the position (mirrors `toggle_window` for the main dropdown).
    position_dropdown(&win, Some(rect));

    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        match crate::commands::usage::collect(&handle).await {
            Ok(snapshot) => {
                let _ = handle.emit("usage-updated", &snapshot);
            }
            Err(e) => tracing::warn!("hover refresh failed: {e}"),
        }
    });
}

fn hide_hover_popover(app: &AppHandle) {
    if let Some(win) = app.get_webview_window(HOVER_LABEL) {
        let _ = win.hide();
    }
}

/// Last-collected snapshot, if any. Locked only briefly to clone it out.
fn cached_snapshot(app: &AppHandle) -> Option<UsageSnapshot> {
    app.state::<Mutex<AppState>>()
        .lock()
        .ok()
        .and_then(|guard| guard.snapshot.clone())
}

/// Provider the hover popover should preview, from settings ("claude" by default).
fn tooltip_provider(app: &AppHandle) -> String {
    app.state::<Mutex<AppState>>()
        .lock()
        .map(|guard| guard.settings.tooltip_provider.clone())
        .unwrap_or_else(|_| "claude".to_string())
}
