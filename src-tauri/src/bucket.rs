//! Bucket-browser helpers shared by the Library window and the local API.
//! S3 has no real folders: a "folder" is a key prefix ending in "/".

use crate::storage::{S3Provider, StorageError};
use crate::t;
use crate::util::split_extension;

/// "" for the bucket root, otherwise "a/b/" with exactly one trailing slash.
pub fn normalized_folder(raw: &str) -> String {
    let trimmed = raw.trim().trim_matches('/');
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}/")
    }
}

/// Checks a key, prefix, or folder name the user typed (the bucket
/// browser, the local API): no "/" at the start, no "." or ".." folders,
/// no empty folder names ("a//b"), and no control characters. A trailing
/// "/" (a folder) is fine.
pub fn check_key(key: &str) -> Result<(), String> {
    let segments: Vec<&str> = key.split('/').collect();
    let last = segments.len() - 1;
    let valid = !key.starts_with('/')
        && !key.chars().any(char::is_control)
        && segments
            .iter()
            .enumerate()
            .all(|(index, segment)| !matches!(*segment, "." | "..") && (!segment.is_empty() || index == last));
    if valid {
        Ok(())
    } else {
        Err(t!("“{0}” can’t be used as a name in the bucket. Leave out a “/” at the start, “.” and “..” as folder names, empty folder names (“//”), and control characters.", key))
    }
}

/// A folder the user typed, checked with `check_key`: "" for the bucket
/// root, otherwise "a/b/" with exactly one trailing slash.
pub fn checked_folder(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    check_key(trimmed)?;
    Ok(normalized_folder(trimmed))
}

pub fn folder_display_name(folder: &str) -> &str {
    let without_slash = folder.strip_suffix('/').unwrap_or(folder);
    without_slash.rsplit('/').next().unwrap_or(without_slash)
}

/// How many numbered names (" 2" to " 999") are tried before a random one.
const MAX_NUMBERED: u32 = 999;

/// Keeps the file's own name, adding " 2", " 3"... when it's taken (or
/// already `claimed` by another file in the same batch) so nothing is
/// overwritten. Past " 999" the name gets a random suffix instead, and
/// when even that is taken it's an error, never a taken name.
pub async fn available_key(
    storage: &S3Provider,
    filename: &str,
    folder: &str,
    claimed: &[String],
) -> Result<String, StorageError> {
    let mut counter = 1;
    loop {
        let candidate = numbered_key(folder, filename, counter);
        if !claimed.contains(&candidate) && !storage.object_exists(&candidate).await? {
            return Ok(candidate);
        }
        if counter >= MAX_NUMBERED {
            break;
        }
        counter += 1;
    }
    let suffix: String = uuid::Uuid::new_v4().simple().to_string().chars().take(8).collect();
    let candidate = suffixed_key(folder, filename, &suffix);
    if !claimed.contains(&candidate) && !storage.object_exists(&candidate).await? {
        return Ok(candidate);
    }
    Err(StorageError::Unknown(t!("An object named “{0}” already exists.", format!("{folder}{filename}"))))
}

/// "a.png" in `folder` for 1, "a 2.png" for 2, and so on.
fn numbered_key(folder: &str, filename: &str, counter: u32) -> String {
    if counter <= 1 {
        format!("{folder}{filename}")
    } else {
        suffixed_key(folder, filename, &counter.to_string())
    }
}

fn suffixed_key(folder: &str, filename: &str, suffix: &str) -> String {
    match split_extension(filename) {
        (base, "") => format!("{folder}{base} {suffix}"),
        (base, ext) => format!("{folder}{base} {suffix}.{ext}"),
    }
}

/// A key's folder ("" or "a/b/") and name.
pub fn split_key(key: &str) -> (&str, &str) {
    match key.rfind('/') {
        Some(slash) => key.split_at(slash + 1),
        None => ("", key),
    }
}

pub enum MoveError {
    Exists,
    Storage(StorageError),
}

impl From<StorageError> for MoveError {
    fn from(error: StorageError) -> Self {
        MoveError::Storage(error)
    }
}

/// Renames or moves an object. S3 does this as a copy followed by a delete,
/// and refuses to overwrite something already at `new_key`. Its thumbnails
/// in the bucket's thumbnail folders (`prefixes`) go along, and a file
/// can't be put inside one of those folders.
pub async fn move_object(storage: &S3Provider, from: &str, new_key: &str, prefixes: &[String]) -> Result<(), MoveError> {
    if crate::thumbnails::is_thumbnail(new_key, prefixes) {
        return Err(MoveError::Storage(StorageError::Unknown(t!("That folder holds this bucket’s thumbnails. Pick another one."))));
    }
    if storage.object_exists(new_key).await? {
        return Err(MoveError::Exists);
    }
    storage.copy(from, new_key).await?;
    crate::thumbnails::bucket::copy(storage, from, new_key, prefixes).await;
    crate::thumbnails::bucket::delete(storage, from, prefixes).await?;
    storage.delete(from).await?;
    Ok(())
}

/// Deletes an object, its thumbnails first (see `thumbnails::bucket`), so
/// a thumbnail never outlives its file.
pub async fn delete_object(storage: &S3Provider, key: &str, prefixes: &[String]) -> Result<(), StorageError> {
    crate::thumbnails::bucket::delete(storage, key, prefixes).await?;
    storage.delete(key).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_folders() {
        assert_eq!(normalized_folder(" /a/b// "), "a/b/");
        assert_eq!(normalized_folder("/"), "");
        assert_eq!(folder_display_name("a/b/"), "b");
        assert_eq!(folder_display_name("top/"), "top");
    }

    #[test]
    fn refuses_unsafe_keys() {
        for good in ["a", "a/b.png", "a/b/", "a b/.env", "a/...", "ü/x"] {
            assert!(check_key(good).is_ok(), "{good}");
        }
        for bad in ["/a", "a//b", "../a", "a/./b", "a/..", "a\u{0}b", "a\nb", "./"] {
            assert!(check_key(bad).is_err(), "{bad:?}");
        }
        assert_eq!(checked_folder(" ").unwrap(), "");
        assert_eq!(checked_folder("a/b").unwrap(), "a/b/");
        assert_eq!(checked_folder("a/b/").unwrap(), "a/b/");
        assert!(checked_folder("/").is_err());
        assert!(checked_folder("a/../b").is_err());
    }

    #[test]
    fn numbers_names() {
        assert_eq!(numbered_key("f/", "a.png", 1), "f/a.png");
        assert_eq!(numbered_key("f/", "a.png", 2), "f/a 2.png");
        assert_eq!(numbered_key("", "README", 3), "README 3");
        assert_eq!(suffixed_key("", "a.tar.gz", "1a2b"), "a.tar 1a2b.gz");
        assert_eq!(split_key("a/b/c.png"), ("a/b/", "c.png"));
        assert_eq!(split_key("c.png"), ("", "c.png"));
    }
}
