//! Which files a watched folder takes, decided from the file on disk:
//! the built-in ignore list, the folder's filter, and the folders that
//! can't be watched at all.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use super::model::{Filter, FilterKind, Subfolders, WatchedFolder};
use super::platform::{self, Attributes};
use crate::t;

/// Partial downloads, editor and sync clients' temporary files, and the
/// files Windows and macOS leave in folders. Case-insensitive.
pub const IGNORED: &[&str] = &[
    "*.crdownload",
    "*.part",
    "*.partial",
    "*.download",
    "*.tmp",
    "*.temp",
    "~$*",
    "~WRL*.tmp",
    "~syncthing~*",
    ".syncthing.*",
    "*.icloud",
    "desktop.ini",
    "Thumbs.db",
    ".DS_Store",
    "*.swp",
    "*.lock",
];

/// Where uploaded originals go ("Move to Uploaded subfolder"); never
/// uploaded itself.
pub const UPLOADED_FOLDER: &str = "Uploaded";

/// What's known about a file that passed the rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileFacts {
    pub size: u64,
    /// Unix milliseconds.
    pub mtime: i64,
    /// The inode, or Windows' file index, to recognize a renamed file.
    pub file_id: Option<u64>,
}

/// Why a file isn't taken. Only `Missing` and `Busy` can change while it's
/// being written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    Missing,
    NotAFile,
    Hidden,
    Empty,
    OutOfScope,
    Ignored,
    Filtered,
    CloudOnly,
}

/// Matches `pattern` against `text`, ignoring case: `*` is any run of
/// characters but "/", `**` any run at all, `?` one character.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.to_lowercase().chars().collect();
    let text: Vec<char> = text.to_lowercase().chars().collect();
    matches(&pattern, &text)
}

fn matches(pattern: &[char], text: &[char]) -> bool {
    match pattern.first() {
        None => text.is_empty(),
        Some('*') => {
            let deep = pattern.get(1) == Some(&'*');
            let rest = if deep { &pattern[2..] } else { &pattern[1..] };
            for skip in 0..=text.len() {
                if matches(rest, &text[skip..]) {
                    return true;
                }
                if skip < text.len() && !deep && text[skip] == '/' {
                    return false;
                }
            }
            false
        }
        Some('?') => !text.is_empty() && text[0] != '/' && matches(&pattern[1..], &text[1..]),
        Some(c) => text.first() == Some(c) && matches(&pattern[1..], &text[1..]),
    }
}

/// A pattern with a "/" is matched against the path inside the watched
/// folder; one without, against the file's name.
fn pattern_matches(pattern: &str, name: &str, relative_path: &str) -> bool {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return false;
    }
    if pattern.contains('/') {
        glob_match(pattern.trim_start_matches('/'), relative_path)
    } else {
        glob_match(pattern, name)
    }
}

pub fn is_ignored(name: &str) -> bool {
    IGNORED.iter().any(|pattern| glob_match(pattern, name))
}

/// The file's path inside `root`, with "/" separators, or None outside it.
pub fn relative_path(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let parts: Vec<String> = relative.components().map(|part| part.as_os_str().to_string_lossy().into_owned()).collect();
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// The folder part of a relative path ("a/b" for "a/b/c.png"), "" at the root.
pub fn subpath(relative_path: &str) -> &str {
    relative_path.rfind('/').map_or("", |slash| &relative_path[..slash])
}

/// Whether a file at `relative_path` is one the folder looks at at all:
/// not in `Uploaded/`, not in a subfolder unless subfolders are included,
/// and not in a hidden one.
pub fn in_scope(subfolders: Subfolders, relative_path: &str) -> bool {
    let parts: Vec<&str> = relative_path.split('/').collect();
    let folders = &parts[..parts.len().saturating_sub(1)];
    if folders.first().is_some_and(|first| first.eq_ignore_ascii_case(UPLOADED_FOLDER)) {
        return false;
    }
    if subfolders == Subfolders::Ignore && !folders.is_empty() {
        return false;
    }
    !folders.iter().any(|folder| folder.starts_with('.'))
}

/// The filter's kind, patterns, and size limits; the ignore list is
/// checked separately.
pub fn passes_filter(filter: &Filter, relative_path: &str, size: u64) -> bool {
    let name = crate::util::last_component(relative_path);
    let mime = crate::output::content_type(name);
    let kind = match filter.kind {
        FilterKind::All => true,
        FilterKind::Images | FilterKind::Screenshots => mime.starts_with("image/"),
        FilterKind::Videos => mime.starts_with("video/"),
        FilterKind::Custom => filter.include.iter().any(|pattern| pattern_matches(pattern, name, relative_path)),
    };
    kind && !filter.exclude.iter().any(|pattern| pattern_matches(pattern, name, relative_path))
        && filter.min_bytes.is_none_or(|min| size >= min)
        && filter.max_bytes.is_none_or(|max| size <= max)
}

/// The rules that need only the name: scope, hidden names, the ignore list.
fn screen_name(folder: &WatchedFolder, relative_path: &str) -> Result<(), Rejection> {
    if !in_scope(folder.subfolders, relative_path) {
        return Err(Rejection::OutOfScope);
    }
    let name = crate::util::last_component(relative_path);
    if name.starts_with('.') {
        return Err(Rejection::Hidden);
    }
    if is_ignored(name) {
        return Err(Rejection::Ignored);
    }
    Ok(())
}

/// The rules that need what the file system says about the file.
fn screen_file(folder: &WatchedFolder, relative_path: &str, size: u64, attributes: Attributes) -> Result<(), Rejection> {
    if attributes.hidden {
        return Err(Rejection::Hidden);
    }
    if size == 0 {
        return Err(Rejection::Empty);
    }
    if attributes.cloud_only && !folder.include_cloud_only {
        return Err(Rejection::CloudOnly);
    }
    if !passes_filter(&folder.filter, relative_path, size) {
        return Err(Rejection::Filtered);
    }
    Ok(())
}

/// Every rule, checked on the file as it is on disk now, with one look at
/// it (`platform::probe`).
pub fn check(folder: &WatchedFolder, path: &Path, relative_path: &str) -> Result<FileFacts, Rejection> {
    screen_name(folder, relative_path)?;
    let probe = match platform::probe(path) {
        Ok(probe) => probe,
        Err(_) => return Err(Rejection::Missing),
    };
    if probe.is_symlink || !probe.is_file {
        return Err(Rejection::NotAFile);
    }
    screen_file(folder, relative_path, probe.size, probe.attributes)?;
    Ok(FileFacts { size: probe.size, mtime: probe.mtime, file_id: probe.file_id })
}

/// Every rule, from a directory listing alone (no file is opened): for
/// counting a folder's files.
pub fn check_listed(folder: &WatchedFolder, listed: &Listed) -> bool {
    screen_name(folder, &listed.relative).is_ok() && screen_file(folder, &listed.relative, listed.size, listed.attributes).is_ok()
}

pub fn mtime_millis(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_millis() as i64)
}

/// A file as its directory listing has it. On Windows the listing carries
/// the size, times, and attributes, so nothing is opened.
#[derive(Debug, Clone)]
pub struct Listed {
    pub path: PathBuf,
    pub relative: String,
    pub size: u64,
    /// Unix milliseconds.
    pub mtime: i64,
    pub attributes: Attributes,
}

/// What `scan` found.
#[derive(Debug, Default)]
pub struct Scan {
    pub files: Vec<Listed>,
    /// Folders ("" for the root) whose own files weren't listed: unchanged
    /// since the last poll of a network share, or unreadable. Rows for
    /// files directly in them count as still there.
    pub unlisted: HashSet<String>,
    /// The watched folder itself could be read.
    pub complete: bool,
}

/// What a network share's folders looked like at the last poll: each
/// folder's modification time and subfolders. A folder whose time hasn't
/// changed has the same entries, so it isn't listed again.
pub type DirectoryCache = HashMap<String, (i64, Vec<String>)>;

/// The files under a watched folder it looks at, without opening them:
/// subfolders only when they're included, never `Uploaded/` or hidden
/// ones, never links. With `cache` (network shares), unchanged folders
/// aren't listed again.
pub fn scan(folder: &WatchedFolder, mut cache: Option<&mut DirectoryCache>) -> Scan {
    let mut scan = Scan { complete: true, ..Scan::default() };
    let mut pending = vec![(folder.path.clone(), String::new())];
    while let Some((directory, prefix)) = pending.pop() {
        let key = prefix.trim_end_matches('/').to_string();
        let mtime = std::fs::metadata(&directory).map(|metadata| mtime_millis(&metadata)).ok();
        if let (Some(cache), Some(mtime)) = (cache.as_deref_mut(), mtime) {
            if let Some((_, subfolders)) = cache.get(&key).filter(|(cached, _)| *cached == mtime && *cached != 0) {
                scan.unlisted.insert(key.clone());
                for subfolder in subfolders.clone() {
                    pending.push((folder.path.join(&subfolder), format!("{subfolder}/")));
                }
                continue;
            }
        }
        let Ok(listing) = std::fs::read_dir(&directory) else {
            if prefix.is_empty() {
                scan.complete = false;
            }
            scan.unlisted.insert(key);
            continue;
        };
        let mut subfolders = Vec::new();
        for item in listing.flatten() {
            let name = item.file_name().to_string_lossy().into_owned();
            let Ok(metadata) = item.metadata() else { continue };
            let relative = format!("{prefix}{name}");
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                if folder.recursive() && !name.starts_with('.') && !(prefix.is_empty() && name.eq_ignore_ascii_case(UPLOADED_FOLDER)) {
                    subfolders.push(relative.clone());
                    pending.push((item.path(), format!("{relative}/")));
                }
            } else if metadata.is_file() {
                scan.files.push(Listed {
                    path: item.path(),
                    relative,
                    size: metadata.len(),
                    mtime: mtime_millis(&metadata),
                    attributes: platform::attributes(&metadata),
                });
            }
        }
        if let (Some(cache), Some(mtime)) = (cache.as_deref_mut(), mtime) {
            cache.insert(key, (mtime, subfolders));
        }
    }
    scan.files.sort_by(|a, b| a.relative.cmp(&b.relative));
    scan
}

/// `scan` without a cache, as (path, relative path) pairs.
#[cfg(test)]
pub fn walk(folder: &WatchedFolder) -> Vec<(PathBuf, String)> {
    scan(folder, None).files.into_iter().map(|listed| (listed.path, listed.relative)).collect()
}

/// Why a folder can't be watched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Forbidden {
    NotAFolder,
    Root,
    Home,
    System,
    AppData,
    Overlaps(String),
}

impl Forbidden {
    pub fn message(&self) -> String {
        match self {
            Forbidden::NotAFolder => t!("That folder doesn’t exist."),
            Forbidden::Root => t!("A whole drive can’t be watched. Pick a folder on it."),
            Forbidden::Home => t!("Your whole user folder can’t be watched. Pick a folder inside it."),
            Forbidden::System => t!("System folders can’t be watched."),
            Forbidden::AppData => t!("Aktar’s own folders can’t be watched."),
            Forbidden::Overlaps(name) => t!("This folder overlaps “{0}”, which is already watched.", name),
        }
    }
}

/// The places that can't be watched, for `forbidden`.
pub struct Protected {
    pub home: Option<PathBuf>,
    pub system: Vec<PathBuf>,
    pub app: Vec<PathBuf>,
}

impl Protected {
    pub fn current(app_dirs: Vec<PathBuf>) -> Self {
        Self { home: platform::home_dir(), system: platform::system_folders(), app: app_dirs }
    }
}

/// Paths compared the way Windows does: case-insensitive, either
/// separator, no trailing separator.
fn comparable(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let text = text.trim_end_matches('/');
    if cfg!(windows) || cfg!(target_os = "macos") {
        text.to_lowercase()
    } else {
        text.to_string()
    }
}

fn is_same_or_inside(path: &str, parent: &str) -> bool {
    path == parent || path.starts_with(&format!("{parent}/"))
}

/// Whether `path` may be watched, given the folders watched already (with
/// their names) other than the one being changed.
pub fn forbidden(path: &Path, protected: &Protected, watched: &[(PathBuf, String)]) -> Option<Forbidden> {
    if path.parent().is_none() || path.components().count() <= 1 {
        return Some(Forbidden::Root);
    }
    let candidate = comparable(path);
    // "C:" or "C:/" after trimming, and "\\server\share" roots.
    if candidate.ends_with(':') || platform::is_share_root(path) {
        return Some(Forbidden::Root);
    }
    if protected.home.as_deref().is_some_and(|home| comparable(home) == candidate) {
        return Some(Forbidden::Home);
    }
    if protected.system.iter().any(|system| is_same_or_inside(&candidate, &comparable(system))) {
        return Some(Forbidden::System);
    }
    if protected.app.iter().any(|app| {
        let app = comparable(app);
        is_same_or_inside(&candidate, &app) || is_same_or_inside(&app, &candidate)
    }) {
        return Some(Forbidden::AppData);
    }
    for (other, name) in watched {
        let other = comparable(other);
        if is_same_or_inside(&candidate, &other) || is_same_or_inside(&other, &candidate) {
            return Some(Forbidden::Overlaps(name.clone()));
        }
    }
    None
}

/// `path` with links and junctions resolved, written without Windows'
/// `\\?\` prefix so it compares with other paths; None when it doesn't
/// exist.
pub fn real_path(path: &Path) -> Option<PathBuf> {
    let real = std::fs::canonicalize(path).ok()?;
    Some(without_verbatim_prefix(&real.to_string_lossy()).map(PathBuf::from).unwrap_or(real))
}

/// "\\?\C:\a" as "C:\a", and "\\?\UNC\server\share" as "\\server\share".
fn without_verbatim_prefix(text: &str) -> Option<String> {
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        Some(format!(r"\\{rest}"))
    } else {
        text.strip_prefix(r"\\?\").map(str::to_string)
    }
}

/// `forbidden`, and the same for where the folder really is: a link or
/// junction that leads into a protected folder (or one already watched)
/// is refused like that folder.
pub fn forbidden_on_disk(path: &Path, protected: &Protected, watched: &[(PathBuf, String)]) -> Option<Forbidden> {
    forbidden(path, protected, watched).or_else(|| {
        let real = real_path(path)?;
        let resolved = |path: &PathBuf| real_path(path).unwrap_or_else(|| path.clone());
        let protected = Protected {
            home: protected.home.as_ref().map(resolved),
            system: protected.system.iter().map(resolved).collect(),
            app: protected.app.iter().map(resolved).collect(),
        };
        let watched: Vec<(PathBuf, String)> = watched.iter().map(|(path, name)| (resolved(path), name.clone())).collect();
        forbidden(&real, &protected, &watched)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_globs_ignoring_case() {
        assert!(glob_match("*.png", "Shot.PNG"));
        assert!(!glob_match("*.png", "shot.png.part"));
        assert!(glob_match("~$*", "~$report.docx"));
        assert!(glob_match("~WRL*.tmp", "~wrl0001.TMP"));
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("*.png", "dir/a.png"));
        assert!(glob_match("dir/*.png", "dir/a.png"));
        assert!(!glob_match("dir/*.png", "dir/sub/a.png"));
        assert!(glob_match("dir/**.png", "dir/sub/a.png"));
        assert!(glob_match("**/raw/*", "2026/raw/a.cr3"));
    }

    #[test]
    fn ignores_temporary_and_system_files() {
        for name in [
            "movie.mp4.crdownload",
            "a.part",
            "b.partial",
            "c.download",
            "d.TMP",
            "e.temp",
            "~$Budget.xlsx",
            "~WRL0005.tmp",
            "~syncthing~a.txt.tmp",
            ".syncthing.a.txt.tmp",
            "photo.jpg.icloud",
            "desktop.ini",
            "thumbs.db",
            ".DS_Store",
            ".notes.swp",
            "package.lock",
        ] {
            assert!(is_ignored(name), "{name}");
        }
        for name in ["movie.mp4", "report.docx", "lockscreen.png", "temperature.csv"] {
            assert!(!is_ignored(name), "{name}");
        }
    }

    #[test]
    fn keeps_to_the_folders_scope() {
        assert!(in_scope(Subfolders::Ignore, "a.png"));
        assert!(!in_scope(Subfolders::Ignore, "sub/a.png"));
        assert!(in_scope(Subfolders::KeepStructure, "sub/a.png"));
        assert!(in_scope(Subfolders::Flatten, "sub/deeper/a.png"));
        assert!(!in_scope(Subfolders::KeepStructure, "Uploaded/a.png"));
        assert!(!in_scope(Subfolders::KeepStructure, "uploaded/sub/a.png"));
        assert!(in_scope(Subfolders::KeepStructure, "sub/Uploaded/a.png"));
        assert!(!in_scope(Subfolders::KeepStructure, ".git/a.png"));
        assert!(in_scope(Subfolders::Ignore, "Uploaded"));
        assert_eq!(subpath("a/b/c.png"), "a/b");
        assert_eq!(subpath("c.png"), "");
    }

    #[test]
    fn applies_the_filter() {
        let all = Filter::default();
        assert!(passes_filter(&all, "a.zip", 1));
        let images = Filter { kind: FilterKind::Images, ..Filter::default() };
        assert!(passes_filter(&images, "a.PNG", 1));
        assert!(!passes_filter(&images, "a.mov", 1));
        let videos = Filter { kind: FilterKind::Videos, ..Filter::default() };
        assert!(passes_filter(&videos, "a.mov", 1));
        assert!(!passes_filter(&videos, "a.png", 1));
        let screenshots = Filter { kind: FilterKind::Screenshots, ..Filter::default() };
        assert!(passes_filter(&screenshots, "Screenshot 2026-10-01 101010.png", 1));
        assert!(!passes_filter(&screenshots, "notes.txt", 1));
        let custom = Filter {
            kind: FilterKind::Custom,
            include: vec!["*.png".into(), "exports/*".into()],
            exclude: vec!["draft-*".into()],
            min_bytes: Some(10),
            max_bytes: Some(100),
        };
        assert!(passes_filter(&custom, "a.png", 50));
        assert!(passes_filter(&custom, "exports/a.csv", 50));
        assert!(!passes_filter(&custom, "a.csv", 50));
        assert!(!passes_filter(&custom, "draft-a.png", 50));
        assert!(!passes_filter(&custom, "a.png", 5));
        assert!(!passes_filter(&custom, "a.png", 500));
        let excluding = Filter { exclude: vec!["*.psd".into()], ..Filter::default() };
        assert!(!passes_filter(&excluding, "art.PSD", 1));
    }

    #[test]
    fn checks_files_on_disk() {
        let dir = std::env::temp_dir().join(format!("aktar-rules-{}", crate::util::new_id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.png"), b"png").unwrap();
        std::fs::write(dir.join("empty.png"), b"").unwrap();
        std::fs::write(dir.join(".hidden.png"), b"x").unwrap();
        std::fs::write(dir.join("sub/b.png"), b"x").unwrap();
        std::fs::write(dir.join("c.png.crdownload"), b"x").unwrap();
        let folder = WatchedFolder::new(&dir);
        let facts = check(&folder, &dir.join("a.png"), "a.png").unwrap();
        assert_eq!(facts.size, 3);
        assert!(facts.mtime > 0);
        assert_eq!(check(&folder, &dir.join("empty.png"), "empty.png"), Err(Rejection::Empty));
        assert_eq!(check(&folder, &dir.join(".hidden.png"), ".hidden.png"), Err(Rejection::Hidden));
        assert_eq!(check(&folder, &dir.join("sub/b.png"), "sub/b.png"), Err(Rejection::OutOfScope));
        assert_eq!(check(&folder, &dir.join("c.png.crdownload"), "c.png.crdownload"), Err(Rejection::Ignored));
        assert_eq!(check(&folder, &dir.join("sub"), "sub"), Err(Rejection::NotAFile));
        assert_eq!(check(&folder, &dir.join("gone.png"), "gone.png"), Err(Rejection::Missing));
        let names: Vec<String> = walk(&folder).into_iter().map(|(_, relative)| relative).collect();
        assert!(names.contains(&"a.png".to_string()));
        assert!(!names.contains(&"sub/b.png".to_string()));
        let recursive = WatchedFolder { subfolders: Subfolders::KeepStructure, ..folder };
        assert!(walk(&recursive).iter().any(|(_, relative)| relative == "sub/b.png"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn refuses_folders_that_cant_be_watched() {
        let base = std::env::temp_dir().join("aktar-forbidden");
        let home = base.join("home");
        let protected = Protected {
            home: Some(home.clone()),
            system: vec![base.join("Windows"), base.join("Program Files")],
            app: vec![home.join("AppData/Roaming/com.getaktar.windows")],
        };
        let watched = vec![(home.join("Pictures/Screenshots"), "Screenshots".to_string())];
        assert_eq!(forbidden(Path::new("/"), &protected, &[]), Some(Forbidden::Root));
        assert_eq!(forbidden(&home, &protected, &[]), Some(Forbidden::Home));
        assert_eq!(forbidden(&base.join("windows/System32"), &protected, &[]), Some(Forbidden::System));
        assert_eq!(forbidden(&base.join("Program Files"), &protected, &[]), Some(Forbidden::System));
        assert_eq!(forbidden(&home.join("AppData/Roaming/com.getaktar.windows"), &protected, &[]), Some(Forbidden::AppData));
        assert_eq!(forbidden(&home.join("AppData"), &protected, &[]), Some(Forbidden::AppData));
        assert_eq!(
            forbidden(&home.join("Pictures"), &protected, &watched),
            Some(Forbidden::Overlaps("Screenshots".into()))
        );
        assert_eq!(
            forbidden(&home.join("Pictures/Screenshots/2026"), &protected, &watched),
            Some(Forbidden::Overlaps("Screenshots".into()))
        );
        assert_eq!(forbidden(&home.join("Pictures/Screenshots2"), &protected, &watched), None);
        assert_eq!(forbidden(&home.join("Desktop"), &protected, &watched), None);
    }

    #[test]
    fn compares_real_paths_without_the_verbatim_prefix() {
        assert_eq!(without_verbatim_prefix(r"\\?\C:\Users\me\AppData"), Some(r"C:\Users\me\AppData".to_string()));
        assert_eq!(without_verbatim_prefix(r"\\?\UNC\server\share\a"), Some(r"\\server\share\a".to_string()));
        assert_eq!(without_verbatim_prefix(r"C:\a"), None);
    }

    #[cfg(unix)]
    #[test]
    fn refuses_links_into_protected_folders() {
        let base = std::env::temp_dir().join(format!("aktar-forbidden-link-{}", crate::util::new_id()));
        let app = base.join("AktarLocalAPI");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::create_dir_all(base.join("Pictures")).unwrap();
        std::os::unix::fs::symlink(&app, base.join("Shortcut")).unwrap();
        let protected = Protected { home: None, system: Vec::new(), app: vec![app.clone()] };
        assert_eq!(forbidden(&base.join("Shortcut"), &protected, &[]), None);
        assert_eq!(forbidden_on_disk(&base.join("Shortcut"), &protected, &[]), Some(Forbidden::AppData));
        assert_eq!(forbidden_on_disk(&base.join("Pictures"), &protected, &[]), None);
        std::fs::remove_dir_all(base).unwrap();
    }
}
