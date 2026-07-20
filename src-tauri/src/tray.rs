//! Menubar tray icon with a click-to-toggle dropdown window and a small hover
//! popover that previews the top usage meters, both positioned under the icon.

use std::sync::Mutex;

use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, Rect, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
};
use tauri_plugin_positioner::{Position, WindowExt};

use crate::scanner::UsageSnapshot;
use crate::settings;
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
            "quit" => {
                // Persist the float position so it survives a quit/restart.
                if let Some(win) = app.get_webview_window("main") {
                    save_float_position(app, &win);
                }
                app.exit(0);
            }
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
        // In float mode, remember where the user placed the window so it
        // reappears at the same spot on the next open (and after restarts).
        save_float_position(app, &window);
        let _ = window.hide();
        return;
    }

    // In float mode the window stays where the user dragged it — no
    // repositioning to the tray icon / taskbar corner.
    let floating = app
        .state::<std::sync::Mutex<AppState>>()
        .lock()
        .map(|g| g.settings.window_mode == "float")
        .unwrap_or(false);

    if floating {
        restore_float_position(app, &window);
    } else {
        position_dropdown(&window, tray_rect);
    }
    let _ = window.show();
    if !floating {
        // macOS can re-place a window onto a different monitor when it becomes
        // visible, undoing the position we set while it was hidden. Re-assert it
        // now that it's on-screen so the dropdown reliably lands on the display
        // whose menu bar was clicked.
        position_dropdown(&window, tray_rect);
    }
    let _ = window.set_focus();
    refresh_on_open(app);
}

/// Persist the window's current position when in float mode so it survives
/// hide/show cycles and app restarts.
fn save_float_position(app: &AppHandle, window: &WebviewWindow) {
    let is_float = app
        .state::<std::sync::Mutex<AppState>>()
        .lock()
        .map(|g| g.settings.window_mode == "float")
        .unwrap_or(false);
    if !is_float {
        return;
    }
    let Ok(pos) = window.outer_position() else { return };
    if let Ok(mut guard) = app.state::<std::sync::Mutex<AppState>>().lock() {
        guard.settings.float_x = Some(pos.x);
        guard.settings.float_y = Some(pos.y);
        let _ = settings::save(app, &guard.settings);
    }
}

/// Restore the saved float position, but only if it lands on a currently
/// connected monitor — a stale position from a disconnected display would
/// leave the window off-screen and unreachable.
fn restore_float_position(app: &AppHandle, window: &WebviewWindow) {
    let saved = app
        .state::<std::sync::Mutex<AppState>>()
        .lock()
        .ok()
        .and_then(|g| match (g.settings.float_x, g.settings.float_y) {
            (Some(x), Some(y)) => Some((x, y)),
            _ => None,
        });
    let Some((x, y)) = saved else { return };

    // Verify the saved position is within a connected monitor's bounds.
    let on_screen = window
        .available_monitors()
        .map(|monitors| {
            monitors.iter().any(|m| {
                let p = m.position();
                let s = m.size();
                x >= p.x
                    && y >= p.y
                    && x < p.x + s.width as i32
                    && y < p.y + s.height as i32
            })
        })
        .unwrap_or(false);

    if on_screen {
        let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
    }
    // If the monitor is gone, fall through — the OS will place the window
    // on a visible display by default.
}

/// Re-apply the current mode's placement to an already-visible window. Called
/// when the user switches modes in Settings while the dropdown is open, so the
/// window snaps to its new anchor immediately (dock → taskbar corner, float →
/// last dragged spot) instead of waiting for the next tray click to reposition
/// it. Mirrors the show path in `toggle_window`, minus the show/focus.
pub(crate) fn apply_mode_placement(app: &AppHandle, window: &WebviewWindow) {
    let floating = app
        .state::<std::sync::Mutex<AppState>>()
        .lock()
        .map(|g| g.settings.window_mode == "float")
        .unwrap_or(false);
    if floating {
        restore_float_position(app, window);
    } else {
        place_at_work_area_corner(window);
    }
}

/// Place the dropdown/popover relative to the tray icon. The anchor differs by
/// platform because the tray lives in opposite corners (macOS menu bar at the
/// top, Windows taskbar at the bottom), so the body is split into two
/// `#[cfg]`-gated `place_window` helpers below.
///
/// `tray_rect` is the icon's bounding rect in physical pixels (top-left global
/// origin). When it's `None` (the "Show / Hide" menu item, which carries no
/// geometry) we defer to the positioner plugin's cached tray location.
fn position_dropdown(window: &WebviewWindow, tray_rect: Option<Rect>) {
    let Some(rect) = tray_rect else {
        let _ = window.move_window_constrained(Position::TrayCenter);
        return;
    };

    // Icon geometry in physical pixels (top-left global origin).
    let pos = rect.position.to_physical::<f64>(1.0);
    let size = rect.size.to_physical::<f64>(1.0);
    place_window(window, pos.x, pos.y, size.width, size.height);
}

/// Reposition the window to its docked anchor without a tray-rect — used when
/// switching to dock mode mid-session (no tray click carries geometry). Falls
/// back to the positioner plugin's cached tray location when no monitor can be
/// resolved, matching `position_dropdown`'s `None`-rect branch.
fn place_at_work_area_corner(window: &WebviewWindow) {
    #[cfg(target_os = "windows")]
    {
        let monitor = window
            .current_monitor()
            .ok()
            .flatten()
            .or_else(|| window.primary_monitor().ok().flatten());
        if let Some(monitor) = monitor {
            pin_bottom_right(window, &monitor);
            return;
        }
    }
    // macOS or no monitor resolved: defer to the positioner's cached tray spot.
    let _ = window.move_window_constrained(Position::TrayCenter);
}

/// macOS: hang the window directly off the menu-bar icon — top edge just below
/// the menu bar, right edge aligned with the icon's right edge so it extends
/// down-left. Anchoring the right edge (rather than centering) keeps the window
/// on-screen, since menu-bar icons sit near the top-right corner.
///
/// All math is done in **logical points** — the only coordinate space that's
/// uniform across displays of different scale (e.g. a 2x Retina laptop next to
/// a 1x external monitor). The result is sent as a `LogicalPosition`, which the
/// OS places directly without reinterpreting it against the window's current
/// display scale; that's what makes this correct on a mixed-DPI setup.
#[cfg(not(target_os = "windows"))]
fn place_window(window: &WebviewWindow, icon_x: f64, icon_y: f64, icon_w: f64, icon_h: f64) {
    // Converting the physical icon position to logical points requires the
    // scale of the display the icon is on — which we recover below.
    let Some(scale) = icon_display_scale(window, icon_x, icon_y) else {
        let _ = window.move_window_constrained(Position::TrayCenter);
        return;
    };

    let icon_right = (icon_x + icon_w) / scale;
    // The icon spans the menu bar height, so its bottom edge is exactly where a
    // menu-bar dropdown should hang from — on this display, not the primary's.
    let menubar_bottom = (icon_y + icon_h) / scale;

    // Window width in logical points (constant regardless of current display).
    let win_scale = window.scale_factor().unwrap_or(1.0).max(0.01);
    let win_w = window.outer_size().map(|s| s.width as f64).unwrap_or(0.0) / win_scale;

    let _ = window.set_position(tauri::LogicalPosition::new(icon_right - win_w, menubar_bottom));
}

/// Windows: pin the window to the **bottom-right corner of the monitor's work
/// area** (flush above the taskbar, against the right edge) — the conventional
/// placement for a Windows tray app. We deliberately do *not* anchor to the icon
/// rect: a Windows tray icon usually lives in the hidden-icons overflow flyout,
/// so its rect points into the flyout (or is stale), not the taskbar. The work
/// area is the screen minus the taskbar, so its corner is a stable, correct
/// anchor wherever the icon actually sits. `work_area` and `outer_size` are both
/// physical pixels, so the math needs no scale conversion.
#[cfg(target_os = "windows")]
fn place_window(window: &WebviewWindow, icon_x: f64, icon_y: f64, _icon_w: f64, _icon_h: f64) {
    // Pick the monitor the icon sits on, then fall back to the window's current
    // monitor and finally the primary. The icon rect is unreliable when the icon
    // lives in the hidden-icons overflow flyout, so this fallback chain keeps the
    // window pinned to a real work-area corner instead of dropping to the
    // positioner plugin's `TrayCenter` (which lands mid-screen and then visibly
    // jumps once the frontend's `fit_tray_window` re-pins it — the reported
    // "shifts from left to right" on open).
    let monitor = icon_monitor(window, icon_x, icon_y)
        .or_else(|| window.current_monitor().ok().flatten())
        .or_else(|| window.primary_monitor().ok().flatten());
    let Some(monitor) = monitor else {
        let _ = window.move_window_constrained(Position::TrayCenter);
        return;
    };
    pin_bottom_right(window, &monitor);
}

/// Pin a window flush to the bottom-right corner of a monitor's work area (above
/// the Windows taskbar, against the right edge). Shared by the tray open
/// placement and `commands::usage::fit_tray_window`'s post-resize re-pin so BOTH
/// derive the exact same pixel from the window's real `outer_size`. Previously
/// the two paths used different inputs (`outer_size` here vs `width * scale`
/// there) and different monitor sources, so the window was placed at one spot
/// before `show()` and then jumped to a slightly different spot when the frontend
/// re-fit it — visible as the window sliding into place on open.
#[cfg(target_os = "windows")]
pub(crate) fn pin_bottom_right(window: &WebviewWindow, monitor: &tauri::Monitor) {
    let wa = monitor.work_area();
    let outer = window.outer_size().unwrap_or_default();
    let x = wa.position.x + wa.size.width as i32 - outer.width as i32;
    let y = wa.position.y + wa.size.height as i32 - outer.height as i32;
    let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
}

/// The monitor whose physical bounds contain the tray icon. On Windows monitor
/// bounds and the icon position are all in the same global physical coordinate
/// space, so a direct containment test is unambiguous (no per-scale
/// reinterpretation like the macOS path needs).
#[cfg(target_os = "windows")]
fn icon_monitor(window: &WebviewWindow, phys_x: f64, phys_y: f64) -> Option<tauri::Monitor> {
    let (x, y) = (phys_x as i32, phys_y as i32);
    window.available_monitors().ok()?.into_iter().find(|m| {
        let p = m.position();
        let s = m.size();
        x >= p.x && x < p.x + s.width as i32 && y >= p.y && y < p.y + s.height as i32
    })
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
#[cfg(not(target_os = "windows"))]
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
