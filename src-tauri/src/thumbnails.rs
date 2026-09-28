//! Small local thumbnails for history rows. The uploaded file itself is
//! never copied, only a PNG whose longest side is `MAX_PIXEL_SIZE`.

use std::path::Path;

use image::{DynamicImage, ImageDecoder, ImageReader};

/// History rows show thumbnails at up to 96 px, so this stays sharp on
/// high-DPI displays.
const MAX_PIXEL_SIZE: u32 = 320;
/// Decoding is done in full before scaling, so skip anything huge.
const MAX_SOURCE_BYTES: u64 = 80 * 1024 * 1024;

pub fn store(source: &Path, destination: &Path) {
    if let Err(error) = try_store(source, destination) {
        log::debug!("No thumbnail for {}: {error}", source.display());
    }
}

fn try_store(source: &Path, destination: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if std::fs::metadata(source)?.len() > MAX_SOURCE_BYTES {
        return Ok(());
    }
    let mut decoder = ImageReader::open(source)?.with_guessed_format()?.into_decoder()?;
    // Honor EXIF orientation, so photos from phones aren't shown sideways.
    let orientation = decoder.orientation()?;
    let mut image = DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
    let thumbnail = image.thumbnail(MAX_PIXEL_SIZE, MAX_PIXEL_SIZE);
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    thumbnail.save_with_format(destination, image::ImageFormat::Png)?;
    Ok(())
}
