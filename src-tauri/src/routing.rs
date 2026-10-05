//! "Use for": which destination an upload goes to when nothing names one
//! (the clipboard shortcut, a drop on the panel, Explorer's menu, the local
//! API without destinationId). Each destination can claim kinds of files
//! and extensions; a file goes to the destination that claims its
//! extension, else its kind, else the default destination. The kind lists
//! are shared with the Mac and mobile apps (see the Mac repo's
//! docs/destination-automation.md), so a file routes the same way on every
//! device.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::destinations::DestinationConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FileKind {
    Image,
    Video,
    Audio,
    Document,
    Archive,
}

pub const KINDS: [FileKind; 5] = [FileKind::Image, FileKind::Video, FileKind::Audio, FileKind::Document, FileKind::Archive];

const IMAGE: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "avif", "heic", "heif", "tif", "tiff", "bmp", "svg", "ico", "cr2", "cr3", "nef", "arw", "dng",
    "orf", "rw2", "raf",
];
const VIDEO: &[&str] = &["mp4", "mov", "m4v", "avi", "mkv", "webm", "wmv", "flv", "3gp", "mpg", "mpeg"];
const AUDIO: &[&str] = &["mp3", "m4a", "aac", "wav", "flac", "ogg", "opus", "aif", "aiff", "wma"];
const DOCUMENT: &[&str] = &[
    "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "key", "pages", "numbers", "odt", "ods", "odp", "rtf", "txt", "md", "csv", "json",
    "epub",
];
const ARCHIVE: &[&str] = &["zip", "rar", "7z", "tar", "gz", "tgz", "bz2", "xz", "dmg", "iso", "pkg"];

impl FileKind {
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            FileKind::Image => IMAGE,
            FileKind::Video => VIDEO,
            FileKind::Audio => AUDIO,
            FileKind::Document => DOCUMENT,
            FileKind::Archive => ARCHIVE,
        }
    }

    /// The kind of a (lowercase, dotless) extension, if it has one.
    pub fn of(extension: &str) -> Option<FileKind> {
        KINDS.into_iter().find(|kind| kind.extensions().contains(&extension))
    }
}

/// A destination's "Use for". Unknown kinds and extensions that can't be
/// one are dropped when it's read, never failing the whole destination.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileRouting {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<FileKind>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
}

impl FileRouting {
    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty() && self.extensions.is_empty()
    }

    /// Duplicates out, extensions normalized, in a stable order.
    pub fn sanitized(self) -> Option<FileRouting> {
        let mut kinds = Vec::new();
        for kind in KINDS {
            if self.kinds.contains(&kind) {
                kinds.push(kind);
            }
        }
        let mut extensions: Vec<String> = Vec::new();
        for extension in self.extensions.iter().filter_map(|raw| normalized_extension(raw)) {
            if !extensions.contains(&extension) {
                extensions.push(extension);
            }
        }
        let routing = FileRouting { kinds, extensions };
        (!routing.is_empty()).then_some(routing)
    }

    /// Read leniently from JSON: anything that isn't a known kind or a
    /// usable extension is left out.
    pub fn from_value(value: &Value) -> Option<FileRouting> {
        let object = value.as_object()?;
        let list = |name: &str| object.get(name).and_then(Value::as_array).cloned().unwrap_or_default();
        let kinds = list("kinds").into_iter().filter_map(|kind| serde_json::from_value::<FileKind>(kind).ok()).collect();
        let extensions = list("extensions").into_iter().filter_map(|extension| extension.as_str().map(str::to_string)).collect();
        FileRouting { kinds, extensions }.sanitized()
    }
}

impl<'de> Deserialize<'de> for FileRouting {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Ok(FileRouting::from_value(&value).unwrap_or_default())
    }
}

/// For `#[serde(deserialize_with)]`: an empty or unreadable "Use for" is
/// none at all.
pub fn lenient<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<FileRouting>, D::Error> {
    let value = Value::deserialize(deserializer)?;
    Ok(FileRouting::from_value(&value))
}

/// An extension as "Use for" keeps it: trimmed, without leading dots,
/// lowercase, 1 to 16 of a-z and 0-9. None when it can't be one.
pub fn normalized_extension(raw: &str) -> Option<String> {
    let extension = raw.trim().trim_start_matches('.').to_ascii_lowercase();
    let valid = (1..=16).contains(&extension.len()) && extension.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
    valid.then_some(extension)
}

/// The lowercase extension of a file name, without the dot.
fn extension_of(filename: &str) -> Option<String> {
    let name = crate::util::last_component(filename);
    let (stem, extension) = name.rsplit_once('.')?;
    (!stem.is_empty()).then(|| extension.to_ascii_lowercase()).filter(|extension| !extension.is_empty())
}

/// Where a file called `filename` goes when nothing names a destination:
/// a destination claiming its extension, else one claiming its kind, else
/// none (the default destination). Among several at the same step, the
/// default destination wins when it's one of them, otherwise the first.
pub fn route<'a>(destinations: &'a [DestinationConfig], default_id: Option<&str>, filename: &str) -> Option<&'a DestinationConfig> {
    let extension = extension_of(filename)?;
    let pick = |matches: Vec<&'a DestinationConfig>| -> Option<&'a DestinationConfig> {
        matches.iter().copied().find(|destination| Some(destination.id.as_str()) == default_id).or_else(|| matches.first().copied())
    };
    let claiming_extension: Vec<&DestinationConfig> = destinations
        .iter()
        .filter(|destination| destination.use_for.as_ref().is_some_and(|routing| routing.extensions.contains(&extension)))
        .collect();
    if let Some(destination) = pick(claiming_extension) {
        return Some(destination);
    }
    let kind = FileKind::of(&extension)?;
    let claiming_kind: Vec<&DestinationConfig> = destinations
        .iter()
        .filter(|destination| destination.use_for.as_ref().is_some_and(|routing| routing.kinds.contains(&kind)))
        .collect();
    pick(claiming_kind)
}

/// Whether any destination has a "Use for", so uploads need routing at all.
pub fn is_active(destinations: &[DestinationConfig]) -> bool {
    destinations.iter().any(|destination| destination.use_for.as_ref().is_some_and(|routing| !routing.is_empty()))
}

/// One line per destination that claims files ("Images, Videos, .dmg go to
/// Screenshots"), for under the panel's destination picker. The default
/// destination is left out: what it claims goes there anyway.
pub fn hints(destinations: &[DestinationConfig], default_id: Option<&str>) -> Vec<String> {
    destinations
        .iter()
        .filter(|destination| Some(destination.id.as_str()) != default_id)
        .filter_map(|destination| {
            let routing = destination.use_for.as_ref().filter(|routing| !routing.is_empty())?;
            let mut parts: Vec<String> = routing.kinds.iter().map(|kind| kind_label(*kind)).collect();
            parts.extend(routing.extensions.iter().map(|extension| format!(".{extension}")));
            Some(crate::t!("{0} go to {1}.", parts.join(", "), destination.name))
        })
        .collect()
}

pub fn kind_label(kind: FileKind) -> String {
    match kind {
        FileKind::Image => crate::t!("Images"),
        FileKind::Video => crate::t!("Videos"),
        FileKind::Audio => crate::t!("Audio"),
        FileKind::Document => crate::t!("Documents"),
        FileKind::Archive => crate::t!("Archives"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn destination(id: &str, kinds: &[FileKind], extensions: &[&str]) -> DestinationConfig {
        let mut destination: DestinationConfig = serde_json::from_value(serde_json::json!({
            "id": id, "name": id, "preset": "minIO", "endpoint": "e", "region": "r", "bucket": "b",
            "publicBaseURL": "https://p", "objectPathTemplate": "{uuid}.{ext}", "forcePathStyle": true,
        }))
        .unwrap();
        destination.use_for = FileRouting { kinds: kinds.to_vec(), extensions: extensions.iter().map(|e| e.to_string()).collect() }.sanitized();
        destination
    }

    #[test]
    fn kinds_match_the_shared_lists() {
        assert_eq!(FileKind::of("png"), Some(FileKind::Image));
        assert_eq!(FileKind::of("cr3"), Some(FileKind::Image));
        assert_eq!(FileKind::of("mov"), Some(FileKind::Video));
        assert_eq!(FileKind::of("opus"), Some(FileKind::Audio));
        assert_eq!(FileKind::of("key"), Some(FileKind::Document));
        assert_eq!(FileKind::of("json"), Some(FileKind::Document));
        assert_eq!(FileKind::of("dmg"), Some(FileKind::Archive));
        assert_eq!(FileKind::of("exe"), None);
        // Every extension belongs to one kind only.
        for kind in KINDS {
            for extension in kind.extensions() {
                assert_eq!(FileKind::of(extension), Some(kind), "{extension}");
            }
        }
    }

    #[test]
    fn extensions_win_over_kinds_and_default_wins_ties() {
        let screenshots = destination("S", &[FileKind::Image, FileKind::Video], &[]);
        let builds = destination("B", &[], &["dmg", "png"]);
        let media = destination("M", &[FileKind::Image], &[]);
        let plain = destination("P", &[], &[]);
        let all = [screenshots.clone(), builds.clone(), media.clone(), plain.clone()];

        // An extension claim beats a kind claim, wherever it is in the list.
        assert_eq!(route(&all, None, "a.png").map(|d| d.id.as_str()), Some("B"));
        assert_eq!(route(&all, None, "build.DMG").map(|d| d.id.as_str()), Some("B"));
        // Several claim the kind: the first, unless the default is one.
        assert_eq!(route(&all, None, "photo.jpg").map(|d| d.id.as_str()), Some("S"));
        assert_eq!(route(&all, Some("M"), "photo.jpg").map(|d| d.id.as_str()), Some("M"));
        assert_eq!(route(&all, Some("P"), "photo.jpg").map(|d| d.id.as_str()), Some("S"));
        // Nothing claims it, or it has no extension: the default.
        assert_eq!(route(&all, None, "notes.txt"), None);
        assert_eq!(route(&all, None, "README"), None);
        assert_eq!(route(&all, None, ".png"), None);
        assert_eq!(route(&all, None, "C:\\shots\\a.mov").map(|d| d.id.as_str()), Some("S"));
        assert!(is_active(&all));
        assert!(!is_active(&[plain]));
    }

    #[test]
    fn validates_extensions() {
        assert_eq!(normalized_extension(" .DMG "), Some("dmg".into()));
        assert_eq!(normalized_extension("..tar"), Some("tar".into()));
        assert_eq!(normalized_extension("mp4"), Some("mp4".into()));
        assert_eq!(normalized_extension(""), None);
        assert_eq!(normalized_extension("tar.gz"), None);
        assert_eq!(normalized_extension("a-b"), None);
        assert_eq!(normalized_extension("ünï"), None);
        assert_eq!(normalized_extension(&"a".repeat(17)), None);
    }

    #[test]
    fn reads_leniently() {
        let routing: FileRouting =
            serde_json::from_str(r#"{"kinds":["image","sticker",3,"video","image"],"extensions":["DMG","tar.gz",7,".zip"]}"#).unwrap();
        assert_eq!(routing, FileRouting { kinds: vec![FileKind::Image, FileKind::Video], extensions: vec!["dmg".into(), "zip".into()] });
        assert_eq!(FileRouting::from_value(&serde_json::json!({"kinds": ["nope"]})), None);
        assert_eq!(FileRouting::from_value(&serde_json::json!("image")), None);
        let json = serde_json::to_string(&FileRouting { kinds: vec![FileKind::Archive], extensions: vec![] }).unwrap();
        assert_eq!(json, r#"{"kinds":["archive"]}"#);
    }
}
