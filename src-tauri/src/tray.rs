//! The notification area icon. A left click opens the upload panel; a right
//! click shows the usual Windows tray menu.

use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Wry};

use crate::windows::AppWindow;
use crate::{panel, t};

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
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                rect,
                ..
            } = event
            {
                panel::remember_tray_rect(tray.app_handle(), rect);
                panel::toggle(tray.app_handle());
            }
        })
        .on_menu_event(|app, event| handle_menu(app, event.id().as_ref()))
        .build(app)?;
    Ok(())
}

fn menu(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    Menu::with_items(
        app,
        &[
            &MenuItem::with_id(app, "open-panel", t!("Upload"), true, None::<&str>)?,
            &MenuItem::with_id(app, "upload-clipboard", t!("Upload Clipboard"), true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "library", t!("Library"), true, None::<&str>)?,
            &MenuItem::with_id(app, "settings", t!("Settings"), true, None::<&str>)?,
            &MenuItem::with_id(app, "check-updates", t!("Check for Updates…"), true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "quit", t!("Quit Aktar"), true, None::<&str>)?,
        ],
    )
}

fn handle_menu(app: &AppHandle, id: &str) {
    match id {
        "open-panel" => panel::show(app),
        "upload-clipboard" => {
            crate::uploads::upload_clipboard(&crate::core::core(app));
        }
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

fn current_icon() -> tauri::Result<Image<'static>> {
    let bytes = if taskbar_uses_light_theme() { ICON_FOR_LIGHT_TASKBAR } else { ICON_FOR_DARK_TASKBAR };
    Image::from_bytes(bytes)
}

/// The taskbar follows the "Windows mode" setting, which can differ from
/// the "app mode" that the webview's `prefers-color-scheme` reflects.
#[cfg(windows)]
fn taskbar_uses_light_theme() -> bool {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};

    let wide = |text: &str| text.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let subkey = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
    let value = wide("SystemUsesLightTheme");
    let mut data: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut data as *mut u32).cast(),
            &mut size,
        )
    };
    status == 0 && data == 1
}

#[cfg(not(windows))]
fn taskbar_uses_light_theme() -> bool {
    false
}
