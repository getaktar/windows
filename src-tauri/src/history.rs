//! Upload history in SQLite. History is metadata-first: the uploaded file
//! itself is never kept, only a small thumbnail (see `thumbnails`).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Serialize;

use crate::destinations::DestinationConfig;
use crate::output::resolve_public_url;

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
    pub has_thumbnail: bool,
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
}

pub struct History {
    connection: Mutex<Connection>,
    thumbnails: PathBuf,
}

const COLUMNS: &str =
    "id, local_filename, object_key, public_url, destination_id, destination_name, mime_type, byte_size, created_at, expires_at";

impl History {
    pub fn open(data_directory: &Path, thumbnails: PathBuf) -> rusqlite::Result<Self> {
        let connection = Connection::open(data_directory.join("history.sqlite"))?;
        connection.execute_batch("PRAGMA journal_mode = WAL;")?;
        prepare(&connection)?;
        Ok(Self { connection: Mutex::new(connection), thumbnails })
    }

    pub fn thumbnail_path(&self, id: &str) -> PathBuf {
        self.thumbnails.join(format!("{id}.png"))
    }

    fn record_from_row(&self, row: &Row) -> rusqlite::Result<UploadRecord> {
        let id: String = row.get(0)?;
        let has_thumbnail = self.thumbnail_path(&id).exists();
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
            has_thumbnail,
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
            has_thumbnail: false,
        };
        let connection = self.connection.lock().unwrap();
        let result = connection.execute(
            &format!("INSERT INTO uploads ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"),
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
                stored.expires_at
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
        let _ = std::fs::remove_file(self.thumbnail_path(id));
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

        let thumbnails = std::env::temp_dir().join("aktar-history-test");
        let history = History { connection: Mutex::new(connection), thumbnails };
        let old = history.get("A").unwrap();
        assert_eq!(old.local_filename, "a.png");
        assert_eq!(old.expires_at, None);
        assert!(history.expired(i64::MAX).is_empty());
    }

    #[test]
    fn tracks_expiring_uploads() {
        let connection = Connection::open_in_memory().unwrap();
        prepare(&connection).unwrap();
        let history = History { connection: Mutex::new(connection), thumbnails: std::env::temp_dir().join("aktar-history-test") };
        let destination = destination();
        let record = history.insert(NewRecord {
            local_filename: "a.png",
            object_key: "tmp/7d/a.png",
            public_url: "https://img.example.com/tmp/7d/a.png",
            destination: &destination,
            mime_type: "image/png",
            byte_size: 5,
            expire_after_days: Some(7),
        });
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
        let history = History { connection: Mutex::new(connection), thumbnails: std::env::temp_dir().join("aktar-history-test") };
        let destination = destination();
        let record = history.insert(NewRecord {
            local_filename: "a.png",
            object_key: "tmp/1d/a.png",
            public_url: "https://img.example.com/tmp/1d/a.png",
            destination: &destination,
            mime_type: "image/png",
            byte_size: 5,
            expire_after_days: Some(1),
        });
        history.clear_expiry("other");
        assert!(history.get(&record.id).unwrap().expires_at.is_some());
        history.clear_expiry(&destination.id);
        assert_eq!(history.get(&record.id).unwrap().expires_at, None);
        assert!(history.expired(i64::MAX).is_empty());
    }
}
