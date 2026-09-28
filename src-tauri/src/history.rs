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
    pub has_thumbnail: bool,
}

pub struct NewRecord<'a> {
    pub local_filename: &'a str,
    pub object_key: &'a str,
    pub public_url: &'a str,
    pub destination: &'a DestinationConfig,
    pub mime_type: &'a str,
    pub byte_size: i64,
}

pub struct History {
    connection: Mutex<Connection>,
    thumbnails: PathBuf,
}

const COLUMNS: &str = "id, local_filename, object_key, public_url, destination_id, destination_name, mime_type, byte_size, created_at";

impl History {
    pub fn open(data_directory: &Path, thumbnails: PathBuf) -> rusqlite::Result<Self> {
        let connection = Connection::open(data_directory.join("history.sqlite"))?;
        connection.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS uploads (
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
            has_thumbnail,
        })
    }

    pub fn insert(&self, record: NewRecord) -> UploadRecord {
        let stored = UploadRecord {
            id: crate::util::new_id(),
            local_filename: record.local_filename.to_string(),
            object_key: record.object_key.to_string(),
            public_url: record.public_url.to_string(),
            destination_id: record.destination.id.clone(),
            destination_name: record.destination.name.clone(),
            mime_type: record.mime_type.to_string(),
            byte_size: record.byte_size,
            created_at: crate::util::now_millis(),
            has_thumbnail: false,
        };
        let connection = self.connection.lock().unwrap();
        let result = connection.execute(
            &format!("INSERT INTO uploads ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"),
            params![
                stored.id,
                stored.local_filename,
                stored.object_key,
                stored.public_url,
                stored.destination_id,
                stored.destination_name,
                stored.mime_type,
                stored.byte_size,
                stored.created_at
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

    /// ...and one renamed or moved there gets its new key and link.
    pub fn object_moved(&self, old_key: &str, new_key: &str, destination: &DestinationConfig) {
        let url = resolve_public_url(&destination.public_base_url, new_key);
        let connection = self.connection.lock().unwrap();
        let _ = connection.execute(
            "UPDATE uploads SET object_key = ?1, public_url = ?2 WHERE object_key = ?3 AND destination_id = ?4",
            params![new_key, url, old_key, destination.id],
        );
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
