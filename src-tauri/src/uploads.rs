//! Coordinates the upload pipeline: expand folders -> zip a folder / process
//! a photo or clean its metadata -> hash it -> reuse an earlier upload of
//! the same bytes, or generate the object key and upload (in one request,
//! or in parts) -> resolve the link -> store history -> format output ->
//! copy to clipboard -> notify. Runs up to `MAX_CONCURRENT` jobs at once.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tauri::async_runtime::JoinHandle;
use tauri_plugin_notification::NotificationExt;
use tokio::sync::watch;

use crate::core::{events, SharedCore, UploadSucceeded};
use crate::credentials;
use crate::destinations::{DestinationConfig, FolderUploadMode, ImageFormat, ImageProcessing};
use crate::folder_upload;
use crate::history::{NewRecord, Replacement, UploadRecord};
use crate::multipart::{self, Candidate, Session, SourceFile};
use crate::outbox::EarlyLink;
use crate::output::{self, ContentHashes};
use crate::quick_share::{self, Origin, PasteTarget, Stopped};
use crate::remote_download::{self, Downloaded, RemoteSource};
use crate::short_keys::{self, ShortKeyTarget};
use crate::storage::{BucketObject, Progress, S3Provider, StorageError, UploadResult, SINGLE_UPLOAD_LIMIT};
use crate::{image_metadata, image_processing};
use crate::t;
use crate::thumbnails::ThumbnailMode;
use crate::windows::AppWindow;

const MAX_CONCURRENT: usize = 3;
/// How many times an upload to a name picked as free looks for another
/// one when the provider says it was taken in the meantime.
const MAX_NAME_RETRIES: usize = 5;
/// An upload that has sent nothing, and heard nothing back, for this long
/// has stalled (Wi-Fi dropped mid-request, a proxy swallowed it) and is
/// failed so it can be retried, instead of spinning forever.
const STALL_TIMEOUT_MS: i64 = 120_000;

#[derive(Debug, Clone)]
pub struct UploadInput {
    pub path: PathBuf,
    pub original_filename: String,
    /// Uploads to exactly this key instead of one generated from the
    /// destination's object path template (bucket browser, local API).
    pub object_key: Option<String>,
    /// A copy Aktar made (a clipboard image), deleted once its job is done
    /// with it: uploaded, cancelled, or dismissed.
    pub temporary: bool,
    pub expiry: Expiry,
    /// A file from a folder uploaded with its structure: its key, under the
    /// folder's own prefix. Unlike `object_key`, "Delete after" still
    /// applies.
    pub folder_key: Option<String>,
    /// The folder upload this file belongs to, so the links are copied
    /// together once the last one is done.
    pub group: Option<UploadGroup>,
    /// A file from a watched folder, which copies and notifies as the
    /// folder says instead.
    pub watched: Option<WatchedSource>,
    /// `object_key` is a free name picked for it (the bucket browser, the
    /// local API's prefix=): where the provider can, it's only written
    /// while nothing is there, and a name taken in the meantime makes way
    /// for the next free one.
    pub keep_existing: bool,
    /// "Replace File": `object_key` is an existing file's, written over so
    /// its link keeps working.
    pub replacing: Option<Replacing>,
    /// Overrides the destination's Short Links for this upload (the local
    /// API's `short=1` / `short=0`): true makes one whatever the length,
    /// false makes none. None follows the destination.
    pub short_link: Option<bool>,
    /// A copied link: its file is downloaded into `path`'s folder when the
    /// upload starts, and then it's like any file.
    pub remote: Option<RemoteSource>,
    /// A folder Aktar made for this file (a copied text or link), deleted
    /// with it.
    pub temporary_folder: Option<PathBuf>,
    /// Someone waits for the result (the local API): no early link, and a
    /// failure is reported rather than waited out.
    pub waited: bool,
}

/// A file written over an existing upload or bucket object, keeping its key
/// and link. It gets the destination's metadata removal and resize, but
/// never a format change, since the key's extension stays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replacing {
    /// The history entry being replaced; none for a file in the bucket view,
    /// whose newest entry at that key (if there is one) is updated.
    pub record_id: Option<String>,
}

/// Where a watched folder's file came from, and how its key is made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchedSource {
    pub folder_id: String,
    pub folder_name: String,
    pub relative_path: String,
    pub batch_id: String,
    pub key_place: output::KeyPlace,
    /// The file's SHA-256, when the watched folder already worked it out.
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadGroup {
    pub id: String,
    pub name: String,
    pub index: usize,
    pub count: usize,
}

/// How long an upload is kept. Settled when it's queued, so a retry
/// later doesn't pick up a "Delete after" changed in the meantime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Expiry {
    /// The "Delete after" setting.
    #[default]
    FromSettings,
    Never,
    Days(u32),
}

impl UploadInput {
    pub fn from_path(path: PathBuf) -> Self {
        let original_filename = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".into());
        Self {
            path,
            original_filename,
            object_key: None,
            temporary: false,
            expiry: Expiry::FromSettings,
            folder_key: None,
            group: None,
            watched: None,
            keep_existing: false,
            replacing: None,
            short_link: None,
            remote: None,
            temporary_folder: None,
            waited: false,
        }
    }

    /// Days until the upload is deleted, once `enqueue` has settled it.
    pub(crate) fn expire_after_days(&self) -> Option<u32> {
        match self.expiry {
            Expiry::Days(days) => Some(days),
            _ => None,
        }
    }

    pub(crate) fn remove_if_temporary(&self) {
        if self.temporary {
            let _ = std::fs::remove_file(&self.path);
        }
        if let Some(folder) = &self.temporary_folder {
            let _ = std::fs::remove_dir_all(folder);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum JobState {
    Waiting,
    /// `resuming` while it continues an upload left unfinished before.
    Uploading { progress: f64, resuming: bool },
    /// `reused` when an earlier upload of the same file was found, and its
    /// link copied instead. `short_link_error`: its short link couldn't be
    /// made, so the original link was copied.
    Succeeded { public_url: String, record_id: String, reused: bool, short_link_error: Option<String> },
    Failed { message: String },
    /// Failed for want of a connection: tried again when the network is
    /// back, and while it looks up after a growing wait.
    Offline { message: String },
    Cancelled,
}

impl JobState {
    fn is_finished_for_good(&self) -> bool {
        matches!(self, JobState::Succeeded { .. } | JobState::Cancelled)
    }
}

pub(crate) struct Job {
    pub(crate) id: String,
    pub(crate) input: UploadInput,
    pub(crate) destination: DestinationConfig,
    pub(crate) state: JobState,
    pub(crate) sender: watch::Sender<JobState>,
    pub(crate) task: Option<JoinHandle<()>>,
    /// The upload the user started that this file is part of (see
    /// `quick_share::UploadAction`); none for a watched folder's file.
    pub(crate) action_id: Option<String>,
    /// The link copied before the bytes went; from then on the key.
    pub(crate) early_link: Option<EarlyLink>,
    /// Kept in the outbox, so it goes on after a restart.
    pub(crate) in_outbox: bool,
    /// A copied link's file, once it's downloaded.
    pub(crate) downloaded: Option<Downloaded>,
    pub(crate) downloading: bool,
    /// When the first try started: an object at the key written after this
    /// is the job's own, from a try whose answer was lost.
    pub(crate) first_attempt_at: Option<i64>,
    /// Tries that failed for want of a connection, and when the next is.
    pub(crate) offline_retries: u32,
    pub(crate) next_retry_at: Option<i64>,
    /// The key and size being written, to take it back after a cancel.
    pub(crate) sending: Option<(String, u64)>,
}

impl Job {
    fn new(id: String, input: UploadInput, destination: DestinationConfig, state: JobState, sender: watch::Sender<JobState>) -> Self {
        Self {
            id,
            input,
            destination,
            state,
            sender,
            task: None,
            action_id: None,
            early_link: None,
            in_outbox: false,
            downloaded: None,
            downloading: false,
            first_attempt_at: None,
            offline_retries: 0,
            next_retry_at: None,
            sending: None,
        }
    }

    /// The name the file goes by: a downloaded link's own name, unless the
    /// upload was renamed (then with the download's extension when the new
    /// name has none).
    pub(crate) fn display_name(&self) -> String {
        let (Some(remote), Some(downloaded)) = (&self.input.remote, &self.downloaded) else { return self.input.original_filename.clone() };
        display_name(&self.input.original_filename, remote, downloaded)
    }
}

fn display_name(original_filename: &str, remote: &RemoteSource, downloaded: &Downloaded) -> String {
    if original_filename == remote.suggested_name {
        return downloaded.filename.clone();
    }
    let extension = crate::util::split_extension(&downloaded.filename).1;
    if crate::util::split_extension(original_filename).1.is_empty() && !extension.is_empty() {
        format!("{original_filename}.{extension}")
    } else {
        original_filename.to_string()
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobSnapshot {
    pub id: String,
    pub filename: String,
    pub destination_id: String,
    pub destination_name: String,
    pub state: JobState,
    /// The watched folder it came from, by name.
    pub source: Option<String>,
    /// Its link is copied already, before the upload finished.
    pub link_copied: bool,
    /// A copied link's file is being downloaded.
    pub downloading: bool,
}

/// What the finished files of a folder (or of a drop spread over several
/// destinations) left to copy, formatted as their destinations copy them,
/// by position.
type GroupLinks = BTreeMap<usize, String>;

#[derive(Default)]
pub struct UploadManager {
    pub(crate) jobs: Mutex<Vec<Job>>,
    /// Folders uploaded with their structure, and drops spread over several
    /// destinations, that still have files going.
    open_groups: Mutex<HashMap<String, GroupLinks>>,
    /// The multipart upload (`multipart::Session` ID) each job is sending,
    /// or left unfinished, by job ID.
    job_sessions: Mutex<HashMap<String, String>>,
    /// Files waiting for their name ("Rename and upload").
    pending_names: Mutex<Vec<PendingName>>,
    /// The uploads the user started, by ID, for Undo and pasting.
    pub(crate) actions: Mutex<HashMap<String, quick_share::UploadAction>>,
    /// The action Undo would take back now; none once its time is up.
    pub(crate) undoable: Mutex<Option<String>>,
    pub(crate) undo_timer: Mutex<Option<JoinHandle<()>>>,
    /// Wakes up for the next upload waiting for the network.
    pub(crate) offline_timer: Mutex<Option<JoinHandle<()>>>,
}

/// A file to upload once it has a name.
struct PendingName {
    id: String,
    input: UploadInput,
    destination: Option<DestinationConfig>,
}

/// What the name dialog shows for a file waiting for its name.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NameRequest {
    pub id: String,
    /// The name without its extension, to start from.
    pub name: String,
    /// The extension, which stays; empty when there's none.
    pub extension: String,
}

impl UploadManager {
    /// Jobs still worth showing: waiting, uploading, or failed (with Retry).
    /// Finished and cancelled jobs are dropped from the list right away.
    pub fn snapshot(&self) -> Vec<JobSnapshot> {
        self.jobs
            .lock()
            .unwrap()
            .iter()
            .map(|job| JobSnapshot {
                id: job.id.clone(),
                filename: job.display_name(),
                destination_id: job.destination.id.clone(),
                destination_name: job.destination.name.clone(),
                state: job.state.clone(),
                source: job.input.watched.as_ref().map(|source| source.folder_name.clone()),
                link_copied: job.early_link.is_some(),
                downloading: job.downloading,
            })
            .collect()
    }
}

pub struct Queued {
    pub job_id: String,
    pub receiver: watch::Receiver<JobState>,
}

/// Queues `inputs` for `destination`. Without one, each file goes where its
/// kind or extension says ("Use for", see `routing`), else to the default
/// destination. Each job keeps the destination it was started for, so a
/// retry or a change of default later doesn't redirect it.
pub fn enqueue(core: &SharedCore, inputs: Vec<UploadInput>, destination: Option<DestinationConfig>) -> Vec<Queued> {
    enqueue_with(core, inputs, destination, Origin::default())
}

/// `enqueue`, for an upload with somewhere to paste its link or a clipboard
/// kept from before (a capture). Everything but a watched folder's files is
/// one `quick_share::UploadAction`, which Undo takes back as a whole.
pub fn enqueue_with(core: &SharedCore, inputs: Vec<UploadInput>, destination: Option<DestinationConfig>, origin: Origin) -> Vec<Queued> {
    let action = inputs.iter().any(|input| input.watched.is_none()).then(|| quick_share::new_action(core, origin));
    let queued = route_and_enqueue(core, inputs, destination, action.as_deref());
    if let Some(action) = &action {
        quick_share::drop_if_empty(core, action);
    }
    queued
}

fn route_and_enqueue(core: &SharedCore, inputs: Vec<UploadInput>, destination: Option<DestinationConfig>, action: Option<&str>) -> Vec<Queued> {
    if destination.is_some() || inputs.is_empty() {
        return enqueue_to(core, inputs, destination, action);
    }
    let destinations = core.destinations.all();
    let default = core.destinations.default_destination();
    let Some(default) = default.filter(|_| crate::routing::is_active(&destinations)) else {
        return enqueue_to(core, inputs, None, action);
    };
    let routed: Vec<(DestinationConfig, UploadInput)> = inputs
        .into_iter()
        .map(|input| {
            let name = routing_name(&input, &default);
            let target = crate::routing::route(&destinations, Some(&default.id), &name).unwrap_or(&default).clone();
            (target, input)
        })
        .collect();
    // Files of one drop that land in different destinations have their
    // links copied together, in drop order, once the last is done.
    let spread = routed.iter().map(|(target, _)| target.id.as_str()).collect::<HashSet<_>>().len() > 1;
    let singles: Vec<usize> = (0..routed.len()).filter(|&index| !routed[index].1.path.is_dir() && routed[index].1.group.is_none()).collect();
    let group = (spread && singles.len() > 1).then(|| {
        let id = crate::util::new_id();
        core.uploads.open_groups.lock().unwrap().insert(id.clone(), GroupLinks::new());
        id
    });
    let mut batches: Vec<(DestinationConfig, Vec<UploadInput>)> = Vec::new();
    for (index, (target, mut input)) in routed.into_iter().enumerate() {
        if let (Some(id), Some(position)) = (&group, singles.iter().position(|&single| single == index)) {
            input.group = Some(UploadGroup { id: id.clone(), name: String::new(), index: position, count: singles.len() });
        }
        match batches.iter_mut().find(|(destination, _)| destination.id == target.id) {
            Some((_, batch)) => batch.push(input),
            None => batches.push((target, vec![input])),
        }
    }
    batches.into_iter().flat_map(|(destination, inputs)| enqueue_to(core, inputs, Some(destination), action)).collect()
}

/// Where `enqueue` would send `inputs` without a destination: the names of
/// the destinations they route to, in order, or none without any.
pub fn routed_destination_names(core: &SharedCore, inputs: &[UploadInput]) -> Vec<String> {
    let Some(default) = core.destinations.default_destination() else { return Vec::new() };
    let destinations = core.destinations.all();
    if !crate::routing::is_active(&destinations) {
        return vec![default.name];
    }
    let mut names: Vec<String> = Vec::new();
    for input in inputs {
        let name = crate::routing::route(&destinations, Some(&default.id), &routing_name(input, &default)).unwrap_or(&default).name.clone();
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// The name a file routes by: its own; a folder going up as a ZIP routes as
/// one, and a folder keeping its structure isn't split (no extension, so
/// the default destination).
fn routing_name(input: &UploadInput, default: &DestinationConfig) -> String {
    if !input.path.is_dir() {
        return input.original_filename.clone();
    }
    match default.folder_upload() {
        FolderUploadMode::Zip => format!("{}.zip", folder_upload::name(&input.path)),
        FolderUploadMode::KeepStructure => String::new(),
    }
}

/// `enqueue` without routing: everything to `destination`, or the default.
fn enqueue_to(core: &SharedCore, inputs: Vec<UploadInput>, destination: Option<DestinationConfig>, action: Option<&str>) -> Vec<Queued> {
    if inputs.is_empty() {
        return Vec::new();
    }
    // Nowhere to upload to yet (the shortcut or a drop before setup): say
    // so, and open Welcome, where the first destination gets added.
    let Some(destination) = destination.or_else(|| core.destinations.default_destination()) else {
        inputs.iter().for_each(UploadInput::remove_if_temporary);
        show_notification(core, &t!("Upload failed"), &t!("No destination to upload to. Add one in Settings."));
        crate::windows::open(&core.app, AppWindow::Onboarding);
        return Vec::new();
    };
    let delete_after_days = destination.expiry_days.unwrap_or(core.settings.get().delete_after_days);
    let rules_active = crate::expiry::is_active(core, &destination.id);
    let inputs = expanding_folders(core, inputs, &destination);
    let mut queued = Vec::new();
    let mut action_jobs = Vec::new();
    {
        let mut jobs = core.uploads.jobs.lock().unwrap();
        for (offset, mut input) in inputs.into_iter().enumerate() {
            input.expiry = settled_expiry(input.expiry, input.object_key.as_deref(), rules_active, delete_after_days);
            let (sender, receiver) = watch::channel(JobState::Waiting);
            let id = crate::util::new_id();
            queued.push(Queued { job_id: id.clone(), receiver });
            let mut job = Job::new(id, input, destination.clone(), JobState::Waiting, sender);
            if let Some(action) = action.filter(|_| job.input.watched.is_none()) {
                job.action_id = Some(action.to_string());
                action_jobs.push(job.id.clone());
            }
            // A watched folder's files line up behind everything else, in
            // order; the user's own uploads go to the front.
            if job.input.watched.is_some() {
                jobs.push(job);
            } else {
                jobs.insert(offset, job);
            }
        }
    }
    if let (Some(action), false) = (action, action_jobs.is_empty()) {
        quick_share::attach_jobs(core, action, action_jobs);
    }
    drain(core);
    queued
}

/// At launch: the uploads whose link was copied before Aktar quit go on, to
/// the keys their links point at. One whose destination was removed has
/// nowhere to go and is dropped; one that had failed waits for Retry or
/// Cancel.
pub fn restore_outbox(core: &SharedCore) {
    let entries = core.outbox.load(crate::util::now_millis());
    if entries.is_empty() {
        return;
    }
    let destinations = core.destinations.all();
    let mut restored = Vec::new();
    for (entry, path) in entries {
        let Some(destination) = destinations.iter().find(|destination| destination.id == entry.destination_id).cloned() else {
            core.outbox.remove(&entry.id);
            continue;
        };
        let input = UploadInput {
            original_filename: entry.original_filename.clone(),
            short_link: entry.short_link_override,
            expiry: entry.expiry_days.map_or(Expiry::Never, Expiry::Days),
            ..UploadInput::from_path(path)
        };
        let state = match &entry.failure {
            Some(message) => JobState::Failed { message: message.clone() },
            None => JobState::Waiting,
        };
        let (sender, _) = watch::channel(state.clone());
        let mut job = Job::new(entry.id.clone(), input, destination, state, sender);
        let action_id = quick_share::new_action(core, Origin::default());
        job.action_id = Some(action_id.clone());
        job.early_link = Some(entry.early_link);
        job.first_attempt_at = Some(entry.first_attempt_at);
        job.in_outbox = true;
        restored.push((job, action_id));
    }
    let mut attached = Vec::new();
    {
        let mut jobs = core.uploads.jobs.lock().unwrap();
        for (offset, (job, action_id)) in restored.into_iter().enumerate() {
            attached.push((action_id, job.id.clone()));
            jobs.insert(offset, job);
        }
    }
    for (action_id, job_id) in attached {
        quick_share::attach_jobs(core, &action_id, vec![job_id]);
    }
    drain(core);
}

/// Folders become one ZIP input (zipped when its turn comes, so a big
/// folder doesn't hold anything up) or one input per file, as the
/// destination says. Exact keys (the bucket browser) are left alone.
fn expanding_folders(core: &SharedCore, inputs: Vec<UploadInput>, destination: &DestinationConfig) -> Vec<UploadInput> {
    let mut expanded = Vec::new();
    for input in inputs {
        if input.object_key.is_some() || input.folder_key.is_some() || !input.path.is_dir() {
            expanded.push(input);
            continue;
        }
        let name = folder_upload::name(&input.path);
        match destination.folder_upload() {
            FolderUploadMode::Zip => expanded.push(UploadInput { original_filename: format!("{name}.zip"), ..input }),
            FolderUploadMode::KeepStructure => match folder_upload::files(&input.path, Some(folder_upload::MAX_FILES)) {
                Ok(entries) => {
                    let prefix = folder_upload::key_prefix(&destination.object_path_template, &name);
                    let id = crate::util::new_id();
                    core.uploads.open_groups.lock().unwrap().insert(id.clone(), GroupLinks::new());
                    let count = entries.len();
                    expanded.extend(entries.into_iter().enumerate().map(|(index, entry)| UploadInput {
                        folder_key: Some(format!("{prefix}{}", entry.relative_path)),
                        group: Some(UploadGroup { id: id.clone(), name: name.clone(), index, count }),
                        expiry: input.expiry,
                        ..UploadInput::from_path(entry.path)
                    }));
                }
                Err(error) => show_notification(core, &t!("Upload failed"), &format!("{name}: {error}")),
            },
        }
    }
    expanded
}

/// How long a queued upload is kept. Nothing expires on a destination
/// whose bucket isn't known to have the lifecycle rules, whatever "Delete
/// after" is set to: the effective setting is `delete_after_days` once the
/// rules are active, and 0 (keep) until then. An upload to an exact key
/// (the bucket browser, the local API's prefix=) is exactly where the user
/// put it: it stays, unless that's inside a `tmp/{N}d/` folder the bucket's
/// rules empty, where it goes after N days like any other file there.
fn settled_expiry(expiry: Expiry, exact_key: Option<&str>, rules_active: bool, delete_after_days: u32) -> Expiry {
    if !rules_active {
        return Expiry::Never;
    }
    if let Some(key) = exact_key {
        return crate::expiry::days_in_key(key).map_or(Expiry::Never, Expiry::Days);
    }
    match expiry {
        Expiry::FromSettings if crate::expiry::is_valid(delete_after_days) => Expiry::Days(delete_after_days),
        Expiry::Days(days) if crate::expiry::is_valid(days) => Expiry::Days(days),
        _ => Expiry::Never,
    }
}

/// Uploads whatever is on the clipboard, after asking for its name when
/// `rename` is set. Returns false when there's nothing uploadable on it.
/// Reading and PNG-encoding a large screenshot takes a moment, so it's done
/// off the async runtime's workers.
pub async fn upload_clipboard(core: &SharedCore, rename: bool, paste: Option<PasteTarget>) -> bool {
    let inputs = tauri::async_runtime::spawn_blocking(crate::clipboard::read_inputs).await.unwrap_or_default();
    if inputs.is_empty() {
        return false;
    }
    if rename {
        ask_for_names(core, inputs, None);
    } else {
        enqueue_with(core, inputs, None, Origin { paste, snapshot: None });
    }
    true
}

/// A destination's own shortcut: the clipboard goes to that destination,
/// never rerouted by "Use for".
pub fn upload_clipboard_to_in_background(core: &SharedCore, destination: DestinationConfig) {
    let core = core.clone();
    tauri::async_runtime::spawn(async move {
        let inputs = tauri::async_runtime::spawn_blocking(crate::clipboard::read_inputs).await.unwrap_or_default();
        if inputs.is_empty() {
            show_notification(&core, "Aktar", &t!("The clipboard has nothing to upload: no file, image, link or text."));
        } else {
            enqueue(&core, inputs, Some(destination));
        }
    });
}

/// "Replace File" on a history entry: `path` goes up to its key, so its
/// link keeps working. `filename` (the local API's) names the file when
/// `path` is a staged copy; its extension sets the content type.
pub fn replace_upload(core: &SharedCore, record_id: &str, path: PathBuf, filename: Option<String>) -> Result<Queued, String> {
    let record = core.history.get(record_id).ok_or_else(|| t!("This upload is no longer in your history."))?;
    let destination = core
        .destinations
        .all()
        .into_iter()
        .find(|destination| destination.id == record.destination_id)
        .ok_or_else(|| t!("This upload’s destination was removed."))?;
    replace(core, destination, record.object_key, Some(record.id), path, filename)
}

/// "Replace File" on a file in the bucket view.
pub fn replace_object(
    core: &SharedCore,
    destination: DestinationConfig,
    key: String,
    path: PathBuf,
    filename: Option<String>,
) -> Result<Queued, String> {
    replace(core, destination, key, None, path, filename)
}

fn replace(
    core: &SharedCore,
    destination: DestinationConfig,
    key: String,
    record_id: Option<String>,
    path: PathBuf,
    filename: Option<String>,
) -> Result<Queued, String> {
    if !path.is_file() {
        return Err(t!("Choose a file to replace it with."));
    }
    let mut input = UploadInput { object_key: Some(key), replacing: Some(Replacing { record_id }), ..UploadInput::from_path(path) };
    if let Some(filename) = filename {
        input.original_filename = filename;
        // Only the local API names its staged copy, and it waits for the
        // result: a failure is answered rather than waited out.
        input.waited = true;
    }
    enqueue_to(core, vec![input], Some(destination), None).into_iter().next().ok_or_else(|| t!("The upload could not be queued."))
}

/// For the shortcuts, the tray menu, and aktar:// links, whose handlers run
/// on the UI thread: uploads the clipboard in the background, and says so
/// when there's nothing on it to upload.
pub fn upload_clipboard_in_background(core: &SharedCore, rename: bool) {
    upload_clipboard_and_paste(core, rename, None);
}

/// `upload_clipboard_in_background`, pasting the link into `paste`'s
/// window once it's copied ("Upload clipboard and paste the link").
pub fn upload_clipboard_and_paste(core: &SharedCore, rename: bool, paste: Option<PasteTarget>) {
    let core = core.clone();
    tauri::async_runtime::spawn(async move {
        if !upload_clipboard(&core, rename, paste).await {
            if paste.is_some() {
                crate::win32::beep();
            }
            show_notification(&core, "Aktar", &t!("The clipboard has nothing to upload: no file, image, link or text."));
        }
    });
}

/// "Rename before upload": each file waits in the panel's name dialog,
/// one at a time, and goes up once it has a name. Folders go up right away.
pub fn ask_for_names(core: &SharedCore, inputs: Vec<UploadInput>, destination: Option<DestinationConfig>) {
    let (folders, files): (Vec<UploadInput>, Vec<UploadInput>) = inputs.into_iter().partition(|input| input.path.is_dir());
    if !folders.is_empty() {
        enqueue(core, folders, destination.clone());
    }
    if files.is_empty() {
        return;
    }
    core.uploads.pending_names.lock().unwrap().extend(files.into_iter().map(|input| PendingName {
        id: crate::util::new_id(),
        input,
        destination: destination.clone(),
    }));
    core.notify(events::NAMES_CHANGED);
    crate::panel::show(&core.app);
}

/// The files waiting for a name, first in line first.
pub fn pending_names(core: &SharedCore) -> Vec<NameRequest> {
    core.uploads
        .pending_names
        .lock()
        .unwrap()
        .iter()
        .map(|pending| {
            let (name, extension) = crate::util::split_extension(&pending.input.original_filename);
            NameRequest { id: pending.id.clone(), name: name.to_string(), extension: extension.to_string() }
        })
        .collect()
}

/// Uploads a file waiting for its name under `name`, or drops it (Cancel)
/// when there's none.
pub fn resolve_name(core: &SharedCore, id: &str, name: Option<String>) {
    let pending = {
        let mut names = core.uploads.pending_names.lock().unwrap();
        names.iter().position(|pending| pending.id == id).map(|index| names.remove(index))
    };
    core.notify(events::NAMES_CHANGED);
    let Some(PendingName { mut input, destination, .. }) = pending else { return };
    match name {
        Some(name) => {
            input.original_filename = named(&input.original_filename, &name);
            enqueue(core, vec![input], destination);
        }
        None => input.remove_if_temporary(),
    }
}

/// The file name for an upload named `typed`: it replaces the name and
/// keeps the extension. Slashes, backslashes and control characters are
/// taken out, and a name that's left empty, "." or ".." keeps the original.
fn named(original: &str, typed: &str) -> String {
    let typed: String = typed.chars().filter(|c| *c != '/' && *c != '\\' && !c.is_control()).collect();
    let typed = typed.trim();
    if matches!(typed, "" | "." | "..") {
        return original.to_string();
    }
    match crate::util::split_extension(original).1 {
        "" => typed.to_string(),
        extension => format!("{typed}.{extension}"),
    }
}

/// Whether the job is still in the list (not dismissed, not finished).
pub fn has_job(core: &SharedCore, job_id: &str) -> bool {
    core.uploads.jobs.lock().unwrap().iter().any(|job| job.id == job_id)
}

/// Tries a failed upload, or one waiting for the network, again.
pub fn retry(core: &SharedCore, job_id: &str) {
    let mut was_failed = false;
    {
        let mut jobs = core.uploads.jobs.lock().unwrap();
        if let Some(job) = jobs.iter_mut().find(|job| job.id == job_id) {
            if matches!(job.state, JobState::Failed { .. } | JobState::Offline { .. }) {
                was_failed = matches!(job.state, JobState::Failed { .. });
                job.state = JobState::Waiting;
                job.sender.send_replace(JobState::Waiting);
            }
        }
    }
    // A kept upload that had failed for good goes on after a restart again.
    if was_failed {
        quick_share::note_outbox_failure(core, job_id, None);
    }
    drain(core);
}

/// Stops the job: in-flight parts stop and its multipart upload is aborted.
/// A job whose link was already copied takes it back: its short link is
/// deleted, it leaves the outbox, and the clipboard gets back what was there
/// before, if it still holds that link. Bytes it may have finished sending
/// are deleted again.
pub fn cancel(core: &SharedCore, job_id: &str) {
    let mut cancelled = None;
    let mut watched_cancelled = None;
    let mut stopped = None;
    let mut sent = None;
    {
        let mut jobs = core.uploads.jobs.lock().unwrap();
        if let Some(index) = jobs.iter().position(|job| job.id == job_id) {
            let job = jobs.remove(index);
            let running = job.task.is_some();
            if let Some(task) = job.task {
                task.abort();
            }
            job.input.remove_if_temporary();
            job.sender.send_replace(JobState::Cancelled);
            cancelled = job.input.group.clone();
            if let Some(source) = &job.input.watched {
                watched_cancelled = Some(source.clone());
            }
            // A replace keeps what it wrote: there's nothing to go back to.
            if let (true, Some((key, size)), None) = (running, job.sending.clone(), &job.input.replacing) {
                sent = Some((job.destination.clone(), key, size, job.first_attempt_at.unwrap_or_default()));
            }
            stopped = Some(Stopped { job_id: job.id.clone(), action_id: job.action_id.clone(), early_link: job.early_link, in_outbox: job.in_outbox, destination: job.destination });
        }
    }
    if let Some(stopped) = &stopped {
        quick_share::withdraw_early_link(core, stopped);
    }
    if let Some((destination, key, size, first_attempt_at)) = sent {
        quick_share::take_back(destination, key, size, first_attempt_at);
    }
    if let Some(source) = watched_cancelled {
        crate::watched::upload_cancelled(core, &source);
    }
    // Its parts are thrown away; quitting, unlike this, keeps them.
    let session = core.uploads.job_sessions.lock().unwrap().remove(job_id);
    if let Some(session) = session.and_then(|id| core.upload_sessions.get(&id)) {
        multipart::abort_in_background(core, session);
    }
    if let Some(group) = cancelled {
        finish_group_if_done(core, &group, stopped.as_ref().and_then(|stopped| stopped.action_id.as_deref()));
    }
    drain(core);
}

/// Drops a job from the list, e.g. one started through the local API whose
/// staged file is already gone, so it can't be retried.
pub fn dismiss(core: &SharedCore, job_id: &str) {
    let mut stopped = None;
    {
        let mut jobs = core.uploads.jobs.lock().unwrap();
        if let Some(index) = jobs.iter().position(|job| job.id == job_id) {
            let job = jobs.remove(index);
            if let Some(task) = job.task {
                task.abort();
            }
            job.input.remove_if_temporary();
            stopped = Some(Stopped { job_id: job.id.clone(), action_id: job.action_id.clone(), early_link: job.early_link, in_outbox: job.in_outbox, destination: job.destination });
        }
    }
    // A failed upload whose link was copied is given up: its link goes too.
    if let Some(stopped) = &stopped {
        quick_share::withdraw_early_link(core, stopped);
    }
    // An unfinished multipart upload stays, so the same file uploaded again
    // continues it.
    core.uploads.job_sessions.lock().unwrap().remove(job_id);
    core.notify(events::JOBS_CHANGED);
}

fn drain(core: &SharedCore) {
    {
        let mut jobs = core.uploads.jobs.lock().unwrap();
        let mut active = jobs.iter().filter(|job| matches!(job.state, JobState::Uploading { .. })).count();
        // The user's own uploads first, then watched folders' files: a drop
        // of thousands into a watched folder never holds up a pasted
        // screenshot.
        let mut order: Vec<usize> = (0..jobs.len()).filter(|&index| jobs[index].input.watched.is_none()).collect();
        order.extend((0..jobs.len()).filter(|&index| jobs[index].input.watched.is_some()));
        for index in order {
            let job = &mut jobs[index];
            if active >= MAX_CONCURRENT {
                break;
            }
            if job.state != JobState::Waiting {
                continue;
            }
            active += 1;
            job.state = JobState::Uploading { progress: 0.0, resuming: false };
            job.sender.send_replace(job.state.clone());
            job.first_attempt_at.get_or_insert_with(crate::util::now_millis);
            job.sending = None;
            let task_core = core.clone();
            let job_id = job.id.clone();
            let input = job.input.clone();
            let destination = job.destination.clone();
            job.task = Some(tauri::async_runtime::spawn(async move {
                match run(&task_core, &job_id, &input, &destination).await {
                    Ok(outcome) => finish(&task_core, &job_id, &destination, outcome).await,
                    Err(failure) => fail(&task_core, &job_id, &input, failure),
                }
            }));
        }
    }
    core.notify(events::JOBS_CHANGED);
}

/// Files made for one upload (a folder's ZIP, a converted photo, a photo
/// without its location), deleted once it's over, however it ends.
#[derive(Default)]
struct Scratch {
    zip: Option<PathBuf>,
    cleaned: Option<PathBuf>,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Some(zip) = &self.zip {
            folder_upload::remove_zip(zip);
        }
        if let Some(cleaned) = &self.cleaned {
            image_metadata::remove_copy(cleaned);
        }
    }
}

/// A finished upload.
struct Outcome {
    /// The input as it went up: a copied link's downloaded file, under the
    /// name it goes by.
    input: UploadInput,
    result: UploadResult,
    /// What's copied: the public URL, or a temporary link.
    link: String,
    /// The name it goes by: the original's, with a converted photo's new
    /// extension.
    filename: String,
    /// SHA-256 of what was uploaded, when it was worked out.
    content_hash: Option<String>,
    /// SHA-256 of the file itself, when it went up unchanged and was hashed.
    original_hash: Option<String>,
    /// The earlier upload of the same file whose link was used instead.
    reused: Option<UploadRecord>,
    /// The thumbnail made from the file that went up, if any.
    thumbnail: Option<Vec<u8>>,
    /// The link copied before the bytes went, and (when the file did go to
    /// its key) what became of its short link.
    early: Option<EarlyLink>,
    early_attempt: Option<crate::short_links::Attempt>,
    _scratch: Scratch,
}

/// Why an upload failed, and whether it may work later on its own (the
/// network dropped, the server had a problem).
pub struct Failure {
    pub message: String,
    pub transient: bool,
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self { message, transient: false }
    }
}

impl From<StorageError> for Failure {
    fn from(error: StorageError) -> Self {
        Self { transient: error.is_transient(), message: error.to_string() }
    }
}

impl From<remote_download::DownloadError> for Failure {
    fn from(error: remote_download::DownloadError) -> Self {
        Self { transient: error.is_transient(), message: error.to_string() }
    }
}

impl Failure {
    /// Windows has no connection: the upload waits for one rather than
    /// trying (and failing) first.
    fn offline() -> Self {
        Self { message: t!("Waiting for a connection"), transient: true }
    }
}

/// What a job brings to its try: kept from earlier ones (a downloaded
/// link, the copied link's key) and the action it's part of.
struct JobContext {
    action_id: Option<String>,
    early_link: Option<EarlyLink>,
    downloaded: Option<Downloaded>,
    first_attempt_at: i64,
}

fn job_context(core: &SharedCore, job_id: &str) -> JobContext {
    let jobs = core.uploads.jobs.lock().unwrap();
    let job = jobs.iter().find(|job| job.id == job_id);
    JobContext {
        action_id: job.and_then(|job| job.action_id.clone()),
        early_link: job.and_then(|job| job.early_link.clone()),
        downloaded: job.and_then(|job| job.downloaded.clone()),
        first_attempt_at: job.and_then(|job| job.first_attempt_at).unwrap_or_else(crate::util::now_millis),
    }
}

fn update_job(core: &SharedCore, job_id: &str, change: impl FnOnce(&mut Job)) {
    if let Some(job) = core.uploads.jobs.lock().unwrap().iter_mut().find(|job| job.id == job_id) {
        change(job);
    }
}

/// Downloads a copied link's file into the folder its placeholder is in.
async fn download_link(core: &SharedCore, job_id: &str, input: &UploadInput, remote: &RemoteSource) -> Result<Downloaded, Failure> {
    if !crate::win32::is_online() {
        return Err(Failure::offline());
    }
    let folder = input.path.parent().map(Path::to_path_buf).unwrap_or_else(std::env::temp_dir);
    update_job(core, job_id, |job| job.downloading = true);
    core.notify(events::JOBS_CHANGED);
    let result = remote_download::download(remote, &folder, |fraction| report_progress(core, job_id, fraction)).await;
    update_job(core, job_id, |job| {
        job.downloading = false;
        if let JobState::Uploading { resuming, .. } = job.state {
            job.state = JobState::Uploading { progress: 0.0, resuming };
            job.sender.send_replace(job.state.clone());
        }
        if let Ok(downloaded) = &result {
            job.downloaded = Some(downloaded.clone());
        }
    });
    core.notify(events::JOBS_CHANGED);
    Ok(result?)
}

/// Uploads the file, or finds it already uploaded.
async fn run(core: &SharedCore, job_id: &str, input: &UploadInput, destination: &DestinationConfig) -> Result<Outcome, Failure> {
    let credentials = credentials::load(&destination.id).map_err(|error| error.to_string())?;
    let provider = S3Provider::new(destination.clone(), credentials);
    let context = job_context(core, job_id);

    // A copied link: its file is downloaded first (once, kept for retries),
    // and then it's like any file.
    let mut input = input.clone();
    if let Some(remote) = input.remote.clone() {
        let downloaded = match context.downloaded.clone() {
            Some(downloaded) => downloaded,
            None => download_link(core, job_id, &input, &remote).await?,
        };
        input.original_filename = display_name(&input.original_filename, &remote, &downloaded);
        input.path = downloaded.path;
    }
    let input = &input;
    // A link already handed out names its key: that's where this upload
    // goes, whatever else is in the bucket.
    let pinned_key = context.early_link.as_ref().map(|early| early.object_key.clone());

    // A folder goes up as a ZIP made on the spot, and a photo converted,
    // or without the metadata the destination removes; all made off the
    // async runtime's workers.
    let policy = destination.image_metadata();
    let mut scratch = Scratch::default();
    let mut path = input.path.clone();
    let mut filename = input.original_filename.clone();
    let mut new_extension = None;
    let zipped = path.is_dir();
    if zipped {
        let folder = path.clone();
        let zipped = tauri::async_runtime::spawn_blocking(move || folder_upload::zip(&folder, policy))
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        scratch.zip = Some(zipped.clone());
        path = zipped;
    } else {
        let mut processed = None;
        // A replacement keeps its key's extension, so its format stays too.
        let processing = match &input.replacing {
            Some(_) => destination
                .image_processing()
                .map(|settings| ImageProcessing { format: ImageFormat::Original, ..settings })
                .filter(ImageProcessing::is_active),
            None => destination.image_processing(),
        };
        if let Some(settings) = processing {
            let original = path.clone();
            match tauri::async_runtime::spawn_blocking(move || image_processing::process(&original, settings, policy))
                .await
                .map_err(|error| error.to_string())?
            {
                Ok(result) => processed = result,
                // Uploaded as it is, as if processing were off.
                Err(error) => log::warn!("Could not process {}: {error}", input.original_filename),
            }
        }
        if let Some(processed) = processed {
            if let Some(extension) = processed.new_extension {
                filename = image_processing::renamed(&filename, extension);
                new_extension = Some(extension);
            }
            scratch.cleaned = Some(processed.path.clone());
            path = processed.path;
        } else {
            let original = path.clone();
            let cleaned = tauri::async_runtime::spawn_blocking(move || image_metadata::stripped_copy(&original, policy))
                .await
                .map_err(|error| error.to_string())?
                .map_err(|error| error.to_string())?;
            if let Some(cleaned) = cleaned {
                scratch.cleaned = Some(cleaned.clone());
                path = cleaned;
            }
        }
    }
    let file_size = std::fs::metadata(&path).map_err(|error| error.to_string())?.len();

    // The bytes that go up are hashed (as they're read, never loaded whole)
    // for the {md5} and {sha256} tokens and for finding an earlier upload
    // of the same file. A ZIP made on the spot is never quite the same.
    let generated_key = input.object_key.is_none() && input.folder_key.is_none();
    let reuse_links = core.settings.get().reuse_duplicate_links && !zipped && pinned_key.is_none();
    // A watched folder's file may be hashed already; it's used as long as
    // the bytes going up are the file's own (and {md5} isn't wanted).
    let original_bytes = !zipped && scratch.cleaned.is_none();
    let known = input
        .watched
        .as_ref()
        .and_then(|source| source.sha256.clone())
        .filter(|_| original_bytes && !destination.object_path_template.contains("{md5}"));
    let hashes = if let Some(sha256) = known {
        Some(ContentHashes { md5: String::new(), sha256 })
    } else if reuse_links || (generated_key && pinned_key.is_none() && output::uses_hashes(&destination.object_path_template)) {
        let hashed = path.clone();
        let hashes = tauri::async_runtime::spawn_blocking(move || crate::util::content_hashes(&hashed))
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        Some(hashes)
    } else {
        None
    };
    let content_hash = hashes.as_ref().filter(|_| reuse_links).map(|hashes| hashes.sha256.clone());
    let original_hash = hashes.as_ref().filter(|_| original_bytes).map(|hashes| hashes.sha256.clone());

    // Never for a file going to an exact key, or one of a folder keeping
    // its structure: its key is where it has to be.
    if let Some(sha256) = content_hash.as_deref().filter(|_| generated_key && input.group.is_none()) {
        if let Some(record) = earlier_upload(core, &provider, destination, input, sha256).await {
            let link = link_for(&provider, destination, &record.object_key, &record.public_url).await;
            return Ok(Outcome {
                input: input.clone(),
                result: UploadResult {
                    object_key: record.object_key.clone(),
                    public_url: record.public_url.clone(),
                    byte_size: record.byte_size,
                },
                link,
                filename,
                content_hash,
                original_hash,
                reused: Some(record),
                thumbnail: None,
                early: None,
                early_attempt: None,
                _scratch: scratch,
            });
        }
    }

    // The thumbnail is made while the file goes up, from exactly what's
    // sent (converted, metadata removed), and waited for before the job
    // finishes: a watched folder or the local API may move or delete the
    // file as soon as it has, and the copies here go when the job ends.
    let mut thumbnail = (destination.thumbnail_mode() != ThumbnailMode::Off && !zipped)
        .then(|| tauri::async_runtime::spawn(crate::thumbnails::generate(path.clone())));

    // A big file continues an upload of it left unfinished, under its key.
    let source = (!zipped).then(|| SourceFile::of(&input.path)).flatten();
    let key_basis = generated_key.then(|| key_basis_for(input, destination, &filename));
    let mut resumed = None;
    if file_size > SINGLE_UPLOAD_LIMIT {
        if let Some(source) = &source {
            for outdated in core.upload_sessions.outdated_for(&destination.id, source) {
                multipart::abort(&provider, &core.upload_sessions, &outdated).await;
            }
        }
        let busy: Vec<String> = core.uploads.job_sessions.lock().unwrap().iter().filter(|(job, _)| *job != job_id).map(|(_, id)| id.clone()).collect();
        let candidate = Candidate {
            destination_id: &destination.id,
            bucket: &destination.bucket,
            file_size,
            source: source.as_ref(),
            sha256: hashes.as_ref().map(|hashes| hashes.sha256.as_str()),
        };
        // An exact key, or the one a copied link points at.
        let exact_key = pinned_key.clone().or_else(|| (!generated_key).then(|| object_key_for(input, destination, &filename, hashes.as_ref(), new_extension)));
        for session in core.upload_sessions.matching(&candidate, &busy) {
            match &exact_key {
                Some(key) => {
                    if resumed.is_none() && session.object_key == *key {
                        resumed = Some(session);
                    }
                }
                // A key from the template is only kept while the template,
                // the name, and the watched folder's subfolder it was made
                // from are the same (and the "Delete after" folder). One
                // made from something else would put the file where it no
                // longer belongs, so it's thrown away.
                None => {
                    let same = session.key_basis.is_some()
                        && session.key_basis == key_basis
                        && crate::expiry::days_in_key(&session.object_key) == input.expire_after_days();
                    if same && resumed.is_none() {
                        resumed = Some(session);
                    } else if !same {
                        multipart::abort(&provider, &core.upload_sessions, &session).await;
                    }
                }
            }
        }
    }
    // An unfinished upload of this job that isn't the one continued now
    // (a folder's ZIP, made again) is thrown away.
    let previous = core.uploads.job_sessions.lock().unwrap().get(job_id).cloned();
    if let Some(previous) = previous.filter(|id| resumed.as_ref().is_none_or(|session| session.id != *id)) {
        core.uploads.job_sessions.lock().unwrap().remove(job_id);
        if let Some(session) = core.upload_sessions.get(&previous) {
            multipart::abort(&provider, &core.upload_sessions, &session).await;
        }
    }

    let mut object_key = match (&pinned_key, &resumed) {
        (Some(key), _) => key.clone(),
        (None, Some(session)) => session.object_key.clone(),
        (None, None) => object_key_for(input, destination, &filename, hashes.as_ref(), new_extension),
    };
    // A converted photo going to a name picked in the bucket browser must
    // not land on another file that has its new name.
    if let (Some(original), true, None) = (&input.object_key, new_extension.is_some(), &resumed) {
        if *original != object_key && provider.object_exists(&object_key).await.unwrap_or(false) {
            let (folder, name) = object_key.rsplit_once('/').map_or(("", object_key.as_str()), |(folder, name)| (folder, name));
            let folder = if folder.is_empty() { String::new() } else { format!("{folder}/") };
            object_key = crate::bucket::available_key(&provider, name, &folder, &[]).await.map_err(|error| error.to_string())?;
        }
    }
    let content_type = upload_content_type(&path, &filename);
    let progress = Arc::new(Progress::default());
    // Only S3 and R2 are known to honor If-None-Match on uploads.
    let only_if_new = input.keep_existing && input.object_key.is_some() && destination.preset.supports_conditional_writes();
    let sending = Sending {
        core,
        job_id,
        provider: &provider,
        destination,
        path: &path,
        content_type: &content_type,
        file_size,
        source: source.clone(),
        sha256: hashes.as_ref().map(|hashes| hashes.sha256.clone()),
        key_basis: key_basis.clone(),
        progress: progress.clone(),
    };
    // Instant link: the link is copied now, before the bytes go, and the
    // upload is kept until they're there (docs/capture-and-instant-links.md).
    let mut early = context.early_link.clone();
    if early.is_none() && core.settings.get().copy_link_right_away && quick_share::can_copy_early(input, context.action_id.is_some()) {
        early = Some(quick_share::copy_early_link(core, job_id, input, destination, &provider, &object_key, &filename).await);
    }
    // Offline, the upload waits for the network right away rather than
    // after the SDK's retries.
    if !crate::win32::is_online() {
        return Err(Failure::offline());
    }
    update_job(core, job_id, |job| job.sending = Some((object_key.clone(), file_size)));

    // A key with {short} never replaces a file that has it already: a
    // taken one is made again with a new code (see `short_keys`).
    let short_key = generated_key && output::uses_short_code(&destination.object_path_template);
    let key_filename = filename.clone();
    let short_pinned = early.as_ref().map(|early| early.object_key.clone());
    let upload = async {
        if short_key {
            let mut target = ShortUpload { sending: &sending, resumed, pinned: short_pinned, first_attempt_at: context.first_attempt_at };
            let new_key = || object_key_for(input, destination, &key_filename, hashes.as_ref(), new_extension);
            return short_keys::write_short_key(&mut target, object_key.clone(), destination.preset.supports_conditional_writes(), new_key).await;
        }
        if file_size <= SINGLE_UPLOAD_LIMIT {
            let mut key = object_key.clone();
            let mut taken = Vec::new();
            loop {
                match provider.upload(&path, &key, &content_type, progress.clone(), only_if_new).await {
                    Err(StorageError::AlreadyExists) if taken.len() < MAX_NAME_RETRIES => {
                        taken.push(key.clone());
                        let (folder, _) = crate::bucket::split_key(&key);
                        let name = crate::bucket::split_key(&object_key).1;
                        key = crate::bucket::available_key(&provider, name, folder, &taken).await?;
                    }
                    result => return result.map(|_| key),
                }
            }
        }
        sending.multipart(&object_key, resumed, false).await?;
        Ok(object_key.clone())
    };
    tokio::pin!(upload);
    let mut ticker = tokio::time::interval(Duration::from_millis(250));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut reported = 0.0;
    loop {
        tokio::select! {
            result = &mut upload => {
                let object_key: String = result?;
                let public_url = output::resolve_public_url(&destination.public_base_url, &object_key);
                let (link, early_attempt) = match &early {
                    // Made when the link was copied.
                    Some(early) if early.object_key == object_key => (early.link.clone(), Some(quick_share::early_attempt(early))),
                    // The key was taken after all (a {short} clash), so the
                    // copied link points elsewhere: its short link goes, and
                    // the new link is copied in its place.
                    Some(early) => {
                        quick_share::delete_unstored_short_link(early, destination);
                        (link_for(&provider, destination, &object_key, &public_url).await, None)
                    }
                    None => (link_for(&provider, destination, &object_key, &public_url).await, None),
                };
                let thumbnail = match thumbnail.take() {
                    Some(task) => task.await.ok().flatten(),
                    None => None,
                };
                return Ok(Outcome {
                    input: input.clone(),
                    result: UploadResult { object_key: object_key.clone(), public_url, byte_size: file_size as i64 },
                    link,
                    filename,
                    content_hash,
                    original_hash,
                    reused: None,
                    thumbnail,
                    early,
                    early_attempt,
                    _scratch: scratch,
                });
            }
            _ = ticker.tick() => {
                if let Some(fraction) = progress.fraction() {
                    // Whole percents are plenty, and spare the windows a
                    // refresh every tick.
                    if fraction - reported >= 0.01 {
                        reported = fraction;
                        report_progress(core, job_id, fraction);
                    }
                }
                if progress.idle_millis() > STALL_TIMEOUT_MS {
                    return Err(StorageError::Network(t!("The connection stopped responding.")).into());
                }
            }
        }
    }
}

/// What a job sends and where from, for its single or multipart upload.
struct Sending<'a> {
    core: &'a SharedCore,
    job_id: &'a str,
    provider: &'a S3Provider,
    destination: &'a DestinationConfig,
    path: &'a Path,
    content_type: &'a str,
    file_size: u64,
    source: Option<SourceFile>,
    sha256: Option<String>,
    key_basis: Option<String>,
    progress: Arc<Progress>,
}

impl Sending<'_> {
    /// Sends the file in parts to `key`: continuing `resumed` (whose key it
    /// is), or a new multipart upload. `only_if_new` as for
    /// `multipart::upload`.
    async fn multipart(&self, key: &str, resumed: Option<Session>, only_if_new: bool) -> Result<(), StorageError> {
        let resuming = resumed.is_some();
        let session: Session = match resumed {
            Some(session) => session,
            None => {
                multipart::start(
                    self.provider,
                    &self.core.upload_sessions,
                    &self.destination.id,
                    &self.destination.bucket,
                    key,
                    self.content_type,
                    self.file_size,
                    self.source.clone(),
                    self.sha256.clone(),
                    self.key_basis.clone(),
                )
                .await?
            }
        };
        self.core.uploads.job_sessions.lock().unwrap().insert(self.job_id.to_string(), session.id.clone());
        if resuming {
            set_resuming(self.core, self.job_id);
        }
        let result = multipart::upload(
            self.provider,
            &self.core.upload_sessions,
            session,
            self.path,
            self.content_type,
            resuming,
            self.progress.clone(),
            only_if_new,
        )
        .await;
        // Done, or aborted because the key was taken: either way there's
        // nothing left to continue.
        if matches!(result, Ok(()) | Err(StorageError::AlreadyExists)) {
            self.core.uploads.job_sessions.lock().unwrap().remove(self.job_id);
        }
        result
    }
}

/// An upload to a key with {short}, for `short_keys::write_short_key`.
struct ShortUpload<'a> {
    sending: &'a Sending<'a>,
    /// The unfinished upload being continued, until a key is tried.
    resumed: Option<Session>,
    /// The key a copied link points at, if one was copied.
    pinned: Option<String>,
    first_attempt_at: i64,
}

impl ShortUpload<'_> {
    /// A try whose answer was lost may have written the key already: an
    /// object there, as big as this file and written since the first try
    /// (two minutes of slack for clocks), is this upload's own.
    async fn is_own_earlier_write(&self, key: &str) -> bool {
        if self.pinned.as_deref() != Some(key) {
            return false;
        }
        match self.sending.provider.object_info(key).await {
            Ok(Some(object)) => {
                object.size == self.sending.file_size as i64
                    && object.last_modified.is_some_and(|modified| modified >= self.first_attempt_at - OWN_WRITE_LEEWAY_MILLIS)
            }
            _ => false,
        }
    }
}

/// How much earlier than the first try an object may be dated and still be
/// the upload's own write.
const OWN_WRITE_LEEWAY_MILLIS: i64 = 120_000;

impl ShortKeyTarget for ShortUpload<'_> {
    async fn exists(&mut self, key: &str) -> Result<bool, StorageError> {
        if !self.sending.provider.object_exists(key).await? {
            return Ok(false);
        }
        Ok(!self.is_own_earlier_write(key).await)
    }

    async fn write(&mut self, key: &str, only_if_new: bool) -> Result<(), StorageError> {
        let sending = self.sending;
        update_job(sending.core, sending.job_id, |job| job.sending = Some((key.to_string(), sending.file_size)));
        let result = if sending.file_size <= SINGLE_UPLOAD_LIMIT {
            sending.provider.upload(sending.path, key, sending.content_type, sending.progress.clone(), only_if_new).await.map(drop)
        } else {
            // A continued upload keeps its key unless that one is taken;
            // then its parts can't move, so it's thrown away and the file
            // starts over under the new key.
            let mut resumed = self.resumed.take();
            if let Some(session) = resumed.take_if(|session| session.object_key != key) {
                sending.core.uploads.job_sessions.lock().unwrap().remove(sending.job_id);
                multipart::abort(sending.provider, &sending.core.upload_sessions, &session).await;
            }
            sending.multipart(key, resumed, only_if_new).await
        };
        match result {
            Err(StorageError::AlreadyExists) if self.is_own_earlier_write(key).await => Ok(()),
            other => other,
        }
    }
}

/// The Content-Type a file is sent with: its type, plus `; charset=utf-8`
/// for plain text, Markdown and CSV whose bytes are UTF-8 (all text Aktar
/// saves from the clipboard is), so a browser doesn't guess another charset
/// and garble "ğ" or "é". History keeps the bare type.
fn upload_content_type(path: &Path, filename: &str) -> String {
    const CHARSET_TYPES: [&str; 4] = ["text/plain", "text/markdown", "text/x-markdown", "text/csv"];
    const MAX_CHARSET_CHECK_BYTES: u64 = 16 * 1024 * 1024;
    let content_type = output::content_type(filename);
    if !CHARSET_TYPES.contains(&content_type.as_str()) {
        return content_type;
    }
    let size = std::fs::metadata(path).map(|metadata| metadata.len()).unwrap_or(0);
    if size == 0 || size > MAX_CHARSET_CHECK_BYTES {
        return content_type;
    }
    match std::fs::read(path) {
        Ok(data) if std::str::from_utf8(&data).is_ok() => format!("{content_type}; charset=utf-8"),
        _ => content_type,
    }
}

/// Where the file goes: the exact key it was given (a folder's file, the
/// bucket browser), or one from the destination's path template, in its
/// `tmp/{N}d/` folder when it expires. A converted photo's key gets the
/// new extension.
fn object_key_for(
    input: &UploadInput,
    destination: &DestinationConfig,
    filename: &str,
    hashes: Option<&ContentHashes>,
    new_extension: Option<&str>,
) -> String {
    if let Some(key) = &input.object_key {
        return with_extension(key, new_extension);
    }
    let key = match &input.folder_key {
        Some(key) => with_extension(key, new_extension),
        None => match &input.watched {
            Some(source) => output::generate_key_in(&destination.object_path_template, filename, hashes, &source.key_place),
            None => output::generate_key(&destination.object_path_template, filename, hashes),
        },
    };
    match input.expire_after_days() {
        Some(days) => crate::expiry::expiring_key(&key, days),
        None => key,
    }
}

/// What a key from the destination's path template is made from, for
/// telling whether an unfinished upload's key still fits: the template,
/// the name the file goes by (after any format change), and a watched
/// folder's subfolder.
fn key_basis_for(input: &UploadInput, destination: &DestinationConfig, filename: &str) -> String {
    let subpath = input.watched.as_ref().map_or("", |source| source.key_place.subpath.as_str());
    multipart::key_basis(&destination.object_path_template, filename, subpath)
}

/// `key` with its last component's extension swapped for `extension`.
fn with_extension(key: &str, extension: Option<&str>) -> String {
    let Some(extension) = extension else { return key.to_string() };
    let (folder, name) = match key.rfind('/') {
        Some(slash) => key.split_at(slash + 1),
        None => ("", key),
    };
    format!("{folder}{}", image_processing::renamed(name, extension))
}

/// The link to copy: the public URL, or a fresh temporary link when the
/// destination is set to one. Signing happens locally, so it only fails on
/// a broken endpoint, where the public URL is the better fallback.
async fn link_for(provider: &S3Provider, destination: &DestinationConfig, object_key: &str, public_url: &str) -> String {
    if let Some(seconds) = destination.temporary_link {
        if let Ok(signed) = provider.temporary_url(object_key, seconds).await {
            return signed;
        }
    }
    public_url.to_string()
}

/// How much later than its history record an uploaded object may be dated
/// and still be that upload: clocks differ, and a multipart upload's object
/// is dated when it's put together at the end.
const REUSE_DATE_LEEWAY_MILLIS: i64 = 5 * 60_000;

/// An earlier upload of the same bytes to this destination that's still
/// in the bucket, unchanged, and expires like this one would: neither ever
/// does, or both are in the same `tmp/{N}d/` folder and it hasn't yet.
/// None when the bucket can't be asked: then the file is just uploaded.
async fn earlier_upload(
    core: &SharedCore,
    provider: &S3Provider,
    destination: &DestinationConfig,
    input: &UploadInput,
    sha256: &str,
) -> Option<UploadRecord> {
    let now = crate::util::now_millis();
    for record in core.history.with_content(&destination.id, sha256) {
        if !expires_alike(&record, input.expire_after_days(), now) {
            continue;
        }
        // Something else went to its key since ({filename} paths): the
        // link serves that now.
        if overwritten_later(&record, &core.history.with_object(&destination.id, &record.object_key)) {
            continue;
        }
        match provider.object_info(&record.object_key).await {
            Ok(Some(object)) if object_is_the_upload(&record, &object) => return Some(record),
            Ok(_) => continue,
            Err(_) => return None,
        }
    }
    None
}

/// Whether a later upload to the record's key (`at_key`, all uploads to
/// it) had other contents, or ones that weren't recorded.
fn overwritten_later(record: &UploadRecord, at_key: &[UploadRecord]) -> bool {
    at_key.iter().any(|other| {
        other.id != record.id
            && other.last_write() > record.last_write()
            && (other.content_hash.is_none() || other.content_hash != record.content_hash)
    })
}

/// Whether the object in the bucket is still the one the record uploaded:
/// the same size, and not written after it (anything written by other
/// means since would be).
fn object_is_the_upload(record: &UploadRecord, object: &BucketObject) -> bool {
    // A file replaced in place was written again then.
    let written = record.last_write();
    object.size == record.byte_size && object.last_modified.is_some_and(|modified| modified <= written + REUSE_DATE_LEEWAY_MILLIS)
}

fn expires_alike(record: &UploadRecord, days: Option<u32>, now: i64) -> bool {
    let record_days = crate::expiry::days_in_key(&record.object_key);
    match days {
        None => record.expires_at.is_none() && record_days.is_none(),
        Some(days) => record_days == Some(days) && record.expires_at.is_some_and(|at| at > now),
    }
}

fn set_resuming(core: &SharedCore, job_id: &str) {
    {
        let mut jobs = core.uploads.jobs.lock().unwrap();
        let Some(job) = jobs.iter_mut().find(|job| job.id == job_id) else { return };
        let JobState::Uploading { progress, .. } = job.state else { return };
        job.state = JobState::Uploading { progress, resuming: true };
        job.sender.send_replace(job.state.clone());
    }
    core.notify(events::JOBS_CHANGED);
}

fn report_progress(core: &SharedCore, job_id: &str, fraction: f64) {
    {
        let mut jobs = core.uploads.jobs.lock().unwrap();
        let Some(job) = jobs.iter_mut().find(|job| job.id == job_id) else { return };
        let JobState::Uploading { resuming, .. } = job.state else { return };
        job.state = JobState::Uploading { progress: fraction, resuming };
        job.sender.send_replace(job.state.clone());
    }
    core.notify(events::JOBS_CHANGED);
}

fn set_state(core: &SharedCore, job_id: &str, state: JobState) {
    let mut jobs = core.uploads.jobs.lock().unwrap();
    if let Some(index) = jobs.iter().position(|job| job.id == job_id) {
        jobs[index].sender.send_replace(state.clone());
        if state.is_finished_for_good() {
            jobs.remove(index);
        } else {
            jobs[index].state = state;
            jobs[index].task = None;
        }
    }
}

/// History keeps the public URL; `outcome.link` is what's copied. An
/// earlier upload whose link is reused stays the one history entry.
async fn finish(core: &SharedCore, job_id: &str, destination: &DestinationConfig, outcome: Outcome) {
    let Outcome { input, result, link, filename, content_hash, original_hash, reused, thumbnail, early, early_attempt, _scratch } = outcome;
    let input = &input;
    let was_reused = reused.is_some();
    let replaced = input.replacing.is_some();
    let shortening = match early_attempt {
        Some(attempt) => attempt,
        None => shorten(core, input, destination, &result, &link, reused.as_ref()).await,
    };
    let record = match reused {
        Some(record) => Some(record),
        None if replaced => {
            let record = replaced_record(core, input, destination, &result, &filename, content_hash.as_deref());
            if let Some(record) = &record {
                core.history.thumbnails.remove(&record.id);
                match &thumbnail {
                    Some(data) => core.history.thumbnails.store(&record.id, data),
                    None if destination.thumbnail_mode() != ThumbnailMode::Off => core.history.thumbnails.mark_unavailable(&record.id),
                    None => {}
                }
            }
            // The bucket view's thumbnail of the old file, and the bucket's.
            crate::thumbnails::remote::forget(core, &destination.id, &result.object_key);
            tauri::async_runtime::spawn(update_bucket_thumbnails(core.clone(), destination.clone(), result.object_key.clone(), thumbnail));
            crate::cloudflare::purge_in_background(core, destination, &result.public_url);
            record
        }
        None => {
            let mime_type = output::content_type(&filename);
            let record = core.history.insert(NewRecord {
                local_filename: &filename,
                object_key: &result.object_key,
                public_url: &result.public_url,
                destination,
                mime_type: &mime_type,
                byte_size: result.byte_size,
                expire_after_days: input.expire_after_days(),
                content_hash: content_hash.as_deref(),
                watched_folder: input.watched.as_ref().map(|source| (source.folder_id.as_str(), source.folder_name.as_str())),
            });
            match &thumbnail {
                Some(data) => core.history.thumbnails.store(&record.id, data),
                // Nothing could be made from the file itself; downloading
                // it again later wouldn't do better.
                None if destination.thumbnail_mode() != ThumbnailMode::Off => core.history.thumbnails.mark_unavailable(&record.id),
                None => {}
            }
            tauri::async_runtime::spawn(update_bucket_thumbnails(core.clone(), destination.clone(), result.object_key.clone(), thumbnail));
            Some(record)
        }
    };
    let record_id = record.map(|record| record.id).unwrap_or_default();
    // For Undo: the history entry this upload made (not a reused or
    // replaced one). It's done, so it leaves the outbox.
    let (action_id, in_outbox) = core
        .uploads
        .jobs
        .lock()
        .unwrap()
        .iter()
        .find(|job| job.id == job_id)
        .map(|job| (job.action_id.clone(), job.in_outbox))
        .unwrap_or_default();
    if let Some(action_id) = &action_id {
        if was_reused {
            quick_share::note_reused(core, action_id, &filename);
        } else if !replaced && !record_id.is_empty() {
            quick_share::note_record(core, action_id, &record_id);
        }
    }
    if in_outbox || early.is_some() {
        core.outbox.remove(job_id);
    }
    let (short_url, short_failure) = short_link_for(core, shortening, &record_id, &link, &result.public_url);
    if let Some(reason) = &short_failure {
        crate::short_links::notify_failed(core, &filename, reason, &record_id);
    }
    // A watched folder's own hooks run for its files; a reused link isn't
    // an upload at all.
    if input.watched.is_none() && !was_reused {
        let event = if replaced { crate::destination_hooks::Event::Replaced } else { crate::destination_hooks::Event::Uploaded };
        let active = crate::short_links::active(core, &record_id).map(|link| link.short_url);
        let payload = crate::destination_hooks::payload(
            event,
            destination,
            &input.path,
            &filename,
            result.byte_size,
            &result.object_key,
            &result.public_url,
            active.as_deref(),
        );
        crate::destination_hooks::run(core, destination, payload);
    }
    drop(_scratch);
    input.remove_if_temporary();

    set_state(
        core,
        job_id,
        JobState::Succeeded {
            public_url: result.public_url.clone(),
            record_id: record_id.clone(),
            reused: was_reused,
            short_link_error: short_failure.clone(),
        },
    );
    core.notify(events::HISTORY_CHANGED);

    if !was_reused {
        core.emit(
            events::UPLOAD_SUCCEEDED,
            UploadSucceeded {
                destination_id: destination.id.clone(),
                object_key: result.object_key.clone(),
                byte_size: result.byte_size,
            },
        );
    }

    // A watched folder copies and notifies as it's set to, once the
    // original is taken care of, and never closes the panel. One that
    // replaced its upload so the link stays clears the link from the cache.
    if let Some(source) = &input.watched {
        if input.object_key.is_some() && !was_reused {
            crate::cloudflare::purge_in_background(core, destination, &result.public_url);
        }
        let uploaded = crate::watched::engine::Uploaded {
            object_key: result.object_key.clone(),
            url: result.public_url.clone(),
            link: format_link(core, destination, &link, short_url.as_deref(), &filename),
            filename: filename.clone(),
            destination_id: destination.id.clone(),
            reused: was_reused,
            byte_size: result.byte_size,
            content_hash: original_hash,
            short_url: short_url.clone(),
        };
        crate::watched::upload_succeeded(core, source, destination, uploaded).await;
        drain(core);
        return;
    }

    // A file from a folder waits for the rest of it: the links are copied
    // together, with one notification, once the last is done.
    let grouped = input.group.as_ref().is_some_and(|group| {
        let mut groups = core.uploads.open_groups.lock().unwrap();
        let links = groups.get_mut(&group.id);
        let open = links.is_some();
        if let Some(links) = links {
            links.insert(group.index, format_link(core, destination, &link, short_url.as_deref(), &filename));
        }
        open
    });
    if let (true, Some(group)) = (grouped, &input.group) {
        finish_group_if_done(core, group, action_id.as_deref());
    } else {
        let settings = core.settings.get();
        let text = format_link(core, destination, &link, short_url.as_deref(), &filename);
        match &early {
            // The key changed after the link was copied: the new one takes
            // its place, if the clipboard still holds it.
            Some(early) if early.object_key != result.object_key => {
                if quick_share::clipboard_still_holds(core, action_id.as_deref(), &early.copied_text) {
                    quick_share::copy_link(core, action_id.as_deref(), &text);
                }
                show_notification(core, &filename, &t!("The link changed while uploading. The new link is copied; the one copied before doesn’t work."));
            }
            // Copied before the bytes went.
            Some(_) => {}
            None => quick_share::copy_link(core, action_id.as_deref(), &text),
        }
        if settings.show_notification && short_failure.is_none() {
            if replaced {
                show_notification(core, &t!("Replaced"), &t!("{0} keeps its link.", filename));
            } else if was_reused {
                quick_share::notify(core, job_id, action_id.as_deref(), None, &filename, &[&t!("Already uploaded - copied the existing link")]);
            } else {
                let expiry = input.expire_after_days().map(deletes_in).unwrap_or_default();
                quick_share::notify(core, job_id, action_id.as_deref(), Some(&record_id), &t!("Uploaded"), &[&filename, &expiry]);
            }
        } else {
            // A "still uploading" notice would be out of date now.
            quick_share::remove_notification(core, job_id);
        }
        if early.is_none() {
            close_panel_if_wanted(core);
        }
        if let Some(action_id) = &action_id {
            quick_share::extend_undo(core, action_id);
        }
    }
    drain(core);
}

/// The history entry a replaced file updates: the one it was started from,
/// or (from the bucket view) the newest upload to that key. A file never
/// uploaded with Aktar gets a new entry, marked as replaced.
fn replaced_record(
    core: &SharedCore,
    input: &UploadInput,
    destination: &DestinationConfig,
    result: &UploadResult,
    filename: &str,
    content_hash: Option<&str>,
) -> Option<UploadRecord> {
    let target = input
        .replacing
        .as_ref()
        .and_then(|replacing| replacing.record_id.as_deref())
        .and_then(|id| core.history.get(id))
        .or_else(|| core.history.with_object(&destination.id, &result.object_key).into_iter().next());
    let mime_type = output::content_type(filename);
    let target = match target {
        Some(target) => target,
        None => core.history.insert(NewRecord {
            local_filename: filename,
            object_key: &result.object_key,
            public_url: &result.public_url,
            destination,
            mime_type: &mime_type,
            byte_size: result.byte_size,
            expire_after_days: input.expire_after_days(),
            content_hash,
            watched_folder: None,
        }),
    };
    core.history.replaced(
        &target.id,
        Replacement { byte_size: result.byte_size, content_hash, mime_type: &mime_type, expire_after_days: input.expire_after_days() },
    )
}

/// After an upload: its thumbnail goes to the bucket when the destination
/// keeps them there. Any other thumbnail at that key (in another profile's
/// folder on this bucket, or one that couldn't be replaced) belongs to a
/// file that was there before, so it goes.
async fn update_bucket_thumbnails(core: SharedCore, destination: DestinationConfig, object_key: String, thumbnail: Option<Vec<u8>>) {
    let mut stale = crate::thumbnails::bucket_prefixes(&destination, &core.destinations.all());
    if stale.is_empty() {
        return;
    }
    let Ok(credentials) = credentials::load(&destination.id) else { return };
    let storage = S3Provider::new(destination.clone(), credentials);
    if let (Some(prefix), Some(data)) = (destination.bucket_thumbnail_prefix(), thumbnail.filter(|data| crate::thumbnails::is_webp(data))) {
        if crate::thumbnails::bucket::save(&storage, data, &object_key, &prefix).await.is_ok() {
            stale.retain(|other| *other != prefix);
        }
    }
    if let Err(error) = crate::thumbnails::bucket::delete(&storage, &object_key, &stale).await {
        log::debug!("Could not delete old thumbnails of {object_key}: {error}");
    }
}

/// What's copied for `link` (with its short link, when there's one): as the
/// destination says, or Settings > Output.
pub(crate) fn format_link(core: &SharedCore, destination: &DestinationConfig, link: &str, short_url: Option<&str>, filename: &str) -> String {
    let settings = core.settings.get();
    let mode = destination.output_mode.unwrap_or(settings.output_mode);
    output::format(link, short_url, mode, filename, &settings.custom_template)
}

/// The short link for a finished upload, made before anything is copied
/// (docs/short-links.md, rules 2, 6, 8, 9 and 11). A replace keeps the key,
/// so its short link stays as it is; a reused upload's active short link is
/// reused (one for a fresh temporary link is new); a file of a folder
/// uploaded with its structure isn't shortened on its own; `short=0` from
/// the local API makes none.
async fn shorten(
    core: &SharedCore,
    input: &UploadInput,
    destination: &DestinationConfig,
    result: &UploadResult,
    link: &str,
    reused: Option<&UploadRecord>,
) -> crate::short_links::Attempt {
    use crate::short_links::{rules::Shortening, Attempt};
    let now = crate::util::now_millis();
    // `link` differs from the public URL only when it's a temporary link.
    let temporary_expires_at = destination.temporary_link.filter(|_| link != result.public_url).map(|seconds| now + seconds as i64 * 1000);
    if input.replacing.is_some() || input.short_link == Some(false) {
        return Attempt::Skipped;
    }
    let upload_expires_at = match reused {
        Some(record) => {
            if temporary_expires_at.is_none() && crate::short_links::active(core, &record.id).is_some() {
                return Attempt::Skipped;
            }
            record.expires_at
        }
        None => input.expire_after_days().map(|days| now + days as i64 * 86_400_000),
    };
    let shortening = Shortening {
        temporary_expires_at,
        upload_expires_at,
        is_folder_file: input.folder_key.is_some(),
        explicit: input.short_link == Some(true),
    };
    crate::short_links::shorten(destination, link, shortening).await
}

/// The short link to copy for an upload, from what `shorten` did: the one
/// it made (now stored), or, when it skipped and `link` is the public URL,
/// the upload's active one (a replace, a reused link). The failure message
/// when it couldn't be made.
fn short_link_for(
    core: &SharedCore,
    shortening: crate::short_links::Attempt,
    record_id: &str,
    link: &str,
    public_url: &str,
) -> (Option<String>, Option<String>) {
    use crate::short_links::Attempt;
    match shortening {
        Attempt::Created(pending) if !record_id.is_empty() => (Some(crate::short_links::store(core, pending, record_id).short_url), None),
        Attempt::Created(pending) => (Some(pending.created.short_url), None),
        Attempt::Failed(message) => (None, Some(message)),
        Attempt::Skipped if link == public_url => (crate::short_links::active(core, record_id).map(|link| link.short_url), None),
        Attempt::Skipped => (None, None),
    }
}

pub(crate) fn close_panel_if_wanted(core: &SharedCore) {
    if core.settings.get().close_panel_after_upload {
        crate::panel::hide(&core.app);
    }
}

/// Once none of a folder's files is waiting or uploading, copies the links
/// of the ones that made it, one per line and in folder order. Failed
/// files have their own notifications and can still be retried; a retry
/// then copies just its own link.
fn finish_group_if_done(core: &SharedCore, group: &UploadGroup, action_id: Option<&str>) {
    let still_going = core.uploads.jobs.lock().unwrap().iter().any(|job| {
        job.input.group.as_ref().is_some_and(|other| other.id == group.id)
            && matches!(job.state, JobState::Waiting | JobState::Uploading { .. })
    });
    if still_going {
        return;
    }
    let Some(links) = core.uploads.open_groups.lock().unwrap().remove(&group.id) else { return };
    // Undo is taking the files back: nothing to copy or announce.
    if links.is_empty() || action_id.is_some_and(|action_id| quick_share::is_undoing(core, action_id)) {
        return;
    }
    let copied: Vec<String> = links.into_values().collect();
    quick_share::copy_link(core, action_id, &copied.join("\n"));
    if core.settings.get().show_notification {
        // A drop spread over several destinations has no folder name.
        let summary = if group.name.is_empty() {
            t!("{0} files", copied.len())
        } else if copied.len() == group.count {
            t!("{0} ({1} files)", group.name, group.count)
        } else {
            t!("{0} ({1} of {2} files)", group.name, copied.len(), group.count)
        };
        quick_share::notify(core, &group.id, action_id, None, &t!("Uploaded"), &[&summary]);
    }
    if let Some(action_id) = action_id {
        quick_share::extend_undo(core, action_id);
    }
    close_panel_if_wanted(core);
}

/// A failure for want of a connection (or while Windows says it's offline)
/// waits for the network, also for a job whose link wasn't copied yet; a
/// watched folder's files have their own retries, and an upload someone
/// waits for reports the failure. Anything else fails and waits for Retry.
fn fail(core: &SharedCore, job_id: &str, input: &UploadInput, failure: Failure) {
    let Failure { message, transient } = failure;
    let (name, action_id, link_copied) = core
        .uploads
        .jobs
        .lock()
        .unwrap()
        .iter()
        .find(|job| job.id == job_id)
        .map(|job| (job.display_name(), job.action_id.clone(), job.early_link.is_some()))
        .unwrap_or_else(|| (input.original_filename.clone(), None, false));
    if input.watched.is_none() && !input.waited && (transient || !crate::win32::is_online()) {
        quick_share::wait_for_network(core, job_id, message);
        if let Some(group) = &input.group {
            finish_group_if_done(core, group, action_id.as_deref());
        }
        drain(core);
        return;
    }
    set_state(core, job_id, JobState::Failed { message: message.clone() });
    quick_share::note_outbox_failure(core, job_id, Some(message.clone()));
    // A watched folder says so itself, a batch at a time, and tries again
    // when it may work.
    match &input.watched {
        Some(source) => crate::watched::upload_failed(core, source, message, transient),
        None => {
            let detail = format!("{name}: {message}");
            let copied = t!("The copied link won’t work until the file is uploaded.");
            let lines: Vec<&str> = if link_copied { vec![&detail, &copied] } else { vec![&detail] };
            // In place of its "Link copied" notice, if it had one.
            quick_share::notify(core, job_id, None, None, &t!("Upload failed"), &lines);
        }
    }
    if let Some(group) = &input.group {
        finish_group_if_done(core, group, action_id.as_deref());
    }
    drain(core);
}

/// "Deletes in 7 days", as the history badge words it on the upload's day.
fn deletes_in(days: u32) -> String {
    if days == 1 {
        t!("Deletes in 1 day")
    } else {
        t!("Deletes in {0} days", days)
    }
}

pub fn show_notification(core: &SharedCore, title: &str, body: &str) {
    let result = if crate::package::is_packaged() {
        crate::package::show_notification(title, body)
    } else {
        core.app.notification().builder().title(title).body(body).show().map_err(|error| error.to_string())
    };
    if let Err(error) = result {
        log::warn!("Could not show a notification: {error}");
    }
}

/// A new temporary link to an uploaded file, for sharing it again from
/// history, e.g. when the bucket is private.
pub async fn temporary_url(core: &SharedCore, record: &UploadRecord, seconds: u64) -> Result<String, String> {
    let destination = core
        .destinations
        .all()
        .into_iter()
        .find(|destination| destination.id == record.destination_id)
        .ok_or_else(|| t!("This upload’s destination was removed."))?;
    let credentials = credentials::load(&destination.id).map_err(|error| error.to_string())?;
    S3Provider::new(destination, credentials)
        .temporary_url(&record.object_key, seconds)
        .await
        .map_err(|error| error.to_string())
}

/// Deletes the remote object and, on success, the history entry. If the
/// destination or its credentials are gone there's nothing left to delete
/// remotely, so the entry is cleaned up silently. A real failure from the
/// provider is returned so the caller can keep the record and offer a retry.
pub async fn delete_remote(core: &SharedCore, record_id: &str) -> Result<(), String> {
    delete_upload(core, record_id).await.map_err(|failure| failure.message)
}

/// `delete_remote`, saying whether a failure may go away on its own: a
/// watched folder deleting the upload of a deleted file tries again then.
pub async fn delete_upload(core: &SharedCore, record_id: &str) -> Result<(), Failure> {
    delete_record(core, record_id, true).await
}

/// The expiry sweep's delete: the upload's short links are marked expired
/// afterwards (rule 10) rather than deleted at the provider.
pub async fn delete_expired(core: &SharedCore, record_id: &str) -> Result<(), String> {
    delete_record(core, record_id, false).await.map_err(|failure| failure.message)
}

/// `clean_up_short_links`: once the file is gone, its short links follow
/// in the background (rule 5).
async fn delete_record(core: &SharedCore, record_id: &str, clean_up_short_links: bool) -> Result<(), Failure> {
    let Some(record) = core.history.get(record_id) else { return Ok(()) };
    let destination = core
        .destinations
        .all()
        .into_iter()
        .find(|destination| destination.id == record.destination_id);
    if let Some(destination) = destination {
        // Only keys that are gone mean there's nothing to delete with; a
        // Credential Manager that can't be read right now keeps the record,
        // so the delete can be tried again.
        match credentials::load(&destination.id) {
            Ok(credentials) => {
                let prefixes = crate::thumbnails::bucket_prefixes(&destination, &core.destinations.all());
                crate::bucket::delete_object(&S3Provider::new(destination.clone(), credentials), &record.object_key, &prefixes).await?;
                crate::thumbnails::remote::forget(core, &destination.id, &record.object_key);
                if clean_up_short_links {
                    crate::short_links::clean_up_after_file_deleted(core, &record.id, Some(&destination), &record.local_filename);
                }
            }
            Err(credentials::CredentialError::NotFound) => {}
            Err(error) => return Err(error.to_string().into()),
        }
    }
    core.history.delete(&record.id);
    core.notify(events::HISTORY_CHANGED);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_uploads() {
        assert_eq!(named("clipboard-1.png", "release notes"), "release notes.png");
        assert_eq!(named("photo.jpg", "  a/b\\c  "), "abc.jpg");
        assert_eq!(named("photo.jpg", " / "), "photo.jpg");
        assert_eq!(named("README", "notes"), "notes");
        assert_eq!(named("README", ".."), "README");
        assert_eq!(named("a.png", "x\u{0}y\tz"), "xyz.png");
    }

    #[test]
    fn swaps_extensions_in_keys() {
        assert_eq!(with_extension("shots/v1.2/photo.png", Some("webp")), "shots/v1.2/photo.webp");
        assert_eq!(with_extension("photo", Some("avif")), "photo.avif");
        assert_eq!(with_extension("a/photo.png", None), "a/photo.png");
    }

    fn record(key: &str, expires_at: Option<i64>) -> UploadRecord {
        UploadRecord {
            id: "R".into(),
            local_filename: "a.png".into(),
            object_key: key.into(),
            public_url: String::new(),
            destination_id: "D".into(),
            destination_name: "Test".into(),
            mime_type: "image/png".into(),
            byte_size: 1,
            created_at: 0,
            expires_at,
            content_hash: Some("h".into()),
            source: None,
            source_name: None,
            replaced_at: None,
            has_thumbnail: false,
            thumbnail_path: None,
            short_url: None,
            short_link_id: None,
            short_provider: None,
        }
    }

    #[test]
    fn reuses_uploads_that_expire_alike() {
        let now = 1_000_000;
        assert!(expires_alike(&record("2026/a.png", None), None, now));
        assert!(!expires_alike(&record("tmp/7d/a.png", None), None, now));
        assert!(!expires_alike(&record("tmp/7d/a.png", Some(now + 1)), None, now));
        assert!(expires_alike(&record("tmp/7d/a.png", Some(now + 1)), Some(7), now));
        assert!(!expires_alike(&record("tmp/7d/a.png", Some(now)), Some(7), now));
        assert!(!expires_alike(&record("tmp/1d/a.png", Some(now + 1)), Some(7), now));
        assert!(!expires_alike(&record("2026/a.png", None), Some(7), now));
    }

    #[test]
    fn reuses_only_what_is_still_at_its_key() {
        let a = UploadRecord { created_at: 1_000, ..record("a.png", None) };
        let later = |id: &str, created_at: i64, hash: Option<&str>| UploadRecord {
            id: id.into(),
            created_at,
            content_hash: hash.map(str::to_string),
            ..record("a.png", None)
        };
        assert!(!overwritten_later(&a, std::slice::from_ref(&a)));
        // The same file uploaded to its key again changes nothing.
        assert!(!overwritten_later(&a, &[later("S", 2_000, Some("h")), a.clone()]));
        // Older uploads to the key don't matter.
        assert!(!overwritten_later(&a, &[a.clone(), later("O", 500, Some("x"))]));
        // A later one with other or unknown contents replaced it.
        assert!(overwritten_later(&a, &[later("B", 2_000, Some("x")), a.clone()]));
        assert!(overwritten_later(&a, &[later("B", 2_000, None), a.clone()]));

        let object = |size: i64, last_modified: Option<i64>| BucketObject { key: "a.png".into(), size, last_modified };
        assert!(object_is_the_upload(&a, &object(1, Some(1_000))));
        assert!(object_is_the_upload(&a, &object(1, Some(1_000 + REUSE_DATE_LEEWAY_MILLIS))));
        assert!(!object_is_the_upload(&a, &object(1, Some(1_001 + REUSE_DATE_LEEWAY_MILLIS))));
        assert!(!object_is_the_upload(&a, &object(2, Some(1_000))));
        assert!(!object_is_the_upload(&a, &object(1, None)));

        // A file replaced in place was written again then, and counts as
        // later than an upload made between.
        let replaced = UploadRecord { replaced_at: Some(9_000), ..a.clone() };
        assert!(object_is_the_upload(&replaced, &object(1, Some(9_000))));
        assert!(!overwritten_later(&replaced, &[later("B", 2_000, Some("x")), replaced.clone()]));
    }

    #[test]
    fn expires_only_on_destinations_with_rules() {
        // The sticky setting applies once the bucket has the rules.
        assert_eq!(settled_expiry(Expiry::FromSettings, None, true, 7), Expiry::Days(7));
        assert_eq!(settled_expiry(Expiry::FromSettings, None, true, 0), Expiry::Never);
        // Until then, nothing expires, whatever it says.
        assert_eq!(settled_expiry(Expiry::FromSettings, None, false, 7), Expiry::Never);
        assert_eq!(settled_expiry(Expiry::Days(30), None, false, 0), Expiry::Never);
        // An explicit duration (the local API) wins over the setting.
        assert_eq!(settled_expiry(Expiry::Days(1), None, true, 7), Expiry::Days(1));
        assert_eq!(settled_expiry(Expiry::Never, None, true, 7), Expiry::Never);
        assert_eq!(settled_expiry(Expiry::Days(3), None, true, 7), Expiry::Never);
        // Exact keys never expire.
        assert_eq!(settled_expiry(Expiry::FromSettings, Some("docs/a.pdf"), true, 7), Expiry::Never);
        assert_eq!(settled_expiry(Expiry::Days(7), Some("docs/a.pdf"), true, 0), Expiry::Never);
        // ...unless they're inside a folder the rules empty.
        assert_eq!(settled_expiry(Expiry::FromSettings, Some("tmp/14d/a.pdf"), true, 7), Expiry::Days(14));
        assert_eq!(settled_expiry(Expiry::Never, Some("tmp/1d/a.pdf"), true, 0), Expiry::Days(1));
        assert_eq!(settled_expiry(Expiry::FromSettings, Some("tmp/14d/a.pdf"), false, 7), Expiry::Never);
    }
}
