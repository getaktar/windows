//! Removes the location, or all metadata, from photos before they're
//! uploaded (the destination's "Image metadata" setting), like the Mac
//! app's ImageMetadataStripper. Covers JPEG, HEIC/HEIF, PNG and TIFF.
//!
//! The pixels are never decoded or re-encoded. JPEG, HEIC and TIFF are
//! edited in place, without changing the size of anything, so every offset
//! in the file (an iPhone photo's HDR gain map, HEIC's item locations) stays
//! valid: removed EXIF entries and their values are zeroed, XMP properties
//! are blanked with spaces, and other metadata blocks are zeroed. PNG
//! chunks carry no offsets, so they're simply dropped or rewritten.
//!
//! The result is checked before it's used: if anything that should be gone
//! is still there, the upload stops instead of sharing where a photo was
//! taken.

use std::path::{Path, PathBuf};

use crate::destinations::ImageMetadataPolicy;
use crate::t;

#[derive(Debug, Clone, thiserror::Error)]
pub enum MetadataError {
    /// Uploading anyway could share where the photo was taken.
    #[error("{}", t!("Couldn't remove the metadata from {0}, so it wasn't uploaded. Set Image metadata to Keep all for this destination to upload it as it is.", .0))]
    CouldNotRewrite(String),
}

/// A copy of `path` without the metadata `policy` removes, or None when
/// the original can be uploaded as it is: not a photo format this handles,
/// or nothing in it to remove. The caller deletes the copy with
/// `remove_copy` once it's done with it.
pub fn stripped_copy(path: &Path, policy: ImageMetadataPolicy) -> Result<Option<PathBuf>, MetadataError> {
    if policy == ImageMetadataPolicy::KeepAll || !path.is_file() {
        return Ok(None);
    }
    let filename = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    let failed = || MetadataError::CouldNotRewrite(filename.clone());
    let Some(format) = sniff_file(path) else { return Ok(None) };

    let mut data = std::fs::read(path).map_err(|_| failed())?;
    let changed = strip(&mut data, format, policy).map_err(|_| failed())?;
    if !changed {
        return Ok(None);
    }
    if still_has_metadata_to_remove(&data, format, policy) {
        return Err(failed());
    }

    let directory = std::env::temp_dir().join("Aktar").join(format!("metadata-{}", crate::util::new_id()));
    let output = directory.join(&filename);
    let written = std::fs::create_dir_all(&directory).and_then(|()| std::fs::write(&output, &data));
    if written.is_err() {
        let _ = std::fs::remove_dir_all(&directory);
        return Err(failed());
    }
    Ok(Some(output))
}

/// Deletes a copy made by `stripped_copy`.
pub fn remove_copy(path: &Path) {
    if let Some(directory) = path.parent() {
        let _ = std::fs::remove_dir_all(directory);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Jpeg,
    Png,
    Tiff,
    Heif,
}

/// The file's format from its first bytes, so a photo named ".bin" is still
/// cleaned and a text file named ".jpg" isn't read whole.
fn sniff_file(path: &Path) -> Option<Format> {
    use std::io::Read;
    let mut head = [0u8; 16];
    let mut file = std::fs::File::open(path).ok()?;
    let read = file.read(&mut head).ok()?;
    sniff(&head[..read])
}

fn sniff(head: &[u8]) -> Option<Format> {
    if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(Format::Jpeg);
    }
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(Format::Png);
    }
    // Classic TIFF only; BigTIFF ("II+") isn't a photo format anyone shares.
    if head.starts_with(b"II*\0") || head.starts_with(b"MM\0*") {
        return Some(Format::Tiff);
    }
    // HEIC and HEIF by the major brand. AVIF is left alone, as on the Mac.
    if head.len() >= 12 && &head[4..8] == b"ftyp" {
        let brand = &head[8..12];
        if [b"heic", b"heix", b"heim", b"heis", b"hevc", b"hevx", b"mif1", b"msf1"].iter().any(|b| brand == *b) {
            return Some(Format::Heif);
        }
    }
    None
}

/// Something in the file couldn't be followed, so it can't be vouched for.
#[derive(Debug)]
struct Malformed;

type Parsed<T> = std::result::Result<T, Malformed>;

/// Removes what `policy` says from `data`. Returns whether anything changed.
fn strip(data: &mut Vec<u8>, format: Format, policy: ImageMetadataPolicy) -> Parsed<bool> {
    match format {
        Format::Jpeg => strip_jpeg(data, 0, policy),
        Format::Tiff => {
            let mut tiff = Tiff::new(data).ok_or(Malformed)?;
            tiff.strip(policy, true)
        }
        Format::Heif => strip_heif(data, policy),
        Format::Png => strip_png(data, policy),
    }
}

/// Whether stripping again would still find something, plus an independent
/// read of the EXIF with the `exif` crate for the location.
fn still_has_metadata_to_remove(data: &[u8], format: Format, policy: ImageMetadataPolicy) -> bool {
    let mut again = data.to_vec();
    if !matches!(strip(&mut again, format, policy), Ok(false)) {
        return true;
    }
    match exif::Reader::new().read_from_container(&mut std::io::Cursor::new(data)) {
        Ok(read) => read.fields().any(|field| {
            field.ifd_num == exif::In::PRIMARY
                && (field.tag.context() == exif::Context::Gps
                    || (policy == ImageMetadataPolicy::RemoveAll && IDENTIFYING_EXIF.contains(&field.tag)))
        }),
        // No EXIF at all is fine; one that can't be read isn't.
        Err(exif::Error::NotFound(_)) => false,
        Err(_) => format != Format::Png && format != Format::Heif,
    }
}

/// What identifies the camera, the person or the time, for the check
/// after "Remove all".
const IDENTIFYING_EXIF: [exif::Tag; 10] = [
    exif::Tag::Make,
    exif::Tag::Model,
    exif::Tag::Software,
    exif::Tag::DateTime,
    exif::Tag::Artist,
    exif::Tag::DateTimeOriginal,
    exif::Tag::DateTimeDigitized,
    exif::Tag::LensModel,
    exif::Tag::BodySerialNumber,
    exif::Tag::CameraOwnerName,
];

// MARK: - TIFF (EXIF)

const TAG_ORIENTATION: u16 = 0x0112;
const TAG_EXIF_IFD: u16 = 0x8769;
const TAG_GPS_IFD: u16 = 0x8825;
const TAG_INTEROP_IFD: u16 = 0xA005;
const TAG_XMP: u16 = 700;
const TAG_THUMBNAIL_OFFSET: u16 = 0x0201;
const TAG_THUMBNAIL_LENGTH: u16 = 0x0202;

/// What a TIFF image needs to be displayed at all, kept by "Remove all" in
/// a TIFF file: dimensions, strips and tiles, color, resolution, and the
/// ICC profile, as ImageIO keeps them.
const TIFF_STRUCTURE: [u16; 36] = [
    254, 255, 256, 257, 258, 259, 262, 263, 266, 273, 274, 277, 278, 279, 280, 281, 282, 283, 284, 290, 291, 296,
    317, 318, 319, 320, 322, 323, 324, 325, 330, 338, 339, 340, 341, 34675,
];

/// A TIFF structure: a whole TIFF file, or the EXIF block inside a JPEG,
/// HEIC or PNG. Offsets are from its first byte, and nothing is ever
/// moved, so the rest of the file doesn't notice.
struct Tiff<'a> {
    data: &'a mut [u8],
    big_endian: bool,
}

#[derive(Clone, Copy)]
struct Entry {
    /// Where the 12-byte entry is.
    at: usize,
    tag: u16,
    kind: u16,
    count: u32,
}

impl<'a> Tiff<'a> {
    fn new(data: &'a mut [u8]) -> Option<Self> {
        let big_endian = match data.get(..4)? {
            b"II*\0" => false,
            b"MM\0*" => true,
            _ => return None,
        };
        Some(Self { data, big_endian })
    }

    fn u16_at(&self, at: usize) -> Parsed<u16> {
        let bytes: [u8; 2] = self.data.get(at..at + 2).ok_or(Malformed)?.try_into().unwrap();
        Ok(if self.big_endian { u16::from_be_bytes(bytes) } else { u16::from_le_bytes(bytes) })
    }

    fn u32_at(&self, at: usize) -> Parsed<u32> {
        let bytes: [u8; 4] = self.data.get(at..at + 4).ok_or(Malformed)?.try_into().unwrap();
        Ok(if self.big_endian { u32::from_be_bytes(bytes) } else { u32::from_le_bytes(bytes) })
    }

    fn put_u16(&mut self, at: usize, value: u16) {
        let bytes = if self.big_endian { value.to_be_bytes() } else { value.to_le_bytes() };
        self.data[at..at + 2].copy_from_slice(&bytes);
    }

    fn put_u32(&mut self, at: usize, value: u32) {
        let bytes = if self.big_endian { value.to_be_bytes() } else { value.to_le_bytes() };
        self.data[at..at + 4].copy_from_slice(&bytes);
    }

    fn zero(&mut self, range: std::ops::Range<usize>) -> bool {
        let end = range.end.min(self.data.len());
        let start = range.start.min(end);
        let slice = &mut self.data[start..end];
        let changed = slice.iter().any(|byte| *byte != 0);
        slice.fill(0);
        changed
    }

    /// The entries of the IFD at `offset`, and where its next-IFD offset is.
    fn entries(&self, offset: usize) -> Parsed<(Vec<Entry>, usize)> {
        let count = self.u16_at(offset)? as usize;
        let mut entries = Vec::with_capacity(count);
        for index in 0..count {
            let at = offset + 2 + index * 12;
            entries.push(Entry { at, tag: self.u16_at(at)?, kind: self.u16_at(at + 2)?, count: self.u32_at(at + 4)? });
        }
        let next = offset + 2 + count * 12;
        self.u32_at(next)?;
        Ok((entries, next))
    }

    /// Where an entry's value is, when it doesn't fit in the entry itself.
    fn value_range(&self, entry: Entry) -> Parsed<Option<std::ops::Range<usize>>> {
        let size = match entry.kind {
            1 | 2 | 6 | 7 => 1usize,
            3 | 8 => 2,
            4 | 9 | 11 | 13 => 4,
            5 | 10 | 12 => 8,
            _ => return Ok(None),
        };
        let length = size.checked_mul(entry.count as usize).ok_or(Malformed)?;
        if length <= 4 {
            return Ok(None);
        }
        let start = self.u32_at(entry.at + 8)? as usize;
        let end = start.checked_add(length).ok_or(Malformed)?;
        if end > self.data.len() {
            return Err(Malformed);
        }
        Ok(Some(start..end))
    }

    /// The IFD offsets in IFD0's chain (IFD0, the thumbnail's IFD1...).
    fn chain(&self) -> Parsed<Vec<usize>> {
        let mut offsets = Vec::new();
        let mut offset = self.u32_at(4)? as usize;
        while offset != 0 {
            if offsets.contains(&offset) || offsets.len() > 16 {
                return Err(Malformed);
            }
            offsets.push(offset);
            let (_, next) = self.entries(offset)?;
            offset = self.u32_at(next)? as usize;
        }
        Ok(offsets)
    }

    /// Takes the IFD's `index`th entry out: the entries after it move up
    /// (they're in the same 12-byte table), and its value is zeroed.
    fn remove_entry(&mut self, offset: usize, index: usize) -> Parsed<()> {
        let (entries, next) = self.entries(offset)?;
        let entry = entries[index];
        if let Some(range) = self.value_range(entry)? {
            self.zero(range);
        }
        let after = entry.at + 12;
        let end = next + 4;
        self.data.copy_within(after..end, entry.at);
        self.zero(end - 12..end);
        let count = entries.len() as u16 - 1;
        self.put_u16(offset, count);
        Ok(())
    }

    /// Zeroes a whole IFD, the values of its entries, and the IFDs they
    /// point to (EXIF's Interop IFD), then marks it empty.
    fn erase_ifd(&mut self, offset: usize, depth: usize) -> Parsed<()> {
        if depth > 4 {
            return Err(Malformed);
        }
        let (entries, next) = self.entries(offset)?;
        for entry in &entries {
            if matches!(entry.tag, TAG_EXIF_IFD | TAG_GPS_IFD | TAG_INTEROP_IFD) {
                let child = self.u32_at(entry.at + 8)? as usize;
                if child != 0 && child != offset {
                    self.erase_ifd(child, depth + 1)?;
                }
            }
            if let Some(range) = self.value_range(*entry)? {
                self.zero(range);
            }
        }
        // An embedded thumbnail (IFD1) is a whole JPEG of its own.
        let thumbnail = entries.iter().find(|entry| entry.tag == TAG_THUMBNAIL_OFFSET).copied();
        let length = entries.iter().find(|entry| entry.tag == TAG_THUMBNAIL_LENGTH).copied();
        if let (Some(thumbnail), Some(length)) = (thumbnail, length) {
            let start = self.u32_at(thumbnail.at + 8)? as usize;
            let length = self.u32_at(length.at + 8)? as usize;
            self.zero(start..start.saturating_add(length));
        }
        self.zero(offset..next + 4);
        Ok(())
    }

    /// Removes what `policy` says. `whole_file` is a TIFF image, whose IFDs
    /// also describe the pixels; otherwise this is an EXIF block, where
    /// "Remove all" keeps only the orientation.
    fn strip(&mut self, policy: ImageMetadataPolicy, whole_file: bool) -> Parsed<bool> {
        let mut changed = false;
        let chain = self.chain()?;
        match policy {
            ImageMetadataPolicy::KeepAll => {}
            ImageMetadataPolicy::RemoveLocation => {
                for offset in chain {
                    let (entries, _) = self.entries(offset)?;
                    if let Some(index) = entries.iter().position(|entry| entry.tag == TAG_GPS_IFD) {
                        let gps = self.u32_at(entries[index].at + 8)? as usize;
                        if gps != 0 {
                            self.erase_ifd(gps, 1)?;
                        }
                        self.remove_entry(offset, index)?;
                        changed = true;
                    }
                    // XMP inside a TIFF file can repeat the location.
                    if let Some(entry) = entries.iter().find(|entry| entry.tag == TAG_XMP) {
                        if let Some(range) = self.value_range(*entry)? {
                            changed |= blank_xmp_location(&mut self.data[range]);
                        }
                    }
                }
            }
            ImageMetadataPolicy::RemoveAll => {
                for (position, offset) in chain.iter().copied().enumerate() {
                    // Past IFD0, an EXIF block only has the thumbnail.
                    if !whole_file && position > 0 {
                        break;
                    }
                    let keep = |tag: u16| if whole_file { TIFF_STRUCTURE.contains(&tag) } else { tag == TAG_ORIENTATION };
                    let mut index = 0;
                    loop {
                        let (entries, _) = self.entries(offset)?;
                        let Some(entry) = entries.get(index).copied() else { break };
                        if keep(entry.tag) {
                            index += 1;
                            continue;
                        }
                        if matches!(entry.tag, TAG_EXIF_IFD | TAG_GPS_IFD | TAG_INTEROP_IFD) {
                            let child = self.u32_at(entry.at + 8)? as usize;
                            if child != 0 && child != offset {
                                self.erase_ifd(child, 1)?;
                            }
                        }
                        self.remove_entry(offset, index)?;
                        changed = true;
                    }
                }
                if !whole_file {
                    let first = chain.first().copied().ok_or(Malformed)?;
                    let (_, next) = self.entries(first)?;
                    let thumbnail = self.u32_at(next)? as usize;
                    if thumbnail != 0 {
                        self.erase_ifd(thumbnail, 1)?;
                        self.put_u32(next, 0);
                        changed = true;
                    }
                }
            }
        }
        Ok(changed)
    }
}

// MARK: - XMP

/// Blanks every `exif:GPS…` property in an XMP packet with spaces, as an
/// attribute (`exif:GPSLatitude="41,0.49N"`) or an element with whatever
/// it holds. XML ignores that whitespace, and the packet keeps its size.
fn blank_xmp_location(xmp: &mut [u8]) -> bool {
    let mut changed = false;
    let mut from = 0;
    while let Some(found) = find(&xmp[from..], b":GPS") {
        let colon = from + found;
        let mut start = colon;
        while start > 0 && is_name_byte(xmp[start - 1]) {
            start -= 1;
        }
        let mut name_end = colon + 1;
        while name_end < xmp.len() && is_name_byte(xmp[name_end]) {
            name_end += 1;
        }
        let name = xmp[start..name_end].to_vec();
        let end = if start > 0 && xmp[start - 1] == b'<' {
            element_end(xmp, start - 1, &name)
        } else if start > 0 && xmp[start - 1] == b'/' {
            // A closing tag whose opening one was already blanked.
            None
        } else {
            attribute_end(xmp, name_end)
        };
        match end {
            Some(end) => {
                let start = if xmp.get(start.wrapping_sub(1)) == Some(&b'<') { start - 1 } else { start };
                xmp[start..end].fill(b' ');
                changed = true;
                from = end;
            }
            None => from = name_end,
        }
    }
    changed
}

fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':')
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

/// `name="value"` or `name='value'`: the end of the closing quote.
fn attribute_end(xmp: &[u8], name_end: usize) -> Option<usize> {
    let mut at = name_end;
    while xmp.get(at)?.is_ascii_whitespace() {
        at += 1;
    }
    if *xmp.get(at)? != b'=' {
        return None;
    }
    at += 1;
    while xmp.get(at)?.is_ascii_whitespace() {
        at += 1;
    }
    let quote = *xmp.get(at)?;
    if quote != b'"' && quote != b'\'' {
        return None;
    }
    let close = xmp[at + 1..].iter().position(|byte| *byte == quote)?;
    Some(at + 1 + close + 1)
}

/// `<name …/>` or `<name …>…</name>`: the end of the element.
fn element_end(xmp: &[u8], open: usize, name: &[u8]) -> Option<usize> {
    let tag_end = open + xmp[open..].iter().position(|byte| *byte == b'>')? + 1;
    if xmp[tag_end - 2] == b'/' {
        return Some(tag_end);
    }
    let mut closing = b"</".to_vec();
    closing.extend_from_slice(name);
    let close = tag_end + find(&xmp[tag_end..], &closing)?;
    let end = close + xmp[close..].iter().position(|byte| *byte == b'>')? + 1;
    Some(end)
}

fn has_xmp_location(xmp: &[u8]) -> bool {
    blank_xmp_location(&mut xmp.to_vec())
}

/// Replaces an XMP packet with an empty one padded to the same size, as
/// the XMP spec allows, or with spaces when it's too small for one.
fn empty_xmp(xmp: &mut [u8]) -> bool {
    const EMPTY: &[u8] = b"<?xpacket begin=\"\xEF\xBB\xBF\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?><x:xmpmeta xmlns:x=\"adobe:ns:meta/\"></x:xmpmeta>";
    const END: &[u8] = b"<?xpacket end=\"w\"?>";
    let mut replacement = vec![b' '; xmp.len()];
    if xmp.len() >= EMPTY.len() + END.len() {
        replacement[..EMPTY.len()].copy_from_slice(EMPTY);
        let at = xmp.len() - END.len();
        replacement[at..].copy_from_slice(END);
    }
    if xmp == replacement.as_slice() {
        return false;
    }
    xmp.copy_from_slice(&replacement);
    true
}

// MARK: - JPEG

const XMP_HEADER: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
const XMP_EXTENSION_HEADER: &[u8] = b"http://ns.adobe.com/xmp/extension/\0";
const EXIF_HEADER: &[u8] = b"Exif\0\0";

/// Cleans the JPEG starting at `start`, then any JPEG after its end: the
/// secondary images of an iPhone photo (its HDR gain map) or a camera's
/// preview, which can carry their own EXIF.
fn strip_jpeg(data: &mut [u8], start: usize, policy: ImageMetadataPolicy) -> Parsed<bool> {
    let mut changed = false;
    let mut at = start;
    let mut images = 0;
    loop {
        let (image_changed, end) = strip_one_jpeg(data, at, policy)?;
        changed |= image_changed;
        images += 1;
        match find(&data[end..], &[0xFF, 0xD8, 0xFF]) {
            Some(next) if images < 16 => at = end + next,
            _ => return Ok(changed),
        }
    }
}

/// Returns whether anything changed, and where the image ends.
fn strip_one_jpeg(data: &mut [u8], start: usize, policy: ImageMetadataPolicy) -> Parsed<(bool, usize)> {
    if data.get(start..start + 2) != Some(&[0xFF, 0xD8]) {
        return Err(Malformed);
    }
    let mut changed = false;
    let mut at = start + 2;
    loop {
        // Fill bytes before a marker.
        while data.get(at) == Some(&0xFF) && data.get(at + 1) == Some(&0xFF) {
            at += 1;
        }
        if data.get(at) != Some(&0xFF) {
            return Err(Malformed);
        }
        let marker = *data.get(at + 1).ok_or(Malformed)?;
        match marker {
            0xD9 => return Ok((changed, at + 2)),
            0x01 | 0xD0..=0xD7 => {
                at += 2;
                continue;
            }
            _ => {}
        }
        let length = u16::from_be_bytes([*data.get(at + 2).ok_or(Malformed)?, *data.get(at + 3).ok_or(Malformed)?]) as usize;
        if length < 2 || at + 2 + length > data.len() {
            return Err(Malformed);
        }
        let payload = at + 4..at + 2 + length;
        changed |= strip_segment(data, at + 1, payload, policy)?;
        at += 2 + length;
        if marker == 0xDA {
            at = skip_scan(data, at)?;
        }
    }
}

/// Past the entropy-coded data after a start of scan, to the next marker.
fn skip_scan(data: &[u8], mut at: usize) -> Parsed<usize> {
    loop {
        let found = data[at..].iter().position(|byte| *byte == 0xFF).ok_or(Malformed)?;
        at += found;
        match data.get(at + 1) {
            None => return Err(Malformed),
            Some(0x00) | Some(0xD0..=0xD7) | Some(0xFF) => at += 1,
            Some(_) => return Ok(at),
        }
    }
}

/// One marker segment. `marker_at` is the marker byte, which a removed
/// block turns into a comment (COM) whose bytes are zeroed.
fn strip_segment(data: &mut [u8], marker_at: usize, payload: std::ops::Range<usize>, policy: ImageMetadataPolicy) -> Parsed<bool> {
    let marker = data[marker_at];
    let body = &data[payload.clone()];
    let remove_all = policy == ImageMetadataPolicy::RemoveAll;
    match marker {
        0xE1 if body.starts_with(EXIF_HEADER) => {
            let mut tiff = Tiff::new(&mut data[payload.start + EXIF_HEADER.len()..payload.end]).ok_or(Malformed)?;
            tiff.strip(policy, false)
        }
        0xE1 if body.starts_with(XMP_HEADER) => {
            let xmp = &mut data[payload.start + XMP_HEADER.len()..payload.end];
            Ok(if remove_all { empty_xmp(xmp) } else { blank_xmp_location(xmp) })
        }
        // Extended XMP is checked against an MD5 of the whole, so it can't
        // be edited; it goes when it has anything to remove.
        0xE1 if body.starts_with(XMP_EXTENSION_HEADER) => {
            let xmp = &data[payload.start + XMP_EXTENSION_HEADER.len()..payload.end];
            Ok((remove_all || has_xmp_location(xmp)) && erase_segment(data, marker_at, payload))
        }
        // IPTC and Photoshop resources, Ducky, other APP1 blocks, comments.
        0xED | 0xEC | 0xE1 | 0xFE if remove_all => Ok(erase_segment(data, marker_at, payload)),
        _ => Ok(false),
    }
}

fn erase_segment(data: &mut [u8], marker_at: usize, payload: std::ops::Range<usize>) -> bool {
    let changed = data[marker_at] != 0xFE || data[payload.clone()].iter().any(|byte| *byte != 0);
    data[payload].fill(0);
    data[marker_at] = 0xFE;
    changed
}

// MARK: - HEIF

/// A box in an ISO base media file: its type, and where its content is.
struct IsoBox {
    kind: [u8; 4],
    content: std::ops::Range<usize>,
}

fn boxes(data: &[u8], range: std::ops::Range<usize>) -> Parsed<Vec<IsoBox>> {
    let mut found = Vec::new();
    let mut at = range.start;
    while at + 8 <= range.end {
        let size = u32::from_be_bytes(data[at..at + 4].try_into().unwrap()) as u64;
        let kind: [u8; 4] = data[at + 4..at + 8].try_into().unwrap();
        let (header, size) = match size {
            1 => (16, u64::from_be_bytes(data.get(at + 8..at + 16).ok_or(Malformed)?.try_into().unwrap())),
            0 => (8, (range.end - at) as u64),
            size => (8, size),
        };
        let end = at.checked_add(usize::try_from(size).map_err(|_| Malformed)?).ok_or(Malformed)?;
        if size < header || end > range.end {
            return Err(Malformed);
        }
        found.push(IsoBox { kind, content: at + header as usize..end });
        at = end;
    }
    Ok(found)
}

/// A cursor over big-endian box fields.
struct Fields<'a> {
    data: &'a [u8],
    at: usize,
}

impl Fields<'_> {
    fn take(&mut self, size: usize) -> Parsed<u64> {
        let bytes = self.data.get(self.at..self.at + size).ok_or(Malformed)?;
        self.at += size;
        Ok(bytes.iter().fold(0u64, |value, byte| value << 8 | *byte as u64))
    }

    fn string(&mut self) -> Parsed<Vec<u8>> {
        let length = self.data.get(self.at..).ok_or(Malformed)?.iter().position(|byte| *byte == 0).ok_or(Malformed)?;
        let text = self.data[self.at..self.at + length].to_vec();
        self.at += length + 1;
        Ok(text)
    }
}

enum HeifItem {
    Exif,
    Xmp,
}

/// The EXIF and XMP items in the `meta` box, each as the pieces of the
/// file it's stored in (almost always one).
fn heif_metadata_items(data: &[u8]) -> Parsed<Vec<(HeifItem, Vec<std::ops::Range<usize>>)>> {
    let top = boxes(data, 0..data.len())?;
    let Some(meta) = top.iter().find(|b| &b.kind == b"meta") else { return Ok(Vec::new()) };
    // meta is a full box: version and flags come first.
    let children = boxes(data, meta.content.start + 4..meta.content.end)?;

    let mut kinds: Vec<(u64, HeifItem)> = Vec::new();
    if let Some(iinf) = children.iter().find(|b| &b.kind == b"iinf") {
        let version = data[iinf.content.start];
        let entries_at = iinf.content.start + 4 + if version == 0 { 2 } else { 4 };
        for infe in boxes(data, entries_at..iinf.content.end)?.iter().filter(|b| &b.kind == b"infe") {
            let mut fields = Fields { data: &data[..infe.content.end], at: infe.content.start };
            let version = fields.take(1)?;
            fields.take(3)?;
            if version < 2 {
                continue;
            }
            let id = fields.take(if version == 2 { 2 } else { 4 })?;
            fields.take(2)?;
            let item_type: [u8; 4] = (fields.take(4)? as u32).to_be_bytes();
            // Some encoders leave the name out entirely.
            let _ = fields.string();
            match &item_type {
                b"Exif" => kinds.push((id, HeifItem::Exif)),
                b"mime" if fields.string().is_ok_and(|kind| kind == b"application/rdf+xml") => kinds.push((id, HeifItem::Xmp)),
                _ => {}
            }
        }
    }
    if kinds.is_empty() {
        return Ok(Vec::new());
    }

    let iloc = children.iter().find(|b| &b.kind == b"iloc").ok_or(Malformed)?;
    let idat = children.iter().find(|b| &b.kind == b"idat").map(|b| b.content.start);
    let mut fields = Fields { data: &data[..iloc.content.end], at: iloc.content.start };
    let version = fields.take(1)?;
    fields.take(3)?;
    let sizes = fields.take(2)?;
    let (offset_size, length_size) = ((sizes >> 12) as usize, (sizes >> 8 & 0xF) as usize);
    let (base_offset_size, index_size) = ((sizes >> 4 & 0xF) as usize, (sizes & 0xF) as usize);
    let item_count = fields.take(if version < 2 { 2 } else { 4 })?;
    let mut located = Vec::new();
    for _ in 0..item_count {
        let id = fields.take(if version < 2 { 2 } else { 4 })?;
        let method = if version >= 1 { fields.take(2)? & 0xF } else { 0 };
        fields.take(2)?;
        let base = fields.take(base_offset_size)?;
        let extent_count = fields.take(2)?;
        let mut extents = Vec::new();
        for _ in 0..extent_count {
            if version >= 1 && index_size > 0 {
                fields.take(index_size)?;
            }
            let offset = fields.take(offset_size)?;
            let length = fields.take(length_size)?;
            let origin = match method {
                0 => 0,
                1 => idat.ok_or(Malformed)? as u64,
                _ => return Err(Malformed),
            };
            let start = usize::try_from(origin + base + offset).map_err(|_| Malformed)?;
            let end = start.checked_add(usize::try_from(length).map_err(|_| Malformed)?).ok_or(Malformed)?;
            if end > data.len() {
                return Err(Malformed);
            }
            extents.push(start..end);
        }
        if let Some(index) = kinds.iter().position(|(kind_id, _)| *kind_id == id) {
            let (_, kind) = kinds.remove(index);
            located.push((kind, extents));
        }
    }
    Ok(located)
}

fn strip_heif(data: &mut [u8], policy: ImageMetadataPolicy) -> Parsed<bool> {
    let mut changed = false;
    for (kind, extents) in heif_metadata_items(data)? {
        // Gathered into one piece to edit, then put back where it was.
        let mut item: Vec<u8> = extents.iter().flat_map(|range| data[range.clone()].to_vec()).collect();
        let item_changed = match kind {
            HeifItem::Exif => {
                // The EXIF item starts with the offset of its TIFF header,
                // past an "Exif\0\0" prefix.
                let skip = u32::from_be_bytes(item.get(..4).ok_or(Malformed)?.try_into().unwrap()) as usize;
                let tiff = item.get_mut(4 + skip..).ok_or(Malformed)?;
                Tiff::new(tiff).ok_or(Malformed)?.strip(policy, false)?
            }
            HeifItem::Xmp if policy == ImageMetadataPolicy::RemoveAll => empty_xmp(&mut item),
            HeifItem::Xmp => blank_xmp_location(&mut item),
        };
        if item_changed {
            let mut at = 0;
            for range in extents {
                let length = range.len();
                data[range].copy_from_slice(&item[at..at + length]);
                at += length;
            }
            changed = true;
        }
    }
    Ok(changed)
}

// MARK: - PNG

fn strip_png(data: &mut Vec<u8>, policy: ImageMetadataPolicy) -> Parsed<bool> {
    const SIGNATURE: usize = 8;
    let mut output = data[..SIGNATURE].to_vec();
    let mut changed = false;
    let mut at = SIGNATURE;
    while at < data.len() {
        let length = u32::from_be_bytes(data.get(at..at + 4).ok_or(Malformed)?.try_into().unwrap()) as usize;
        let end = at.checked_add(12 + length).ok_or(Malformed)?;
        if end > data.len() {
            return Err(Malformed);
        }
        let kind: [u8; 4] = data[at + 4..at + 8].try_into().unwrap();
        let mut body = data[at + 8..at + 8 + length].to_vec();
        let keyword = body.split(|byte| *byte == 0).next().unwrap_or_default().to_vec();
        let text = matches!(&kind, b"tEXt" | b"iTXt" | b"zTXt");
        let keep = match (&kind, policy) {
            (b"eXIf", _) => {
                let mut tiff = Tiff::new(&mut body).ok_or(Malformed)?;
                changed |= tiff.strip(policy, false)?;
                policy != ImageMetadataPolicy::RemoveAll || tiff_has_orientation(&body)
            }
            (_, ImageMetadataPolicy::RemoveAll) if text || &kind == b"tIME" => false,
            // Uncompressed XMP is cleaned in place; compressed XMP and
            // ImageMagick's hex-encoded EXIF can't be read here, so they go.
            (b"iTXt", _) if keyword == b"XML:com.adobe.xmp" => match itxt_uncompressed_text(&body) {
                Some(text_at) => {
                    changed |= blank_xmp_location(&mut body[text_at..]);
                    true
                }
                None => false,
            },
            (_, _) if text && keyword.starts_with(b"Raw profile type") => false,
            _ => true,
        };
        if keep {
            output.extend_from_slice(&(body.len() as u32).to_be_bytes());
            output.extend_from_slice(&kind);
            output.extend_from_slice(&body);
            let mut crc_input = kind.to_vec();
            crc_input.extend_from_slice(&body);
            output.extend_from_slice(&crc32(&crc_input).to_be_bytes());
        } else {
            changed = true;
        }
        at = end;
        if &kind == b"IEND" {
            break;
        }
    }
    if changed {
        *data = output;
    }
    Ok(changed)
}

/// Where the text of an uncompressed iTXt chunk starts, or None when it's
/// compressed.
fn itxt_uncompressed_text(body: &[u8]) -> Option<usize> {
    let keyword_end = body.iter().position(|byte| *byte == 0)?;
    if *body.get(keyword_end + 1)? != 0 {
        return None;
    }
    let mut at = keyword_end + 3;
    // Language tag, then translated keyword, each null-terminated.
    for _ in 0..2 {
        at += body.get(at..)?.iter().position(|byte| *byte == 0)? + 1;
    }
    Some(at)
}

fn tiff_has_orientation(tiff: &[u8]) -> bool {
    let mut copy = tiff.to_vec();
    let Some(tiff) = Tiff::new(&mut copy) else { return false };
    tiff.u32_at(4)
        .and_then(|offset| tiff.entries(offset as usize))
        .is_ok_and(|(entries, _)| entries.iter().any(|entry| entry.tag == TAG_ORIENTATION))
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for byte in bytes {
        crc ^= *byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { 0xEDB8_8320 ^ (crc >> 1) } else { crc >> 1 };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(Path::new(FIXTURES).join(name)).unwrap()
    }

    fn exif_fields(data: &[u8]) -> Vec<exif::Tag> {
        match exif::Reader::new().read_from_container(&mut std::io::Cursor::new(data)) {
            Ok(read) => read.fields().filter(|field| field.ifd_num == exif::In::PRIMARY).map(|field| field.tag).collect(),
            Err(_) => Vec::new(),
        }
    }

    fn has_gps(tags: &[exif::Tag]) -> bool {
        tags.iter().any(|tag| tag.context() == exif::Context::Gps)
    }

    #[test]
    fn removes_the_location_and_keeps_the_rest() {
        for (name, format) in [
            ("location.jpg", Format::Jpeg),
            ("location.heic", Format::Heif),
            ("location.png", Format::Png),
            ("location.tiff", Format::Tiff),
        ] {
            let original = fixture(name);
            assert_eq!(sniff(&original), Some(format), "{name}");
            let before = exif_fields(&original);
            assert!(has_gps(&before), "{name} should start with a location");
            assert!(before.contains(&exif::Tag::Model), "{name}");

            let mut data = original.clone();
            assert!(strip(&mut data, format, ImageMetadataPolicy::RemoveLocation).unwrap(), "{name}");
            assert!(!still_has_metadata_to_remove(&data, format, ImageMetadataPolicy::RemoveLocation), "{name}");
            let after = exif_fields(&data);
            assert!(!has_gps(&after), "{name} still has a location");
            assert!(after.contains(&exif::Tag::Model), "{name} lost its camera");
            assert!(after.contains(&exif::Tag::DateTimeOriginal), "{name} lost its date");
            if format != Format::Png {
                assert_eq!(data.len(), original.len(), "{name} changed size");
            }
            // Nothing left to do the second time.
            assert!(!strip(&mut data, format, ImageMetadataPolicy::RemoveLocation).unwrap(), "{name}");
        }
    }

    #[test]
    fn removes_everything_but_the_orientation() {
        for (name, format) in [
            ("location.jpg", Format::Jpeg),
            ("location.heic", Format::Heif),
            ("location.png", Format::Png),
            ("location.tiff", Format::Tiff),
        ] {
            let mut data = fixture(name);
            assert!(strip(&mut data, format, ImageMetadataPolicy::RemoveAll).unwrap(), "{name}");
            assert!(!still_has_metadata_to_remove(&data, format, ImageMetadataPolicy::RemoveAll), "{name}");
            let after = exif_fields(&data);
            assert!(!has_gps(&after), "{name}");
            for tag in IDENTIFYING_EXIF {
                assert!(!after.contains(&tag), "{name} still has {tag}");
            }
            if format != Format::Png {
                assert!(after.contains(&exif::Tag::Orientation), "{name} lost its orientation");
            }
            if format == Format::Tiff {
                assert!(after.contains(&exif::Tag::ImageWidth), "{name} lost its structure");
            }
        }
    }

    #[test]
    fn leaves_photos_without_a_location_alone() {
        let mut data = fixture("location.jpg");
        strip(&mut data, Format::Jpeg, ImageMetadataPolicy::RemoveLocation).unwrap();
        let clean = data.clone();
        assert!(!strip(&mut data, Format::Jpeg, ImageMetadataPolicy::RemoveLocation).unwrap());
        assert_eq!(data, clean);
    }

    #[test]
    fn blanks_xmp_locations() {
        let xmp = br#"<rdf:Description exif:GPSLatitude="41,0.492N" exif:GPSLongitude='28,58.7E' tiff:Model="X"><exif:GPSAltitude>39/1</exif:GPSAltitude><exif:GPSTimeStamp/><dc:title>Hi</dc:title></rdf:Description>"#;
        let mut data = xmp.to_vec();
        assert!(blank_xmp_location(&mut data));
        let text = String::from_utf8(data.clone()).unwrap();
        assert!(!text.contains("GPS"), "{text}");
        assert!(text.contains(r#"tiff:Model="X""#) && text.contains("<dc:title>Hi</dc:title>"), "{text}");
        assert_eq!(data.len(), xmp.len());
        assert!(!blank_xmp_location(&mut data));
    }

    #[test]
    fn cleans_xmp_in_a_jpeg() {
        let xmp = br#"<x:xmpmeta><rdf:RDF><rdf:Description exif:GPSLatitude="41,0.492N" tiff:Make="Apple"/></rdf:RDF></x:xmpmeta>"#;
        let mut segment = XMP_HEADER.to_vec();
        segment.extend_from_slice(xmp);
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE1];
        jpeg.extend_from_slice(&((segment.len() + 2) as u16).to_be_bytes());
        jpeg.extend_from_slice(&segment);
        jpeg.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x02, 0x12, 0xFF, 0x00, 0x34, 0xFF, 0xD9]);

        let mut location = jpeg.clone();
        assert!(strip(&mut location, Format::Jpeg, ImageMetadataPolicy::RemoveLocation).unwrap());
        assert!(find(&location, b"GPS").is_none() && find(&location, b"Apple").is_some());

        let mut all = jpeg.clone();
        assert!(strip(&mut all, Format::Jpeg, ImageMetadataPolicy::RemoveAll).unwrap());
        assert!(find(&all, b"Apple").is_none());
        assert_eq!(all.len(), jpeg.len());
    }

    #[test]
    fn refuses_what_it_cant_follow() {
        let mut truncated = fixture("location.jpg");
        truncated.truncate(40);
        assert!(strip(&mut truncated, Format::Jpeg, ImageMetadataPolicy::RemoveLocation).is_err());
    }

    #[test]
    fn copies_only_when_something_changes() {
        let directory = std::env::temp_dir().join(format!("aktar-metadata-test-{}", crate::util::new_id()));
        std::fs::create_dir_all(&directory).unwrap();
        let photo = directory.join("photo.jpg");
        std::fs::write(&photo, fixture("location.jpg")).unwrap();
        let text = directory.join("notes.jpg");
        std::fs::write(&text, b"not a photo").unwrap();

        assert!(stripped_copy(&photo, ImageMetadataPolicy::KeepAll).unwrap().is_none());
        assert!(stripped_copy(&text, ImageMetadataPolicy::RemoveAll).unwrap().is_none());
        let copy = stripped_copy(&photo, ImageMetadataPolicy::RemoveLocation).unwrap().unwrap();
        assert_eq!(copy.file_name(), photo.file_name());
        assert!(!has_gps(&exif_fields(&std::fs::read(&copy).unwrap())));
        assert!(stripped_copy(&copy, ImageMetadataPolicy::RemoveLocation).unwrap().is_none());
        remove_copy(&copy);
        assert!(!copy.exists());
        std::fs::remove_dir_all(&directory).unwrap();
    }
}

