//! What watched folders ask Windows directly: file attributes and IDs,
//! whether a file is still open for writing, the Screenshots folder, the
//! power source and the network's cost. Other platforms (the engine's
//! tests run anywhere) get plain stand-ins.

use std::path::{Path, PathBuf};

/// One look at a file: everything the rules and the write-complete check
/// need, from a single handle on Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Probe {
    pub is_file: bool,
    pub is_symlink: bool,
    pub attributes: Attributes,
    pub size: u64,
    /// Unix milliseconds.
    pub mtime: i64,
    pub file_id: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Attributes {
    /// Hidden or system.
    pub hidden: bool,
    /// Online-only (OneDrive and other cloud files placeholders).
    pub cloud_only: bool,
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};

    const HIDDEN: u32 = 0x2;
    const SYSTEM: u32 = 0x4;
    const OFFLINE: u32 = 0x1000;
    const RECALL_ON_OPEN: u32 = 0x40000;
    const RECALL_ON_DATA_ACCESS: u32 = 0x400000;
    const CLOUD: u32 = OFFLINE | RECALL_ON_OPEN | RECALL_ON_DATA_ACCESS;

    /// Links are left out before this (`is_symlink`); OneDrive's files are
    /// reparse points too, and count as regular files.
    pub fn attributes(metadata: &std::fs::Metadata) -> Attributes {
        let value = metadata.file_attributes();
        Attributes { hidden: value & (HIDDEN | SYSTEM) != 0, cloud_only: value & CLOUD != 0 }
    }

    /// Opened once, for nothing but its attributes (which never recalls an
    /// online-only file), sharing everything so the app writing it isn't
    /// disturbed, and without following links. Type, size, times, and the
    /// file index all come from that one handle.
    pub fn probe(path: &Path) -> std::io::Result<Probe> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION};
        const FILE_READ_ATTRIBUTES: u32 = 0x80;
        const OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        const BACKUP_SEMANTICS: u32 = 0x0200_0000;
        let file = std::fs::OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .share_mode(0x7)
            .custom_flags(OPEN_REPARSE_POINT | BACKUP_SEMANTICS)
            .open(path)?;
        let metadata = file.metadata()?;
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        let file_id = (unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } != 0)
            .then(|| (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow));
        Ok(Probe {
            is_file: metadata.is_file(),
            is_symlink: metadata.file_type().is_symlink(),
            attributes: attributes(&metadata),
            size: metadata.len(),
            mtime: super::super::rules::mtime_millis(&metadata),
            file_id,
        })
    }

    /// Whether nothing else has the file open for writing: opening it while
    /// only sharing read access fails then. Antivirus scanners and the
    /// search indexer hold new files for a moment too.
    pub fn is_unlocked(path: &Path) -> bool {
        std::fs::OpenOptions::new().read(true).share_mode(0x1).open(path).is_ok()
    }

    pub fn home_dir() -> Option<PathBuf> {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }

    pub fn system_folders() -> Vec<PathBuf> {
        let mut folders = Vec::new();
        for variable in ["SystemRoot", "ProgramFiles", "ProgramFiles(x86)", "ProgramData", "ProgramW6432"] {
            if let Some(value) = std::env::var_os(variable) {
                folders.push(PathBuf::from(value));
            }
        }
        if folders.is_empty() {
            folders.extend([PathBuf::from(r"C:\Windows"), PathBuf::from(r"C:\Program Files")]);
        }
        folders
    }

    /// `\\server\share`, the root of a network share.
    pub fn is_share_root(path: &Path) -> bool {
        let text = path.to_string_lossy().replace('/', "\\");
        let text = text.trim_end_matches('\\');
        text.starts_with(r"\\") && text[2..].split('\\').filter(|part| !part.is_empty()).count() <= 2
    }

    /// A network share or a mapped network drive: watched by polling,
    /// since change notifications from file servers are unreliable.
    pub fn is_network_path(path: &Path) -> bool {
        use std::path::{Component, Prefix};
        use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;
        const DRIVE_REMOTE: u32 = 4;
        match path.components().next() {
            Some(Component::Prefix(prefix)) => match prefix.kind() {
                Prefix::UNC(..) | Prefix::VerbatimUNC(..) => true,
                Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
                    let root: Vec<u16> = format!("{}:\\", letter as char).encode_utf16().chain(std::iter::once(0)).collect();
                    unsafe { GetDriveTypeW(root.as_ptr()) == DRIVE_REMOTE }
                }
                _ => false,
            },
            _ => false,
        }
    }

    /// Where Windows saves screenshots (Win+PrtScn, the Snipping Tool),
    /// usually Pictures\Screenshots.
    pub fn screenshots_folder() -> Option<PathBuf> {
        use windows::Win32::System::Com::CoTaskMemFree;
        use windows::Win32::UI::Shell::{FOLDERID_Pictures, FOLDERID_Screenshots, SHGetKnownFolderPath, KF_FLAG_DEFAULT};
        let known = |id| unsafe {
            let path = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
            let text = path.to_string().ok();
            CoTaskMemFree(Some(path.0 as *const _));
            text.map(PathBuf::from)
        };
        known(&FOLDERID_Screenshots).or_else(|| known(&FOLDERID_Pictures).map(|pictures| pictures.join("Screenshots")))
    }

    pub fn on_battery() -> bool {
        use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
        let mut status: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
        // ACLineStatus: 0 offline (battery), 1 online, 255 unknown.
        unsafe { GetSystemPowerStatus(&mut status) != 0 && status.ACLineStatus == 0 }
    }

    /// Whether the internet connection is metered (a phone's hotspot, a
    /// connection set as metered in Settings), or over its data limit.
    pub fn metered() -> bool {
        use windows::Networking::Connectivity::{NetworkCostType, NetworkInformation};
        let Ok(profile) = NetworkInformation::GetInternetConnectionProfile() else { return false };
        let Ok(cost) = profile.GetConnectionCost() else { return false };
        let kind = cost.NetworkCostType().unwrap_or(NetworkCostType::Unrestricted);
        kind == NetworkCostType::Fixed
            || kind == NetworkCostType::Variable
            || cost.Roaming().unwrap_or(false)
            || cost.OverDataLimit().unwrap_or(false)
    }

    /// Calls `changed` whenever the network changes (connected, dropped,
    /// a different one), so failed uploads are retried as soon as it's back.
    pub fn watch_network(changed: impl Fn() + Send + Sync + 'static) {
        use windows::Networking::Connectivity::{NetworkInformation, NetworkStatusChangedEventHandler};
        let handler = NetworkStatusChangedEventHandler::new(move |_| {
            changed();
            Ok(())
        });
        // Registered for as long as the app runs.
        if let Err(error) = NetworkInformation::NetworkStatusChanged(&handler) {
            log::warn!("Could not follow network changes: {error}");
        }
    }

    /// Whether the energy saver (battery saver) is on.
    pub fn energy_saver() -> bool {
        use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
        let mut status: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
        unsafe { GetSystemPowerStatus(&mut status) != 0 && status.SystemStatusFlag == 1 }
    }

    /// Calls `power_changed` when the power source or the energy saver
    /// changes, and `resumed` after waking up from sleep. Returns false when
    /// Windows wouldn't register them.
    pub fn watch_power(power_changed: impl Fn() + Send + Sync + 'static, resumed: impl Fn() + Send + Sync + 'static) -> bool {
        use std::sync::OnceLock;
        use windows_sys::core::GUID;
        use windows_sys::Win32::System::Power::{PowerSettingRegisterNotification, RegisterSuspendResumeNotification, DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS};
        use windows_sys::Win32::UI::WindowsAndMessaging::{DEVICE_NOTIFY_CALLBACK, PBT_APMRESUMEAUTOMATIC, PBT_POWERSETTINGCHANGE};

        type Callbacks = (Box<dyn Fn() + Send + Sync>, Box<dyn Fn() + Send + Sync>);
        static CALLBACKS: OnceLock<Callbacks> = OnceLock::new();
        if CALLBACKS.set((Box::new(power_changed), Box::new(resumed))).is_err() {
            return false;
        }
        unsafe extern "system" fn callback(_context: *const core::ffi::c_void, kind: u32, _setting: *const core::ffi::c_void) -> u32 {
            if let Some((power_changed, resumed)) = CALLBACKS.get() {
                match kind {
                    PBT_POWERSETTINGCHANGE => power_changed(),
                    PBT_APMRESUMEAUTOMATIC => resumed(),
                    _ => {}
                }
            }
            0
        }
        // Kept for as long as the app runs.
        let parameters: &'static mut DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS =
            Box::leak(Box::new(DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS { Callback: Some(callback), Context: std::ptr::null_mut() }));
        const GUID_ACDC_POWER_SOURCE: GUID = GUID::from_u128(0x5d3e9a59_e9d5_4b00_a6bd_ff34ff516548);
        let parameters = parameters as *mut DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS as *mut core::ffi::c_void;
        let mut registration = std::ptr::null_mut();
        const GUID_POWER_SAVING_STATUS: GUID = GUID::from_u128(0xe00958c0_c213_4ace_ac77_fecced2eeea5);
        let power = unsafe { PowerSettingRegisterNotification(&GUID_ACDC_POWER_SOURCE, DEVICE_NOTIFY_CALLBACK, parameters, &mut registration) } == 0;
        let mut saver_registration = std::ptr::null_mut();
        let saver =
            unsafe { PowerSettingRegisterNotification(&GUID_POWER_SAVING_STATUS, DEVICE_NOTIFY_CALLBACK, parameters, &mut saver_registration) } == 0;
        let resume = unsafe { RegisterSuspendResumeNotification(parameters, DEVICE_NOTIFY_CALLBACK) } != 0;
        power && saver && resume
    }

    /// CREATE_NO_WINDOW: a script runs without flashing a console.
    pub fn hide_console(command: &mut tokio::process::Command) {
        command.creation_flags(0x0800_0000);
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn attributes(_metadata: &std::fs::Metadata) -> Attributes {
        Attributes::default()
    }

    pub fn probe(path: &Path) -> std::io::Result<Probe> {
        let metadata = std::fs::symlink_metadata(path)?;
        #[cfg(unix)]
        let file_id = {
            use std::os::unix::fs::MetadataExt;
            Some(metadata.ino())
        };
        #[cfg(not(unix))]
        let file_id = None;
        Ok(Probe {
            is_file: metadata.is_file(),
            is_symlink: metadata.file_type().is_symlink(),
            attributes: attributes(&metadata),
            size: metadata.len(),
            mtime: super::super::rules::mtime_millis(&metadata),
            file_id,
        })
    }

    pub fn is_unlocked(_path: &Path) -> bool {
        true
    }

    pub fn home_dir() -> Option<PathBuf> {
        std::env::var_os("HOME").map(PathBuf::from)
    }

    pub fn system_folders() -> Vec<PathBuf> {
        ["/System", "/Library", "/Applications", "/usr", "/bin", "/sbin", "/etc", "/var", "/private"]
            .into_iter()
            .map(PathBuf::from)
            .collect()
    }

    pub fn is_share_root(_path: &Path) -> bool {
        false
    }

    pub fn is_network_path(_path: &Path) -> bool {
        false
    }

    pub fn screenshots_folder() -> Option<PathBuf> {
        home_dir().map(|home| home.join("Pictures").join("Screenshots"))
    }

    pub fn on_battery() -> bool {
        false
    }

    pub fn energy_saver() -> bool {
        false
    }

    pub fn metered() -> bool {
        false
    }

    pub fn watch_network(_changed: impl Fn() + Send + Sync + 'static) {}

    pub fn watch_power(_power_changed: impl Fn() + Send + Sync + 'static, _resumed: impl Fn() + Send + Sync + 'static) -> bool {
        false
    }

    pub fn hide_console(_command: &mut tokio::process::Command) {}
}

pub use imp::*;

/// Moves a file to the Recycle Bin. Never deletes it for good: when that
/// doesn't work (a network share has no Recycle Bin), the file stays.
pub fn move_to_trash(path: &Path) -> Result<(), String> {
    trash::delete(path).map_err(|error| error.to_string())
}

/// Moves a file into `<folder>/Uploaded/`, as "name (2).ext" when the name
/// is taken there. Returns where it went.
pub fn move_to_uploaded(root: &Path, path: &Path) -> std::io::Result<PathBuf> {
    let target_folder = root.join(super::rules::UPLOADED_FOLDER);
    std::fs::create_dir_all(&target_folder)?;
    let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
    let (stem, extension) = crate::util::split_extension(&name);
    let mut target = target_folder.join(&name);
    let mut number = 2;
    while target.exists() {
        target = target_folder.join(match extension {
            "" => format!("{stem} ({number})"),
            extension => format!("{stem} ({number}).{extension}"),
        });
        number += 1;
    }
    std::fs::rename(path, &target)?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_into_uploaded_without_overwriting() {
        let dir = std::env::temp_dir().join(format!("aktar-moved-{}", crate::util::new_id()));
        std::fs::create_dir_all(dir.join("Uploaded")).unwrap();
        std::fs::write(dir.join("Uploaded/a.png"), b"old").unwrap();
        std::fs::write(dir.join("a.png"), b"new").unwrap();
        let target = move_to_uploaded(&dir, &dir.join("a.png")).unwrap();
        assert_eq!(target, dir.join("Uploaded/a (2).png"));
        assert_eq!(std::fs::read(dir.join("Uploaded/a.png")).unwrap(), b"old");
        std::fs::write(dir.join("README"), b"x").unwrap();
        assert_eq!(move_to_uploaded(&dir, &dir.join("README")).unwrap(), dir.join("Uploaded/README"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
