use std::path::Path;

use serde::Serialize;

/// Writes to a sibling temp file and renames it over the target, so a crash
/// mid-write never leaves a truncated settings or destinations file.
pub fn write_json_atomically(path: &Path, value: &impl Serialize) {
    let data = match serde_json::to_vec_pretty(value) {
        Ok(data) => data,
        Err(error) => return log::error!("Could not encode {}: {error}", path.display()),
    };
    let temp = path.with_extension("json.tmp");
    if let Err(error) = std::fs::write(&temp, data) {
        return log::error!("Could not write {}: {error}", temp.display());
    }
    // Antivirus scanners and sync clients (OneDrive) briefly open a file
    // they see change, and renaming over a file that's open fails with
    // "access denied" on Windows, so give them a moment.
    const ATTEMPTS: u64 = 8;
    for attempt in 1..=ATTEMPTS {
        match std::fs::rename(&temp, path) {
            Ok(()) => return,
            Err(error) if attempt == ATTEMPTS => log::error!("Could not save {}: {error}", path.display()),
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(25 * attempt)),
        }
    }
}

/// MD5 and SHA-256 of a file, read a piece at a time so a big file is
/// never in memory whole.
pub fn content_hashes(path: &Path) -> std::io::Result<crate::output::ContentHashes> {
    use md5::Digest as _;
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let (mut md5, mut sha256) = (md5::Md5::new(), sha2::Sha256::new());
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        md5.update(&buffer[..read]);
        sha2::Digest::update(&mut sha256, &buffer[..read]);
    }
    Ok(crate::output::ContentHashes { md5: hex(&md5.finalize()), sha256: hex(&sha2::Digest::finalize(sha256)) })
}

/// SHA-256 of a file alone (a watched folder telling a change from a
/// touch), read a piece at a time.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut sha256 = <sha2::Sha256 as sha2::Digest>::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        sha2::Digest::update(&mut sha256, &buffer[..read]);
    }
    Ok(hex(&sha2::Digest::finalize(sha256)))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// ISO 8601 in UTC with whole seconds, the format the Mac app's local API
/// uses (`JSONEncoder.DateEncodingStrategy.iso8601`).
pub fn iso8601(millis: i64) -> String {
    chrono::DateTime::from_timestamp_millis(millis)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Uppercase UUIDs, like `UUID().uuidString` on the Mac.
pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string().to_uppercase()
}

/// Splits a file name the way `NSString.pathExtension` does: "a.tar.gz" is
/// ("a.tar", "gz"), and a leading-dot name like ".env" has no extension.
pub fn split_extension(filename: &str) -> (&str, &str) {
    match filename.rfind('.') {
        Some(index) if index > 0 && index + 1 < filename.len() => (&filename[..index], &filename[index + 1..]),
        _ => (filename, ""),
    }
}

/// The last path component, accepting both separators since keys use "/"
/// and Windows paths use "\".
pub fn last_component(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_extensions_like_foundation() {
        assert_eq!(split_extension("photo.png"), ("photo", "png"));
        assert_eq!(split_extension("archive.tar.gz"), ("archive.tar", "gz"));
        assert_eq!(split_extension(".env"), (".env", ""));
        assert_eq!(split_extension("README"), ("README", ""));
        assert_eq!(split_extension("trailing."), ("trailing.", ""));
    }

    #[test]
    fn hashes_contents() {
        let path = std::env::temp_dir().join(format!("aktar-hash-{}", new_id()));
        std::fs::write(&path, b"hello").unwrap();
        let hashes = content_hashes(&path).unwrap();
        assert_eq!(hashes.md5, "5d41402abc4b2a76b9719d911017c592");
        assert_eq!(hashes.sha256, "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824");
        assert_eq!(sha256_file(&path).unwrap(), hashes.sha256);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn formats_dates_without_fractions() {
        assert_eq!(iso8601(1_790_000_000_123), "2026-09-21T14:13:20Z");
    }
}
