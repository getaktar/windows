//! The thumbnails of uploads in history, one file per upload: `<id>.webp`
//! (`<id>.png` from versions before 0.6, still read). They're kept next to
//! the history, not in a cache folder Windows or a cleanup tool may empty:
//! the file they were made from is usually gone, so one that's deleted
//! can't simply be made again. `<id>.none` remembers that none could be
//! made, so it isn't tried again (which could mean downloading the file).

use std::path::{Path, PathBuf};

pub struct LocalStore {
    directory: PathBuf,
}

const EXTENSIONS: [&str; 2] = ["webp", "png"];

impl LocalStore {
    /// `old`: where versions before 0.6 kept them (the cache folder), moved
    /// over when it's another folder.
    pub fn open(directory: PathBuf, old: Option<&Path>) -> Self {
        let _ = std::fs::create_dir_all(&directory);
        if let Some(old) = old.filter(|old| *old != directory) {
            move_old(old, &directory);
        }
        Self { directory }
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// The upload's thumbnail file, if it has one.
    pub fn path(&self, id: &str) -> Option<PathBuf> {
        EXTENSIONS.iter().map(|extension| self.file(id, extension)).find(|path| path.exists())
    }

    /// WebP, or PNG when WebP couldn't be encoded.
    pub fn store(&self, id: &str, data: &[u8]) {
        let (extension, other) = if super::is_webp(data) { ("webp", "png") } else { ("png", "webp") };
        if write_atomically(&self.file(id, extension), data).is_ok() {
            let _ = std::fs::remove_file(self.file(id, other));
            let _ = std::fs::remove_file(self.file(id, "none"));
        }
    }

    pub fn remove(&self, id: &str) {
        for extension in EXTENSIONS.iter().chain(&["none"]) {
            let _ = std::fs::remove_file(self.file(id, extension));
        }
    }

    pub fn mark_unavailable(&self, id: &str) {
        let _ = std::fs::write(self.file(id, "none"), b"");
    }

    pub fn is_unavailable(&self, id: &str) -> bool {
        self.file(id, "none").exists()
    }

    /// Settings > General > Clear: every upload's thumbnail, and what
    /// couldn't be made, so it's tried again when shown.
    pub fn remove_all(&self) {
        if let Ok(entries) = std::fs::read_dir(&self.directory) {
            for entry in entries.flatten() {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    fn file(&self, id: &str, extension: &str) -> PathBuf {
        self.directory.join(format!("{id}.{extension}"))
    }
}

/// So a row never shows half a file.
pub(super) fn write_atomically(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let temp = path.with_extension("tmp");
    std::fs::write(&temp, data)?;
    std::fs::rename(&temp, path)
}

/// Moves the thumbnails a version before 0.6 kept elsewhere, leaving any
/// already in the new place alone.
fn move_old(old: &Path, directory: &Path) {
    let Ok(entries) = std::fs::read_dir(old) else { return };
    for entry in entries.flatten() {
        let target = directory.join(entry.file_name());
        if !target.exists() && std::fs::rename(entry.path(), &target).is_err() {
            let _ = std::fs::copy(entry.path(), &target);
        }
    }
    let _ = std::fs::remove_dir_all(old);
}

/// Space taken by every file under `folder`.
pub fn disk_usage(folder: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(folder) else { return 0 };
    entries
        .flatten()
        .map(|entry| match entry.metadata() {
            Ok(metadata) if metadata.is_dir() => disk_usage(&entry.path()),
            Ok(metadata) => metadata.len(),
            Err(_) => 0,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_one_file_per_upload() {
        let root = std::env::temp_dir().join(format!("aktar-store-{}", crate::util::new_id()));
        let old = root.join("cache");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("A.png"), b"png").unwrap();
        let store = LocalStore::open(root.join("thumbnails"), Some(&old));
        assert!(!old.exists());
        assert_eq!(store.path("A").unwrap().extension().unwrap(), "png");

        let webp = b"RIFF\0\0\0\0WEBPVP8 data".to_vec();
        store.store("A", &webp);
        assert_eq!(store.path("A").unwrap().extension().unwrap(), "webp");
        assert!(!store.file("A", "png").exists());
        store.mark_unavailable("B");
        assert!(store.is_unavailable("B"));
        assert!(disk_usage(store.directory()) > 0);
        store.remove("A");
        assert!(store.path("A").is_none());
        store.remove_all();
        assert!(!store.is_unavailable("B"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
