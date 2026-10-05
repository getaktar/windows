//! Makes thumbnails. Photos are decoded with the `image` crate; everything
//! else (videos, PDFs, RAW and HEIC photos, Office documents) goes through
//! the Windows shell's thumbnailer, the one Explorer uses, so whatever
//! Explorer can preview gets a thumbnail here too. A file the shell only has
//! an icon for gets none.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::Duration;

use image::{DynamicImage, ImageDecoder, ImageReader};

/// Longest side of a thumbnail, in pixels: sharp in rows on high-DPI
/// displays, and up to 256 px wide where a file's details show one.
pub const MAX_PIXEL_SIZE: u32 = 512;
const WEBP_QUALITY: f32 = 80.0;
/// The shell thumbnailer gets this long per file; one that's stuck is left
/// behind on its own thread.
#[cfg_attr(not(windows), allow(dead_code))]
const SHELL_TIMEOUT: Duration = Duration::from_secs(15);
/// Decoding is done in full before scaling, so huge photos are left to the
/// shell.
const MAX_DECODED_BYTES: u64 = 80 * 1024 * 1024;

/// Kinds of files that only ever have an icon.
const ICON_ONLY: [&str; 28] = [
    "zip", "7z", "rar", "tar", "gz", "tgz", "bz2", "xz", "zst", "cab", "dmg", "iso", "img", "vhd", "vhdx", "exe", "msi", "msix",
    "appx", "dll", "sys", "bat", "cmd", "ps1", "apk", "ipa", "jar", "pkg",
];

/// Whether a file of this name could have a thumbnail at all.
pub fn can_have_thumbnail(filename: &str) -> bool {
    match crate::util::split_extension(filename) {
        (_, "") => false,
        (_, extension) => !ICON_ONLY.contains(&extension.to_ascii_lowercase().as_str()),
    }
}

/// "RIFF", a length, then "WEBP".
pub fn is_webp(data: &[u8]) -> bool {
    data.len() > 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP"
}

/// The thumbnail of the file at `path`, as WebP (PNG only if WebP couldn't
/// be encoded), or None.
pub async fn generate(path: PathBuf) -> Option<Vec<u8>> {
    let name = path.file_name()?.to_string_lossy().into_owned();
    if path.is_dir() || !can_have_thumbnail(&name) {
        return None;
    }
    tauri::async_runtime::spawn_blocking(move || {
        let image = decoded(&path).or_else(|| shell::thumbnail(&path, MAX_PIXEL_SIZE))?;
        encode(&image)
    })
    .await
    .ok()
    .flatten()
}

/// A photo the `image` crate reads, upright.
fn decoded(path: &Path) -> Option<DynamicImage> {
    image::ImageFormat::from_path(path).ok()?;
    if std::fs::metadata(path).ok()?.len() > MAX_DECODED_BYTES {
        return None;
    }
    let mut decoder = ImageReader::open(path).ok()?.with_guessed_format().ok()?.into_decoder().ok()?;
    let (width, height) = decoder.dimensions();
    if !crate::image_processing::within_pixel_limit(width, height) {
        return None;
    }
    // Honor EXIF orientation, so photos from phones aren't shown sideways.
    let orientation = decoder.orientation().ok()?;
    let mut image = DynamicImage::from_decoder(decoder).ok()?;
    image.apply_orientation(orientation);
    Some(image)
}

/// Never larger than `MAX_PIXEL_SIZE`, whatever the source.
pub(super) fn encode(image: &DynamicImage) -> Option<Vec<u8>> {
    let image = if image.width().max(image.height()) > MAX_PIXEL_SIZE {
        image.thumbnail(MAX_PIXEL_SIZE, MAX_PIXEL_SIZE)
    } else {
        image.clone()
    };
    let (width, height) = (image.width(), image.height());
    let webp = if image.color().has_alpha() {
        let pixels = image.to_rgba8();
        webp::Encoder::from_rgba(&pixels, width, height).encode(WEBP_QUALITY).to_vec()
    } else {
        let pixels = image.to_rgb8();
        webp::Encoder::from_rgb(&pixels, width, height).encode(WEBP_QUALITY).to_vec()
    };
    if is_webp(&webp) {
        return Some(webp);
    }
    let mut png = Vec::new();
    image.write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png).ok()?;
    Some(png)
}

#[cfg(windows)]
mod shell {
    use std::path::Path;

    use image::{DynamicImage, RgbaImage};
    use windows::core::HSTRING;
    use windows::Win32::Foundation::SIZE;
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, GetObjectW, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
        DIB_RGB_COLORS, HBITMAP,
    };
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::{IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_BIGGERSIZEOK, SIIGBF_THUMBNAILONLY};

    /// The shell's thumbnail on a thread of its own (thumbnail handlers want
    /// a single-threaded apartment), given up on after `SHELL_TIMEOUT`.
    pub fn thumbnail(path: &Path, size: u32) -> Option<DynamicImage> {
        let path = path.to_path_buf();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("thumbnail".into())
            .spawn(move || {
                let _ = sender.send(on_this_thread(&path, size));
            })
            .ok()?;
        receiver.recv_timeout(super::SHELL_TIMEOUT).ok().flatten()
    }

    fn on_this_thread(path: &Path, size: u32) -> Option<DynamicImage> {
        let initialized = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
        let image = shell_image(path, size);
        if initialized {
            unsafe { CoUninitialize() };
        }
        image
    }

    fn shell_image(path: &Path, size: u32) -> Option<DynamicImage> {
        let side = i32::try_from(size).ok()?;
        let factory: IShellItemImageFactory = unsafe { SHCreateItemFromParsingName(&HSTRING::from(path.as_os_str()), None) }.ok()?;
        // Only a real thumbnail: without THUMBNAILONLY the shell hands back
        // the file's icon.
        let bitmap = unsafe { factory.GetImage(SIZE { cx: side, cy: side }, SIIGBF_THUMBNAILONLY | SIIGBF_BIGGERSIZEOK) }.ok()?;
        let image = pixels(bitmap);
        let _ = unsafe { DeleteObject(bitmap.into()) };
        image
    }

    /// The bitmap's pixels, top row first, as straight (not premultiplied)
    /// RGBA.
    fn pixels(bitmap: HBITMAP) -> Option<DynamicImage> {
        let mut info = BITMAP::default();
        let size = std::mem::size_of::<BITMAP>() as i32;
        if unsafe { GetObjectW(bitmap.into(), size, Some(&mut info as *mut BITMAP as *mut _)) } == 0 {
            return None;
        }
        let (width, height) = (u32::try_from(info.bmWidth).ok()?, u32::try_from(info.bmHeight.abs()).ok()?);
        if width == 0 || height == 0 {
            return None;
        }
        let mut header = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                // Negative: top-down rows.
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut data = vec![0u8; (width as usize) * (height as usize) * 4];
        let context = unsafe { CreateCompatibleDC(None) };
        let lines = unsafe {
            GetDIBits(context, bitmap, 0, height, Some(data.as_mut_ptr().cast()), &mut header, DIB_RGB_COLORS)
        };
        let _ = unsafe { DeleteDC(context) };
        if lines == 0 {
            return None;
        }
        // Bitmaps without transparency often leave alpha at 0.
        let opaque = data.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 0);
        for pixel in data.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
            if opaque {
                pixel[3] = 255;
            } else if pixel[3] > 0 && pixel[3] < 255 {
                let alpha = u32::from(pixel[3]);
                for channel in &mut pixel[..3] {
                    *channel = ((u32::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
                }
            }
        }
        RgbaImage::from_raw(width, height, data).map(DynamicImage::ImageRgba8)
    }
}

#[cfg(not(windows))]
mod shell {
    /// Development builds on other systems have only the `image` crate.
    pub fn thumbnail(_path: &std::path::Path, _size: u32) -> Option<image::DynamicImage> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_which_files_can_have_one() {
        for name in ["a.png", "a.mov", "a.mp4", "a.pdf", "a.heic", "a.docx", "a.txt", "a.CR2"] {
            assert!(can_have_thumbnail(name), "{name}");
        }
        for name in ["a.zip", "a.exe", "a.MSI", "noextension"] {
            assert!(!can_have_thumbnail(name), "{name}");
        }
        assert!(!is_webp(b"RIFF0000WAVEdata"));
    }

    #[test]
    fn makes_a_small_webp() {
        let image = DynamicImage::ImageRgb8(image::RgbImage::from_pixel(1200, 600, image::Rgb([255, 0, 0])));
        let data = encode(&image).unwrap();
        assert!(is_webp(&data));
        let decoded = image::load_from_memory(&data).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (512, 256));
    }

    #[tokio::test]
    async fn generates_from_a_photo_on_disk() {
        let folder = std::env::temp_dir().join(format!("aktar-thumbnail-{}", crate::util::new_id()));
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("photo.png");
        image::RgbaImage::from_pixel(300, 900, image::Rgba([0, 0, 255, 128])).save(&path).unwrap();
        let data = generate(path.clone()).await.unwrap();
        let decoded = image::load_from_memory(&data).unwrap();
        assert_eq!(decoded.height(), 512);
        assert!((170..=171).contains(&decoded.width()));
        assert!(generate(folder.join("archive.zip")).await.is_none());
        let _ = std::fs::remove_dir_all(&folder);
    }
}
