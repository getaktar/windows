//! Thumbnails saved in the bucket (`ThumbnailMode::Bucket`), kept in step
//! with their files: whatever deletes, renames or moves a file does the
//! same to its thumbnail in every thumbnail folder of that bucket (see
//! `bucket_prefixes`). With no such folder, none of these sends anything.

use crate::storage::{S3Provider, StorageError};

/// A thumbnail is a few kilobytes; anything far bigger at a thumbnail's key
/// isn't one.
pub const MAX_THUMBNAIL_BYTES: u64 = 2 * 1024 * 1024;

pub async fn save(storage: &S3Provider, data: Vec<u8>, object_key: &str, prefix: &str) -> Result<(), StorageError> {
    let Some(key) = super::key_for(object_key, prefix) else { return Ok(()) };
    storage.put_bytes(&key, data, super::CONTENT_TYPE).await
}

/// Called before the file itself is deleted, so a failure leaves both in
/// place rather than a thumbnail of a file that's gone. Deleting one that
/// isn't there succeeds.
pub async fn delete(storage: &S3Provider, object_key: &str, prefixes: &[String]) -> Result<(), StorageError> {
    let keys: Vec<String> = prefixes.iter().filter_map(|prefix| super::key_for(object_key, prefix)).collect();
    match keys.as_slice() {
        [] => Ok(()),
        [key] => storage.delete(key).await,
        _ => storage.delete_many(&keys).await,
    }
}

/// After a file was copied to `new_key`: its thumbnails go along. One that
/// can't be copied is made again when it's next shown.
pub async fn copy(storage: &S3Provider, old_key: &str, new_key: &str, prefixes: &[String]) {
    for prefix in prefixes {
        let (Some(source), Some(target)) = (super::key_for(old_key, prefix), super::key_for(new_key, prefix)) else { continue };
        if storage.object_exists(&source).await.unwrap_or(false) {
            let _ = storage.copy(&source, &target).await;
        }
    }
}

/// The thumbnail of the file at `object_key`, unless it was written before
/// `written_after` (Unix milliseconds: the file was replaced after it was
/// made). None when there's none or it can't be read.
pub async fn fetch(storage: &S3Provider, object_key: &str, prefix: &str, written_after: Option<i64>) -> Option<Vec<u8>> {
    let key = super::key_for(object_key, prefix)?;
    let (data, last_modified) = storage.get_small(&key, MAX_THUMBNAIL_BYTES).await.ok().flatten()?;
    // S3 keeps whole seconds.
    if let (Some(after), Some(written)) = (written_after, last_modified) {
        if written < after / 1000 * 1000 {
            return None;
        }
    }
    Some(data)
}

/// Deletes every thumbnail in a thumbnail folder, at the root and in the
/// expiring folders. Only `.webp` files, the only kind Aktar puts there, so
/// nothing else that ended up in that folder is touched.
pub async fn delete_all(storage: &S3Provider, prefix: &str) -> Result<(), StorageError> {
    let suffix = format!(".{}", super::FILE_EXTENSION);
    for root in super::roots(prefix) {
        let mut token = None;
        loop {
            let page = storage.list_recursively(&root, token).await?;
            let keys: Vec<String> = page.objects.into_iter().map(|object| object.key).filter(|key| key.ends_with(&suffix)).collect();
            storage.delete_many(&keys).await?;
            token = page.next_continuation_token;
            if token.is_none() {
                break;
            }
        }
    }
    Ok(())
}
