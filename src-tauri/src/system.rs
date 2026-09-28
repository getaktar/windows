//! The few things Aktar asks Windows directly rather than through Tauri:
//! theme settings, the regional format, the double-click time, and an error
//! box for when the app can't start. Other platforms (the UI is sometimes
//! run elsewhere during development) get plain defaults.

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegGetValueW, RegNotifyChangeKeyValue, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_NOTIFY,
        REG_NOTIFY_CHANGE_LAST_SET, RRF_RT_REG_DWORD,
    };

    const PERSONALIZE: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize";

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn personalize_flag(name: &str) -> Option<bool> {
        let (subkey, value) = (wide(PERSONALIZE), wide(name));
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
        (status == 0).then_some(data == 1)
    }

    /// The "Windows mode" setting, which colors the taskbar.
    pub fn taskbar_uses_light_theme() -> bool {
        personalize_flag("SystemUsesLightTheme").unwrap_or(false)
    }

    /// The "app mode" setting, which the webviews' `prefers-color-scheme`
    /// follows.
    pub fn apps_use_light_theme() -> bool {
        personalize_flag("AppsUseLightTheme").unwrap_or(true)
    }

    /// Calls `changed` on a background thread whenever a theme setting
    /// changes. Windows notifies apps of the app mode (which the panel sees
    /// as a theme change) but not of the Windows mode on its own, so the
    /// tray icon would otherwise keep the wrong color.
    pub fn watch_theme(changed: impl Fn() + Send + 'static) {
        std::thread::spawn(move || {
            let subkey = wide(PERSONALIZE);
            let mut key: HKEY = std::ptr::null_mut();
            if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_NOTIFY, &mut key) } != 0 {
                return;
            }
            // Blocks until a value under the key is written.
            while unsafe { RegNotifyChangeKeyValue(key, 0, REG_NOTIFY_CHANGE_LAST_SET, std::ptr::null_mut(), 0) } == 0 {
                changed();
            }
            unsafe { RegCloseKey(key) };
        });
    }

    /// The regional format (Settings > Time & language > Region), e.g.
    /// "en-GB", which dates and numbers should follow.
    pub fn region_locale() -> Option<String> {
        use windows_sys::Win32::Globalization::GetUserDefaultLocaleName;
        let mut buffer = [0u16; 85];
        let length = unsafe { GetUserDefaultLocaleName(buffer.as_mut_ptr(), buffer.len() as i32) };
        (length > 1).then(|| String::from_utf16_lossy(&buffer[..length as usize - 1]))
    }

    pub fn double_click_millis() -> u64 {
        u64::from(unsafe { windows_sys::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime() })
    }

    /// Keeps a borderless popup out of Alt+Tab, like the system flyouts.
    pub fn make_tool_window(hwnd: HWND) {
        #[cfg(target_pointer_width = "64")]
        unsafe {
            use windows_sys::Win32::UI::WindowsAndMessaging::{
                GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
            };
            let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            let style = (style | WS_EX_TOOLWINDOW as isize) & !(WS_EX_APPWINDOW as isize);
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style);
        }
        #[cfg(not(target_pointer_width = "64"))]
        let _ = hwnd;
    }

    /// For when Aktar can't start at all: there's no console, and without a
    /// window the user would otherwise see nothing happen.
    pub fn show_fatal_error(message: &str) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
        let (text, title) = (wide(message), wide("Aktar"));
        unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), MB_OK | MB_ICONERROR) };
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn taskbar_uses_light_theme() -> bool {
        false
    }

    pub fn apps_use_light_theme() -> bool {
        true
    }

    pub fn watch_theme(_changed: impl Fn() + Send + 'static) {}

    pub fn region_locale() -> Option<String> {
        None
    }

    pub fn double_click_millis() -> u64 {
        500
    }

    pub fn show_fatal_error(message: &str) {
        eprintln!("{message}");
    }
}

pub use imp::*;
