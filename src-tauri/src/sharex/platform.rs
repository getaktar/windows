//! What the ShareX import asks Windows directly: registry values, the
//! Documents known folder, the computer's name, and DPAPI. std and the
//! windows crates only, so it can be type-checked on its own. Other
//! platforms (development builds) find nothing and decrypt nothing.

#[cfg(windows)]
mod imp {
    use std::path::PathBuf;

    use windows_sys::Win32::Foundation::{LocalFree, ERROR_SUCCESS};
    use windows_sys::Win32::Security::Cryptography::{CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB};
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn registry_string(root: HKEY, subkey: &str, name: &str) -> Option<String> {
        let (subkey, name) = (wide(subkey), wide(name));
        let mut size: u32 = 0;
        // First the size, then the value (expanded when it's REG_EXPAND_SZ).
        let status = unsafe {
            RegGetValueW(root, subkey.as_ptr(), name.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), std::ptr::null_mut(), &mut size)
        };
        if status != ERROR_SUCCESS || size < 2 {
            return None;
        }
        let mut buffer = vec![0u16; (size as usize).div_ceil(2)];
        let status = unsafe {
            RegGetValueW(
                root,
                subkey.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buffer.as_mut_ptr().cast(),
                &mut size,
            )
        };
        if status != ERROR_SUCCESS {
            return None;
        }
        let length = buffer.iter().position(|unit| *unit == 0).unwrap_or(buffer.len());
        let value = String::from_utf16_lossy(&buffer[..length]);
        (!value.trim().is_empty()).then_some(value)
    }

    /// A string value under HKLM (`local_machine`) or HKCU.
    pub fn registry_value(local_machine: bool, subkey: &str, name: &str) -> Option<String> {
        registry_string(if local_machine { HKEY_LOCAL_MACHINE } else { HKEY_CURRENT_USER }, subkey, name)
    }

    /// The Documents known folder, which may be redirected (OneDrive).
    pub fn documents_folder() -> Option<PathBuf> {
        use windows::Win32::System::Com::CoTaskMemFree;
        use windows::Win32::UI::Shell::{FOLDERID_Documents, SHGetKnownFolderPath, KF_FLAG_DEFAULT};
        unsafe {
            let path = SHGetKnownFolderPath(&FOLDERID_Documents, KF_FLAG_DEFAULT, None).ok()?;
            let text = path.to_string().ok();
            CoTaskMemFree(Some(path.0 as *const _));
            text.map(PathBuf::from)
        }
    }

    /// `CryptUnprotectData` for the current user, without entropy, as
    /// ShareX's `DPAPI.Decrypt` does. None when it can't be decrypted here
    /// (another user's or another PC's file).
    pub fn unprotect(data: &[u8]) -> Option<Vec<u8>> {
        let input = CRYPT_INTEGER_BLOB { cbData: u32::try_from(data.len()).ok()?, pbData: data.as_ptr().cast_mut() };
        let mut output = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        let ok = unsafe {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        if ok == 0 || output.pbData.is_null() {
            return None;
        }
        let plain = unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
        unsafe {
            // Wiped before it's freed: it's a secret key.
            std::ptr::write_bytes(output.pbData, 0, output.cbData as usize);
            LocalFree(output.pbData.cast());
        }
        Some(plain)
    }

    /// `Environment.MachineName`: the NetBIOS name.
    pub fn machine_name() -> Option<String> {
        std::env::var("COMPUTERNAME").ok().filter(|name| !name.is_empty())
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::PathBuf;

    pub fn registry_value(_local_machine: bool, _subkey: &str, _name: &str) -> Option<String> {
        None
    }

    pub fn documents_folder() -> Option<PathBuf> {
        None
    }

    pub fn unprotect(_data: &[u8]) -> Option<Vec<u8>> {
        None
    }

    pub fn machine_name() -> Option<String> {
        std::env::var("COMPUTERNAME").ok().or_else(|| std::env::var("HOSTNAME").ok()).filter(|name| !name.is_empty())
    }
}

pub use imp::*;
