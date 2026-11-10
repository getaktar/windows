//! File requests (docs/share-pages.md, part 4): a link where anyone who has
//! it can send files straight into the bucket until it expires. The page
//! PUTs each file and its metadata to presigned URLs under
//! `.aktar/requests/<id>/`; Aktar collects what arrives, runs it through
//! the destination's normal upload, and deletes the copy.
//!
//! Requests are kept on this PC only (`requests.json`, next to
//! destinations.json): not in the bucket, not transferred.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::core::{events, SharedCore};
use crate::destinations::DestinationConfig;
use crate::storage::{S3Provider, StorageError};
use crate::t;
use crate::uploads::{JobState, RequestSource, UploadInput};

/// How long a request link can be valid: 1 hour, 1 day, 7 days (SigV4
/// presigned URLs can't live longer).
pub const DURATIONS: [u64; 3] = [3600, 86_400, 604_800];
pub const SLOT_COUNTS: [u32; 4] = [1, 5, 10, 20];
pub const SIZE_LIMITS: [u64; 4] = [100_000_000, 1_000_000_000, 2_000_000_000, 5_000_000_000];
const MAX_TITLE_CHARS: usize = 100;
const MAX_NOTE_CHARS: usize = 1000;
const MAX_NAME_CHARS: usize = 200;
const MAX_META_BYTES: u64 = 4096;
/// A file sent without its metadata is taken once it's this old.
const META_WAIT_MILLIS: i64 = 10 * 60_000;
/// How long after a request expires it's still checked (a send that
/// started just before).
const GRACE_MILLIS: i64 = 3_600_000;
const CHECK_INTERVAL: Duration = Duration::from_secs(120);
pub const CORS_RULE_ID: &str = "aktar-file-requests";

/// A request, as kept in requests.json.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileRequest {
    pub id: String,
    #[serde(rename = "destinationID", alias = "destinationId")]
    pub destination_id: String,
    pub title: String,
    pub note: String,
    /// ISO 8601 UTC.
    pub created_at: String,
    pub expires_at: String,
    pub slot_count: u32,
    pub max_bytes: u64,
    pub link: String,
    #[serde(default)]
    pub received: u32,
    #[serde(default)]
    pub closed_at: Option<String>,
}

impl FileRequest {
    fn prefix(&self) -> String {
        prefix(&self.id)
    }

    fn expires_millis(&self) -> i64 {
        crate::util::parse_iso8601(&self.expires_at).unwrap_or(0)
    }

    /// Still checked: open, and not past its expiry plus the grace hour.
    fn is_checked_at(&self, now: i64) -> bool {
        self.closed_at.is_none() && now <= self.expires_millis() + GRACE_MILLIS
    }
}

/// Where a request's page and slots live in the bucket.
pub fn prefix(id: &str) -> String {
    format!(".aktar/requests/{id}/")
}

pub struct RequestStore {
    path: PathBuf,
    requests: Mutex<Vec<FileRequest>>,
    /// Slots being collected right now, "<request id>/<slot>".
    collecting: Mutex<HashSet<String>>,
    /// One check at a time.
    checking: tokio::sync::Mutex<()>,
}

impl RequestStore {
    pub fn load(directory: &Path) -> Self {
        let path = directory.join("requests.json");
        let requests = std::fs::read(&path).ok().and_then(|data| serde_json::from_slice(&data).ok()).unwrap_or_default();
        Self { path, requests: Mutex::new(requests), collecting: Mutex::new(HashSet::new()), checking: tokio::sync::Mutex::new(()) }
    }

    /// Newest first.
    pub fn all(&self) -> Vec<FileRequest> {
        let mut requests = self.requests.lock().unwrap().clone();
        requests.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        requests
    }

    fn get(&self, id: &str) -> Option<FileRequest> {
        self.requests.lock().unwrap().iter().find(|request| request.id == id).cloned()
    }

    fn mutate(&self, change: impl FnOnce(&mut Vec<FileRequest>)) {
        let mut requests = self.requests.lock().unwrap();
        change(&mut requests);
        crate::util::write_json_atomically(&self.path, &*requests);
    }

    fn update(&self, id: &str, change: impl FnOnce(&mut FileRequest)) {
        self.mutate(|requests| {
            if let Some(request) = requests.iter_mut().find(|request| request.id == id) {
                change(request);
            }
        });
    }

    /// Forgets a closed request.
    pub fn remove(&self, id: &str) {
        self.mutate(|requests| requests.retain(|request| request.id != id || request.closed_at.is_none()));
    }
}

// MARK: - Rules

/// A request's ID: 10 random characters from `[0-9A-Za-z]`.
pub fn new_id() -> String {
    crate::share_page::random_alphanumeric(10)
}

/// The keys of slot `index` (1-based): the file and its metadata.
pub fn slot_keys(id: &str, index: u32) -> (String, String) {
    let prefix = prefix(id);
    (format!("{prefix}{index}"), format!("{prefix}{index}.json"))
}

/// A sender's file name, made safe: its last path component, without
/// control characters or `/ \ :`, at most 200 characters, never empty.
pub fn sanitized_name(raw: &str, index: u32) -> String {
    let last = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let cleaned: String = last.chars().filter(|c| !c.is_control() && !matches!(c, '/' | '\\' | ':')).take(MAX_NAME_CHARS).collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        format!("upload-{index}")
    } else {
        cleaned.to_string()
    }
}

/// The name in a slot's metadata (`{ v, name, type, size, sent }`); its
/// `type` is ignored, the destination decides the Content-Type.
pub fn name_from_meta(data: &[u8], index: u32) -> String {
    let name = serde_json::from_slice::<serde_json::Value>(data)
        .ok()
        .and_then(|meta| meta.get("name").and_then(serde_json::Value::as_str).map(str::to_string))
        .unwrap_or_default();
    sanitized_name(&name, index)
}

/// What to do with a slot, from what's listed under the request's prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotState {
    Empty,
    /// The file is there but its metadata isn't yet: wait.
    Waiting,
    /// Take it: with its metadata, or without once it's 10 minutes old.
    Ready { has_meta: bool, size: i64 },
}

/// Pairs slot `index`'s file with its metadata in `objects` (key, size,
/// last modified).
pub fn slot_state(objects: &[(String, i64, Option<i64>)], id: &str, index: u32, now: i64) -> SlotState {
    let (file_key, meta_key) = slot_keys(id, index);
    let Some((_, size, modified)) = objects.iter().find(|(key, _, _)| *key == file_key) else { return SlotState::Empty };
    if objects.iter().any(|(key, _, _)| *key == meta_key) {
        return SlotState::Ready { has_meta: true, size: *size };
    }
    match modified {
        Some(modified) if now - modified >= META_WAIT_MILLIS => SlotState::Ready { has_meta: false, size: *size },
        _ => SlotState::Waiting,
    }
}

/// A CORS rule, as S3 has it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct CorsRule {
    #[serde(rename = "ID", skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub allowed_origins: Vec<String>,
    pub allowed_methods: Vec<String>,
    pub allowed_headers: Vec<String>,
    pub expose_headers: Vec<String>,
    pub max_age_seconds: Option<i32>,
}

/// The rule that lets the page at `origin` send files.
pub fn request_rule(origin: &str) -> CorsRule {
    CorsRule {
        id: Some(CORS_RULE_ID.into()),
        allowed_origins: vec![origin.to_string()],
        allowed_methods: vec!["PUT".into()],
        allowed_headers: vec!["content-type".into(), "if-none-match".into()],
        expose_headers: vec!["etag".into()],
        max_age_seconds: Some(3600),
    }
}

/// The bucket's rules with Aktar's for `origin` added, or put in place of
/// the one it had (matched by ID, or, from a provider that drops IDs, by
/// the same origin with PUT). Every other rule stays. None when the rule
/// is there already as it should be.
pub fn merged_cors(existing: &[CorsRule], origin: &str) -> Option<Vec<CorsRule>> {
    let rule = request_rule(origin);
    let is_ours = |other: &CorsRule| {
        other.id.as_deref() == Some(CORS_RULE_ID)
            || (other.id.is_none() && other.allowed_origins == rule.allowed_origins && other.allowed_methods.iter().any(|method| method.eq_ignore_ascii_case("PUT")))
    };
    let mut rules = Vec::with_capacity(existing.len() + 1);
    let mut placed = false;
    let mut unchanged = false;
    for other in existing {
        if is_ours(other) {
            if !placed {
                unchanged = covers(other, &rule);
                rules.push(rule.clone());
                placed = true;
            }
        } else {
            rules.push(other.clone());
        }
    }
    if !placed {
        rules.push(rule);
    }
    (!(placed && unchanged && rules.len() == existing.len())).then_some(rules)
}

/// Whether `rule` already allows what `wanted` does (a provider may have
/// dropped the ID or changed the case).
fn covers(rule: &CorsRule, wanted: &CorsRule) -> bool {
    let has_all = |have: &[String], want: &[String]| want.iter().all(|item| have.iter().any(|other| other.eq_ignore_ascii_case(item) || other == "*"));
    has_all(&rule.allowed_origins, &wanted.allowed_origins)
        && has_all(&rule.allowed_methods, &wanted.allowed_methods)
        && has_all(&rule.allowed_headers, &wanted.allowed_headers)
        && has_all(&rule.expose_headers, &wanted.expose_headers)
}

/// The origin a URL is served from.
pub fn origin_of(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    parsed.host_str()?;
    Some(parsed.origin().ascii_serialization())
}

/// Where the request page is served from: the public base URL, or, on a
/// temporary-link destination, the S3 endpoint itself.
pub fn page_origin(destination: &DestinationConfig) -> Option<String> {
    if destination.temporary_link.is_some() {
        return crate::storage::object_origin(destination);
    }
    origin_of(&crate::output::resolve_public_url(&destination.public_base_url, "x"))
}

// MARK: - Creating

/// What the "Request Files" dialog asks for.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewRequest {
    pub destination_id: Option<String>,
    pub title: String,
    pub note: String,
    pub seconds: u64,
    pub slot_count: u32,
    pub max_bytes: u64,
}

/// Why a request couldn't be made.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum CreateError {
    /// The bucket refused the CORS rule: what to allow by hand.
    Cors { message: String, rule: String },
    Failed { message: String },
}

impl From<String> for CreateError {
    fn from(message: String) -> Self {
        CreateError::Failed { message }
    }
}

impl From<StorageError> for CreateError {
    fn from(error: StorageError) -> Self {
        CreateError::Failed { message: error.to_string() }
    }
}

fn clipped(text: &str, max: usize) -> String {
    text.trim().chars().take(max).collect()
}

/// Makes a request: CORS when the page uploads to another origin, the
/// presigned slots, the page, and its link.
pub async fn create(core: &SharedCore, new: NewRequest) -> Result<FileRequest, CreateError> {
    let destination = core
        .destinations
        .find(new.destination_id.as_deref())
        .ok_or_else(|| t!("This upload’s destination was removed."))?;
    if !DURATIONS.contains(&new.seconds) || !SLOT_COUNTS.contains(&new.slot_count) || !SIZE_LIMITS.contains(&new.max_bytes) {
        return Err(t!("The request could not be made.").into());
    }
    let credentials = crate::credentials::load(&destination.id).map_err(|error| error.to_string())?;
    let provider = S3Provider::new(destination.clone(), credentials);
    ensure_cors(core, &provider, &destination).await?;

    let id = new_id();
    let now = crate::util::now_millis();
    let expires = now + new.seconds as i64 * 1000;
    let conditional = destination.preset.supports_conditional_writes();
    let mut slots = Vec::new();
    for index in 1..=new.slot_count {
        let (file_key, meta_key) = slot_keys(&id, index);
        slots.push(crate::share_page::Slot {
            file: provider.presigned_put(&file_key, "application/octet-stream", conditional, new.seconds).await?,
            meta: provider.presigned_put(&meta_key, "application/json", conditional, new.seconds).await?,
        });
    }
    let title = clipped(&new.title, MAX_TITLE_CHARS);
    let note = clipped(&new.note, MAX_NOTE_CHARS);
    let mut page = crate::share_page::Page::new(crate::share_page::Kind::Request, &crate::i18n::current(), now);
    page.expires = Some(crate::share_page::iso8601(expires));
    page.request = Some(crate::share_page::Request { title: title.clone(), note: note.clone(), max_bytes: new.max_bytes, conditional, slots });
    let page_key = format!("{}index.html", prefix(&id));
    provider.put_page(&page_key, crate::share_page::build_page(&page)).await?;
    let link = match destination.temporary_link {
        Some(_) => provider.temporary_url(&page_key, new.seconds).await?,
        None => crate::output::resolve_public_url(&destination.public_base_url, &page_key),
    };
    let request = FileRequest {
        id,
        destination_id: destination.id.clone(),
        title,
        note,
        created_at: crate::share_page::iso8601(now),
        expires_at: crate::share_page::iso8601(expires),
        slot_count: new.slot_count,
        max_bytes: new.max_bytes,
        link,
        received: 0,
        closed_at: None,
    };
    core.requests.mutate(|requests| requests.push(request.clone()));
    core.notify(events::REQUESTS_CHANGED);
    Ok(request)
}

/// Adds Aktar's CORS rule when the page will upload to another origin,
/// once per destination and origin.
async fn ensure_cors(core: &SharedCore, provider: &S3Provider, destination: &DestinationConfig) -> Result<(), CreateError> {
    let (Some(page), Some(target)) = (page_origin(destination), crate::storage::object_origin(destination)) else {
        return Err(t!("The endpoint URL is not valid.").into());
    };
    if page == target || destination.request_cors_origin.as_deref() == Some(page.as_str()) {
        return Ok(());
    }
    let refused = |page: &str| CreateError::Cors {
        message: t!("This bucket doesn’t let browsers upload to it. Allow PUT from {0} in its CORS settings, then try again.", page),
        rule: serde_json::to_string_pretty(&[request_rule(page)]).unwrap_or_default(),
    };
    let existing = provider.bucket_cors().await.map_err(|error| {
        log::info!("Could not read the bucket's CORS rules: {error}");
        refused(&page)
    })?;
    if let Some(rules) = merged_cors(&existing, &page) {
        provider.set_bucket_cors(&rules).await.map_err(|error| {
            log::info!("Could not set the bucket's CORS rules: {error}");
            refused(&page)
        })?;
    }
    core.destinations.modify(&destination.id, |saved| saved.request_cors_origin = Some(page.clone()));
    core.notify(events::DESTINATIONS_CHANGED);
    Ok(())
}

// MARK: - Collecting

/// Checks open requests at launch and every 2 minutes.
pub fn schedule(core: &SharedCore) {
    let core = core.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            check_all(&core).await;
            tokio::time::sleep(CHECK_INTERVAL).await;
        }
    });
}

/// "Check Now", and the timer's check: collects what arrived, and closes
/// what has expired.
pub async fn check_all(core: &SharedCore) {
    let _checking = core.requests.checking.lock().await;
    let now = crate::util::now_millis();
    for request in core.requests.all().into_iter().filter(|request| request.closed_at.is_none()) {
        let Some(destination) = core.destinations.find(Some(&request.destination_id)).filter(|destination| destination.id == request.destination_id) else {
            core.requests.update(&request.id, |request| request.closed_at = Some(crate::share_page::iso8601(now)));
            continue;
        };
        let Ok(credentials) = crate::credentials::load(&destination.id) else { continue };
        let provider = S3Provider::new(destination.clone(), credentials);
        match collect(core, &provider, &destination, &request, now).await {
            Ok(0) => {}
            Ok(found) => {
                let title = if request.title.is_empty() { t!("File Request") } else { request.title.clone() };
                let body = if found == 1 { t!("Received 1 file") } else { t!("Received {0} files", found) };
                crate::uploads::show_notification(core, &title, &body);
            }
            Err(error) => log::info!("Could not check file request {}: {error}", request.id),
        }
        // That was its last check.
        if !request.is_checked_at(now) {
            let _ = close_with(core, &provider, &request.id).await;
        }
    }
    core.notify(events::REQUESTS_CHANGED);
}

/// Starts collecting each slot that has a file; returns how many.
async fn collect(core: &SharedCore, provider: &S3Provider, destination: &DestinationConfig, request: &FileRequest, now: i64) -> Result<usize, StorageError> {
    let mut objects = Vec::new();
    let mut token = None;
    loop {
        let listing = provider.list_recursively(&request.prefix(), token).await?;
        objects.extend(listing.objects.into_iter().map(|object| (object.key, object.size, object.last_modified)));
        token = listing.next_continuation_token;
        if token.is_none() {
            break;
        }
    }
    let mut found = 0;
    for index in 1..=request.slot_count {
        let SlotState::Ready { has_meta, size } = slot_state(&objects, &request.id, index, now) else { continue };
        let busy = format!("{}/{index}", request.id);
        if !core.requests.collecting.lock().unwrap().insert(busy.clone()) {
            continue;
        }
        let (file_key, meta_key) = slot_keys(&request.id, index);
        let limit = request.max_bytes + 1_000_000;
        // Bigger than the page allows: deleted unread.
        if size < 0 || size as u64 > limit {
            let _ = provider.delete_many(&[file_key, meta_key]).await;
            core.requests.collecting.lock().unwrap().remove(&busy);
            continue;
        }
        let name = match has_meta {
            true => match provider.get_small(&meta_key, MAX_META_BYTES).await {
                Ok(Some((data, _))) => name_from_meta(&data, index),
                _ => sanitized_name("", index),
            },
            false => sanitized_name("", index),
        };
        // Saved under a plain name (the sender's could be one Windows
        // can't use); the upload goes by the sender's.
        let folder = std::env::temp_dir().join(format!("aktar-request-{}", crate::util::new_id()));
        let extension = crate::util::split_extension(&name).1;
        let local_name = match extension.chars().all(|c| c.is_ascii_alphanumeric()) && !extension.is_empty() && extension.len() <= 16 {
            true => format!("file.{extension}"),
            false => "file".to_string(),
        };
        let path = folder.join(local_name);
        let downloaded = match std::fs::create_dir_all(&folder) {
            Ok(()) => provider.download(&file_key, &path, limit).await,
            Err(error) => Err(StorageError::Unknown(error.to_string())),
        };
        match downloaded {
            Ok(true) => {}
            Ok(false) => {
                let _ = std::fs::remove_dir_all(&folder);
                core.requests.collecting.lock().unwrap().remove(&busy);
                continue;
            }
            // Tried again on the next check.
            Err(error) => {
                log::info!("Could not download a file sent to request {}: {error}", request.id);
                let _ = std::fs::remove_dir_all(&folder);
                core.requests.collecting.lock().unwrap().remove(&busy);
                continue;
            }
        }
        let input = UploadInput {
            original_filename: name,
            temporary_folder: Some(folder),
            request: Some(RequestSource { request_id: request.id.clone(), title: request.title.clone() }),
            ..UploadInput::from_path(path)
        };
        let Some(queued) = crate::uploads::enqueue(core, vec![input], Some(destination.clone())).into_iter().next() else {
            core.requests.collecting.lock().unwrap().remove(&busy);
            continue;
        };
        found += 1;
        // Once it's in history, the copy in the request's folder goes.
        let (core, destination, request_id) = (core.clone(), destination.clone(), request.id.clone());
        let mut receiver = queued.receiver;
        tauri::async_runtime::spawn(async move {
            loop {
                let state = receiver.borrow_and_update().clone();
                match state {
                    JobState::Succeeded { .. } => {
                        if let Ok(credentials) = crate::credentials::load(&destination.id) {
                            let _ = S3Provider::new(destination.clone(), credentials).delete_many(&[file_key, meta_key]).await;
                        }
                        core.requests.update(&request_id, |request| request.received += 1);
                        core.notify(events::REQUESTS_CHANGED);
                        break;
                    }
                    JobState::Cancelled => break,
                    _ => {}
                }
                if receiver.changed().await.is_err() {
                    break;
                }
            }
            core.requests.collecting.lock().unwrap().remove(&busy);
        });
    }
    Ok(found)
}

/// "Close Now": everything under the request's folder is deleted (the link
/// stops working at once), and it's marked closed.
pub async fn close(core: &SharedCore, id: &str) -> Result<(), String> {
    let request = core.requests.get(id).ok_or_else(|| t!("The request could not be made."))?;
    let destination = core.destinations.find(Some(&request.destination_id)).filter(|destination| destination.id == request.destination_id);
    match destination.and_then(|destination| crate::credentials::load(&destination.id).ok().map(|credentials| S3Provider::new(destination, credentials))) {
        Some(provider) => close_with(core, &provider, id).await.map_err(|error| error.to_string())?,
        None => core.requests.update(id, |request| request.closed_at = Some(crate::share_page::iso8601(crate::util::now_millis()))),
    }
    core.notify(events::REQUESTS_CHANGED);
    Ok(())
}

async fn close_with(core: &SharedCore, provider: &S3Provider, id: &str) -> Result<(), StorageError> {
    let mut keys = Vec::new();
    let mut token = None;
    loop {
        let listing = provider.list_recursively(&prefix(id), token).await?;
        keys.extend(listing.objects.into_iter().map(|object| object.key));
        token = listing.next_continuation_token;
        if token.is_none() {
            break;
        }
    }
    if !keys.is_empty() {
        provider.delete_many(&keys).await?;
    }
    core.requests.update(id, |request| request.closed_at = Some(crate::share_page::iso8601(crate::util::now_millis())));
    Ok(())
}

/// A destination is being removed: its requests close (while its keys
/// can still delete their folders).
pub fn close_for_destination(core: &SharedCore, destination: &DestinationConfig) {
    let open: Vec<String> = core
        .requests
        .all()
        .into_iter()
        .filter(|request| request.destination_id == destination.id && request.closed_at.is_none())
        .map(|request| request.id)
        .collect();
    if open.is_empty() {
        return;
    }
    let now = crate::share_page::iso8601(crate::util::now_millis());
    for id in &open {
        core.requests.update(id, |request| request.closed_at = Some(now.clone()));
    }
    if let Ok(credentials) = crate::credentials::load(&destination.id) {
        let provider = S3Provider::new(destination.clone(), credentials);
        tauri::async_runtime::spawn(async move {
            for id in open {
                let mut token = None;
                let mut keys = Vec::new();
                while let Ok(listing) = provider.list_recursively(&prefix(&id), token).await {
                    keys.extend(listing.objects.into_iter().map(|object| object.key));
                    token = listing.next_continuation_token;
                    if token.is_none() {
                        break;
                    }
                }
                if !keys.is_empty() {
                    let _ = provider.delete_many(&keys).await;
                }
            }
        });
    }
    core.notify(events::REQUESTS_CHANGED);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_names() {
        assert_eq!(sanitized_name("report.pdf", 1), "report.pdf");
        assert_eq!(sanitized_name("../../etc/passwd", 2), "passwd");
        assert_eq!(sanitized_name("C:\\Users\\x\\a.txt", 3), "a.txt");
        assert_eq!(sanitized_name("a:b\u{0}c\nd.txt", 4), "abcd.txt");
        assert_eq!(sanitized_name("", 5), "upload-5");
        assert_eq!(sanitized_name("  ", 6), "upload-6");
        assert_eq!(sanitized_name("..", 7), "upload-7");
        assert_eq!(sanitized_name("dir/", 8), "upload-8");
        assert_eq!(sanitized_name(&"é".repeat(300), 9).chars().count(), 200);
        assert_eq!(name_from_meta(br#"{"v":1,"name":"shot.png","type":"text/html","size":3,"sent":"2026-11-09T12:00:00Z"}"#, 1), "shot.png");
        assert_eq!(name_from_meta(b"not json", 2), "upload-2");
        assert_eq!(name_from_meta(br#"{"name":42}"#, 3), "upload-3");
    }

    #[test]
    fn pairs_slots_with_their_metadata() {
        let now = 100 * 60_000;
        let objects = vec![
            (".aktar/requests/abc/index.html".to_string(), 10, Some(0)),
            (".aktar/requests/abc/1".to_string(), 5, Some(now - 1000)),
            (".aktar/requests/abc/1.json".to_string(), 80, Some(now - 1000)),
            (".aktar/requests/abc/2".to_string(), 7, Some(now - 1000)),
            (".aktar/requests/abc/3".to_string(), 9, Some(now - META_WAIT_MILLIS)),
            (".aktar/requests/abc/12".to_string(), 1, Some(now)),
        ];
        assert_eq!(slot_state(&objects, "abc", 1, now), SlotState::Ready { has_meta: true, size: 5 });
        assert_eq!(slot_state(&objects, "abc", 2, now), SlotState::Waiting);
        assert_eq!(slot_state(&objects, "abc", 3, now), SlotState::Ready { has_meta: false, size: 9 });
        assert_eq!(slot_state(&objects, "abc", 4, now), SlotState::Empty);
        // "1" is not "12".
        assert_eq!(slot_state(&objects, "abc", 12, now), SlotState::Waiting);
        assert_eq!(slot_keys("abc", 3), (".aktar/requests/abc/3".into(), ".aktar/requests/abc/3.json".into()));
        let id = new_id();
        assert!(id.len() == 10 && id.bytes().all(|byte| byte.is_ascii_alphanumeric()));
    }

    #[test]
    fn merges_the_cors_rule() {
        let origin = "https://cdn.example.com";
        let other = CorsRule {
            id: Some("site".into()),
            allowed_origins: vec!["https://example.com".into()],
            allowed_methods: vec!["GET".into()],
            allowed_headers: vec![],
            expose_headers: vec![],
            max_age_seconds: None,
        };
        // Added next to the rules there are.
        let merged = merged_cors(std::slice::from_ref(&other), origin).unwrap();
        assert_eq!(merged, vec![other.clone(), request_rule(origin)]);
        // Already there: nothing to write.
        assert_eq!(merged_cors(&merged, origin), None);
        // Another origin replaces it, by ID.
        let moved = merged_cors(&merged, "https://files.example.com").unwrap();
        assert_eq!(moved, vec![other.clone(), request_rule("https://files.example.com")]);
        // A provider that drops IDs: matched by origin and PUT.
        let without_id = CorsRule { id: None, ..request_rule(origin) };
        assert_eq!(merged_cors(&[without_id.clone(), other.clone()], origin), None);
        let narrower = CorsRule { allowed_headers: vec!["content-type".into()], ..without_id };
        assert_eq!(merged_cors(&[narrower, other.clone()], origin).unwrap(), vec![request_rule(origin), other.clone()]);
        // The JSON shown when the bucket refuses.
        let json = serde_json::to_string(&request_rule(origin)).unwrap();
        assert_eq!(
            json,
            r#"{"ID":"aktar-file-requests","AllowedOrigins":["https://cdn.example.com"],"AllowedMethods":["PUT"],"AllowedHeaders":["content-type","if-none-match"],"ExposeHeaders":["etag"],"MaxAgeSeconds":3600}"#
        );
    }

    #[test]
    fn finds_origins() {
        assert_eq!(origin_of("https://cdn.example.com/a/b").as_deref(), Some("https://cdn.example.com"));
        assert_eq!(origin_of("http://127.0.0.1:9000/bucket/x").as_deref(), Some("http://127.0.0.1:9000"));
        assert_eq!(origin_of("not a url"), None);
    }

    #[test]
    fn reads_requests_json() {
        let json = r#"[{"id":"abc","destinationID":"D","title":"","note":"","createdAt":"2026-11-09T12:00:00Z","expiresAt":"2026-11-10T12:00:00Z","slotCount":10,"maxBytes":2000000000,"link":"https://x","received":2,"closedAt":null}]"#;
        let requests: Vec<FileRequest> = serde_json::from_str(json).unwrap();
        let request = &requests[0];
        assert_eq!(request.received, 2);
        let expires = crate::util::parse_iso8601("2026-11-10T12:00:00Z").unwrap();
        assert!(request.is_checked_at(expires + GRACE_MILLIS));
        assert!(!request.is_checked_at(expires + GRACE_MILLIS + 1));
        assert_eq!(serde_json::to_string(&requests).unwrap(), json);
    }
}
