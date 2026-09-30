//! Non-secret destination configuration, persisted as JSON. Secrets never
//! live here, see `credentials`.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::output::OutputMode;
use crate::t;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProviderPreset {
    #[serde(rename = "cloudflareR2")]
    CloudflareR2,
    #[serde(rename = "amazonS3")]
    AmazonS3,
    #[serde(rename = "minIO")]
    MinIO,
    #[serde(rename = "backblazeB2")]
    BackblazeB2,
    #[serde(rename = "digitalOceanSpaces")]
    DigitalOceanSpaces,
    #[serde(rename = "customS3")]
    CustomS3,
}

impl ProviderPreset {
    /// The raw value the Mac app uses, which the local API reports as `provider`.
    pub fn raw_value(self) -> &'static str {
        match self {
            ProviderPreset::CloudflareR2 => "cloudflareR2",
            ProviderPreset::AmazonS3 => "amazonS3",
            ProviderPreset::MinIO => "minIO",
            ProviderPreset::BackblazeB2 => "backblazeB2",
            ProviderPreset::DigitalOceanSpaces => "digitalOceanSpaces",
            ProviderPreset::CustomS3 => "customS3",
        }
    }

    pub fn display_name(self) -> String {
        match self {
            ProviderPreset::CloudflareR2 => "Cloudflare R2".into(),
            ProviderPreset::AmazonS3 => "Amazon S3".into(),
            ProviderPreset::MinIO => "MinIO".into(),
            ProviderPreset::BackblazeB2 => "Backblaze B2".into(),
            ProviderPreset::DigitalOceanSpaces => "DigitalOcean Spaces".into(),
            ProviderPreset::CustomS3 => t!("Other S3-Compatible"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DestinationConfig {
    pub id: String,
    pub name: String,
    pub preset: ProviderPreset,
    #[serde(rename = "accountID", default)]
    pub account_id: Option<String>,
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    #[serde(rename = "publicBaseURL")]
    pub public_base_url: String,
    pub object_path_template: String,
    pub force_path_style: bool,
    #[serde(default)]
    pub is_default: bool,
    /// What's copied after an upload here; none follows Settings > Output.
    /// With the fields below, destinations work as upload profiles
    /// ("Builds", "Logs", "Screenshots"), even several on one bucket.
    #[serde(default)]
    pub output_mode: Option<OutputMode>,
    /// "Delete after" for uploads here, in days (0 keeps them); none until
    /// it's picked for this destination, when Settings' last choice applies.
    #[serde(default)]
    pub expiry_days: Option<u32>,
    /// Copy a temporary link valid this many seconds (one of
    /// `TEMPORARY_LINK_SECONDS`) after each upload instead of the public
    /// URL; none copies the public URL.
    #[serde(default)]
    pub temporary_link: Option<u64>,
    /// What to strip from photos before they're uploaded here; none is
    /// `ImageMetadataPolicy::default()` (remove the location).
    #[serde(default)]
    pub image_metadata: Option<ImageMetadataPolicy>,
    /// How folders are uploaded here; none is `FolderUploadMode::default()`
    /// (as a ZIP).
    #[serde(default)]
    pub folder_upload: Option<FolderUploadMode>,
}

/// How long a temporary (presigned) link can stay valid, in seconds: 5 and
/// 15 minutes, 1 hour, 1 day and 7 days, the longest S3 allows. The minute
/// long ones are for confidential files: S3 can't count downloads, so a
/// link can't be single-use, but one that dies minutes after it's sent is
/// close.
pub const TEMPORARY_LINK_SECONDS: [u64; 5] = [300, 900, 3600, 86_400, 604_800];

/// What happens to a photo's metadata before it's uploaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImageMetadataPolicy {
    /// Drops the GPS location and keeps everything else (camera, date,
    /// orientation, color profile), so a shared photo never gives away
    /// where it was taken.
    #[default]
    RemoveLocation,
    /// Drops EXIF, GPS, IPTC and XMP. Orientation and the color profile
    /// stay, so the image still looks the same.
    RemoveAll,
    KeepAll,
}

/// How a folder is uploaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FolderUploadMode {
    /// One .zip file, and one link to it.
    #[default]
    Zip,
    /// Every file under one new folder in the bucket, with the same
    /// subfolders, and all the links copied at the end.
    KeepStructure,
}

impl DestinationConfig {
    pub fn image_metadata(&self) -> ImageMetadataPolicy {
        self.image_metadata.unwrap_or_default()
    }

    pub fn folder_upload(&self) -> FolderUploadMode {
        self.folder_upload.unwrap_or_default()
    }

    /// Settings that can't be right (edited by hand, or from a newer
    /// version) are dropped, so they fall back to the defaults.
    fn sanitize(&mut self) {
        if self.expiry_days.is_some_and(|days| days != 0 && !crate::expiry::is_valid(days)) {
            self.expiry_days = None;
        }
        if self.temporary_link.is_some_and(|seconds| !TEMPORARY_LINK_SECONDS.contains(&seconds)) {
            self.temporary_link = None;
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Wrapper {
    destinations: Vec<DestinationConfig>,
    #[serde(rename = "defaultID")]
    default_id: Option<String>,
}

pub struct DestinationStore {
    path: PathBuf,
    inner: Mutex<Wrapper>,
}

impl DestinationStore {
    pub fn load(directory: &Path) -> Self {
        let path = directory.join("destinations.json");
        let mut wrapper: Wrapper = std::fs::read(&path)
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default();
        wrapper.destinations.iter_mut().for_each(DestinationConfig::sanitize);
        sync_default_flags(&mut wrapper);
        Self { path, inner: Mutex::new(wrapper) }
    }

    pub fn all(&self) -> Vec<DestinationConfig> {
        self.inner.lock().unwrap().destinations.clone()
    }

    pub fn default_destination(&self) -> Option<DestinationConfig> {
        let inner = self.inner.lock().unwrap();
        inner
            .destinations
            .iter()
            .find(|destination| Some(&destination.id) == inner.default_id.as_ref())
            .or_else(|| inner.destinations.first())
            .cloned()
    }

    /// Looks a destination up by ID (case-insensitively, since callers of
    /// the local API may lowercase it), or the default one for no ID.
    pub fn find(&self, id: Option<&str>) -> Option<DestinationConfig> {
        match id.map(str::trim).filter(|id| !id.is_empty()) {
            None => self.default_destination(),
            Some(id) => self
                .inner
                .lock()
                .unwrap()
                .destinations
                .iter()
                .find(|destination| destination.id.eq_ignore_ascii_case(id))
                .cloned(),
        }
    }

    pub fn add(&self, mut destination: DestinationConfig) {
        destination.sanitize();
        self.mutate(|wrapper| {
            if wrapper.destinations.is_empty() {
                destination.is_default = true;
                wrapper.default_id = Some(destination.id.clone());
            }
            wrapper.destinations.push(destination);
        });
    }

    pub fn update(&self, mut destination: DestinationConfig) {
        destination.sanitize();
        self.mutate(|wrapper| {
            if let Some(existing) = wrapper.destinations.iter_mut().find(|d| d.id == destination.id) {
                *existing = destination;
            }
        });
    }

    pub fn remove(&self, id: &str) {
        self.mutate(|wrapper| {
            wrapper.destinations.retain(|destination| destination.id != id);
            if wrapper.default_id.as_deref() == Some(id) {
                wrapper.default_id = wrapper.destinations.first().map(|d| d.id.clone());
            }
        });
        let _ = crate::credentials::delete(id);
    }

    /// Changes one destination in place, e.g. the panel's "Delete after"
    /// or "Link" choice for it.
    pub fn modify(&self, id: &str, change: impl FnOnce(&mut DestinationConfig)) -> Option<DestinationConfig> {
        let mut changed = None;
        self.mutate(|wrapper| {
            if let Some(destination) = wrapper.destinations.iter_mut().find(|d| d.id == id) {
                change(destination);
                destination.sanitize();
                changed = Some(destination.clone());
            }
        });
        changed
    }

    pub fn set_default(&self, id: &str) {
        self.mutate(|wrapper| wrapper.default_id = Some(id.to_string()));
    }

    fn mutate(&self, change: impl FnOnce(&mut Wrapper)) {
        let mut inner = self.inner.lock().unwrap();
        change(&mut inner);
        sync_default_flags(&mut inner);
        crate::util::write_json_atomically(&self.path, &*inner);
    }
}

/// `default_id` is what uploads use, so it wins; the per-destination flags
/// (which Settings shows) only decide when it points nowhere.
fn sync_default_flags(wrapper: &mut Wrapper) {
    let points_somewhere = wrapper
        .destinations
        .iter()
        .any(|destination| Some(&destination.id) == wrapper.default_id.as_ref());
    if !points_somewhere {
        wrapper.default_id = wrapper
            .destinations
            .iter()
            .find(|destination| destination.is_default)
            .or_else(|| wrapper.destinations.first())
            .map(|destination| destination.id.clone());
    }
    for destination in &mut wrapper.destinations {
        destination.is_default = Some(&destination.id) == wrapper.default_id.as_ref();
    }
}
