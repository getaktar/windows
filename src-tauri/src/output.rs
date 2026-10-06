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
        OutputMode::Markdown => format!("[{}]({public_url})", markdown_escaped(filename)),
        OutputMode::Html if is_image(filename) => format!("<img src=\"{public_url}\" alt=\"\">"),
        OutputMode::Html => format!("<a href=\"{public_url}\">{}</a>", html_escaped(filename)),
        // The template is the user's own, so nothing is escaped in it.
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

/// `text` safe inside an HTML element or a quoted attribute.
fn html_escaped(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            c => escaped.push(c),
        }
    }
    escaped
}

/// `text` as Markdown link text that can't end the link early.
fn markdown_escaped(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '\\' | '[' | ']' | '(' | ')') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
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

/// Extensions a browser runs as a page or a script when the file is opened
/// from the bucket's own address.
const ACTIVE_EXTENSIONS: [&str; 9] = ["html", "htm", "xhtml", "xht", "svg", "svgz", "xml", "js", "mjs"];
/// The same, by content type (any parameters after ";" aside).
const ACTIVE_CONTENT_TYPES: [&str; 7] = [
    "text/html",
    "application/xhtml+xml",
    "image/svg+xml",
    "text/xml",
    "application/xml",
    "text/javascript",
    "application/javascript",
];

/// `attachment` for files that would otherwise run in the browser on the
/// bucket's domain (HTML, SVG, XML, JavaScript), so opening the link
/// downloads them instead. The content type stays, so an SVG still shows
/// in an `<img>`. None for everything else.
pub fn content_disposition(object_key: &str, content_type: &str) -> Option<&'static str> {
    let name = object_key.rsplit('/').next().unwrap_or(object_key);
    let extension = split_extension(name).1.to_ascii_lowercase();
    let mime = content_type.split(';').next().unwrap_or_default().trim().to_ascii_lowercase();
    (ACTIVE_EXTENSIONS.contains(&extension.as_str()) || ACTIVE_CONTENT_TYPES.contains(&mime.as_str())).then_some("attachment")
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

/// The path template of a new destination. Saved ones keep theirs.
pub const DEFAULT_OBJECT_PATH_TEMPLATE: &str = "{year}/{month}/{short}.{ext}";
/// The shortest links: just the code, on the bucket's own domain.
pub const CLEAN_URL_TEMPLATE: &str = "{short}.{ext}";

/// Keys with `{short}` are never written over an existing file; see
/// `short_keys`.
pub fn uses_short_code(template: &str) -> bool {
    template.contains("{short}")
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
    generate_key_with(template, original_filename, hashes, place, date, crate::short_keys::short_code)
}

/// `generate_key_at` with `short_code` filling `{short}` (see
/// `short_keys`); it's only asked when the template has one.
pub fn generate_key_with(
    template: &str,
    original_filename: &str,
    hashes: Option<&ContentHashes>,
    place: &KeyPlace,
    date: DateTime<Local>,
    short_code: impl FnOnce() -> String,
) -> String {
    let (name, ext) = split_extension(original_filename);
    // A name can't add folders to the key, nor an extension.
    let name = key_segment(name);
    let ext = if ext.is_empty() { String::new() } else { key_segment(ext) };
    let uuid = uuid::Uuid::new_v4().to_string();
    let random: String = uuid::Uuid::new_v4().to_string().chars().take(8).collect();
    let short = if uses_short_code(template) { short_code() } else { String::new() };
    let subpath = place.subpath.trim_matches('/').to_string();
    let replacements = [
        ("{year}", format!("{:04}", date.year())),
        ("{month}", format!("{:02}", date.month())),
        ("{day}", format!("{:02}", date.day())),
        ("{date}", date.format("%Y-%m-%d").to_string()),
        ("{time}", date.format("%H%M%S").to_string()),
        ("{filename}", name.clone()),
        ("{uuid}", uuid),
        ("{random}", random),
        ("{short}", short),
        ("{ext}", ext.clone()),
        ("{md5}", hashes.map(|hashes| hashes.md5.clone()).unwrap_or_default()),
        ("{sha256}", hashes.map(|hashes| hashes.sha256.clone()).unwrap_or_default()),
        // A folder's name is one segment, like a file's.
        ("{folder}", if place.folder.is_empty() { String::new() } else { key_segment(&place.folder.replace(['/', '\\'], "-")) }),
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

/// A file's name (or extension) as one segment of a key: without slashes,
/// backslashes, NUL or other control characters, which would add folders
/// or break the key. A name left empty, or "." or "..", is "file".
pub fn key_segment(name: &str) -> String {
    let cleaned: String = name.chars().filter(|c| *c != '/' && *c != '\\' && !c.is_control()).collect();
    match cleaned.as_str() {
        "" | "." | ".." => "file".to_string(),
        _ => cleaned,
    }
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
    fn escapes_file_names_in_markup() {
        let url = "https://x.dev/a";
        assert_eq!(format(url, OutputMode::Html, "<b>&'\".pdf", ""), "<a href=\"https://x.dev/a\">&lt;b&gt;&amp;&#39;&quot;.pdf</a>");
        assert_eq!(format(url, OutputMode::Markdown, "a](evil) \\[1].pdf", ""), "[a\\]\\(evil\\) \\\\\\[1\\].pdf](https://x.dev/a)");
        // The custom template is the user's own.
        assert_eq!(format(url, OutputMode::Custom, "<a>.txt", "{filename}"), "<a>.txt");
    }

    #[test]
    fn downloads_active_content() {
        for key in ["a.html", "dir/b.HTM", "c.svg", "d.svgz", "e.xml", "f.js", "g.mjs", "h.xhtml", "i.xht"] {
            assert_eq!(content_disposition(key, "application/octet-stream"), Some("attachment"), "{key}");
        }
        assert_eq!(content_disposition("page", "text/html; charset=utf-8"), Some("attachment"));
        assert_eq!(content_disposition("x.bin", "image/svg+xml"), Some("attachment"));
        assert_eq!(content_disposition("a.png", &content_type("a.png")), None);
        assert_eq!(content_disposition("html/a.txt", "text/plain"), None);
    }

    #[test]
    fn keeps_names_to_one_key_segment() {
        let date = Local.with_ymd_and_hms(2026, 3, 7, 9, 5, 1).unwrap();
        let none = KeyPlace::default();
        assert_eq!(generate_key_at("up/{filename}.{ext}", "../../etc\\pass\u{0}wd.png", None, &none, date), "up/....etcpasswd.png");
        assert_eq!(generate_key_at("up/{filename}.{ext}", "...png", None, &none, date), "up/file.png");
        assert_eq!(generate_key_at("up/{filename}", "..", None, &none, date), "up/file");
        assert_eq!(generate_key_at("up/{filename}", "a\nb", None, &none, date), "up/ab");
        assert_eq!(key_segment("ok name"), "ok name");
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
    fn fills_the_short_code() {
        let date = Local.with_ymd_and_hms(2026, 3, 7, 9, 5, 1).unwrap();
        let none = KeyPlace::default();
        let key = generate_key_with("{year}/{short}.{ext}", "photo.png", None, &none, date, || "A7kdP2x".into());
        assert_eq!(key, "2026/A7kdP2x.png");
        // No code is drawn for a template without {short}.
        let key = generate_key_with("{filename}.{ext}", "photo.png", None, &none, date, || panic!("drew a code"));
        assert_eq!(key, "photo.png");
        assert_eq!(generate_key_at(DEFAULT_OBJECT_PATH_TEMPLATE, "a.png", None, &none, date).len(), "2026/03/".len() + 7 + ".png".len());
        assert!(uses_short_code(CLEAN_URL_TEMPLATE));
        assert!(!uses_short_code("{year}/{uuid}.{ext}"));
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
