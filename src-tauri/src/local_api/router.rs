//! Maps local API requests onto the same pieces the app's own windows use,
//! so an upload started from Raycast shows up in the panel, lands in
//! history, and follows the Output settings exactly like one dropped on the
//! panel.
//!
//! ```text
//! GET    /v1/status
//! GET    /v1/destinations
//! GET    /v1/uploads?query=&destinationId=&limit=
//! POST   /v1/uploads?filename=&destinationId=&prefix=&expires=     (raw file bytes)
//! POST   /v1/uploads/clipboard?destinationId=&expires=
//! DELETE /v1/uploads/{id}
//! GET    /v1/destinations/{id}/objects?prefix=&continuationToken=
//! DELETE /v1/destinations/{id}/objects?key=
//! POST   /v1/destinations/{id}/objects/move                {"from", "to"}
//! POST   /v1/destinations/{id}/folders                     {"prefix", "name"}
//! POST   /v1/destinations/{id}/links                       {"key", "expiresIn"}
//! GET    /v1/watched-folders
//! POST   /v1/watched-folders/pause                         {"minutes"} (none: until resumed)
//! POST   /v1/watched-folders/resume
//! POST   /v1/watched-folders/{id}                          {"enabled"}
//! ```
//!
//! An upload's response is `{"upload": {..., "reused": false}, "reused": false}`; `reused`
//! is true when the same file was already uploaded there and that upload's
//! link was used instead (Settings > General > Reuse links for duplicate
//! files). Uploads with `prefix` are never reused.
//!
//! `expires` is a number of days (1, 7, 14, or 30) after which the upload is
//! deleted. Left out or 0, it's kept: the app's "Delete after" setting never
//! applies here, so a script is never surprised by a file disappearing. It's
//! refused (409) for a destination whose bucket doesn't have Aktar's
//! lifecycle rules yet, since nothing would delete the file.

use serde::{Deserialize, Serialize};

use super::server::{Request, Response};
use super::API_VERSION;
use crate::bucket;
use crate::core::{events, SharedCore};
use crate::credentials::{self, CredentialError};
use crate::destinations::DestinationConfig;
use crate::history::UploadRecord;
use crate::output::{self, resolve_public_url, OutputMode};
use crate::storage::{BucketListing, BucketObject, S3Provider};
use crate::uploads::{self, Expiry, JobState, UploadInput};
use crate::util::iso8601;

pub async fn handle(core: &SharedCore, request: Request) -> Response {
    let parts: Vec<&str> = request.path.split('/').filter(|part| !part.is_empty()).collect();
    if parts.first() != Some(&"v1") {
        return Response::error(404, "Not found.");
    }
    let route = &parts[1..];

    match (request.method.as_str(), route) {
        ("GET", ["status"]) => status(core),
        ("GET", ["destinations"]) => {
            let destinations: Vec<DestinationDto> = core.destinations.all().iter().map(|d| destination_dto(core, d)).collect();
            Response::json(200, serde_json::json!({ "destinations": destinations }))
        }
        ("GET", ["uploads"]) => list_uploads(core, &request),
        ("POST", ["uploads"]) => upload_body(core, &request).await,
        ("POST", ["uploads", "clipboard"]) => upload_clipboard(core, &request).await,
        ("DELETE", ["uploads", id]) => delete_upload(core, id).await,
        ("GET", ["watched-folders"]) => Response::json(200, watched_dto(core)),
        ("POST", ["watched-folders", "pause"]) => {
            let body: PauseBody = match decode_optional(&request) {
                Ok(body) => body,
                Err(response) => return response,
            };
            let minutes = match body.minutes.as_ref().map(crate::watched::pause_minutes) {
                None => None,
                Some(Some(minutes)) => Some(minutes),
                Some(None) => return Response::error(400, "minutes must be a whole number from 1 to 525600, or left out to pause until resumed."),
            };
            let blocking_core = core.clone();
            let _ = tauri::async_runtime::spawn_blocking(move || crate::watched::pause(&blocking_core, minutes)).await;
            Response::json(200, watched_dto(core))
        }
        ("POST", ["watched-folders", "resume"]) => {
            let blocking_core = core.clone();
            let _ = tauri::async_runtime::spawn_blocking(move || crate::watched::resume(&blocking_core)).await;
            Response::json(200, watched_dto(core))
        }
        ("POST", ["watched-folders", id]) => {
            let body: EnabledBody = match decode(&request) {
                Ok(body) => body,
                Err(response) => return response,
            };
            if core.watched.engine.store.folder(id).is_none() {
                return Response::error(404, "No watched folder with that ID.");
            }
            let (blocking_core, folder_id, enabled) = (core.clone(), id.to_string(), body.enabled);
            let _ = tauri::async_runtime::spawn_blocking(move || blocking_core.watched.engine.set_enabled(&folder_id, enabled)).await;
            core.watched.wake();
            match watched_dto(core).folders.into_iter().find(|folder| folder.id.eq_ignore_ascii_case(id)) {
                Some(folder) => Response::json(200, folder),
                None => Response::error(404, "No watched folder with that ID."),
            }
        }
        (_, ["destinations", id, rest @ ..]) if !rest.is_empty() => {
            let Some(destination) = core.destinations.find(Some(id)) else {
                return Response::error(404, "No destination with that ID.");
            };
            let rest: Vec<&str> = rest.to_vec();
            handle_bucket(core, &request, destination, &rest).await
        }
        _ => Response::error(404, "Not found."),
    }
}

// MARK: - Status & history

fn status(core: &SharedCore) -> Response {
    let info = core.app.package_info();
    let settings = core.settings.get();
    Response::json(
        200,
        StatusDto {
            app: "Aktar",
            version: info.version.to_string(),
            build: info.version.to_string(),
            api_version: API_VERSION,
            default_destination_id: core.destinations.default_destination().map(|d| d.id),
            output_format: settings.output_mode.raw_value(),
            platform: "windows",
            watching: {
                let overview = core.watched.overview();
                WatchingDto { paused: overview.paused, folders: overview.folders.len() }
            },
        },
    )
}

fn list_uploads(core: &SharedCore, request: &Request) -> Response {
    // Same as the Mac app: a negative limit means 1, and a destinationId
    // that isn't a UUID at all is ignored rather than matching nothing.
    let limit = request
        .query
        .get("limit")
        .and_then(|value| value.trim().parse::<i64>().ok())
        .unwrap_or(200)
        .clamp(1, 1000) as usize;
    let query = request.query.get("query").map(|q| q.trim().to_lowercase()).unwrap_or_default();
    let destination_id = request
        .query
        .get("destinationId")
        .filter(|id| uuid::Uuid::parse_str(id).is_ok());

    let uploads: Vec<UploadDto> = core
        .history
        .all()
        .into_iter()
        .filter(|record| destination_id.is_none_or(|id| record.destination_id.eq_ignore_ascii_case(id)))
        .filter(|record| {
            query.is_empty()
                || record.local_filename.to_lowercase().contains(&query)
                || record.object_key.to_lowercase().contains(&query)
        })
        .take(limit)
        .map(|record| upload_dto(core, &record))
        .collect();
    Response::json(200, serde_json::json!({ "uploads": uploads }))
}

async fn delete_upload(core: &SharedCore, id: &str) -> Response {
    let Some(record) = core.history.get(id) else {
        return Response::error(404, "No upload with that ID.");
    };
    match uploads::delete_remote(core, &record.id).await {
        Ok(()) => Response::json(200, serde_json::json!({ "deleted": id })),
        Err(message) => Response::error(502, message),
    }
}

// MARK: - Uploading

/// Callers send the file's bytes rather than a path, same as on the Mac.
/// They're written back out under the original name (the content type comes
/// from the extension) and handed to the upload manager like any other file.
async fn upload_body(core: &SharedCore, request: &Request) -> Response {
    let filename = crate::util::last_component(request.query.get("filename").map(String::as_str).unwrap_or_default()).to_string();
    if filename.is_empty() || filename == "." || filename == ".." {
        return Response::error(400, "The filename query parameter is required.");
    }
    // One segment of a key, without control characters.
    let filename = output::key_segment(&filename);
    let prefix = match request.query.get("prefix").map(|raw| bucket::checked_folder(raw)) {
        Some(Err(message)) => return Response::error(400, message),
        Some(Ok(prefix)) => Some(prefix),
        None => None,
    };
    let Some(destination) = core.destinations.find(request.query.get("destinationId").map(String::as_str)) else {
        return Response::error(404, "No destination to upload to. Add one in Aktar's Settings.");
    };
    let expiry = match expiry_from(request, crate::expiry::is_active(core, &destination.id)) {
        Ok(expiry) => expiry,
        Err(response) => return response,
    };
    // A prefix picks the exact key, and exact keys never expire.
    if request.query.contains_key("prefix") && expiry != Expiry::Never {
        return Response::error(400, "The expires parameter can't be combined with prefix.");
    }

    // The file is staged under a fixed name: the caller's name only becomes
    // the object key and history entry. Joining it into a path would let
    // a name like "C:evil.dll" escape the staging folder, and names Windows
    // can't store (CON, "a?b") would fail.
    let directory = std::env::temp_dir().join("AktarLocalAPI").join(format!("upload-{}", crate::util::new_id()));
    let path = directory.join("upload");
    let staged = async {
        tokio::fs::create_dir_all(&directory).await?;
        match &request.body_file {
            Some(body_file) => tokio::fs::rename(body_file, &path).await,
            None => tokio::fs::write(&path, &request.body).await,
        }
    }
    .await;
    if staged.is_err() {
        let _ = tokio::fs::remove_dir_all(&directory).await;
        return Response::error(500, "Could not stage the file for upload.");
    }

    let mut input = UploadInput { original_filename: filename.clone(), expiry, ..UploadInput::from_path(path) };
    if let Some(prefix) = prefix {
        let key = match credentials::load(&destination.id) {
            Ok(creds) => bucket::available_key(&S3Provider::new(destination.clone(), creds), &filename, &prefix, &[]).await,
            Err(error) => {
                let _ = tokio::fs::remove_dir_all(&directory).await;
                return credential_failure(error);
            }
        };
        match key {
            Ok(key) => {
                input.object_key = Some(key);
                input.keep_existing = true;
            }
            Err(error) => {
                let _ = tokio::fs::remove_dir_all(&directory).await;
                return Response::error(502, error.to_string());
            }
        }
    }
    let response = run(core, input, destination).await;
    let _ = tokio::fs::remove_dir_all(&directory).await;
    response
}

async fn upload_clipboard(core: &SharedCore, request: &Request) -> Response {
    let Some(destination) = core.destinations.find(request.query.get("destinationId").map(String::as_str)) else {
        return Response::error(404, "No destination to upload to. Add one in Aktar's Settings.");
    };
    let expiry = match expiry_from(request, crate::expiry::is_active(core, &destination.id)) {
        Ok(expiry) => expiry,
        Err(response) => return response,
    };
    let inputs = tauri::async_runtime::spawn_blocking(crate::clipboard::read_inputs).await.unwrap_or_default();
    let Some(mut input) = inputs.into_iter().next() else {
        return Response::error(422, "The clipboard has no file or image to upload.");
    };
    input.expiry = expiry;
    // One request, one upload: a folder always goes up as a ZIP here,
    // whatever the destination does with folders.
    let mut destination = destination;
    if input.path.is_dir() {
        destination.folder_upload = Some(crate::destinations::FolderUploadMode::Zip);
    }
    run(core, input, destination).await
}

const EXPIRY_NOT_SET_UP: &str =
    "Auto-delete isn't set up for this destination. Set it up from Aktar's panel (Delete after) or the destination's settings.";

/// The `expires` query parameter: days until the upload is deleted. Only
/// accepted for a destination whose lifecycle rules are active, since those
/// are what deletes the file.
fn expiry_from(request: &Request, rules_active: bool) -> Result<Expiry, Response> {
    let Some(raw) = request.query.get("expires").map(|value| value.trim()).filter(|value| !value.is_empty()) else {
        return Ok(Expiry::Never);
    };
    match raw.parse::<u32>() {
        Ok(0) => Ok(Expiry::Never),
        Ok(days) if crate::expiry::is_valid(days) && rules_active => Ok(Expiry::Days(days)),
        Ok(days) if crate::expiry::is_valid(days) => Err(Response::error(409, EXPIRY_NOT_SET_UP)),
        _ => Err(Response::error(400, "The expires parameter must be 0, 1, 7, 14, or 30 (days).")),
    }
}

/// Queues the upload and waits for it to settle, so the caller gets the
/// finished history entry (and its links) back in the response.
async fn run(core: &SharedCore, input: UploadInput, destination: DestinationConfig) -> Response {
    let Some(mut queued) = uploads::enqueue(core, vec![input], Some(destination)).into_iter().next() else {
        return Response::error(500, "The upload could not be queued.");
    };
    loop {
        let state = queued.receiver.borrow_and_update().clone();
        match state {
            JobState::Succeeded { record_id, reused, .. } => {
                // `reused`: the file was already in the bucket, and that
                // upload's entry is what's returned.
                return match core.history.get(&record_id) {
                    Some(record) => {
                        // In the upload too, where the Mac puts it; the
                        // top-level copy stays for clients written against 0.3.0.
                        let mut upload = serde_json::to_value(upload_dto(core, &record)).unwrap_or_default();
                        if let Some(fields) = upload.as_object_mut() {
                            fields.insert("reused".into(), reused.into());
                        }
                        Response::json(201, serde_json::json!({ "upload": upload, "reused": reused }))
                    }
                    None => Response::error(500, "The upload finished but its history entry is missing."),
                };
            }
            // The staged copy of the file is deleted once this returns, so
            // a failed job can't be retried from the panel; remove it and
            // let the caller (Raycast) show the error instead.
            JobState::Failed { message } => {
                uploads::dismiss(core, &queued.job_id);
                return Response::error(502, message);
            }
            JobState::Cancelled => {
                uploads::dismiss(core, &queued.job_id);
                return Response::error(409, "The upload was cancelled in Aktar.");
            }
            JobState::Waiting | JobState::Uploading { .. } => {
                if queued.receiver.changed().await.is_err() {
                    return Response::error(409, "The upload was cancelled in Aktar.");
                }
            }
        }
    }
}

// MARK: - Bucket browsing

#[derive(Deserialize)]
struct MoveBody {
    from: String,
    to: String,
}

#[derive(Deserialize)]
struct FolderBody {
    prefix: Option<String>,
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LinkBody {
    key: String,
    expires_in: Option<i64>,
}

async fn handle_bucket(core: &SharedCore, request: &Request, destination: DestinationConfig, route: &[&str]) -> Response {
    let storage = match credentials::load(&destination.id) {
        Ok(creds) => S3Provider::new(destination.clone(), creds),
        Err(error) => return credential_failure(error),
    };

    match (request.method.as_str(), route) {
        ("GET", ["objects"]) => {
            let prefix = bucket::normalized_folder(request.query.get("prefix").map(String::as_str).unwrap_or_default());
            let token = request.query.get("continuationToken").filter(|token| !token.is_empty()).cloned();
            match storage.list(&prefix, token).await {
                Ok(listing) => Response::json(200, listing_dto(&listing, &destination)),
                Err(error) => Response::error(502, error.to_string()),
            }
        }
        ("DELETE", ["objects"]) => {
            let Some(key) = request.query.get("key").filter(|key| !key.is_empty()) else {
                return Response::error(400, "The key query parameter is required.");
            };
            match storage.delete(key).await {
                Ok(()) => {
                    core.history.object_deleted(key, &destination.id);
                    core.notify(events::HISTORY_CHANGED);
                    Response::json(200, serde_json::json!({ "deleted": key }))
                }
                Err(error) => Response::error(502, error.to_string()),
            }
        }
        ("POST", ["objects", "move"]) => {
            let body: MoveBody = match decode(request) {
                Ok(body) => body,
                Err(response) => return response,
            };
            let new_key = body.to.trim().trim_end_matches('/').to_string();
            if new_key.is_empty() || new_key == body.from {
                return Response::error(400, "Pick a different name or folder.");
            }
            if let Err(message) = bucket::check_key(&new_key) {
                return Response::error(400, message);
            }
            match bucket::move_object(&storage, &body.from, &new_key).await {
                Ok(()) => {
                    core.history.object_moved(&body.from, &new_key, &destination, crate::expiry::is_active(core, &destination.id));
                    core.notify(events::HISTORY_CHANGED);
                    let object = BucketObject { key: new_key, size: 0, last_modified: Some(crate::util::now_millis()) };
                    Response::json(200, object_dto(&object, &destination))
                }
                Err(bucket::MoveError::Exists) => {
                    Response::error(409, format!("An object named \u{201C}{new_key}\u{201D} already exists."))
                }
                Err(bucket::MoveError::Storage(error)) => Response::error(502, error.to_string()),
            }
        }
        ("POST", ["folders"]) => {
            let body: FolderBody = match decode(request) {
                Ok(body) => body,
                Err(response) => return response,
            };
            let name = body.name.trim().trim_end_matches('/');
            if name.is_empty() {
                return Response::error(400, "The folder name is required.");
            }
            let prefix = match bucket::check_key(name).and_then(|()| bucket::checked_folder(body.prefix.as_deref().unwrap_or_default())) {
                Ok(prefix) => prefix,
                Err(message) => return Response::error(400, message),
            };
            let folder = format!("{prefix}{name}/");
            match storage.create_folder(&folder).await {
                Ok(()) => Response::json(201, FolderDto::new(&folder)),
                Err(error) => Response::error(502, error.to_string()),
            }
        }
        ("POST", ["links"]) => {
            let body: LinkBody = match decode(request) {
                Ok(body) => body,
                Err(response) => return response,
            };
            // SigV4 presigned URLs are capped at seven days.
            let seconds = body.expires_in.unwrap_or(3600).clamp(60, 604_800);
            match storage.temporary_url(&body.key, seconds as u64).await {
                Ok(url) => Response::json(
                    200,
                    serde_json::json!({ "url": url, "expiresAt": iso8601(crate::util::now_millis() + seconds * 1000) }),
                ),
                Err(error) => Response::error(502, error.to_string()),
            }
        }
        _ => Response::error(404, "Not found."),
    }
}

fn decode<T: for<'de> Deserialize<'de>>(request: &Request) -> Result<T, Response> {
    serde_json::from_slice(&request.body).map_err(|error| Response::error(400, format!("Invalid request body: {error}")))
}

/// Like `decode`, with an empty body counting as `{}`.
fn decode_optional<T: for<'de> Deserialize<'de> + Default>(request: &Request) -> Result<T, Response> {
    if request.body.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    decode(request)
}

// MARK: - Watched folders

#[derive(Deserialize, Default)]
struct PauseBody {
    /// Checked by `watched::pause_minutes`; null or missing pauses until
    /// resumed.
    minutes: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct EnabledBody {
    enabled: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WatchedDto {
    paused: bool,
    paused_until: Option<String>,
    folders: Vec<WatchedFolderDto>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WatchedFolderDto {
    id: String,
    name: String,
    path: String,
    enabled: bool,
    status: crate::watched::engine::Status,
    #[serde(rename = "destinationID")]
    destination_id: Option<String>,
    waiting: usize,
    uploading: usize,
    failed: usize,
    awaiting_confirmation: usize,
    on_delete: crate::watched::model::OnDelete,
    confirm_delete: bool,
    deleting: usize,
    awaiting_delete_confirmation: usize,
    last_upload_at: Option<String>,
}

fn watched_dto(core: &SharedCore) -> WatchedDto {
    let overview = core.watched.overview();
    WatchedDto {
        paused: overview.paused,
        paused_until: overview.paused_until.iso(),
        folders: overview
            .folders
            .into_iter()
            .map(|info| WatchedFolderDto {
                id: info.folder.id,
                name: info.folder.name,
                path: info.folder.path.to_string_lossy().into_owned(),
                enabled: info.folder.enabled,
                status: info.status,
                destination_id: info.folder.destination_id,
                waiting: info.waiting,
                uploading: info.uploading,
                failed: info.failed,
                awaiting_confirmation: info.awaiting_confirmation,
                on_delete: info.folder.on_delete,
                confirm_delete: info.folder.confirm_delete,
                deleting: info.deleting,
                awaiting_delete_confirmation: info.awaiting_delete_confirmation,
                last_upload_at: info.last_upload_at.map(iso8601),
            })
            .collect(),
    }
}

fn credential_failure(error: CredentialError) -> Response {
    match error {
        CredentialError::NotFound => Response::error(409, error.to_string()),
        other => Response::error(502, other.to_string()),
    }
}

// MARK: - DTOs

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusDto {
    app: &'static str,
    version: String,
    build: String,
    api_version: u32,
    // Left out when empty, as the Mac app does, rather than sent as null.
    #[serde(skip_serializing_if = "Option::is_none")]
    default_destination_id: Option<String>,
    output_format: &'static str,
    platform: &'static str,
    watching: WatchingDto,
}

#[derive(Serialize)]
struct WatchingDto {
    paused: bool,
    folders: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DestinationDto {
    id: String,
    name: String,
    provider: &'static str,
    provider_name: String,
    bucket: String,
    #[serde(rename = "publicBaseURL")]
    public_base_url: String,
    is_default: bool,
}

fn destination_dto(core: &SharedCore, destination: &DestinationConfig) -> DestinationDto {
    DestinationDto {
        id: destination.id.clone(),
        name: destination.name.clone(),
        provider: destination.preset.raw_value(),
        provider_name: destination.preset.display_name(),
        bucket: destination.bucket.clone(),
        public_base_url: destination.public_base_url.clone(),
        is_default: core.destinations.default_destination().map(|d| d.id) == Some(destination.id.clone()),
    }
}

#[derive(Serialize)]
struct Formats {
    url: String,
    markdown: String,
    html: String,
    custom: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UploadDto {
    id: String,
    filename: String,
    object_key: String,
    url: String,
    destination_id: String,
    destination_name: String,
    mime_type: String,
    size: i64,
    created_at: String,
    /// When an expiring upload gets deleted.
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_at: Option<String>,
    formats: Formats,
}

fn upload_dto(core: &SharedCore, record: &UploadRecord) -> UploadDto {
    let template = core.settings.get().custom_template;
    let formatted = |mode: OutputMode| output::format(&record.public_url, mode, &record.local_filename, &template);
    UploadDto {
        id: record.id.clone(),
        filename: record.local_filename.clone(),
        object_key: record.object_key.clone(),
        url: record.public_url.clone(),
        destination_id: record.destination_id.clone(),
        destination_name: record.destination_name.clone(),
        mime_type: record.mime_type.clone(),
        size: record.byte_size,
        created_at: iso8601(record.created_at),
        expires_at: record.expires_at.map(iso8601),
        formats: Formats {
            url: formatted(OutputMode::Url),
            markdown: formatted(OutputMode::Markdown),
            html: formatted(OutputMode::Html),
            custom: formatted(OutputMode::Custom),
        },
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ListingDto {
    prefix: String,
    folders: Vec<FolderDto>,
    objects: Vec<ObjectDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_continuation_token: Option<String>,
}

#[derive(Serialize)]
struct FolderDto {
    prefix: String,
    name: String,
}

impl FolderDto {
    fn new(prefix: &str) -> Self {
        Self { prefix: prefix.to_string(), name: bucket::folder_display_name(prefix).to_string() }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ObjectDto {
    key: String,
    name: String,
    size: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_modified: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
}

fn listing_dto(listing: &BucketListing, destination: &DestinationConfig) -> ListingDto {
    ListingDto {
        prefix: listing.prefix.clone(),
        folders: listing.folders.iter().map(|folder| FolderDto::new(folder)).collect(),
        objects: listing.objects.iter().map(|object| object_dto(object, destination)).collect(),
        next_continuation_token: listing.next_continuation_token.clone(),
    }
}

fn object_dto(object: &BucketObject, destination: &DestinationConfig) -> ObjectDto {
    let has_public_url = !destination.public_base_url.trim().is_empty();
    ObjectDto {
        key: object.key.clone(),
        name: object.name().to_string(),
        size: object.size,
        last_modified: object.last_modified.map(iso8601),
        url: has_public_url.then(|| resolve_public_url(&destination.public_base_url, &object.key)),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn request(expires: Option<&str>) -> Request {
        Request {
            method: "POST".into(),
            path: "/v1/uploads".into(),
            query: expires.map(|value| HashMap::from([("expires".to_string(), value.to_string())])).unwrap_or_default(),
            body: Vec::new(),
            body_file: None,
        }
    }

    fn error(result: Result<Expiry, Response>) -> (u16, String) {
        let response = result.expect_err("should be refused");
        let body: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
        (response.status, body["error"].as_str().unwrap().to_string())
    }

    #[test]
    fn reads_expires() {
        assert_eq!(expiry_from(&request(None), false).ok(), Some(Expiry::Never));
        assert_eq!(expiry_from(&request(Some("")), false).ok(), Some(Expiry::Never));
        assert_eq!(expiry_from(&request(Some("0")), false).ok(), Some(Expiry::Never));
        assert_eq!(expiry_from(&request(Some("7")), true).ok(), Some(Expiry::Days(7)));
        assert_eq!(expiry_from(&request(Some(" 30 ")), true).ok(), Some(Expiry::Days(30)));
        assert_eq!(error(expiry_from(&request(Some("3")), true)).0, 400);
        assert_eq!(error(expiry_from(&request(Some("soon")), false)).0, 400);
    }

    #[test]
    fn refuses_expires_without_rules() {
        let (status, message) = error(expiry_from(&request(Some("7")), false));
        assert_eq!(status, 409);
        assert_eq!(message, EXPIRY_NOT_SET_UP);
    }
}
