//! Coordinates the upload pipeline: generate object key -> upload -> resolve
//! public URL -> store history -> format output -> copy to clipboard ->
//! notify. Runs up to `MAX_CONCURRENT` jobs at once.

use std::path::PathBuf;
use std::sync::Mutex;

use serde::Serialize;
use tauri::async_runtime::JoinHandle;
use tauri_plugin_notification::NotificationExt;
use tokio::sync::watch;

use crate::core::{events, SharedCore, UploadSucceeded};
use crate::credentials;
use crate::destinations::DestinationConfig;
use crate::history::NewRecord;
use crate::output;
use crate::storage::{S3Provider, UploadResult};
use crate::t;

const MAX_CONCURRENT: usize = 3;

#[derive(Debug, Clone)]
pub struct UploadInput {
    pub path: PathBuf,
    pub original_filename: String,
    /// Uploads to exactly this key instead of one generated from the
    /// destination's object path template (bucket browser, local API).
    pub object_key: Option<String>,
}

impl UploadInput {
    pub fn from_path(path: PathBuf) -> Self {
        let original_filename = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".into());
        Self { path, original_filename, object_key: None }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum JobState {
    Waiting,
    Uploading { progress: f64 },
    Succeeded { public_url: String, record_id: String },
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
}

#[derive(Default)]
pub struct UploadManager {
    jobs: Mutex<Vec<Job>>,
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
            })
            .collect()
    }
}

pub struct Queued {
    pub job_id: String,
    pub receiver: watch::Receiver<JobState>,
}

/// Queues `inputs` for `destination`, or the default destination. Each job
/// keeps the destination it was started for, so a retry or a change of
/// default later doesn't redirect it.
pub fn enqueue(core: &SharedCore, inputs: Vec<UploadInput>, destination: Option<DestinationConfig>) -> Vec<Queued> {
    let Some(destination) = destination.or_else(|| core.destinations.default_destination()) else {
        return Vec::new();
    };
    let mut queued = Vec::new();
    {
        let mut jobs = core.uploads.jobs.lock().unwrap();
        for (offset, input) in inputs.into_iter().enumerate() {
            let (sender, receiver) = watch::channel(JobState::Waiting);
            let id = crate::util::new_id();
            queued.push(Queued { job_id: id.clone(), receiver });
            jobs.insert(
                offset,
                Job { id, input, destination: destination.clone(), state: JobState::Waiting, sender, task: None },
            );
        }
    }
    drain(core);
    queued
}

/// Uploads whatever is on the clipboard. Returns false when there's
/// nothing uploadable on it.
pub fn upload_clipboard(core: &SharedCore) -> bool {
    let inputs = crate::clipboard::read_inputs();
    if inputs.is_empty() {
        return false;
    }
    enqueue(core, inputs, None);
    true
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
    {
        let mut jobs = core.uploads.jobs.lock().unwrap();
        if let Some(index) = jobs.iter().position(|job| job.id == job_id) {
            let job = jobs.remove(index);
            if let Some(task) = job.task {
                task.abort();
            }
            job.sender.send_replace(JobState::Cancelled);
        }
    }
    drain(core);
}

/// Drops a job from the list, e.g. one started through the local API whose
/// staged file is already gone, so it can't be retried.
pub fn dismiss(core: &SharedCore, job_id: &str) {
    core.uploads.jobs.lock().unwrap().retain(|job| job.id != job_id);
    core.notify(events::JOBS_CHANGED);
}

fn drain(core: &SharedCore) {
    {
        let mut jobs = core.uploads.jobs.lock().unwrap();
        let mut active = jobs.iter().filter(|job| matches!(job.state, JobState::Uploading { .. })).count();
        for job in jobs.iter_mut() {
            if active >= MAX_CONCURRENT {
                break;
            }
            if job.state != JobState::Waiting {
                continue;
            }
            active += 1;
            job.state = JobState::Uploading { progress: 0.0 };
            job.sender.send_replace(job.state.clone());
            let task_core = core.clone();
            let job_id = job.id.clone();
            let input = job.input.clone();
            let destination = job.destination.clone();
            job.task = Some(tauri::async_runtime::spawn(async move {
                match run(&input, &destination).await {
                    Ok(result) => finish(&task_core, &job_id, &input, &destination, result).await,
                    Err(message) => fail(&task_core, &job_id, &input, message),
                }
            }));
        }
    }
    core.notify(events::JOBS_CHANGED);
}

async fn run(input: &UploadInput, destination: &DestinationConfig) -> Result<UploadResult, String> {
    let credentials = credentials::load(&destination.id).map_err(|error| error.to_string())?;
    let provider = S3Provider::new(destination.clone(), credentials);
    let object_key = input
        .object_key
        .clone()
        .unwrap_or_else(|| output::generate_key(&destination.object_path_template, &input.original_filename));
    let content_type = output::content_type(&input.original_filename);
    provider
        .upload(&input.path, &object_key, &content_type)
        .await
        .map_err(|error| error.to_string())
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

async fn finish(core: &SharedCore, job_id: &str, input: &UploadInput, destination: &DestinationConfig, result: UploadResult) {
    let mime_type = output::content_type(&input.original_filename);
    let record = core.history.insert(NewRecord {
        local_filename: &input.original_filename,
        object_key: &result.object_key,
        public_url: &result.public_url,
        destination,
        mime_type: &mime_type,
        byte_size: result.byte_size,
    });
    // Before the job counts as finished: whoever started it (the local API)
    // may delete the file as soon as it has.
    if mime_type.starts_with("image/") {
        let source = input.path.clone();
        let target = core.history.thumbnail_path(&record.id);
        let _ = tauri::async_runtime::spawn_blocking(move || crate::thumbnails::store(&source, &target)).await;
    }

    set_state(
        core,
        job_id,
        JobState::Succeeded { public_url: result.public_url.clone(), record_id: record.id.clone() },
    );
    core.notify(events::HISTORY_CHANGED);

    let settings = core.settings.get();
    let copied = output::format(&result.public_url, settings.output_mode, &input.original_filename, &settings.custom_template);
    crate::clipboard::copy(&copied);
    if settings.show_notification {
        show_notification(core, &t!("Uploaded"), &input.original_filename);
    }
    core.emit(
        events::UPLOAD_SUCCEEDED,
        UploadSucceeded {
            destination_id: destination.id.clone(),
            object_key: result.object_key.clone(),
            byte_size: result.byte_size,
        },
    );
    if settings.close_panel_after_upload {
        crate::panel::hide(&core.app);
    }
    drain(core);
}

fn fail(core: &SharedCore, job_id: &str, input: &UploadInput, message: String) {
    set_state(core, job_id, JobState::Failed { message: message.clone() });
    show_notification(core, &t!("Upload failed"), &format!("{}: {message}", input.original_filename));
    drain(core);
}

pub fn show_notification(core: &SharedCore, title: &str, body: &str) {
    if let Err(error) = core.app.notification().builder().title(title).body(body).show() {
        log::warn!("Could not show a notification: {error}");
    }
}

/// Deletes the remote object and, on success, the history entry. If the
/// destination or its credentials are gone there's nothing left to delete
/// remotely, so the entry is cleaned up silently. A real failure from the
/// provider is returned so the caller can keep the record and offer a retry.
pub async fn delete_remote(core: &SharedCore, record_id: &str) -> Result<(), String> {
    let Some(record) = core.history.get(record_id) else { return Ok(()) };
    let destination = core
        .destinations
        .all()
        .into_iter()
        .find(|destination| destination.id == record.destination_id);
    let credentials = destination.as_ref().and_then(|destination| credentials::load(&destination.id).ok());
    if let (Some(destination), Some(credentials)) = (destination, credentials) {
        S3Provider::new(destination, credentials)
            .delete(&record.object_key)
            .await
            .map_err(|error| error.to_string())?;
    }
    core.history.delete(&record.id);
    core.notify(events::HISTORY_CHANGED);
    Ok(())
}
