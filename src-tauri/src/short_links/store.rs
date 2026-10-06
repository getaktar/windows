//! Short link records, in the history database. Separate from the uploads
//! (one upload can have several, and they outlive its history entry when
//! cleanup fails), tied to them by `upload_id`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Serialize;

use super::rules::{Snapshot, Status};

/// A short link made for an upload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShortLink {
    pub id: String,
    pub upload_id: String,
    /// The definition's id, or "custom".
    pub provider: String,
    pub provider_name: String,
    /// The provider's id or code, for delete, update and stats.
    pub provider_id: Option<String>,
    /// The short domain it was made on, for providers that need it again.
    pub domain: Option<String>,
    pub short_url: String,
    pub target_url: String,
    /// Unix milliseconds, like the times below.
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub status: Status,
    pub clicks: Option<i64>,
    pub last_click_at: Option<i64>,
    pub stats_checked_at: Option<i64>,
}

impl ShortLink {
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            id: self.id.clone(),
            provider: self.provider.clone(),
            provider_id: self.provider_id.clone(),
            domain: self.domain.clone(),
            status: self.status,
            created_at: self.created_at,
            expires_at: self.expires_at,
        }
    }

    /// Active, but past its expiry: shown as expired.
    pub fn display_status(&self, now: i64) -> Status {
        super::rules::display_status(&self.snapshot(), now)
    }
}

const COLUMNS: &str = "id, upload_id, provider, provider_name, provider_id, domain, short_url, target_url, created_at, expires_at, status, clicks, last_click_at, stats_checked_at";

/// Creates the table; called with the history's own.
pub fn prepare(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS short_links (
             id TEXT PRIMARY KEY NOT NULL,
             upload_id TEXT NOT NULL,
             provider TEXT NOT NULL,
             provider_name TEXT NOT NULL,
             provider_id TEXT,
             domain TEXT,
             short_url TEXT NOT NULL,
             target_url TEXT NOT NULL,
             created_at INTEGER NOT NULL,
             expires_at INTEGER,
             status TEXT NOT NULL,
             clicks INTEGER,
             last_click_at INTEGER,
             stats_checked_at INTEGER
         );
         CREATE INDEX IF NOT EXISTS short_links_upload ON short_links (upload_id, created_at DESC);",
    )
}

/// The upload's active short link as one column ("id", "provider" and
/// "url" joined by U+001F), for the history's queries: the newest active
/// one that hasn't run out, as `rules::active` picks it.
macro_rules! active_column {
    () => {
        "(SELECT s.id || char(31) || s.provider || char(31) || s.short_url FROM short_links s \
         WHERE s.upload_id = uploads.id AND s.status = 'active' \
         AND (s.expires_at IS NULL OR s.expires_at > CAST(strftime('%s', 'now') AS INTEGER) * 1000) \
         ORDER BY s.created_at DESC LIMIT 1)"
    };
}
pub(crate) use active_column;

/// `active_column!`'s value split up: id, provider, short URL.
pub fn split_active(value: Option<String>) -> Option<(String, String, String)> {
    let value = value?;
    let mut parts = value.splitn(3, '\u{1f}');
    Some((parts.next()?.to_string(), parts.next()?.to_string(), parts.next()?.to_string()))
}

#[derive(Clone)]
pub struct ShortLinkStore {
    connection: Arc<Mutex<Connection>>,
}

impl ShortLinkStore {
    pub fn new(connection: Arc<Mutex<Connection>>) -> Self {
        Self { connection }
    }

    fn from_row(row: &Row) -> rusqlite::Result<ShortLink> {
        let status: String = row.get(10)?;
        Ok(ShortLink {
            id: row.get(0)?,
            upload_id: row.get(1)?,
            provider: row.get(2)?,
            provider_name: row.get(3)?,
            provider_id: row.get(4)?,
            domain: row.get(5)?,
            short_url: row.get(6)?,
            target_url: row.get(7)?,
            created_at: row.get(8)?,
            expires_at: row.get(9)?,
            status: Status::from_raw(&status),
            clicks: row.get(11)?,
            last_click_at: row.get(12)?,
            stats_checked_at: row.get(13)?,
        })
    }

    pub fn insert(&self, link: &ShortLink) {
        let connection = self.connection.lock().unwrap();
        if let Err(error) = connection.execute(
            &format!("INSERT INTO short_links ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)"),
            params![
                link.id,
                link.upload_id,
                link.provider,
                link.provider_name,
                link.provider_id,
                link.domain,
                link.short_url,
                link.target_url,
                link.created_at,
                link.expires_at,
                link.status.raw_value(),
                link.clicks,
                link.last_click_at,
                link.stats_checked_at
            ],
        ) {
            log::error!("Could not save a short link: {error}");
        }
    }

    /// The upload's short links, newest first.
    pub fn for_upload(&self, upload_id: &str) -> Vec<ShortLink> {
        let connection = self.connection.lock().unwrap();
        let Ok(mut statement) =
            connection.prepare(&format!("SELECT {COLUMNS} FROM short_links WHERE upload_id = ?1 ORDER BY created_at DESC"))
        else {
            return Vec::new();
        };
        statement.query_map([upload_id], Self::from_row).map(|rows| rows.filter_map(Result::ok).collect()).unwrap_or_default()
    }

    pub fn get(&self, id: &str) -> Option<ShortLink> {
        let connection = self.connection.lock().unwrap();
        connection
            .query_row(&format!("SELECT {COLUMNS} FROM short_links WHERE id = ?1 COLLATE NOCASE"), [id], Self::from_row)
            .optional()
            .ok()
            .flatten()
    }

    pub fn set_statuses(&self, statuses: &HashMap<String, Status>) {
        let connection = self.connection.lock().unwrap();
        for (id, status) in statuses {
            if let Err(error) = connection.execute("UPDATE short_links SET status = ?2 WHERE id = ?1", params![id, status.raw_value()]) {
                log::error!("Could not update a short link: {error}");
            }
        }
    }

    pub fn set_target(&self, id: &str, target_url: &str) {
        let connection = self.connection.lock().unwrap();
        let _ = connection.execute("UPDATE short_links SET target_url = ?2 WHERE id = ?1", params![id, target_url]);
    }

    pub fn set_stats(&self, id: &str, clicks: Option<i64>, last_click_at: Option<i64>, checked_at: i64) {
        let connection = self.connection.lock().unwrap();
        let _ = connection.execute(
            "UPDATE short_links SET clicks = ?2, last_click_at = ?3, stats_checked_at = ?4 WHERE id = ?1",
            params![id, clicks, last_click_at, checked_at],
        );
    }

    /// Remove from History: the upload's records go (the links themselves
    /// are left alone).
    pub fn delete_for_upload(&self, upload_id: &str) {
        let connection = self.connection.lock().unwrap();
        let _ = connection.execute("DELETE FROM short_links WHERE upload_id = ?1", [upload_id]);
    }
}
