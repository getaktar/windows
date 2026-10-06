//! Non-secret destination configuration, persisted as JSON. Secrets never
//! live here, see `credentials`.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::output::OutputMode;
use crate::routing::FileRouting;
use crate::short_links::definition::ShortLinkSettings;
use crate::thumbnails::ThumbnailMode;
use crate::watched::model::Hook;
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
    /// Whether uploads can be made conditional (If-None-Match: *), so a
    /// file never lands on one that appeared at its key in the meantime.
    pub fn supports_conditional_writes(self) -> bool {
        matches!(self, ProviderPreset::AmazonS3 | ProviderPreset::CloudflareR2)
    }

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
    /// Converting, recompressing and resizing photos before they're
    /// uploaded here; none leaves them as they are.
    #[serde(default)]
    pub image_processing: Option<ImageProcessing>,
    /// Where thumbnails of uploads here are kept; none is
    /// `ThumbnailMode::default()` (on this PC). See `thumbnail_mode`.
    #[serde(default, deserialize_with = "crate::thumbnails::lenient_mode")]
    pub thumbnails: Option<ThumbnailMode>,
    /// The bucket folder for `ThumbnailMode::Bucket`; none is
    /// `thumbnails::DEFAULT_PREFIX`. See `bucket_thumbnail_prefix`.
    #[serde(default)]
    pub thumbnail_prefix: Option<String>,
    /// The kinds of files and extensions that come here when an upload
    /// doesn't name a destination; see `routing`.
    #[serde(default, deserialize_with = "crate::routing::lenient")]
    pub use_for: Option<FileRouting>,
    /// Uploads here are sent with a one-minute cache time, so a replaced
    /// file shows up everywhere within about a minute.
    #[serde(default)]
    pub short_cache: Option<bool>,
    /// The Cloudflare zone whose cache is cleared for a replaced file; the
    /// token is in Credential Manager with the keys (`StorageCredentials`).
    #[serde(default)]
    pub cloudflare_zone_id: Option<String>,
    /// Run after each upload and replace here (not for a watched folder's
    /// files, which run their folder's own).
    #[serde(default)]
    pub hooks: Option<Vec<Hook>>,
    /// The link shortener uploads here go through; none is off. Its token
    /// is in Credential Manager with the keys (`StorageCredentials`).
    /// Settings this version can't use (an unknown provider) read as off.
    #[serde(default, deserialize_with = "crate::short_links::definition::lenient")]
    pub short_links: Option<ShortLinkSettings>,
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

/// What happens to a photo's pixels before it's uploaded (the destination's
/// "Image Processing" settings), see `image_processing`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageProcessing {
    #[serde(default)]
    pub format: ImageFormat,
    /// Lossy quality, one of `IMAGE_QUALITIES`; none doesn't recompress.
    #[serde(default)]
    pub quality: Option<u8>,
    /// The longest side photos are scaled down to, one of `IMAGE_SIZES`;
    /// none keeps their size.
    #[serde(default)]
    pub max_long_edge: Option<u32>,
}

/// The format photos are uploaded in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImageFormat {
    #[default]
    Original,
    Webp,
    Avif,
}

/// "Light", "Medium" and "Strong" compression.
pub const IMAGE_QUALITIES: [u8; 3] = [90, 80, 65];
pub const IMAGE_SIZES: [u32; 5] = [3840, 2560, 1920, 1280, 1024];

impl ImageProcessing {
    /// Whether it changes anything at all.
    pub fn is_active(&self) -> bool {
        self.format != ImageFormat::Original || self.quality.is_some() || self.max_long_edge.is_some()
    }
}

impl DestinationConfig {
    pub fn image_metadata(&self) -> ImageMetadataPolicy {
        self.image_metadata.unwrap_or_default()
    }

    pub fn folder_upload(&self) -> FolderUploadMode {
        self.folder_upload.unwrap_or_default()
    }

    pub fn short_cache(&self) -> bool {
        self.short_cache.unwrap_or(false)
    }

    /// The hooks that run after an upload or replace here.
    pub fn enabled_hooks(&self) -> Vec<Hook> {
        self.hooks.iter().flatten().filter(|hook| hook.enabled).cloned().collect()
    }

    /// The image processing to apply, or none when it's all off.
    pub fn image_processing(&self) -> Option<ImageProcessing> {
        self.image_processing.filter(ImageProcessing::is_active)
    }

    /// Settings that can't be right (edited by hand, or from a newer
    /// version) are dropped, so they fall back to the defaults.
    pub(crate) fn sanitize(&mut self) {
        if self.expiry_days.is_some_and(|days| days != 0 && !crate::expiry::is_valid(days)) {
            self.expiry_days = None;
        }
        if self.temporary_link.is_some_and(|seconds| !TEMPORARY_LINK_SECONDS.contains(&seconds)) {
            self.temporary_link = None;
        }
        if let Some(processing) = &mut self.image_processing {
            if processing.quality.is_some_and(|quality| !IMAGE_QUALITIES.contains(&quality)) {
                processing.quality = None;
            }
            if processing.max_long_edge.is_some_and(|size| !IMAGE_SIZES.contains(&size)) {
                processing.max_long_edge = None;
            }
            if !processing.is_active() {
                self.image_processing = None;
            }
        }
        self.thumbnail_prefix = self
            .thumbnail_prefix
            .take()
            .filter(|prefix| crate::thumbnails::problem_with_prefix(prefix).is_none())
            .and_then(|prefix| crate::thumbnails::normalized_prefix(&prefix));
        self.use_for = self.use_for.take().and_then(FileRouting::sanitized);
        self.cloudflare_zone_id = self.cloudflare_zone_id.take().and_then(|zone| normalized_zone_id(&zone));
        if self.short_cache == Some(false) {
            self.short_cache = None;
        }
        if self.hooks.as_ref().is_some_and(Vec::is_empty) {
            self.hooks = None;
        }
        if self.short_links.as_ref().is_some_and(|settings| settings.definition().is_none()) {
            self.short_links = None;
        }
    }
}

/// A Cloudflare zone ID: 32 hex digits, as the dashboard shows it. None
/// when it can't be one.
pub fn normalized_zone_id(raw: &str) -> Option<String> {
    let zone = raw.trim().to_ascii_lowercase();
    (zone.len() == 32 && zone.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(zone)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_image_processing() {
        let old = r#"{"id":"A","name":"N","preset":"minIO","endpoint":"e","region":"r","bucket":"b","publicBaseURL":"p","objectPathTemplate":"{uuid}.{ext}","forcePathStyle":true}"#;
        let decoded: DestinationConfig = serde_json::from_str(old).unwrap();
        assert_eq!(decoded.image_processing, None);
        assert_eq!(decoded.image_processing(), None);

        let mut config = decoded.clone();
        config.image_processing = Some(ImageProcessing { format: ImageFormat::Webp, quality: Some(80), max_long_edge: Some(1920) });
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains(r#""imageProcessing":{"format":"webp","quality":80,"maxLongEdge":1920}"#), "{json}");
        let round_trip: DestinationConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(round_trip, config);

        // Values no version offers are dropped, and nothing left is off.
        config.image_processing = Some(ImageProcessing { format: ImageFormat::Original, quality: Some(42), max_long_edge: Some(999) });
        config.sanitize();
        assert_eq!(config.image_processing, None);
        let partial: ImageProcessing = serde_json::from_str(r#"{"format":"avif"}"#).unwrap();
        assert_eq!(partial, ImageProcessing { format: ImageFormat::Avif, quality: None, max_long_edge: None });
    }
}
