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
    let filename = format!("clipboard-{}.png", chrono::Utc::now().timestamp());
    let directory: PathBuf = std::env::temp_dir().join("Aktar");
    std::fs::create_dir_all(&directory).ok()?;
    let path = directory.join(&filename);
    buffer.save_with_format(&path, image::ImageFormat::Png).ok()?;
    Some(UploadInput { path, original_filename: filename, object_key: None })
}
