//! Watched folders: files dropped into a folder the user picked are
//! uploaded on their own, with that folder's rules. `engine` decides what
//! to upload; this module connects it to the app: the upload pipeline, the
//! file system watchers, notifications, the clipboard, and the windows.

pub mod engine;
mod hooks;
pub use hooks::check_webhook_url;
pub mod ledger;
pub mod model;
pub mod platform;
pub mod rules;
mod watcher;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::core::{events, SharedCore};
use crate::destinations::DestinationConfig;
use crate::uploads::{self, Expiry, UploadInput, WatchedSource};
use crate::windows::AppWindow;
use crate::t;
use engine::{Conditions, Engine, Host, Now, Overview, Planned, Uploaded};
use model::{Hook, LinkOverride, Pause, Subfolders, WatchedFolder, WatchedFolderStore};

/// Wake-ups (file system events, commands) are gathered this long before
/// a tick, so a burst of events is one tick.
const DEBOUNCE: Duration = Duration::from_millis(150);
/// The windows hear about changes at most this often (the last change is
/// always sent).
const CHANGED_EVERY: Duration = Duration::from_millis(250);

pub struct WatchService {
    pub engine: Engine<AppHost>,
    watchers: Mutex<watcher::Watchers>,
    wake: tokio::sync::Notify,
    /// What Settings should show when it opens (or next looks): a tab, and a
    /// folder to add from File Explorer's "Watch with Aktar".
    request: Mutex<SettingsRequest>,
    /// Whether the tray menu shows watching, and as paused, to rebuild it
    /// only when that changes.
    tray_state: Mutex<Option<(bool, bool)>>,
    /// When the windows were last told about a change, and whether a
    /// delayed telling is on its way.
    changed: Mutex<(Option<std::time::Instant>, bool)>,
    /// Scripts the user picked in Aktar's own file dialog this session, by
    /// hook ID, waiting for their folder to be saved. A script hook is only
    /// saved or run when it's one of these or already saved as it is.
    picked_scripts: Mutex<std::collections::HashMap<String, String>>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsRequest {
    pub tab: Option<String>,
    pub watch_path: Option<String>,
    /// An aktar://import link to open "Import from Another Device" with.
    pub import_link: Option<String>,
}

impl WatchService {
    pub fn new(app: &AppHandle, data_dir: &Path, ledger: ledger::Ledger) -> Self {
        let host = AppHost { app: app.clone() };
        Self {
            engine: Engine::new(host, WatchedFolderStore::load(data_dir), ledger),
            watchers: Mutex::new(watcher::Watchers::default()),
            wake: tokio::sync::Notify::new(),
            request: Mutex::new(SettingsRequest::default()),
            tray_state: Mutex::new(None),
            changed: Mutex::new((None, false)),
            picked_scripts: Mutex::new(std::collections::HashMap::new()),
        }
    }

    pub fn wake(&self) {
        self.wake.notify_one();
    }

    pub fn overview(&self) -> Overview {
        self.engine.overview(Now::current())
    }
}

/// The app's side of the engine.
pub struct AppHost {
    app: AppHandle,
}

impl AppHost {
    fn core(&self) -> SharedCore {
        crate::core::core(&self.app)
    }
}

impl Host for AppHost {
    fn enqueue(&self, folder: &WatchedFolder, files: &[Planned]) -> Vec<Option<String>> {
        let core = self.core();
        let Some(destination) = destination_for(&core, folder) else {
            return vec![None; files.len()];
        };
        let expiry = match folder.expiry_days {
            None => Expiry::FromSettings,
            Some(0) => Expiry::Never,
            Some(days) => Expiry::Days(days),
        };
        let inputs: Vec<UploadInput> = files
            .iter()
            .map(|file| {
                let subpath = match folder.subfolders {
                    Subfolders::KeepStructure => rules::subpath(&file.relative_path).to_string(),
                    Subfolders::Ignore | Subfolders::Flatten => String::new(),
                };
                UploadInput {
                    object_key: file.overwrite_key.clone(),
                    expiry,
                    watched: Some(WatchedSource {
                        folder_id: folder.id.clone(),
                        folder_name: folder.name.clone(),
                        relative_path: file.relative_path.clone(),
                        batch_id: file.batch_id.clone(),
                        key_place: crate::output::KeyPlace {
                            folder: folder.name.clone(),
                            subpath,
                            keep_structure: folder.subfolders == Subfolders::KeepStructure,
                        },
                        sha256: file.sha256.clone(),
                    }),
                    ..UploadInput::from_path(file.path.clone())
                }
            })
            .collect();
        uploads::enqueue(&core, inputs, Some(destination)).into_iter().map(|queued| Some(queued.job_id)).collect()
    }

    fn retry_job(&self, job_id: &str) -> bool {
        let core = self.core();
        if !uploads::has_job(&core, job_id) {
            return false;
        }
        uploads::retry(&core, job_id);
        true
    }

    fn notify(&self, title: &str, body: &str) {
        uploads::show_notification(&self.core(), title, body);
    }

    fn copy(&self, links: &[String]) {
        crate::clipboard::copy(&links.join("\n"));
    }

    /// At most every `CHANGED_EVERY`, with the last change always sent:
    /// every window reloads on it.
    fn changed(&self) {
        let core = self.core();
        refresh_tray_if_needed(&core);
        let delay = {
            let mut changed = core.watched.changed.lock().unwrap();
            if changed.1 {
                return;
            }
            let since = changed.0.map(|at| at.elapsed());
            match since {
                Some(since) if since < CHANGED_EVERY => {
                    changed.1 = true;
                    CHANGED_EVERY - since
                }
                _ => {
                    changed.0 = Some(std::time::Instant::now());
                    drop(changed);
                    core.notify(events::WATCHED_CHANGED);
                    return;
                }
            }
        };
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(delay).await;
            *core.watched.changed.lock().unwrap() = (Some(std::time::Instant::now()), false);
            core.notify(events::WATCHED_CHANGED);
        });
    }

    fn conditions(&self) -> Conditions {
        Conditions { on_battery: platform::on_battery(), metered: platform::metered() }
    }

    fn energy_saver(&self) -> bool {
        platform::energy_saver()
    }

    fn delete_remote(&self, folder: &WatchedFolder, entry: &ledger::Entry, batch_id: &str) {
        let core = self.core();
        let (folder_id, relative, batch_id) = (folder.id.clone(), entry.relative_path.clone(), batch_id.to_string());
        let destination_id = entry.destination_id.clone().unwrap_or_default();
        let object_key = entry.object_key.clone().unwrap_or_default();
        tauri::async_runtime::spawn(async move {
            let result = delete_upload_of(&core, &folder_id, &destination_id, &object_key).await;
            let engine_core = core.clone();
            let _ = tauri::async_runtime::spawn_blocking(move || {
                engine_core.watched.engine.remote_deleted(&folder_id, &relative, &batch_id, result, Now::current())
            })
            .await;
            core.watched.wake();
        });
    }

    fn used_elsewhere(&self, folder_id: &str, destination_id: &str, object_key: &str) -> bool {
        let source = crate::history::watched_source(folder_id);
        self.core()
            .history
            .with_object(destination_id, object_key)
            .iter()
            .any(|record| record.source.as_deref() != Some(source.as_str()))
    }
}

/// Deletes a deleted file's upload the way the Library deletes one: the
/// object, then its history entries (several when it was replaced in place).
/// One no longer in the history has just its object deleted.
async fn delete_upload_of(core: &SharedCore, folder_id: &str, destination_id: &str, object_key: &str) -> Result<(), (String, bool)> {
    let source = crate::history::watched_source(folder_id);
    let records: Vec<_> = core
        .history
        .with_object(destination_id, object_key)
        .into_iter()
        .filter(|record| record.source.as_deref() == Some(source.as_str()))
        .collect();
    if let Some((first, rest)) = records.split_first() {
        uploads::delete_upload(core, &first.id).await.map_err(|failure| (failure.message, failure.transient))?;
        for record in rest {
            core.history.delete(&record.id);
        }
        core.notify(events::HISTORY_CHANGED);
        return Ok(());
    }
    let destination = core
        .destinations
        .find(Some(destination_id))
        .ok_or_else(|| (t!("This upload’s destination was removed."), false))?;
    let credentials = crate::credentials::load(&destination.id).map_err(|error| (error.to_string(), false))?;
    let prefixes = crate::thumbnails::bucket_prefixes(&destination, &core.destinations.all());
    crate::bucket::delete_object(&crate::storage::S3Provider::new(destination.clone(), credentials), object_key, &prefixes)
        .await
        .map_err(|error| (error.to_string(), error.is_transient()))?;
    crate::thumbnails::remote::forget(core, &destination.id, object_key);
    core.history.object_deleted(object_key, &destination.id);
    core.notify(events::HISTORY_CHANGED);
    Ok(())
}

/// The folder's destination, with its own path and link in place of the
/// destination's ("Delete after" goes with each upload). None when it was removed, or there's no
/// destination at all.
fn destination_for(core: &SharedCore, folder: &WatchedFolder) -> Option<DestinationConfig> {
    let mut destination = match &folder.destination_id {
        Some(id) => core.destinations.find(Some(id))?,
        None => core.destinations.default_destination()?,
    };
    if let Some(template) = &folder.path_template {
        destination.object_path_template = template.clone();
    }
    match folder.temporary_link {
        None => {}
        Some(LinkOverride::Public) => destination.temporary_link = None,
        Some(LinkOverride::Temporary(seconds)) => destination.temporary_link = Some(seconds),
    }
    Some(destination)
}

/// Starts watching: a reconcile scan of every folder, then ticks only when
/// something is due or something happened (events, commands, the power
/// source, the network, waking up). Nothing runs while nothing is.
pub fn start(core: &SharedCore) {
    core.watched.engine.rescan_all();
    // A hash coming back from the pool ticks the loop to apply it.
    let waker_core = std::sync::Arc::downgrade(core);
    core.watched.engine.set_waker(std::sync::Arc::new(move || {
        if let Some(core) = waker_core.upgrade() {
            core.watched.wake();
        }
    }));
    let network_core = core.clone();
    platform::watch_network(move || {
        let core = network_core.clone();
        // Off the thread Windows calls this on.
        tauri::async_runtime::spawn(async move {
            // Give the connection a moment to come up properly.
            tokio::time::sleep(Duration::from_secs(3)).await;
            let engine_core = core.clone();
            let _ = tauri::async_runtime::spawn_blocking(move || engine_core.watched.engine.network_changed(Now::current())).await;
            core.watched.wake();
        });
    });
    let (power_core, resume_core) = (core.clone(), core.clone());
    platform::watch_power(
        move || {
            power_core.watched.engine.conditions_changed();
            power_core.watched.engine.energy_saver_changed();
            power_core.watched.wake();
        },
        move || {
            resume_core.watched.engine.rescan_all();
            resume_core.watched.wake();
        },
    );
    let core = core.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            let tick_core = core.clone();
            // File checks and hashing block, so they're done off the
            // runtime's workers.
            let next = tauri::async_runtime::spawn_blocking(move || {
                let now = Now::current();
                let next = tick_core.watched.engine.tick(now);
                let plan = tick_core.watched.engine.watch_plan(now);
                tick_core.watched.watchers.lock().unwrap().sync(&tick_core, plan);
                next
            })
            .await
            .ok()
            .flatten();
            let woken = match next {
                Some(at) => tokio::select! {
                    _ = tokio::time::sleep_until(tokio::time::Instant::from_std(at)) => false,
                    _ = core.watched.wake.notified() => true,
                },
                None => {
                    core.watched.wake.notified().await;
                    true
                }
            };
            if woken {
                tokio::time::sleep(DEBOUNCE).await;
            }
        }
    });
}

/// From the upload pipeline: a watched file went up. The object is looked
/// up in the bucket before the original is moved away, then the folder's
/// hooks run.
pub async fn upload_succeeded(core: &SharedCore, source: &WatchedSource, destination: &DestinationConfig, uploaded: Uploaded) {
    let Some(folder) = core.watched.engine.store.folder(&source.folder_id) else { return };
    let verified = match folder.after_upload {
        model::AfterUpload::Trash | model::AfterUpload::MoveToUploaded => verify(destination, &uploaded).await,
        _ => true,
    };
    let engine_core = core.clone();
    let source = source.clone();
    let after = tauri::async_runtime::spawn_blocking(move || {
        engine_core.watched.engine.succeeded(&source.folder_id, &source.relative_path, &source.batch_id, &uploaded, verified, Now::current())
    })
    .await
    .ok()
    .flatten();
    if let Some(after) = after {
        hooks::run(core, after);
    }
    core.watched.wake();
}

/// Whether the object is in the bucket with the size that was uploaded.
async fn verify(destination: &DestinationConfig, uploaded: &Uploaded) -> bool {
    let Ok(credentials) = crate::credentials::load(&destination.id) else { return false };
    let provider = crate::storage::S3Provider::new(destination.clone(), credentials);
    matches!(provider.object_size(&uploaded.object_key).await, Ok(Some(size)) if size == uploaded.byte_size)
}

/// Off the async runtime's workers: it writes to the ledger.
pub fn upload_failed(core: &SharedCore, source: &WatchedSource, message: String, transient: bool) {
    let (core, source) = (core.clone(), source.clone());
    tauri::async_runtime::spawn_blocking(move || {
        core.watched.engine.failed(&source.folder_id, &source.relative_path, message, transient, Now::current());
        core.watched.wake();
    });
}

pub fn upload_cancelled(core: &SharedCore, source: &WatchedSource) {
    let (core, source) = (core.clone(), source.clone());
    tauri::async_runtime::spawn_blocking(move || {
        core.watched.engine.cancelled(&source.folder_id, &source.relative_path, &source.batch_id);
        core.watched.wake();
    });
}

// MARK: - Pausing

/// The longest pause with an end: a year.
pub const MAX_PAUSE_MINUTES: u64 = 525_600;

/// Pauses every folder for `minutes` (at most a year), or until resumed.
pub fn pause(core: &SharedCore, minutes: Option<u64>) {
    core.watched.engine.set_pause(pause_for(minutes, crate::util::now_millis()));
    core.watched.wake();
}

/// The pause `minutes` from `now_millis` makes; none or 0 is until resumed.
fn pause_for(minutes: Option<u64>, now_millis: i64) -> Pause {
    match minutes.filter(|minutes| *minutes > 0) {
        Some(minutes) => {
            let millis = minutes.min(MAX_PAUSE_MINUTES) as i64 * 60_000;
            Pause::Until(now_millis.saturating_add(millis))
        }
        None => Pause::Forever,
    }
}

/// Pause minutes from the local API: a whole number, up to a year (more
/// is a year). None when it's anything else, zero or negative included.
pub fn pause_minutes(value: &serde_json::Value) -> Option<u64> {
    let minutes = value.as_u64()?;
    (minutes > 0).then(|| minutes.min(MAX_PAUSE_MINUTES))
}

/// The same from an aktar:// link's `minutes=`.
pub fn pause_minutes_text(text: &str) -> Option<u64> {
    let text = text.trim();
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    // Too many digits for a number is more than a year all the same.
    let minutes: u64 = text.parse().unwrap_or(u64::MAX);
    (minutes > 0).then(|| minutes.min(MAX_PAUSE_MINUTES))
}

pub fn resume(core: &SharedCore) {
    core.watched.engine.set_pause(Pause::None);
    core.watched.wake();
}

/// "Until Tomorrow": until midnight.
pub fn minutes_until_tomorrow() -> u64 {
    use chrono::{Duration as Days, Local, NaiveTime};
    let now = Local::now();
    let midnight = (now.date_naive() + Days::days(1)).and_time(NaiveTime::MIN);
    let until = midnight.and_local_timezone(Local).earliest().unwrap_or(now + Days::hours(24));
    ((until - now).num_seconds().max(60) as u64).div_ceil(60)
}

/// From the folder list alone: whether there are folders, and whether the
/// user paused them.
fn refresh_tray_if_needed(core: &SharedCore) {
    let list = core.watched.engine.store.get();
    let state = Some((!list.folders.is_empty(), list.paused_until.is_active(crate::util::now_millis())));
    let mut current = core.watched.tray_state.lock().unwrap();
    if *current != state {
        *current = state;
        drop(current);
        crate::tray::refresh_menu(&core.app);
    }
}

// MARK: - Adding folders

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderCheck {
    pub path: String,
    pub name: String,
    /// Files already there that it would upload.
    pub existing_files: usize,
}

/// Aktar's own data and cache folders, which can't be watched (nor
/// uploaded from by the windows).
pub(crate) fn app_dirs(core: &SharedCore) -> Vec<PathBuf> {
    let paths = core.app.path();
    [paths.app_data_dir(), paths.app_local_data_dir(), paths.app_cache_dir(), paths.app_config_dir()]
        .into_iter()
        .filter_map(Result::ok)
        .collect()
}

/// Refuses folders that can't be watched, with the reason.
fn validate(core: &SharedCore, path: &Path, except: Option<&str>) -> Result<(), String> {
    if !path.is_dir() {
        return Err(rules::Forbidden::NotAFolder.message());
    }
    let watched: Vec<(PathBuf, String)> = core
        .watched
        .engine
        .store
        .get()
        .folders
        .into_iter()
        .filter(|folder| Some(folder.id.as_str()) != except)
        .map(|folder| (folder.path, folder.name))
        .collect();
    match rules::forbidden(path, &rules::Protected::current(app_dirs(core)), &watched) {
        Some(reason) => Err(reason.message()),
        None => Ok(()),
    }
}

/// Checks a folder picked to be watched, and counts the files in it that
/// would be uploaded, for "This folder already has 37 files".
pub fn check(core: &SharedCore, path: &str) -> Result<FolderCheck, String> {
    let path = PathBuf::from(path);
    validate(core, &path, None)?;
    let draft = WatchedFolder::new(&path);
    Ok(FolderCheck { path: path.to_string_lossy().into_owned(), name: draft.name.clone(), existing_files: engine::existing_files(&draft) })
}

pub fn add(core: &SharedCore, path: &str, upload_existing: bool) -> Result<WatchedFolder, String> {
    let path = PathBuf::from(path);
    validate(core, &path, None)?;
    let folder = core.watched.engine.add(WatchedFolder::new(&path), upload_existing, Now::current());
    core.watched.wake();
    Ok(folder)
}

/// "Upload Screenshots Automatically": Windows' Screenshots folder, made
/// if it isn't there yet. What's in it already is skipped.
pub fn add_screenshots(core: &SharedCore) -> Result<WatchedFolder, String> {
    let path = platform::screenshots_folder().ok_or_else(|| t!("Couldn’t find the Screenshots folder."))?;
    std::fs::create_dir_all(&path).map_err(|error| error.to_string())?;
    validate(core, &path, None)?;
    let folder = core.watched.engine.add(WatchedFolder::screenshots(&path), false, Now::current());
    core.watched.wake();
    Ok(folder)
}

pub fn save(core: &SharedCore, folder: WatchedFolder) -> Result<(), String> {
    validate(core, &folder.path, Some(&folder.id))?;
    for hook in &folder.hooks {
        check_hook(core, hook)?;
    }
    core.watched.engine.update(folder, Now::current());
    core.watched.wake();
    Ok(())
}

/// Sends a sample payload through a hook, for its "Test" button.
pub async fn test_hook(core: &SharedCore, folder: WatchedFolder, hook: Hook) -> Result<(), String> {
    check_hook(core, &hook)?;
    hooks::test(core, &folder, &hook).await
}

/// A script picked in a file dialog Aktar opens itself, as a new hook: the
/// windows can't name a program to run, only ask for this dialog. None
/// when it was cancelled.
pub async fn pick_script(core: &SharedCore, window: &tauri::WebviewWindow) -> Result<Option<Hook>, String> {
    use tauri_plugin_dialog::DialogExt;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    window
        .dialog()
        .file()
        .set_parent(window)
        .add_filter(t!("Scripts"), &["ps1", "bat", "cmd", "exe"])
        .pick_file(move |path| {
            let _ = sender.send(path);
        });
    let Some(path) = receiver.await.map_err(|error| error.to_string())? else { return Ok(None) };
    let target = path.into_path().map_err(|error| error.to_string())?.to_string_lossy().into_owned();
    let hook = Hook { id: crate::util::new_id(), kind: model::HookKind::Script, target: target.clone(), enabled: true };
    core.watched.picked_scripts.lock().unwrap().insert(hook.id.clone(), target);
    Ok(Some(hook))
}

/// Refuses a hook the windows made up: a script that wasn't picked in
/// Aktar's own dialog (or saved before, exactly as it is), and a webhook
/// that would send the upload's details over plain http:// across the
/// internet.
fn check_hook(core: &SharedCore, hook: &Hook) -> Result<(), String> {
    match hook.kind {
        model::HookKind::Webhook => hooks::check_webhook_url(&hook.target),
        model::HookKind::Script => {
            let picked = core.watched.picked_scripts.lock().unwrap().get(&hook.id) == Some(&hook.target);
            let saved = || {
                core.watched.engine.store.get().folders.iter().flat_map(|folder| &folder.hooks).any(|saved| {
                    saved.id == hook.id && saved.kind == model::HookKind::Script && saved.target == hook.target
                })
            };
            if picked || saved() {
                Ok(())
            } else {
                Err(t!("Pick the script with “Add Script…”."))
            }
        }
    }
}

/// "Watch with Aktar" in File Explorer: Settings opens on Watched Folders
/// and adds the folder there, asking about the files already in it.
pub fn request_watch(core: &SharedCore, path: PathBuf) {
    {
        let mut request = core.watched.request.lock().unwrap();
        request.tab = Some("watched".into());
        request.watch_path = Some(path.to_string_lossy().into_owned());
    }
    open_settings(core);
}

/// Settings, on the Watched Folders tab (aktar://watch).
pub fn show_settings(core: &SharedCore) {
    core.watched.request.lock().unwrap().tab = Some("watched".into());
    open_settings(core);
}

/// aktar://import: Settings opens "Import from Another Device" with the
/// link filled in. Nothing is imported until the code is typed and Import
/// is pressed there.
pub fn show_import(core: &SharedCore, link: &str) {
    let mut request = core.watched.request.lock().unwrap();
    request.tab = Some("destinations".into());
    request.import_link = Some(link.to_string());
    drop(request);
    open_settings(core);
}

fn open_settings(core: &SharedCore) {
    crate::windows::open(&core.app, AppWindow::Settings);
    // An open Settings window takes it now; a new one when it has loaded.
    core.notify(events::SETTINGS_REQUEST);
}

pub fn take_request(core: &SharedCore) -> SettingsRequest {
    std::mem::take(&mut *core.watched.request.lock().unwrap())
}
