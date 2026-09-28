use std::path::Path;

use serde::Serialize;

/// Writes to a sibling temp file and renames it over the target, so a crash
/// mid-write never leaves a truncated settings or destinations file.
pub fn write_json_atomically(path: &Path, value: &impl Serialize) {
    let Ok(data) = serde_json::to_vec_pretty(value) else { return };
    let temp = path.with_extension("json.tmp");
    if std::fs::write(&temp, data).is_ok() {
        let _ = std::fs::rename(&temp, path);
    }
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
    fn formats_dates_without_fractions() {
        assert_eq!(iso8601(1_790_000_000_123), "2026-09-21T14:13:20Z");
    }
}
