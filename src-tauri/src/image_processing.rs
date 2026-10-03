//! Converts, recompresses and resizes photos before they're uploaded (the
//! destination's "Image Processing" settings), like the Mac app's
//! ImageProcessor. Covers JPEG, PNG, HEIC/HEIF (where Windows can decode
//! it), WebP, TIFF and BMP; GIFs (which may be animated), SVGs and
//! everything else are left as they are.
//!
//! The pixels are decoded with the EXIF orientation applied, scaled down if
//! they're bigger than the destination allows, and encoded again. The
//! destination's "Image metadata" policy then decides what EXIF the new
//! file gets; the color profile is always kept, or the pixels converted to
//! sRGB where the format can't carry one.

use std::io::Cursor;
use std::path::{Path, PathBuf};

use image::codecs::avif::AvifEncoder;
use image::codecs::bmp::BmpEncoder;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType as PngFilter, PngEncoder};
use image::codecs::tiff::TiffEncoder;
use image::codecs::webp::WebPEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, ImageDecoder, ImageEncoder, ImageReader};

use crate::destinations::{ImageFormat, ImageMetadataPolicy, ImageProcessing};
use crate::image_metadata;

/// Bigger files aren't processed: decoding them would take more memory
/// than a photo should.
pub const MAX_SOURCE_BYTES: u64 = 200 * 1024 * 1024;
/// Nor are images with more pixels than this, whatever the file's size: a
/// small file can claim huge dimensions, and decoding it would take all
/// the memory there is. Read from the header, before anything is decoded.
pub const MAX_PIXELS: u64 = 100_000_000;
/// The quality conversions use when no compression is picked.
const DEFAULT_QUALITY: u8 = 90;
/// rav1e's speed, 1 (slowest) to 10. Without NASM, 9 encodes a 12 MP
/// photo in about 2 seconds on 8 cores, at the size 8 gets in twice that;
/// 10 is faster again but noticeably bigger.
const AVIF_SPEED: u8 = 9;

/// A processed copy of a photo, which the caller deletes with
/// `image_metadata::remove_copy` once it's done with it.
#[derive(Debug)]
pub struct Processed {
    pub path: PathBuf,
    /// The extension the file now has, when it changed ("webp" for a PNG
    /// converted to WebP).
    pub new_extension: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Jpeg,
    Png,
    Heif,
    Webp { lossless: bool },
    Tiff,
    Bmp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Jpeg(u8),
    Png,
    Tiff,
    Bmp,
    WebpLossy(u8),
    WebpLossless,
    Avif(u8),
}

impl Target {
    fn extension(self) -> &'static str {
        match self {
            Target::Jpeg(_) => "jpg",
            Target::Png => "png",
            Target::Tiff => "tiff",
            Target::Bmp => "bmp",
            Target::WebpLossy(_) | Target::WebpLossless => "webp",
            Target::Avif(_) => "avif",
        }
    }

    /// Whether this is the format the photo was already in.
    fn is_format_of(self, source: Source) -> bool {
        matches!(
            (self, source),
            (Target::Jpeg(_), Source::Jpeg)
                | (Target::Png, Source::Png)
                | (Target::Tiff, Source::Tiff)
                | (Target::Bmp, Source::Bmp)
                | (Target::WebpLossy(_) | Target::WebpLossless, Source::Webp { .. })
        )
    }
}

/// What a photo becomes, or None when it's uploaded as it is: no format
/// change, no compression for its format, and no need to resize.
fn plan(source: Source, has_alpha: bool, settings: ImageProcessing, resizing: bool) -> Option<Target> {
    match settings.format {
        // Lossless WebP for a transparent PNG (screenshots, logos) when no
        // compression was picked, so nothing is lost but the size.
        ImageFormat::Webp if settings.quality.is_none() && source == Source::Png && has_alpha => Some(Target::WebpLossless),
        ImageFormat::Webp => Some(Target::WebpLossy(settings.quality.unwrap_or(DEFAULT_QUALITY))),
        ImageFormat::Avif => Some(Target::Avif(settings.quality.unwrap_or(DEFAULT_QUALITY))),
        ImageFormat::Original => {
            let quality = settings.quality.unwrap_or(DEFAULT_QUALITY);
            match source {
                // Lossy formats are recompressed at the quality picked.
                Source::Jpeg if settings.quality.is_some() || resizing => Some(Target::Jpeg(quality)),
                Source::Webp { lossless: true } if settings.quality.is_none() && resizing => Some(Target::WebpLossless),
                Source::Webp { .. } if settings.quality.is_some() || resizing => Some(Target::WebpLossy(quality)),
                // Windows can't write HEIC, so a HEIC photo that needs
                // resizing becomes a JPEG. Only compressing it would make
                // a JPEG bigger than the HEIC, so it's left alone then.
                Source::Heif if resizing => Some(Target::Jpeg(quality)),
                // Lossless formats only change when they're resized.
                Source::Png if resizing => Some(Target::Png),
                Source::Tiff if resizing => Some(Target::Tiff),
                Source::Bmp if resizing => Some(Target::Bmp),
                _ => None,
            }
        }
    }
}

/// Whether an image of `width` x `height` may be decoded.
pub fn within_pixel_limit(width: u32, height: u32) -> bool {
    u64::from(width) * u64::from(height) <= MAX_PIXELS
}

/// A processed copy of `path`, or None when it's uploaded as it is (not a
/// photo this handles, too big, or nothing to change). A photo that can't
/// be decoded is uploaded as it is too, as if processing were off.
pub fn process(path: &Path, settings: ImageProcessing, policy: ImageMetadataPolicy) -> Result<Option<Processed>, String> {
    let size = std::fs::metadata(path).map_err(|error| error.to_string())?.len();
    if size > MAX_SOURCE_BYTES {
        return Ok(None);
    }
    let Some(source) = sniff_file(path) else { return Ok(None) };
    let decoded = decode(path, source)?;
    let Some(decoded) = decoded else { return Ok(None) };
    let (width, height) = (decoded.image.width(), decoded.image.height());
    let resizing = settings.max_long_edge.is_some_and(|max| width.max(height) > max);
    let Some(target) = plan(source, decoded.image.color().has_alpha(), settings, resizing) else { return Ok(None) };

    let mut image = decoded.image;
    if let Some(max) = settings.max_long_edge.filter(|_| resizing) {
        image = image.resize(max, max, FilterType::Lanczos3);
    }
    let exif = decoded.exif.as_deref().and_then(|exif| image_metadata::exif_for_reencoded(exif, policy));
    let encoded = encode(&image, target, decoded.icc.as_deref(), exif.as_deref())?;

    let same_format = target.is_format_of(source);
    // Recompressing in the same format that didn't make the file smaller
    // isn't worth the quality it costs.
    if same_format && !resizing && encoded.len() as u64 >= size {
        return Ok(None);
    }
    let extension = if same_format { None } else { Some(target.extension()) };
    let stem = path.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_else(|| "image".into());
    let filename = match (extension, path.extension()) {
        (Some(extension), _) => format!("{stem}.{extension}"),
        (None, Some(original)) => format!("{stem}.{}", original.to_string_lossy()),
        (None, None) => stem,
    };
    let directory = std::env::temp_dir().join("Aktar").join(format!("processed-{}", crate::util::new_id()));
    let output = directory.join(filename);
    let written = std::fs::create_dir_all(&directory).and_then(|()| std::fs::write(&output, &encoded));
    if let Err(error) = written {
        let _ = std::fs::remove_dir_all(&directory);
        return Err(error.to_string());
    }
    Ok(Some(Processed { path: output, new_extension: extension }))
}

/// `filename` with the extension a processed copy got: "photo.png" and
/// "webp" give "photo.webp". A name without an extension gets one.
pub fn renamed(filename: &str, new_extension: &str) -> String {
    let (name, _) = crate::util::split_extension(filename);
    format!("{name}.{new_extension}")
}

/// The file's format from its first bytes, as `image_metadata` does.
fn sniff_file(path: &Path) -> Option<Source> {
    use std::io::Read;
    let mut head = [0u8; 32];
    let mut file = std::fs::File::open(path).ok()?;
    let read = file.read(&mut head).ok()?;
    sniff(&head[..read])
}

fn sniff(head: &[u8]) -> Option<Source> {
    if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(Source::Jpeg);
    }
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        // An animated PNG keeps its first frame only; leave it alone.
        return Some(Source::Png);
    }
    if head.starts_with(b"II*\0") || head.starts_with(b"MM\0*") {
        return Some(Source::Tiff);
    }
    if head.starts_with(b"BM") && head.len() >= 14 {
        return Some(Source::Bmp);
    }
    if head.len() >= 16 && head.starts_with(b"RIFF") && &head[8..12] == b"WEBP" {
        return match &head[12..16] {
            b"VP8 " => Some(Source::Webp { lossless: false }),
            b"VP8L" => Some(Source::Webp { lossless: true }),
            // The extended format: animated ones (flag 0x02) are left
            // alone, like GIFs.
            b"VP8X" if head.len() > 20 && head[20] & 0x02 == 0 => Some(Source::Webp { lossless: false }),
            _ => None,
        };
    }
    if head.len() >= 12 && &head[4..8] == b"ftyp" {
        let brand = &head[8..12];
        if [b"heic", b"heix", b"heim", b"heis", b"mif1"].iter().any(|b| brand == *b) {
            return Some(Source::Heif);
        }
    }
    None
}

struct Decoded {
    /// Upright: the EXIF orientation is already applied.
    image: DynamicImage,
    icc: Option<Vec<u8>>,
    exif: Option<Vec<u8>>,
}

fn decode(path: &Path, source: Source) -> Result<Option<Decoded>, String> {
    if source == Source::Heif {
        return Ok(decode_heif(path));
    }
    let failed = |error: image::ImageError| error.to_string();
    let mut decoder = ImageReader::open(path)
        .map_err(|error| error.to_string())?
        .with_guessed_format()
        .map_err(|error| error.to_string())?
        .into_decoder()
        .map_err(failed)?;
    let (width, height) = decoder.dimensions();
    if !within_pixel_limit(width, height) {
        return Ok(None);
    }
    let icc = decoder.icc_profile().ok().flatten();
    let exif = decoder.exif_metadata().ok().flatten();
    let orientation = decoder.orientation().map_err(failed)?;
    let mut image = DynamicImage::from_decoder(decoder).map_err(failed)?;
    image.apply_orientation(orientation);
    Ok(Some(Decoded { image, icc, exif }))
}

/// HEIC goes through Windows Imaging Component, which decodes it once the
/// "HEIF Image Extensions" from the Microsoft Store are installed (most
/// PCs have them). Without them, or on other systems, the photo is
/// uploaded as it is.
fn decode_heif(path: &Path) -> Option<Decoded> {
    let image = wic::decode(path)?;
    let data = std::fs::read(path).ok()?;
    Some(Decoded { image, icc: image_metadata::heif_icc_profile(&data), exif: image_metadata::heif_exif(&data) })
}

#[cfg(windows)]
mod wic {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use image::{DynamicImage, RgbaImage};
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::GENERIC_READ;
    use windows::Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, GUID_WICPixelFormat32bppRGBA, IWICImagingFactory, WICConvertBitmapSource,
        WICDecodeMetadataCacheOnDemand,
    };
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};

    /// The first image of the file, upright: the HEIF decoder applies the
    /// rotation the file asks for.
    pub fn decode(path: &Path) -> Option<DynamicImage> {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        unsafe {
            // Already initialized on this thread is fine too.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let factory: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
            let decoder = factory
                .CreateDecoderFromFilename(PCWSTR(wide.as_ptr()), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand)
                .ok()?;
            let frame = decoder.GetFrame(0).ok()?;
            let (mut width, mut height) = (0u32, 0u32);
            frame.GetSize(&mut width, &mut height).ok()?;
            if !super::within_pixel_limit(width, height) {
                return None;
            }
            let converted = WICConvertBitmapSource(&GUID_WICPixelFormat32bppRGBA, &frame).ok()?;
            let (mut width, mut height) = (0u32, 0u32);
            converted.GetSize(&mut width, &mut height).ok()?;
            let stride = width.checked_mul(4)?;
            let mut pixels = vec![0u8; (stride as usize).checked_mul(height as usize)?];
            converted.CopyPixels(std::ptr::null(), stride, &mut pixels).ok()?;
            RgbaImage::from_raw(width, height, pixels).map(DynamicImage::ImageRgba8)
        }
    }
}

#[cfg(not(windows))]
mod wic {
    pub fn decode(_path: &std::path::Path) -> Option<image::DynamicImage> {
        None
    }
}

fn encode(image: &DynamicImage, target: Target, icc: Option<&[u8]>, exif: Option<&[u8]>) -> Result<Vec<u8>, String> {
    let failed = |error: image::ImageError| error.to_string();
    let mut output = Vec::new();
    match target {
        Target::Jpeg(quality) => {
            let mut encoder = JpegEncoder::new_with_quality(&mut output, quality);
            attach(&mut encoder, icc, exif);
            DynamicImage::ImageRgb8(image.to_rgb8()).write_with_encoder(encoder).map_err(failed)?;
        }
        Target::Png => {
            let mut encoder = PngEncoder::new_with_quality(&mut output, CompressionType::Best, PngFilter::Adaptive);
            attach(&mut encoder, icc, exif);
            image.write_with_encoder(encoder).map_err(failed)?;
        }
        Target::Tiff => {
            let mut writer = Cursor::new(&mut output);
            let mut encoder = TiffEncoder::new(&mut writer);
            attach(&mut encoder, icc, None);
            image.write_with_encoder(encoder).map_err(failed)?;
        }
        Target::Bmp => {
            let encoder = BmpEncoder::new(&mut output);
            image.write_with_encoder(encoder).map_err(failed)?;
        }
        Target::WebpLossless => {
            let mut encoder = WebPEncoder::new_lossless(&mut output);
            attach(&mut encoder, icc, exif);
            DynamicImage::ImageRgba8(image.to_rgba8()).write_with_encoder(encoder).map_err(failed)?;
        }
        Target::WebpLossy(quality) => {
            let encoded = if image.color().has_alpha() {
                let pixels = image.to_rgba8();
                webp::Encoder::from_rgba(&pixels, image.width(), image.height()).encode(f32::from(quality)).to_vec()
            } else {
                let pixels = image.to_rgb8();
                webp::Encoder::from_rgb(&pixels, image.width(), image.height()).encode(f32::from(quality)).to_vec()
            };
            if encoded.is_empty() {
                return Err("WebP encoding failed".into());
            }
            output = with_webp_metadata(&encoded, image.width(), image.height(), icc, exif).ok_or("WebP encoding failed")?;
        }
        Target::Avif(quality) => {
            // AVIF here can't carry an ICC profile: the pixels are
            // converted to sRGB, which it's tagged as, instead.
            let pixels = in_srgb(image.to_rgba8(), icc);
            let mut encoder = AvifEncoder::new_with_speed_quality(&mut output, AVIF_SPEED, quality);
            if let Some(exif) = exif {
                let _ = encoder.set_exif_metadata(exif.to_vec());
            }
            let image = if image.color().has_alpha() {
                DynamicImage::ImageRgba8(pixels)
            } else {
                DynamicImage::ImageRgb8(DynamicImage::ImageRgba8(pixels).to_rgb8())
            };
            image.write_with_encoder(encoder).map_err(failed)?;
        }
    }
    Ok(output)
}

fn attach(encoder: &mut impl ImageEncoder, icc: Option<&[u8]>, exif: Option<&[u8]>) {
    if let Some(icc) = icc {
        let _ = encoder.set_icc_profile(icc.to_vec());
    }
    if let Some(exif) = exif {
        let _ = encoder.set_exif_metadata(exif.to_vec());
    }
}

/// The pixels converted from `icc` to sRGB. Without a profile, or one
/// that can't be read, they're taken to be sRGB already.
fn in_srgb(mut pixels: image::RgbaImage, icc: Option<&[u8]>) -> image::RgbaImage {
    let Some(profile) = icc.and_then(|icc| moxcms::ColorProfile::new_from_slice(icc).ok()) else { return pixels };
    let srgb = moxcms::ColorProfile::new_srgb();
    let Ok(transform) = profile.create_transform_8bit(moxcms::Layout::Rgba, &srgb, moxcms::Layout::Rgba, Default::default()) else {
        return pixels;
    };
    let source = pixels.as_raw().clone();
    if transform.transform(&source, &mut pixels).is_err() {
        return image::RgbaImage::from_raw(pixels.width(), pixels.height(), source).unwrap_or(pixels);
    }
    pixels
}

/// Adds a color profile and EXIF to a WebP from libwebp, which writes
/// neither: the "extended" format, with a VP8X header first, the ICCP
/// chunk before the image data and the EXIF chunk after it.
fn with_webp_metadata(encoded: &[u8], width: u32, height: u32, icc: Option<&[u8]>, exif: Option<&[u8]>) -> Option<Vec<u8>> {
    if icc.is_none() && exif.is_none() {
        return Some(encoded.to_vec());
    }
    if encoded.len() < 12 || &encoded[..4] != b"RIFF" || &encoded[8..12] != b"WEBP" {
        return None;
    }
    let mut image_chunks: Vec<([u8; 4], &[u8])> = Vec::new();
    let mut at = 12;
    while at + 8 <= encoded.len() {
        let kind: [u8; 4] = encoded[at..at + 4].try_into().ok()?;
        let size = u32::from_le_bytes(encoded[at + 4..at + 8].try_into().ok()?) as usize;
        let body = encoded.get(at + 8..at + 8 + size)?;
        if matches!(&kind, b"ALPH" | b"VP8 " | b"VP8L") {
            image_chunks.push((kind, body));
        }
        at += 8 + size + size % 2;
    }
    if image_chunks.is_empty() {
        return None;
    }
    let has_alpha = image_chunks.iter().any(|(kind, body)| {
        kind == b"ALPH" || (kind == b"VP8L" && body.len() >= 5 && u32::from_le_bytes(body[1..5].try_into().unwrap()) >> 28 & 1 == 1)
    });

    let mut flags = 0u8;
    if icc.is_some() {
        flags |= 0x20;
    }
    if has_alpha {
        flags |= 0x10;
    }
    if exif.is_some() {
        flags |= 0x08;
    }
    let mut header = vec![flags, 0, 0, 0];
    header.extend_from_slice(&(width - 1).to_le_bytes()[..3]);
    header.extend_from_slice(&(height - 1).to_le_bytes()[..3]);

    let mut chunks: Vec<([u8; 4], &[u8])> = vec![(*b"VP8X", &header)];
    if let Some(icc) = icc {
        chunks.push((*b"ICCP", icc));
    }
    chunks.extend(image_chunks);
    if let Some(exif) = exif {
        chunks.push((*b"EXIF", exif));
    }
    let mut body = b"WEBP".to_vec();
    for (kind, data) in chunks {
        body.extend_from_slice(&kind);
        body.extend_from_slice(&u32::try_from(data.len()).ok()?.to_le_bytes());
        body.extend_from_slice(data);
        if data.len() % 2 == 1 {
            body.push(0);
        }
    }
    let mut output = b"RIFF".to_vec();
    output.extend_from_slice(&u32::try_from(body.len()).ok()?.to_le_bytes());
    output.extend_from_slice(&body);
    Some(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

    fn settings(format: ImageFormat, quality: Option<u8>, max_long_edge: Option<u32>) -> ImageProcessing {
        ImageProcessing { format, quality, max_long_edge }
    }

    #[test]
    fn plans_what_each_photo_becomes() {
        let off = settings(ImageFormat::Original, None, None);
        for source in [Source::Jpeg, Source::Png, Source::Heif, Source::Webp { lossless: false }, Source::Tiff, Source::Bmp] {
            assert_eq!(plan(source, false, off, false), None, "{source:?}");
        }
        let webp = settings(ImageFormat::Webp, None, None);
        assert_eq!(plan(Source::Jpeg, false, webp, false), Some(Target::WebpLossy(90)));
        assert_eq!(plan(Source::Png, true, webp, false), Some(Target::WebpLossless));
        assert_eq!(plan(Source::Png, false, webp, false), Some(Target::WebpLossy(90)));
        assert_eq!(plan(Source::Png, true, settings(ImageFormat::Webp, Some(65), None), false), Some(Target::WebpLossy(65)));
        assert_eq!(plan(Source::Heif, false, settings(ImageFormat::Avif, Some(80), None), false), Some(Target::Avif(80)));

        // Keeping the format: lossy ones are recompressed, lossless ones
        // only change when resized.
        let medium = settings(ImageFormat::Original, Some(80), None);
        assert_eq!(plan(Source::Jpeg, false, medium, false), Some(Target::Jpeg(80)));
        assert_eq!(plan(Source::Webp { lossless: false }, false, medium, false), Some(Target::WebpLossy(80)));
        assert_eq!(plan(Source::Heif, false, medium, false), None);
        assert_eq!(plan(Source::Heif, false, medium, true), Some(Target::Jpeg(80)));
        assert_eq!(plan(Source::Png, false, medium, false), None);
        assert_eq!(plan(Source::Png, false, medium, true), Some(Target::Png));
        assert_eq!(plan(Source::Tiff, false, off, true), Some(Target::Tiff));
        assert_eq!(plan(Source::Jpeg, false, off, true), Some(Target::Jpeg(90)));
        assert_eq!(plan(Source::Webp { lossless: true }, true, off, true), Some(Target::WebpLossless));
    }

    #[test]
    fn sniffs_only_still_photos() {
        assert_eq!(sniff(b"GIF89a\x01\x00\x01\x00"), None);
        assert_eq!(sniff(b"<svg xmlns=\"http://www.w3.org/2000/svg\">"), None);
        assert_eq!(sniff(b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n"), None);
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8X\x0a\0\0\0\x02\0\0\0"), None);
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8X\x0a\0\0\0\x10\0\0\0"), Some(Source::Webp { lossless: false }));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8L\x0a\0\0\0"), Some(Source::Webp { lossless: true }));
        let jpeg = std::fs::read(Path::new(FIXTURES).join("location.jpg")).unwrap();
        assert_eq!(sniff(&jpeg[..32]), Some(Source::Jpeg));
    }

    fn sample(name: &str, width: u32, height: u32, alpha: bool) -> PathBuf {
        let directory = std::env::temp_dir().join(format!("aktar-processing-test-{}", crate::util::new_id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(name);
        let image = image::RgbaImage::from_fn(width, height, |x, y| {
            image::Rgba([(x * 255 / width) as u8, (y * 255 / height) as u8, 128, if alpha { (x % 255) as u8 } else { 255 }])
        });
        let image = DynamicImage::ImageRgba8(image);
        if alpha {
            image.save(&path).unwrap();
        } else {
            DynamicImage::ImageRgb8(image.to_rgb8()).save(&path).unwrap();
        }
        path
    }

    fn exif_tags(data: &[u8]) -> Vec<exif::Tag> {
        exif::Reader::new()
            .read_from_container(&mut std::io::Cursor::new(data))
            .map(|read| read.fields().filter(|field| field.ifd_num == exif::In::PRIMARY).map(|field| field.tag).collect())
            .unwrap_or_default()
    }

    #[test]
    fn converts_a_photo_to_webp_without_its_location() {
        let original = Path::new(FIXTURES).join("location.jpg");
        let processed = process(&original, settings(ImageFormat::Webp, Some(80), Some(1024)), ImageMetadataPolicy::RemoveLocation)
            .unwrap()
            .unwrap();
        assert_eq!(processed.new_extension, Some("webp"));
        assert!(processed.path.to_string_lossy().ends_with("location.webp"));
        let data = std::fs::read(&processed.path).unwrap();
        assert_eq!(&data[..4], b"RIFF");
        let decoded = image::load_from_memory(&data).unwrap();
        let source = image::open(&original).unwrap();
        assert!(decoded.width().max(decoded.height()) <= 1024.max(source.width().max(source.height())));
        // The camera's details carry over, the location doesn't.
        let tags = exif_tags(&data);
        assert!(tags.contains(&exif::Tag::Model), "{tags:?}");
        assert!(tags.iter().all(|tag| tag.context() != exif::Context::Gps));
        image_metadata::remove_copy(&processed.path);
    }

    #[test]
    fn writes_no_metadata_for_remove_all() {
        let original = Path::new(FIXTURES).join("location.jpg");
        let processed = process(&original, settings(ImageFormat::Webp, None, None), ImageMetadataPolicy::RemoveAll).unwrap().unwrap();
        assert!(exif_tags(&std::fs::read(&processed.path).unwrap()).is_empty());
        image_metadata::remove_copy(&processed.path);
        let kept = process(&original, settings(ImageFormat::Original, None, Some(1024)), ImageMetadataPolicy::KeepAll).unwrap();
        // Smaller than 1024 already: nothing to do.
        let source = image::open(&original).unwrap();
        if source.width().max(source.height()) <= 1024 {
            assert!(kept.is_none());
        }
    }

    #[test]
    fn resizes_without_upscaling() {
        let png = sample("shot.png", 3000, 1500, true);
        let processed = process(&png, settings(ImageFormat::Original, None, Some(1920)), ImageMetadataPolicy::RemoveLocation)
            .unwrap()
            .unwrap();
        assert_eq!(processed.new_extension, None);
        assert!(processed.path.to_string_lossy().ends_with("shot.png"));
        let resized = image::open(&processed.path).unwrap();
        assert_eq!((resized.width(), resized.height()), (1920, 960));
        assert!(resized.color().has_alpha());
        image_metadata::remove_copy(&processed.path);

        assert!(process(&png, settings(ImageFormat::Original, None, Some(3840)), ImageMetadataPolicy::RemoveLocation).unwrap().is_none());
        // A transparent PNG becomes lossless WebP, keeping its alpha.
        let webp = process(&png, settings(ImageFormat::Webp, None, Some(3840)), ImageMetadataPolicy::RemoveLocation).unwrap().unwrap();
        let data = std::fs::read(&webp.path).unwrap();
        assert_eq!(&data[12..16], b"VP8L");
        let decoded = image::load_from_memory(&data).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (3000, 1500));
        assert!(decoded.color().has_alpha());
        image_metadata::remove_copy(&webp.path);
        std::fs::remove_dir_all(png.parent().unwrap()).unwrap();
    }

    #[test]
    fn encodes_avif() {
        let jpeg = sample("photo.jpg", 640, 480, false);
        let processed = process(&jpeg, settings(ImageFormat::Avif, Some(65), None), ImageMetadataPolicy::RemoveLocation).unwrap().unwrap();
        assert_eq!(processed.new_extension, Some("avif"));
        let data = std::fs::read(&processed.path).unwrap();
        assert_eq!(&data[4..12], b"ftypavif");
        image_metadata::remove_copy(&processed.path);
        std::fs::remove_dir_all(jpeg.parent().unwrap()).unwrap();
    }

    #[test]
    fn adds_metadata_to_lossy_webp() {
        let image = DynamicImage::ImageRgba8(image::RgbaImage::from_fn(33, 17, |x, _| image::Rgba([x as u8, 0, 0, 128])));
        let icc = moxcms::ColorProfile::new_display_p3().encode().unwrap();
        let exif = image_metadata::exif_for_reencoded(
            &exif_block(),
            ImageMetadataPolicy::KeepAll,
        )
        .unwrap();
        let encoded = encode(&image, Target::WebpLossy(80), Some(&icc), Some(&exif)).unwrap();
        assert_eq!(&encoded[12..16], b"VP8X");
        let mut decoder = image::codecs::webp::WebPDecoder::new(Cursor::new(&encoded)).unwrap();
        assert_eq!(decoder.dimensions(), (33, 17));
        assert_eq!(decoder.icc_profile().unwrap(), Some(icc));
        assert_eq!(decoder.exif_metadata().unwrap(), Some(exif));
        assert!(decoder.color_type().has_alpha());
    }

    /// The EXIF of the JPEG fixture, as a TIFF structure.
    fn exif_block() -> Vec<u8> {
        let data = std::fs::read(Path::new(FIXTURES).join("location.jpg")).unwrap();
        exif::Reader::new().read_from_container(&mut Cursor::new(&data)).unwrap().buf().to_vec()
    }

    /// A BMP header claiming `width` x `height` pixels, without the pixels.
    fn bmp_header(width: u32, height: u32) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(b"BM");
        data.extend_from_slice(&54u32.to_le_bytes()); // file size
        data.extend_from_slice(&0u32.to_le_bytes()); // reserved
        data.extend_from_slice(&54u32.to_le_bytes()); // pixel data offset
        data.extend_from_slice(&40u32.to_le_bytes()); // BITMAPINFOHEADER
        data.extend_from_slice(&width.to_le_bytes());
        data.extend_from_slice(&height.to_le_bytes());
        data.extend_from_slice(&1u16.to_le_bytes()); // planes
        data.extend_from_slice(&24u16.to_le_bytes()); // bits per pixel
        data.extend_from_slice(&[0; 24]); // no compression, sizes, palette
        data
    }

    #[test]
    fn leaves_huge_images_alone() {
        assert!(within_pixel_limit(10_000, 10_000));
        assert!(!within_pixel_limit(10_000, 10_001));
        assert!(!within_pixel_limit(u32::MAX, u32::MAX));

        // 40,000 x 40,000 in 54 bytes: never decoded, uploaded as it is.
        let directory = std::env::temp_dir().join(format!("aktar-processing-test-{}", crate::util::new_id()));
        std::fs::create_dir_all(&directory).unwrap();
        let bomb = directory.join("bomb.bmp");
        std::fs::write(&bomb, bmp_header(40_000, 40_000)).unwrap();
        let result = process(&bomb, settings(ImageFormat::Webp, Some(80), Some(1024)), ImageMetadataPolicy::RemoveAll);
        assert!(matches!(result, Ok(None)), "{result:?}");
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn renames_converted_files() {
        assert_eq!(renamed("photo.png", "webp"), "photo.webp");
        assert_eq!(renamed("my.photo.HEIC", "jpg"), "my.photo.jpg");
        assert_eq!(renamed("clipboard", "avif"), "clipboard.avif");
    }
}

