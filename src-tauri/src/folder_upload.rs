//! Folder uploads, like the Mac app's FolderUpload: a folder goes up as one
//! ZIP, or file by file under a folder of its own in the bucket, as the
//! destination's "Folders" setting says. Hidden files (.env, .git,
//! desktop.ini, Thumbs.db) are left out either way: a shared link shouldn't
//! carry secrets or a repository along.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::destinations::ImageMetadataPolicy;
use crate::image_metadata::{self, MetadataError};
use crate::t;

/// More than this is almost certainly a mistake (a whole user folder), and
/// would flood the bucket and the history.
pub const MAX_FILES: usize = 2000;

#[derive(Debug, Clone, thiserror::Error)]
pub enum FolderUploadError {
    #[error("{}", t!("{0} has no files to upload.", .0))]
    Empty(String),
    #[error("{}", t!("{0} has more than {1} files. Upload it as a ZIP instead.", .0, .1))]
    TooManyFiles(String, usize),
    #[error("{}", t!("Couldn't make a ZIP of {0}.", .0))]
    CouldNotZip(String),
    #[error(transparent)]
    Metadata(#[from] MetadataError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    /// Path inside the folder, with "/" separators, e.g. "css/site.css".
    pub relative_path: String,
}

/// The name a folder goes by: its own, or "folder" for a drive's root.
pub fn name(folder: &Path) -> String {
    folder.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| "folder".into())
}

/// The files in `folder` and its subfolders, skipping hidden ones and
/// links (which could lead outside it, or around in circles). `limit` is
/// None when they all go into one ZIP anyway.
pub fn files(folder: &Path, limit: Option<usize>) -> Result<Vec<Entry>, FolderUploadError> {
    let name = name(folder);
    let mut entries = Vec::new();
    let mut pending = vec![(folder.to_path_buf(), String::new())];
    while let Some((directory, prefix)) = pending.pop() {
        let Ok(listing) = std::fs::read_dir(&directory) else { continue };
        for item in listing.flatten() {
            let file_name = item.file_name().to_string_lossy().into_owned();
            let Ok(metadata) = std::fs::symlink_metadata(item.path()) else { continue };
            if is_hidden(&file_name, &metadata) || metadata.file_type().is_symlink() {
                continue;
            }
            let relative_path = format!("{prefix}{file_name}");
            if metadata.is_dir() {
                pending.push((item.path(), format!("{relative_path}/")));
            } else if metadata.is_file() {
                entries.push(Entry { path: item.path(), relative_path });
                if limit.is_some_and(|limit| entries.len() > limit) {
                    return Err(FolderUploadError::TooManyFiles(name, limit.unwrap_or_default()));
                }
            }
        }
    }
    if entries.is_empty() {
        return Err(FolderUploadError::Empty(name));
    }
    entries.sort_by_key(|entry| entry.relative_path.to_lowercase());
    Ok(entries)
}

#[cfg(windows)]
fn is_hidden(name: &str, metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const HIDDEN: u32 = 0x2;
    const SYSTEM: u32 = 0x4;
    // Junctions and other reparse points, which `is_symlink` doesn't cover.
    const REPARSE_POINT: u32 = 0x400;
    name.starts_with('.') || metadata.file_attributes() & (HIDDEN | SYSTEM | REPARSE_POINT) != 0
}

#[cfg(not(windows))]
fn is_hidden(name: &str, _metadata: &std::fs::Metadata) -> bool {
    name.starts_with('.')
}

/// Where a folder's files go in the bucket: the destination's path
/// template, used as if for a file named like the folder, as a folder.
/// `{year}/{month}/{uuid}.{ext}` and "Project" give
/// "2026/09/7f3c…/Project/", so two uploads of the same folder never
/// collide and the folder name still shows in every link.
pub fn key_prefix(template: &str, folder_name: &str) -> String {
    let mut base = crate::output::generate_key(template, folder_name);
    // The folder has no extension, so "{uuid}.{ext}" ends in a dot.
    while base.ends_with('.') || base.ends_with('/') {
        base.pop();
    }
    if base.is_empty() {
        return format!("{folder_name}/");
    }
    let last = base.rsplit('/').next().unwrap_or(&base);
    if last == folder_name {
        format!("{base}/")
    } else {
        format!("{base}/{folder_name}/")
    }
}

/// A .zip of `folder`, named after it, in a temporary folder the caller
/// deletes with `remove_zip`. Hidden files are left out, the same as when
/// the folder keeps its structure, and the files sit under the folder's
/// own name, as File Explorer's "Compress to ZIP file" does. Photos go in
/// without the metadata `image_metadata` removes, the same as photos
/// uploaded on their own; one that can't be cleaned stops the ZIP rather
/// than share where it was taken.
pub fn zip(folder: &Path, image_metadata: ImageMetadataPolicy) -> Result<PathBuf, FolderUploadError> {
    let name = name(folder);
    let directory = std::env::temp_dir().join("Aktar").join(format!("zip-{}", crate::util::new_id()));
    let output = directory.join(format!("{name}.zip"));
    let result = write_zip(folder, &name, &output, image_metadata);
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&directory);
    }
    result.map(|()| output)
}

fn write_zip(folder: &Path, name: &str, output: &Path, policy: ImageMetadataPolicy) -> Result<(), FolderUploadError> {
    let failed = || FolderUploadError::CouldNotZip(name.to_string());
    let entries = files(folder, None)?;
    std::fs::create_dir_all(output.parent().ok_or_else(failed)?).map_err(|_| failed())?;
    let mut writer = zip::ZipWriter::new(std::fs::File::create(output).map_err(|_| failed())?);
    for entry in entries {
        let cleaned = image_metadata::stripped_copy(&entry.path, policy)?;
        let source = cleaned.as_deref().unwrap_or(&entry.path);
        let added = add_file(&mut writer, source, &format!("{name}/{}", entry.relative_path));
        if let Some(cleaned) = &cleaned {
            image_metadata::remove_copy(cleaned);
        }
        added.map_err(|_| failed())?;
    }
    writer.finish().map_err(|_| failed())?.flush().map_err(|_| failed())
}

fn add_file(writer: &mut zip::ZipWriter<std::fs::File>, source: &Path, name: &str) -> std::io::Result<()> {
    let metadata = std::fs::metadata(source)?;
    let mut options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .large_file(metadata.len() >= u32::MAX as u64);
    if let Some(modified) = metadata.modified().ok().and_then(zip_time) {
        options = options.last_modified_time(modified);
    }
    writer.start_file(name, options)?;
    std::io::copy(&mut std::fs::File::open(source)?, writer)?;
    Ok(())
}

fn zip_time(time: std::time::SystemTime) -> Option<zip::DateTime> {
    use chrono::{Datelike, Timelike};
    let local: chrono::DateTime<chrono::Local> = time.into();
    zip::DateTime::from_date_and_time(
        u16::try_from(local.year()).ok()?,
        local.month() as u8,
        local.day() as u8,
        local.hour() as u8,
        local.minute() as u8,
        local.second() as u8,
    )
    .ok()
}

pub fn remove_zip(path: &Path) {
    if let Some(directory) = path.parent() {
        let _ = std::fs::remove_dir_all(directory);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_folder() -> PathBuf {
        let root = std::env::temp_dir().join(format!("aktar-folder-test-{}", crate::util::new_id())).join("Project");
        for (path, contents) in [
            ("index.html", "<h1>Hi</h1>"),
            ("css/site.css", "body{}"),
            (".env", "SECRET=1"),
            (".git/config", "[core]"),
        ] {
            let file = root.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, contents).unwrap();
        }
        let photo = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/location.jpg")).unwrap();
        std::fs::write(root.join("css/photo.jpg"), photo).unwrap();
        root
    }

    #[test]
    fn lists_visible_files() {
        let folder = sample_folder();
        let paths: Vec<String> = files(&folder, Some(MAX_FILES)).unwrap().into_iter().map(|e| e.relative_path).collect();
        assert_eq!(paths, ["css/photo.jpg", "css/site.css", "index.html"]);
        assert!(matches!(files(&folder, Some(2)), Err(FolderUploadError::TooManyFiles(_, 2))));
        let empty = folder.join("empty");
        std::fs::create_dir_all(empty.join(".hidden")).unwrap();
        assert!(matches!(files(&empty, None), Err(FolderUploadError::Empty(_))));
        std::fs::remove_dir_all(folder.parent().unwrap()).unwrap();
    }

    #[test]
    fn names_the_folder_in_the_bucket() {
        let prefix = key_prefix("{year}/{month}/{uuid}.{ext}", "Project");
        assert!(prefix.ends_with("/Project/") && prefix.matches('/').count() == 4, "{prefix}");
        assert_eq!(key_prefix("{filename}.{ext}", "Project"), "Project/");
        assert_eq!(key_prefix("shots/{filename}", "Project"), "shots/Project/");
        assert_eq!(key_prefix("", "Project"), "Project/");
    }

    #[test]
    fn zips_without_hidden_files_or_locations() {
        let folder = sample_folder();
        let zipped = zip(&folder, ImageMetadataPolicy::RemoveLocation).unwrap();
        assert_eq!(zipped.file_name().unwrap(), "Project.zip");
        let mut archive = zip::ZipArchive::new(std::fs::File::open(&zipped).unwrap()).unwrap();
        let mut names: Vec<String> = archive.file_names().map(str::to_string).collect();
        names.sort();
        assert_eq!(names, ["Project/css/photo.jpg", "Project/css/site.css", "Project/index.html"]);
        let mut photo = Vec::new();
        std::io::Read::read_to_end(&mut archive.by_name("Project/css/photo.jpg").unwrap(), &mut photo).unwrap();
        let read = exif::Reader::new().read_from_container(&mut std::io::Cursor::new(&photo)).unwrap();
        assert!(read.fields().all(|field| field.tag.context() != exif::Context::Gps));
        assert!(read.get_field(exif::Tag::Model, exif::In::PRIMARY).is_some());

        remove_zip(&zipped);
        assert!(!zipped.parent().unwrap().exists());
        std::fs::remove_dir_all(folder.parent().unwrap()).unwrap();
    }
}
