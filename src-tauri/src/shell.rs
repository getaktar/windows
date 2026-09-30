//! "Upload with Aktar" from File Explorer. The right-click menu entry (added
//! by `register_context_menu`) and the "Send to" shortcut (added by the
//! installer, see `windows/installer-hooks.nsh`) start
//! `aktar.exe --upload <paths>`. A running Aktar gets those arguments from
//! the single-instance plugin; otherwise the launch they started uploads
//! them.

use std::path::{Path, PathBuf};

use crate::core::SharedCore;
use crate::uploads::{self, UploadInput};

pub const UPLOAD_FLAG: &str = "--upload";

/// True when these launch arguments ask for an upload.
pub fn is_upload(argv: &[String]) -> bool {
    argv.iter().any(|arg| arg == UPLOAD_FLAG)
}

/// Uploads the files and folders named after `--upload`, resolving
/// relative paths against `cwd`. Folders go up as the destination's
/// Folders setting says (a ZIP, or file by file).
pub fn handle(core: &SharedCore, argv: &[String], cwd: &Path) {
    let paths = paths_after_flag(argv, cwd);
    if paths.is_empty() {
        return;
    }
    uploads::enqueue(core, paths.into_iter().map(UploadInput::from_path).collect(), None);
}

/// Adds "Upload with Aktar" to File Explorer's right-click menu for files
/// and folders,
/// in the current language and pointing at this copy of aktar.exe, so it's
/// run at launch and when the language changes. The uninstaller removes it.
/// A Microsoft Store (MSIX) install can't add it: a package's registry
/// writes stay private to the package.
#[cfg(windows)]
pub fn register_context_menu() {
    use windows_sys::Win32::System::Registry::{RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ};

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }
    fn set(subkey: &str, name: Option<&str>, value: &str) {
        let (subkey, value) = (wide(subkey), wide(value));
        let name = name.map(wide);
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                subkey.as_ptr(),
                name.as_ref().map_or(std::ptr::null(), |name| name.as_ptr()),
                REG_SZ,
                value.as_ptr().cast(),
                (value.len() * 2) as u32,
            );
        }
    }

    if crate::package::is_packaged() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let exe = exe.to_string_lossy();
    for key in [r"Software\Classes\*\shell\Aktar.Upload", r"Software\Classes\Directory\shell\Aktar.Upload"] {
        set(key, Some("MUIVerb"), &t!("Upload with Aktar"));
        set(key, Some("Icon"), &format!("\"{exe}\",0"));
        // One aktar.exe per selected item, however many are selected;
        // each hands its path to the running copy.
        set(key, Some("MultiSelectModel"), "Player");
        set(&format!(r"{key}\command"), None, &format!("\"{exe}\" {UPLOAD_FLAG} \"%1\""));
    }
}

#[cfg(not(windows))]
pub fn register_context_menu() {}

fn paths_after_flag(argv: &[String], cwd: &Path) -> Vec<PathBuf> {
    let Some(flag) = argv.iter().position(|arg| arg == UPLOAD_FLAG) else {
        return Vec::new();
    };
    argv[flag + 1..].iter().map(|arg| cwd.join(arg)).filter(|path| path.is_file() || path.is_dir()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_existing_files_and_folders_after_the_flag() {
        let dir = std::env::temp_dir().join(format!("aktar-shell-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("folder")).unwrap();
        std::fs::write(dir.join("a.png"), b"a").unwrap();
        let argv: Vec<String> = ["aktar.exe", "a.png", UPLOAD_FLAG, "a.png", "folder", "missing.txt"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(paths_after_flag(&argv, &dir), vec![dir.join("a.png"), dir.join("folder")]);
        assert!(paths_after_flag(&argv[..2], &dir).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
