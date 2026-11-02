//! Uploads whose link was copied before their bytes were all in the bucket
//! (the Mac app's docs/capture-and-instant-links.md, "Outbox"): kept until
//! they are, so the link handed out still starts working after Aktar quits,
//! crashes or the PC restarts. One folder per upload in Local AppData (the
//! copies can be big, so never in the roaming profile): `entry.json`, in
//! the Mac app's format, and `file\<name>`, a copy of what the upload
//! started from.
//!
//! NTFS has no instant copy like APFS's clones, so a file up to
//! `MAX_COPY_BYTES` is copied, and a bigger one is kept as a hard link on
//! the same disk (instant; it shares the bytes, which apps that save by
//! writing a new file and renaming it don't touch). A bigger file that
//! can't be linked (another disk, FAT) isn't kept, and its upload only goes
//! on while Aktar runs.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Older entries are dropped at launch, with their copy of the file, like
/// unfinished multipart uploads.
pub const MAX_AGE_MILLIS: i64 = 7 * 86_400_000;
/// Bigger files (and folders) are linked rather than copied.
pub const MAX_COPY_BYTES: u64 = 512 * 1024 * 1024;

/// The link copied before an upload finished. From then on the upload goes
/// to `object_key` and nowhere else, also when it's retried or continued
/// after a restart.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EarlyLink {
    pub object_key: String,
    /// The public URL or temporary link that was copied (or shortened).
    pub link: String,
    /// When `link` is a temporary link, when it stops working.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "iso_date_option")]
    pub temporary_expires_at: Option<i64>,
    /// The short link made for `link`, not in history yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_link: Option<EarlyShortLink>,
    /// Why the short link couldn't be made (the long link was copied); said
    /// once the upload is done, when Retry has an upload to work on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_link_error: Option<String>,
    /// What was put on the clipboard.
    pub copied_text: String,
}

/// A short link made at the provider for an early link.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EarlyShortLink {
    pub short_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    pub provider: String,
    pub provider_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    pub target_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "iso_date_option")]
    pub expires_at: Option<i64>,
}

/// An upload whose link was copied and that hasn't finished: what it needs
/// to go on after a restart.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutboxEntry {
    /// The upload job's ID.
    pub id: String,
    #[serde(rename = "destinationID")]
    pub destination_id: String,
    pub original_filename: String,
    /// The file's name inside the entry's folder.
    pub stored_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiry_days: Option<u32>,
    /// The upload's own Short Links override (the local API's short=).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_link_override: Option<bool>,
    pub early_link: EarlyLink,
    /// When the first try started: an object at the key written after this
    /// is the upload's own, from a try whose answer was lost.
    #[serde(with = "iso_date")]
    pub first_attempt_at: i64,
    /// A failure that won't pass by itself; such an entry isn't started at
    /// launch, it waits for Retry or Cancel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
}

impl OutboxEntry {
    pub fn is_stale(&self, now: i64) -> bool {
        now - self.first_attempt_at > MAX_AGE_MILLIS
    }
}

/// The kept uploads, one folder each under `directory`.
pub struct Outbox {
    directory: PathBuf,
}

impl Outbox {
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    fn folder(&self, id: &str) -> PathBuf {
        self.directory.join(id)
    }

    fn manifest(&self, id: &str) -> PathBuf {
        self.folder(id).join("entry.json")
    }

    pub fn file_path(&self, entry: &OutboxEntry) -> PathBuf {
        self.folder(&entry.id).join("file").join(&entry.stored_name)
    }

    pub fn contains(&self, id: &str) -> bool {
        is_id(id) && self.manifest(id).is_file()
    }

    /// Writes the entry, keeping a copy of `source` the first time. Blocks
    /// while a file is copied; call it off the async runtime's workers.
    /// False when the file couldn't (or shouldn't) be kept; nothing is left
    /// behind then.
    pub fn save(&self, entry: &OutboxEntry, source: &Path) -> bool {
        if !is_id(&entry.id) {
            return false;
        }
        if self.contains(&entry.id) {
            return self.update(entry);
        }
        let target = self.file_path(entry);
        let kept = target.parent().is_some_and(|parent| std::fs::create_dir_all(parent).is_ok()) && keep_copy(source, &target);
        if kept && self.update(entry) {
            return true;
        }
        self.remove(&entry.id);
        false
    }

    /// Rewrites the entry of an upload that's already kept.
    pub fn update(&self, entry: &OutboxEntry) -> bool {
        if !is_id(&entry.id) || !self.folder(&entry.id).is_dir() {
            return false;
        }
        let Ok(data) = serde_json::to_vec_pretty(entry) else { return false };
        let manifest = self.manifest(&entry.id);
        let temporary = manifest.with_extension("json.tmp");
        std::fs::write(&temporary, data).is_ok() && std::fs::rename(&temporary, &manifest).is_ok()
    }

    /// Removes an entry and its copy. A copy the upload still has open
    /// (Windows won't delete an open file) goes once it's closed: this
    /// tries again for a few seconds, off the caller's thread.
    pub fn remove(&self, id: &str) {
        if !is_id(id) {
            return;
        }
        let folder = self.folder(id);
        if std::fs::remove_dir_all(&folder).is_ok() || !folder.exists() {
            return;
        }
        std::thread::spawn(move || {
            for _ in 0..20 {
                std::thread::sleep(std::time::Duration::from_millis(500));
                if std::fs::remove_dir_all(&folder).is_ok() || !folder.exists() {
                    return;
                }
            }
            log::warn!("Could not remove {} from the outbox", folder.display());
        });
    }

    /// Every kept upload whose file is still there, oldest first. Folders
    /// that can't be read, and entries older than `MAX_AGE_MILLIS`, are
    /// removed.
    pub fn load(&self, now: i64) -> Vec<(OutboxEntry, PathBuf)> {
        let Ok(folders) = std::fs::read_dir(&self.directory) else { return Vec::new() };
        let mut result = Vec::new();
        for folder in folders.flatten() {
            let id = folder.file_name().to_string_lossy().into_owned();
            let entry = std::fs::read(self.manifest(&id))
                .ok()
                .and_then(|data| serde_json::from_slice::<OutboxEntry>(&data).ok())
                .filter(|entry| is_id(&id) && entry.id == id && !entry.is_stale(now) && self.file_path(entry).exists());
            match entry {
                Some(entry) => {
                    let path = self.file_path(&entry);
                    result.push((entry, path));
                }
                None => {
                    let _ = std::fs::remove_dir_all(folder.path());
                }
            }
        }
        result.sort_by_key(|(entry, _)| entry.first_attempt_at);
        result
    }
}

/// A job ID (an uppercase UUID): anything else never names a folder here.
fn is_id(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok() && !id.contains(['/', '\\', '.'])
}

/// Copies a file up to `MAX_COPY_BYTES`, or links a bigger one on the same
/// disk; copies a folder (a ZIP upload) up to `MAX_COPY_BYTES` in all.
fn keep_copy(source: &Path, target: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(source) else { return false };
    if metadata.is_dir() {
        return folder_size(source, MAX_COPY_BYTES) <= MAX_COPY_BYTES && copy_folder(source, target).is_ok();
    }
    if metadata.len() <= MAX_COPY_BYTES {
        return std::fs::copy(source, target).is_ok();
    }
    std::fs::hard_link(source, target).is_ok()
}

/// A folder's size, counting up to just past `limit`.
fn folder_size(folder: &Path, limit: u64) -> u64 {
    let mut total = 0;
    let mut pending = vec![folder.to_path_buf()];
    while let Some(current) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else { continue };
        for entry in entries.flatten() {
            let Ok(metadata) = entry.metadata() else { continue };
            if metadata.is_dir() {
                pending.push(entry.path());
            } else {
                total += metadata.len();
            }
            if total > limit {
                return total;
            }
        }
    }
    total
}

fn copy_folder(source: &Path, target: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let destination = target.join(entry.file_name());
        // Links and junctions are left out, as a folder upload leaves them.
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            copy_folder(&entry.path(), &destination)?;
        } else {
            std::fs::copy(entry.path(), destination)?;
        }
    }
    Ok(())
}

/// Dates as ISO 8601 in UTC with whole seconds, like the Mac app's outbox.
mod iso_date {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(millis: &i64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&crate::util::iso8601(*millis))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
        let text = String::deserialize(deserializer)?;
        crate::util::parse_iso8601(&text).ok_or_else(|| serde::de::Error::custom("not an ISO 8601 date"))
    }
}

mod iso_date_option {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(millis: &Option<i64>, serializer: S) -> Result<S::Ok, S::Error> {
        match millis {
            Some(millis) => serializer.serialize_str(&crate::util::iso8601(*millis)),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<i64>, D::Error> {
        let text = Option::<String>::deserialize(deserializer)?;
        match text {
            Some(text) => crate::util::parse_iso8601(&text).map(Some).ok_or_else(|| serde::de::Error::custom("not an ISO 8601 date")),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Folder(PathBuf);

    impl Folder {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("aktar-outbox-test-{}", crate::util::new_id()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Whole seconds, as the dates come back.
    fn now() -> i64 {
        crate::util::now_millis() / 1000 * 1000
    }

    fn entry(id: &str, first_attempt_at: i64) -> OutboxEntry {
        OutboxEntry {
            id: id.to_string(),
            destination_id: "D".into(),
            original_filename: "photo.png".into(),
            stored_name: "photo.png".into(),
            expiry_days: Some(7),
            short_link_override: None,
            early_link: EarlyLink {
                object_key: "tmp/7d/AbC1234.png".into(),
                link: "https://cdn.example.com/tmp/7d/AbC1234.png".into(),
                temporary_expires_at: None,
                short_link: Some(EarlyShortLink {
                    short_url: "https://s.example.com/x".into(),
                    provider_id: Some("x".into()),
                    provider: "shlink".into(),
                    provider_name: "Shlink".into(),
                    domain: None,
                    target_url: "https://cdn.example.com/tmp/7d/AbC1234.png".into(),
                    expires_at: Some(first_attempt_at + 86_400_000),
                }),
                short_link_error: None,
                copied_text: "https://s.example.com/x".into(),
            },
            first_attempt_at,
            failure: None,
        }
    }

    #[test]
    fn writes_the_mac_apps_format() {
        let entry = entry("0A1B2C3D-0000-4000-8000-000000000001", 1_793_613_600_000);
        let json: serde_json::Value = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["destinationID"], "D");
        assert_eq!(json["firstAttemptAt"], "2026-11-02T10:00:00Z");
        assert_eq!(json["earlyLink"]["objectKey"], "tmp/7d/AbC1234.png");
        assert_eq!(json["earlyLink"]["shortLink"]["shortUrl"], "https://s.example.com/x");
        assert_eq!(json["earlyLink"]["shortLink"]["expiresAt"], "2026-11-03T10:00:00Z");
        assert_eq!(json["expiryDays"], 7);
        // Unset values are left out, as Swift's Codable leaves them.
        assert!(json.get("failure").is_none());
        assert!(json.get("shortLinkOverride").is_none());
        assert!(json["earlyLink"].get("temporaryExpiresAt").is_none());
        assert_eq!(serde_json::from_value::<OutboxEntry>(json).unwrap(), entry);
    }

    #[test]
    fn keeps_updates_and_removes_uploads() {
        let folder = Folder::new();
        let outbox = Outbox::new(folder.0.join("outbox"));
        let source = folder.0.join("photo.png");
        std::fs::write(&source, b"png bytes").unwrap();
        let id = crate::util::new_id();
        let mut kept = entry(&id, now());
        assert!(outbox.save(&kept, &source));
        assert!(outbox.contains(&id));
        // The copy stays when the original goes.
        std::fs::remove_file(&source).unwrap();
        let loaded = outbox.load(now());
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].0, kept);
        assert_eq!(std::fs::read(&loaded[0].1).unwrap(), b"png bytes");

        kept.failure = Some("Access denied".into());
        assert!(outbox.update(&kept));
        assert_eq!(outbox.load(now())[0].0.failure.as_deref(), Some("Access denied"));

        outbox.remove(&id);
        assert!(!outbox.contains(&id));
        assert!(outbox.load(now()).is_empty());
    }

    #[test]
    fn drops_old_and_broken_entries() {
        let folder = Folder::new();
        let outbox = Outbox::new(folder.0.join("outbox"));
        let source = folder.0.join("a.txt");
        std::fs::write(&source, b"a").unwrap();
        let old = crate::util::new_id();
        assert!(outbox.save(&entry(&old, now() - MAX_AGE_MILLIS - 60_000), &source));
        let fresh = crate::util::new_id();
        assert!(outbox.save(&entry(&fresh, now()), &source));
        // A folder without a readable entry, and one whose file is gone.
        std::fs::create_dir_all(folder.0.join("outbox").join(crate::util::new_id())).unwrap();
        let missing = crate::util::new_id();
        assert!(outbox.save(&entry(&missing, now()), &source));
        std::fs::remove_dir_all(folder.0.join("outbox").join(&missing).join("file")).unwrap();

        let loaded = outbox.load(now());
        assert_eq!(loaded.iter().map(|(entry, _)| entry.id.clone()).collect::<Vec<_>>(), vec![fresh.clone()]);
        assert_eq!(std::fs::read_dir(folder.0.join("outbox")).unwrap().count(), 1);
    }

    #[test]
    fn keeps_folders_for_zip_uploads() {
        let folder = Folder::new();
        let outbox = Outbox::new(folder.0.join("outbox"));
        let source = folder.0.join("Project");
        std::fs::create_dir_all(source.join("docs")).unwrap();
        std::fs::write(source.join("docs").join("a.md"), b"# A").unwrap();
        let id = crate::util::new_id();
        let mut kept = entry(&id, now());
        kept.stored_name = "Project".into();
        assert!(outbox.save(&kept, &source));
        let loaded = outbox.load(now());
        assert_eq!(std::fs::read(loaded[0].1.join("docs").join("a.md")).unwrap(), b"# A");
    }

    #[test]
    fn never_uses_names_that_arent_ids() {
        let folder = Folder::new();
        let outbox = Outbox::new(folder.0.join("outbox"));
        let source = folder.0.join("a.txt");
        std::fs::write(&source, b"a").unwrap();
        assert!(!outbox.save(&entry("..", now()), &source));
        assert!(!outbox.save(&entry("../escape", now()), &source));
        assert!(!folder.0.join("escape").exists());
    }
}
