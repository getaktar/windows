//! The Windows calls behind capture, upload and paste, Undo and the offline
//! queue (the Mac app's docs/capture-and-instant-links.md): the clipboard as
//! it is (to put it back), key presses, the foreground window, free disk
//! space, the network, and notifications that can be replaced or taken
//! back. Only std, `log` and the Windows crates are used here, so the
//! Windows half can be type-checked on its own. Elsewhere (the tests on a
//! Mac) the functions do nothing.

#![cfg_attr(not(windows), allow(dead_code, unused_variables))]

use std::path::Path;

/// Bigger clipboards aren't kept for Undo.
pub const MAX_SNAPSHOT_BYTES: usize = 64 * 1024 * 1024;

/// What was on the clipboard, format by format, in the order it was put
/// there.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClipboardSnapshot {
    formats: Vec<(u32, Vec<u8>)>,
}

/// The clipboard, as far as it can be kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Snapshot {
    Kept(ClipboardSnapshot),
    /// Marked by a password manager as not to be kept or shown: never put
    /// back after its app stopped watching it.
    Private,
    TooBig,
    /// Another app held the clipboard open.
    Unavailable,
}

/// A notification's button: its label and what it hands `on_button`.
pub struct ToastButton<'a> {
    pub label: &'a str,
    pub argument: &'a str,
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{CloseHandle, GlobalFree, HWND};
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData, GetClipboardFormatNameW, GetClipboardOwner,
        GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE};
    use windows_sys::Win32::System::Threading::{GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, GetKeyboardLayout, MapVirtualKeyW, SendInput, VkKeyScanExW, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
        KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, MAPVK_VK_TO_VSC, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

    // Standard clipboard formats (winuser.h).
    const CF_TEXT: u32 = 1;
    const CF_BITMAP: u32 = 2;
    const CF_METAFILEPICT: u32 = 3;
    const CF_OEMTEXT: u32 = 7;
    const CF_DIB: u32 = 8;
    const CF_PALETTE: u32 = 9;
    const CF_UNICODETEXT: u32 = 13;
    const CF_ENHMETAFILE: u32 = 14;
    const CF_DIBV5: u32 = 17;
    const CF_OWNERDISPLAY: u32 = 0x80;
    const CF_DSPBITMAP: u32 = 0x82;
    const CF_DSPMETAFILEPICT: u32 = 0x83;
    const CF_DSPENHMETAFILE: u32 = 0x8E;

    /// Formats password managers put on what they copy (KeePass, 1Password,
    /// Bitwarden and others; learn.microsoft.com "Clipboard Formats").
    const PRIVATE_MARKERS: [&str; 2] = ["ExcludeClipboardContentFromMonitorProcessing", "Clipboard Viewer Ignore"];
    /// Set to 0 on what mustn't go to clipboard history or other devices.
    const HISTORY_MARKERS: [&str; 2] = ["CanIncludeInClipboardHistory", "CanUploadToCloudClipboard"];

    /// The clipboard, open until dropped. Another app may hold it for a
    /// moment, so opening it is tried a few times.
    struct OpenClipboardGuard;

    impl OpenClipboardGuard {
        fn open() -> Option<Self> {
            for attempt in 0..10u64 {
                if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
                    return Some(Self);
                }
                std::thread::sleep(Duration::from_millis(20 + attempt * 10));
            }
            None
        }
    }

    impl Drop for OpenClipboardGuard {
        fn drop(&mut self) {
            unsafe { CloseClipboard() };
        }
    }

    /// Goes up with every change to the clipboard, by any app.
    pub fn clipboard_sequence() -> u32 {
        unsafe { GetClipboardSequenceNumber() }
    }

    fn format_name(format: u32) -> Option<String> {
        if format < 0xC000 {
            return None;
        }
        let mut buffer = [0u16; 256];
        let length = unsafe { GetClipboardFormatNameW(format, buffer.as_mut_ptr(), buffer.len() as i32) };
        (length > 0).then(|| String::from_utf16_lossy(&buffer[..length as usize]))
    }

    /// Formats whose data is a GDI handle rather than memory, or private
    /// to the app that put them there: they can't be copied.
    fn is_handle_format(format: u32) -> bool {
        matches!(format, CF_BITMAP | CF_METAFILEPICT | CF_PALETTE | CF_ENHMETAFILE | CF_OWNERDISPLAY | CF_DSPBITMAP | CF_DSPMETAFILEPICT | CF_DSPENHMETAFILE)
            || (0x200..=0x3FF).contains(&format)
    }

    /// A memory format's bytes, while the clipboard is open.
    fn read_format(format: u32) -> Option<Vec<u8>> {
        let handle = unsafe { GetClipboardData(format) };
        if handle.is_null() {
            return None;
        }
        let size = unsafe { GlobalSize(handle) };
        let pointer = unsafe { GlobalLock(handle) };
        if pointer.is_null() {
            return None;
        }
        let bytes = unsafe { std::slice::from_raw_parts(pointer as *const u8, size) }.to_vec();
        unsafe { GlobalUnlock(handle) };
        Some(bytes)
    }

    pub fn take_snapshot() -> Snapshot {
        let Some(_clipboard) = OpenClipboardGuard::open() else { return Snapshot::Unavailable };
        let mut available = Vec::new();
        let mut format = 0;
        loop {
            format = unsafe { EnumClipboardFormats(format) };
            if format == 0 {
                break;
            }
            available.push(format);
        }
        for &format in &available {
            let Some(name) = format_name(format) else { continue };
            if PRIVATE_MARKERS.iter().any(|marker| marker.eq_ignore_ascii_case(&name)) {
                return Snapshot::Private;
            }
            if HISTORY_MARKERS.iter().any(|marker| marker.eq_ignore_ascii_case(&name)) {
                let off = read_format(format).is_some_and(|bytes| bytes.len() >= 4 && bytes[..4] == [0, 0, 0, 0]);
                if off {
                    return Snapshot::Private;
                }
            }
        }
        // Windows makes these from another format that's kept.
        let has_unicode = available.contains(&CF_UNICODETEXT);
        let has_dibv5 = available.contains(&CF_DIBV5);
        let mut formats = Vec::new();
        let mut total = 0;
        for &format in &available {
            if is_handle_format(format) || (has_unicode && matches!(format, CF_TEXT | CF_OEMTEXT)) || (has_dibv5 && format == CF_DIB) {
                continue;
            }
            let Some(bytes) = read_format(format) else { continue };
            total += bytes.len();
            if total > MAX_SNAPSHOT_BYTES {
                return Snapshot::TooBig;
            }
            formats.push((format, bytes));
        }
        Snapshot::Kept(ClipboardSnapshot { formats })
    }

    /// Puts `snapshot` back on the clipboard. False when some of it
    /// couldn't be.
    pub fn restore(snapshot: &ClipboardSnapshot) -> bool {
        let Some(_clipboard) = OpenClipboardGuard::open() else { return false };
        if unsafe { EmptyClipboard() } == 0 {
            return false;
        }
        let mut all = true;
        for (format, bytes) in &snapshot.formats {
            let handle = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1)) };
            if handle.is_null() {
                all = false;
                continue;
            }
            let pointer = unsafe { GlobalLock(handle) };
            if pointer.is_null() {
                unsafe { GlobalFree(handle) };
                all = false;
                continue;
            }
            unsafe {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer as *mut u8, bytes.len());
                GlobalUnlock(handle);
            }
            // The clipboard owns the memory once this succeeds.
            if unsafe { SetClipboardData(*format, handle) }.is_null() {
                unsafe { GlobalFree(handle) };
                all = false;
            }
        }
        all
    }

    pub fn clear_clipboard() -> bool {
        let Some(_clipboard) = OpenClipboardGuard::open() else { return false };
        unsafe { EmptyClipboard() != 0 }
    }

    /// The program that put what's on the clipboard there, by file name
    /// ("SnippingTool.exe"), when Windows says.
    pub fn clipboard_owner_process() -> Option<String> {
        let owner = unsafe { GetClipboardOwner() };
        if owner.is_null() {
            return None;
        }
        process_name(owner)
    }

    fn process_name(window: HWND) -> Option<String> {
        let mut process_id = 0u32;
        unsafe { GetWindowThreadProcessId(window, &mut process_id) };
        if process_id == 0 {
            return None;
        }
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) };
        if process.is_null() {
            return None;
        }
        let mut buffer = [0u16; 1024];
        let mut size = buffer.len() as u32;
        let found = unsafe { QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut size) } != 0;
        unsafe { CloseHandle(process) };
        if !found {
            return None;
        }
        let path = String::from_utf16_lossy(&buffer[..size as usize]);
        Path::new(&path).file_name().map(|name| name.to_string_lossy().into_owned())
    }

    /// The link a browser put on the clipboard along with its text
    /// ("UniformResourceLocatorW"), for a link copied with its title.
    pub fn clipboard_url() -> Option<String> {
        let name: Vec<u16> = "UniformResourceLocatorW".encode_utf16().chain(Some(0)).collect();
        let format = unsafe { RegisterClipboardFormatW(name.as_ptr()) };
        if format == 0 || unsafe { IsClipboardFormatAvailable(format) } == 0 {
            return None;
        }
        let _clipboard = OpenClipboardGuard::open()?;
        let bytes = read_format(format)?;
        let units: Vec<u16> = bytes.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).take_while(|unit| *unit != 0).collect();
        Some(String::from_utf16_lossy(&units))
    }

    fn key_down(key: u16) -> bool {
        unsafe { GetAsyncKeyState(i32::from(key)) as u16 & 0x8000 != 0 }
    }

    /// Whether Shift, Ctrl, Alt or a Windows key is held.
    pub fn modifiers_down() -> bool {
        [VK_SHIFT, VK_CONTROL, VK_MENU, VK_LWIN, VK_RWIN].into_iter().any(key_down)
    }

    fn key(code: u16, up: bool) -> INPUT {
        let extended = matches!(code, VK_LWIN | VK_RWIN);
        let mut flags = if up { KEYEVENTF_KEYUP } else { 0 };
        if extended {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: code,
                    wScan: unsafe { MapVirtualKeyW(u32::from(code), MAPVK_VK_TO_VSC) } as u16,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    fn send(inputs: &[INPUT]) -> bool {
        let sent = unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32) };
        sent as usize == inputs.len()
    }

    /// Taps a key no app uses while Alt or a Windows key is down, so
    /// letting go of it after a shortcut doesn't open the app's menu bar
    /// or Start (the shortcut's own key never reached the app).
    pub fn mask_menu_key() {
        if key_down(VK_MENU) || key_down(VK_LWIN) || key_down(VK_RWIN) {
            const UNASSIGNED: u16 = 0xE8;
            send(&[key(UNASSIGNED, false), key(UNASSIGNED, true)]);
        }
    }

    /// Presses Ctrl+V in the foreground app, with the key that types "v"
    /// in its keyboard layout (letter keys carry the letter's key code, so
    /// that's V on Dvorak, AZERTY or Turkish F alike).
    pub fn send_paste() -> bool {
        let layout = unsafe { GetKeyboardLayout(GetWindowThreadProcessId(GetForegroundWindow(), std::ptr::null_mut())) };
        let scan = unsafe { VkKeyScanExW(u16::from(b'v'), layout) };
        let v = if scan != -1 && (scan as u16) >> 8 == 0 { (scan as u16) & 0xFF } else { u16::from(b'V') };
        send(&[key(VK_CONTROL, false), key(v, false), key(v, true), key(VK_CONTROL, true)])
    }

    /// Windows+Shift+S: the system's snipping bar (Snipping Tool), which
    /// puts the snip on the clipboard.
    pub fn send_screen_snip() -> bool {
        let s = u16::from(b'S');
        send(&[key(VK_LWIN, false), key(VK_SHIFT, false), key(s, false), key(s, true), key(VK_SHIFT, true), key(VK_LWIN, true)])
    }

    /// The window in front, unless it's one of Aktar's own.
    pub fn foreground_window() -> Option<isize> {
        let window = unsafe { GetForegroundWindow() };
        if window.is_null() {
            return None;
        }
        let mut process_id = 0u32;
        unsafe { GetWindowThreadProcessId(window, &mut process_id) };
        (process_id != unsafe { GetCurrentProcessId() }).then_some(window as isize)
    }

    pub fn is_foreground(window: isize) -> bool {
        foreground_handle() == window
    }

    /// The window in front, whoever's it is; 0 when there's none.
    pub fn foreground_handle() -> isize {
        (unsafe { GetForegroundWindow() }) as isize
    }

    pub fn beep() {
        use windows_sys::Win32::System::Diagnostics::Debug::MessageBeep;
        use windows_sys::Win32::UI::WindowsAndMessaging::MB_OK;
        unsafe { MessageBeep(MB_OK) };
    }

    /// The space this user may still use on the disk `path` is on.
    pub fn free_disk_space(path: &Path) -> Option<u64> {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut available = 0u64;
        let found = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut available, std::ptr::null_mut(), std::ptr::null_mut()) } != 0;
        found.then_some(available)
    }

    /// False only when Windows has no connection at all. A network it
    /// can't confirm internet on still counts: offices often block the
    /// check while the internet works, and an upload that fails for want
    /// of a connection waits anyway.
    pub fn is_online() -> bool {
        use windows::Networking::Connectivity::{NetworkConnectivityLevel, NetworkInformation};
        match NetworkInformation::GetInternetConnectionProfile() {
            Ok(profile) => profile.GetNetworkConnectivityLevel() != Ok(NetworkConnectivityLevel::None),
            Err(_) => false,
        }
    }

    /// Calls `changed` whenever the network changes, for as long as the app
    /// runs.
    pub fn watch_network(changed: impl Fn() + Send + Sync + 'static) {
        use windows::Networking::Connectivity::{NetworkInformation, NetworkStatusChangedEventHandler};
        let handler = NetworkStatusChangedEventHandler::new(move |_| {
            changed();
            Ok(())
        });
        if let Err(error) = NetworkInformation::NetworkStatusChanged(&handler) {
            log::warn!("Could not follow network changes: {error}");
        }
    }

    const TOAST_GROUP: &str = "aktar";

    /// A notification that replaces any earlier one with the same `tag`, with
    /// up to two lines under the title and an optional button.
    pub fn show_toast(
        app_id: &str,
        tag: &str,
        title: &str,
        lines: &[&str],
        button: Option<ToastButton<'_>>,
        on_button: Option<Box<dyn Fn(String) + Send + Sync + 'static>>,
    ) -> Result<(), String> {
        use windows::core::{Interface, HSTRING, IInspectable};
        use windows::Data::Xml::Dom::XmlDocument;
        use windows::Foundation::TypedEventHandler;
        use windows::UI::Notifications::{ToastActivatedEventArgs, ToastNotification, ToastNotificationManager};

        let mut texts = format!("<text>{}</text>", escape_xml(title));
        for line in lines.iter().filter(|line| !line.is_empty()).take(2) {
            texts.push_str(&format!("<text>{}</text>", escape_xml(line)));
        }
        let actions = match &button {
            Some(button) => format!(
                "<actions><action content=\"{}\" arguments=\"{}\"/></actions>",
                escape_xml(button.label),
                escape_xml(button.argument)
            ),
            None => String::new(),
        };
        let xml = format!("<toast><visual><binding template=\"ToastGeneric\">{texts}</binding></visual>{actions}</toast>");
        let show = || -> windows::core::Result<()> {
            let document = XmlDocument::new()?;
            document.LoadXml(&HSTRING::from(xml.as_str()))?;
            let toast = ToastNotification::CreateToastNotification(&document)?;
            toast.SetTag(&HSTRING::from(tag))?;
            toast.SetGroup(&HSTRING::from(TOAST_GROUP))?;
            if let Some(on_button) = on_button {
                toast.Activated(&TypedEventHandler::<ToastNotification, IInspectable>::new(move |_, args| {
                    let argument = args
                        .as_ref()
                        .and_then(|args| args.cast::<ToastActivatedEventArgs>().ok())
                        .and_then(|args| args.Arguments().ok())
                        .map(|argument| argument.to_string())
                        .unwrap_or_default();
                    on_button(argument);
                    Ok(())
                }))?;
            }
            ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(app_id))?.Show(&toast)
        };
        show().map_err(|error| error.to_string())
    }

    /// Takes a notification shown with `show_toast` away.
    pub fn remove_toast(app_id: &str, tag: &str) {
        use windows::core::HSTRING;
        use windows::UI::Notifications::ToastNotificationManager;
        let removed = ToastNotificationManager::History()
            .and_then(|history| history.RemoveGroupedTagWithId(&HSTRING::from(tag), &HSTRING::from(TOAST_GROUP), &HSTRING::from(app_id)));
        if let Err(error) = removed {
            log::debug!("Could not remove a notification: {error}");
        }
    }

    fn escape_xml(text: &str) -> String {
        text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn clipboard_sequence() -> u32 {
        0
    }

    pub fn take_snapshot() -> Snapshot {
        Snapshot::Unavailable
    }

    pub fn restore(_snapshot: &ClipboardSnapshot) -> bool {
        false
    }

    pub fn clear_clipboard() -> bool {
        false
    }

    pub fn clipboard_owner_process() -> Option<String> {
        None
    }

    pub fn clipboard_url() -> Option<String> {
        None
    }

    pub fn modifiers_down() -> bool {
        false
    }

    pub fn mask_menu_key() {}

    pub fn send_paste() -> bool {
        false
    }

    pub fn send_screen_snip() -> bool {
        false
    }

    pub fn foreground_window() -> Option<isize> {
        None
    }

    pub fn is_foreground(_window: isize) -> bool {
        false
    }

    pub fn foreground_handle() -> isize {
        0
    }

    pub fn beep() {}

    pub fn free_disk_space(_path: &Path) -> Option<u64> {
        None
    }

    pub fn is_online() -> bool {
        true
    }

    pub fn watch_network(_changed: impl Fn() + Send + Sync + 'static) {}

    pub fn show_toast(
        _app_id: &str,
        _tag: &str,
        _title: &str,
        _lines: &[&str],
        _button: Option<ToastButton<'_>>,
        _on_button: Option<Box<dyn Fn(String) + Send + Sync + 'static>>,
    ) -> Result<(), String> {
        Err("Notifications with buttons are only shown on Windows".into())
    }

    pub fn remove_toast(_app_id: &str, _tag: &str) {}
}

pub use imp::*;
