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
use crate::storage::{BucketListing, BucketObject, ConnectionResult, S3Provider};
use crate::transfer::{self, TransferError};
use crate::updater::{self, UpdateStatus};
use crate::uploads::{self, JobSnapshot, NameRequest, UploadInput};
use crate::watched::engine::Now;
use crate::watched::model::{Hook, WatchedFolder};
use crate::windows::AppWindow;
use crate::thumbnails::{self, ThumbnailMode};
use crate::{bucket, cloudflare_setup, i18n, panel, t};

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
            cloudflare_token: None,
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
///
/// `cloudflare_token`: none keeps the stored one, an empty one removes it.
/// A destination saved without a Cloudflare zone ID loses its token.
#[tauri::command]
pub fn save_destination(
    core: Core,
    mut config: DestinationConfig,
    credentials: Option<StorageCredentials>,
    rules: Option<FormRules>,
    delete_old_thumbnails: Option<bool>,
    cloudflare_token: Option<String>,
) -> Result<DestinationConfig, String> {
    let is_new = config.id.is_empty();
    if is_new {
        config.id = crate::util::new_id();
    }
    check_destination_hooks(&core, &config)?;
    // Read before saving replaces them: the old folder may be in another
    // bucket, with other keys.
    let cleanup = delete_old_thumbnails
        .unwrap_or(false)
        .then(|| old_thumbnail_prefix(&core, &config))
        .flatten()
        .and_then(|(saved, prefix)| credentials::load(&saved.id).ok().map(|keys| (saved, prefix, keys)));
    let saved = store_destination(&core, config, credentials, rules, is_new)?;
    // Without a zone ID the token has nothing to purge, so it goes too.
    let token = if saved.cloudflare_zone_id.is_none() { Some(String::new()) } else { cloudflare_token };
    if let Some(token) = token {
        let mut keys = credentials::load(&saved.id).map_err(|error| error.to_string())?;
        let token = Some(token.trim().to_string()).filter(|token| !token.is_empty());
        if keys.cloudflare_token != token {
            keys.cloudflare_token = token;
            credentials::save(&keys, &saved.id).map_err(|error| error.to_string())?;
        }
    }
    if let Some((old, prefix, keys)) = cleanup {
        let core = core.inner().clone();
        tauri::async_runtime::spawn(async move {
            let name = old.name.clone();
            match thumbnails::bucket::delete_all(&S3Provider::new(old, keys), &prefix).await {
                Ok(()) => uploads::show_notification(&core, &t!("Deleted the thumbnails of {0} from the bucket", name), ""),
                Err(error) => uploads::show_notification(
                    &core,
                    &t!("Couldn't delete the thumbnails of {0} from the bucket", name),
                    &error.to_string(),
                ),
            }
        });
    }
    Ok(saved)
}

/// Adds `config` under its own ID when `is_new`, or updates the one with
/// that ID.
fn store_destination(
    core: &Core,
    config: DestinationConfig,
    credentials: Option<StorageCredentials>,
    rules: Option<FormRules>,
    is_new: bool,
) -> Result<DestinationConfig, String> {
    let credentials = filled(credentials);
    // A result about another bucket (the connection fields were edited
    // after the check) says nothing about this one.
    let rules = match rules.unwrap_or_default() {
        FormRules::Checked { connection: Some(connection), .. } if !connection.is_for(&config) => FormRules::NotChecked,
        rules => rules,
    };
    let reconnected = !is_new && !is_saved_connection(core, &config, credentials.as_ref());
    if is_new {
        let Some(credentials) = credentials else {
            return Err(t!("Enter an Access Key ID and Secret Access Key."));
        };
        credentials::save(&credentials, &config.id).map_err(|error| error.to_string())?;
        core.destinations.add(config.clone());
    } else {
        if let Some(mut credentials) = credentials {
            // New keys keep the Cloudflare token saved with the old ones.
            credentials.cloudflare_token = credentials::load(&config.id).ok().and_then(|keys| keys.cloudflare_token);
            credentials::save(&credentials, &config.id).map_err(|error| error.to_string())?;
        }
        let old = core.destinations.find(Some(&config.id)).filter(|saved| saved.id == config.id);
        core.destinations.update(config.clone());
        if let (Some(old), Some(new)) = (old, core.destinations.find(Some(&config.id))) {
            destination_updated(core, &old, &new);
        }
    }
    match rules {
        FormRules::Checked { check, .. } => expiry::record(core, &config.id, check),
        // Rows from the old bucket shouldn't expire against the new one.
        FormRules::NotChecked if reconnected => expiry::record(core, &config.id, None),
        FormRules::NotChecked => {}
    }
    core.notify(events::DESTINATIONS_CHANGED);
    Ok(config)
}

/// Refuses "After Upload" hooks the windows made up: a script that wasn't
/// picked in Aktar's own dialog (or saved with this destination before,
/// exactly as it is), and a webhook that would send the upload's details
/// over plain http:// across the internet.
fn check_destination_hooks(core: &SharedCore, config: &DestinationConfig) -> Result<(), String> {
    let saved = core.destinations.find(Some(&config.id)).filter(|saved| saved.id == config.id).and_then(|saved| saved.hooks).unwrap_or_default();
    for hook in config.hooks.iter().flatten() {
        match hook.kind {
            crate::watched::model::HookKind::Webhook => crate::watched::check_webhook_url(&hook.target)?,
            crate::watched::model::HookKind::Script => {
                let known = saved.iter().any(|saved| saved.id == hook.id && saved.kind == hook.kind && saved.target == hook.target);
                if !known && !crate::watched::is_picked_script(core, hook) {
                    return Err(t!("Pick the script with “Add Script…”."));
                }
            }
        }
    }
    Ok(())
}

/// Whether the destination has a Cloudflare token saved with its keys, for
/// the form (which never sees the token itself).
#[tauri::command]
pub fn has_cloudflare_token(id: String) -> bool {
    credentials::load(&id).is_ok_and(|keys| keys.cloudflare_token.is_some_and(|token| !token.is_empty()))
}

/// "Check" next to the Cloudflare token: the one typed, or the saved one.
#[tauri::command]
pub async fn check_cloudflare_token(destination_id: Option<String>, token: Option<String>) -> Result<(), String> {
    let token = token
        .filter(|token| !token.trim().is_empty())
        .or_else(|| destination_id.and_then(|id| credentials::load(&id).ok()).and_then(|keys| keys.cloudflare_token))
        .unwrap_or_default();
    crate::cloudflare::verify(&token).await
}

/// "Test" on a destination's hook: a made-up upload.
#[tauri::command]
pub async fn test_destination_hook(core: Core<'_>, destination: DestinationConfig, hook: Hook) -> Result<(), String> {
    let config = DestinationConfig { hooks: Some(vec![hook.clone()]), ..destination };
    check_destination_hooks(&core, &config)?;
    crate::destination_hooks::test(&core, &config, &hook).await
}

/// A destination's own "Upload clipboard to this destination" shortcut.
#[tauri::command]
pub fn set_destination_shortcut(app: AppHandle, core: Core, id: String, accelerator: Option<String>) -> Result<(), String> {
    crate::hotkey::set_destination(&app, &id, accelerator.as_deref())?;
    core.notify(events::SETTINGS_CHANGED);
    Ok(())
}

/// Under the panel's destination picker: where files go when a destination
/// other than the default claims them ("Images go to Screenshots").
#[tauri::command]
pub fn routing_hints(core: Core) -> Vec<String> {
    let default = core.destinations.default_destination().map(|destination| destination.id);
    crate::routing::hints(&core.destinations.all(), default.as_deref())
}

/// Thumbnails turned off: the ones made for this destination go, here and
/// in the bucket view's cache. Pointed at another bucket: what was made
/// from the old one's files no longer applies. (Thumbnails saved in the
/// bucket are left there; the form asks about those.)
fn destination_updated(core: &SharedCore, old: &DestinationConfig, new: &DestinationConfig) {
    let off = new.thumbnail_mode() == ThumbnailMode::Off;
    if off && old.thumbnail_mode() != ThumbnailMode::Off {
        core.history.remove_thumbnails(&new.id);
        core.notify(events::HISTORY_CHANGED);
    }
    if off
        || !thumbnails::same_bucket(old, new)
        || old.thumbnail_mode() != new.thumbnail_mode()
        || old.bucket_thumbnail_prefix() != new.bucket_thumbnail_prefix()
    {
        thumbnails::remote::forget_destination(core, &new.id);
    }
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

// MARK: - Transfer to another device

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferShare {
    link: String,
    /// "XXXX-XXXX-XXXX", shown next to the QR code and never part of the link.
    code: String,
}

/// "Share to Another Device": a fresh code and link for the destination,
/// with its keys from Credential Manager. Nothing is kept: closing the
/// window forgets the code.
#[tauri::command]
pub async fn create_transfer(core: Core<'_>, destination_id: String) -> Result<TransferShare, String> {
    let destination = core
        .destinations
        .find(Some(&destination_id))
        .filter(|destination| destination.id == destination_id)
        .ok_or_else(|| t!("No destination to upload to. Add one in Settings."))?;
    let custom_template = Some(core.settings.get().custom_template);
    tauri::async_runtime::spawn_blocking(move || {
        let credentials = credentials::load(&destination.id).map_err(|error| error.to_string())?;
        let payload = transfer::TransferPayload { destination, credentials, custom_template };
        let code = transfer::generate_code();
        let link = transfer::seal(&payload, &code)?;
        Ok(TransferShare { link, code: transfer::display_code(&code) })
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Whether a pasted or scanned link can be opened, before the code is
/// asked for.
#[tauri::command]
pub fn check_transfer_link(link: String) -> Result<(), TransferError> {
    transfer::envelope(&link).map(|_| ())
}

/// The destination and keys in a transfer link. Nothing is saved:
/// `import_destination` saves them once the code was right.
#[tauri::command]
pub async fn open_transfer(link: String, code: String) -> Result<transfer::TransferPayload, TransferError> {
    tauri::async_runtime::spawn_blocking(move || transfer::open(&link, &code))
        .await
        .map_err(|_| TransferError::NotTransfer)?
}

/// "Import from Another Device", once the code was right: adds the
/// destination under the ID it had on the other device, or updates the one
/// here with that ID ("Update Existing"), keys included. "Add as Copy"
/// sends a new ID. The app-level template comes along for a destination
/// that copies with it, unless this app's own was already changed.
/// Returns the destination as saved, default flag included.
#[tauri::command]
pub fn import_destination(
    core: Core,
    mut config: DestinationConfig,
    credentials: StorageCredentials,
    custom_template: Option<String>,
) -> Result<DestinationConfig, String> {
    let Ok(id) = uuid::Uuid::parse_str(config.id.trim()) else {
        return Err(t!("This isn’t an Aktar transfer link."));
    };
    config.id = id.hyphenated().to_string().to_uppercase();
    let existing = core.destinations.find(Some(&config.id)).filter(|existing| existing.id.eq_ignore_ascii_case(&config.id));
    if let Some(existing) = &existing {
        config.id = existing.id.clone();
    }
    let custom = config.output_mode == Some(crate::output::OutputMode::Custom);
    let saved = store_destination(&core, config, Some(credentials), None, existing.is_none())?;
    if let Some(template) = custom_template.filter(|template| custom && !template.is_empty()) {
        if core.settings.get().custom_template == Settings::default().custom_template {
            core.settings.update(|settings| settings.custom_template = template);
            core.notify(events::SETTINGS_CHANGED);
        }
    }
    Ok(core.destinations.find(Some(&saved.id)).unwrap_or(saved))
}

// MARK: - Set Up Cloudflare R2

/// "Open Cloudflare": the token page with the permissions filled in.
#[tauri::command]
pub fn open_cloudflare_token_page(app: AppHandle) -> Result<(), String> {
    app.opener().open_url(cloudflare_setup::token_url(), None::<&str>).map_err(|error| error.to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudflareToken {
    /// Also the S3 access key ID.
    token_id: String,
    accounts: Vec<cloudflare_setup::Account>,
}

/// The pasted token's ID and the accounts it can see.
#[tauri::command]
pub async fn cloudflare_check_token(token: String) -> Result<CloudflareToken, String> {
    let token_id = cloudflare_setup::verify(&token).await?;
    let accounts = cloudflare_setup::accounts(&token).await?;
    if accounts.is_empty() {
        return Err(t!("This token can’t see any Cloudflare account. Create it with the button above, for all accounts."));
    }
    Ok(CloudflareToken { token_id, accounts })
}

#[derive(Serialize)]
pub struct CloudflareAccount {
    buckets: Vec<String>,
    zones: Vec<cloudflare_setup::Zone>,
}

/// An account's buckets and domains. Without its domains the r2.dev
/// address still works, so only the buckets can fail this.
#[tauri::command]
pub async fn cloudflare_account(token: String, account_id: String) -> Result<CloudflareAccount, String> {
    let (buckets, zones) = tokio::join!(
        cloudflare_setup::buckets(&account_id, &token),
        cloudflare_setup::zones(&account_id, &token),
    );
    Ok(CloudflareAccount { buckets: buckets?, zones: zones.unwrap_or_default() })
}

#[tauri::command]
pub async fn cloudflare_create_bucket(token: String, account_id: String, bucket: String) -> Result<(), String> {
    cloudflare_setup::create_bucket(&bucket, &account_id, &token).await
}

/// Turns on public links for the bucket: its r2.dev address, or `domain`
/// on `zone` (left as it is when it's already connected). Returns the
/// public base URL.
#[tauri::command]
pub async fn cloudflare_enable_public_links(
    token: String,
    account_id: String,
    bucket: String,
    zone: Option<cloudflare_setup::Zone>,
    domain: Option<String>,
) -> Result<String, String> {
    let (Some(zone), Some(domain)) = (zone, domain) else {
        return cloudflare_setup::enable_public_dev_url(&bucket, &account_id, &token).await;
    };
    let domain = domain.trim().to_ascii_lowercase();
    if !cloudflare_setup::is_valid_domain(&domain, &zone.name) {
        return Err(t!("Cloudflare refused the request."));
    }
    let connected = cloudflare_setup::custom_domains(&bucket, &account_id, &token).await?;
    if !connected.iter().any(|existing| existing.eq_ignore_ascii_case(&domain)) {
        cloudflare_setup::attach_domain(&domain, &zone, &bucket, &account_id, &token).await?;
    }
    Ok(format!("https://{domain}"))
}

/// Saves the set-up bucket as a normal R2 destination, with the keys made
/// from the token (the token itself isn't kept). Its auto-delete rules
/// aren't checked yet. Returns it as saved, default flag included.
/// `own_domain` says the links are on a domain of the user's own.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub fn cloudflare_save_destination(
    core: Core,
    token: String,
    token_id: String,
    account_id: String,
    bucket: String,
    public_base_url: String,
    name: String,
    own_domain: bool,
) -> Result<DestinationConfig, String> {
    let name = Some(name.trim()).filter(|name| !name.is_empty()).unwrap_or("Cloudflare R2");
    let config = cloudflare_setup::destination(name, account_id.trim(), bucket.trim(), &public_base_url, own_domain);
    let credentials = cloudflare_setup::credentials(token_id.trim(), token.trim());
    let saved = store_destination(&core, config, Some(credentials), None, true)?;
    Ok(core.destinations.find(Some(&saved.id)).unwrap_or(saved))
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
    let _ = crate::hotkey::set_destination(&core.app, &id, None);
    core.destinations.remove(&id);
    thumbnails::remote::forget_destination(&core, &id);
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

/// The files and folders a window asked to upload. Anything inside
/// Aktar's own folders (settings, history, thumbnails, the files it stages
/// for uploads) is left out: a window never uploads those.
fn inputs_from(core: &SharedCore, paths: Vec<String>) -> Vec<UploadInput> {
    let own: Vec<PathBuf> = crate::watched::app_dirs(core)
        .into_iter()
        .chain([std::env::temp_dir().join("Aktar"), std::env::temp_dir().join("AktarLocalAPI")])
        .filter_map(|dir| canonical(&dir))
        .collect();
    paths
        .into_iter()
        .map(PathBuf::from)
        .filter(|path| path.is_file() || path.is_dir())
        .filter(|path| {
            let inside = canonical(path).is_some_and(|path| own.iter().any(|dir| path.starts_with(dir)));
            if inside {
                log::warn!("Refused to upload {} from Aktar's own folders", path.display());
            }
            !inside
        })
        .map(UploadInput::from_path)
        .collect()
}

/// "Replace File…" on a history entry: `path` goes up to its key, so its
/// link keeps working.
#[tauri::command]
pub fn replace_upload(core: Core, id: String, path: String) -> Result<(), String> {
    let path = replacement(&core, path)?;
    uploads::replace_upload(&core, &id, path, None).map(|_| ())
}

/// "Replace File…" on a file in the bucket view.
#[tauri::command]
pub fn replace_object(core: Core, destination_id: String, key: String, path: String) -> Result<(), String> {
    let destination = core
        .destinations
        .find(Some(&destination_id))
        .filter(|destination| destination.id.eq_ignore_ascii_case(&destination_id))
        .ok_or_else(|| t!("This upload’s destination was removed."))?;
    bucket::check_key(&key)?;
    let path = replacement(&core, path)?;
    uploads::replace_object(&core, destination, key, path, None).map(|_| ())
}

/// The file a window picked to replace another with, unless it's a folder
/// or inside Aktar's own folders.
fn replacement(core: &SharedCore, path: String) -> Result<PathBuf, String> {
    inputs_from(core, vec![path])
        .into_iter()
        .map(|input| input.path)
        .find(|path| path.is_file())
        .ok_or_else(|| t!("Choose a file to replace it with."))
}

/// The path with every link resolved, or None when it doesn't exist.
fn canonical(path: &std::path::Path) -> Option<PathBuf> {
    std::fs::canonicalize(path).ok()
}

/// Returns how many of `paths` are files or folders that could be queued.
/// With no destination set up, nothing is queued and the user is told why.
/// With `rename`, each file first waits for its name in the panel.
#[tauri::command]
pub fn upload_files(core: Core, paths: Vec<String>, destination_id: Option<String>, rename: Option<bool>) -> usize {
    let destination = destination_id.and_then(|id| core.destinations.find(Some(&id)));
    let inputs = inputs_from(&core, paths);
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

/// The thumbnail of a file in the bucket view, as a data URL, or none (a
/// file icon). See `thumbnails::remote`.
#[tauri::command]
pub async fn bucket_thumbnail(core: Core<'_>, destination_id: String, object: BucketObject) -> Result<Option<String>, ()> {
    let Some(destination) = core.destinations.find(Some(&destination_id)) else { return Ok(None) };
    let prefixes = thumbnails::bucket_prefixes(&destination, &core.destinations.all());
    let data = thumbnails::remote::for_object(&core, &destination, &object, &prefixes, true).await;
    Ok(data.map(|data| {
        use base64::Engine as _;
        let mime = if thumbnails::is_webp(&data) { "image/webp" } else { "image/png" };
        format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(data))
    }))
}

/// Makes or fetches the thumbnail of a history entry that has none; true
/// when there's one now (History is told to refresh).
#[tauri::command]
pub async fn load_record_thumbnail(core: Core<'_>, id: String) -> Result<bool, ()> {
    let made = thumbnails::remote::for_record(&core, &id).await;
    if made {
        core.notify(events::HISTORY_CHANGED);
    }
    Ok(made)
}

/// Settings > General: the space thumbnails take on this PC, in bytes.
#[tauri::command]
pub async fn thumbnail_usage(core: Core<'_>) -> Result<u64, ()> {
    let folders = [core.history.thumbnails.directory().to_path_buf(), core.bucket_thumbnails.clone()];
    Ok(tauri::async_runtime::spawn_blocking(move || folders.iter().map(|folder| thumbnails::disk_usage(folder)).sum())
        .await
        .unwrap_or_default())
}

/// Settings > General > Clear: every thumbnail on this PC. They're made
/// again when shown; the ones in buckets stay.
#[tauri::command]
pub fn clear_thumbnails(core: Core) {
    core.history.thumbnails.remove_all();
    thumbnails::remote::forget_all(&core);
    core.notify(events::THUMBNAILS_CLEARED);
    core.notify(events::HISTORY_CHANGED);
}

/// The bucket folder a saved destination keeps thumbnails in, when saving
/// `config` would stop using it (thumbnails off the bucket, in another
/// folder, or in another bucket) and no other destination on that bucket
/// still uses it: the destination form then asks whether to delete them.
#[tauri::command]
pub fn thumbnail_cleanup_prefix(core: Core, config: DestinationConfig) -> Option<String> {
    old_thumbnail_prefix(&core, &config).map(|(_, prefix)| prefix)
}

fn old_thumbnail_prefix(core: &SharedCore, config: &DestinationConfig) -> Option<(DestinationConfig, String)> {
    let mut config = config.clone();
    config.sanitize();
    let saved = core.destinations.find(Some(&config.id)).filter(|saved| saved.id == config.id)?;
    let old = saved.bucket_thumbnail_prefix()?;
    if config.bucket_thumbnail_prefix().as_ref() == Some(&old) && thumbnails::same_bucket(&saved, &config) {
        return None;
    }
    let others_use_it = core.destinations.all().iter().any(|other| {
        other.id != saved.id && thumbnails::same_bucket(other, &saved) && other.bucket_thumbnail_prefix().as_ref() == Some(&old)
    });
    (!others_use_it).then_some((saved, old))
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
    let (destination, storage) = storage_for(&core, &destination_id)?;
    let result = if recursive {
        storage.list_recursively(&prefix, continuation_token).await
    } else {
        storage.list(&prefix, continuation_token).await
    };
    let mut listing = result.map_err(|error| error.to_string())?;
    // Thumbnail folders are Aktar's own, hidden like in the Mac app.
    let hidden = thumbnails::bucket_prefixes(&destination, &core.destinations.all());
    listing.folders.retain(|folder| !thumbnails::is_hidden_folder(folder, &hidden));
    listing.objects.retain(|object| !thumbnails::is_thumbnail(&object.key, &hidden));
    Ok(listing)
}

#[tauri::command]
pub async fn bucket_delete(core: Core<'_>, destination_id: String, key: String) -> Result<(), String> {
    let (destination, storage) = storage_for(&core, &destination_id)?;
    let prefixes = thumbnails::bucket_prefixes(&destination, &core.destinations.all());
    bucket::delete_object(&storage, &key, &prefixes).await.map_err(|error| error.to_string())?;
    thumbnails::remote::forget(&core, &destination.id, &key);
    core.history.object_deleted(&key, &destination.id);
    core.notify(events::HISTORY_CHANGED);
    Ok(())
}

/// Renames or moves an object; `new_key` is a full key, so changing the
/// folder part moves it. Returns the cleaned-up key it ended up at.
#[tauri::command]
pub async fn bucket_move(core: Core<'_>, destination_id: String, from: String, to: String) -> Result<String, String> {
    let new_key = to.trim().trim_end_matches('/').to_string();
    if new_key.is_empty() || new_key == from {
        return Ok(from);
    }
    bucket::check_key(&new_key)?;
    let (destination, storage) = storage_for(&core, &destination_id)?;
    let prefixes = thumbnails::bucket_prefixes(&destination, &core.destinations.all());
    match bucket::move_object(&storage, &from, &new_key, &prefixes).await {
        Ok(()) => {
            thumbnails::remote::forget(&core, &destination.id, &from);
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
    let name = name.trim().trim_end_matches('/');
    if name.is_empty() {
        return Err(t!("The folder name is required."));
    }
    bucket::check_key(name)?;
    let folder = format!("{}{name}/", bucket::checked_folder(&prefix)?);
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
    let prefix = bucket::checked_folder(&prefix)?;
    let (destination, storage) = storage_for(&core, &destination_id)?;
    let mut claimed: Vec<String> = Vec::new();
    let mut inputs = Vec::new();
    for input in inputs_from(&core, paths) {
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
            // Taken by something else in the meantime: another free name.
            file.keep_existing = true;
            inputs.push(file);
        }
    }
    let count = inputs.len();
    uploads::enqueue(&core, inputs, Some(destination));
    Ok(count)
}

/// Downloads a file for an inline preview (PDF, text, Markdown). Going
/// through Rust avoids the CORS rules a fetch from the webview would hit.
/// Kept in memory only, and never more than 25 MB: a bigger file (by its
/// Content-Length, or as it arrives) is "Preview unavailable". Redirects
/// are only followed on the same host.
#[tauri::command]
pub async fn fetch_remote(url: String) -> Result<tauri::ipc::Response, String> {
    const MAX_BYTES: usize = 25 * 1024 * 1024;
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("Unsupported URL".into());
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let same_host = attempt.previous().first().is_some_and(|first| first.host_str() == attempt.url().host_str());
            if attempt.previous().len() > 5 || !same_host {
                attempt.stop()
            } else {
                attempt.follow()
            }
        }))
        .build()
        .map_err(|error| error.to_string())?;
    let mut response = client.get(&url).send().await.map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(response.status().to_string());
    }
    if response.content_length().is_some_and(|length| length > MAX_BYTES as u64) {
        return Err(t!("Preview unavailable"));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| error.to_string())? {
        if bytes.len() + chunk.len() > MAX_BYTES {
            return Err(t!("Preview unavailable"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(tauri::ipc::Response::new(bytes))
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

/// Saves the QR code as a PNG where the user picks in a save dialog this
/// opens itself, so the window never says where to write: it only suggests
/// the file's name. False when the dialog was cancelled.
#[tauri::command]
pub async fn save_qr_image(window: tauri::WebviewWindow, text: String, file_name: String) -> Result<bool, String> {
    use tauri_plugin_dialog::DialogExt;
    let data = crate::qr::png(&text)?;
    let file_name = std::path::Path::new(&file_name)
        .file_name()
        .map_or_else(|| "QR.png".to_string(), |name| name.to_string_lossy().into_owned());
    let (sender, receiver) = tokio::sync::oneshot::channel();
    window
        .dialog()
        .file()
        .set_parent(&window)
        .set_file_name(file_name)
        .add_filter("PNG", &["png"])
        .save_file(move |path| {
            let _ = sender.send(path);
        });
    let Some(path) = receiver.await.map_err(|error| error.to_string())? else { return Ok(false) };
    let path = path.into_path().map_err(|error| error.to_string())?;
    tokio::fs::write(path, data).await.map_err(|error| error.to_string())?;
    Ok(true)
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
    if let Err(message) = refuse_destination_shortcut(&previous, accelerator.as_deref()) {
        let _ = crate::hotkey::register(&app, previous.shortcut.as_deref(), previous.rename_shortcut.as_deref());
        return Err(message);
    }
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
    if let Err(message) = refuse_destination_shortcut(&previous, accelerator.as_deref()) {
        let _ = crate::hotkey::register(&app, previous.shortcut.as_deref(), previous.rename_shortcut.as_deref());
        return Err(message);
    }
    if let Err(message) = crate::hotkey::register(&app, previous.shortcut.as_deref(), accelerator.as_deref()) {
        let _ = crate::hotkey::register(&app, previous.shortcut.as_deref(), previous.rename_shortcut.as_deref());
        return Err(message);
    }
    core.settings.update(|settings| settings.rename_shortcut = accelerator);
    core.notify(events::SETTINGS_CHANGED);
    Ok(())
}

/// An app shortcut can't be one a destination already has: pressing it
/// would upload to that destination instead.
fn refuse_destination_shortcut(settings: &Settings, accelerator: Option<&str>) -> Result<(), String> {
    let Some(shortcut) = accelerator.and_then(|accelerator| crate::hotkey::parse(accelerator).ok()) else { return Ok(()) };
    let taken = settings.destination_shortcuts.values().any(|other| crate::hotkey::parse(other).ok() == Some(shortcut));
    if taken {
        return Err(t!("This shortcut is already in use. Try a different one."));
    }
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

/// Copies a transfer link or the API token, kept out of clipboard history.
#[tauri::command]
pub fn copy_secret(text: String) -> Result<(), String> {
    crate::clipboard::copy_concealed(&text)
}

/// Empties the clipboard if it still holds `text` (a transfer link the
/// closing window copied).
#[tauri::command]
pub fn clear_clipboard_if(text: String) {
    crate::clipboard::clear_if_holding(&text);
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

/// "Add Script…": the script is picked in a file dialog this opens itself.
/// The new hook, or None when the dialog was cancelled.
#[tauri::command]
pub async fn pick_watch_script(core: Core<'_>, window: tauri::WebviewWindow) -> Result<Option<Hook>, String> {
    let core: SharedCore = core.inner().clone();
    crate::watched::pick_script(&core, &window).await
}

/// Whether a webhook address can be used, for the "Add Webhook" dialog.
#[tauri::command]
pub fn check_webhook_url(url: String) -> Result<(), String> {
    crate::watched::check_webhook_url(&url)
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
