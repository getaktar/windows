//! Multipart uploads for files over `storage::SINGLE_UPLOAD_LIMIT`, and
//! picking them up where they stopped. Each open upload is kept in
//! multipart.json with the parts the server confirmed, so a failed upload's
//! Retry, or the same file uploaded again after Aktar was restarted, sends
//! only the parts that are missing. Quitting leaves them open; cancelling
//! aborts them, and ones left for a week, or whose file changed, are
//! aborted at launch.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::task::JoinSet;

use crate::core::SharedCore;
use crate::credentials;
use crate::storage::{part_size, Progress, S3Provider, StorageError, UploadedPart};

/// Parts sent at once.
const PARTS_IN_FLIGHT: usize = 4;
/// Open uploads older than this are aborted rather than continued.
const MAX_AGE_MILLIS: i64 = 7 * 24 * 3_600_000;

/// An open multipart upload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: String,
    pub destination_id: String,
    pub bucket: String,
    pub object_key: String,
    pub upload_id: String,
    pub part_size: u64,
    /// The size of what's uploaded, which can differ from the source's (a
    /// photo converted first).
    pub file_size: u64,
    /// The file the upload was started for, to recognize it again.
    pub source: Option<SourceFile>,
    /// SHA-256 of what's uploaded, when it was worked out.
    pub sha256: Option<String>,
    /// The parts sent so far, as the server acknowledged them.
    pub parts: Vec<UploadedPart>,
    /// Unix milliseconds.
    pub created_at: i64,
}

/// A file as it was when its upload started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceFile {
    pub path: String,
    pub size: u64,
    /// Unix milliseconds of its last change.
    pub modified: i64,
}

impl SourceFile {
    pub fn of(path: &Path) -> Option<Self> {
        let metadata = std::fs::metadata(path).ok().filter(std::fs::Metadata::is_file)?;
        let modified = metadata.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis() as i64;
        Some(Self { path: path.to_string_lossy().into_owned(), size: metadata.len(), modified })
    }

    /// Whether the file is still there, unchanged.
    fn is_unchanged(&self) -> bool {
        SourceFile::of(Path::new(&self.path)).as_ref() == Some(self)
    }
}

impl Session {
    fn part_count(&self) -> i32 {
        self.file_size.div_ceil(self.part_size).max(1) as i32
    }

    /// Where part `number` is in the file, and how long it is.
    fn part_range(&self, number: i32) -> (u64, u64) {
        let offset = (number as u64 - 1) * self.part_size;
        (offset, self.part_size.min(self.file_size - offset))
    }

    fn is_expired(&self, now: i64) -> bool {
        now - self.created_at > MAX_AGE_MILLIS
    }
}

/// What a new upload is matched against open ones with.
pub struct Candidate<'a> {
    pub destination_id: &'a str,
    pub bucket: &'a str,
    pub file_size: u64,
    pub source: Option<&'a SourceFile>,
    pub sha256: Option<&'a str>,
}

pub struct SessionStore {
    path: PathBuf,
    sessions: Mutex<Vec<Session>>,
}

impl SessionStore {
    pub fn load(directory: &Path) -> Self {
        let path = directory.join("multipart.json");
        let sessions = std::fs::read(&path).ok().and_then(|data| serde_json::from_slice(&data).ok()).unwrap_or_default();
        Self { path, sessions: Mutex::new(sessions) }
    }

    pub fn all(&self) -> Vec<Session> {
        self.sessions.lock().unwrap().clone()
    }

    pub fn get(&self, id: &str) -> Option<Session> {
        self.sessions.lock().unwrap().iter().find(|session| session.id == id).cloned()
    }

    /// Adds the session, or replaces the one with its ID.
    pub fn put(&self, session: Session) {
        self.change(|sessions| match sessions.iter_mut().find(|existing| existing.id == session.id) {
            Some(existing) => *existing = session,
            None => sessions.push(session),
        });
    }

    fn add_part(&self, id: &str, part: UploadedPart) {
        self.change(|sessions| {
            if let Some(session) = sessions.iter_mut().find(|session| session.id == id) {
                session.parts.retain(|existing| existing.number != part.number);
                session.parts.push(part);
            }
        });
    }

    pub fn remove(&self, id: &str) {
        self.change(|sessions| sessions.retain(|session| session.id != id));
    }

    fn change(&self, change: impl FnOnce(&mut Vec<Session>)) {
        let mut sessions = self.sessions.lock().unwrap();
        change(&mut sessions);
        crate::util::write_json_atomically(&self.path, &*sessions);
    }

    /// The open uploads of the same file to the same bucket: the same path,
    /// size and modification date, or the same contents. Ones in `busy`
    /// (another job is sending them) are left out.
    pub fn matching(&self, candidate: &Candidate, busy: &[String]) -> Vec<Session> {
        let now = crate::util::now_millis();
        self.all()
            .into_iter()
            .filter(|session| {
                session.destination_id == candidate.destination_id
                    && session.bucket == candidate.bucket
                    && session.file_size == candidate.file_size
                    && !session.is_expired(now)
                    && !busy.contains(&session.id)
                    && ((candidate.source.is_some() && session.source.as_ref() == candidate.source)
                        || (candidate.sha256.is_some() && session.sha256.as_deref() == candidate.sha256))
            })
            .collect()
    }

    /// Open uploads of the file at `path` that it has changed since.
    pub fn outdated_for(&self, destination_id: &str, source: &SourceFile) -> Vec<Session> {
        self.all()
            .into_iter()
            .filter(|session| {
                session.destination_id == destination_id
                    && session.source.as_ref().is_some_and(|other| other.path == source.path && other != source)
            })
            .collect()
    }
}

/// Starts a multipart upload for `file_size` bytes at `object_key`, and
/// keeps it in `store`.
#[allow(clippy::too_many_arguments)]
pub async fn start(
    provider: &S3Provider,
    store: &SessionStore,
    destination_id: &str,
    bucket: &str,
    object_key: &str,
    content_type: &str,
    file_size: u64,
    source: Option<SourceFile>,
    sha256: Option<String>,
) -> Result<Session, StorageError> {
    let upload_id = provider.create_multipart(object_key, content_type).await?;
    let session = Session {
        id: crate::util::new_id(),
        destination_id: destination_id.to_string(),
        bucket: bucket.to_string(),
        object_key: object_key.to_string(),
        upload_id,
        part_size: part_size(file_size),
        file_size,
        source,
        sha256,
        parts: Vec::new(),
        created_at: crate::util::now_millis(),
    };
    store.put(session.clone());
    Ok(session)
}

/// Sends the parts of `session` the server doesn't have yet from `path`, up
/// to `PARTS_IN_FLIGHT` at once, then puts the object together. A session
/// being resumed first asks the server which parts it has; if the server
/// no longer knows the upload, it starts over under the same key. On
/// success the session is gone from `store`; on failure it stays, with the
/// parts that made it.
pub async fn upload(
    provider: &S3Provider,
    store: &SessionStore,
    mut session: Session,
    path: &Path,
    content_type: &str,
    resuming: bool,
    progress: Arc<Progress>,
) -> Result<(), StorageError> {
    if resuming {
        match provider.uploaded_parts(&session.object_key, &session.upload_id).await? {
            Some(parts) => {
                // A part of a different size than planned (it shouldn't
                // happen) is sent again.
                session.parts = parts.into_iter().filter(|part| part_fits(&session, part)).collect();
            }
            None => {
                session.upload_id = provider.create_multipart(&session.object_key, content_type).await?;
                session.parts.clear();
            }
        }
        store.put(session.clone());
    }

    let done: Vec<i32> = session.parts.iter().map(|part| part.number).collect();
    progress.start(session.file_size, session.parts.iter().map(|part| part.size).sum());
    let mut missing = (1..=session.part_count()).filter(|number| !done.contains(number));
    let sender = provider.part_sender();
    let mut in_flight = JoinSet::new();
    loop {
        while in_flight.len() < PARTS_IN_FLIGHT {
            let Some(number) = missing.next() else { break };
            let (offset, length) = session.part_range(number);
            let (sender, key, upload_id, path, progress) =
                (sender.clone(), session.object_key.clone(), session.upload_id.clone(), path.to_path_buf(), progress.clone());
            in_flight.spawn(async move {
                let etag = sender.send(&key, &upload_id, number, &path, offset, length, progress).await?;
                Ok::<_, StorageError>(UploadedPart { number, etag, size: length })
            });
        }
        // Dropping the set (an error, or the job cancelled) stops the
        // parts still going.
        let Some(finished) = in_flight.join_next().await else { break };
        let part = finished.map_err(|error| StorageError::Unknown(error.to_string()))??;
        store.add_part(&session.id, part.clone());
        session.parts.push(part);
    }

    provider.complete_multipart(&session.object_key, &session.upload_id, &session.parts).await?;
    store.remove(&session.id);
    Ok(())
}

fn part_fits(session: &Session, part: &UploadedPart) -> bool {
    part.number >= 1 && part.number <= session.part_count() && session.part_range(part.number).1 == part.size
}

/// Aborts the upload on the server (whatever it answers) and forgets it.
pub async fn abort(provider: &S3Provider, store: &SessionStore, session: &Session) {
    if let Err(error) = provider.abort_multipart(&session.object_key, &session.upload_id).await {
        log::info!("Could not abort the multipart upload of {}: {error}", session.object_key);
    }
    store.remove(&session.id);
}

/// Aborts `session` with its destination's keys, in the background. One
/// whose destination is gone is just forgotten.
pub fn abort_in_background(core: &SharedCore, session: Session) {
    let core = core.clone();
    tauri::async_runtime::spawn(async move {
        let destination = core.destinations.all().into_iter().find(|destination| destination.id == session.destination_id);
        let credentials = destination.as_ref().and_then(|destination| credentials::load(&destination.id).ok());
        match (destination, credentials) {
            (Some(destination), Some(credentials)) if destination.bucket == session.bucket => {
                abort(&S3Provider::new(destination, credentials), &core.upload_sessions, &session).await;
            }
            _ => core.upload_sessions.remove(&session.id),
        }
    });
}

/// At launch: aborts the open uploads that can't be continued any more,
/// because they're a week old or their file changed or is gone.
pub fn clean_up(core: &SharedCore) {
    let now = crate::util::now_millis();
    for session in core.upload_sessions.all() {
        let usable = !session.is_expired(now) && session.source.as_ref().is_some_and(SourceFile::is_unchanged);
        if !usable {
            abort_in_background(core, session);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(file_size: u64) -> Session {
        Session {
            id: "S".into(),
            destination_id: "D".into(),
            bucket: "b".into(),
            object_key: "k".into(),
            upload_id: "U".into(),
            part_size: part_size(file_size),
            file_size,
            source: None,
            sha256: None,
            parts: Vec::new(),
            created_at: 0,
        }
    }

    #[test]
    fn sizes_parts_to_fit_ten_thousand() {
        const MIB: u64 = 1024 * 1024;
        assert_eq!(part_size(65 * MIB), 16 * MIB);
        assert_eq!(part_size(100 * 1024 * MIB), 16 * MIB);
        // 5 TB: about 556 MiB parts, 9,000 or fewer of them.
        let five_tb = 5 * 1000 * 1000 * 1000 * 1000u64;
        let size = part_size(five_tb);
        assert_eq!(size % MIB, 0);
        assert!(five_tb.div_ceil(size) <= 9000);
        assert!(five_tb.div_ceil(size - MIB) > 9000 || size == 16 * MIB);

        let session = session(40 * MIB + 5);
        assert_eq!(session.part_count(), 3);
        assert_eq!(session.part_range(1), (0, 16 * MIB));
        assert_eq!(session.part_range(3), (32 * MIB, 8 * MIB + 5));
        assert!(part_fits(&session, &UploadedPart { number: 3, etag: "e".into(), size: 8 * MIB + 5 }));
        assert!(!part_fits(&session, &UploadedPart { number: 3, etag: "e".into(), size: 16 * MIB }));
        assert!(!part_fits(&session, &UploadedPart { number: 4, etag: "e".into(), size: 1 }));
    }

    #[test]
    fn matches_the_same_file_only() {
        let directory = std::env::temp_dir().join(format!("aktar-multipart-test-{}", crate::util::new_id()));
        std::fs::create_dir_all(&directory).unwrap();
        let file = directory.join("big.bin");
        std::fs::write(&file, b"0123456789").unwrap();
        let source = SourceFile::of(&file).unwrap();
        let store = SessionStore::load(&directory);
        let mut open = session(10);
        open.source = Some(source.clone());
        open.created_at = crate::util::now_millis();
        store.put(open.clone());

        let candidate = |source: Option<&SourceFile>, sha256: Option<&str>| {
            store.matching(&Candidate { destination_id: "D", bucket: "b", file_size: 10, source, sha256 }, &[]).len()
        };
        assert_eq!(candidate(Some(&source), None), 1);
        assert_eq!(candidate(None, Some("abc")), 0);
        assert!(store.matching(&Candidate { destination_id: "D", bucket: "b", file_size: 10, source: Some(&source), sha256: None }, &["S".into()]).is_empty());
        assert!(store.matching(&Candidate { destination_id: "E", bucket: "b", file_size: 10, source: Some(&source), sha256: None }, &[]).is_empty());

        // Kept across launches, with its parts.
        store.add_part("S", UploadedPart { number: 1, etag: "\"e1\"".into(), size: 10 });
        let reloaded = SessionStore::load(&directory);
        assert_eq!(reloaded.get("S").unwrap().parts.len(), 1);

        // The file changes: the session is outdated.
        std::fs::write(&file, b"01234567890").unwrap();
        let changed = SourceFile::of(&file).unwrap();
        assert_eq!(store.outdated_for("D", &changed).len(), 1);
        assert!(!source.is_unchanged());
        store.remove("S");
        assert!(SessionStore::load(&directory).all().is_empty());
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
