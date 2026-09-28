//! Bucket-browser helpers shared by the Library window and the local API.
//! S3 has no real folders: a "folder" is a key prefix ending in "/".

use crate::storage::{S3Provider, StorageError};
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

pub fn folder_display_name(folder: &str) -> &str {
    let without_slash = folder.strip_suffix('/').unwrap_or(folder);
    without_slash.rsplit('/').next().unwrap_or(without_slash)
}

/// Keeps the file's own name, adding " 2", " 3"... when it's taken (or
/// already `claimed` by another file in the same batch) so nothing is
/// overwritten.
pub async fn available_key(
    storage: &S3Provider,
    filename: &str,
    folder: &str,
    claimed: &[String],
) -> Result<String, StorageError> {
    let (base, ext) = split_extension(filename);
    let mut candidate = format!("{folder}{filename}");
    let mut counter = 2;
    loop {
        if !claimed.contains(&candidate) && !storage.object_exists(&candidate).await? {
            return Ok(candidate);
        }
        if counter >= 1000 {
            return Ok(candidate);
        }
        candidate = if ext.is_empty() {
            format!("{folder}{base} {counter}")
        } else {
            format!("{folder}{base} {counter}.{ext}")
        };
        counter += 1;
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
/// and refuses to overwrite something already at `new_key`.
pub async fn move_object(storage: &S3Provider, from: &str, new_key: &str) -> Result<(), MoveError> {
    if storage.object_exists(new_key).await? {
        return Err(MoveError::Exists);
    }
    storage.copy(from, new_key).await?;
    storage.delete(from).await?;
    Ok(())
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
}
