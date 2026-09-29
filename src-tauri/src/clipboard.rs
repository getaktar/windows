use std::path::PathBuf;

use arboard::{Clipboard, ImageData};

use crate::uploads::{Expiry, UploadInput};

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

/// Inspects the clipboard for something uploadable: files copied in File
/// Explorer take priority, falling back to raw image data (a screenshot or
/// an image copied from a browser, which isn't backed by a file).
pub fn read_inputs() -> Vec<UploadInput> {
    let Ok(mut clipboard) = Clipboard::new() else { return Vec::new() };

    if let Ok(paths) = clipboard.get().file_list() {
        let files: Vec<UploadInput> = paths
            .into_iter()
            .filter(|path| path.is_file())
            .map(UploadInput::from_path)
            .collect();
        if !files.is_empty() {
            return files;
        }
    }

    if let Ok(image) = clipboard.get_image() {
        if let Some(input) = write_temporary_image(image) {
            return vec![input];
        }
    }
    Vec::new()
}

fn write_temporary_image(image: ImageData) -> Option<UploadInput> {
    let buffer = image::RgbaImage::from_raw(image.width as u32, image.height as u32, image.bytes.into_owned())?;
    // The name the upload goes by (history, the object key), same as on the
    // Mac. The file itself gets a unique name: two pastes within the same
    // second must never write to the one file while the first is uploading.
    let filename = format!("clipboard-{}.png", chrono::Utc::now().timestamp());
    let directory = temporary_directory();
    std::fs::create_dir_all(&directory).ok()?;
    let path = directory.join(format!("{}.png", crate::util::new_id()));
    buffer.save_with_format(&path, image::ImageFormat::Png).ok()?;
    Some(UploadInput { path, original_filename: filename, object_key: None, temporary: true, expiry: Expiry::FromSettings })
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
