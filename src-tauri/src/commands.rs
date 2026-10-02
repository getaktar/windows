//! Everything the windows can ask of the app. Storage, credentials, and the
//! clipboard are only reachable through these, never through general
//! purpose plugin APIs.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_opener::OpenerExt;

use crate::core::{events, SharedCore};
use crate::credentials::{self, StorageCredentials};
use crate::destinations::DestinationConfig;
use crate::expiry::{self, FormRules, RulesCheck};
use crate::history::UploadRecord;
use crate::local_api::{self, LocalApiState};
use crate::settings::{Settings, SettingsPatch};
use crate::storage::{BucketListing, ConnectionResult, S3Provider};
use crate::updater::{self, UpdateStatus};
use crate::uploads::{self, JobSnapshot, NameRequest, UploadInput};
use crate::watched::engine::Now;
use crate::watched::model::{Hook, WatchedFolder};
use crate::windows::AppWindow;
use crate::{bucket, i18n, panel, t};

type Core<'a> = State<'a, SharedCore>;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    version: String,
    language: String,
    region_locale: Option<String>,
    /// Installed from the Microsoft Store, which handles updates.
    packaged: bool,
    language_override: Option<String>,
    languages: Vec<(String, String)>,
}

#[tauri::command]
pub fn app_info(core: Core) -> AppInfo {
    AppInfo {
        version: core.app.package_info().version.to_string(),
        language: i18n::current(),
        region_locale: crate::system::region_locale(),
        packaged: crate::package::is_packaged(),
        language_override: core.settings.get().language,
        languages: i18n::SUPPORTED.iter().map(|(code, name)| (code.to_string(), name.to_string())).collect(),
    }
}

// MARK: - Destinations

#[tauri::command]
pub fn list_destinations(core: Core) -> Vec<DestinationConfig> {
    core.destinations.all()
}

fn filled(credentials: Option<StorageCredentials>) -> Option<StorageCredentials> {
    credentials.filter(|c| !c.access_key_id.trim().is_empty() && !c.secret_access_key.is_empty()).map(|c| {
        StorageCredentials {
            access_key_id: c.access_key_id.trim().to_string(),
            secret_access_key: c.secret_access_key.trim().to_string(),
            session_token: c.session_token.filter(|token| !token.trim().is_empty()),
        }
    })
}

/// Adds a destination (when `config.id` is empty) or updates one. Leaving
/// the credential fields empty while editing keeps the stored ones.
///
/// `rules` is what the form found out about the lifecycle rules while it
/// was open, recorded under the destination's ID. When nothing was checked
/// there, a status that was true of another bucket or key no longer
/// applies, so it's reset to not active.
#[tauri::command]
pub fn save_destination(
    core: Core,
    mut config: DestinationConfig,
    credentials: Option<StorageCredentials>,
    rules: Option<FormRules>,
) -> Result<DestinationConfig, String> {
    let is_new = config.id.is_empty();
    let credentials = filled(credentials);
    // A result about another bucket (the connection fields were edited
    // after the check) says nothing about this one.
    let rules = match rules.unwrap_or_default() {
        FormRules::Checked { connection: Some(connection), .. } if !connection.is_for(&config) => FormRules::NotChecked,
        rules => rules,
    };
    let reconnected = !is_new && !is_saved_connection(&core, &config, credentials.as_ref());
    if is_new {
        config.id = crate::util::new_id();
        let Some(credentials) = credentials else {
            return Err(t!("Enter an Access Key ID and Secret Access Key."));
        };
        credentials::save(&credentials, &config.id).map_err(|error| error.to_string())?;
        core.destinations.add(config.clone());
    } else {
        if let Some(credentials) = credentials {
            credentials::save(&credentials, &config.id).map_err(|error| error.to_string())?;
        }
        core.destinations.update(config.clone());
    }
    match rules {
        FormRules::Checked { check, .. } => expiry::record(&core, &config.id, check),
        // Rows from the old bucket shouldn't expire against the new one.
        FormRules::NotChecked if reconnected => expiry::record(&core, &config.id, None),
        FormRules::NotChecked => {}
    }
    core.notify(events::DESTINATIONS_CHANGED);
    Ok(config)
}

/// Whether `config` reaches the same bucket with the same keys as the
/// saved destination with its ID, so a lifecycle rules result obtained
/// with it is true of that destination.
fn is_saved_connection(core: &Core, config: &DestinationConfig, credentials: Option<&StorageCredentials>) -> bool {
    let Some(saved) = core.destinations.find(Some(&config.id)).filter(|saved| saved.id == config.id) else {
        return false;
    };
    let same_keys = match credentials {
        None => true,
        Some(entered) => credentials::load(&config.id).is_ok_and(|stored| {
            stored.access_key_id == entered.access_key_id && stored.secret_access_key == entered.secret_access_key
        }),
    };
    same_keys
        && saved.endpoint.trim() == config.endpoint.trim()
        && saved.bucket.trim() == config.bucket.trim()
        && saved.region.trim() == config.region.trim()
}

/// A copy under a new ID with the same keys, as a starting point for
/// another upload profile on the same bucket.
#[tauri::command]
pub fn duplicate_destination(core: Core, id: String) -> Result<DestinationConfig, String> {
    let original = core
        .destinations
        .find(Some(&id))
        .filter(|destination| destination.id == id)
        .ok_or_else(|| t!("No destination to upload to. Add one in Settings."))?;
    let credentials = credentials::load(&original.id).map_err(|error| error.to_string())?;
    let copy = DestinationConfig {
        id: crate::util::new_id(),
        name: t!("{0} Copy", original.name),
        is_default: false,
        ..original.clone()
    };
    credentials::save(&credentials, &copy.id).map_err(|error| error.to_string())?;
    // Same bucket, same rules.
    if let Some(check) = expiry::cached(&core, &original.id) {
        expiry::record(&core, &copy.id, Some(check));
    }
    core.destinations.add(copy.clone());
    core.notify(events::DESTINATIONS_CHANGED);
    Ok(copy)
}

/// The panel's "Delete after" choice, kept per destination.
#[tauri::command]
pub fn set_destination_expiry(core: Core, id: String, days: u32) {
    core.destinations.modify(&id, |destination| destination.expiry_days = Some(days));
    core.notify(events::DESTINATIONS_CHANGED);
}

/// The panel's "Link" choice: the public URL (none) or a temporary link
/// valid this many seconds.
#[tauri::command]
pub fn set_destination_link(core: Core, id: String, seconds: Option<u64>) {
    core.destinations.modify(&id, |destination| destination.temporary_link = seconds);
    core.notify(events::DESTINATIONS_CHANGED);
}

#[tauri::command]
pub fn remove_destination(core: Core, id: String) {
    core.destinations.remove(&id);
    expiry::record(&core, &id, None);
    core.notify(events::DESTINATIONS_CHANGED);
}

#[tauri::command]
pub fn set_default_destination(core: Core, id: String) {
    core.destinations.set_default(&id);
    core.notify(events::DESTINATIONS_CHANGED);
}

#[tauri::command]
pub async fn test_connection(
    config: DestinationConfig,
    credentials: Option<StorageCredentials>,
) -> Result<ConnectionResult, String> {
    let credentials = match filled(credentials) {
        Some(credentials) => credentials,
        None if !config.id.is_empty() => credentials::load(&config.id).map_err(|error| error.to_string())?,
        None => return Err(t!("Enter an Access Key ID and Secret Access Key.")),
    };
    S3Provider::new(config, credentials).test_connection().await.map_err(|error| error.to_string())
}

/// Whether the destination's lifecycle rules for expiring uploads were set
/// up, as of the last check. None when they haven't been checked yet.
#[tauri::command]
pub fn expiry_rules_status(core: Core, destination_id: String) -> Option<RulesCheck> {
    expiry::cached(&core, &destination_id)
}

/// The entered credentials, or the saved destination's.
fn credentials_for(config: &DestinationConfig, credentials: Option<StorageCredentials>) -> Result<StorageCredentials, String> {
    match filled(credentials) {
        Some(credentials) => Ok(credentials),
        None if !config.id.is_empty() => credentials::load(&config.id).map_err(|error| error.to_string()),
        None => Err(t!("Enter an Access Key ID and Secret Access Key.")),
    }
}

/// Installs the lifecycle rules for expiring uploads ("Set Up" in the
/// destination form, "Set Up Auto-Delete" in the panel), with the
/// credentials being entered or the saved ones. Once they're active, "Delete
/// after" applies to the destination. The result is recorded right away
/// only when it's true of the saved destination; the form records its own
/// when it's saved.
#[tauri::command]
pub async fn set_up_expiry_rules(
    core: Core<'_>,
    config: DestinationConfig,
    credentials: Option<StorageCredentials>,
) -> Result<RulesCheck, String> {
    let record = is_saved_connection(&core, &config, filled(credentials.clone()).as_ref());
    let credentials = credentials_for(&config, credentials)?;
    expiry::set_up(&core, config, credentials, record).await
}

/// The `tmp/{N}d/` folders that already hold files while their rule isn't
/// in place: setting the rules up would have the bucket delete those too,
/// so the windows ask first.
#[tauri::command]
pub async fn expiry_prefixes_in_use(
    config: DestinationConfig,
    credentials: Option<StorageCredentials>,
) -> Result<Vec<String>, String> {
    let credentials = credentials_for(&config, credentials)?;
    S3Provider::new(config, credentials).expiry_prefixes_in_use().await.map_err(|error| error.to_string())
}

/// "Turn Off and Remove Rules" in the destination form: takes Aktar's
/// lifecycle rules out of the bucket, keeping its other rules.
#[tauri::command]
pub async fn remove_expiry_rules(
    core: Core<'_>,
    config: DestinationConfig,
    credentials: Option<StorageCredentials>,
) -> Result<(), String> {
    let record = is_saved_connection(&core, &config, filled(credentials.clone()).as_ref());
    let credentials = credentials_for(&config, credentials)?;
    expiry::remove(&core, config, credentials, record).await
}

// MARK: - Uploads

fn inputs_from(paths: Vec<String>) -> Vec<UploadInput> {
    paths
        .into_iter()
        .map(PathBuf::from)
        .filter(|path| path.is_file() || path.is_dir())
        .map(UploadInput::from_path)
        .collect()
}

/// Returns how many of `paths` are files or folders that could be queued.
/// With no destination set up, nothing is queued and the user is told why.
/// With `rename`, each file first waits for its name in the panel.
#[tauri::command]
pub fn upload_files(core: Core, paths: Vec<String>, destination_id: Option<String>, rename: Option<bool>) -> usize {
    let destination = destination_id.and_then(|id| core.destinations.find(Some(&id)));
    let inputs = inputs_from(paths);
    let count = inputs.len();
    if count > 0 && rename == Some(true) && core.destinations.default_destination().is_some() {
        uploads::ask_for_names(&core, inputs, destination);
    } else if count > 0 {
        uploads::enqueue(&core, inputs, destination);
    }
    count
}

#[tauri::command]
pub async fn upload_clipboard(core: Core<'_>) -> Result<bool, String> {
    Ok(uploads::upload_clipboard(&core, false).await)
}

/// Files waiting for their name in the panel's "Name This Upload".
#[tauri::command]
pub fn pending_names(core: Core) -> Vec<NameRequest> {
    uploads::pending_names(&core)
}

/// Uploads a file waiting for its name, or drops it when `name` is none.
#[tauri::command]
pub fn resolve_name(core: Core, id: String, name: Option<String>) {
    uploads::resolve_name(&core, &id, name);
}

/// Whether Alt is held right now: files dropped on the panel with Alt
/// held are named before they go up. Drops don't carry the keys pressed.
#[tauri::command]
pub fn alt_key_down() -> bool {
    crate::system::alt_key_down()
}

#[tauri::command]
pub fn list_jobs(core: Core) -> Vec<JobSnapshot> {
    core.uploads.snapshot()
}

#[tauri::command]
pub fn retry_job(core: Core, id: String) {
    uploads::retry(&core, &id);
}

#[tauri::command]
pub fn cancel_job(core: Core, id: String) {
    uploads::cancel(&core, &id);
}

#[tauri::command]
pub fn dismiss_job(core: Core, id: String) {
    uploads::dismiss(&core, &id);
}

// MARK: - History

#[tauri::command]
pub fn list_history(core: Core) -> Vec<UploadRecord> {
    core.history.all()
}

/// "Copy Temporary Link" for an upload in history: a fresh presigned link,
/// which works even when the bucket is private or the one copied at upload
/// time has run out.
#[tauri::command]
pub async fn record_temporary_link(core: Core<'_>, id: String, seconds: u64) -> Result<String, String> {
    let record = core.history.get(&id).ok_or_else(|| t!("This upload’s destination was removed."))?;
    uploads::temporary_url(&core, &record, seconds.clamp(60, 604_800)).await
}

#[tauri::command]
pub fn thumbnails_dir(core: Core) -> String {
    core.thumbnails_dir.to_string_lossy().into_owned()
}

/// Deletes each record's remote file, then its history entry. Returns the
/// error for every record that couldn't be deleted.
#[tauri::command]
pub async fn delete_remote(core: Core<'_>, ids: Vec<String>) -> Result<HashMap<String, String>, String> {
    let mut failures = HashMap::new();
    for id in ids {
        if let Err(message) = uploads::delete_remote(&core, &id).await {
            failures.insert(id, message);
        }
    }
    Ok(failures)
}

#[tauri::command]
pub fn remove_from_history(core: Core, ids: Vec<String>) {
    for id in ids {
        core.history.delete(&id);
    }
    core.notify(events::HISTORY_CHANGED);
}

// MARK: - Bucket browser

fn storage_for(core: &SharedCore, destination_id: &str) -> Result<(DestinationConfig, S3Provider), String> {
    let destination = core
        .destinations
        .find(Some(destination_id))
        .ok_or_else(|| t!("No destination to upload to. Add one in Settings."))?;
    let credentials = credentials::load(&destination.id).map_err(|error| error.to_string())?;
    Ok((destination.clone(), S3Provider::new(destination, credentials)))
}

#[tauri::command]
pub async fn list_objects(
    core: Core<'_>,
    destination_id: String,
    prefix: String,
    continuation_token: Option<String>,
    recursive: bool,
) -> Result<BucketListing, String> {
    let (_, storage) = storage_for(&core, &destination_id)?;
    let result = if recursive {
        storage.list_recursively(&prefix, continuation_token).await
    } else {
        storage.list(&prefix, continuation_token).await
    };
    result.map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn bucket_delete(core: Core<'_>, destination_id: String, key: String) -> Result<(), String> {
    let (destination, storage) = storage_for(&core, &destination_id)?;
    storage.delete(&key).await.map_err(|error| error.to_string())?;
    core.history.object_deleted(&key, &destination.id);
    core.notify(events::HISTORY_CHANGED);
    Ok(())
}

/// Renames or moves an object; `new_key` is a full key, so changing the
/// folder part moves it. Returns the cleaned-up key it ended up at.
#[tauri::command]
pub async fn bucket_move(core: Core<'_>, destination_id: String, from: String, to: String) -> Result<String, String> {
    let new_key = to.trim().trim_matches('/').to_string();
    if new_key.is_empty() || new_key == from {
        return Ok(from);
    }
    let (destination, storage) = storage_for(&core, &destination_id)?;
    match bucket::move_object(&storage, &from, &new_key).await {
        Ok(()) => {
            core.history.object_moved(&from, &new_key, &destination, expiry::is_active(&core, &destination.id));
            core.notify(events::HISTORY_CHANGED);
            Ok(new_key)
        }
        Err(bucket::MoveError::Exists) => Err(t!("An object named “{0}” already exists.", new_key)),
        Err(bucket::MoveError::Storage(error)) => Err(error.to_string()),
    }
}

#[tauri::command]
pub async fn bucket_create_folder(core: Core<'_>, destination_id: String, prefix: String, name: String) -> Result<String, String> {
    let name = name.trim().trim_matches('/');
    if name.is_empty() {
        return Err(t!("The folder name is required."));
    }
    let folder = format!("{}{name}/", bucket::normalized_folder(&prefix));
    let (_, storage) = storage_for(&core, &destination_id)?;
    storage.create_folder(&folder).await.map_err(|error| error.to_string())?;
    Ok(folder)
}

#[tauri::command]
pub async fn bucket_presign(core: Core<'_>, destination_id: String, key: String, seconds: u64) -> Result<String, String> {
    let (_, storage) = storage_for(&core, &destination_id)?;
    storage.temporary_url(&key, seconds.clamp(60, 604_800)).await.map_err(|error| error.to_string())
}

/// Uploads into `prefix` under each file's own name, adding " 2", " 3"...
/// when a name is taken so nothing is overwritten. A folder keeps its
/// structure here, like copying it in File Explorer. Returns how many
/// files were queued.
#[tauri::command]
pub async fn bucket_upload(core: Core<'_>, destination_id: String, paths: Vec<String>, prefix: String) -> Result<usize, String> {
    let (destination, storage) = storage_for(&core, &destination_id)?;
    let prefix = bucket::normalized_folder(&prefix);
    let mut claimed: Vec<String> = Vec::new();
    let mut inputs = Vec::new();
    for input in inputs_from(paths) {
        let files = if input.path.is_dir() {
            let root = format!("{prefix}{}/", crate::folder_upload::name(&input.path));
            crate::folder_upload::files(&input.path, Some(crate::folder_upload::MAX_FILES))
                .map_err(|error| error.to_string())?
                .into_iter()
                .map(|entry| {
                    let folder = match entry.relative_path.rfind('/') {
                        Some(slash) => format!("{root}{}", &entry.relative_path[..=slash]),
                        None => root.clone(),
                    };
                    (UploadInput::from_path(entry.path), folder)
                })
                .collect()
        } else {
            vec![(input, prefix.clone())]
        };
        for (mut file, folder) in files {
            let key = bucket::available_key(&storage, &file.original_filename, &folder, &claimed)
                .await
                .map_err(|error| error.to_string())?;
            claimed.push(key.clone());
            file.object_key = Some(key);
            inputs.push(file);
        }
    }
    let count = inputs.len();
    uploads::enqueue(&core, inputs, Some(destination));
    Ok(count)
}

/// Downloads a file for an inline preview (PDF, text, Markdown). Going
/// through Rust avoids the CORS rules a fetch from the webview would hit.
#[tauri::command]
pub async fn fetch_remote(url: String) -> Result<tauri::ipc::Response, String> {
    const MAX_BYTES: u64 = 25 * 1024 * 1024;
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("Unsupported URL".into());
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|error| error.to_string())?;
    let response = client.get(&url).send().await.map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(response.status().to_string());
    }
    if response.content_length().is_some_and(|length| length > MAX_BYTES) {
        return Err(t!("Preview unavailable"));
    }
    let bytes = response.bytes().await.map_err(|error| error.to_string())?;
    Ok(tauri::ipc::Response::new(bytes.to_vec()))
}

// MARK: - QR codes

#[tauri::command]
pub fn qr_code(text: String) -> Result<crate::qr::QrMatrix, String> {
    crate::qr::matrix(&text)
}

#[tauri::command]
pub fn copy_qr_image(text: String) -> Result<(), String> {
    crate::clipboard::copy_image(&crate::qr::image(&text)?)
}

/// Saves the QR code as a PNG where the user picked in the save dialog.
#[tauri::command]
pub async fn save_qr_image(text: String, path: String) -> Result<(), String> {
    let data = crate::qr::png(&text)?;
    tokio::fs::write(path, data).await.map_err(|error| error.to_string())
}

// MARK: - Settings

#[tauri::command]
pub fn get_settings(core: Core) -> Settings {
    core.settings.get()
}

#[tauri::command]
pub fn update_settings(core: Core, patch: SettingsPatch) -> Settings {
    let settings = core.settings.apply_patch(patch);
    core.notify(events::SETTINGS_CHANGED);
    settings
}

#[tauri::command]
pub fn set_shortcut_paused(app: AppHandle, paused: bool) {
    crate::hotkey::set_paused(&app, paused);
}

#[tauri::command]
pub fn set_shortcut(app: AppHandle, core: Core, accelerator: Option<String>) -> Result<(), String> {
    let previous = core.settings.get();
    if let Err(message) = crate::hotkey::register(&app, accelerator.as_deref(), previous.rename_shortcut.as_deref()) {
        // Put the old ones back so a failed change doesn't leave none.
        let _ = crate::hotkey::register(&app, previous.shortcut.as_deref(), previous.rename_shortcut.as_deref());
        return Err(message);
    }
    core.settings.update(|settings| settings.shortcut = accelerator);
    core.notify(events::SETTINGS_CHANGED);
    Ok(())
}

/// "Rename and upload clipboard", which asks for the upload's name first.
#[tauri::command]
pub fn set_rename_shortcut(app: AppHandle, core: Core, accelerator: Option<String>) -> Result<(), String> {
    let previous = core.settings.get();
    if let Err(message) = crate::hotkey::register(&app, previous.shortcut.as_deref(), accelerator.as_deref()) {
        let _ = crate::hotkey::register(&app, previous.shortcut.as_deref(), previous.rename_shortcut.as_deref());
        return Err(message);
    }
    core.settings.update(|settings| settings.rename_shortcut = accelerator);
    core.notify(events::SETTINGS_CHANGED);
    Ok(())
}

/// Unlike the Mac app, the language switches right away, no restart needed.
#[tauri::command]
pub fn set_language(app: AppHandle, core: Core, code: Option<String>) -> String {
    core.settings.update(|settings| settings.language = code.clone());
    let applied = i18n::apply(code.as_deref());
    crate::tray::refresh_menu(&app);
    crate::windows::retitle_all(&app);
    crate::shell::register_context_menu();
    core.emit(events::LANGUAGE_CHANGED, applied.clone());
    core.notify(events::SETTINGS_CHANGED);
    applied
}

/// The Store package's startup task, or the NSIS install's login item.
#[tauri::command]
pub async fn get_launch_at_login(app: AppHandle) -> Result<bool, String> {
    if crate::package::is_packaged() {
        return tauri::async_runtime::spawn_blocking(crate::package::startup_enabled)
            .await
            .map_err(|error| error.to_string());
    }
    Ok(app.autolaunch().is_enabled().unwrap_or(false))
}

#[tauri::command]
pub async fn set_launch_at_login(app: AppHandle, enabled: bool) -> Result<bool, String> {
    if crate::package::is_packaged() {
        return tauri::async_runtime::spawn_blocking(move || crate::package::set_startup_enabled(enabled))
            .await
            .map_err(|error| error.to_string())?;
    }
    let manager = app.autolaunch();
    let result = if enabled { manager.enable() } else { manager.disable() };
    result.map_err(|error| error.to_string())?;
    Ok(manager.is_enabled().unwrap_or(enabled))
}

// MARK: - Local API

#[tauri::command]
pub fn local_api_state(core: Core) -> LocalApiState {
    local_api::state(&core)
}

#[tauri::command]
pub fn set_local_api_enabled(core: Core, enabled: bool) {
    local_api::set_enabled(&core, enabled);
}

#[tauri::command]
pub fn set_local_api_port(core: Core, port: u16) -> Result<(), String> {
    local_api::set_port(&core, port)
}

#[tauri::command]
pub fn regenerate_api_token(core: Core) {
    local_api::regenerate_token(&core);
}

// MARK: - Shell

#[tauri::command]
pub fn copy_text(text: String) {
    crate::clipboard::copy(&text);
}

#[tauri::command]
pub fn open_url(app: AppHandle, url: String) -> Result<(), String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("Unsupported URL".into());
    }
    app.opener().open_url(url, None::<&str>).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn open_window(app: AppHandle, name: String) {
    if let Some(which) = AppWindow::parse(&name) {
        crate::windows::open(&app, which);
    }
}

#[tauri::command]
pub fn close_window(app: AppHandle, name: String) {
    if let Some(which) = AppWindow::parse(&name) {
        crate::windows::close(&app, which);
    }
}

#[tauri::command]
pub fn show_panel(app: AppHandle) {
    panel::show(&app);
}

#[tauri::command]
pub fn finish_onboarding(app: AppHandle) {
    crate::windows::finish_onboarding(&app);
}

#[tauri::command]
pub fn hide_panel(app: AppHandle) {
    panel::hide(&app);
}

#[tauri::command]
pub fn set_panel_showing_dialog(app: AppHandle, showing: bool) {
    panel::set_showing_dialog(&app, showing);
}

#[tauri::command]
pub fn set_panel_height(app: AppHandle, height: f64) {
    panel::set_height(&app, height);
}

#[tauri::command]
pub fn quit_app(app: AppHandle) {
    crate::quit(&app);
}

// MARK: - Updates

#[tauri::command]
pub fn update_status(core: Core) -> UpdateStatus {
    updater::status(&core)
}

#[tauri::command]
pub fn check_for_updates(app: AppHandle) {
    updater::check_now(&app);
}

#[tauri::command]
pub async fn install_update(core: Core<'_>) -> Result<(), String> {
    let core: SharedCore = core.inner().clone();
    updater::install(&core).await;
    Ok(())
}

// MARK: - Watched folders

#[tauri::command]
pub async fn watched_folders(core: Core<'_>) -> Result<crate::watched::engine::Overview, String> {
    blocking(&core, |core| Ok(core.watched.overview())).await
}

/// Checks a folder picked to be watched; refused folders come back as the
/// reason.
#[tauri::command]
pub async fn check_watch_folder(core: Core<'_>, path: String) -> Result<crate::watched::FolderCheck, String> {
    blocking(&core, move |core| crate::watched::check(&core, &path)).await
}

/// `upload_existing`: "Upload Them" rather than "Skip Existing Files".
#[tauri::command]
pub async fn add_watched_folder(core: Core<'_>, path: String, upload_existing: bool) -> Result<WatchedFolder, String> {
    blocking(&core, move |core| crate::watched::add(&core, &path, upload_existing)).await
}

#[tauri::command]
pub async fn add_screenshots_folder(core: Core<'_>) -> Result<WatchedFolder, String> {
    blocking(&core, |core| crate::watched::add_screenshots(&core)).await
}

#[tauri::command]
pub async fn save_watched_folder(core: Core<'_>, folder: WatchedFolder) -> Result<(), String> {
    blocking(&core, move |core| crate::watched::save(&core, folder)).await
}

/// Walking a folder takes a moment, so it's done off the async runtime's
/// workers (and the UI thread).
async fn blocking<T: Send + 'static>(
    core: &Core<'_>,
    work: impl FnOnce(SharedCore) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let core: SharedCore = core.inner().clone();
    tauri::async_runtime::spawn_blocking(move || work(core)).await.map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn remove_watched_folder(core: Core<'_>, id: String) -> Result<(), String> {
    blocking(&core, move |core| {
        core.watched.engine.remove(&id);
        core.watched.wake();
        Ok(())
    })
    .await
}

/// "Reset": forgets what the folder handled.
#[tauri::command]
pub async fn reset_watched_folder(core: Core<'_>, id: String) -> Result<(), String> {
    blocking(&core, move |core| {
        core.watched.engine.reset(&id);
        core.watched.wake();
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn set_watched_folder_enabled(core: Core<'_>, id: String, enabled: bool) -> Result<(), String> {
    blocking(&core, move |core| {
        core.watched.engine.set_enabled(&id, enabled);
        core.watched.wake();
        Ok(())
    })
    .await
}

/// "Upload" (true) or "Skip" on a large batch waiting for the go-ahead.
#[tauri::command]
pub async fn confirm_watched_batch(core: Core<'_>, id: String, upload: bool) -> Result<(), String> {
    let core: SharedCore = core.inner().clone();
    tauri::async_runtime::spawn_blocking(move || core.watched.engine.confirm(&id, upload)).await.map_err(|error| error.to_string())
}

/// "Delete from Bucket" (true) or "Keep Uploaded Files" on a large
/// deletion.
#[tauri::command]
pub async fn confirm_watched_deletions(core: Core<'_>, id: String, delete: bool) -> Result<(), String> {
    blocking(&core, move |core| {
        core.watched.engine.confirm_deletions(&id, delete);
        core.watched.wake();
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn upload_watched_pending(core: Core<'_>, id: String) -> Result<(), String> {
    let core: SharedCore = core.inner().clone();
    let engine_core = core.clone();
    tauri::async_runtime::spawn_blocking(move || engine_core.watched.engine.upload_pending_now(&id, Now::current()))
        .await
        .map_err(|error| error.to_string())?;
    core.watched.wake();
    Ok(())
}

#[tauri::command]
pub async fn retry_watched_failed(core: Core<'_>, id: String) -> Result<(), String> {
    let core: SharedCore = core.inner().clone();
    tauri::async_runtime::spawn_blocking(move || core.watched.engine.retry_failed(&id, Now::current()))
        .await
        .map_err(|error| error.to_string())
}

/// Pauses every folder for `minutes`, or until resumed when it's none.
/// `untilTomorrow` pauses until midnight instead.
#[tauri::command]
pub async fn pause_watching(core: Core<'_>, minutes: Option<u64>, until_tomorrow: Option<bool>) -> Result<(), String> {
    let minutes = if until_tomorrow == Some(true) { Some(crate::watched::minutes_until_tomorrow()) } else { minutes };
    blocking(&core, move |core| {
        crate::watched::pause(&core, minutes);
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn resume_watching(core: Core<'_>) -> Result<(), String> {
    blocking(&core, |core| {
        crate::watched::resume(&core);
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn set_watch_pause_conditions(core: Core<'_>, on_battery: Option<bool>, on_metered: Option<bool>) -> Result<(), String> {
    blocking(&core, move |core| {
        core.watched.engine.set_pause_conditions(on_battery, on_metered);
        core.watched.wake();
        Ok(())
    })
    .await
}

/// A hook's "Test": sends a sample upload through it.
#[tauri::command]
pub async fn test_watch_hook(core: Core<'_>, folder: WatchedFolder, hook: Hook) -> Result<(), String> {
    let core: SharedCore = core.inner().clone();
    crate::watched::test_hook(&core, folder, hook).await
}

/// "Show in Explorer" for a watched folder.
#[tauri::command]
pub fn show_folder(app: AppHandle, path: String) -> Result<(), String> {
    if !std::path::Path::new(&path).is_dir() {
        return Err(t!("That folder doesn’t exist."));
    }
    app.opener().open_path(path, None::<&str>).map_err(|error| error.to_string())
}

/// What Settings should do on opening: a tab to show, a folder to add.
#[tauri::command]
pub fn take_settings_request(core: Core) -> crate::watched::SettingsRequest {
    crate::watched::take_request(&core)
}
