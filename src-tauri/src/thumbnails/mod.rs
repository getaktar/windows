//! Thumbnails of uploads, made the way Explorer makes them, kept on this PC
//! and, if the destination says so, in the bucket. The Mac, iOS and Android
//! apps use the same setting, key layout and format; see
//! docs/thumbnails.md in the Mac repository.

pub mod bucket;
mod generate;
pub mod remote;
mod store;

use serde::{Deserialize, Deserializer, Serialize};

use crate::destinations::DestinationConfig;
use crate::t;

pub use generate::{can_have_thumbnail, generate, is_webp, MAX_PIXEL_SIZE};
pub use store::{disk_usage, LocalStore};

/// Where a destination's thumbnails live.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ThumbnailMode {
    /// Nothing is made, downloaded or shown; rows show file icons.
    Off,
    /// Made on this PC and kept only here.
    #[default]
    Local,
    /// Kept here too, and also saved to the bucket under a folder of the
    /// user's choosing, so other devices (and a reinstall) can show them.
    Bucket,
}

/// A `thumbnails` value from destinations.json or a transfer link: one this
/// version doesn't know is left unset (the default) rather than failing.
pub fn lenient_mode<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<ThumbnailMode>, D::Error> {
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.filter(serde_json::Value::is_string).and_then(|value| serde_json::from_value(value).ok()))
}

pub const DEFAULT_PREFIX: &str = ".aktar/thumbnails/";
pub const FILE_EXTENSION: &str = "webp";
pub const CONTENT_TYPE: &str = "image/webp";

impl DestinationConfig {
    /// The mode that applies: a destination saved before thumbnails had a
    /// setting keeps them on this PC, as before.
    pub fn thumbnail_mode(&self) -> ThumbnailMode {
        self.thumbnails.unwrap_or_default()
    }

    /// The folder thumbnails are saved under in the bucket, when this
    /// destination saves them there.
    pub fn bucket_thumbnail_prefix(&self) -> Option<String> {
        (self.thumbnail_mode() == ThumbnailMode::Bucket)
            .then(|| self.thumbnail_prefix.as_deref().and_then(normalized_prefix).unwrap_or_else(|| DEFAULT_PREFIX.to_string()))
    }
}

/// `raw` as a folder: trimmed, without leading slashes, ending in one.
/// None when nothing is left.
pub fn normalized_prefix(raw: &str) -> Option<String> {
    let prefix = raw.trim().trim_start_matches('/').trim_end_matches('/');
    (!prefix.is_empty()).then(|| format!("{prefix}/"))
}

/// Why `raw` can't be the thumbnail folder, or None when it can.
pub fn problem_with_prefix(raw: &str) -> Option<String> {
    let Some(prefix) = normalized_prefix(raw) else {
        return Some(t!("Enter a folder for the thumbnails."));
    };
    if let Err(problem) = crate::bucket::check_key(&prefix) {
        return Some(problem);
    }
    if prefix.starts_with(crate::expiry::PREFIX_ROOT) {
        return Some(t!("The thumbnail folder can’t be inside tmp/, where files expire."));
    }
    None
}

/// Where the thumbnail of the file at `object_key` goes. A thumbnail's key
/// mirrors its file's key under the thumbnail folder, so it's found without
/// a list or a manifest, and an expiring file's thumbnail stays inside the
/// same `tmp/{N}d/` folder so the bucket's lifecycle rule deletes both
/// together, whether or not Aktar is running:
///
/// ```text
/// photos/cat.png        -> .aktar/thumbnails/photos/cat.png.webp
/// tmp/7d/photos/cat.png -> tmp/7d/.aktar/thumbnails/photos/cat.png.webp
/// ```
///
/// None when that "file" is a folder or a thumbnail itself.
pub fn key_for(object_key: &str, prefix: &str) -> Option<String> {
    if object_key.is_empty() || object_key.ends_with('/') || is_thumbnail(object_key, &[prefix.to_string()]) {
        return None;
    }
    if let Some(days) = crate::expiry::days_in_key(object_key) {
        let expiring = crate::expiry::prefix(days);
        return Some(format!("{expiring}{prefix}{}.{FILE_EXTENSION}", &object_key[expiring.len()..]));
    }
    Some(format!("{prefix}{object_key}.{FILE_EXTENSION}"))
}

/// The folders a prefix's thumbnails are in: at the root, and inside each
/// `tmp/{N}d/` folder for expiring files.
pub fn roots(prefix: &str) -> Vec<String> {
    std::iter::once(prefix.to_string())
        .chain(crate::expiry::DURATIONS.iter().map(|&days| format!("{}{prefix}", crate::expiry::prefix(days))))
        .collect()
}

pub fn is_thumbnail(key: &str, prefixes: &[String]) -> bool {
    prefixes.iter().flat_map(|prefix| roots(prefix)).any(|root| key.starts_with(&root))
}

/// Whether the bucket view leaves `folder` out: a thumbnail folder or
/// anything in it, and a dot folder (such as `.aktar/`) on the way to one,
/// the way dot folders are usually hidden.
pub fn is_hidden_folder(folder: &str, prefixes: &[String]) -> bool {
    let dot = crate::bucket::folder_display_name(folder).starts_with('.');
    prefixes
        .iter()
        .flat_map(|prefix| roots(prefix))
        .any(|root| folder.starts_with(&root) || (dot && root.starts_with(folder)))
}

/// The thumbnail folders in the bucket `destination` points at: its own
/// when it saves thumbnails there, and those of other destinations on the
/// same bucket that do (profiles sharing one bucket). Deleting or moving a
/// file takes its thumbnail in each along.
pub fn bucket_prefixes(destination: &DestinationConfig, all: &[DestinationConfig]) -> Vec<String> {
    let mut prefixes: Vec<String> = Vec::new();
    for candidate in std::iter::once(destination).chain(all) {
        if candidate.id != destination.id && !same_bucket(candidate, destination) {
            continue;
        }
        if let Some(prefix) = candidate.bucket_thumbnail_prefix() {
            if !prefixes.contains(&prefix) {
                prefixes.push(prefix);
            }
        }
    }
    prefixes
}

/// Whether two destinations upload to the same bucket, however their
/// endpoints are written.
pub fn same_bucket(a: &DestinationConfig, b: &DestinationConfig) -> bool {
    fn endpoint(raw: &str) -> String {
        let lower = raw.trim().to_lowercase();
        let bare = lower.strip_prefix("https://").or_else(|| lower.strip_prefix("http://")).unwrap_or(&lower);
        bare.trim_end_matches('/').to_string()
    }
    endpoint(&a.endpoint) == endpoint(&b.endpoint) && a.bucket.trim() == b.bucket.trim()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::destinations::ProviderPreset;

    fn destination(id: &str, bucket: &str) -> DestinationConfig {
        DestinationConfig {
            id: id.into(),
            name: bucket.into(),
            preset: ProviderPreset::CustomS3,
            account_id: None,
            endpoint: "https://s3.example.com".into(),
            region: "auto".into(),
            bucket: bucket.into(),
            public_base_url: "https://files.example.com".into(),
            object_path_template: "{filename}".into(),
            force_path_style: true,
            is_default: false,
            output_mode: None,
            expiry_days: None,
            temporary_link: None,
            image_metadata: None,
            folder_upload: None,
            image_processing: None,
            thumbnails: None,
            thumbnail_prefix: None,
        }
    }

    #[test]
    fn mirrors_the_file() {
        assert_eq!(key_for("photos/cat.png", DEFAULT_PREFIX).as_deref(), Some(".aktar/thumbnails/photos/cat.png.webp"));
        assert_eq!(key_for("cat", "thumbs/").as_deref(), Some("thumbs/cat.webp"));
    }

    #[test]
    fn keeps_expiring_thumbnails_in_the_same_folder() {
        assert_eq!(
            key_for("tmp/7d/photos/cat.png", DEFAULT_PREFIX).as_deref(),
            Some("tmp/7d/.aktar/thumbnails/photos/cat.png.webp")
        );
        assert_eq!(key_for("tmp/1d/a.mov", DEFAULT_PREFIX).as_deref(), Some("tmp/1d/.aktar/thumbnails/a.mov.webp"));
        // Not an expiring folder Aktar uses.
        assert_eq!(key_for("tmp/2d/a.mov", DEFAULT_PREFIX).as_deref(), Some(".aktar/thumbnails/tmp/2d/a.mov.webp"));
    }

    #[test]
    fn no_thumbnail_of_a_folder_or_a_thumbnail() {
        assert_eq!(key_for("photos/", DEFAULT_PREFIX), None);
        assert_eq!(key_for("", DEFAULT_PREFIX), None);
        assert_eq!(key_for(".aktar/thumbnails/a.png.webp", DEFAULT_PREFIX), None);
        assert_eq!(key_for("tmp/7d/.aktar/thumbnails/a.png.webp", DEFAULT_PREFIX), None);
    }

    #[test]
    fn hides_thumbnail_folders() {
        let prefixes = [DEFAULT_PREFIX.to_string()];
        assert!(is_thumbnail("tmp/30d/.aktar/thumbnails/a.png.webp", &prefixes));
        assert!(!is_thumbnail(".aktar/other/a.png", &prefixes));
        assert!(is_hidden_folder(".aktar/", &prefixes));
        assert!(is_hidden_folder(".aktar/thumbnails/photos/", &prefixes));
        assert!(is_hidden_folder("tmp/7d/.aktar/", &prefixes));
        assert!(!is_hidden_folder("tmp/", &prefixes));
        assert!(!is_hidden_folder("tmp/7d/", &prefixes));
        assert!(!is_hidden_folder("media/", &["media/thumbs/".to_string()]));
        assert!(is_hidden_folder("media/thumbs/", &["media/thumbs/".to_string()]));
        assert!(!is_hidden_folder(".aktar/", &[]));
    }

    #[test]
    fn checks_prefixes() {
        assert_eq!(normalized_prefix(" /previews// ").as_deref(), Some("previews/"));
        assert_eq!(normalized_prefix(" / "), None);
        assert!(problem_with_prefix("previews").is_none());
        assert!(problem_with_prefix(DEFAULT_PREFIX).is_none());
        for bad in ["", "tmp/thumbs", "a/../b", "a//b"] {
            assert!(problem_with_prefix(bad).is_some(), "{bad}");
        }
    }

    #[test]
    fn finds_the_prefixes_of_profiles_on_the_bucket() {
        let mut local = destination("A", "files");
        assert_eq!(local.thumbnail_mode(), ThumbnailMode::Local);
        assert_eq!(local.bucket_thumbnail_prefix(), None);
        let mut same = destination("B", "files");
        same.thumbnails = Some(ThumbnailMode::Bucket);
        same.thumbnail_prefix = Some("previews/".into());
        let mut other = destination("C", "other");
        other.thumbnails = Some(ThumbnailMode::Bucket);
        let all = vec![local.clone(), same, other.clone()];
        assert_eq!(bucket_prefixes(&local, &all), vec!["previews/".to_string()]);
        assert_eq!(bucket_prefixes(&other, &all), vec![DEFAULT_PREFIX.to_string()]);
        local.thumbnails = Some(ThumbnailMode::Off);
        assert!(bucket_prefixes(&local, std::slice::from_ref(&local)).is_empty());
    }

    #[test]
    fn reads_unknown_modes_as_unset() {
        let json = r#"{"id":"A","name":"N","preset":"minIO","endpoint":"e","region":"r","bucket":"b","publicBaseURL":"p","objectPathTemplate":"{uuid}.{ext}","forcePathStyle":true,"thumbnails":"cloud"}"#;
        let decoded: DestinationConfig = serde_json::from_str(json).unwrap();
        assert_eq!(decoded.thumbnails, None);
        let json = json.replace("cloud", "bucket");
        let decoded: DestinationConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.thumbnails, Some(ThumbnailMode::Bucket));
    }
}
