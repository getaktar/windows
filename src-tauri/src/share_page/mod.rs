//! Share pages (docs/share-pages.md in the Mac repo): the one static HTML
//! page Aktar writes next to an upload, a gallery, a text page, a file
//! request, or an encrypted file's page. Everything here is a port of
//! `docs/share-page/reference.mjs`, so every app fills the page the same
//! way. The encrypted file format is in `crypto`.

pub mod crypto;

use serde::Serialize;

/// The page, byte for byte the Mac repo's `docs/share-page/template.html`.
/// Never edit this copy: change it there and copy it here (the test below
/// checks the hash).
pub const TEMPLATE: &str = include_str!("../../resources/share-page.html");
/// SHA-256 of `TEMPLATE`, as `shasum -a 256 docs/share-page/template.html`
/// prints it in the Mac repo.
#[cfg(test)]
const TEMPLATE_SHA256: &str = "f8fc8be762b53c2ea8c83f11ad23cfaf2f5d0ad2109334db54c7a93e756aea93";

/// How a page is uploaded: it opens in the browser (no
/// Content-Disposition), and isn't cached for long, since Replace File
/// writes it again.
pub const CONTENT_TYPE: &str = "text/html; charset=utf-8";
pub const CACHE_CONTROL: &str = "public, max-age=60";

/// og:image is the file itself only up to this size.
const OG_IMAGE_MAX_BYTES: u64 = 8 * 1024 * 1024;
/// Text pages embed files up to this size.
pub const MAX_TEXT_BYTES: u64 = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    File,
    Gallery,
    Text,
    Encrypted,
    Request,
}

/// A file the page shows. `og_image` is only for the head (see
/// `og_image_for`), never in the data.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PageFile {
    pub name: String,
    pub url: String,
    pub size: u64,
    /// The Content-Type it was uploaded with.
    pub mime_type: String,
    pub thumb: Option<String>,
    pub og_image: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Encrypted {
    pub url: String,
    pub size: u64,
    pub password: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub title: String,
    pub note: String,
    pub max_bytes: u64,
    pub conditional: bool,
    pub slots: Vec<Slot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Slot {
    /// Presigned PUTs for the file and its `.json` metadata.
    pub file: String,
    pub meta: String,
}

/// What a page shows (`{{DATA}}`, version 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub kind: Kind,
    /// The app's UI language code.
    pub lang: String,
    /// ISO 8601 UTC, see `iso8601`.
    pub created: String,
    pub expires: Option<String>,
    pub files: Option<Vec<PageFile>>,
    pub text: Option<String>,
    pub syntax: Option<String>,
    pub encrypted: Option<Encrypted>,
    pub request: Option<Request>,
    /// None shows the "Shared with Aktar" footer; v1 never turns it off.
    pub brand: Option<bool>,
}

impl Page {
    pub fn new(kind: Kind, lang: &str, created_millis: i64) -> Self {
        Self {
            kind,
            lang: lang.to_string(),
            created: iso8601(created_millis),
            expires: None,
            files: None,
            text: None,
            syntax: None,
            encrypted: None,
            request: None,
            brand: None,
        }
    }
}

/// ISO 8601 UTC with whole seconds: "2026-11-09T12:00:00Z".
pub fn iso8601(millis: i64) -> String {
    crate::util::iso8601(millis)
}

// MARK: - Escaping

/// `& < > " ' { }` as entities, so nothing put in the page can be markup or
/// a placeholder.
pub fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            '{' => escaped.push_str("&#123;"),
            '}' => escaped.push_str("&#125;"),
            c => escaped.push(c),
        }
    }
    escaped
}

/// JSON (as `serde_json` writes it, the same as `JSON.stringify`) for
/// inside a script element: nothing in it can end the element or start a
/// comment.
pub fn escape_json(json: &str) -> String {
    let mut escaped = String::with_capacity(json.len());
    for c in json.chars() {
        match c {
            '<' => escaped.push_str("\\u003c"),
            '>' => escaped.push_str("\\u003e"),
            '&' => escaped.push_str("\\u0026"),
            '\u{2028}' => escaped.push_str("\\u2028"),
            '\u{2029}' => escaped.push_str("\\u2029"),
            c => escaped.push(c),
        }
    }
    escaped
}

fn json<T: Serialize + ?Sized>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".into())
}

// MARK: - Head

/// "2.4 MB": decimal units, one decimal below 100, none for bytes. Rounds
/// as JavaScript's `toFixed` does (half up on the exact value), so the
/// apps' descriptions match.
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut n = bytes as f64;
    let mut unit = 0;
    while n >= 1000.0 && unit < UNITS.len() - 1 {
        n /= 1000.0;
        unit += 1;
    }
    let digits = if unit == 0 || n >= 100.0 { 0 } else { 1 };
    format!("{} {}", to_fixed(n, digits), UNITS[unit])
}

/// `Number.prototype.toFixed` for a positive `n` and 0 or 1 digits: the
/// exact decimal value, rounded half up.
fn to_fixed(n: f64, digits: usize) -> String {
    // A double's exact decimal expansion below 10^15 ends well within this.
    let exact = format!("{n:.80}");
    let (whole, fraction) = exact.split_once('.').unwrap_or((&exact, ""));
    let kept: String = format!("{whole}{}", &fraction[..digits]);
    let round_up = fraction.as_bytes().get(digits).is_some_and(|digit| *digit >= b'5');
    let mut value: u128 = kept.parse().unwrap_or(0);
    if round_up {
        value += 1;
    }
    let text = format!("{value:0width$}", width = digits + 1);
    if digits == 0 {
        text
    } else {
        let (whole, fraction) = text.split_at(text.len() - digits);
        format!("{whole}.{fraction}")
    }
}

fn extension_label(name: &str) -> String {
    match name.rfind('.') {
        Some(dot) if dot > 0 && dot < name.len() - 1 => name[dot + 1..].to_uppercase(),
        _ => String::new(),
    }
}

/// JavaScript's `\s`.
fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'
    )
}

/// The text excerpt for og:description: whitespace collapsed, 200
/// characters (code points) at most, with an ellipsis when cut.
pub fn excerpt(text: &str) -> String {
    let mut flat = String::with_capacity(text.len().min(4096));
    let mut in_space = false;
    for c in text.chars() {
        if is_js_whitespace(c) {
            in_space = true;
        } else {
            if in_space && !flat.is_empty() {
                flat.push(' ');
            }
            in_space = false;
            flat.push(c);
        }
    }
    if flat.chars().count() > 200 {
        let mut cut: String = flat.chars().take(199).collect();
        cut.push('\u{2026}');
        cut
    } else {
        flat
    }
}

fn is_page_video(mime_type: &str) -> bool {
    matches!(mime_type, "video/mp4" | "video/webm")
}

/// The `<title>` and the tags in place of `{{META}}`, in `headFor`'s order.
/// Both unescaped title and the finished tags.
pub fn head_for(page: &Page) -> (String, String) {
    let files = page.files.as_deref().unwrap_or_default();
    let mut image = None;
    let mut video = None;
    let (title, description) = match page.kind {
        Kind::Encrypted => ("Encrypted file".to_string(), "Open the link to decrypt it in the browser.".to_string()),
        Kind::Request => {
            let request = page.request.as_ref();
            let title = request.map(|request| request.title.clone()).filter(|title| !title.is_empty()).unwrap_or_else(|| "Send files".into());
            let description = match request.map(|request| request.note.as_str()).filter(|note| !note.is_empty()) {
                Some(note) => excerpt(note),
                None => "Upload files straight to the requester\u{2019}s storage.".into(),
            };
            (title, description)
        }
        Kind::Gallery => {
            let first = files.first().map(|file| file.name.as_str()).unwrap_or_default();
            let total: u64 = files.iter().map(|file| file.size).sum();
            image = files.iter().find_map(|file| file.og_image.clone());
            (format!("{first} +{}", files.len().saturating_sub(1)), format!("{} files \u{b7} {}", files.len(), format_size(total)))
        }
        Kind::File | Kind::Text => {
            let file = files.first().cloned().unwrap_or_default();
            let description = match (&page.kind, page.text.as_deref().filter(|text| !text.is_empty())) {
                (Kind::Text, Some(text)) => excerpt(text),
                _ => [format_size(file.size), extension_label(&file.name)].into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" \u{b7} "),
            };
            image = file.og_image.clone();
            if is_page_video(&file.mime_type) {
                video = Some(file.clone());
            }
            (file.name, description)
        }
    };
    let mut tags: Vec<(&str, &str, String)> = vec![
        ("name", "description", description.clone()),
        ("property", "og:title", title.clone()),
        ("property", "og:description", description),
        ("property", "og:type", if video.is_some() { "video.other" } else { "website" }.into()),
    ];
    if let Some(image) = &image {
        tags.push(("property", "og:image", image.clone()));
        tags.push(("property", "og:image:alt", title.clone()));
    }
    if let Some(video) = &video {
        tags.push(("property", "og:video", video.url.clone()));
        tags.push(("property", "og:video:secure_url", video.url.clone()));
        tags.push(("property", "og:video:type", video.mime_type.clone()));
    }
    tags.push(("name", "twitter:card", if image.is_some() { "summary_large_image" } else { "summary" }.into()));
    tags.push(("name", "twitter:title", title.clone()));
    let meta = tags
        .iter()
        .map(|(attribute, key, value)| format!("<meta {attribute}=\"{key}\" content=\"{}\">", escape_html(value)))
        .collect::<Vec<_>>()
        .join("\n");
    (title, meta)
}

/// The page's data as JSON, with the fields in `buildPage`'s order.
/// The MIME type without parameters (`text/plain; charset=utf-8` ->
/// `text/plain`), as the page data and the encrypted metadata carry it.
pub fn bare_mime(content_type: &str) -> String {
    content_type.split(';').next().unwrap_or("").trim().to_string()
}

pub fn data_json(page: &Page) -> String {
    let lang = if page.lang.is_empty() { "en" } else { page.lang.as_str() };
    let mut out = format!("{{\"v\":1,\"kind\":{},\"lang\":{},\"created\":{},\"expires\":{}", json(&page.kind), json(lang), json(&page.created), json(&page.expires));
    if let Some(files) = &page.files {
        let files: Vec<String> = files
            .iter()
            .map(|file| {
                format!(
                    "{{\"name\":{},\"url\":{},\"size\":{},\"type\":{},\"thumb\":{}}}",
                    json(&file.name),
                    json(&file.url),
                    file.size,
                    json(&bare_mime(&file.mime_type)),
                    json(&file.thumb)
                )
            })
            .collect();
        out.push_str(&format!(",\"files\":[{}]", files.join(",")));
    }
    if page.kind == Kind::Text {
        out.push_str(&format!(",\"text\":{},\"syntax\":{}", json(&page.text), json(&page.syntax)));
    }
    if let (Kind::Encrypted, Some(encrypted)) = (page.kind, &page.encrypted) {
        out.push_str(&format!(",\"encrypted\":{}", json(encrypted)));
    }
    if let (Kind::Request, Some(request)) = (page.kind, &page.request) {
        out.push_str(&format!(",\"request\":{}", json(request)));
    }
    if page.brand == Some(false) {
        out.push_str(",\"brand\":false");
    }
    out.push('}');
    out
}

/// The finished page: the template with `{{LANG}}`, `{{TITLE}}`,
/// `{{META}}` and `{{DATA}}` replaced, each once and in that order. What
/// goes in can't contain a placeholder (`{` and `}` are escaped in the
/// first three, and the data comes last). Replaced literally (unlike JS's
/// `String.replace` with a string, which would expand `$&` in a title).
pub fn build_page(page: &Page) -> String {
    let (title, meta) = head_for(page);
    let lang = if page.lang.is_empty() { "en" } else { page.lang.as_str() };
    TEMPLATE
        .replacen("{{LANG}}", &escape_html(lang), 1)
        .replacen("{{TITLE}}", &escape_html(&title), 1)
        .replacen("{{META}}", &meta, 1)
        .replacen("{{DATA}}", &escape_json(&data_json(page)), 1)
}

// MARK: - What gets a page, and where

/// A single file's page: next to it (`2026/11/x7Kp2.png.html`), so Delete
/// after removes both.
pub fn page_key(file_key: &str) -> String {
    format!("{file_key}.html")
}

/// A gallery's page, next to the first file that made it.
pub fn gallery_key(first_file_key: &str) -> String {
    format!("{first_file_key}.gallery.html")
}

/// An encrypted upload's key, ignoring the path template (names and
/// extensions would leak): `e/` plus 22 random characters, under the
/// "Delete after" folder when it expires. Its page is this plus `.html`.
pub fn encrypted_key(expire_after_days: Option<u32>) -> String {
    let key = format!("e/{}", random_alphanumeric(22));
    match expire_after_days {
        Some(days) => crate::expiry::expiring_key(&key, days),
        None => key,
    }
}

/// `count` random characters from `[0-9A-Za-z]`, without bias.
pub fn random_alphanumeric(count: usize) -> String {
    use rand::Rng;
    const ALPHABET: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let mut rng = rand::rngs::OsRng;
    (0..count).map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char).collect()
}

/// The page's og:image: the file itself for an image crawlers render (not
/// SVG or HEIC) up to 8 MB, else the bucket thumbnail, else none.
pub fn og_image_for(mime_type: &str, size: u64, file_url: &str, thumb: Option<&str>) -> Option<String> {
    let mime = mime_type.split(';').next().unwrap_or_default().trim().to_ascii_lowercase();
    let renders = mime.starts_with("image/") && !matches!(mime.as_str(), "image/svg+xml" | "image/heic" | "image/heif" | "image/heic-sequence" | "image/heif-sequence");
    if renders && size <= OG_IMAGE_MAX_BYTES {
        return Some(file_url.to_string());
    }
    thumb.map(str::to_string)
}

/// Media keep the file link in Markdown and HTML (they're embedded there).
pub fn is_media(mime_type: &str) -> bool {
    ["image/", "video/", "audio/"].iter().any(|prefix| mime_type.starts_with(prefix))
}

/// Whether a file of this Content-Type may become a text page (the size
/// and UTF-8 checks come on top).
pub fn is_text_type(mime_type: &str) -> bool {
    let mime = mime_type.split(';').next().unwrap_or_default().trim().to_ascii_lowercase();
    mime.starts_with("text/")
        || matches!(
            mime.as_str(),
            "application/json" | "application/xml" | "application/javascript" | "application/x-sh" | "application/x-yaml" | "application/toml"
        )
}

/// The file's text when it's a text page: a text type, at most 256 KB,
/// and valid UTF-8.
pub fn text_contents(path: &std::path::Path, mime_type: &str) -> Option<String> {
    if !is_text_type(mime_type) {
        return None;
    }
    let size = std::fs::metadata(path).ok()?.len();
    if size > MAX_TEXT_BYTES {
        return None;
    }
    String::from_utf8(std::fs::read(path).ok()?).ok()
}

/// The highlighter's language for a file name (docs/share-pages.md, part 3).
pub fn syntax_for(filename: &str) -> &'static str {
    let name = crate::util::last_component(filename);
    match name.to_ascii_lowercase().as_str() {
        "dockerfile" => return "dockerfile",
        "makefile" | "gnumakefile" => return "makefile",
        _ => {}
    }
    let extension = crate::util::split_extension(name).1.to_ascii_lowercase();
    match extension.as_str() {
        "swift" => "swift",
        "kt" | "kts" => "kotlin",
        "java" => "java",
        "js" | "mjs" | "cjs" => "js",
        "ts" | "mts" | "cts" => "ts",
        "jsx" => "jsx",
        "tsx" => "tsx",
        "json" => "json",
        "py" => "python",
        "rb" => "ruby",
        "go" => "go",
        "rs" => "rust",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        "cs" => "csharp",
        "php" => "php",
        "sh" => "shell",
        "bash" => "bash",
        "zsh" => "zsh",
        "ps1" | "psm1" | "psd1" => "powershell",
        "sql" => "sql",
        "html" | "htm" => "html",
        "css" => "css",
        "scss" => "scss",
        "xml" | "plist" | "svg" => "xml",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "ini" | "cfg" | "conf" => "ini",
        "md" | "markdown" => "markdown",
        "dockerfile" => "dockerfile",
        "mk" | "makefile" => "makefile",
        "r" => "r",
        "pl" | "pm" => "perl",
        "lua" => "lua",
        "dart" => "dart",
        "scala" | "sc" => "scala",
        "ex" | "exs" => "elixir",
        _ => "text",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest;

    fn file(name: &str, size: u64, mime_type: &str, og_image: Option<&str>) -> PageFile {
        PageFile {
            name: name.into(),
            url: format!("https://cdn.example.com/{name}"),
            size,
            mime_type: mime_type.into(),
            thumb: None,
            og_image: og_image.map(str::to_string),
        }
    }

    #[test]
    fn the_template_is_the_mac_copy() {
        // Source: aktar/mac docs/share-page/template.html. If this fails,
        // copy that file here again byte for byte (never edit this copy).
        assert_eq!(crate::util::hex(&sha2::Sha256::digest(TEMPLATE.as_bytes())), TEMPLATE_SHA256);
        for placeholder in ["{{LANG}}", "{{TITLE}}", "{{META}}", "{{DATA}}"] {
            assert_eq!(TEMPLATE.matches(placeholder).count(), 1, "{placeholder}");
        }
    }

    #[test]
    fn escapes_like_the_reference() {
        assert_eq!(
            escape_html("<a href=\"x\">{{DATA}}</a> & 'q'"),
            "&lt;a href=&quot;x&quot;&gt;&#123;&#123;DATA&#125;&#125;&lt;/a&gt; &amp; &#39;q&#39;"
        );
        assert_eq!(escape_json(&json("</script><!-- & \u{2028}\u{2029}")), "\"\\u003c/script\\u003e\\u003c!-- \\u0026 \\u2028\\u2029\"");
    }

    #[test]
    fn formats_sizes_like_to_fixed() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(999), "999 B");
        assert_eq!(format_size(1000), "1.0 KB");
        assert_eq!(format_size(2_400_000), "2.4 MB");
        assert_eq!(format_size(512_000), "512 KB");
        // Exact ties round up, as toFixed does (Rust's own rounding would
        // give 1.2 and 100).
        assert_eq!(format_size(1250), "1.3 KB");
        assert_eq!(format_size(100_500), "101 KB");
        assert_eq!(format_size(1_050), "1.1 KB");
        assert_eq!(format_size(99_960_000), "100.0 MB");
        assert_eq!(format_size(999_999), "1000 KB");
        assert_eq!(format_size(4_100_000), "4.1 MB");
    }

    #[test]
    fn excerpts_text() {
        assert_eq!(excerpt("  a\n\n b  "), "a b");
        assert_eq!(excerpt(&"x".repeat(300)).chars().count(), 200);
        assert!(excerpt(&"x".repeat(300)).ends_with('\u{2026}'));
        assert_eq!(excerpt(&"x".repeat(200)), "x".repeat(200));
        // JavaScript's \s: U+FEFF is space, U+0085 isn't.
        assert_eq!(excerpt("a\u{feff}b\u{85}c\u{3000}d"), "a b\u{85}c d");
    }

    #[test]
    fn writes_the_head_and_data_of_a_file_page() {
        let mut page = Page::new(Kind::File, "tr", 1_794_225_600_000);
        page.files = Some(vec![file("</script><script>alert(1)</script>{{META}}.png", 2_400_000, "image/png", Some("https://cdn.example.com/a.png"))]);
        let html = build_page(&page);
        let start = html.find("<script id=\"aktar-data\"").unwrap();
        let end = start + html[start..].find("</script>").unwrap();
        let outside = format!("{}{}", &html[..start], &html[end..]);
        assert!(!outside.contains("{{"), "no placeholder left outside the data");
        assert_eq!(html.matches("</script>").count(), 2, "only the two script elements end");
        assert!(html.contains("<meta property=\"og:image\" content=\"https://cdn.example.com/a.png\">"));
        assert!(html.contains("<meta name=\"twitter:card\" content=\"summary_large_image\">"));
        assert!(html.contains("content=\"2.4 MB \u{b7} PNG\""));
        assert!(html.contains("<html lang=\"tr\""));
        assert!(html.contains(&format!("<title>{}</title>", escape_html("</script><script>alert(1)</script>{{META}}.png"))));
        // A "$&" in a name is just text.
        page.files = Some(vec![file("a$&b$'.txt", 5, "text/plain", None)]);
        assert!(build_page(&page).contains("<title>a$&amp;b$&#39;.txt</title>"));
    }

    #[test]
    fn orders_the_head_tags() {
        let mut page = Page::new(Kind::File, "en", 0);
        page.files = Some(vec![file("clip.mp4", 5_000_000, "video/mp4", Some("https://cdn.example.com/t.webp"))]);
        let (title, meta) = head_for(&page);
        assert_eq!(title, "clip.mp4");
        assert_eq!(
            meta,
            [
                "<meta name=\"description\" content=\"5.0 MB \u{b7} MP4\">",
                "<meta property=\"og:title\" content=\"clip.mp4\">",
                "<meta property=\"og:description\" content=\"5.0 MB \u{b7} MP4\">",
                "<meta property=\"og:type\" content=\"video.other\">",
                "<meta property=\"og:image\" content=\"https://cdn.example.com/t.webp\">",
                "<meta property=\"og:image:alt\" content=\"clip.mp4\">",
                "<meta property=\"og:video\" content=\"https://cdn.example.com/clip.mp4\">",
                "<meta property=\"og:video:secure_url\" content=\"https://cdn.example.com/clip.mp4\">",
                "<meta property=\"og:video:type\" content=\"video/mp4\">",
                "<meta name=\"twitter:card\" content=\"summary_large_image\">",
                "<meta name=\"twitter:title\" content=\"clip.mp4\">",
            ]
            .join("\n")
        );
        let mut page = Page::new(Kind::File, "en", 0);
        page.files = Some(vec![file("README", 12, "application/octet-stream", None)]);
        let (_, meta) = head_for(&page);
        assert!(meta.contains("content=\"12 B\""));
        assert!(meta.contains("og:type\" content=\"website\""));
        assert!(meta.contains("twitter:card\" content=\"summary\""));
    }

    #[test]
    fn writes_the_data_for_each_kind() {
        let mut page = Page::new(Kind::Text, "pt-BR", 1_794_225_600_000);
        page.expires = Some("2026-11-16T12:00:00Z".into());
        page.files = Some(vec![file("a.swift", 6, "text/x-swift", None)]);
        page.text = Some("let x = \"<b>\"\n".into());
        page.syntax = Some("swift".into());
        assert_eq!(
            data_json(&page),
            r#"{"v":1,"kind":"text","lang":"pt-BR","created":"2026-11-09T12:00:00Z","expires":"2026-11-16T12:00:00Z","files":[{"name":"a.swift","url":"https://cdn.example.com/a.swift","size":6,"type":"text/x-swift","thumb":null}],"text":"let x = \"<b>\"\n","syntax":"swift"}"#
        );
        let (title, meta) = head_for(&page);
        assert_eq!(title, "a.swift");
        assert!(meta.contains("content=\"let x = &quot;&lt;b&gt;&quot;\""), "{meta}");
        assert!(build_page(&page).contains(r#""text":"let x = \"\u003cb\u003e\"\n""#));

        let mut page = Page::new(Kind::Gallery, "en", 0);
        page.files = Some(vec![
            file("a.pdf", 1_000_000, "application/pdf", None),
            file("b.png", 3_100_000, "image/png", Some("https://cdn.example.com/b.png")),
            file("c.txt", 0, "text/plain", None),
        ]);
        let (title, meta) = head_for(&page);
        assert_eq!(title, "a.pdf +2");
        assert!(meta.contains("content=\"3 files \u{b7} 4.1 MB\""));
        assert!(meta.contains("og:image\" content=\"https://cdn.example.com/b.png\""));
        assert!(!data_json(&page).contains("ogImage"));
        assert!(!data_json(&page).contains("\"text\""));

        let mut page = Page::new(Kind::Encrypted, "en", 0);
        page.encrypted = Some(Encrypted { url: "https://cdn.example.com/e/x".into(), size: 4567, password: true });
        assert_eq!(
            data_json(&page),
            r#"{"v":1,"kind":"encrypted","lang":"en","created":"1970-01-01T00:00:00Z","expires":null,"encrypted":{"url":"https://cdn.example.com/e/x","size":4567,"password":true}}"#
        );
        let (title, meta) = head_for(&page);
        assert_eq!(title, "Encrypted file");
        assert!(meta.contains("Open the link to decrypt it in the browser."));
        assert!(!meta.contains("og:image"));

        let mut page = Page::new(Kind::Request, "", 0);
        page.request = Some(Request {
            title: String::new(),
            note: String::new(),
            max_bytes: 2_000_000_000,
            conditional: true,
            slots: vec![Slot { file: "https://s3/f?a=1&b=2".into(), meta: "https://s3/f.json".into() }],
        });
        assert_eq!(
            data_json(&page),
            r#"{"v":1,"kind":"request","lang":"en","created":"1970-01-01T00:00:00Z","expires":null,"request":{"title":"","note":"","maxBytes":2000000000,"conditional":true,"slots":[{"file":"https://s3/f?a=1&b=2","meta":"https://s3/f.json"}]}}"#
        );
        assert!(build_page(&page).contains(r#""file":"https://s3/f?a=1\u0026b=2""#));
        let (title, meta) = head_for(&page);
        assert_eq!(title, "Send files");
        assert!(meta.contains("Upload files straight to the requester\u{2019}s storage."));
        page.request.as_mut().unwrap().title = "Logs".into();
        page.request.as_mut().unwrap().note = "Please send\n the logs".into();
        let (title, meta) = head_for(&page);
        assert_eq!(title, "Logs");
        assert!(meta.contains("content=\"Please send the logs\""));
    }

    #[test]
    fn places_pages() {
        assert_eq!(page_key("2026/11/x7Kp2.png"), "2026/11/x7Kp2.png.html");
        assert_eq!(page_key("tmp/7d/2026/11/x7Kp2.png"), "tmp/7d/2026/11/x7Kp2.png.html");
        assert_eq!(gallery_key("2026/11/a.png"), "2026/11/a.png.gallery.html");
        let key = encrypted_key(None);
        assert!(key.starts_with("e/") && key.len() == 24 && key[2..].bytes().all(|byte| byte.is_ascii_alphanumeric()), "{key}");
        assert!(encrypted_key(Some(7)).starts_with("tmp/7d/e/"));
        assert_ne!(encrypted_key(None), encrypted_key(None));
    }

    #[test]
    fn picks_og_images() {
        assert_eq!(og_image_for("image/png", 100, "https://x/a.png", None).as_deref(), Some("https://x/a.png"));
        assert_eq!(og_image_for("image/png", OG_IMAGE_MAX_BYTES + 1, "https://x/a.png", Some("https://x/t.webp")).as_deref(), Some("https://x/t.webp"));
        assert_eq!(og_image_for("image/svg+xml", 100, "https://x/a.svg", None), None);
        assert_eq!(og_image_for("image/heic", 100, "https://x/a.heic", Some("https://x/t.webp")).as_deref(), Some("https://x/t.webp"));
        assert_eq!(og_image_for("application/pdf", 100, "https://x/a.pdf", None), None);
    }

    #[test]
    fn tells_text_and_its_syntax() {
        for mime in ["text/plain; charset=utf-8", "text/markdown", "application/json", "application/x-yaml", "application/toml", "application/x-sh"] {
            assert!(is_text_type(mime), "{mime}");
        }
        for mime in ["application/pdf", "image/svg+xml", "application/octet-stream"] {
            assert!(!is_text_type(mime), "{mime}");
        }
        assert_eq!(syntax_for("main.swift"), "swift");
        assert_eq!(syntax_for("App.TSX"), "tsx");
        assert_eq!(syntax_for("Dockerfile"), "dockerfile");
        assert_eq!(syntax_for("Makefile"), "makefile");
        assert_eq!(syntax_for("notes.txt"), "text");
        assert_eq!(syntax_for("data.csv"), "text");
        assert_eq!(syntax_for("app.log"), "text");
        assert_eq!(syntax_for("README.md"), "markdown");
        assert_eq!(syntax_for("script.ps1"), "powershell");
        assert_eq!(syntax_for("noext"), "text");
    }
}
