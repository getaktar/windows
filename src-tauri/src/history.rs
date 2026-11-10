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
    /// Unix milliseconds when the file was last replaced in place (same
    /// key and link), or none.
    pub replaced_at: Option<i64>,
    pub has_thumbnail: bool,
    /// The thumbnail file on this PC, for the windows to show.
    pub thumbnail_path: Option<String>,
    /// The upload's active short link (see `short_links`): what's copied
    /// and shown for it, and its record's ID and provider.
    pub short_url: Option<String>,
    pub short_link_id: Option<String>,
    pub short_provider: Option<String>,
    /// The key of the upload's share page or gallery page
    /// (docs/share-pages.md); none for an upload without one.
    pub share_page_key: Option<String>,
    /// An encrypted upload's key (base64url of the 32 bytes) and, for a
    /// password, its salt (hex), so Replace File keeps the link. Never
    /// sent to the windows: they get `share_url`.
    #[serde(skip_serializing)]
    pub encryption_key: Option<String>,
    #[serde(skip_serializing)]
    pub encryption_salt: Option<String>,
    /// The link to the share page (with the key in its fragment for an
    /// encrypted upload with the key in the link), worked out when read.
    pub share_url: Option<String>,
    /// Encrypted (docs/share-pages.md, part 5): its link is never formatted.
    pub encrypted: bool,
}

impl UploadRecord {
    /// Unix milliseconds of the last write to its key: the replace, or the
    /// upload. What a bucket object's date is compared with.
    pub fn last_write(&self) -> i64 {
        self.replaced_at.unwrap_or(self.created_at)
    }
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
    /// Any other source and its name, when it's not a watched folder:
    /// `request:<ID>` with the request's title, `sharex` with "ShareX".
    pub source: Option<(&'a str, &'a str)>,
    /// When it was uploaded, for one imported from elsewhere; none is now.
    pub created_at: Option<i64>,
    /// See `UploadRecord::share_page_key`.
    pub share_page_key: Option<&'a str>,
    /// See `UploadRecord::encryption_key`: the key and the salt.
    pub encryption: Option<(&'a str, Option<&'a str>)>,
}

/// What a replaced file changes in its history entry.
pub struct Replacement<'a> {
    pub byte_size: i64,
    pub content_hash: Option<&'a str>,
    pub mime_type: &'a str,
    /// Days until it's deleted, for a file under `tmp/{N}d/`.
    pub expire_after_days: Option<u32>,
}

pub struct History {
    /// Shared with the watched folders' ledger, which keeps its table in
    /// the same database.
    connection: Arc<Mutex<Connection>>,
    pub thumbnails: LocalStore,
}

macro_rules! columns {
    () => {
        "id, local_filename, object_key, public_url, destination_id, destination_name, mime_type, byte_size, created_at, expires_at, content_hash, source, source_name, replaced_at, share_page_key, encryption_key, encryption_salt"
    };
}
const COLUMNS: &str = columns!();
/// `COLUMNS` and the upload's active short link, for reading.
const SELECT_COLUMNS: &str = concat!(columns!(), ", ", crate::short_links::store::active_column!());

/// The share page's link for an upload whose page is at `page_key`: its
/// public URL sits where the file's does, with the key in the fragment
/// for an encrypted upload whose key is in the link.
pub fn share_url(public_url: &str, object_key: &str, page_key: &str, encryption_key: Option<&str>) -> String {
    let encoded_key = crate::output::encode_key_path(object_key);
    let base = public_url.strip_suffix(&encoded_key).unwrap_or(public_url);
    let page_url = format!("{base}{}", crate::output::encode_key_path(page_key));
    match encryption_key {
        Some(key) => format!("{page_url}#k={key}"),
        None => page_url,
    }
}

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

    /// The short links of uploads, in this database.
    pub fn short_links(&self) -> crate::short_links::store::ShortLinkStore {
        crate::short_links::store::ShortLinkStore::new(self.connection.clone())
    }

    /// The watched folders' ledger, in this database.
    pub fn ledger(&self) -> rusqlite::Result<crate::watched::ledger::Ledger> {
        crate::watched::ledger::Ledger::new(self.connection.clone())
    }

    fn record_from_row(&self, row: &Row) -> rusqlite::Result<UploadRecord> {
        let id: String = row.get(0)?;
        let thumbnail_path = self.thumbnails.path(&id).map(|path| path.to_string_lossy().into_owned());
        let short = crate::short_links::store::split_active(row.get(17)?);
        let share_page_key: Option<String> = row.get(14)?;
        let encryption_key: Option<String> = row.get(15)?;
        let encryption_salt: Option<String> = row.get(16)?;
        let public_url: String = row.get(3)?;
        let object_key: String = row.get(2)?;
        let link_key = encryption_key.as_deref().filter(|_| encryption_salt.is_none());
        Ok(UploadRecord {
            id,
            local_filename: row.get(1)?,
            share_url: share_page_key.as_deref().map(|key| share_url(&public_url, &object_key, key, link_key)),
            object_key,
            public_url,
            destination_id: row.get(4)?,
            destination_name: row.get(5)?,
            mime_type: row.get(6)?,
            byte_size: row.get(7)?,
            created_at: row.get(8)?,
            expires_at: row.get(9)?,
            content_hash: row.get(10)?,
            source: row.get(11)?,
            source_name: row.get(12)?,
            replaced_at: row.get(13)?,
            has_thumbnail: thumbnail_path.is_some(),
            thumbnail_path,
            short_link_id: short.as_ref().map(|(id, _, _)| id.clone()),
            short_provider: short.as_ref().map(|(_, provider, _)| provider.clone()),
            short_url: short.map(|(_, _, url)| url),
            share_page_key,
            encrypted: encryption_key.is_some(),
            encryption_key,
            encryption_salt,
        })
    }

    pub fn insert(&self, record: NewRecord) -> UploadRecord {
        let created_at = record.created_at.unwrap_or_else(crate::util::now_millis);
        let share_page_key = record.share_page_key.map(str::to_string);
        let encryption_key = record.encryption.map(|(key, _)| key.to_string());
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
            source: record.watched_folder.map(|(id, _)| watched_source(id)).or_else(|| record.source.map(|(source, _)| source.to_string())),
            source_name: record.watched_folder.or(record.source).map(|(_, name)| name.to_string()),
            replaced_at: None,
            has_thumbnail: false,
            thumbnail_path: None,
            short_url: None,
            short_link_id: None,
            short_provider: None,
            share_url: share_page_key.as_deref().map(|key| {
                let link_key = encryption_key.as_deref().filter(|_| record.encryption.is_some_and(|(_, salt)| salt.is_none()));
                share_url(record.public_url, record.object_key, key, link_key)
            }),
            share_page_key,
            encrypted: encryption_key.is_some(),
            encryption_key,
            encryption_salt: record.encryption.and_then(|(_, salt)| salt.map(str::to_string)),
        };
        let connection = self.connection.lock().unwrap();
        let result = connection.execute(
            &format!("INSERT INTO uploads ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)"),
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
                stored.source_name,
                stored.replaced_at,
                stored.share_page_key,
                stored.encryption_key,
                stored.encryption_salt
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
        let Ok(mut statement) = connection.prepare(&format!("SELECT {SELECT_COLUMNS} FROM uploads ORDER BY created_at DESC")) else {
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
            "SELECT {SELECT_COLUMNS} FROM uploads WHERE expires_at IS NOT NULL AND expires_at <= ?1 ORDER BY expires_at"
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
            "SELECT {SELECT_COLUMNS} FROM uploads WHERE destination_id = ?1 AND content_hash = ?2 ORDER BY created_at DESC"
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
            "SELECT {SELECT_COLUMNS} FROM uploads WHERE destination_id = ?1 AND object_key = ?2 ORDER BY created_at DESC"
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
            .query_row(&format!("SELECT {SELECT_COLUMNS} FROM uploads WHERE id = ?1 COLLATE NOCASE"), [id], |row| self.record_from_row(row))
            .optional()
            .ok()
            .flatten()
    }

    /// A file replaced in place: the entry keeps its ID, key, link, name and
    /// upload date, and gets the new file's size, hash and content type,
    /// a new expiry when it's under `tmp/{N}d/` (counted from now), and
    /// `replaced_at`.
    pub fn replaced(&self, id: &str, replacement: Replacement) -> Option<UploadRecord> {
        let now = crate::util::now_millis();
        let expires_at = replacement.expire_after_days.map(|days| crate::expiry::expires_at(now, days));
        {
            let connection = self.connection.lock().unwrap();
            if let Err(error) = connection.execute(
                "UPDATE uploads SET byte_size = ?2, content_hash = ?3, mime_type = ?4, expires_at = ?5, replaced_at = ?6 WHERE id = ?1",
                params![id, replacement.byte_size, replacement.content_hash, replacement.mime_type, expires_at, now],
            ) {
                log::error!("Could not update upload history: {error}");
            }
        }
        self.get(id)
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

    /// Whether another upload than `except_id` shares the page at
    /// `page_key` (the files of a gallery).
    pub fn others_with_page(&self, destination_id: &str, page_key: &str, except_id: &str) -> bool {
        let connection = self.connection.lock().unwrap();
        connection
            .query_row(
                "SELECT COUNT(*) FROM uploads WHERE destination_id = ?1 AND share_page_key = ?2 AND id != ?3",
                params![destination_id, page_key, except_id],
                |row| row.get::<_, i64>(0),
            )
            .map(|count| count > 0)
            .unwrap_or(true)
    }

    /// The files of a gallery all point to its page.
    pub fn set_share_page(&self, ids: &[String], page_key: &str) {
        let connection = self.connection.lock().unwrap();
        for id in ids {
            if let Err(error) = connection.execute("UPDATE uploads SET share_page_key = ?2 WHERE id = ?1", params![id, page_key]) {
                log::error!("Could not update upload history: {error}");
            }
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
    if !has_column(connection, "uploads", "replaced_at")? {
        connection.execute_batch("ALTER TABLE uploads ADD COLUMN replaced_at INTEGER;")?;
    }
    if !has_column(connection, "uploads", "share_page_key")? {
        connection.execute_batch(
            "ALTER TABLE uploads ADD COLUMN share_page_key TEXT; ALTER TABLE uploads ADD COLUMN encryption_key TEXT; ALTER TABLE uploads ADD COLUMN encryption_salt TEXT;",
        )?;
    }
    crate::short_links::store::prepare(connection)
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
            use_for: None,
            short_cache: None,
            cloudflare_zone_id: None,
            hooks: None,
            short_links: None,
            share_page: None,
            encryption: None,
            request_cors_origin: None,
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
    fn keeps_share_pages_and_encryption_keys() {
        assert_eq!(share_url("https://cdn.example.com/2026/a%20b.png", "2026/a b.png", "2026/a b.png.html", None), "https://cdn.example.com/2026/a%20b.png.html");
        assert_eq!(share_url("https://cdn.example.com/e/X", "e/X", "e/X.html", Some("KEY")), "https://cdn.example.com/e/X.html#k=KEY");
        let connection = Connection::open_in_memory().unwrap();
        prepare(&connection).unwrap();
        let history = History { connection: Arc::new(Mutex::new(connection)), thumbnails: LocalStore::open(std::env::temp_dir().join("aktar-history-test"), None) };
        let destination = destination();
        let new = |key: &'static str, page: Option<&'static str>, encryption: Option<(&'static str, Option<&'static str>)>| NewRecord {
            local_filename: "a.png",
            object_key: key,
            public_url: "",
            destination: &destination,
            mime_type: "image/png",
            byte_size: 5,
            expire_after_days: None,
            content_hash: None,
            watched_folder: None,
            source: Some(("request:R1", "Logs")),
            created_at: Some(1_000),
            share_page_key: page,
            encryption,
        };
        let plain = history.insert(NewRecord { public_url: "https://img.example.com/a.png", ..new("a.png", Some("a.png.html"), None) });
        let stored = history.get(&plain.id).unwrap();
        assert_eq!(stored.share_url.as_deref(), Some("https://img.example.com/a.png.html"));
        assert_eq!((stored.created_at, stored.source.as_deref(), stored.source_name.as_deref()), (1_000, Some("request:R1"), Some("Logs")));
        let linked = history.insert(NewRecord { public_url: "https://img.example.com/e/X", ..new("e/X", Some("e/X.html"), Some(("KEY", None))) });
        assert_eq!(history.get(&linked.id).unwrap().share_url.as_deref(), Some("https://img.example.com/e/X.html#k=KEY"));
        assert_eq!(linked.share_url.as_deref(), Some("https://img.example.com/e/X.html#k=KEY"));
        // A password's key never goes in the link.
        let password = history.insert(NewRecord { public_url: "https://img.example.com/e/Y", ..new("e/Y", Some("e/Y.html"), Some(("KEY", Some("00ff")))) });
        let stored = history.get(&password.id).unwrap();
        assert_eq!(stored.share_url.as_deref(), Some("https://img.example.com/e/Y.html"));
        assert_eq!((stored.encryption_key.as_deref(), stored.encryption_salt.as_deref()), (Some("KEY"), Some("00ff")));
        assert!(!serde_json::to_string(&stored).unwrap().contains("KEY"), "the key isn't sent to the windows");
        // A gallery: the page goes with the last of its files.
        let b = history.insert(new("b.png", None, None));
        history.set_share_page(&[plain.id.clone(), b.id.clone()], "a.png.gallery.html");
        assert!(history.others_with_page(&destination.id, "a.png.gallery.html", &plain.id));
        history.delete(&b.id);
        assert!(!history.others_with_page(&destination.id, "a.png.gallery.html", &plain.id));
        assert_eq!(history.get(&plain.id).unwrap().share_page_key.as_deref(), Some("a.png.gallery.html"));
    }

    #[test]
    fn replaces_in_place() {
        let connection = Connection::open_in_memory().unwrap();
        prepare(&connection).unwrap();
        assert!(has_column(&connection, "uploads", "replaced_at").unwrap());
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
            content_hash: Some("old"),
            watched_folder: None,
            source: None,
            created_at: None,
            share_page_key: None,
            encryption: None,
        });
        assert_eq!(record.replaced_at, None);
        let replaced = history
            .replaced(&record.id, Replacement { byte_size: 9, content_hash: Some("new"), mime_type: "image/jpeg", expire_after_days: Some(7) })
            .unwrap();
        // Same entry, key, link, name and upload date; the new file's facts.
        assert_eq!(replaced.id, record.id);
        assert_eq!(replaced.object_key, record.object_key);
        assert_eq!(replaced.public_url, record.public_url);
        assert_eq!(replaced.local_filename, "a.png");
        assert_eq!(replaced.created_at, record.created_at);
        assert_eq!((replaced.byte_size, replaced.content_hash.as_deref(), replaced.mime_type.as_str()), (9, Some("new"), "image/jpeg"));
        let replaced_at = replaced.replaced_at.unwrap();
        assert!(replaced_at >= record.created_at);
        // Its 7 days start again.
        assert_eq!(replaced.expires_at, Some(replaced_at + 7 * 86_400_000));
        assert_eq!(history.with_content(&destination.id, "new").len(), 1);
        assert!(history.with_content(&destination.id, "old").is_empty());
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
            source: None,
            created_at: None,
            share_page_key: None,
            encryption: None,
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
            source: None,
            created_at: None,
            share_page_key: None,
            encryption: None,
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
