//! What each watched folder has handled already: one row per file, in the
//! history database next to the upload history. Every decision whether a
//! file is new, changed, or just renamed is made against it.
//!
//! A file deleted from the folder keeps its row for a day as `gone`: a new
//! file at the same path is new (not a change of the old one), the same
//! file showing up elsewhere is a rename, and an upload whose file was
//! deleted can be deleted from the bucket too (`OnDelete::DeleteRemote`).

use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection, OptionalExtension, Row};

use super::model::Modified;
use super::rules::FileFacts;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Uploaded,
    /// There before the folder was added, or skipped in the large batch
    /// prompt: never uploaded.
    Skipped,
    Failed,
    /// Queued; one still pending at launch never finished, and goes again.
    Pending,
    /// Deleted from the folder (`Entry::gone_at`). Purged after a day.
    Gone,
}

/// How long after a file disappears it counts as the same file when
/// something shows up at its path again (an atomic save), and before its
/// upload is deleted from the bucket.
pub const GRACE_MILLIS: i64 = 3_000;
/// Gone rows are forgotten after this long.
pub const GONE_KEPT_MILLIS: i64 = 24 * 60 * 60 * 1000;

/// Deleting a gone file's upload from the bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteDelete {
    /// Due once the grace period is over, behind the large-deletion guard.
    Pending,
    /// Due, and the user already said yes (or it's a retry).
    Confirmed,
    /// Part of a large deletion, waiting for the user.
    Held,
    /// Sent.
    Running,
    /// Didn't work, and won't be tried again on its own.
    Failed,
}

impl RemoteDelete {
    fn raw(self) -> &'static str {
        match self {
            RemoteDelete::Pending => "pending",
            RemoteDelete::Confirmed => "confirmed",
            RemoteDelete::Held => "held",
            RemoteDelete::Running => "running",
            RemoteDelete::Failed => "failed",
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "pending" => Some(RemoteDelete::Pending),
            "confirmed" => Some(RemoteDelete::Confirmed),
            "held" => Some(RemoteDelete::Held),
            "running" => Some(RemoteDelete::Running),
            "failed" => Some(RemoteDelete::Failed),
            _ => None,
        }
    }
}

impl State {
    fn raw(self) -> &'static str {
        match self {
            State::Uploaded => "uploaded",
            State::Skipped => "skipped",
            State::Failed => "failed",
            State::Pending => "pending",
            State::Gone => "gone",
        }
    }

    fn parse(raw: &str) -> Self {
        match raw {
            "uploaded" => State::Uploaded,
            "skipped" => State::Skipped,
            "failed" => State::Failed,
            "gone" => State::Gone,
            _ => State::Pending,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub folder_id: String,
    pub relative_path: String,
    pub size: u64,
    /// Unix milliseconds.
    pub mtime: i64,
    pub file_id: Option<u64>,
    pub sha256: Option<String>,
    pub state: State,
    pub attempts: u32,
    pub last_error: Option<String>,
    pub object_key: Option<String>,
    pub url: Option<String>,
    pub destination_id: Option<String>,
    /// Unix milliseconds.
    pub handled_at: i64,
    /// When a failed upload (or a gone file's remote delete) is tried
    /// again on its own (network and server errors); none waits for Retry.
    pub retry_at: Option<i64>,
    /// When the file was found deleted (Unix milliseconds), for `Gone`.
    pub gone_at: Option<i64>,
    /// The state before it was gone, to go back to when it turns out to be
    /// an atomic save or a rename.
    pub prior_state: Option<State>,
    /// The upload reused an earlier upload's link: that object isn't this
    /// file's to delete.
    pub reused: bool,
    pub remote_delete: Option<RemoteDelete>,
}

impl Entry {
    pub fn new(folder_id: &str, relative_path: &str, facts: FileFacts, state: State) -> Self {
        Self {
            folder_id: folder_id.to_string(),
            relative_path: relative_path.to_string(),
            size: facts.size,
            mtime: facts.mtime,
            file_id: facts.file_id,
            sha256: None,
            state,
            attempts: 0,
            last_error: None,
            object_key: None,
            url: None,
            destination_id: None,
            handled_at: crate::util::now_millis(),
            retry_at: None,
            gone_at: None,
            prior_state: None,
            reused: false,
            remote_delete: None,
        }
    }

    /// The row as it was before the file went (an atomic save put it back,
    /// or it was renamed): its delete from the bucket is off.
    pub fn restored(&self) -> Entry {
        Entry {
            state: self.prior_state.unwrap_or(State::Skipped),
            gone_at: None,
            prior_state: None,
            remote_delete: None,
            retry_at: None,
            ..self.clone()
        }
    }

    /// The row once its file is found deleted. `delete_remote` when the
    /// folder deletes uploads with their files, and this one is an upload
    /// of its own.
    pub fn gone(&self, now: i64, delete_remote: bool) -> Entry {
        let uploaded = self.state == State::Uploaded && self.object_key.is_some();
        Entry {
            state: State::Gone,
            gone_at: Some(now),
            prior_state: Some(self.state),
            attempts: 0,
            last_error: None,
            retry_at: None,
            remote_delete: (delete_remote && uploaded && !self.reused).then_some(RemoteDelete::Pending),
            ..self.clone()
        }
    }

    pub fn same_contents_as(&self, facts: &FileFacts) -> bool {
        self.size == facts.size && self.mtime == facts.mtime
    }
}

/// What to do with a file that passed the rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Never seen: upload it.
    New,
    /// Changed since it was uploaded, and the folder uploads changes.
    Changed(Box<Entry>),
    /// The same file under a new name: the row follows it, nothing is
    /// uploaded.
    Renamed { from: String },
    /// Handled already (or a change the folder ignores).
    Handled,
}

/// Decides from the ledger alone, plus the file's checksum when a change
/// has to be told from a touch, and whether another row's file is still
/// where it was. A gone row at the file's path is the same file again only
/// within the grace period (an atomic save: deleted and written again);
/// after that, whatever is there is new.
pub fn decide(
    modified: Modified,
    existing: Option<&Entry>,
    facts: &FileFacts,
    sha256: impl FnOnce() -> Option<String>,
    others: &[Entry],
    still_there: impl Fn(&str) -> bool,
    now: i64,
) -> Decision {
    let existing = match existing {
        Some(entry) if entry.state == State::Gone => {
            entry.gone_at.filter(|at| now - at < GRACE_MILLIS).map(|_| entry.restored())
        }
        other => other.cloned(),
    };
    if let Some(entry) = existing {
        return match entry.state {
            State::Pending => Decision::New,
            _ if entry.same_contents_as(facts) => Decision::Handled,
            // A failed file that's been replaced starts over.
            State::Failed | State::Gone => Decision::New,
            State::Uploaded | State::Skipped if modified == Modified::Ignore => Decision::Handled,
            State::Uploaded | State::Skipped => match (&entry.sha256, sha256()) {
                (Some(previous), Some(current)) if *previous == current => Decision::Handled,
                _ => Decision::Changed(Box::new(entry)),
            },
        };
    }
    if facts.file_id.is_some() {
        let handled = |state: Option<State>| matches!(state, Some(State::Uploaded | State::Skipped));
        let moved = others.iter().find(|other| {
            other.file_id == facts.file_id
                && other.same_contents_as(facts)
                && match other.state {
                    State::Gone => handled(other.prior_state),
                    state => handled(Some(state)) && !still_there(&other.relative_path),
                }
        });
        if let Some(moved) = moved {
            return Decision::Renamed { from: moved.relative_path.clone() };
        }
    }
    Decision::New
}

/// Counts for a folder's card.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    pub failed: usize,
    pub last_upload_at: Option<i64>,
    pub uploaded: usize,
    /// Uploads of deleted files on their way out of the bucket.
    pub deleting: usize,
    /// ...and ones waiting for the user (a large deletion).
    pub held: usize,
}

#[derive(Clone)]
pub struct Ledger {
    connection: Arc<Mutex<Connection>>,
}

/// Written out (not built with `format!`), so the statements are the same
/// strings each time and come from the connection's statement cache.
macro_rules! columns {
    () => {
        "folder_id, relative_path, size, mtime, file_id, sha256, state, attempts, last_error, object_key, url, destination_id, handled_at, retry_at, gone_at, prior_state, reused, remote_delete"
    };
}
const SELECT: &str = concat!("SELECT ", columns!(), " FROM watched_files ");
const INSERT: &str = concat!(
    "INSERT OR REPLACE INTO watched_files (",
    columns!(),
    ") VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)"
);

impl Ledger {
    /// Shares the history database's connection.
    pub fn new(connection: Arc<Mutex<Connection>>) -> rusqlite::Result<Self> {
        prepare(&connection.lock().unwrap())?;
        let ledger = Self { connection };
        ledger.resume_interrupted_deletes();
        Ok(ledger)
    }

    #[cfg(test)]
    pub fn in_memory() -> Self {
        Self::new(Arc::new(Mutex::new(Connection::open_in_memory().unwrap()))).unwrap()
    }

    fn entry(row: &Row) -> rusqlite::Result<Entry> {
        Ok(Entry {
            folder_id: row.get(0)?,
            relative_path: row.get(1)?,
            size: row.get::<_, i64>(2)? as u64,
            mtime: row.get(3)?,
            file_id: row.get::<_, Option<i64>>(4)?.map(|id| id as u64),
            sha256: row.get(5)?,
            state: State::parse(&row.get::<_, String>(6)?),
            attempts: row.get(7)?,
            last_error: row.get(8)?,
            object_key: row.get(9)?,
            url: row.get(10)?,
            destination_id: row.get(11)?,
            handled_at: row.get(12)?,
            retry_at: row.get(13)?,
            gone_at: row.get(14)?,
            prior_state: row.get::<_, Option<String>>(15)?.map(|raw| State::parse(&raw)),
            reused: row.get::<_, Option<bool>>(16)?.unwrap_or(false),
            remote_delete: row.get::<_, Option<String>>(17)?.and_then(|raw| RemoteDelete::parse(&raw)),
        })
    }

    pub fn get(&self, folder_id: &str, relative_path: &str) -> Option<Entry> {
        let connection = self.connection.lock().unwrap();
        let mut statement = connection.prepare_cached(concat!("SELECT ", columns!(), " FROM watched_files WHERE folder_id = ?1 AND relative_path = ?2")).ok()?;
        statement.query_row(params![folder_id, relative_path], Self::entry).optional().ok().flatten()
    }

    /// The folder's rows for the file with this ID (inode or file index),
    /// for telling a rename from a new file.
    pub fn by_file_id(&self, folder_id: &str, file_id: u64) -> Vec<Entry> {
        self.select("WHERE folder_id = ?1 AND file_id = ?2", params![folder_id, file_id as i64])
    }

    /// Whether the folder has rows inside the subfolder `prefix` ("a/b/").
    pub fn has_prefix(&self, folder_id: &str, prefix: &str) -> bool {
        // Everything starting with "a/b/" sorts before "a/b0" ("0" follows "/").
        let end = format!("{}0", prefix.trim_end_matches('/'));
        let connection = self.connection.lock().unwrap();
        let Ok(mut statement) = connection
            .prepare_cached("SELECT 1 FROM watched_files WHERE folder_id = ?1 AND relative_path >= ?2 AND relative_path < ?3 LIMIT 1")
        else {
            return false;
        };
        statement.exists(params![folder_id, prefix, end]).unwrap_or(false)
    }

    /// When the ledger next has something due (Unix milliseconds): a failed
    /// upload's retry, a gone file's delete (after its grace period and any
    /// retry wait), or a gone row to forget.
    pub fn next_due(&self) -> Option<i64> {
        let connection = self.connection.lock().unwrap();
        let Ok(mut statement) = connection.prepare_cached(
            "SELECT MIN(due) FROM (
                 SELECT MIN(retry_at) AS due FROM watched_files WHERE state = 'failed' AND retry_at IS NOT NULL
                 UNION ALL
                 SELECT MIN(MAX(gone_at + ?1, COALESCE(retry_at, 0))) FROM watched_files
                     WHERE state = 'gone' AND remote_delete IN ('pending', 'confirmed')
                 UNION ALL
                 SELECT MIN(gone_at + ?2) FROM watched_files WHERE state = 'gone' AND (remote_delete IS NULL OR remote_delete = 'failed')
             )",
        ) else {
            return None;
        };
        statement.query_row(params![GRACE_MILLIS, GONE_KEPT_MILLIS], |row| row.get::<_, Option<i64>>(0)).ok().flatten()
    }

    pub fn entries(&self, folder_id: &str) -> Vec<Entry> {
        self.select("WHERE folder_id = ?1", params![folder_id])
    }

    pub fn failed(&self, folder_id: &str) -> Vec<Entry> {
        self.select("WHERE folder_id = ?1 AND state = 'failed'", params![folder_id])
    }

    /// Failed uploads whose automatic retry is due at `now`.
    pub fn due_retries(&self, now: i64) -> Vec<Entry> {
        self.select("WHERE state = 'failed' AND retry_at IS NOT NULL AND retry_at <= ?1", params![now])
    }

    /// Every failed upload that's tried again on its own.
    pub fn retryable(&self) -> Vec<Entry> {
        self.select("WHERE state = 'failed' AND retry_at IS NOT NULL", params![])
    }

    /// Gone files whose upload is due to be deleted from the bucket at
    /// `now`: past the grace period, and past any retry's wait.
    pub fn due_deletes(&self, now: i64) -> Vec<Entry> {
        self.select(
            "WHERE state = 'gone' AND remote_delete IN ('pending', 'confirmed') AND gone_at <= ?1 AND (retry_at IS NULL OR retry_at <= ?2)",
            params![now - GRACE_MILLIS, now],
        )
    }

    /// A folder's deletions waiting for the user's go-ahead.
    pub fn held_deletes(&self, folder_id: &str) -> Vec<Entry> {
        self.select("WHERE folder_id = ?1 AND state = 'gone' AND remote_delete = 'held'", params![folder_id])
    }

    /// Other files (in any folder) that are still there and went to the
    /// same object.
    pub fn references(&self, destination_id: &str, object_key: &str) -> Vec<Entry> {
        self.select("WHERE destination_id = ?1 AND object_key = ?2 AND state != 'gone'", params![destination_id, object_key])
    }

    /// Back online: deletes waiting to be tried again go right away.
    pub fn retry_deletes_now(&self) {
        let connection = self.connection.lock().unwrap();
        let _ = connection.execute(
            "UPDATE watched_files SET retry_at = NULL WHERE state = 'gone' AND remote_delete IN ('pending', 'confirmed')",
            [],
        );
    }

    /// Deletes sent when Aktar quit never reported back: they go again.
    fn resume_interrupted_deletes(&self) {
        let connection = self.connection.lock().unwrap();
        let _ = connection.execute("UPDATE watched_files SET remote_delete = 'confirmed' WHERE remote_delete = 'running'", []);
    }

    /// Forgets files gone for longer than `GONE_KEPT_MILLIS`, unless their
    /// upload is still to be deleted.
    pub fn purge_gone(&self, now: i64) {
        let connection = self.connection.lock().unwrap();
        let _ = connection.execute(
            "DELETE FROM watched_files WHERE state = 'gone' AND gone_at < ?1 AND (remote_delete IS NULL OR remote_delete = 'failed')",
            [now - GONE_KEPT_MILLIS],
        );
    }

    fn select(&self, filter: &str, values: impl rusqlite::Params) -> Vec<Entry> {
        let connection = self.connection.lock().unwrap();
        let Ok(mut statement) = connection.prepare_cached(&format!("{SELECT}{filter} ORDER BY relative_path")) else {
            return Vec::new();
        };
        statement.query_map(values, Self::entry).map(|rows| rows.filter_map(Result::ok).collect()).unwrap_or_default()
    }

    /// How many of the folder's files failed, and when the last upload was.
    /// Read from the (folder, state, remote delete, time) index alone, a
    /// group per state rather than every row.
    pub fn summary(&self, folder_id: &str) -> Summary {
        let connection = self.connection.lock().unwrap();
        let mut summary = Summary::default();
        let Ok(mut statement) = connection.prepare_cached(
            "SELECT state, remote_delete, COUNT(*), MAX(handled_at) FROM watched_files WHERE folder_id = ?1 GROUP BY state, remote_delete",
        ) else {
            return summary;
        };
        let rows = statement.query_map([folder_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, i64>(2)? as usize, row.get::<_, Option<i64>>(3)?))
        });
        for (state, remote_delete, count, last) in rows.into_iter().flatten().flatten() {
            match (State::parse(&state), remote_delete.as_deref().and_then(RemoteDelete::parse)) {
                (State::Failed, _) => summary.failed += count,
                (State::Uploaded, _) => {
                    summary.uploaded += count;
                    summary.last_upload_at = summary.last_upload_at.max(last);
                }
                (State::Gone, Some(RemoteDelete::Pending | RemoteDelete::Confirmed | RemoteDelete::Running)) => summary.deleting += count,
                (State::Gone, Some(RemoteDelete::Held)) => summary.held += count,
                _ => {}
            }
        }
        summary
    }

    pub fn put(&self, entry: &Entry) {
        insert(&self.connection.lock().unwrap(), entry);
    }

    /// Many rows in one transaction: a folder's existing files.
    pub fn put_all(&self, entries: &[Entry]) {
        let connection = self.connection.lock().unwrap();
        let _ = connection.execute_batch("BEGIN");
        for entry in entries {
            insert(&connection, entry);
        }
        let _ = connection.execute_batch("COMMIT");
    }

    pub fn rename(&self, folder_id: &str, from: &str, to: &str) {
        let connection = self.connection.lock().unwrap();
        let _ = connection.execute("DELETE FROM watched_files WHERE folder_id = ?1 AND relative_path = ?2", params![folder_id, to]);
        let _ = connection.execute(
            "UPDATE watched_files SET relative_path = ?3 WHERE folder_id = ?1 AND relative_path = ?2",
            params![folder_id, from, to],
        );
    }

    pub fn remove(&self, folder_id: &str, relative_path: &str) {
        let connection = self.connection.lock().unwrap();
        let _ = connection.execute("DELETE FROM watched_files WHERE folder_id = ?1 AND relative_path = ?2", params![folder_id, relative_path]);
    }

    /// "Reset", and removing the folder: everything it handled is forgotten.
    pub fn remove_folder(&self, folder_id: &str) {
        let connection = self.connection.lock().unwrap();
        let _ = connection.execute("DELETE FROM watched_files WHERE folder_id = ?1", [folder_id]);
    }
}

fn insert(connection: &Connection, entry: &Entry) {
    let result = connection.prepare_cached(INSERT).and_then(|mut statement| statement.execute(
        params![
            entry.folder_id,
            entry.relative_path,
            entry.size as i64,
            entry.mtime,
            entry.file_id.map(|id| id as i64),
            entry.sha256,
            entry.state.raw(),
            entry.attempts,
            entry.last_error,
            entry.object_key,
            entry.url,
            entry.destination_id,
            entry.handled_at,
            entry.retry_at,
            entry.gone_at,
            entry.prior_state.map(State::raw),
            entry.reused,
            entry.remote_delete.map(RemoteDelete::raw),
        ],
    ));
    if let Err(error) = result {
        log::error!("Could not save a watched file: {error}");
    }
}

fn prepare(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS watched_files (
             folder_id TEXT NOT NULL,
             relative_path TEXT NOT NULL,
             size INTEGER NOT NULL,
             mtime INTEGER NOT NULL,
             file_id INTEGER,
             sha256 TEXT,
             state TEXT NOT NULL,
             attempts INTEGER NOT NULL DEFAULT 0,
             last_error TEXT,
             object_key TEXT,
             url TEXT,
             destination_id TEXT,
             handled_at INTEGER NOT NULL,
             retry_at INTEGER,
             PRIMARY KEY (folder_id, relative_path)
         );
         CREATE INDEX IF NOT EXISTS watched_files_state ON watched_files (state, retry_at);",
    )?;
    // Added for deleted files; nullable, so rows from before stay valid.
    for (column, kind) in [("gone_at", "INTEGER"), ("prior_state", "TEXT"), ("reused", "INTEGER"), ("remote_delete", "TEXT")] {
        if !has_column(connection, column)? {
            connection.execute_batch(&format!("ALTER TABLE watched_files ADD COLUMN {column} {kind};"))?;
        }
    }
    connection.execute_batch(
        "CREATE INDEX IF NOT EXISTS watched_files_object ON watched_files (destination_id, object_key);
         CREATE INDEX IF NOT EXISTS watched_files_file_id ON watched_files (folder_id, file_id);
         CREATE INDEX IF NOT EXISTS watched_files_summary ON watched_files (folder_id, state, remote_delete, handled_at);",
    )
}

fn has_column(connection: &Connection, column: &str) -> rusqlite::Result<bool> {
    let mut statement = connection.prepare("PRAGMA table_info(watched_files)")?;
    let names = statement.query_map([], |row| row.get::<_, String>(1))?;
    for name in names {
        if name? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(size: u64, mtime: i64, file_id: Option<u64>) -> FileFacts {
        FileFacts { size, mtime, file_id }
    }

    fn uploaded(path: &str, facts: FileFacts, sha256: Option<&str>) -> Entry {
        Entry { sha256: sha256.map(str::to_string), object_key: Some(format!("k/{path}")), ..Entry::new("F", path, facts, State::Uploaded) }
    }

    #[test]
    fn tells_new_changed_and_handled_files_apart() {
        let file = facts(10, 1000, Some(7));
        assert_eq!(decide(Modified::Ignore, None, &file, || None, &[], |_| true, 0), Decision::New);

        let entry = uploaded("a.png", file, Some("abc"));
        assert_eq!(decide(Modified::UploadAgain, Some(&entry), &file, || panic!("no need to hash"), &[], |_| true, 0), Decision::Handled);

        let touched = facts(10, 2000, Some(7));
        assert_eq!(decide(Modified::Ignore, Some(&entry), &touched, || Some("zzz".into()), &[], |_| true, 0), Decision::Handled);
        // Saved again without changes: same checksum.
        assert_eq!(decide(Modified::UploadAgain, Some(&entry), &touched, || Some("abc".into()), &[], |_| true, 0), Decision::Handled);
        assert_eq!(
            decide(Modified::Overwrite, Some(&entry), &touched, || Some("def".into()), &[], |_| true, 0),
            Decision::Changed(Box::new(entry.clone()))
        );

        let pending = Entry::new("F", "a.png", file, State::Pending);
        assert_eq!(decide(Modified::Ignore, Some(&pending), &file, || None, &[], |_| true, 0), Decision::New);
        let failed = Entry::new("F", "a.png", file, State::Failed);
        assert_eq!(decide(Modified::Ignore, Some(&failed), &file, || None, &[], |_| true, 0), Decision::Handled);
        assert_eq!(decide(Modified::Ignore, Some(&failed), &touched, || None, &[], |_| true, 0), Decision::New);
        let skipped = Entry::new("F", "a.png", file, State::Skipped);
        assert_eq!(decide(Modified::Ignore, Some(&skipped), &touched, || None, &[], |_| true, 0), Decision::Handled);
    }

    #[test]
    fn follows_renamed_files() {
        let file = facts(10, 1000, Some(7));
        let others = vec![uploaded("old.png", file, None)];
        assert_eq!(decide(Modified::Ignore, None, &file, || None, &others, |_| false, 0), Decision::Renamed { from: "old.png".into() });
        // The old one is still there: a copy, so a new file.
        assert_eq!(decide(Modified::Ignore, None, &file, || None, &others, |_| true, 0), Decision::New);
        // Different contents, or no ID to go by.
        assert_eq!(decide(Modified::Ignore, None, &facts(11, 1000, Some(7)), || None, &others, |_| false, 0), Decision::New);
        assert_eq!(decide(Modified::Ignore, None, &facts(10, 1000, None), || None, &others, |_| false, 0), Decision::New);
    }

    #[test]
    fn a_new_file_where_one_was_deleted_is_new() {
        let old = facts(10, 1000, Some(7));
        let gone = uploaded("Screenshot (5).png", old, Some("abc")).gone(50_000, true);
        assert_eq!(gone.remote_delete, Some(RemoteDelete::Pending));
        let other = facts(20, 2000, Some(8));
        // Long after: a different file, uploaded like any other.
        assert_eq!(decide(Modified::Ignore, Some(&gone), &other, || None, &[], |_| true, 50_000 + GRACE_MILLIS), Decision::New);
        // Within the grace period it's an atomic save of the same file,
        // treated as the folder treats changes.
        assert_eq!(decide(Modified::Ignore, Some(&gone), &other, || None, &[], |_| true, 51_500), Decision::Handled);
        assert_eq!(
            decide(Modified::Overwrite, Some(&gone), &other, || Some("def".into()), &[], |_| true, 51_500),
            Decision::Changed(Box::new(gone.restored()))
        );
        assert_eq!(gone.restored().state, State::Uploaded);
        assert_eq!(gone.restored().remote_delete, None);

        // The same file somewhere else, even long after: a rename.
        let elsewhere = vec![gone.clone()];
        assert_eq!(
            decide(Modified::Ignore, None, &old, || None, &elsewhere, |_| true, 10_000_000),
            Decision::Renamed { from: "Screenshot (5).png".into() }
        );
        // Never uploaded on its own (a reused link, a skipped file): nothing
        // to delete from the bucket.
        let reused = Entry { reused: true, ..uploaded("a.png", old, None) };
        assert_eq!(reused.gone(1, true).remote_delete, None);
        assert_eq!(Entry::new("F", "b.png", old, State::Skipped).gone(1, true).remote_delete, None);
        assert_eq!(uploaded("c.png", old, None).gone(1, false).remote_delete, None);
    }

    #[test]
    fn keeps_gone_rows_for_a_day() {
        let ledger = Ledger::in_memory();
        let file = facts(1, 1, Some(1));
        ledger.put(&uploaded("kept.png", file, None).gone(0, false));
        ledger.put(&uploaded("deleting.png", file, None).gone(0, true));
        ledger.put(&uploaded("recent.png", file, None).gone(GONE_KEPT_MILLIS, false));
        assert_eq!(ledger.due_deletes(GRACE_MILLIS - 1).len(), 0);
        assert_eq!(ledger.due_deletes(GRACE_MILLIS).len(), 1);
        assert_eq!(ledger.summary("F").deleting, 1);
        ledger.purge_gone(GONE_KEPT_MILLIS + 1);
        assert!(ledger.get("F", "kept.png").is_none());
        assert!(ledger.get("F", "deleting.png").is_some());
        assert!(ledger.get("F", "recent.png").is_some());
        assert_eq!(ledger.get("F", "recent.png").unwrap().prior_state, Some(State::Uploaded));
        // Still there elsewhere: the object is shared.
        ledger.put(&Entry { destination_id: Some("D".into()), ..uploaded("x.png", file, None) });
        assert_eq!(ledger.references("D", "k/x.png").len(), 1);
    }

    #[test]
    fn stores_rows() {
        let ledger = Ledger::in_memory();
        let mut entry = uploaded("a.png", facts(10, 1000, Some(u64::MAX - 1)), Some("abc"));
        ledger.put(&entry);
        assert_eq!(ledger.get("F", "a.png"), Some(entry.clone()));
        entry.state = State::Failed;
        entry.retry_at = Some(5);
        ledger.put(&entry);
        assert_eq!(ledger.due_retries(4).len(), 0);
        assert_eq!(ledger.due_retries(5).len(), 1);
        assert_eq!(ledger.summary("F").failed, 1);
        ledger.rename("F", "a.png", "b.png");
        assert!(ledger.get("F", "a.png").is_none());
        assert_eq!(ledger.get("F", "b.png").unwrap().file_id, Some(u64::MAX - 1));
        ledger.put_all(&[Entry::new("F", "c.png", facts(1, 1, None), State::Skipped), Entry::new("G", "c.png", facts(1, 1, None), State::Skipped)]);
        assert_eq!(ledger.entries("F").len(), 2);
        ledger.remove_folder("F");
        assert!(ledger.entries("F").is_empty());
        assert_eq!(ledger.entries("G").len(), 1);
    }
}
