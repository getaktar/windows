//! What's watched and how, kept in `watched-folders.json` next to
//! `destinations.json`. The keys are the same as the Mac app's, so the two
//! stay in step.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchedFolder {
    pub id: String,
    /// Shown everywhere, and `{folder}` in object keys. The folder's own
    /// name unless the user picks another.
    pub name: String,
    pub path: PathBuf,
    #[serde(default = "enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub preset: Option<Preset>,
    /// None uploads to the default destination.
    #[serde(rename = "destinationID", default)]
    pub destination_id: Option<String>,
    /// None uses the destination's own path template.
    #[serde(default)]
    pub path_template: Option<String>,
    #[serde(default)]
    pub subfolders: Subfolders,
    #[serde(default)]
    pub filter: Filter,
    /// Upload online-only (OneDrive) files too, which downloads them.
    #[serde(default)]
    pub include_cloud_only: bool,
    #[serde(default)]
    pub modified: Modified,
    #[serde(default)]
    pub after_upload: AfterUpload,
    /// What happens to an upload when its file is deleted from the folder.
    /// Only while the original stays in the folder (`AfterUpload::Keep`).
    #[serde(default)]
    pub on_delete: OnDelete,
    /// "Ask before deleting": with `OnDelete::DeleteRemote`, every delete
    /// from the bucket waits for the user. On unless turned off.
    #[serde(default = "enabled")]
    pub confirm_delete: bool,
    #[serde(default)]
    pub clipboard: ClipboardPolicy,
    #[serde(default)]
    pub notifications: Notifications,
    /// None follows the destination's "Link".
    #[serde(default)]
    pub temporary_link: Option<LinkOverride>,
    /// None follows the destination's "Delete after"; 0 keeps uploads.
    #[serde(default)]
    pub expiry_days: Option<u32>,
    #[serde(default)]
    pub hooks: Vec<Hook>,
    #[serde(default = "now_iso")]
    pub added_at: String,
}

fn enabled() -> bool {
    true
}

fn now_iso() -> String {
    crate::util::iso8601(crate::util::now_millis())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Preset {
    Screenshots,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Subfolders {
    #[default]
    Ignore,
    KeepStructure,
    Flatten,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Filter {
    pub kind: FilterKind,
    /// Globs, for `FilterKind::Custom`.
    pub include: Vec<String>,
    /// Always applied, on top of the built-in ignore list.
    pub exclude: Vec<String>,
    pub min_bytes: Option<u64>,
    pub max_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FilterKind {
    #[default]
    All,
    Images,
    Videos,
    /// Windows has no screenshot marker on files: images only.
    Screenshots,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Modified {
    #[default]
    Ignore,
    UploadAgain,
    /// Uploads to the same key, so the link stays the same.
    Overwrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AfterUpload {
    #[default]
    Keep,
    /// The Recycle Bin, never a permanent delete.
    Trash,
    MoveToUploaded,
    /// A Finder tag on the Mac; kept as is on Windows.
    Tag,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OnDelete {
    #[default]
    Keep,
    /// Deletes the upload from the bucket too, after a grace period.
    DeleteRemote,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ClipboardPolicy {
    CopyLink,
    #[default]
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Notifications {
    Each,
    #[default]
    Grouped,
    FailuresOnly,
}

/// A watched folder's "Link": a temporary link valid this many seconds, or
/// the public URL whatever the destination copies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkOverride {
    Public,
    Temporary(u64),
}

impl Serialize for LinkOverride {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            LinkOverride::Public => serializer.serialize_str("public"),
            LinkOverride::Temporary(seconds) => serializer.serialize_u64(*seconds),
        }
    }
}

impl<'de> Deserialize<'de> for LinkOverride {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match serde_json::Value::deserialize(deserializer)? {
            serde_json::Value::String(text) if text == "public" => Ok(LinkOverride::Public),
            serde_json::Value::Number(number) => {
                number.as_u64().map(LinkOverride::Temporary).ok_or_else(|| serde::de::Error::custom("invalid link"))
            }
            _ => Err(serde::de::Error::custom("invalid link")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hook {
    pub id: String,
    pub kind: HookKind,
    /// The webhook's URL, or the script's path.
    pub target: String,
    #[serde(default = "enabled")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HookKind {
    Webhook,
    Script,
}

/// The global pause: none, until a moment (Unix milliseconds), or until
/// the user resumes. Stored as null, an ISO 8601 date, or "forever".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pause {
    #[default]
    None,
    Until(i64),
    Forever,
}

impl Pause {
    pub fn is_active(self, now_millis: i64) -> bool {
        match self {
            Pause::None => false,
            Pause::Until(until) => until > now_millis,
            Pause::Forever => true,
        }
    }

    /// How the local API and the windows show it.
    pub fn iso(self) -> Option<String> {
        match self {
            Pause::None => None,
            Pause::Until(until) => Some(crate::util::iso8601(until)),
            Pause::Forever => Some("forever".into()),
        }
    }
}

impl Serialize for Pause {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.iso() {
            Some(text) => serializer.serialize_str(&text),
            None => serializer.serialize_none(),
        }
    }
}

impl<'de> Deserialize<'de> for Pause {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match Option::<String>::deserialize(deserializer)? {
            None => Pause::None,
            Some(text) if text == "forever" => Pause::Forever,
            Some(text) => chrono::DateTime::parse_from_rfc3339(&text).map_or(Pause::None, |date| Pause::Until(date.timestamp_millis())),
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WatchedFolders {
    pub folders: Vec<WatchedFolder>,
    pub paused_until: Pause,
    pub pause_on_battery: bool,
    /// Windows' metered connections.
    pub pause_on_metered: bool,
}

impl WatchedFolder {
    /// A new folder with the defaults: everything in it, nothing done to
    /// the originals, one notification per batch.
    pub fn new(path: &Path) -> Self {
        Self {
            id: crate::util::new_id(),
            name: default_name(path),
            path: path.to_path_buf(),
            enabled: true,
            preset: None,
            destination_id: None,
            path_template: None,
            subfolders: Subfolders::default(),
            filter: Filter::default(),
            include_cloud_only: false,
            modified: Modified::default(),
            after_upload: AfterUpload::default(),
            on_delete: OnDelete::default(),
            confirm_delete: true,
            clipboard: ClipboardPolicy::default(),
            notifications: Notifications::default(),
            temporary_link: None,
            expiry_days: None,
            hooks: Vec::new(),
            added_at: now_iso(),
        }
    }

    /// The screenshots preset: its link copied and a notification for each.
    pub fn screenshots(path: &Path) -> Self {
        Self {
            name: "Screenshots".into(),
            preset: Some(Preset::Screenshots),
            filter: Filter { kind: FilterKind::Screenshots, ..Filter::default() },
            clipboard: ClipboardPolicy::CopyLink,
            notifications: Notifications::Each,
            ..Self::new(path)
        }
    }

    pub fn recursive(&self) -> bool {
        self.subfolders != Subfolders::Ignore
    }

    /// Whether deleting a file deletes its upload: only when it's set, and
    /// the original stays in the folder (Aktar's own moves aren't
    /// deletions).
    pub fn deletes_remote(&self) -> bool {
        self.on_delete == OnDelete::DeleteRemote && matches!(self.after_upload, AfterUpload::Keep | AfterUpload::Tag)
    }

    /// Values no version offers (edited by hand, or from a newer version)
    /// fall back to the defaults.
    fn sanitize(&mut self) {
        if self.name.trim().is_empty() {
            self.name = default_name(&self.path);
        }
        if self.expiry_days.is_some_and(|days| days != 0 && !crate::expiry::is_valid(days)) {
            self.expiry_days = None;
        }
        if let Some(LinkOverride::Temporary(seconds)) = self.temporary_link {
            if !crate::destinations::TEMPORARY_LINK_SECONDS.contains(&seconds) {
                self.temporary_link = None;
            }
        }
        if self.path_template.as_deref().is_some_and(|template| template.trim().is_empty()) {
            self.path_template = None;
        }
        // No Finder tags on Windows.
        if self.after_upload == AfterUpload::Tag {
            self.after_upload = AfterUpload::Keep;
        }
    }
}

/// The folder's own name, or the drive's ("D:") for a drive's root.
pub fn default_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().trim_end_matches(['\\', '/']).to_string())
}

pub struct WatchedFolderStore {
    path: Option<PathBuf>,
    inner: Mutex<WatchedFolders>,
}

impl WatchedFolderStore {
    pub fn load(directory: &Path) -> Self {
        let path = directory.join("watched-folders.json");
        let mut list: WatchedFolders = std::fs::read(&path)
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default();
        list.folders.iter_mut().for_each(WatchedFolder::sanitize);
        Self { path: Some(path), inner: Mutex::new(list) }
    }

    /// Kept in memory only, for tests.
    #[cfg(test)]
    pub fn in_memory(list: WatchedFolders) -> Self {
        Self { path: None, inner: Mutex::new(list) }
    }

    pub fn get(&self) -> WatchedFolders {
        self.inner.lock().unwrap().clone()
    }

    pub fn folder(&self, id: &str) -> Option<WatchedFolder> {
        self.inner.lock().unwrap().folders.iter().find(|folder| folder.id.eq_ignore_ascii_case(id)).cloned()
    }

    pub fn update(&self, change: impl FnOnce(&mut WatchedFolders)) -> WatchedFolders {
        let mut inner = self.inner.lock().unwrap();
        change(&mut inner);
        inner.folders.iter_mut().for_each(WatchedFolder::sanitize);
        if let Some(path) = &self.path {
            crate::util::write_json_atomically(path, &*inner);
        }
        inner.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_writes_the_shared_keys() {
        let json = r#"{"folders":[{"id":"A","name":"Shots","path":"C:\\Shots","enabled":true,"preset":"screenshots",
            "destinationID":null,"pathTemplate":"{folder}/{filename}.{ext}","subfolders":"keepStructure",
            "filter":{"kind":"custom","include":["*.png"],"exclude":["*.psd"],"minBytes":10,"maxBytes":null},
            "includeCloudOnly":false,"modified":"overwrite","afterUpload":"moveToUploaded","onDelete":"deleteRemote","clipboard":"copyLink",
            "notifications":"each","temporaryLink":"public","expiryDays":7,"bookmark":"ignored",
            "hooks":[{"id":"H","kind":"webhook","target":"https://example.com","enabled":true}],"addedAt":"2026-10-01T10:00:00Z"}],
            "pausedUntil":"forever","pauseOnBattery":true,"pauseOnMetered":false}"#;
        let list: WatchedFolders = serde_json::from_str(json).unwrap();
        let folder = &list.folders[0];
        assert_eq!(folder.preset, Some(Preset::Screenshots));
        assert_eq!(folder.subfolders, Subfolders::KeepStructure);
        assert_eq!(folder.filter.kind, FilterKind::Custom);
        assert_eq!(folder.filter.min_bytes, Some(10));
        assert_eq!(folder.temporary_link, Some(LinkOverride::Public));
        assert_eq!(folder.after_upload, AfterUpload::MoveToUploaded);
        assert_eq!(folder.on_delete, OnDelete::DeleteRemote);
        // Moved away after upload: deleting it is Aktar's doing, not the user's.
        assert!(!folder.deletes_remote());
        assert_eq!(list.paused_until, Pause::Forever);

        let written = serde_json::to_value(&list).unwrap();
        assert_eq!(written["pausedUntil"], "forever");
        assert_eq!(written["folders"][0]["destinationID"], serde_json::Value::Null);
        assert_eq!(written["folders"][0]["temporaryLink"], "public");
        assert_eq!(written["folders"][0]["onDelete"], "deleteRemote");
        assert_eq!(written["folders"][0]["confirmDelete"], true);
        let quiet: WatchedFolder = serde_json::from_str(r#"{"id":"B","name":"B","path":"C:\\B","confirmDelete":false}"#).unwrap();
        assert!(!quiet.confirm_delete);
        assert_eq!(written["folders"][0]["filter"]["minBytes"], 10);

        let mut folder = folder.clone();
        folder.temporary_link = Some(LinkOverride::Temporary(3600));
        assert_eq!(serde_json::to_value(&folder).unwrap()["temporaryLink"], 3600);

        let paused: WatchedFolders = serde_json::from_str(r#"{"pausedUntil":"2026-10-01T14:00:00Z"}"#).unwrap();
        assert!(matches!(paused.paused_until, Pause::Until(_)));
        assert!(paused.folders.is_empty());
        assert!(!paused.pause_on_battery);
    }

    #[test]
    fn falls_back_to_defaults() {
        let list: WatchedFolders =
            serde_json::from_str(r#"{"folders":[{"id":"A","name":" ","path":"C:\\Users\\me\\Shots","expiryDays":3,"temporaryLink":42,"afterUpload":"tag"}]}"#)
                .unwrap();
        let store = WatchedFolderStore::in_memory(list);
        let folder = &store.update(|_| {}).folders[0];
        assert!(folder.enabled);
        assert_eq!(folder.expiry_days, None);
        assert_eq!(folder.temporary_link, None);
        assert_eq!(folder.after_upload, AfterUpload::Keep);
        assert_eq!(folder.notifications, Notifications::Grouped);
        assert_eq!(folder.on_delete, OnDelete::Keep);
        // Missing: asks.
        assert!(folder.confirm_delete);
        assert!(!folder.name.trim().is_empty());
    }
}
