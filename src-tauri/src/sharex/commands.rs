//! The windows' side of "Import from ShareX": find or pick ShareX's
//! settings, review what was found, import it. What was read stays here
//! between the review and the import, so the secret keys never reach the
//! windows.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::State;

use super::{Candidate, HistoryEntry, Parsed, Places, Secret, Skipped, UploaderKind};
use crate::core::{events, SharedCore};
use crate::credentials::{self, StorageCredentials};
use crate::destinations::{DestinationConfig, ProviderPreset};
use crate::history::NewRecord;
use crate::short_links::definition::{ShortLinkSettings, CUSTOM_ID};
use crate::short_links::sharex::SecretLocation;
use crate::t;

type Core<'a> = State<'a, SharedCore>;

/// What the last scan found, until it's imported or another scan starts.
struct Pending {
    destinations: Vec<Candidate>,
    shorteners: Vec<super::ShortenerCandidate>,
    history: Vec<HistoryEntry>,
}

static PENDING: Mutex<Option<Pending>> = Mutex::new(None);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareXStatus {
    /// ShareX's personal folder, when it's on this PC.
    pub folder: Option<String>,
    /// Its settings are there to import.
    pub has_settings: bool,
    /// The panel offers the import once: ShareX is there, there's no
    /// destination yet, and the offer wasn't answered.
    pub show_banner: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewDestination {
    pub id: String,
    pub kind: UploaderKind,
    pub name: String,
    pub preset: ProviderPreset,
    pub bucket: String,
    pub endpoint: String,
    #[serde(rename = "publicBaseURL")]
    pub public_base_url: String,
    pub template: String,
    pub notes: Vec<String>,
    /// The secret key couldn't be read: the review asks for it.
    pub needs_secret: bool,
    /// The S3 endpoint isn't known (Backblaze didn't answer): asked for too.
    pub needs_endpoint: bool,
    /// The same endpoint, bucket and access key are already in Aktar.
    pub already_in_aktar: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewShortener {
    pub id: String,
    pub name: String,
    pub host: String,
    pub method: String,
    pub endpoint: String,
    #[serde(rename = "usesHTTP")]
    pub uses_http: bool,
    pub deletion_skipped: bool,
    pub has_token: bool,
    pub secret_locations: Vec<SecretLocation>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareXReview {
    /// The folder or file it was read from.
    pub source: String,
    pub destinations: Vec<ReviewDestination>,
    pub shorteners: Vec<ReviewShortener>,
    /// History entries under one of these destinations' URLs.
    pub history_count: usize,
    /// The other entries (Imgur, other hosts).
    pub history_skipped: usize,
    pub skipped: Vec<Skipped>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DestinationChoice {
    pub id: String,
    /// Typed in the review when ShareX's couldn't be read.
    #[serde(default)]
    pub secret_access_key: Option<String>,
    #[serde(default)]
    pub endpoint: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShortenerChoice {
    pub id: String,
    /// A destination of the review (its `id`) or one already in Aktar.
    pub destination: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportChoices {
    pub destinations: Vec<DestinationChoice>,
    #[serde(default)]
    pub shorteners: Vec<ShortenerChoice>,
    #[serde(default)]
    pub history: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportFailure {
    pub name: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareXImported {
    /// As saved, for Test Connection.
    pub destinations: Vec<DestinationConfig>,
    pub failures: Vec<ImportFailure>,
    pub shorteners: usize,
    pub history_imported: usize,
    /// Already in history (imported before).
    pub history_duplicates: usize,
}

fn locate_here() -> Option<super::Located> {
    let places = Places::of_this_pc();
    let folder = super::personal_folder(&places)?;
    super::locate(&folder, super::platform::machine_name().as_deref(), &places)
}

#[tauri::command]
pub fn sharex_status(core: Core) -> ShareXStatus {
    let places = Places::of_this_pc();
    let folder = super::personal_folder(&places);
    let has_settings = folder.as_deref().is_some_and(|folder| super::locate(folder, super::platform::machine_name().as_deref(), &places).is_some());
    let show_banner = has_settings && core.destinations.all().is_empty() && !core.settings.get().sharex_offer_answered;
    ShareXStatus { folder: folder.map(|folder| folder.to_string_lossy().into_owned()), has_settings, show_banner }
}

/// The panel's offer was answered (or the import was opened): it's not
/// shown again.
#[tauri::command]
pub fn sharex_dismiss_offer(core: Core) {
    core.settings.update(|settings| settings.sharex_offer_answered = true);
    core.notify(events::SETTINGS_CHANGED);
}

/// The panel's "Import": Settings, on Destinations, with the import open.
#[tauri::command]
pub fn sharex_open_import(core: Core) {
    core.settings.update(|settings| settings.sharex_offer_answered = true);
    core.notify(events::SETTINGS_CHANGED);
    crate::watched::show_sharex_import(&core);
}

/// Reads the settings found in ShareX's folder. None when there are none.
#[tauri::command]
pub async fn sharex_scan(core: Core<'_>) -> Result<Option<ShareXReview>, String> {
    let Some(located) = tauri::async_runtime::spawn_blocking(locate_here).await.map_err(|error| error.to_string())? else { return Ok(None) };
    let source = located.folder.to_string_lossy().into_owned();
    scan(&core, located.uploaders_config, None, located.history, source).await.map(Some)
}

/// "Choose File…": ShareX's UploadersConfig.json or a `.sxb` backup. None
/// when the dialog was cancelled.
#[tauri::command]
pub async fn sharex_pick_file(window: tauri::WebviewWindow, core: Core<'_>) -> Result<Option<ShareXReview>, String> {
    use tauri_plugin_dialog::DialogExt;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    window
        .dialog()
        .file()
        .set_parent(&window)
        .set_title(t!("Choose ShareX’s UploadersConfig.json or a ShareX backup (.sxb)."))
        .add_filter("ShareX", &["json", "sxb"])
        .pick_file(move |path| {
            let _ = sender.send(path);
        });
    let Some(path) = receiver.await.ok().flatten() else { return Ok(None) };
    let path = path.into_path().map_err(|_| t!("This isn’t ShareX’s UploadersConfig.json or a ShareX backup (.sxb)."))?;
    let picked = path.clone();
    let text = tauri::async_runtime::spawn_blocking(move || super::read_picked(&picked)).await.map_err(|error| error.to_string())??;
    // A settings file in ShareX's folder has the history next to it.
    let history = path
        .parent()
        .filter(|_| !path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("sxb")))
        .and_then(|folder| ["History.db", "History.json"].iter().map(|name| folder.join(name)).find(|path| path.is_file()));
    let source = path.to_string_lossy().into_owned();
    scan(&core, path, Some(text), history, source).await.map(Some)
}

async fn scan(core: &SharedCore, config: PathBuf, text: Option<String>, history: Option<PathBuf>, source: String) -> Result<ShareXReview, String> {
    let (mut parsed, history) = tauri::async_runtime::spawn_blocking(move || -> Result<(Parsed, Vec<HistoryEntry>), String> {
        let text = match text {
            Some(text) => text,
            None => super::read_text(&config, super::MAX_CONFIG_BYTES)?,
        };
        let parsed = super::parse_uploaders_config(&text, &super::platform::unprotect)?;
        let history = match history {
            Some(path) => super::read_history(&path).unwrap_or_else(|error| {
                log::warn!("Could not read ShareX's history: {error}");
                Vec::new()
            }),
            None => Vec::new(),
        };
        Ok((parsed, history))
    })
    .await
    .map_err(|error| error.to_string())??;
    for candidate in parsed.destinations.iter_mut().filter(|candidate| candidate.kind == UploaderKind::BackblazeB2) {
        authorize(candidate).await;
    }
    let review = review(core, &parsed, &history, source);
    *PENDING.lock().unwrap() = Some(Pending { destinations: parsed.destinations, shorteners: parsed.shorteners, history });
    Ok(review)
}

/// Asks Backblaze for a B2 destination's endpoint, when its key is known.
async fn authorize(candidate: &mut Candidate) -> bool {
    let Secret::Known(key) = &candidate.secret else { return false };
    match super::authorize_b2(&candidate.access_key_id, key).await {
        Ok(authorization) => {
            super::apply_b2_authorization(candidate, &authorization);
            true
        }
        Err(error) => {
            candidate.notes.push(t!("Backblaze didn’t give the S3 endpoint ({0}). Enter it below, such as https://s3.us-west-004.backblazeb2.com.", error));
            false
        }
    }
}

/// The destination already in Aktar with the same endpoint, bucket and
/// access key, if there is one. A B2 one whose endpoint isn't known yet is
/// compared by bucket and key.
fn existing_match(candidate: &Candidate, existing: &[(DestinationConfig, Option<String>)]) -> Option<DestinationConfig> {
    let endpoint = super::comparable_endpoint(&candidate.config.endpoint);
    existing
        .iter()
        .find(|(destination, access_key)| {
            (endpoint.is_empty() || super::comparable_endpoint(&destination.endpoint) == endpoint)
                && destination.bucket.eq_ignore_ascii_case(&candidate.config.bucket)
                && access_key.as_deref() == Some(candidate.access_key_id.as_str())
        })
        .map(|(destination, _)| destination.clone())
}

fn existing_with_keys(core: &SharedCore) -> Vec<(DestinationConfig, Option<String>)> {
    core.destinations.all().into_iter().map(|destination| {
        let key = credentials::load(&destination.id).ok().map(|keys| keys.access_key_id);
        (destination, key)
    }).collect()
}

fn review(core: &SharedCore, parsed: &Parsed, history: &[HistoryEntry], source: String) -> ShareXReview {
    let existing = existing_with_keys(core);
    let destinations = parsed
        .destinations
        .iter()
        .map(|candidate| ReviewDestination {
            id: candidate.review_id.clone(),
            kind: candidate.kind,
            name: candidate.config.name.clone(),
            preset: candidate.config.preset,
            bucket: candidate.config.bucket.clone(),
            endpoint: candidate.config.endpoint.clone(),
            public_base_url: candidate.config.public_base_url.clone(),
            template: candidate.config.object_path_template.clone(),
            notes: candidate.notes.clone(),
            needs_secret: !matches!(candidate.secret, Secret::Known(_)),
            needs_endpoint: candidate.config.endpoint.is_empty(),
            already_in_aktar: existing_match(candidate, &existing).is_some(),
        })
        .collect();
    let shorteners = parsed
        .shorteners
        .iter()
        .map(|shortener| ReviewShortener {
            id: shortener.review_id.clone(),
            name: shortener.import.name.clone().unwrap_or_else(|| shortener.import.host.clone()),
            host: shortener.import.host.clone(),
            method: shortener.import.method.clone(),
            endpoint: shortener.import.endpoint.clone(),
            uses_http: shortener.import.uses_http,
            deletion_skipped: shortener.import.deletion_skipped,
            has_token: shortener.import.token.is_some(),
            secret_locations: shortener.import.secret_locations.clone(),
        })
        .collect();
    let bases: Vec<&str> = parsed.destinations.iter().filter_map(|candidate| candidate.sharex_base_url.as_deref()).collect();
    let history_count = history.iter().filter(|entry| bases.iter().any(|base| super::object_key_under(base, &entry.url).is_some())).count();
    ShareXReview { source, destinations, shorteners, history_count, history_skipped: history.len() - history_count, skipped: parsed.skipped.clone() }
}

#[tauri::command]
pub async fn sharex_import(core: Core<'_>, choices: ImportChoices) -> Result<ShareXImported, String> {
    let Some(pending) = PENDING.lock().unwrap().take() else {
        return Err(t!("Read ShareX’s settings again, then import."));
    };
    let core = core.inner().clone();
    let existing = existing_with_keys(&core);
    let mut imported: Vec<DestinationConfig> = Vec::new();
    let mut failures: Vec<ImportFailure> = Vec::new();
    // Review ID -> the destination it is in Aktar now (new or already there).
    let mut by_review: HashMap<String, DestinationConfig> = HashMap::new();
    let mut matchers: Vec<(String, DestinationConfig)> = Vec::new();
    for candidate in &pending.destinations {
        if let Some(destination) = existing_match(candidate, &existing) {
            by_review.insert(candidate.review_id.clone(), destination.clone());
            if let Some(base) = &candidate.sharex_base_url {
                matchers.push((base.clone(), destination));
            }
        }
    }
    for choice in &choices.destinations {
        let Some(mut candidate) = pending.destinations.iter().find(|candidate| candidate.review_id == choice.id).cloned() else { continue };
        if by_review.contains_key(&candidate.review_id) {
            continue;
        }
        let name = candidate.config.name.clone();
        let fail = |failures: &mut Vec<ImportFailure>, message: String| failures.push(ImportFailure { name: name.clone(), message });
        if let Some(secret) = choice.secret_access_key.as_deref().map(str::trim).filter(|secret| !secret.is_empty()) {
            candidate.secret = Secret::Known(secret.to_string());
        }
        let Secret::Known(secret) = candidate.secret.clone() else {
            fail(&mut failures, t!("Enter the secret key."));
            continue;
        };
        if let Some(endpoint) = choice.endpoint.as_deref().map(str::trim).filter(|endpoint| !endpoint.is_empty()) {
            super::apply_typed_endpoint(&mut candidate, endpoint);
        }
        if candidate.config.endpoint.is_empty() && candidate.kind == UploaderKind::BackblazeB2 {
            authorize(&mut candidate).await;
        }
        if candidate.config.endpoint.is_empty() {
            fail(&mut failures, t!("Enter the S3 endpoint."));
            continue;
        }
        let keys = StorageCredentials {
            access_key_id: candidate.access_key_id.clone(),
            secret_access_key: secret,
            session_token: None,
            cloudflare_token: None,
            short_link_token: None,
        };
        let mut config = candidate.config.clone();
        config.sanitize();
        if let Err(error) = credentials::save(&keys, &config.id) {
            fail(&mut failures, error.to_string());
            continue;
        }
        // The first one becomes the default only when there was none.
        core.destinations.add(config.clone());
        let saved = core.destinations.find(Some(&config.id)).unwrap_or(config);
        by_review.insert(candidate.review_id.clone(), saved.clone());
        if let Some(base) = &candidate.sharex_base_url {
            matchers.push((base.clone(), saved.clone()));
        }
        imported.push(saved);
    }

    let mut shorteners = 0;
    for choice in &choices.shorteners {
        let Some(shortener) = pending.shorteners.iter().find(|shortener| shortener.review_id == choice.id) else { continue };
        let target = by_review
            .get(&choice.destination)
            .cloned()
            .or_else(|| core.destinations.find(Some(&choice.destination)).filter(|destination| destination.id.eq_ignore_ascii_case(&choice.destination)));
        let Some(target) = target else { continue };
        let name = shortener.import.name.clone().unwrap_or_else(|| shortener.import.host.clone());
        match attach_shortener(&core, &target, &shortener.import) {
            Ok(()) => shorteners += 1,
            Err(message) => failures.push(ImportFailure { name, message }),
        }
    }

    let (history_imported, history_duplicates) = if choices.history && !matchers.is_empty() {
        let history_core = core.clone();
        let entries = pending.history;
        tauri::async_runtime::spawn_blocking(move || import_history(&history_core, &entries, &matchers)).await.map_err(|error| error.to_string())?
    } else {
        (0, 0)
    };
    core.settings.update(|settings| settings.sharex_offer_answered = true);
    core.notify(events::DESTINATIONS_CHANGED);
    if history_imported > 0 {
        core.notify(events::HISTORY_CHANGED);
    }
    let destinations = imported.iter().filter_map(|destination| core.destinations.find(Some(&destination.id))).collect();
    Ok(ShareXImported { destinations, failures, shorteners, history_imported, history_duplicates })
}

/// A shortener custom uploader as the destination's short links, as the
/// `.sxcu` import in the destination form sets it up, its token with the
/// destination's keys.
fn attach_shortener(core: &SharedCore, destination: &DestinationConfig, import: &crate::short_links::sharex::ShareXImport) -> Result<(), String> {
    let settings = ShortLinkSettings {
        provider_id: CUSTOM_ID.to_string(),
        endpoint: None,
        domain: None,
        custom: Some(import.definition.clone()),
        only_longer_than: 0,
        shorten_temporary_links: false,
        allow_insecure_http: import.uses_http,
    };
    if settings.definition().is_none() {
        return Err(t!("Aktar can’t import this configuration: it uses {0}.", t!("a definition Aktar can’t read")));
    }
    let mut keys = credentials::load(&destination.id).map_err(|error| error.to_string())?;
    keys.short_link_token = import.token.clone();
    credentials::save(&keys, &destination.id).map_err(|error| error.to_string())?;
    core.destinations.modify(&destination.id, |saved| saved.short_links = Some(settings));
    Ok(())
}

/// History entries under an imported destination's URL become history
/// records; ones already there (same destination and key) are skipped.
/// Returns how many were imported and how many were there already.
fn import_history(core: &SharedCore, entries: &[HistoryEntry], matchers: &[(String, DestinationConfig)]) -> (usize, usize) {
    let mut imported = 0;
    let mut duplicates = 0;
    for entry in entries {
        let Some((destination, key)) = matchers.iter().find_map(|(base, destination)| super::object_key_under(base, &entry.url).map(|key| (destination, key))) else {
            continue;
        };
        if !core.history.with_object(&destination.id, &key).is_empty() {
            duplicates += 1;
            continue;
        }
        let name = record_name(&entry.file_name, &key);
        let mime_type = crate::output::content_type(&name);
        core.history.insert(NewRecord {
            local_filename: &name,
            object_key: &key,
            public_url: entry.url.trim(),
            destination,
            mime_type: &mime_type,
            byte_size: 0,
            expire_after_days: None,
            content_hash: None,
            watched_folder: None,
            source: Some(("sharex", "ShareX")),
            created_at: entry.date_time,
            share_page_key: None,
            encryption: None,
        });
        imported += 1;
    }
    (imported, duplicates)
}

/// ShareX's FileName, or the key's last part without one.
fn record_name(file_name: &str, key: &str) -> String {
    let name = file_name.trim();
    if name.is_empty() {
        crate::util::last_component(key).to_string()
    } else {
        Path::new(name).file_name().map_or_else(|| name.to_string(), |name| name.to_string_lossy().into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_records() {
        assert_eq!(record_name("shot.png", "a/b.png"), "shot.png");
        assert_eq!(record_name("", "ShareX/2026/11/x.png"), "x.png");
        assert_eq!(record_name("  ", "x.png"), "x.png");
    }
}
