//! Thumbnails for files that are already in a bucket: the rows of the
//! bucket view, and history entries that have none on this PC (uploaded
//! before thumbnails were made for their kind of file). In order:
//!
//! 1. The thumbnail this PC made when it uploaded the file.
//! 2. With `ThumbnailMode::Bucket`, the one saved in the bucket, unless the
//!    file was replaced after it was made.
//! 3. One made here from the file, if it's small enough to download
//!    (`MAX_SOURCE_BYTES`), and then saved to the bucket too in that mode.
//! 4. Nothing; the row keeps its file icon, and that's remembered.
//!
//! With `ThumbnailMode::Off` nothing is looked up or downloaded at all.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use sha2::Digest;
use tokio::sync::Semaphore;

use super::ThumbnailMode;
use crate::core::SharedCore;
use crate::destinations::DestinationConfig;
use crate::storage::{BucketObject, S3Provider};

/// Larger files aren't downloaded just to make a thumbnail.
pub const MAX_SOURCE_BYTES: u64 = 25 * 1024 * 1024;
/// Thumbnails of bucket files are made again after this long.
const CACHE_LIFETIME: Duration = Duration::from_secs(30 * 86_400);
/// Downloads and generation running at once.
const MAX_RUNNING: usize = 3;

fn limit() -> &'static Semaphore {
    static LIMIT: OnceLock<Semaphore> = OnceLock::new();
    LIMIT.get_or_init(|| Semaphore::new(MAX_RUNNING))
}

/// One look-up per file at a time: two rows asking for the same file wait
/// for the first.
fn key_lock(key: &str) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    let mut locks = LOCKS.get_or_init(Default::default).lock().unwrap();
    // Locks nobody holds or waits for are dropped as new ones come.
    locks.retain(|_, lock| Arc::strong_count(lock) > 1);
    locks.entry(key.to_string()).or_default().clone()
}

fn history_in_flight() -> &'static Mutex<HashSet<String>> {
    static IN_FLIGHT: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    IN_FLIGHT.get_or_init(Default::default)
}

enum Outcome {
    Made(Vec<u8>),
    /// There's no thumbnail to be had; not tried again.
    Unavailable,
    /// Offline or the like; tried again next time.
    Failed,
}

// MARK: - Bucket view

/// The thumbnail of a file in the bucket view, or None (a file icon).
pub async fn for_object(core: &SharedCore, destination: &DestinationConfig, object: &BucketObject, prefixes: &[String]) -> Option<Vec<u8>> {
    if destination.thumbnail_mode() == ThumbnailMode::Off || object.key.ends_with('/') || super::is_thumbnail(&object.key, prefixes) {
        return None;
    }
    let file = cache_file(core, &destination.id, object);
    let lock = key_lock(&file.to_string_lossy());
    let _held = lock.lock().await;
    if file.with_extension("none").exists() {
        return None;
    }
    if let Ok(data) = std::fs::read(file.with_extension("thumb")) {
        return Some(data);
    }
    if let Some(data) = uploaded_thumbnail(core, &destination.id, object) {
        return Some(data);
    }
    let storage = provider(destination)?;
    let outcome = {
        let _permit = limit().acquire().await.ok()?;
        make(&storage, destination, &object.key, object.size.max(0) as u64, object.last_modified, None).await
    };
    match outcome {
        Outcome::Made(data) => {
            write(&file.with_extension("thumb"), &data);
            Some(data)
        }
        Outcome::Unavailable => {
            write(&file.with_extension("none"), b"");
            None
        }
        Outcome::Failed => None,
    }
}

/// The thumbnail this PC made when it uploaded the object, if the object
/// is still that upload (not replaced since).
fn uploaded_thumbnail(core: &SharedCore, destination_id: &str, object: &BucketObject) -> Option<Vec<u8>> {
    let newest = core.history.with_object(destination_id, &object.key).into_iter().next()?;
    if object.last_modified.is_some_and(|written| written > newest.created_at + 60_000) {
        return None;
    }
    std::fs::read(core.history.thumbnails.path(&newest.id)?).ok()
}

// MARK: - History

/// Makes or fetches a thumbnail for a history entry that has none. Not for
/// one whose file has expired or was replaced by a later upload to the same
/// key. True when one was made.
pub async fn for_record(core: &SharedCore, record_id: &str) -> bool {
    let store = &core.history.thumbnails;
    let Some(record) = core.history.get(record_id) else { return false };
    let Some(destination) = core.destinations.all().into_iter().find(|destination| destination.id == record.destination_id) else {
        return false;
    };
    if destination.thumbnail_mode() == ThumbnailMode::Off
        || record.expires_at.is_some_and(|expires_at| expires_at <= crate::util::now_millis())
        || store.path(&record.id).is_some()
        || store.is_unavailable(&record.id)
    {
        return false;
    }
    if !history_in_flight().lock().unwrap().insert(record.id.clone()) {
        return false;
    }
    let newer = core
        .history
        .with_object(&destination.id, &record.object_key)
        .iter()
        .any(|other| other.id != record.id && other.created_at > record.created_at);
    let outcome = if newer {
        Outcome::Unavailable
    } else if let Some(storage) = provider(&destination) {
        let _permit = limit().acquire().await;
        // The bucket's own thumbnail is written just after the file.
        let written_after = record.created_at - 120_000;
        make(&storage, &destination, &record.object_key, record.byte_size.max(0) as u64, Some(written_after), Some(record.created_at)).await
    } else {
        Outcome::Failed
    };
    history_in_flight().lock().unwrap().remove(&record.id);
    match outcome {
        Outcome::Made(data) => {
            store.store(&record.id, &data);
            true
        }
        Outcome::Unavailable => {
            store.mark_unavailable(&record.id);
            false
        }
        Outcome::Failed => false,
    }
}

// MARK: - Making one

/// `uploaded_at`: for a history entry, the file must still be that upload,
/// or the thumbnail would show another file.
async fn make(
    storage: &S3Provider,
    destination: &DestinationConfig,
    object_key: &str,
    size: u64,
    written_after: Option<i64>,
    uploaded_at: Option<i64>,
) -> Outcome {
    let prefix = destination.bucket_thumbnail_prefix();
    if let Some(prefix) = &prefix {
        if let Some(data) = super::bucket::fetch(storage, object_key, prefix, written_after).await.filter(|data| is_thumbnail(data)) {
            return Outcome::Made(data);
        }
    }

    let name = crate::bucket::split_key(object_key).1.to_string();
    if size > MAX_SOURCE_BYTES || !super::can_have_thumbnail(&name) {
        return Outcome::Unavailable;
    }
    if let Some(uploaded_at) = uploaded_at {
        match storage.last_modified(object_key).await {
            Ok(None) => return Outcome::Unavailable,
            Ok(Some(written)) if written > uploaded_at + 60_000 => return Outcome::Unavailable,
            Ok(Some(_)) => {}
            Err(_) => return Outcome::Failed,
        }
    }
    let folder = std::env::temp_dir().join("Aktar").join(format!("thumbnail-{}", crate::util::new_id()));
    if std::fs::create_dir_all(&folder).is_err() {
        return Outcome::Failed;
    }
    let file = folder.join(&name);
    let downloaded = storage.download(object_key, &file, MAX_SOURCE_BYTES).await;
    let outcome = match downloaded {
        Ok(false) => Outcome::Unavailable,
        Err(_) => Outcome::Failed,
        Ok(true) => match super::generate(file).await {
            None => Outcome::Unavailable,
            Some(data) => {
                if let (Some(prefix), true) = (&prefix, super::is_webp(&data)) {
                    let _ = super::bucket::save(storage, data.clone(), object_key, prefix).await;
                }
                Outcome::Made(data)
            }
        },
    };
    let _ = std::fs::remove_dir_all(&folder);
    outcome
}

/// Whether what's at a thumbnail's key really is one: a WebP image of
/// thumbnail size (another app's, or one made at an older size, is kept as
/// it is).
fn is_thumbnail(data: &[u8]) -> bool {
    super::is_webp(data)
        && image::ImageReader::new(std::io::Cursor::new(data))
            .with_guessed_format()
            .ok()
            .and_then(|reader| reader.into_dimensions().ok())
            .is_some_and(|(width, height)| width.max(height) <= super::MAX_PIXEL_SIZE * 2)
}

fn provider(destination: &DestinationConfig) -> Option<S3Provider> {
    let credentials = crate::credentials::load(&destination.id).ok()?;
    Some(S3Provider::new(destination.clone(), credentials))
}

// MARK: - Cache

/// Named after the object's size and date too, so a replaced file doesn't
/// show the old file's thumbnail. Without an extension.
fn cache_file(core: &SharedCore, destination_id: &str, object: &BucketObject) -> PathBuf {
    let identity = format!("{}\n{}\n{}", object.key, object.size, object.last_modified.unwrap_or_default());
    key_folder(core, destination_id, &object.key).join(hash(&identity))
}

fn key_folder(core: &SharedCore, destination_id: &str, key: &str) -> PathBuf {
    core.bucket_thumbnails.join(destination_id).join(hash(key))
}

fn hash(text: &str) -> String {
    crate::util::hex(&sha2::Sha256::digest(text.as_bytes()))
}

fn write(path: &Path, data: &[u8]) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = super::store::write_atomically(path, data);
}

/// What was made from a file the bucket no longer has under that key.
pub fn forget(core: &SharedCore, destination_id: &str, key: &str) {
    let _ = std::fs::remove_dir_all(key_folder(core, destination_id, key));
}

/// Everything made for a destination's bucket, when its thumbnails are
/// turned off or it points at another bucket.
pub fn forget_destination(core: &SharedCore, destination_id: &str) {
    let _ = std::fs::remove_dir_all(core.bucket_thumbnails.join(destination_id));
}

/// Settings > General > Clear: every bucket's.
pub fn forget_all(core: &SharedCore) {
    let _ = std::fs::remove_dir_all(&core.bucket_thumbnails);
}

/// Drops what hasn't been written for `CACHE_LIFETIME`; at launch.
pub fn prune(folder: &Path) {
    let Ok(entries) = std::fs::read_dir(folder) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.metadata() {
            Ok(metadata) if metadata.is_dir() => {
                prune(&path);
                let _ = std::fs::remove_dir(&path);
            }
            Ok(metadata) => {
                let old = metadata.modified().ok().and_then(|modified| SystemTime::now().duration_since(modified).ok());
                if old.is_some_and(|age| age > CACHE_LIFETIME) {
                    let _ = std::fs::remove_file(&path);
                }
            }
            Err(_) => {}
        }
    }
}
