//! From file to link, fast (the Mac app's docs/capture-and-instant-links.md):
//! the link copied before the bytes are sent, uploads that wait for the
//! network and survive a restart (the outbox), pasting the link where the
//! user types, and Undo. The upload pipeline itself (`uploads`) calls in
//! here at each step.

use std::collections::HashSet;
use std::time::Duration;

use crate::core::{events, SharedCore};
use crate::destinations::DestinationConfig;
use crate::outbox::{EarlyLink, EarlyShortLink, OutboxEntry};
use crate::storage::S3Provider;
use crate::t;
use crate::uploads::{self, JobState, UploadInput};
use crate::win32::{self, ClipboardSnapshot, Snapshot, ToastButton};

/// How long Undo is offered after a link is copied or an upload ends.
pub const UNDO_WINDOW_MS: i64 = 10_000;
/// A quick upload only says "Uploaded"; one that takes longer says its
/// link is copied and the file still uploading.
const UPLOADING_NOTICE_DELAY: Duration = Duration::from_secs(2);
/// How long after the shortcut (or the end of a capture) the link is still
/// pasted into the app that was in front.
pub const PASTE_WINDOW_MS: i64 = 30_000;
/// Waits before trying an upload that waits for the network again, while
/// the network looks up: then every 300 seconds.
pub const OFFLINE_DELAYS_SECONDS: [i64; 5] = [15, 30, 60, 120, 300];
/// Older actions are forgotten once there are more than this many.
const MAX_ACTIONS: usize = 100;

/// The wait before the next try, after `tries` failed ones.
pub fn offline_delay_seconds(tries: u32) -> i64 {
    let index = (tries.max(1) as usize - 1).min(OFFLINE_DELAYS_SECONDS.len() - 1);
    OFFLINE_DELAYS_SECONDS[index]
}

/// Where an upload's link is pasted: the window that was in front when the
/// shortcut was pressed, if it still is, and only for a little while (a
/// link that turns up much later stays copied).
#[derive(Debug, Clone, Copy)]
pub struct PasteTarget {
    pub window: isize,
    pub deadline: i64,
}

impl PasteTarget {
    /// The window in front now (never one of Aktar's), for `millis`.
    pub fn foreground(millis: i64) -> Option<Self> {
        win32::foreground_window().map(|window| Self { window, deadline: crate::util::now_millis() + millis })
    }

    fn is_still_valid(&self) -> bool {
        crate::util::now_millis() < self.deadline && win32::is_foreground(self.window)
    }
}

/// How an upload was started, beyond its files: where to paste its link,
/// and (for a capture) the clipboard as it was before.
#[derive(Default)]
pub struct Origin {
    pub paste: Option<PasteTarget>,
    pub snapshot: Option<Snapshot>,
}

/// One upload the user started: a drop, a paste, a capture, an Explorer
/// menu, the local API (not a watched folder's files). Undo takes it back
/// as a whole, and its link is pasted where asked.
pub struct UploadAction {
    pub id: String,
    created_at: i64,
    paste: Option<PasteTarget>,
    pasted: bool,
    /// The clipboard before the action first copied a link; None when it
    /// couldn't be kept (too big, or marked private), so Undo empties it.
    snapshot: Option<ClipboardSnapshot>,
    took_snapshot: bool,
    /// The clipboard's sequence number right after the action's last copy,
    /// and what that copy was: Undo only touches a clipboard nobody copied
    /// anything to since.
    last_sequence: Option<u32>,
    last_copied_text: Option<String>,
    /// Undo is offered until then, and while a file of it still uploads.
    undo_deadline: i64,
    pub job_ids: Vec<String>,
    /// The history entries it made (not a reused or replaced one).
    pub record_ids: Vec<String>,
    /// Files it took nothing up for (an earlier upload's link was copied).
    reused_names: Vec<String>,
    notification_tags: HashSet<String>,
    undoing: bool,
}

/// Starts an action and returns its ID.
pub fn new_action(core: &SharedCore, origin: Origin) -> String {
    let id = crate::util::new_id();
    let (snapshot, took_snapshot) = match origin.snapshot {
        Some(Snapshot::Kept(snapshot)) => (Some(snapshot), true),
        Some(_) => (None, true),
        None => (None, false),
    };
    let action = UploadAction {
        id: id.clone(),
        created_at: crate::util::now_millis(),
        paste: origin.paste,
        pasted: false,
        snapshot,
        took_snapshot,
        last_sequence: None,
        last_copied_text: None,
        undo_deadline: 0,
        job_ids: Vec::new(),
        record_ids: Vec::new(),
        reused_names: Vec::new(),
        notification_tags: HashSet::new(),
        undoing: false,
    };
    let pending = pending_actions(core);
    let mut actions = core.uploads.actions.lock().unwrap();
    actions.insert(id.clone(), action);
    // Keeps the latest ones that are over (a notification's Undo can still
    // find them), and every one still going.
    if actions.len() > MAX_ACTIONS {
        let mut over: Vec<(i64, String)> =
            actions.values().filter(|action| !pending.contains(&action.id) && action.id != id).map(|action| (action.created_at, action.id.clone())).collect();
        over.sort();
        let excess = actions.len() - MAX_ACTIONS;
        for (_, old) in over.into_iter().take(excess) {
            actions.remove(&old);
        }
    }
    id
}

/// The actions that still have a file waiting, uploading or failed.
fn pending_actions(core: &SharedCore) -> HashSet<String> {
    core.uploads.jobs.lock().unwrap().iter().filter_map(|job| job.action_id.clone()).collect()
}

pub fn attach_jobs(core: &SharedCore, action_id: &str, job_ids: Vec<String>) {
    if let Some(action) = core.uploads.actions.lock().unwrap().get_mut(action_id) {
        action.job_ids.extend(job_ids);
    }
}

/// An action with no files after all (nowhere to upload to) is dropped.
pub fn drop_if_empty(core: &SharedCore, action_id: &str) {
    let mut actions = core.uploads.actions.lock().unwrap();
    if actions.get(action_id).is_some_and(|action| action.job_ids.is_empty()) {
        actions.remove(action_id);
    }
}

pub fn note_record(core: &SharedCore, action_id: &str, record_id: &str) {
    if let Some(action) = core.uploads.actions.lock().unwrap().get_mut(action_id) {
        action.record_ids.push(record_id.to_string());
    }
}

pub fn note_reused(core: &SharedCore, action_id: &str, filename: &str) {
    if let Some(action) = core.uploads.actions.lock().unwrap().get_mut(action_id) {
        action.reused_names.push(filename.to_string());
    }
}

pub fn is_undoing(core: &SharedCore, action_id: &str) -> bool {
    core.uploads.actions.lock().unwrap().get(action_id).is_some_and(|action| action.undoing)
}

// MARK: - Copying

/// Copies an upload's link. For an action, what was on the clipboard before
/// is kept first (for Undo), and the link is pasted if asked to.
pub fn copy_link(core: &SharedCore, action_id: Option<&str>, text: &str) {
    let Some(action_id) = action_id else {
        crate::clipboard::copy(text);
        return;
    };
    let needs_snapshot = match core.uploads.actions.lock().unwrap().get(action_id) {
        Some(action) => !action.took_snapshot,
        None => {
            crate::clipboard::copy(text);
            return;
        }
    };
    // Read off the lock: a big clipboard takes a moment.
    let snapshot = needs_snapshot.then(win32::take_snapshot);
    crate::clipboard::copy(text);
    let sequence = win32::clipboard_sequence();
    let paste = {
        let mut actions = core.uploads.actions.lock().unwrap();
        let Some(action) = actions.get_mut(action_id) else { return };
        if let Some(snapshot) = snapshot {
            action.took_snapshot = true;
            action.snapshot = match snapshot {
                Snapshot::Kept(snapshot) => Some(snapshot),
                _ => None,
            };
        }
        action.last_sequence = Some(sequence);
        action.last_copied_text = Some(text.to_string());
        let paste = if action.pasted { None } else { action.paste };
        action.pasted = true;
        paste
    };
    extend_undo(core, action_id);
    if let Some(target) = paste {
        paste_soon(target);
    }
}

/// Presses Ctrl+V once the keys of the shortcut are let go (at most a
/// second and a half), if the window is still in front.
fn paste_soon(target: PasteTarget) {
    std::thread::spawn(move || {
        for _ in 0..30 {
            if !win32::modifiers_down() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if target.is_still_valid() && !win32::send_paste() {
            log::warn!("Could not paste the link");
        }
    });
}

/// Whether the clipboard still holds `text` as the action copied it.
pub fn clipboard_still_holds(core: &SharedCore, action_id: Option<&str>, text: &str) -> bool {
    if let Some(action) = action_id.and_then(|id| core.uploads.actions.lock().unwrap().get(id).map(|action| (action.last_sequence, action.last_copied_text.clone()))) {
        return action.0 == Some(win32::clipboard_sequence()) && action.1.as_deref() == Some(text);
    }
    crate::clipboard::text().as_deref() == Some(text)
}

/// Puts back what was on the clipboard before the action copied its link,
/// if the clipboard still holds the action's last copy.
fn restore_clipboard(core: &SharedCore, action_id: &str) {
    let (sequence, snapshot) = {
        let mut actions = core.uploads.actions.lock().unwrap();
        let Some(action) = actions.get_mut(action_id) else { return };
        let Some(sequence) = action.last_sequence.take() else { return };
        (sequence, action.snapshot.clone())
    };
    if win32::clipboard_sequence() != sequence {
        return;
    }
    let done = match &snapshot {
        Some(snapshot) => win32::restore(snapshot),
        None => win32::clear_clipboard(),
    };
    if !done {
        log::warn!("Could not put the clipboard back");
    }
}

// MARK: - Instant link

/// Whether a job copies its link before its bytes are sent: a file (or a
/// folder as a ZIP) the user uploaded, to a key Aktar makes. Not a watched
/// folder's file, a file of a folder or of a drop to several destinations
/// (their links are copied together), a replace (its link doesn't change),
/// an exact key, or an upload someone waits for.
pub fn can_copy_early(input: &UploadInput, has_action: bool) -> bool {
    has_action
        && input.watched.is_none()
        && input.group.is_none()
        && input.replacing.is_none()
        && input.object_key.is_none()
        && input.folder_key.is_none()
        && !input.waited
}

/// Copies the link to `object_key` (shortened, as "Copy as" says) before the
/// bytes go, and keeps the upload in the outbox until they're there.
pub async fn copy_early_link(
    core: &SharedCore,
    job_id: &str,
    input: &UploadInput,
    destination: &DestinationConfig,
    provider: &S3Provider,
    object_key: &str,
    filename: &str,
) -> EarlyLink {
    use crate::short_links::{rules::Shortening, Attempt};
    let public_url = crate::output::resolve_public_url(&destination.public_base_url, object_key);
    let now = crate::util::now_millis();
    // Signing happens on this PC, so it works offline too.
    let mut link = public_url.clone();
    let mut temporary_expires_at = None;
    if let Some(seconds) = destination.temporary_link {
        if let Ok(signed) = provider.temporary_url(object_key, seconds).await {
            link = signed;
            temporary_expires_at = Some(now + seconds as i64 * 1000);
        }
    }
    let shortening = if input.short_link == Some(false) {
        Attempt::Skipped
    } else {
        let shortening = Shortening {
            temporary_expires_at,
            upload_expires_at: input.expire_after_days().map(|days| now + days as i64 * 86_400_000),
            is_folder_file: false,
            explicit: input.short_link == Some(true),
        };
        crate::short_links::shorten(destination, &link, shortening).await
    };
    let mut early = EarlyLink { object_key: object_key.to_string(), link: link.clone(), temporary_expires_at, short_link: None, short_link_error: None, copied_text: String::new() };
    let mut short_url = None;
    match shortening {
        Attempt::Created(pending) => {
            short_url = Some(pending.created.short_url.clone());
            early.short_link = Some(EarlyShortLink {
                short_url: pending.created.short_url,
                provider_id: pending.created.provider_id,
                provider: pending.provider,
                provider_name: pending.provider_name,
                domain: pending.domain,
                target_url: pending.target_url,
                expires_at: pending.expires_at,
            });
        }
        Attempt::Failed(message) => early.short_link_error = Some(message),
        Attempt::Skipped => {}
    }
    early.copied_text = uploads::format_link(core, destination, &link, short_url.as_deref(), filename);
    let action_id = {
        let mut jobs = core.uploads.jobs.lock().unwrap();
        match jobs.iter_mut().find(|job| job.id == job_id) {
            Some(job) => {
                job.early_link = Some(early.clone());
                job.action_id.clone()
            }
            // Cancelled while the short link was being made.
            None => {
                drop(jobs);
                delete_unstored_short_link(&early, destination);
                return early;
            }
        }
    };
    copy_link(core, action_id.as_deref(), &early.copied_text);
    core.notify(events::JOBS_CHANGED);
    keep_in_outbox(core, job_id);
    schedule_uploading_notice(core, job_id, action_id, filename);
    uploads::close_panel_if_wanted(core);
    early
}

/// "Link copied, still uploading" for an upload that takes a while.
fn schedule_uploading_notice(core: &SharedCore, job_id: &str, action_id: Option<String>, filename: &str) {
    if !core.settings.get().show_notification {
        return;
    }
    let (core, job_id, filename) = (core.clone(), job_id.to_string(), filename.to_string());
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(UPLOADING_NOTICE_DELAY).await;
        let still_uploading = core
            .uploads
            .jobs
            .lock()
            .unwrap()
            .iter()
            .any(|job| job.id == job_id && job.early_link.is_some() && matches!(job.state, JobState::Uploading { .. }));
        if still_uploading {
            let body = t!("Still uploading. The link works once the upload is done.");
            notify(&core, &job_id, action_id.as_deref(), None, &t!("Link copied"), &[&filename, &body]);
        }
    });
}

/// A job's early link and outbox entry, when it stops without finishing.
pub struct Stopped {
    pub job_id: String,
    pub action_id: Option<String>,
    pub early_link: Option<EarlyLink>,
    pub in_outbox: bool,
    pub destination: DestinationConfig,
}

/// A job whose link was copied stops without finishing (Cancel, Undo,
/// Remove): its short link is deleted, it leaves the outbox, and the
/// clipboard gets back what was there before, if it still holds the link.
pub fn withdraw_early_link(core: &SharedCore, stopped: &Stopped) {
    if stopped.in_outbox {
        core.outbox.remove(&stopped.job_id);
    }
    let Some(early) = &stopped.early_link else { return };
    delete_unstored_short_link(early, &stopped.destination);
    remove_notification(core, &stopped.job_id);
    if let Some(action_id) = &stopped.action_id {
        if clipboard_still_holds(core, Some(action_id), &early.copied_text) {
            restore_clipboard(core, action_id);
        }
    }
}

/// Deletes the short link made for an early link that's no longer used. One
/// that can't be deleted (the provider can't, or isn't the one set up any
/// more) is left; it points at nothing.
pub fn delete_unstored_short_link(early: &EarlyLink, destination: &DestinationConfig) {
    let Some(short) = early.short_link.clone() else { return };
    let Some(engine) = crate::short_links::engine_for(destination) else { return };
    let snapshot = crate::short_links::rules::Snapshot {
        id: crate::util::new_id(),
        provider: short.provider,
        provider_id: short.provider_id,
        domain: short.domain,
        status: crate::short_links::rules::Status::Active,
        created_at: crate::util::now_millis(),
        expires_at: short.expires_at,
    };
    tauri::async_runtime::spawn(async move {
        if let Err(error) = crate::short_links::rules::delete(&snapshot, Some(&engine)).await {
            log::debug!("Could not delete an unused short link: {error}");
        }
    });
}

/// A file cancelled or undone while (or after) its bytes went: what it
/// wrote is deleted, once any request still on its way had a moment to
/// land. Only an object as big as the file and written since its first try
/// is taken; anything else at the key is someone else's.
pub fn take_back(destination: DestinationConfig, key: String, size: u64, first_attempt_at: i64) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(3)).await;
        let Ok(credentials) = crate::credentials::load(&destination.id) else { return };
        let provider = S3Provider::new(destination, credentials);
        let Ok(Some(object)) = provider.object_info(&key).await else { return };
        let own = object.size == size as i64 && object.last_modified.is_some_and(|modified| modified >= first_attempt_at - 120_000);
        if own {
            if let Err(error) = provider.delete(&key).await {
                log::warn!("Could not take back {key}: {error}");
            }
        }
    });
}

// MARK: - Outbox

/// Keeps the job's file and early link so the upload goes on after a
/// restart. Copying a file takes a moment, so it's done off the async
/// runtime's workers.
pub fn keep_in_outbox(core: &SharedCore, job_id: &str) {
    let Some((entry, source)) = outbox_entry(core, job_id, None) else { return };
    if core.outbox.contains(job_id) {
        let kept = core.outbox.update(&entry);
        set_in_outbox(core, job_id, kept);
        return;
    }
    let core = core.clone();
    let job_id = job_id.to_string();
    tauri::async_runtime::spawn(async move {
        let saving = core.clone();
        let saved_entry = entry.clone();
        let saved = tauri::async_runtime::spawn_blocking(move || saving.outbox.save(&saved_entry, &source)).await.unwrap_or(false);
        // It may have finished (or stopped) while the copy was made.
        let still_going = set_in_outbox(&core, &job_id, saved);
        if saved && !still_going {
            core.outbox.remove(&job_id);
        }
    });
}

/// Marks the job as kept (or not); false when it's no longer in the list.
fn set_in_outbox(core: &SharedCore, job_id: &str, kept: bool) -> bool {
    let mut jobs = core.uploads.jobs.lock().unwrap();
    match jobs.iter_mut().find(|job| job.id == job_id && !matches!(job.state, JobState::Succeeded { .. } | JobState::Cancelled)) {
        Some(job) => {
            job.in_outbox = kept;
            true
        }
        None => false,
    }
}

/// The outbox entry for a job with an early link, and the file it keeps.
pub fn outbox_entry(core: &SharedCore, job_id: &str, failure: Option<String>) -> Option<(OutboxEntry, std::path::PathBuf)> {
    let jobs = core.uploads.jobs.lock().unwrap();
    let job = jobs.iter().find(|job| job.id == job_id)?;
    let early_link = job.early_link.clone()?;
    let source = job.downloaded.as_ref().map_or_else(|| job.input.path.clone(), |downloaded| downloaded.path.clone());
    let stored_name = source.file_name()?.to_string_lossy().into_owned();
    let entry = OutboxEntry {
        id: job.id.clone(),
        destination_id: job.destination.id.clone(),
        original_filename: job.display_name(),
        stored_name,
        expiry_days: job.input.expire_after_days(),
        short_link_override: job.input.short_link,
        early_link,
        first_attempt_at: job.first_attempt_at.unwrap_or_else(crate::util::now_millis),
        failure,
    };
    Some((entry, source))
}

/// A kept upload failed for good: it waits for Retry or Cancel, also after
/// a restart.
pub fn note_outbox_failure(core: &SharedCore, job_id: &str, failure: Option<String>) {
    let in_outbox = core.uploads.jobs.lock().unwrap().iter().any(|job| job.id == job_id && job.in_outbox);
    if !in_outbox {
        return;
    }
    if let Some((entry, _)) = outbox_entry(core, job_id, failure) {
        core.outbox.update(&entry);
    }
}

// MARK: - Offline queue

/// A failure for want of a connection: the job waits, and is tried again as
/// soon as the network is back, and while it looks up after a growing wait.
pub fn wait_for_network(core: &SharedCore, job_id: &str, message: String) {
    let first = {
        let mut jobs = core.uploads.jobs.lock().unwrap();
        let Some(job) = jobs.iter_mut().find(|job| job.id == job_id) else { return };
        job.offline_retries += 1;
        job.next_retry_at = Some(crate::util::now_millis() + offline_delay_seconds(job.offline_retries) * 1000);
        job.state = JobState::Offline { message: message.clone() };
        job.sender.send_replace(job.state.clone());
        job.task = None;
        (job.offline_retries == 1).then(|| (job.display_name(), job.early_link.is_some(), job.action_id.clone()))
    };
    core.notify(events::JOBS_CHANGED);
    if let Some((filename, link_copied, action_id)) = first {
        let body = if link_copied {
            t!("The link is copied. The file uploads when you’re back online, and the link works from then on.")
        } else {
            t!("The file uploads when you’re back online.")
        };
        notify(core, job_id, action_id.as_deref(), None, &t!("Waiting for a connection"), &[&filename, &body]);
    }
    schedule_offline_retry(core);
}

/// Wakes up for the next upload waiting for the network whose wait is over.
pub fn schedule_offline_retry(core: &SharedCore) {
    let due = core
        .uploads
        .jobs
        .lock()
        .unwrap()
        .iter()
        .filter(|job| matches!(job.state, JobState::Offline { .. }))
        .map(|job| job.next_retry_at.unwrap_or_default())
        .min();
    let mut timer = core.uploads.offline_timer.lock().unwrap();
    if let Some(previous) = timer.take() {
        previous.abort();
    }
    let Some(due) = due else { return };
    let wait = (due - crate::util::now_millis()).max(1_000) as u64;
    let task_core = core.clone();
    *timer = Some(tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(wait)).await;
        retry_offline(&task_core, true);
    }));
}

/// Starts the uploads waiting for the network: all of them when it just came
/// back, otherwise those whose wait is over. While it's down they wait for
/// it to come back.
pub fn retry_offline(core: &SharedCore, only_due: bool) {
    if !win32::is_online() {
        if let Some(timer) = core.uploads.offline_timer.lock().unwrap().take() {
            timer.abort();
        }
        return;
    }
    let now = crate::util::now_millis();
    let due: Vec<String> = core
        .uploads
        .jobs
        .lock()
        .unwrap()
        .iter()
        .filter(|job| matches!(job.state, JobState::Offline { .. }) && (!only_due || job.next_retry_at.is_none_or(|at| at <= now)))
        .map(|job| job.id.clone())
        .collect();
    for job_id in due {
        uploads::retry(core, &job_id);
    }
    schedule_offline_retry(core);
}

/// Follows the network for as long as the app runs.
pub fn watch_network(core: &SharedCore) {
    let core = core.clone();
    win32::watch_network(move || {
        let core = core.clone();
        // Windows calls this on a thread of its own; the network may take a
        // moment to carry traffic once it's back.
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_secs(2)).await;
            retry_offline(&core, false);
        });
    });
}

// MARK: - Notifications

/// The app identity notifications go out under: the package's in the
/// Microsoft Store build, the app's identifier when installed, PowerShell's
/// in a dev build (as the short links' Retry notification does).
fn toast_app_id(core: &SharedCore) -> Result<String, String> {
    #[cfg(windows)]
    if crate::package::is_packaged() {
        return windows::ApplicationModel::AppInfo::Current()
            .and_then(|info| info.AppUserModelId())
            .map(|id| id.to_string())
            .map_err(|error| error.to_string());
    }
    if tauri::is_dev() {
        return Ok("{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\\WindowsPowerShell\\v1.0\\powershell.exe".to_string());
    }
    Ok(core.app.config().identifier.clone())
}

fn tag(job_or_group_id: &str) -> String {
    // Tags may be 64 characters; IDs are 36.
    format!("u-{job_or_group_id}")
}

/// A notification about an upload, replacing the last one about it (same
/// `id`), with Undo while its action can still be undone. `upload_id` is
/// the history entry Undo deletes once the action is long gone.
pub fn notify(core: &SharedCore, id: &str, action_id: Option<&str>, upload_id: Option<&str>, title: &str, lines: &[&str]) {
    let lines: Vec<&str> = lines.iter().copied().filter(|line| !line.is_empty()).collect();
    let lines = lines.as_slice();
    let tag = tag(id);
    let shown = toast_app_id(core).and_then(|app_id| {
        let undo = action_id.map(|action_id| {
            let (core, action_id, upload_id) = (core.clone(), action_id.to_string(), upload_id.map(str::to_string));
            Box::new(move |argument: String| {
                if argument == "undo" {
                    let (core, action_id, upload_id) = (core.clone(), action_id.clone(), upload_id.clone());
                    tauri::async_runtime::spawn(async move { undo(&core, &action_id, upload_id).await });
                }
            }) as Box<dyn Fn(String) + Send + Sync>
        });
        let undo_label = t!("Undo");
        let button = undo.as_ref().map(|_| ToastButton { label: &undo_label, argument: "undo" });
        win32::show_toast(&app_id, &tag, title, lines, button, undo)
    });
    match shown {
        Ok(()) => {
            if let Some(action_id) = action_id {
                if let Some(action) = core.uploads.actions.lock().unwrap().get_mut(action_id) {
                    action.notification_tags.insert(tag);
                }
            }
        }
        Err(error) => {
            if cfg!(windows) {
                log::warn!("Could not show a notification with Undo: {error}");
            }
            uploads::show_notification(core, title, &lines.join("\n"));
        }
    }
}

pub fn remove_notification(core: &SharedCore, id: &str) {
    if let Ok(app_id) = toast_app_id(core) {
        win32::remove_toast(&app_id, &tag(id));
    }
}

fn remove_notification_tags(core: &SharedCore, tags: &HashSet<String>) {
    let Ok(app_id) = toast_app_id(core) else { return };
    for tag in tags {
        win32::remove_toast(&app_id, tag);
    }
}

// MARK: - Undo

/// Offers Undo for the action for `UNDO_WINDOW_MS` from now, and as long as
/// one of its files still uploads.
pub fn extend_undo(core: &SharedCore, action_id: &str) {
    {
        let mut actions = core.uploads.actions.lock().unwrap();
        let Some(action) = actions.get_mut(action_id) else { return };
        if action.undoing {
            return;
        }
        action.undo_deadline = crate::util::now_millis() + UNDO_WINDOW_MS;
    }
    let changed = core.uploads.undoable.lock().unwrap().replace(action_id.to_string()).as_deref() != Some(action_id);
    let mut timer = core.uploads.undo_timer.lock().unwrap();
    if let Some(previous) = timer.take() {
        previous.abort();
    }
    let (task_core, watched) = (core.clone(), action_id.to_string());
    *timer = Some(tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let core = &task_core;
            if core.uploads.undoable.lock().unwrap().as_deref() != Some(watched.as_str()) {
                return;
            }
            let pending = core.uploads.jobs.lock().unwrap().iter().any(|job| job.action_id.as_deref() == Some(watched.as_str()));
            let deadline = core.uploads.actions.lock().unwrap().get(&watched).map_or(0, |action| action.undo_deadline);
            if !pending && crate::util::now_millis() >= deadline {
                let mut undoable = core.uploads.undoable.lock().unwrap();
                if undoable.as_deref() == Some(watched.as_str()) {
                    *undoable = None;
                }
                drop(undoable);
                core.notify(events::UNDO_CHANGED);
                return;
            }
        }
    }));
    drop(timer);
    if changed {
        core.notify(events::UNDO_CHANGED);
    }
}

/// The history entries Undo would take back right now (the panel's Undo
/// buttons).
pub fn undoable_records(core: &SharedCore) -> Vec<String> {
    let Some(id) = core.uploads.undoable.lock().unwrap().clone() else { return Vec::new() };
    core.uploads.actions.lock().unwrap().get(&id).map(|action| action.record_ids.clone()).unwrap_or_default()
}

/// The Undo shortcut: takes back the latest upload while Undo is offered
/// for it. False when there's nothing to undo.
pub fn undo_latest(core: &SharedCore) -> bool {
    let Some(id) = core.uploads.undoable.lock().unwrap().clone() else { return false };
    let core = core.clone();
    tauri::async_runtime::spawn(async move { undo(&core, &id, None).await });
    true
}

/// The panel's Undo next to an upload: its whole action.
pub async fn undo_record(core: &SharedCore, record_id: &str) {
    let action_id = core
        .uploads
        .actions
        .lock()
        .unwrap()
        .values()
        .find(|action| action.record_ids.iter().any(|id| id == record_id))
        .map(|action| action.id.clone());
    if let Some(action_id) = action_id {
        undo(core, &action_id, None).await;
    }
}

/// Takes an upload back: files still going stop (and their short links are
/// deleted), finished ones are deleted from the bucket with their short
/// links and history entries, and the clipboard gets back what was on it
/// before, if it still holds the link Aktar copied. A link reused from an
/// earlier upload only leaves the clipboard; that file stays. `upload_id`
/// is for a notification whose action is long gone: that upload alone is
/// deleted.
pub async fn undo(core: &SharedCore, action_id: &str, upload_id: Option<String>) {
    let known = {
        let mut actions = core.uploads.actions.lock().unwrap();
        match actions.get_mut(action_id) {
            Some(action) if action.undoing => return,
            Some(action) => {
                action.undoing = true;
                true
            }
            None => false,
        }
    };
    if !known {
        let Some(upload_id) = upload_id else { return };
        let Some(record) = core.history.get(&upload_id) else { return };
        match uploads::delete_remote(core, &upload_id).await {
            Ok(()) => notify_undone(core, &[record.local_filename]),
            Err(reason) => uploads::show_notification(core, &t!("Couldn't delete {0} from the bucket", record.local_filename), &reason),
        }
        return;
    }
    {
        let mut undoable = core.uploads.undoable.lock().unwrap();
        if undoable.as_deref() == Some(action_id) {
            *undoable = None;
            if let Some(timer) = core.uploads.undo_timer.lock().unwrap().take() {
                timer.abort();
            }
        }
    }
    core.notify(events::UNDO_CHANGED);

    let going: Vec<(String, String)> = core
        .uploads
        .jobs
        .lock()
        .unwrap()
        .iter()
        .filter(|job| job.action_id.as_deref() == Some(action_id))
        .map(|job| (job.id.clone(), job.display_name()))
        .collect();
    let (record_ids, reused_names) = core
        .uploads
        .actions
        .lock()
        .unwrap()
        .get(action_id)
        .map(|action| (action.record_ids.clone(), action.reused_names.clone()))
        .unwrap_or_default();
    let mut names: Vec<String> = Vec::new();
    for (job_id, name) in going {
        names.push(name);
        uploads::cancel(core, &job_id);
    }
    let mut failures: Vec<(String, String)> = Vec::new();
    for record_id in record_ids {
        let Some(record) = core.history.get(&record_id) else { continue };
        names.push(record.local_filename.clone());
        if let Err(reason) = uploads::delete_remote(core, &record_id).await {
            failures.push((record.local_filename, reason));
        }
    }
    names.extend(reused_names);
    restore_clipboard(core, action_id);
    let tags = core.uploads.actions.lock().unwrap().get(action_id).map(|action| action.notification_tags.clone()).unwrap_or_default();
    remove_notification_tags(core, &tags);
    for (name, reason) in &failures {
        uploads::show_notification(core, &t!("Couldn't delete {0} from the bucket", name), reason);
    }
    if failures.is_empty() {
        notify_undone(core, &names);
    }
    core.uploads.actions.lock().unwrap().remove(action_id);
    core.notify(events::HISTORY_CHANGED);
}

fn notify_undone(core: &SharedCore, names: &[String]) {
    if !core.settings.get().show_notification || names.is_empty() {
        return;
    }
    let body = match names {
        [name] => name.clone(),
        _ => t!("{0} files", names.len()),
    };
    uploads::show_notification(core, &t!("Upload undone"), &body);
}

/// What became of the short link when the link was copied, for the
/// upload's history entry.
pub fn early_attempt(early: &EarlyLink) -> crate::short_links::Attempt {
    use crate::short_links::{engine::Created, Attempt, Pending};
    if let Some(short) = &early.short_link {
        return Attempt::Created(Pending {
            created: Created { short_url: short.short_url.clone(), provider_id: short.provider_id.clone() },
            provider: short.provider.clone(),
            provider_name: short.provider_name.clone(),
            domain: short.domain.clone(),
            target_url: short.target_url.clone(),
            expires_at: short.expires_at,
        });
    }
    match &early.short_link_error {
        Some(message) => Attempt::Failed(message.clone()),
        None => Attempt::Skipped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_longer_after_each_try() {
        assert_eq!(offline_delay_seconds(0), 15);
        assert_eq!(offline_delay_seconds(1), 15);
        assert_eq!(offline_delay_seconds(2), 30);
        assert_eq!(offline_delay_seconds(3), 60);
        assert_eq!(offline_delay_seconds(4), 120);
        assert_eq!(offline_delay_seconds(5), 300);
        assert_eq!(offline_delay_seconds(50), 300);
    }

    #[test]
    fn copies_early_only_for_the_users_own_single_uploads() {
        let mut input = UploadInput::from_path("a.png".into());
        assert!(can_copy_early(&input, true));
        assert!(!can_copy_early(&input, false));
        input.waited = true;
        assert!(!can_copy_early(&input, true));
        input.waited = false;
        input.object_key = Some("docs/a.png".into());
        assert!(!can_copy_early(&input, true));
        input.object_key = None;
        input.replacing = Some(uploads::Replacing { record_id: None });
        assert!(!can_copy_early(&input, true));
        input.replacing = None;
        input.folder_key = Some("Project/a.png".into());
        assert!(!can_copy_early(&input, true));
    }
}
