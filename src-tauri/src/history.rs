//! Upload history in SQLite. History is metadata-first: the uploaded file
//! itself is never kept, only a small thumbnail (see `thumbnails`).

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Serialize;

use crate::destinations::DestinationConfig;
use crate::output::resolve_public_url;
use crate::thumbnails::LocalStore;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadRecord {
    pub id: String,
    pub local_filename: String,
    pub object_key: String,
    pub public_url: String,
    pub destination_id: String,
    pub destination_name: String,
    pub mime_type: String,
    pub byte_size: i64,
    /// Unix milliseconds.
    pub created_at: i64,
    /// Unix milliseconds when an expiring upload gets deleted, or none.
    pub expires_at: Option<i64>,
    /// SHA-256 (lowercase hex) of the bytes uploaded, for reusing the link
    /// when the same file comes up again. None for uploads made before it
    /// was recorded, and for ones it isn't worked out for.
    pub content_hash: Option<String>,
    /// Where the upload came from when it wasn't the user directly:
    /// "watchedFolder:<ID>" for a watched folder's.
    pub source: Option<String>,
    /// The watched folder's name at the time, for "Watched: Screenshots"
    /// even after it's removed.
    pub source_name: Option<String>,
    pub has_thumbnail: bool,
    /// The thumbnail file on this PC, for the windows to show.
    pub thumbnail_path: Option<String>,
}

pub struct NewRecord<'a> {
    pub local_filename: &'a str,
    pub object_key: &'a str,
    pub public_url: &'a str,
    pub destination: &'a DestinationConfig,
    pub mime_type: &'a str,
    pub byte_size: i64,
    /// Days until it's deleted, for an expiring upload.
    pub expire_after_days: Option<u32>,
    pub content_hash: Option<&'a str>,
    /// A watched folder's ID and name.
    pub watched_folder: Option<(&'a str, &'a str)>,
}

pub struct History {
    /// Shared with the watched folders' ledger, which keeps its table in
    /// the same database.
    connection: Arc<Mutex<Connection>>,
    pub thumbnails: LocalStore,
}

const COLUMNS: &str = "id, local_filename, object_key, public_url, destination_id, destination_name, mime_type, byte_size, created_at, expires_at, content_hash, source, source_name";

/// The `source` of a watched folder's uploads.
pub fn watched_source(folder_id: &str) -> String {
    format!("watchedFolder:{folder_id}")
}

impl History {
    pub fn open(data_directory: &Path, thumbnails: LocalStore) -> rusqlite::Result<Self> {
        let connection = Connection::open(data_directory.join("history.sqlite"))?;
        // NORMAL is the usual pairing with WAL: a crash of the app never
        // loses or damages anything; only a power cut can lose the last
        // moments, never corrupt the database. FULL fsyncs every write.
        connection.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")?;
        // The ledger writes from other threads; wait for it rather than fail.
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        prepare(&connection)?;
        Ok(Self { connection: Arc::new(Mutex::new(connection)), thumbnails })
    }

    /// The watched folders' ledger, in this database.
    pub fn ledger(&self) -> rusqlite::Result<crate::watched::ledger::Ledger> {
        crate::watched::ledger::Ledger::new(self.connection.clone())
    }

    fn record_from_row(&self, row: &Row) -> rusqlite::Result<UploadRecord> {
        let id: String = row.get(0)?;
        let thumbnail_path = self.thumbnails.path(&id).map(|path| path.to_string_lossy().into_owned());
        Ok(UploadRecord {
            id,
            local_filename: row.get(1)?,
            object_key: row.get(2)?,
            public_url: row.get(3)?,
            destination_id: row.get(4)?,
            destination_name: row.get(5)?,
            mime_type: row.get(6)?,
            byte_size: row.get(7)?,
            created_at: row.get(8)?,
            expires_at: row.get(9)?,
            content_hash: row.get(10)?,
            source: row.get(11)?,
            source_name: row.get(12)?,
            has_thumbnail: thumbnail_path.is_some(),
            thumbnail_path,
        })
    }

    pub fn insert(&self, record: NewRecord) -> UploadRecord {
        let created_at = crate::util::now_millis();
        let stored = UploadRecord {
            id: crate::util::new_id(),
            local_filename: record.local_filename.to_string(),
            object_key: record.object_key.to_string(),
            public_url: record.public_url.to_string(),
            destination_id: record.destination.id.clone(),
            destination_name: record.destination.name.clone(),
            mime_type: record.mime_type.to_string(),
            byte_size: record.byte_size,
            created_at,
            expires_at: record.expire_after_days.map(|days| crate::expiry::expires_at(created_at, days)),
            content_hash: record.content_hash.map(str::to_string),
            source: record.watched_folder.map(|(id, _)| watched_source(id)),
            source_name: record.watched_folder.map(|(_, name)| name.to_string()),
            has_thumbnail: false,
            thumbnail_path: None,
        };
        let connection = self.connection.lock().unwrap();
        let result = connection.execute(
            &format!("INSERT INTO uploads ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)"),
            params![
                stored.id,
                stored.local_filename,
                stored.object_key,
                stored.public_url,
                stored.destination_id,
                stored.destination_name,
                stored.mime_type,
                stored.byte_size,
                stored.created_at,
                stored.expires_at,
                stored.content_hash,
                stored.source,
                stored.source_name
            ],
        );
        if let Err(error) = result {
            log::error!("Could not save upload history: {error}");
        }
        stored
    }

    /// Newest first.
    pub fn all(&self) -> Vec<UploadRecord> {
        let connection = self.connection.lock().unwrap();
        let Ok(mut statement) = connection.prepare(&format!("SELECT {COLUMNS} FROM uploads ORDER BY created_at DESC")) else {
            return Vec::new();
        };
        statement
            .query_map([], |row| self.record_from_row(row))
            .map(|rows| rows.filter_map(Result::ok).collect())
            .unwrap_or_default()
    }

    /// Expiring uploads whose time is up at `now`, oldest first.
    pub fn expired(&self, now: i64) -> Vec<UploadRecord> {
        let connection = self.connection.lock().unwrap();
        let Ok(mut statement) = connection.prepare(&format!(
            "SELECT {COLUMNS} FROM uploads WHERE expires_at IS NOT NULL AND expires_at <= ?1 ORDER BY expires_at"
        )) else {
            return Vec::new();
        };
        statement
            .query_map([now], |row| self.record_from_row(row))
            .map(|rows| rows.filter_map(Result::ok).collect())
            .unwrap_or_default()
    }

    /// Earlier uploads of the same bytes to `destination_id`, newest first.
    pub fn with_content(&self, destination_id: &str, content_hash: &str) -> Vec<UploadRecord> {
        let connection = self.connection.lock().unwrap();
        let Ok(mut statement) = connection.prepare(&format!(
            "SELECT {COLUMNS} FROM uploads WHERE destination_id = ?1 AND content_hash = ?2 ORDER BY created_at DESC"
        )) else {
            return Vec::new();
        };
        statement
            .query_map(params![destination_id, content_hash], |row| self.record_from_row(row))
            .map(|rows| rows.filter_map(Result::ok).collect())
            .unwrap_or_default()
    }

    /// Uploads that went to `object_key` in `destination_id`, newest first.
    pub fn with_object(&self, destination_id: &str, object_key: &str) -> Vec<UploadRecord> {
        let connection = self.connection.lock().unwrap();
        let Ok(mut statement) = connection.prepare(&format!(
            "SELECT {COLUMNS} FROM uploads WHERE destination_id = ?1 AND object_key = ?2 ORDER BY created_at DESC"
        )) else {
            return Vec::new();
        };
        statement
            .query_map(params![destination_id, object_key], |row| self.record_from_row(row))
            .map(|rows| rows.filter_map(Result::ok).collect())
            .unwrap_or_default()
    }

    pub fn get(&self, id: &str) -> Option<UploadRecord> {
        let connection = self.connection.lock().unwrap();
        connection
            .query_row(&format!("SELECT {COLUMNS} FROM uploads WHERE id = ?1 COLLATE NOCASE"), [id], |row| self.record_from_row(row))
            .optional()
            .ok()
            .flatten()
    }

    pub fn delete(&self, id: &str) {
        let connection = self.connection.lock().unwrap();
        let _ = connection.execute("DELETE FROM uploads WHERE id = ?1", [id]);
        drop(connection);
        self.thumbnails.remove(id);
    }

    /// Turning a destination's thumbnails off removes the ones made for it.
    pub fn remove_thumbnails(&self, destination_id: &str) {
        let ids: Vec<String> = {
            let connection = self.connection.lock().unwrap();
            let Ok(mut statement) = connection.prepare("SELECT id FROM uploads WHERE destination_id = ?1") else { return };
            statement
                .query_map([destination_id], |row| row.get(0))
                .map(|rows| rows.filter_map(Result::ok).collect())
                .unwrap_or_default()
        };
        for id in ids {
            self.thumbnails.remove(&id);
        }
    }

    /// Keeps history in step with the bucket browser: an object deleted
    /// there drops out of history.
    pub fn object_deleted(&self, key: &str, destination_id: &str) {
        for id in self.ids_for(key, destination_id) {
            self.delete(&id);
        }
    }

    /// ...and one renamed or moved there gets its new key and link. Moved
    /// out of its `tmp/{N}d/` folder, lifecycle no longer deletes it, so it
    /// stops expiring; moved into another one, the copy expires N days from
    /// now, but only when `rules_active`: without Aktar's rules in the
    /// bucket, nothing deletes it, and a folder that happens to be called
    /// `tmp/7d/` is just a folder.
    pub fn object_moved(&self, old_key: &str, new_key: &str, destination: &DestinationConfig, rules_active: bool) {
        let url = resolve_public_url(&destination.public_base_url, new_key);
        let connection = self.connection.lock().unwrap();
        let old_days = crate::expiry::days_in_key(old_key);
        let new_days = crate::expiry::days_in_key(new_key);
        let result = if new_days.is_some() && new_days == old_days {
            connection.execute(
                "UPDATE uploads SET object_key = ?1, public_url = ?2 WHERE object_key = ?3 AND destination_id = ?4",
                params![new_key, url, old_key, destination.id],
            )
        } else {
            let expires_at = new_days
                .filter(|_| rules_active)
                .map(|days| crate::expiry::expires_at(crate::util::now_millis(), days));
            connection.execute(
                "UPDATE uploads SET object_key = ?1, public_url = ?2, expires_at = ?5 WHERE object_key = ?3 AND destination_id = ?4",
                params![new_key, url, old_key, destination.id, expires_at],
            )
        };
        if let Err(error) = result {
            log::error!("Could not update upload history: {error}");
        }
    }

    /// Uploads to `destination_id` stop expiring: its bucket no longer has
    /// Aktar's rules, so those files stay for good, and neither the history
    /// nor Aktar's own sweep may treat them as due.
    pub fn clear_expiry(&self, destination_id: &str) {
        let connection = self.connection.lock().unwrap();
        if let Err(error) = connection.execute(
            "UPDATE uploads SET expires_at = NULL WHERE destination_id = ?1 AND expires_at IS NOT NULL",
            [destination_id],
        ) {
            log::error!("Could not update upload history: {error}");
        }
    }

    fn ids_for(&self, key: &str, destination_id: &str) -> Vec<String> {
        let connection = self.connection.lock().unwrap();
        let Ok(mut statement) = connection.prepare("SELECT id FROM uploads WHERE object_key = ?1 AND destination_id = ?2") else {
            return Vec::new();
        };
        statement
            .query_map(params![key, destination_id], |row| row.get(0))
            .map(|rows| rows.filter_map(Result::ok).collect())
            .unwrap_or_default()
    }
}

/// Creates the table, or brings one from an older version up to date.
/// Columns added later are nullable, so existing rows stay valid as they are.
fn prepare(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS uploads (
             id TEXT PRIMARY KEY NOT NULL,
             local_filename TEXT NOT NULL,
             object_key TEXT NOT NULL,
             public_url TEXT NOT NULL,
             destination_id TEXT NOT NULL,
             destination_name TEXT NOT NULL,
             mime_type TEXT NOT NULL,
             byte_size INTEGER NOT NULL,
             created_at INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS uploads_created_at ON uploads (created_at DESC);
         CREATE INDEX IF NOT EXISTS uploads_object ON uploads (destination_id, object_key);",
    )?;
    if !has_column(connection, "uploads", "expires_at")? {
        connection.execute_batch("ALTER TABLE uploads ADD COLUMN expires_at INTEGER;")?;
    }
    if !has_column(connection, "uploads", "content_hash")? {
        connection.execute_batch("ALTER TABLE uploads ADD COLUMN content_hash TEXT;")?;
    }
    connection.execute_batch("CREATE INDEX IF NOT EXISTS uploads_content ON uploads (destination_id, content_hash);")?;
    if !has_column(connection, "uploads", "source")? {
        connection.execute_batch("ALTER TABLE uploads ADD COLUMN source TEXT; ALTER TABLE uploads ADD COLUMN source_name TEXT;")?;
    }
    Ok(())
}

fn has_column(connection: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
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
    use crate::destinations::ProviderPreset;

    fn destination() -> DestinationConfig {
        DestinationConfig {
            id: "D1".into(),
            name: "Test".into(),
            preset: ProviderPreset::MinIO,
            account_id: None,
            endpoint: "http://127.0.0.1:9000".into(),
            region: "us-east-1".into(),
            bucket: "test".into(),
            public_base_url: "https://img.example.com".into(),
            object_path_template: "{uuid}.{ext}".into(),
            force_path_style: true,
            is_default: true,
            output_mode: None,
            expiry_days: None,
            temporary_link: None,
            image_metadata: None,
            folder_upload: None,
            image_processing: None,
            thumbnails: None,
            thumbnail_prefix: None,
        }
    }

    #[test]
    fn adds_expires_at_to_an_existing_database() {
        let connection = Connection::open_in_memory().unwrap();
        // The table as versions before expiring uploads created it.
        connection
            .execute_batch(
                "CREATE TABLE uploads (
                     id TEXT PRIMARY KEY NOT NULL, local_filename TEXT NOT NULL, object_key TEXT NOT NULL,
                     public_url TEXT NOT NULL, destination_id TEXT NOT NULL, destination_name TEXT NOT NULL,
                     mime_type TEXT NOT NULL, byte_size INTEGER NOT NULL, created_at INTEGER NOT NULL
                 );
                 INSERT INTO uploads VALUES ('A', 'a.png', '2026/a.png', 'https://x/a.png', 'D1', 'Test', 'image/png', 5, 1000);",
            )
            .unwrap();
        prepare(&connection).unwrap();
        // Running again (every launch) is a no-op.
        prepare(&connection).unwrap();
        assert!(has_column(&connection, "uploads", "expires_at").unwrap());
        assert!(has_column(&connection, "uploads", "content_hash").unwrap());
        assert!(has_column(&connection, "uploads", "source").unwrap());

        let thumbnails = LocalStore::open(std::env::temp_dir().join("aktar-history-test"), None);
        let history = History { connection: Arc::new(Mutex::new(connection)), thumbnails };
        let old = history.get("A").unwrap();
        assert_eq!(old.local_filename, "a.png");
        assert_eq!(old.expires_at, None);
        assert_eq!(old.content_hash, None);
        assert_eq!(old.source, None);
        assert!(history.expired(i64::MAX).is_empty());
    }

    #[test]
    fn tracks_expiring_uploads() {
        let connection = Connection::open_in_memory().unwrap();
        prepare(&connection).unwrap();
        let history = History { connection: Arc::new(Mutex::new(connection)), thumbnails: LocalStore::open(std::env::temp_dir().join("aktar-history-test"), None) };
        let destination = destination();
        let record = history.insert(NewRecord {
            local_filename: "a.png",
            object_key: "tmp/7d/a.png",
            public_url: "https://img.example.com/tmp/7d/a.png",
            destination: &destination,
            mime_type: "image/png",
            byte_size: 5,
            expire_after_days: Some(7),
            content_hash: None,
            watched_folder: Some(("F1", "Screenshots")),
        });
        assert_eq!(history.get(&record.id).unwrap().source.as_deref(), Some("watchedFolder:F1"));
        assert_eq!(history.get(&record.id).unwrap().source_name.as_deref(), Some("Screenshots"));
        let expires_at = record.expires_at.unwrap();
        assert_eq!(expires_at, record.created_at + 7 * 86_400_000);
        assert_eq!(history.get(&record.id).unwrap().expires_at, Some(expires_at));
        assert!(history.expired(expires_at - 1).is_empty());
        assert_eq!(history.expired(expires_at).len(), 1);

        // Renamed inside its folder: still expires on the same day.
        history.object_moved("tmp/7d/a.png", "tmp/7d/b.png", &destination, true);
        assert_eq!(history.get(&record.id).unwrap().expires_at, Some(expires_at));
        // Moved out of it: kept for good.
        history.object_moved("tmp/7d/b.png", "keep/b.png", &destination, true);
        let moved = history.get(&record.id).unwrap();
        assert_eq!(moved.expires_at, None);
        assert_eq!(moved.public_url, "https://img.example.com/keep/b.png");
        // Moved into a tmp/ folder: expires only if the bucket has the rules.
        history.object_moved("keep/b.png", "tmp/1d/b.png", &destination, false);
        assert_eq!(history.get(&record.id).unwrap().expires_at, None);
        history.object_moved("tmp/1d/b.png", "tmp/14d/b.png", &destination, true);
        assert!(history.get(&record.id).unwrap().expires_at.is_some());
    }

    #[test]
    fn clears_expiry_when_the_rules_go() {
        let connection = Connection::open_in_memory().unwrap();
        prepare(&connection).unwrap();
        let history = History { connection: Arc::new(Mutex::new(connection)), thumbnails: LocalStore::open(std::env::temp_dir().join("aktar-history-test"), None) };
        let destination = destination();
        let record = history.insert(NewRecord {
            local_filename: "a.png",
            object_key: "tmp/1d/a.png",
            public_url: "https://img.example.com/tmp/1d/a.png",
            destination: &destination,
            mime_type: "image/png",
            byte_size: 5,
            expire_after_days: Some(1),
            content_hash: Some("abc"),
            watched_folder: None,
        });
        assert_eq!(history.with_content(&destination.id, "abc").len(), 1);
        assert!(history.with_content("other", "abc").is_empty());
        assert!(history.with_content(&destination.id, "abd").is_empty());
        history.clear_expiry("other");
        assert!(history.get(&record.id).unwrap().expires_at.is_some());
        history.clear_expiry(&destination.id);
        assert_eq!(history.get(&record.id).unwrap().expires_at, None);
        assert!(history.expired(i64::MAX).is_empty());
    }
}
