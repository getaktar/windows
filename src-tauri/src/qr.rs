//! QR codes for links ("Show QR Code"): the modules for the window to draw
//! as crisp squares, and a PNG to copy or save.

use qrcode::{EcLevel, QrCode};
use serde::Serialize;

/// White modules around the code, as the QR spec asks for.
const QUIET_ZONE: u32 = 4;
/// Pixels per module in the PNG: about 600 px for a typical link.
const MODULE_PIXELS: u32 = 12;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QrMatrix {
    /// Modules per side, without the quiet zone.
    pub size: usize,
    /// Row by row, "1" for a dark module and "0" for a light one.
    pub modules: String,
}

fn code(text: &str) -> Result<QrCode, String> {
    QrCode::with_error_correction_level(text.as_bytes(), EcLevel::M).map_err(|error| error.to_string())
}

pub fn matrix(text: &str) -> Result<QrMatrix, String> {
    let code = code(text)?;
    let modules = code.to_colors().iter().map(|color| if *color == qrcode::Color::Dark { '1' } else { '0' }).collect();
    Ok(QrMatrix { size: code.width(), modules })
}

/// Black on white with the quiet zone, in whole pixels per module so it
/// stays sharp.
pub fn image(text: &str) -> Result<image::GrayImage, String> {
    let code = code(text)?;
    let width = code.width() as u32;
    let colors = code.to_colors();
    let side = (width + 2 * QUIET_ZONE) * MODULE_PIXELS;
    Ok(image::GrayImage::from_fn(side, side, |x, y| {
        let (column, row) = (x / MODULE_PIXELS, y / MODULE_PIXELS);
        let inside = (QUIET_ZONE..QUIET_ZONE + width).contains(&column) && (QUIET_ZONE..QUIET_ZONE + width).contains(&row);
        let dark = inside && colors[((row - QUIET_ZONE) * width + column - QUIET_ZONE) as usize] == qrcode::Color::Dark;
        image::Luma([if dark { 0 } else { 255 }])
    }))
}

pub fn png(text: &str) -> Result<Vec<u8>, String> {
    let mut data = Vec::new();
    image(text)?
        .write_to(&mut std::io::Cursor::new(&mut data), image::ImageFormat::Png)
        .map_err(|error| error.to_string())?;
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_the_code_with_a_quiet_zone() {
        let link = "https://img.example.com/2026/10/0b4e7c1e-0e5d-4a8f-9a0e-3c1f2a6b7d8e.png";
        let matrix = matrix(link).unwrap();
        assert_eq!(matrix.modules.len(), matrix.size * matrix.size);
        let image = image(link).unwrap();
        let side = (matrix.size as u32 + 8) * MODULE_PIXELS;
        assert_eq!(image.dimensions(), (side, side));
        // The corner is quiet zone, then the finder pattern starts dark.
        assert_eq!(image.get_pixel(0, 0).0, [255]);
        assert_eq!(image.get_pixel(4 * MODULE_PIXELS, 4 * MODULE_PIXELS).0, [0]);
        assert!(png(link).unwrap().starts_with(b"\x89PNG"));
    }
}
