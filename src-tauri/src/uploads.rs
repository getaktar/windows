//! Coordinates the upload pipeline: generate object key -> upload -> resolve
//! public URL -> store history -> format output -> copy to clipboard ->
//! notify. Runs up to `MAX_CONCURRENT` jobs at once.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tauri::async_runtime::JoinHandle;
use tauri_plugin_notification::NotificationExt;
use tokio::sync::watch;

use crate::core::{events, SharedCore, UploadSucceeded};
use crate::credentials;
use crate::destinations::DestinationConfig;
use crate::history::NewRecord;
use crate::output;
use crate::storage::{Progress, S3Provider, StorageError, UploadResult};
use crate::t;
use crate::windows::AppWindow;

const MAX_CONCURRENT: usize = 3;
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
        Self { path, original_filename, object_key: None, temporary: false, expiry: Expiry::FromSettings }
    }

    /// Days until the upload is deleted, once `enqueue` has settled it.
    fn expire_after_days(&self) -> Option<u32> {
        match self.expiry {
            Expiry::Days(days) => Some(days),
            _ => None,
        }
    }

    fn remove_if_temporary(&self) {
        if self.temporary {
            let _ = std::fs::remove_file(&self.path);
        }
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
    let delete_after_days = core.settings.get().delete_after_days;
    let rules_active = crate::expiry::is_active(core, &destination.id);
    let mut queued = Vec::new();
    {
        let mut jobs = core.uploads.jobs.lock().unwrap();
        for (offset, mut input) in inputs.into_iter().enumerate() {
            input.expiry = settled_expiry(input.expiry, input.object_key.as_deref(), rules_active, delete_after_days);
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

/// Uploads whatever is on the clipboard. Returns false when there's
/// nothing uploadable on it. Reading and PNG-encoding a large screenshot
/// takes a moment, so it's done off the async runtime's workers.
pub async fn upload_clipboard(core: &SharedCore) -> bool {
    let inputs = tauri::async_runtime::spawn_blocking(crate::clipboard::read_inputs).await.unwrap_or_default();
    if inputs.is_empty() {
        return false;
    }
    enqueue(core, inputs, None);
    true
}

/// For the shortcut, the tray menu, and aktar:// links, whose handlers run
/// on the UI thread: uploads the clipboard in the background, and says so
/// when there's nothing on it to upload.
pub fn upload_clipboard_in_background(core: &SharedCore) {
    let core = core.clone();
    tauri::async_runtime::spawn(async move {
        if !upload_clipboard(&core).await {
            show_notification(&core, "Aktar", &t!("The clipboard has no file or image to upload."));
        }
    });
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
            job.input.remove_if_temporary();
            job.sender.send_replace(JobState::Cancelled);
        }
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
                match run(&task_core, &job_id, &input, &destination).await {
                    Ok(result) => finish(&task_core, &job_id, &input, &destination, result).await,
                    Err(message) => fail(&task_core, &job_id, &input, message),
                }
            }));
        }
    }
    core.notify(events::JOBS_CHANGED);
}

async fn run(core: &SharedCore, job_id: &str, input: &UploadInput, destination: &DestinationConfig) -> Result<UploadResult, String> {
    let credentials = credentials::load(&destination.id).map_err(|error| error.to_string())?;
    let provider = S3Provider::new(destination.clone(), credentials);
    let object_key = input.object_key.clone().unwrap_or_else(|| {
        let key = output::generate_key(&destination.object_path_template, &input.original_filename);
        match input.expire_after_days() {
            Some(days) => crate::expiry::expiring_key(&key, days),
            None => key,
        }
    });
    let content_type = output::content_type(&input.original_filename);

    let progress = Arc::new(Progress::default());
    let upload = provider.upload(&input.path, &object_key, &content_type, progress.clone());
    tokio::pin!(upload);
    let mut ticker = tokio::time::interval(Duration::from_millis(250));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut reported = 0.0;
    loop {
        tokio::select! {
            result = &mut upload => return result.map_err(|error| error.to_string()),
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
                    return Err(StorageError::Network(t!("The connection stopped responding.")).to_string());
                }
            }
        }
    }
}

fn report_progress(core: &SharedCore, job_id: &str, fraction: f64) {
    {
        let mut jobs = core.uploads.jobs.lock().unwrap();
        let Some(job) = jobs.iter_mut().find(|job| job.id == job_id) else { return };
        if !matches!(job.state, JobState::Uploading { .. }) {
            return;
        }
        job.state = JobState::Uploading { progress: fraction };
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

async fn finish(core: &SharedCore, job_id: &str, input: &UploadInput, destination: &DestinationConfig, result: UploadResult) {
    let mime_type = output::content_type(&input.original_filename);
    let record = core.history.insert(NewRecord {
        local_filename: &input.original_filename,
        object_key: &result.object_key,
        public_url: &result.public_url,
        destination,
        mime_type: &mime_type,
        byte_size: result.byte_size,
        expire_after_days: input.expire_after_days(),
    });
    // Before the job counts as finished: whoever started it (the local API)
    // may delete the file as soon as it has.
    if mime_type.starts_with("image/") {
        let source = input.path.clone();
        let target = core.history.thumbnail_path(&record.id);
        let _ = tauri::async_runtime::spawn_blocking(move || crate::thumbnails::store(&source, &target)).await;
    }
    input.remove_if_temporary();

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
        let body = match input.expire_after_days() {
            Some(days) => format!("{}\n{}", input.original_filename, deletes_in(days)),
            None => input.original_filename.clone(),
        };
        show_notification(core, &t!("Uploaded"), &body);
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

#[cfg(test)]
mod tests {
    use super::*;

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
