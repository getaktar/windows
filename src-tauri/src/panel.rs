//! The tray popup: a borderless, always-on-top window that opens next to the
//! notification area icon, like the Mac app's menu bar panel.
//!
//! It closes when the user clicks somewhere else, but not when that click
//! starts a drag: dragging a file in from File Explorer first activates
//! Explorer, and closing the panel then would pull the drop target away
//! before the file arrives.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use tauri::{
    AppHandle, LogicalSize, Manager, PhysicalPosition, PhysicalSize, Rect, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder, WindowEvent,
};

use crate::core::events;

pub const LABEL: &str = "panel";
const WIDTH: f64 = 340.0;
const MARGIN: f64 = 12.0;

#[derive(Default)]
pub struct PanelState {
    /// Where the tray icon was when it was last clicked, in physical pixels.
    tray_rect: Mutex<Option<(PhysicalPosition<f64>, PhysicalSize<f64>)>>,
    watching: AtomicBool,
    /// Set while the panel shows a dialog of its own (the Browse file
    /// picker), which takes focus without meaning "close the panel".
    showing_dialog: AtomicBool,
}

pub fn create(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    let window = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("index.html#/panel".into()))
        .title("Aktar")
        .inner_size(WIDTH, 420.0)
        .decorations(false)
        .resizable(false)
        .maximizable(false)
        .minimizable(false)
        .skip_taskbar(true)
        .always_on_top(true)
        // Windows 11 rounds the corners of an undecorated window that has
        // a shadow, so the panel looks like the system flyouts.
        .shadow(true)
        .visible(false)
        .build()?;

    let handle = app.clone();
    window.on_window_event(move |event| {
        match event {
            WindowEvent::Focused(false) => watch_outside_clicks(&handle),
            WindowEvent::ThemeChanged(_) => crate::tray::refresh_icon(&handle),
            // Alt+F4 hides the panel; it's created once and reused.
            WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                hide(&handle);
            }
            _ => {}
        }
    });
    Ok(window)
}

fn window(app: &AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window(LABEL)
}

pub fn remember_tray_rect(app: &AppHandle, rect: Rect) {
    let scale = window(app).and_then(|window| window.scale_factor().ok()).unwrap_or(1.0);
    let position = rect.position.to_physical::<f64>(scale);
    let size = rect.size.to_physical::<f64>(scale);
    *app.state::<PanelState>().tray_rect.lock().unwrap() = Some((position, size));
}

pub fn toggle(app: &AppHandle) {
    match window(app) {
        Some(window) if window.is_visible().unwrap_or(false) => hide(app),
        _ => show(app),
    }
}

pub fn show(app: &AppHandle) {
    let Some(window) = window(app) else { return };
    place(app, &window);
    let _ = window.show();
    let _ = window.unminimize();
    let _ = window.set_focus();
    crate::core::core(app).notify(events::PANEL_SHOWN);
}

pub fn hide(app: &AppHandle) {
    if let Some(window) = window(app) {
        let _ = window.hide();
    }
}

pub fn set_showing_dialog(app: &AppHandle, showing: bool) {
    app.state::<PanelState>().showing_dialog.store(showing, Ordering::SeqCst);
}

/// The panel sizes itself to its content; the frontend reports the height.
pub fn set_height(app: &AppHandle, height: f64) {
    let Some(window) = window(app) else { return };
    let _ = window.set_size(LogicalSize::new(WIDTH, height.clamp(120.0, 720.0)));
    if window.is_visible().unwrap_or(false) {
        place(app, &window);
    }
}

/// Above the tray icon when the taskbar is at the bottom (below it when
/// it's at the top), kept inside the monitor's work area.
fn place(app: &AppHandle, window: &WebviewWindow) {
    let Ok(size) = window.outer_size() else { return };
    let (width, height) = (f64::from(size.width), f64::from(size.height));
    let tray = *app.state::<PanelState>().tray_rect.lock().unwrap();
    let scale = window.scale_factor().unwrap_or(1.0);
    let margin = MARGIN * scale;

    let monitor = match tray {
        Some((position, size)) => app
            .monitor_from_point(position.x + size.width / 2.0, position.y + size.height / 2.0)
            .ok()
            .flatten(),
        None => None,
    }
    .or_else(|| app.primary_monitor().ok().flatten());
    let Some(monitor) = monitor else { return };
    let area = monitor.work_area();
    let (left, top) = (f64::from(area.position.x), f64::from(area.position.y));
    let (right, bottom) = (left + f64::from(area.size.width), top + f64::from(area.size.height));

    let (anchor_x, taskbar_at_top) = match tray {
        Some((position, size)) => {
            let center_y = position.y + size.height / 2.0;
            (position.x + size.width / 2.0, center_y < (top + bottom) / 2.0)
        }
        None => (right, false),
    };
    let x = (anchor_x - width / 2.0).clamp(left + margin, (right - width - margin).max(left + margin));
    let y = if taskbar_at_top { top + margin } else { (bottom - height - margin).max(top) };
    let _ = window.set_position(PhysicalPosition::new(x.round() as i32, y.round() as i32));
}

fn cursor_over(rect: Option<(PhysicalPosition<f64>, PhysicalSize<f64>)>, cursor: PhysicalPosition<f64>) -> bool {
    rect.is_some_and(|(position, size)| {
        cursor.x >= position.x
            && cursor.x <= position.x + size.width
            && cursor.y >= position.y
            && cursor.y <= position.y + size.height
    })
}

fn panel_rect(window: &WebviewWindow) -> Option<(PhysicalPosition<f64>, PhysicalSize<f64>)> {
    let position = window.outer_position().ok()?;
    let size = window.outer_size().ok()?;
    Some((
        PhysicalPosition::new(f64::from(position.x), f64::from(position.y)),
        PhysicalSize::new(f64::from(size.width), f64::from(size.height)),
    ))
}

/// Runs after the panel loses focus. A plain click elsewhere closes it; a
/// click that turns into a drag keeps it open (and keeps watching, so the
/// next plain click outside still closes it); a click on the tray icon is
/// left to the tray handler, which toggles the panel itself.
fn watch_outside_clicks(app: &AppHandle) {
    let state = app.state::<PanelState>();
    if state.watching.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut first = true;
        loop {
            let Some(window) = window(&app) else { break };
            if !window.is_visible().unwrap_or(false) || window.is_focused().unwrap_or(false) {
                break;
            }
            if app.state::<PanelState>().showing_dialog.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
            // Focus left because of a click (or Alt+Tab); anything later
            // needs a new mouse press to count.
            if first || mouse_button_down() {
                first = false;
                tokio::time::sleep(Duration::from_millis(250)).await;
                let cursor = app.cursor_position().unwrap_or_default();
                let tray = *app.state::<PanelState>().tray_rect.lock().unwrap();
                let over_panel = cursor_over(panel_rect(&window), cursor);
                if cursor_over(tray, cursor) {
                    wait_for_release().await;
                } else if mouse_button_down() {
                    // A drag, maybe headed for the drop zone.
                    wait_for_release().await;
                } else if !over_panel {
                    hide(&app);
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(60)).await;
        }
        app.state::<PanelState>().watching.store(false, Ordering::SeqCst);
    });
}

async fn wait_for_release() {
    while mouse_button_down() {
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

#[cfg(windows)]
fn mouse_button_down() -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_RBUTTON};
    // The high bit is set while the button is held.
    let pressed = |key: u16| unsafe { GetAsyncKeyState(i32::from(key)) } as u16 & 0x8000 != 0;
    pressed(VK_LBUTTON) || pressed(VK_RBUTTON)
}

#[cfg(not(windows))]
fn mouse_button_down() -> bool {
    false
}
