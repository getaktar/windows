use std::path::PathBuf;

use arboard::{Clipboard, ImageData};

use crate::uploads::UploadInput;

pub fn copy(text: &str) {
    match Clipboard::new() {
        Ok(mut clipboard) => {
            if let Err(error) = clipboard.set_text(text) {
                log::warn!("Could not copy to the clipboard: {error}");
            }
        }
        Err(error) => log::warn!("Could not open the clipboard: {error}"),
    }
}

/// Copies a secret (a transfer link, the local API token), kept out of
/// Windows' clipboard history and cloud clipboard.
pub fn copy_concealed(text: &str) -> Result<(), String> {
    let mut clipboard = Clipboard::new().map_err(|error| error.to_string())?;
    #[cfg(windows)]
    let result = {
        use arboard::SetExtWindows;
        clipboard.set().exclude_from_monitoring().text(text)
    };
    #[cfg(not(windows))]
    let result = clipboard.set_text(text);
    result.map_err(|error| error.to_string())
}

/// The text on the clipboard, if there is any.
pub fn text() -> Option<String> {
    Clipboard::new().ok()?.get_text().ok()
}

/// Empties the clipboard if it still holds `text`, and only then: what was
/// copied since is left alone.
pub fn clear_if_holding(text: &str) {
    let Ok(mut clipboard) = Clipboard::new() else { return };
    if clipboard.get_text().is_ok_and(|current| current == text) {
        let _ = clipboard.clear();
    }
}

/// Puts an image on the clipboard (a QR code).
pub fn copy_image(image: &image::GrayImage) -> Result<(), String> {
    let rgba = image::DynamicImage::ImageLuma8(image.clone()).to_rgba8();
    let data = ImageData { width: rgba.width() as usize, height: rgba.height() as usize, bytes: rgba.into_raw().into() };
    Clipboard::new().and_then(|mut clipboard| clipboard.set_image(data)).map_err(|error| error.to_string())
}

/// Inspects the clipboard for something uploadable (the Mac app's
/// docs/capture-and-instant-links.md, "Clipboard text and links"), in this
/// order:
/// - files and folders copied in File Explorer,
/// - raw image data (a screenshot, or an image copied from a browser),
/// - a link (the whole text is one http(s) address): the file it points to
///   is downloaded when the upload starts,
/// - text, saved as a .txt, .md or .json file.
pub fn read_inputs() -> Vec<UploadInput> {
    let Ok(mut clipboard) = Clipboard::new() else { return Vec::new() };

    if let Ok(paths) = clipboard.get().file_list() {
        let files: Vec<UploadInput> = paths
            .into_iter()
            .filter(|path| path.is_file() || path.is_dir())
            .map(UploadInput::from_path)
            .collect();
        if !files.is_empty() {
            return files;
        }
    }

    if let Ok(image) = clipboard.get_image() {
        let filename = format!("clipboard-{}.png", chrono::Utc::now().timestamp());
        if let Some(input) = write_temporary_image(image, filename) {
            return vec![input];
        }
    }

    let text = clipboard.get_text().unwrap_or_default();
    drop(clipboard);
    let link = crate::clipboard_text::link_in(&text).or_else(|| copied_web_link(&text));
    if let Some(link) = link {
        return remote_input(link).into_iter().collect();
    }
    if crate::clipboard_text::has_content(&text) {
        return write_temporary_text(&text).into_iter().collect();
    }
    Vec::new()
}

/// A link copied with its title (one line, or none) as the text, and the
/// address in the browser's URL format.
fn copied_web_link(text: &str) -> Option<url::Url> {
    if text.contains(['\n', '\r']) {
        return None;
    }
    crate::win32::clipboard_url().and_then(|raw| crate::clipboard_text::link_in(&raw))
}

/// The image on the clipboard as a screenshot ("Screenshot 2026-11-02 at
/// 14.03.12.png", named the way the Mac app names its captures), or None
/// when there's no image.
pub fn read_capture() -> Option<UploadInput> {
    let image = Clipboard::new().ok()?.get_image().ok()?;
    let filename = format!("Screenshot {}.png", chrono::Local::now().format("%Y-%m-%d at %H.%M.%S"));
    write_temporary_image(image, filename)
}

fn write_temporary_image(image: ImageData, filename: String) -> Option<UploadInput> {
    let buffer = image::RgbaImage::from_raw(image.width as u32, image.height as u32, image.bytes.into_owned())?;
    // `filename` is the name the upload goes by (history, the object key).
    // The file itself gets a unique name: two pastes within the same second
    // must never write to the one file while the first is uploading.
    let directory = temporary_directory();
    std::fs::create_dir_all(&directory).ok()?;
    let path = directory.join(format!("{}.png", crate::util::new_id()));
    buffer.save_with_format(&path, image::ImageFormat::Png).ok()?;
    Some(UploadInput { original_filename: filename, temporary: true, ..UploadInput::from_path(path) })
}

/// A folder of its own in the temp folder, deleted with its upload.
fn new_temporary_folder() -> Option<PathBuf> {
    let folder = temporary_directory().join(crate::util::new_id());
    std::fs::create_dir_all(&folder).ok()?;
    Some(folder)
}

/// Text saved as `clipboard-<8 hex>.<txt|md|json>`, UTF-8. Never named after
/// its first line: that could be a password or a secret.
fn write_temporary_text(text: &str) -> Option<UploadInput> {
    let id = uuid::Uuid::new_v4().simple().to_string();
    let filename = format!("clipboard-{}.{}", &id[..8], crate::clipboard_text::kind_of(text).extension());
    let folder = new_temporary_folder()?;
    let path = folder.join(&filename);
    if std::fs::write(&path, text.as_bytes()).is_err() {
        let _ = std::fs::remove_dir_all(&folder);
        return None;
    }
    Some(UploadInput { original_filename: filename, temporary: true, temporary_folder: Some(folder), ..UploadInput::from_path(path) })
}

/// An upload whose file is downloaded from `link` first, into a folder of
/// its own, deleted with the upload like a copied image.
pub fn remote_input(link: url::Url) -> Option<UploadInput> {
    let source = crate::remote_download::RemoteSource::new(link);
    let folder = new_temporary_folder()?;
    let path = folder.join(&source.suggested_name);
    Some(UploadInput {
        original_filename: source.suggested_name.clone(),
        temporary: true,
        temporary_folder: Some(folder),
        remote: Some(source),
        ..UploadInput::from_path(path)
    })
}

fn temporary_directory() -> PathBuf {
    std::env::temp_dir().join("Aktar")
}

/// Clipboard images and local API uploads are staged in the temp folder
/// only for as long as their job, and jobs don't outlive the app, so
/// anything still there at launch was left by a crash or a failed upload
/// that was never retried.
pub fn remove_leftovers() {
    let _ = std::fs::remove_dir_all(temporary_directory());
    let _ = std::fs::remove_dir_all(std::env::temp_dir().join("AktarLocalAPI"));
}
