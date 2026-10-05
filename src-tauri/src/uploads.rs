//! Coordinates the upload pipeline: expand folders -> zip a folder / process
//! a photo or clean its metadata -> hash it -> reuse an earlier upload of
//! the same bytes, or generate the object key and upload (in one request,
//! or in parts) -> resolve the link -> store history -> format output ->
//! copy to clipboard -> notify. Runs up to `MAX_CONCURRENT` jobs at once.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
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
use crate::output::{self, ContentHashes};
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
        }
    }

    /// Days until the upload is deleted, once `enqueue` has settled it.
    fn expire_after_days(&self) -> Option<u32> {
        match self.expiry {
            Expiry::Days(days) => Some(days),
            _ => None,
        }
    }

    pub(crate) fn remove_if_temporary(&self) {
        if self.temporary {
            let _ = std::fs::remove_file(&self.path);
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
    /// link copied instead.
    Succeeded { public_url: String, record_id: String, reused: bool },
    Failed { message: String },
    Cancelled,
}

impl JobState {
    fn is_finished_for_good(&self) -> bool {
        matches!(self, JobState::Succeeded { .. } | JobState::Cancelled)
    }
}

struct Job {
    id: String,
    input: UploadInput,
    destination: DestinationConfig,
    state: JobState,
    sender: watch::Sender<JobState>,
    task: Option<JoinHandle<()>>,
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
}

/// What the finished files of a folder (or of a drop spread over several
/// destinations) left to copy, formatted as their destinations copy them,
/// by position.
type GroupLinks = BTreeMap<usize, String>;

#[derive(Default)]
pub struct UploadManager {
    jobs: Mutex<Vec<Job>>,
    /// Folders uploaded with their structure, and drops spread over several
    /// destinations, that still have files going.
    open_groups: Mutex<HashMap<String, GroupLinks>>,
    /// The multipart upload (`multipart::Session` ID) each job is sending,
    /// or left unfinished, by job ID.
    job_sessions: Mutex<HashMap<String, String>>,
    /// Files waiting for their name ("Rename and upload").
    pending_names: Mutex<Vec<PendingName>>,
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
                filename: job.input.original_filename.clone(),
                destination_id: job.destination.id.clone(),
                destination_name: job.destination.name.clone(),
                state: job.state.clone(),
                source: job.input.watched.as_ref().map(|source| source.folder_name.clone()),
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
    if destination.is_some() || inputs.is_empty() {
        return enqueue_to(core, inputs, destination);
    }
    let destinations = core.destinations.all();
    let default = core.destinations.default_destination();
    let Some(default) = default.filter(|_| crate::routing::is_active(&destinations)) else {
        return enqueue_to(core, inputs, None);
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
    batches.into_iter().flat_map(|(destination, inputs)| enqueue_to(core, inputs, Some(destination))).collect()
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
fn enqueue_to(core: &SharedCore, inputs: Vec<UploadInput>, destination: Option<DestinationConfig>) -> Vec<Queued> {
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
    {
        let mut jobs = core.uploads.jobs.lock().unwrap();
        for (offset, mut input) in inputs.into_iter().enumerate() {
            input.expiry = settled_expiry(input.expiry, input.object_key.as_deref(), rules_active, delete_after_days);
            let (sender, receiver) = watch::channel(JobState::Waiting);
            let id = crate::util::new_id();
            queued.push(Queued { job_id: id.clone(), receiver });
            let job = Job { id, input, destination: destination.clone(), state: JobState::Waiting, sender, task: None };
            // A watched folder's files line up behind everything else, in
            // order; the user's own uploads go to the front.
            if job.input.watched.is_some() {
                jobs.push(job);
            } else {
                jobs.insert(offset, job);
            }
        }
    }
    drain(core);
    queued
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
pub async fn upload_clipboard(core: &SharedCore, rename: bool) -> bool {
    let inputs = tauri::async_runtime::spawn_blocking(crate::clipboard::read_inputs).await.unwrap_or_default();
    if inputs.is_empty() {
        return false;
    }
    if rename {
        ask_for_names(core, inputs, None);
    } else {
        enqueue(core, inputs, None);
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
            show_notification(&core, "Aktar", &t!("The clipboard has no file or image to upload."));
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
    }
    enqueue_to(core, vec![input], Some(destination)).into_iter().next().ok_or_else(|| t!("The upload could not be queued."))
}

/// For the shortcuts, the tray menu, and aktar:// links, whose handlers run
/// on the UI thread: uploads the clipboard in the background, and says so
/// when there's nothing on it to upload.
pub fn upload_clipboard_in_background(core: &SharedCore, rename: bool) {
    let core = core.clone();
    tauri::async_runtime::spawn(async move {
        if !upload_clipboard(&core, rename).await {
            show_notification(&core, "Aktar", &t!("The clipboard has no file or image to upload."));
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

pub fn retry(core: &SharedCore, job_id: &str) {
    {
        let mut jobs = core.uploads.jobs.lock().unwrap();
        if let Some(job) = jobs.iter_mut().find(|job| job.id == job_id) {
            if matches!(job.state, JobState::Failed { .. }) {
                job.state = JobState::Waiting;
                job.sender.send_replace(JobState::Waiting);
            }
        }
    }
    drain(core);
}

pub fn cancel(core: &SharedCore, job_id: &str) {
    let mut cancelled = None;
    let mut watched_cancelled = None;
    {
        let mut jobs = core.uploads.jobs.lock().unwrap();
        if let Some(index) = jobs.iter().position(|job| job.id == job_id) {
            let job = jobs.remove(index);
            if let Some(task) = job.task {
                task.abort();
            }
            job.input.remove_if_temporary();
            job.sender.send_replace(JobState::Cancelled);
            cancelled = job.input.group.clone();
            if let Some(source) = &job.input.watched {
                watched_cancelled = Some(source.clone());
            }
        }
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
        finish_group_if_done(core, &group);
    }
    drain(core);
}

/// Drops a job from the list, e.g. one started through the local API whose
/// staged file is already gone, so it can't be retried.
pub fn dismiss(core: &SharedCore, job_id: &str) {
    {
        let mut jobs = core.uploads.jobs.lock().unwrap();
        if let Some(index) = jobs.iter().position(|job| job.id == job_id) {
            let job = jobs.remove(index);
            if let Some(task) = job.task {
                task.abort();
            }
            job.input.remove_if_temporary();
        }
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
            let task_core = core.clone();
            let job_id = job.id.clone();
            let input = job.input.clone();
            let destination = job.destination.clone();
            job.task = Some(tauri::async_runtime::spawn(async move {
                match run(&task_core, &job_id, &input, &destination).await {
                    Ok(outcome) => finish(&task_core, &job_id, &input, &destination, outcome).await,
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

/// Uploads the file, or finds it already uploaded.
async fn run(core: &SharedCore, job_id: &str, input: &UploadInput, destination: &DestinationConfig) -> Result<Outcome, Failure> {
    let credentials = credentials::load(&destination.id).map_err(|error| error.to_string())?;
    let provider = S3Provider::new(destination.clone(), credentials);

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
    let reuse_links = core.settings.get().reuse_duplicate_links && !zipped;
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
    } else if reuse_links || (generated_key && output::uses_hashes(&destination.object_path_template)) {
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
        let exact_key = (!generated_key).then(|| object_key_for(input, destination, &filename, hashes.as_ref(), new_extension));
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

    let mut object_key = match &resumed {
        Some(session) => session.object_key.clone(),
        None => object_key_for(input, destination, &filename, hashes.as_ref(), new_extension),
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
    let content_type = output::content_type(&filename);
    let progress = Arc::new(Progress::default());
    // Only S3 and R2 are known to honor If-None-Match on uploads.
    let only_if_new = input.keep_existing && input.object_key.is_some() && destination.preset.supports_conditional_writes();
    let upload = async {
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
        let resuming = resumed.is_some();
        let session: Session = match resumed {
            Some(session) => session,
            None => {
                multipart::start(
                    &provider,
                    &core.upload_sessions,
                    &destination.id,
                    &destination.bucket,
                    &object_key,
                    &content_type,
                    file_size,
                    source.clone(),
                    hashes.as_ref().map(|hashes| hashes.sha256.clone()),
                    key_basis.clone(),
                )
                .await?
            }
        };
        core.uploads.job_sessions.lock().unwrap().insert(job_id.to_string(), session.id.clone());
        if resuming {
            set_resuming(core, job_id);
        }
        multipart::upload(&provider, &core.upload_sessions, session, &path, &content_type, resuming, progress.clone()).await?;
        core.uploads.job_sessions.lock().unwrap().remove(job_id);
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
                let link = link_for(&provider, destination, &object_key, &public_url).await;
                let thumbnail = match thumbnail.take() {
                    Some(task) => task.await.ok().flatten(),
                    None => None,
                };
                return Ok(Outcome {
                    result: UploadResult { object_key: object_key.clone(), public_url, byte_size: file_size as i64 },
                    link,
                    filename,
                    content_hash,
                    original_hash,
                    reused: None,
                    thumbnail,
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
async fn finish(core: &SharedCore, job_id: &str, input: &UploadInput, destination: &DestinationConfig, outcome: Outcome) {
    let Outcome { result, link, filename, content_hash, original_hash, reused, thumbnail, _scratch } = outcome;
    let was_reused = reused.is_some();
    let replaced = input.replacing.is_some();
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
    // A watched folder's own hooks run for its files; a reused link isn't
    // an upload at all.
    if input.watched.is_none() && !was_reused {
        let event = if replaced { crate::destination_hooks::Event::Replaced } else { crate::destination_hooks::Event::Uploaded };
        let payload =
            crate::destination_hooks::payload(event, destination, &input.path, &filename, result.byte_size, &result.object_key, &result.public_url);
        crate::destination_hooks::run(core, destination, payload);
    }
    drop(_scratch);
    input.remove_if_temporary();

    set_state(
        core,
        job_id,
        JobState::Succeeded {
            public_url: result.public_url.clone(),
            record_id: record.map(|record| record.id).unwrap_or_default(),
            reused: was_reused,
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
            link: format_link(core, destination, &link, &filename),
            filename: filename.clone(),
            destination_id: destination.id.clone(),
            reused: was_reused,
            byte_size: result.byte_size,
            content_hash: original_hash,
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
            links.insert(group.index, format_link(core, destination, &link, &filename));
        }
        open
    });
    if let (true, Some(group)) = (grouped, &input.group) {
        finish_group_if_done(core, group);
    } else {
        let settings = core.settings.get();
        crate::clipboard::copy(&format_link(core, destination, &link, &filename));
        if settings.show_notification {
            if replaced {
                show_notification(core, &t!("Replaced"), &t!("{0} keeps its link.", filename));
            } else if was_reused {
                show_notification(core, &filename, &t!("Already uploaded - copied the existing link"));
            } else {
                let body = match input.expire_after_days() {
                    Some(days) => format!("{filename}\n{}", deletes_in(days)),
                    None => filename.clone(),
                };
                show_notification(core, &t!("Uploaded"), &body);
            }
        }
        close_panel_if_wanted(core);
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

/// What's copied for `link`: as the destination says, or Settings > Output.
fn format_link(core: &SharedCore, destination: &DestinationConfig, link: &str, filename: &str) -> String {
    let settings = core.settings.get();
    let mode = destination.output_mode.unwrap_or(settings.output_mode);
    output::format(link, mode, filename, &settings.custom_template)
}

fn close_panel_if_wanted(core: &SharedCore) {
    if core.settings.get().close_panel_after_upload {
        crate::panel::hide(&core.app);
    }
}

/// Once none of a folder's files is waiting or uploading, copies the links
/// of the ones that made it, one per line and in folder order. Failed
/// files have their own notifications and can still be retried; a retry
/// then copies just its own link.
fn finish_group_if_done(core: &SharedCore, group: &UploadGroup) {
    let still_going = core.uploads.jobs.lock().unwrap().iter().any(|job| {
        job.input.group.as_ref().is_some_and(|other| other.id == group.id)
            && matches!(job.state, JobState::Waiting | JobState::Uploading { .. })
    });
    if still_going {
        return;
    }
    let Some(links) = core.uploads.open_groups.lock().unwrap().remove(&group.id) else { return };
    if links.is_empty() {
        return;
    }
    let copied: Vec<String> = links.into_values().collect();
    crate::clipboard::copy(&copied.join("\n"));
    if core.settings.get().show_notification {
        // A drop spread over several destinations has no folder name.
        let summary = if group.name.is_empty() {
            t!("{0} files", copied.len())
        } else if copied.len() == group.count {
            t!("{0} ({1} files)", group.name, group.count)
        } else {
            t!("{0} ({1} of {2} files)", group.name, copied.len(), group.count)
        };
        show_notification(core, &t!("Uploaded"), &summary);
    }
    close_panel_if_wanted(core);
}

fn fail(core: &SharedCore, job_id: &str, input: &UploadInput, failure: Failure) {
    let Failure { message, transient } = failure;
    set_state(core, job_id, JobState::Failed { message: message.clone() });
    // A watched folder says so itself, a batch at a time, and tries again
    // when it may work.
    match &input.watched {
        Some(source) => crate::watched::upload_failed(core, source, message, transient),
        None => show_notification(core, &t!("Upload failed"), &format!("{}: {message}", input.original_filename)),
    }
    if let Some(group) = &input.group {
        finish_group_if_done(core, group);
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
                let id = destination.id.clone();
                crate::bucket::delete_object(&S3Provider::new(destination, credentials), &record.object_key, &prefixes).await?;
                crate::thumbnails::remote::forget(core, &id, &record.object_key);
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
