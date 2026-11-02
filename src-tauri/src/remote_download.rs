//! A link copied to the clipboard, downloaded when its upload starts and
//! then uploaded like any file (the Mac app's docs/capture-and-instant-links.md,
//! "A copied link"). Names, refusals and the request follow the Mac app's
//! `RemoteDownload.swift`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use percent_encoding::percent_decode_str;
use url::Url;

use crate::t;

/// Longer names are cut, keeping the extension.
pub const MAX_NAME_LENGTH: usize = 120;
/// Kept free on the disk the file is downloaded to.
pub const RESERVE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// A download that hears nothing for this long has stalled.
const TIMEOUT: Duration = Duration::from_secs(60);
const MAX_REDIRECTS: usize = 10;

/// Content types that say nothing about the file, so the link's own
/// extension is kept.
const GENERIC_TYPES: &[&str] = &[
    "application/octet-stream",
    "binary/octet-stream",
    "application/binary",
    "application/unknown",
    "application/x-download",
    "application/force-download",
    "application/download",
    "text/plain",
];

/// A copied link to download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteSource {
    pub url: Url,
    /// The name guessed from the link before anything is downloaded. The
    /// download's own name replaces it, unless the upload was renamed.
    pub suggested_name: String,
}

impl RemoteSource {
    pub fn new(url: Url) -> Self {
        let suggested_name = suggested_name(&url);
        Self { url, suggested_name }
    }
}

/// A finished download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Downloaded {
    pub path: PathBuf,
    pub filename: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadError {
    /// A 4xx answer, which won't change by trying again.
    Status(u16),
    /// The link opened a web page rather than a file.
    WebPage,
    TooLarge,
    Empty,
    /// No connection, a dropped one, or a server that's down or busy (5xx,
    /// 408, 429): it waits and is tried again like an upload.
    Network(String),
    /// Writing the file failed.
    File(String),
}

impl DownloadError {
    pub fn is_transient(&self) -> bool {
        matches!(self, DownloadError::Network(_))
    }
}

impl std::fmt::Display for DownloadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            DownloadError::Status(code) => t!("The link couldn’t be downloaded (HTTP {0}).", code),
            DownloadError::WebPage => t!("The link opens a web page, not a file. Copy the link of the image or file itself."),
            DownloadError::TooLarge => t!("The linked file is bigger than the free space on this PC."),
            DownloadError::Empty => t!("The link gave an empty file."),
            DownloadError::Network(message) | DownloadError::File(message) => message.clone(),
        };
        formatter.write_str(&message)
    }
}

/// From the link alone: its last path segment, or "download".
pub fn suggested_name(url: &Url) -> String {
    let last = url.path_segments().and_then(|mut segments| segments.rfind(|segment| !segment.is_empty())).unwrap_or("");
    let decoded = percent_decode_str(last).decode_utf8_lossy();
    sanitized(&decoded).unwrap_or_else(|| "download".to_string())
}

/// Content-Disposition's filename, else the link's last path segment (after
/// redirects), else "download"; with the extension of `mime_type` when the
/// name has none, or one of another kind of file.
pub fn resolve_name(url: &Url, content_disposition: Option<&str>, mime_type: Option<&str>) -> String {
    let mut name = content_disposition
        .and_then(filename_from_content_disposition)
        .and_then(|name| sanitized(&name))
        .unwrap_or_else(|| suggested_name(url));
    let mime = mime_type.and_then(|value| value.split(';').next()).map(|value| value.trim().to_ascii_lowercase()).unwrap_or_default();
    if mime.is_empty() || GENERIC_TYPES.contains(&mime.as_str()) {
        return limited(&name);
    }
    let Some(preferred) = preferred_extension(&mime) else { return limited(&name) };
    let (base, extension) = crate::util::split_extension(&name);
    if extension.is_empty() {
        name = format!("{name}.{preferred}");
    } else if !same_kind(extension, &mime) {
        name = format!("{base}.{preferred}");
    }
    limited(&name)
}

/// `filename*=UTF-8''...` (RFC 6266/5987) when there is one, otherwise
/// `filename="..."` or `filename=...`.
pub fn filename_from_content_disposition(header: &str) -> Option<String> {
    let parameters = parameters(header);
    let encoded = parameters.iter().find(|(name, _)| name == "filename*").and_then(|(_, value)| {
        let parts: Vec<&str> = value.splitn(3, '\'').collect();
        let [_, _, encoded] = parts.as_slice() else { return None };
        let decoded = percent_decode_str(encoded).decode_utf8().ok()?.into_owned();
        (!decoded.is_empty()).then_some(decoded)
    });
    encoded.or_else(|| parameters.into_iter().find(|(name, value)| name == "filename" && !value.is_empty()).map(|(_, value)| value))
}

/// The header's `name=value` parameters, names lowercased, quoted values
/// unquoted. The disposition type ("attachment") comes first and is skipped.
fn parameters(header: &str) -> Vec<(String, String)> {
    let mut result = Vec::new();
    let mut chars = header.chars().peekable();
    let mut first = true;
    loop {
        // A name: up to "=" or ";".
        let mut name = String::new();
        while let Some(&c) = chars.peek() {
            if c == '=' || c == ';' {
                break;
            }
            name.push(c);
            chars.next();
        }
        let has_value = chars.peek() == Some(&'=');
        let mut value = String::new();
        if has_value {
            chars.next();
            while chars.peek().is_some_and(|c| *c == ' ' || *c == '\t') {
                chars.next();
            }
            if chars.peek() == Some(&'"') {
                chars.next();
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => {
                            if let Some(escaped) = chars.next() {
                                value.push(escaped);
                            }
                        }
                        '"' => break,
                        _ => value.push(c),
                    }
                }
                // Anything after the closing quote, up to the next ";".
                while chars.peek().is_some_and(|c| *c != ';') {
                    chars.next();
                }
            } else {
                while let Some(&c) = chars.peek() {
                    if c == ';' {
                        break;
                    }
                    value.push(c);
                    chars.next();
                }
                value = value.trim().to_string();
            }
        }
        // The disposition type itself has no "=".
        if has_value || !first {
            let name = name.trim().to_ascii_lowercase();
            if !name.is_empty() {
                result.push((name, value));
            }
        }
        first = false;
        if chars.next().is_none() {
            break;
        }
    }
    result
}

/// One safe path segment, or None when nothing usable is left.
fn sanitized(raw: &str) -> Option<String> {
    // A name with folders in it ("a/b.png", "..\\c.png") keeps its last part.
    let last = raw.split(['/', '\\']).rfind(|part| !part.is_empty()).unwrap_or("");
    let trimmed = last.trim();
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        return None;
    }
    let cleaned = crate::output::key_segment(trimmed);
    // Characters Windows doesn't allow in file names (the download is
    // saved under this name) become "-".
    let cleaned: String = cleaned.chars().map(|c| if matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') { '-' } else { c }).collect();
    let cleaned = cleaned.trim_end_matches(['.', ' ']).to_string();
    if cleaned.is_empty() || (cleaned == "file" && trimmed != "file") {
        return None;
    }
    Some(cleaned)
}

fn limited(name: &str) -> String {
    if name.chars().count() <= MAX_NAME_LENGTH {
        return name.to_string();
    }
    let (base, extension) = crate::util::split_extension(name);
    let room = MAX_NAME_LENGTH.saturating_sub(if extension.is_empty() { 0 } else { extension.chars().count() + 1 }).max(1);
    let cut: String = base.chars().take(room).collect();
    if extension.is_empty() {
        cut
    } else {
        format!("{cut}.{extension}")
    }
}

/// The extension a file of `mime` gets.
fn preferred_extension(mime: &str) -> Option<&'static str> {
    let known = match canonical(mime) {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/avif" => "avif",
        "image/heic" => "heic",
        "image/heif" => "heif",
        "image/svg+xml" => "svg",
        "image/bmp" => "bmp",
        "image/tiff" => "tiff",
        "image/x-icon" => "ico",
        "video/mp4" => "mp4",
        "video/quicktime" => "mov",
        "video/webm" => "webm",
        "video/x-matroska" => "mkv",
        "video/x-msvideo" => "avi",
        "video/mpeg" => "mpg",
        "audio/mpeg" => "mp3",
        "audio/mp4" => "m4a",
        "audio/aac" => "aac",
        "audio/wav" => "wav",
        "audio/ogg" => "ogg",
        "audio/flac" => "flac",
        "application/pdf" => "pdf",
        "application/zip" => "zip",
        "application/gzip" => "gz",
        "application/x-tar" => "tar",
        "application/x-7z-compressed" => "7z",
        "application/vnd.rar" => "rar",
        "application/json" => "json",
        "application/xml" => "xml",
        "text/csv" => "csv",
        "text/markdown" => "md",
        "text/css" => "css",
        "text/javascript" => "js",
        "application/wasm" => "wasm",
        "application/rtf" => "rtf",
        "application/msword" => "doc",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => "docx",
        "application/vnd.ms-excel" => "xls",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => "xlsx",
        "application/vnd.ms-powerpoint" => "ppt",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => "pptx",
        "font/woff" => "woff",
        "font/woff2" => "woff2",
        "font/ttf" => "ttf",
        "font/otf" => "otf",
        _ => "",
    };
    if !known.is_empty() {
        return Some(known);
    }
    mime_guess::get_mime_extensions_str(mime).and_then(|extensions| extensions.first().copied())
}

/// The usual name of a type that goes by several.
fn canonical(mime: &str) -> &str {
    match mime {
        "image/jpg" | "image/pjpeg" => "image/jpeg",
        "image/x-png" => "image/png",
        "image/vnd.microsoft.icon" => "image/x-icon",
        "audio/mp3" | "audio/x-mp3" | "audio/mpeg3" => "audio/mpeg",
        "audio/x-wav" | "audio/wave" | "audio/vnd.wave" => "audio/wav",
        "audio/x-m4a" => "audio/mp4",
        "video/x-m4v" => "video/mp4",
        "application/x-zip-compressed" | "application/x-zip" => "application/zip",
        "application/x-gzip" => "application/gzip",
        "application/x-rar-compressed" => "application/vnd.rar",
        "text/x-markdown" => "text/markdown",
        "application/javascript" | "application/x-javascript" => "text/javascript",
        "text/xml" => "application/xml",
        other => other,
    }
}

fn is_xml(mime: &str) -> bool {
    mime == "application/xml" || mime.ends_with("+xml")
}

fn is_json_type(mime: &str) -> bool {
    mime == "application/json" || mime.ends_with("+json")
}

/// Whether a name ending in `extension` is a file of type `mime`, or one
/// close enough to keep its extension (photo.jpeg served as image/jpeg).
fn same_kind(extension: &str, mime: &str) -> bool {
    let extension = extension.to_ascii_lowercase();
    if preferred_extension(mime) == Some(extension.as_str()) {
        return true;
    }
    if mime_guess::get_mime_extensions_str(mime).is_some_and(|extensions| extensions.iter().any(|known| known.eq_ignore_ascii_case(&extension))) {
        return true;
    }
    let served = canonical(mime);
    mime_guess::from_ext(&extension).iter().any(|candidate| {
        let candidate = candidate.essence_str().to_ascii_lowercase();
        let candidate = canonical(&candidate);
        candidate == served || (is_xml(candidate) && is_xml(served)) || (is_json_type(candidate) && is_json_type(served))
    })
}

/// Downloads `source` into `folder` (which must exist), under the name
/// `resolve_name` gives it: the response must be a file (not a web page),
/// no bigger than the disk can take while keeping `RESERVE_BYTES` free. No
/// cookies or cache are used. `progress` hears the fraction done, in steps
/// of 1%, when the size is known.
pub async fn download(source: &RemoteSource, folder: &Path, progress: impl Fn(f64) + Send) -> Result<Downloaded, DownloadError> {
    let max_bytes = crate::win32::free_disk_space(folder).map_or(u64::MAX, |free| free.saturating_sub(RESERVE_BYTES));
    let client = reqwest::Client::builder()
        .user_agent(format!("Aktar/{} (+https://getaktar.com)", env!("CARGO_PKG_VERSION")))
        .connect_timeout(TIMEOUT)
        .read_timeout(TIMEOUT)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS || !matches!(attempt.url().scheme(), "http" | "https") {
                attempt.stop()
            } else {
                attempt.follow()
            }
        }))
        .build()
        .map_err(|error| DownloadError::Network(error.to_string()))?;
    let mut response = client.get(source.url.clone()).send().await.map_err(network_error)?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        // A busy or failing server may answer later; anything else won't.
        return Err(if status >= 500 || status == 408 || status == 429 {
            DownloadError::Network(t!("The link couldn’t be downloaded (HTTP {0}).", status))
        } else {
            DownloadError::Status(status)
        });
    }
    let header = |name: reqwest::header::HeaderName| response.headers().get(name).and_then(|value| value.to_str().ok()).map(str::to_string);
    let content_type = header(reqwest::header::CONTENT_TYPE);
    let content_disposition = header(reqwest::header::CONTENT_DISPOSITION);
    let mime = content_type.as_deref().and_then(|value| value.split(';').next()).map(|value| value.trim().to_ascii_lowercase());
    if matches!(mime.as_deref(), Some("text/html" | "application/xhtml+xml")) {
        return Err(DownloadError::WebPage);
    }
    let expected = response.content_length();
    if expected.is_some_and(|length| length > max_bytes) {
        return Err(DownloadError::TooLarge);
    }
    let name = resolve_name(response.url(), content_disposition.as_deref(), content_type.as_deref());

    let partial = folder.join(format!(".download-{}", crate::util::new_id()));
    let result = async {
        use tokio::io::AsyncWriteExt;
        let mut file = tokio::fs::File::create(&partial).await.map_err(|error| DownloadError::File(error.to_string()))?;
        let mut written: u64 = 0;
        let mut reported = 0.0;
        while let Some(chunk) = response.chunk().await.map_err(network_error)? {
            written += chunk.len() as u64;
            if written > max_bytes {
                return Err(DownloadError::TooLarge);
            }
            file.write_all(&chunk).await.map_err(|error| DownloadError::File(error.to_string()))?;
            if let Some(total) = expected.filter(|total| *total > 0) {
                let fraction = (written as f64 / total as f64).min(1.0);
                if fraction - reported >= 0.01 {
                    reported = fraction;
                    progress(fraction);
                }
            }
        }
        file.flush().await.map_err(|error| DownloadError::File(error.to_string()))?;
        Ok(written)
    }
    .await;
    let written = match result {
        Ok(written) => written,
        Err(error) => {
            let _ = std::fs::remove_file(&partial);
            return Err(error);
        }
    };
    if written == 0 {
        let _ = std::fs::remove_file(&partial);
        return Err(DownloadError::Empty);
    }
    let path = folder.join(&name);
    let _ = std::fs::remove_file(&path);
    if let Err(error) = std::fs::rename(&partial, &path) {
        let _ = std::fs::remove_file(&partial);
        return Err(DownloadError::File(error.to_string()));
    }
    progress(1.0);
    Ok(Downloaded { path, filename: name })
}

fn network_error(error: reqwest::Error) -> DownloadError {
    DownloadError::Network(t!("Could not connect. {0}", error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    #[test]
    fn suggests_names_from_links() {
        assert_eq!(suggested_name(&url("https://example.com/shots/photo%20one.png?x=1")), "photo one.png");
        assert_eq!(suggested_name(&url("https://example.com/a/")), "a");
        assert_eq!(suggested_name(&url("https://example.com/")), "download");
        assert_eq!(suggested_name(&url("https://example.com")), "download");
        assert_eq!(suggested_name(&url("https://example.com/..%2F..%2Fsecret.txt")), "secret.txt");
        assert_eq!(suggested_name(&url("https://example.com/a%5Cb.png")), "b.png");
        assert_eq!(suggested_name(&url("https://example.com/what%3F.png")), "what-.png");
    }

    #[test]
    fn reads_content_disposition() {
        assert_eq!(filename_from_content_disposition("attachment; filename=\"report.pdf\"").as_deref(), Some("report.pdf"));
        assert_eq!(filename_from_content_disposition("attachment; filename=report.pdf").as_deref(), Some("report.pdf"));
        assert_eq!(
            filename_from_content_disposition("attachment; filename=\"fallback.pdf\"; filename*=UTF-8''%C3%B6zet%20rapor.pdf").as_deref(),
            Some("özet rapor.pdf")
        );
        assert_eq!(filename_from_content_disposition("inline; FILENAME=\"a \\\"b\\\".png\"").as_deref(), Some("a \"b\".png"));
        assert_eq!(filename_from_content_disposition("filename=bare.txt").as_deref(), Some("bare.txt"));
        assert_eq!(filename_from_content_disposition("attachment"), None);
        assert_eq!(filename_from_content_disposition("attachment; filename=\"\""), None);
    }

    #[test]
    fn resolves_names_with_the_served_type() {
        let link = url("https://cdn.example.com/files/photo.jpg");
        assert_eq!(resolve_name(&link, None, Some("image/jpeg")), "photo.jpg");
        assert_eq!(resolve_name(&link, None, Some("image/webp")), "photo.webp");
        assert_eq!(resolve_name(&link, None, Some("application/octet-stream")), "photo.jpg");
        assert_eq!(resolve_name(&url("https://example.com/photo.jpeg"), None, Some("image/jpeg; charset=binary")), "photo.jpeg");
        assert_eq!(resolve_name(&url("https://example.com/download"), None, Some("application/pdf")), "download.pdf");
        assert_eq!(resolve_name(&url("https://example.com/image.php?id=3"), None, Some("image/png")), "image.png");
        assert_eq!(resolve_name(&url("https://example.com/x"), Some("attachment; filename=\"Q3.xlsx\""), None), "Q3.xlsx");
        assert_eq!(resolve_name(&url("https://example.com/notes.md"), None, Some("text/markdown")), "notes.md");
        assert_eq!(resolve_name(&url("https://example.com/data.json"), None, Some("text/plain")), "data.json");
        let long = format!("https://example.com/{}.png", "a".repeat(300));
        let name = resolve_name(&url(&long), None, Some("image/png"));
        assert_eq!(name.chars().count(), MAX_NAME_LENGTH);
        assert!(name.ends_with(".png"));
    }

    /// Answers each connection with `respond(path)`: the raw response.
    async fn server(respond: fn(&str) -> Vec<u8>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else { return };
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buffer = [0u8; 1024];
                    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                        let Ok(read) = stream.read(&mut buffer).await else { return };
                        if read == 0 {
                            return;
                        }
                        request.extend_from_slice(&buffer[..read]);
                    }
                    let text = String::from_utf8_lossy(&request);
                    let path = text.split_whitespace().nth(1).unwrap_or("/").to_string();
                    let _ = stream.write_all(&respond(&path)).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        format!("http://{address}")
    }

    fn response(status: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
        let mut text = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
        for (name, value) in headers {
            text.push_str(&format!("{name}: {value}\r\n"));
        }
        text.push_str("\r\n");
        let mut bytes = text.into_bytes();
        bytes.extend_from_slice(body);
        bytes
    }

    fn routes(path: &str) -> Vec<u8> {
        match path {
            "/image" => response("200 OK", &[("Content-Type", "image/png")], b"\x89PNG fake"),
            "/named" => response("200 OK", &[("Content-Type", "application/pdf"), ("Content-Disposition", "attachment; filename=\"Q3 report.pdf\"")], b"%PDF"),
            "/moved" => response("302 Found", &[("Location", "/files/photo.jpg")], b""),
            "/files/photo.jpg" => response("200 OK", &[("Content-Type", "image/webp")], b"RIFFxxxxWEBP"),
            "/page" => response("200 OK", &[("Content-Type", "text/html; charset=utf-8")], b"<html></html>"),
            "/empty" => response("200 OK", &[("Content-Type", "image/png")], b""),
            "/busy" => response("503 Service Unavailable", &[], b"busy"),
            _ => response("404 Not Found", &[], b"nope"),
        }
    }

    async fn fetch(base: &str, path: &str) -> (tempfile_dir::Dir, Result<Downloaded, DownloadError>) {
        let folder = tempfile_dir::Dir::new();
        let source = RemoteSource::new(url(&format!("{base}{path}")));
        let result = download(&source, folder.path(), |_| {}).await;
        (folder, result)
    }

    #[tokio::test]
    async fn downloads_files_and_refuses_pages() {
        let base = server(routes).await;

        let (_folder, image) = fetch(&base, "/image").await;
        let image = image.unwrap();
        assert_eq!(image.filename, "image.png");
        assert_eq!(std::fs::read(&image.path).unwrap(), b"\x89PNG fake");

        let (_folder, named) = fetch(&base, "/named").await;
        assert_eq!(named.unwrap().filename, "Q3 report.pdf");

        // Redirects are followed; the name comes from where they end, with
        // the served type's extension.
        let (_folder, moved) = fetch(&base, "/moved").await;
        assert_eq!(moved.unwrap().filename, "photo.webp");

        let (folder, page) = fetch(&base, "/page").await;
        assert_eq!(page, Err(DownloadError::WebPage));
        assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 0);

        let (folder, empty) = fetch(&base, "/empty").await;
        assert_eq!(empty, Err(DownloadError::Empty));
        assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 0);

        let (_folder, missing) = fetch(&base, "/missing").await;
        assert_eq!(missing, Err(DownloadError::Status(404)));
        assert!(!missing.unwrap_err().is_transient());

        let (_folder, busy) = fetch(&base, "/busy").await;
        assert!(busy.unwrap_err().is_transient());
    }

    #[tokio::test]
    async fn a_closed_port_waits_for_the_network() {
        // Bound and dropped: nothing listens there.
        let address = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        let (_folder, result) = fetch(&format!("http://{address}"), "/a.png").await;
        assert!(result.unwrap_err().is_transient());
    }

    /// A folder of its own under the temp folder, removed when dropped.
    mod tempfile_dir {
        pub struct Dir(std::path::PathBuf);

        impl Dir {
            pub fn new() -> Self {
                let path = std::env::temp_dir().join(format!("aktar-download-test-{}", crate::util::new_id()));
                std::fs::create_dir_all(&path).unwrap();
                Self(path)
            }

            pub fn path(&self) -> &std::path::Path {
                &self.0
            }
        }

        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }
}
