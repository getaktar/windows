//! The notification area icon. A left click opens the upload panel; a right
//! click shows the usual Windows tray menu.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Wry};

use crate::windows::AppWindow;
use crate::{panel, system, t};

const TRAY_ID: &str = "main";

/// Monochrome glyphs like the system's own tray icons: a dark one for the
/// light taskbar and a white one for the dark taskbar.
const ICON_FOR_LIGHT_TASKBAR: &[u8] = include_bytes!("../icons/tray-light-taskbar.png");
const ICON_FOR_DARK_TASKBAR: &[u8] = include_bytes!("../icons/tray-dark-taskbar.png");

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(current_icon()?)
        .tooltip("Aktar")
        .menu(&menu(app)?)
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray, event| match event {
            TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, rect, .. } => {
                panel::remember_tray_rect(tray.app_handle(), rect);
                // A double-click arrives as two clicks; the second one must
                // not close the panel the first one just opened.
                if is_second_click() {
                    panel::show(tray.app_handle());
                } else {
                    panel::toggle(tray.app_handle());
                }
            }
            TrayIconEvent::DoubleClick { button: MouseButton::Left, .. } => panel::show(tray.app_handle()),
            _ => {}
        })
        .on_menu_event(|app, event| handle_menu(app, event.id().as_ref()))
        .build(app)?;
    Ok(())
}

/// Removed before an update's installer takes over, which ends the process
/// without the usual cleanup and would leave a dead icon behind until the
/// pointer passes over it.
pub fn remove(app: &AppHandle) {
    let _ = app.remove_tray_by_id(TRAY_ID);
}

fn is_second_click() -> bool {
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap();
    let limit = Duration::from_millis(system::double_click_millis());
    let second = last.is_some_and(|previous| previous.elapsed() < limit);
    // A third click starts a new pair.
    *last = if second { None } else { Some(Instant::now()) };
    second
}

fn menu(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    let menu = Menu::with_items(
        app,
        &[
            &MenuItem::with_id(app, "open-panel", t!("Upload"), true, None::<&str>)?,
            &MenuItem::with_id(app, "upload-clipboard", t!("Upload Clipboard"), true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "library", t!("Library"), true, None::<&str>)?,
            &MenuItem::with_id(app, "settings", t!("Settings"), true, None::<&str>)?,
        ],
    )?;
    // The Store updates its own packages.
    if !crate::package::is_packaged() {
        menu.append(&MenuItem::with_id(app, "check-updates", t!("Check for Updates…"), true, None::<&str>)?)?;
    }
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(app, "quit", t!("Quit Aktar"), true, None::<&str>)?)?;
    Ok(menu)
}

fn handle_menu(app: &AppHandle, id: &str) {
    match id {
        "open-panel" => panel::show(app),
        "upload-clipboard" => crate::uploads::upload_clipboard_in_background(&crate::core::core(app)),
        "library" => crate::windows::open(app, AppWindow::Library),
        "settings" => crate::windows::open(app, AppWindow::Settings),
        "check-updates" => crate::updater::check_now(app),
        "quit" => crate::quit(app),
        _ => {}
    }
}

/// Rebuilt after a language change.
pub fn refresh_menu(app: &AppHandle) {
    if let (Some(tray), Ok(menu)) = (app.tray_by_id(TRAY_ID), menu(app)) {
        let _ = tray.set_menu(Some(menu));
    }
}

/// Called when Windows switches between light and dark mode.
pub fn refresh_icon(app: &AppHandle) {
    if let (Some(tray), Ok(icon)) = (app.tray_by_id(TRAY_ID), current_icon()) {
        let _ = tray.set_icon(Some(icon));
    }
}

/// Keeps the icon's color in step with the taskbar. The taskbar follows the
/// "Windows mode" setting, which can differ from (and change without) the
/// "app mode" that the webviews and their theme events reflect.
pub fn follow_taskbar_theme(app: &AppHandle) {
    let app = app.clone();
    system::watch_theme(move || refresh_icon(&app));
}

fn current_icon() -> tauri::Result<Image<'static>> {
    let bytes = if system::taskbar_uses_light_theme() { ICON_FOR_LIGHT_TASKBAR } else { ICON_FOR_DARK_TASKBAR };
    Image::from_bytes(bytes)
}
