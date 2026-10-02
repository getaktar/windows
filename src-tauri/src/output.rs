//! What gets copied after an upload, how object keys are generated from a
//! destination's path template, and how public URLs are built. Ported from
//! the Mac app's OutputFormatter, ObjectKeyGenerator, and PublicURLResolver
//! so both apps produce identical keys and links.

use chrono::{DateTime, Datelike, Local};
use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS, NON_ALPHANUMERIC};
use serde::{Deserialize, Serialize};

use crate::util::split_extension;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum OutputMode {
    #[default]
    Url,
    Markdown,
    Html,
    Custom,
}

impl OutputMode {
    pub fn raw_value(self) -> &'static str {
        match self {
            OutputMode::Url => "url",
            OutputMode::Markdown => "markdown",
            OutputMode::Html => "html",
            OutputMode::Custom => "custom",
        }
    }
}

pub fn format(public_url: &str, mode: OutputMode, filename: &str, custom_template: &str) -> String {
    match mode {
        OutputMode::Url => public_url.to_string(),
        OutputMode::Markdown if is_image(filename) => format!("![]({public_url})"),
        OutputMode::Markdown => format!("[{filename}]({public_url})"),
        OutputMode::Html if is_image(filename) => format!("<img src=\"{public_url}\" alt=\"\">"),
        OutputMode::Html => format!("<a href=\"{public_url}\">{filename}</a>"),
        OutputMode::Custom => {
            let (name, ext) = split_extension(filename);
            custom_template
                .replace("{url}", public_url)
                .replace("{filename}", filename)
                .replace("{name}", name)
                .replace("{ext}", ext)
        }
    }
}

/// Image files get embed markup (`![]()`, `<img>`); anything else gets a
/// plain link, since embedding a PDF or zip as an image would just render
/// as a broken icon wherever it's pasted.
pub fn is_image(filename: &str) -> bool {
    content_type(filename).starts_with("image/")
}

pub fn content_type(filename: &str) -> String {
    let (_, ext) = split_extension(filename);
    if ext.is_empty() {
        return "application/octet-stream".into();
    }
    mime_guess::from_ext(ext)
        .first_raw()
        .unwrap_or("application/octet-stream")
        .to_string()
}

/// Lowercase hex digests of the bytes uploaded, for `{md5}` and `{sha256}`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ContentHashes {
    pub md5: String,
    pub sha256: String,
}

/// Whether the template needs the file's contents hashed.
pub fn uses_hashes(template: &str) -> bool {
    template.contains("{md5}") || template.contains("{sha256}")
}

/// Where a file came from, for the `{folder}` and `{subpath}` tokens: a
/// watched folder's name, and the file's folder inside it ("" at its
/// root). Both are "" for every other upload.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyPlace {
    pub folder: String,
    pub subpath: String,
    /// "Include, keep folder structure": the subpath goes in before the
    /// key's last component when the template doesn't place it itself.
    pub keep_structure: bool,
}

/// A key from the template. `hashes` fill `{md5}` and `{sha256}`; without
/// them (a folder's prefix) those come out empty.
pub fn generate_key(template: &str, original_filename: &str, hashes: Option<&ContentHashes>) -> String {
    generate_key_at(template, original_filename, hashes, &KeyPlace::default(), Local::now())
}

/// A key for a file from a watched folder.
pub fn generate_key_in(template: &str, original_filename: &str, hashes: Option<&ContentHashes>, place: &KeyPlace) -> String {
    generate_key_at(template, original_filename, hashes, place, Local::now())
}

pub fn generate_key_at(
    template: &str,
    original_filename: &str,
    hashes: Option<&ContentHashes>,
    place: &KeyPlace,
    date: DateTime<Local>,
) -> String {
    let (name, ext) = split_extension(original_filename);
    let uuid = uuid::Uuid::new_v4().to_string();
    let random: String = uuid::Uuid::new_v4().to_string().chars().take(8).collect();
    let subpath = place.subpath.trim_matches('/').to_string();
    let replacements = [
        ("{year}", format!("{:04}", date.year())),
        ("{month}", format!("{:02}", date.month())),
        ("{day}", format!("{:02}", date.day())),
        ("{date}", date.format("%Y-%m-%d").to_string()),
        ("{time}", date.format("%H%M%S").to_string()),
        ("{filename}", name.to_string()),
        ("{uuid}", uuid),
        ("{random}", random),
        ("{ext}", ext.to_string()),
        ("{md5}", hashes.map(|hashes| hashes.md5.clone()).unwrap_or_default()),
        ("{sha256}", hashes.map(|hashes| hashes.sha256.clone()).unwrap_or_default()),
        // A folder's name is one segment, like a file's.
        ("{folder}", place.folder.replace(['/', '\\'], "-")),
        ("{subpath}", subpath.clone()),
    ];
    let mut result = template.to_string();
    for (token, value) in replacements {
        result = result.replace(token, &value);
    }
    if place.keep_structure && !subpath.is_empty() && !template.contains("{subpath}") {
        result = match result.rfind('/') {
            Some(slash) => format!("{}/{subpath}/{}", &result[..slash], &result[slash + 1..]),
            None => format!("{subpath}/{result}"),
        };
    }
    collapse_empty_segments(&result)
}

/// "a//b" is "a/b", and a key never starts with "/": tokens that come out
/// empty (`{subpath}` at a folder's root) leave no gaps.
fn collapse_empty_segments(key: &str) -> String {
    key.split('/').filter(|segment| !segment.is_empty()).collect::<Vec<_>>().join("/")
}

/// Everything Foundation's `.urlPathAllowed` would escape, plus "+": S3
/// decodes a "+" in a path as a space, so "a+b.png" would otherwise 404.
const PATH_SEGMENT: &AsciiSet = &CONTROLS
    .add(b'+')
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

/// Percent-encodes each "/"-separated segment of an object key.
pub fn encode_key_path(key: &str) -> String {
    key.split('/')
        .map(|segment| utf8_percent_encode(segment, PATH_SEGMENT).to_string())
        .collect::<Vec<_>>()
        .join("/")
}

/// Everything but unreserved characters, for the CopyObject source header,
/// which S3-compatible servers decode in different ways; with only
/// unreserved characters left as they are, they all agree.
const COPY_SOURCE_SEGMENT: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'.').remove(b'_').remove(b'~');

/// The key part of a CopyObject source ("bucket/key"), each segment
/// strictly percent-encoded.
pub fn encode_copy_source(key: &str) -> String {
    key.split('/')
        .map(|segment| utf8_percent_encode(segment, COPY_SOURCE_SEGMENT).to_string())
        .collect::<Vec<_>>()
        .join("/")
}

/// publicURL = publicBaseURL + objectKey. The S3 endpoint and the public
/// serving URL are never assumed to be the same host. A bare domain
/// ("img.example.com") is treated as HTTPS.
pub fn resolve_public_url(base_url: &str, object_key: &str) -> String {
    let mut base = normalized_base(base_url);
    while base.ends_with('/') {
        base.pop();
    }
    format!("{base}/{}", encode_key_path(object_key))
}

fn normalized_base(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn formats_images_and_files_differently() {
        let url = "https://cdn.example.com/a.png";
        assert_eq!(format(url, OutputMode::Markdown, "a.png", ""), "![](https://cdn.example.com/a.png)");
        assert_eq!(format(url, OutputMode::Markdown, "a.pdf", ""), "[a.pdf](https://cdn.example.com/a.png)");
        assert_eq!(format(url, OutputMode::Html, "a.png", ""), "<img src=\"https://cdn.example.com/a.png\" alt=\"\">");
        assert_eq!(format(url, OutputMode::Custom, "a.b.png", "{name}|{ext}|{filename}|{url}"), "a.b|png|a.b.png|https://cdn.example.com/a.png");
    }

    #[test]
    fn resolves_public_urls() {
        assert_eq!(resolve_public_url("img.example.com/", "2026/09/a b.png"), "https://img.example.com/2026/09/a%20b.png");
        assert_eq!(resolve_public_url(" http://x.dev ", "ü/#1.txt"), "http://x.dev/%C3%BC/%231.txt");
        assert_eq!(resolve_public_url("x.dev", "a+b (1).png"), "https://x.dev/a%2Bb%20(1).png");
    }

    #[test]
    fn encodes_copy_sources_strictly() {
        assert_eq!(encode_copy_source("dir/a+b (1)&ü.png"), "dir/a%2Bb%20%281%29%26%C3%BC.png");
    }

    #[test]
    fn generates_keys_from_templates() {
        let date = Local.with_ymd_and_hms(2026, 3, 7, 9, 5, 1).unwrap();
        let none = KeyPlace::default();
        let key = generate_key_at("{year}/{month}/{day}/{date}-{time}-{filename}.{ext}", "shot.final.png", None, &none, date);
        assert_eq!(key, "2026/03/07/2026-03-07-090501-shot.final.png");
        let random = generate_key_at("{random}", "x", None, &none, date);
        assert_eq!(random.len(), 8);
    }

    #[test]
    fn fills_content_hashes() {
        let date = Local.with_ymd_and_hms(2026, 3, 7, 9, 5, 1).unwrap();
        let hashes = ContentHashes { md5: "9e10".into(), sha256: "ab12".into() };
        let none = KeyPlace::default();
        assert_eq!(generate_key_at("{md5}/{sha256}.{ext}", "a.webp", Some(&hashes), &none, date), "9e10/ab12.webp");
        assert_eq!(generate_key_at("x{md5}.{ext}", "a.webp", None, &none, date), "x.webp");
        assert!(uses_hashes("{year}/{sha256}.{ext}"));
        assert!(uses_hashes("{md5}"));
        assert!(!uses_hashes("{year}/{uuid}.{ext}"));
    }

    #[test]
    fn fills_watched_folder_tokens() {
        let date = Local.with_ymd_and_hms(2026, 3, 7, 9, 5, 1).unwrap();
        let none = KeyPlace::default();
        // Outside watched folders both are empty, and leave no gaps.
        assert_eq!(generate_key_at("{folder}/{subpath}/{filename}.{ext}", "a.png", None, &none, date), "a.png");
        let root = KeyPlace { folder: "Screen/Shots".into(), subpath: String::new(), keep_structure: true };
        assert_eq!(generate_key_at("{folder}/{subpath}/{filename}.{ext}", "a.png", None, &root, date), "Screen-Shots/a.png");
        let nested = KeyPlace { folder: "Shots".into(), subpath: "2026/march".into(), keep_structure: false };
        assert_eq!(generate_key_at("{folder}/{subpath}/{filename}.{ext}", "a.png", None, &nested, date), "Shots/2026/march/a.png");
        // Not in the template, so flattened...
        assert_eq!(generate_key_at("{year}/{filename}.{ext}", "a.png", None, &nested, date), "2026/a.png");
        // ...unless the structure is kept: before the last component.
        let kept = KeyPlace { keep_structure: true, ..nested.clone() };
        assert_eq!(generate_key_at("{year}/{filename}.{ext}", "a.png", None, &kept, date), "2026/2026/march/a.png");
        assert_eq!(generate_key_at("{filename}.{ext}", "a.png", None, &kept, date), "2026/march/a.png");
        assert_eq!(generate_key_at("{subpath}/{filename}.{ext}", "a.png", None, &kept, date), "2026/march/a.png");
        assert_eq!(generate_key_at("/x//{md5}/{filename}.{ext}", "a.png", None, &none, date), "x/a.png");
    }
}
