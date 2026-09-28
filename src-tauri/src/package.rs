//! Running as the Microsoft Store (MSIX) package rather than the NSIS
//! install. The same executable goes into both; a few things work
//! differently inside a package:
//!
//! - Updates come from the Store, so Aktar's own updater stays off.
//! - "Launch at sign-in" is the package's startup task (declared in
//!   packaging/msix/AppxManifest.xml): a packaged app's registry writes land
//!   in a private copy that Windows never reads at sign-in.
//! - Notifications go out under the package's identity, the only one
//!   Windows shows them for.

use std::sync::OnceLock;

/// The `TaskId` of the startup task in the manifest.
#[cfg(windows)]
const STARTUP_TASK: &str = "AktarStartup";

pub fn is_packaged() -> bool {
    static PACKAGED: OnceLock<bool> = OnceLock::new();
    *PACKAGED.get_or_init(detect)
}

#[cfg(windows)]
fn detect() -> bool {
    use windows_sys::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
    use windows_sys::Win32::Storage::Packaging::Appx::GetCurrentPackageFullName;
    // Without a package this fails with APPMODEL_ERROR_NO_PACKAGE; with one,
    // it asks for a buffer to put the name in.
    let mut length = 0u32;
    unsafe { GetCurrentPackageFullName(&mut length, std::ptr::null_mut()) == ERROR_INSUFFICIENT_BUFFER }
}

#[cfg(not(windows))]
fn detect() -> bool {
    false
}

/// Whether the startup task started this launch, at sign-in. (The NSIS
/// install's login item passes a command-line flag instead.)
#[cfg(windows)]
pub fn launched_at_sign_in() -> bool {
    use windows::ApplicationModel::Activation::ActivationKind;
    use windows::ApplicationModel::AppInstance;
    is_packaged()
        && AppInstance::GetActivatedEventArgs()
            .and_then(|args| args.Kind())
            .is_ok_and(|kind| kind == ActivationKind::StartupTask)
}

#[cfg(not(windows))]
pub fn launched_at_sign_in() -> bool {
    false
}

#[cfg(windows)]
fn startup_task() -> windows::core::Result<windows::ApplicationModel::StartupTask> {
    windows::ApplicationModel::StartupTask::GetAsync(&windows::core::HSTRING::from(STARTUP_TASK))?.get()
}

/// Blocks while Windows answers; call it off the UI thread.
#[cfg(windows)]
pub fn startup_enabled() -> bool {
    use windows::ApplicationModel::StartupTaskState;
    startup_task()
        .and_then(|task| task.State())
        .is_ok_and(|state| state == StartupTaskState::Enabled || state == StartupTaskState::EnabledByPolicy)
}

/// Turns the startup task on or off and returns whether it's on now. Once
/// the user has turned Aktar off in Task Manager or Settings, only they can
/// turn it back on there; that's reported as an error saying so. Blocks
/// while Windows answers; call it off the UI thread.
#[cfg(windows)]
pub fn set_startup_enabled(enabled: bool) -> Result<bool, String> {
    use windows::ApplicationModel::StartupTaskState;
    let task = startup_task().map_err(|error| error.to_string())?;
    if !enabled {
        task.Disable().map_err(|error| error.to_string())?;
        return Ok(false);
    }
    let state = task
        .RequestEnableAsync()
        .and_then(|operation| operation.get())
        .map_err(|error| error.to_string())?;
    if state == StartupTaskState::DisabledByUser || state == StartupTaskState::DisabledByPolicy {
        return Err(crate::t!("Windows has turned off starting Aktar at sign-in. Turn it back on in Settings > Apps > Startup."));
    }
    Ok(state == StartupTaskState::Enabled || state == StartupTaskState::EnabledByPolicy)
}

#[cfg(not(windows))]
pub fn startup_enabled() -> bool {
    false
}

#[cfg(not(windows))]
pub fn set_startup_enabled(_enabled: bool) -> Result<bool, String> {
    Ok(false)
}

/// A toast under the package's identity.
#[cfg(windows)]
pub fn show_notification(title: &str, body: &str) -> Result<(), String> {
    use windows::core::HSTRING;
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};

    let xml = format!(
        "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>",
        escape_xml(title),
        escape_xml(body)
    );
    let show = || -> windows::core::Result<()> {
        let document = XmlDocument::new()?;
        document.LoadXml(&HSTRING::from(xml))?;
        let toast = ToastNotification::CreateToastNotification(&document)?;
        ToastNotificationManager::CreateToastNotifier()?.Show(&toast)
    };
    show().map_err(|error| error.to_string())
}

#[cfg(not(windows))]
pub fn show_notification(_title: &str, _body: &str) -> Result<(), String> {
    Ok(())
}

#[cfg_attr(not(windows), allow(dead_code))]
fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_toast_text() {
        assert_eq!(escape_xml("a<b> & \"c\" 'd'"), "a&lt;b&gt; &amp; &quot;c&quot; &apos;d&apos;");
    }

    #[test]
    fn a_test_run_is_not_packaged() {
        assert!(!is_packaged());
        assert!(!launched_at_sign_in());
    }
}
