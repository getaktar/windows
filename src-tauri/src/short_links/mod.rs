//! Bring Your Own Shortener: short links for uploads, made with the user's
//! own link shortener (Shlink, YOURLS, Kutt, Dub, Short.io, or a custom HTTP
//! request), as the Mac app does (its docs/short-links.md). This module is
//! the one place the upload pipeline, History, the bucket view and the
//! local API go through: making short links after an upload or by hand,
//! cleaning them up when files are deleted, moved or expire, and their
//! stats. Short links never hold up or fail a file operation.

pub mod definition;
pub mod engine;
pub mod rules;
pub mod sharex;
pub mod store;
#[cfg(test)]
mod tests;

use std::collections::HashMap;

use serde::Serialize;

use crate::core::{events, SharedCore};
use crate::destinations::DestinationConfig;
use crate::history::UploadRecord;
use crate::storage::S3Provider;
use crate::t;
use definition::ShortLinkSettings;
use engine::{Created, Engine, Operations, ShortLinkError};
use rules::{Decision, MovePlan, Shortening, Status};
use store::ShortLink;

/// How long fetched stats are shown before they're fetched again.
pub const STATS_LIFETIME_MS: i64 = 5 * 60_000;

/// The destination's shortener, with its token; none when it has none.
pub fn engine_for(destination: &DestinationConfig) -> Option<Engine> {
    let settings = destination.short_links.clone()?;
    let definition = settings.definition()?.clone();
    let token = crate::credentials::load(&destination.id).ok().and_then(|keys| keys.short_link_token);
    Some(Engine::new(definition, settings, token))
}

fn destination(core: &SharedCore, id: &str) -> Option<DestinationConfig> {
    core.destinations.all().into_iter().find(|destination| destination.id == id)
}

/// The message for an error, without the token.
pub fn message(error: &ShortLinkError, token: Option<&str>) -> String {
    engine::redact(&error.to_string(), token)
}

// MARK: - Creating

/// A short link made at the provider and not stored yet (the upload's
/// history entry comes after it).
#[derive(Debug, Clone)]
pub struct Pending {
    pub created: Created,
    pub provider: String,
    pub provider_name: String,
    pub domain: Option<String>,
    pub target_url: String,
    pub expires_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub enum Attempt {
    /// Not for this link (off, too short, a temporary link, ...).
    Skipped,
    Created(Pending),
    /// Made the user's way, so the original link is copied and the user
    /// told; the message never contains the token.
    Failed(String),
}

/// Rules 2 and 6: shortens `link` when the destination's rules say so.
/// Waited for before the link is copied; a failure comes back as
/// `Attempt::Failed`, never as an error.
pub async fn shorten(destination: &DestinationConfig, link: &str, shortening: Shortening) -> Attempt {
    let Some(engine) = engine_for(destination) else {
        return if shortening.explicit { Attempt::Failed(ShortLinkError::NotConfigured.to_string()) } else { Attempt::Skipped };
    };
    let decision = rules::decide(Some(&engine.settings), Some(engine.capabilities()), link, shortening, crate::util::now_millis());
    let Decision::Create { expires_at } = decision else { return Attempt::Skipped };
    match engine.create(link, expires_at).await {
        Ok(created) => Attempt::Created(Pending {
            created,
            provider: engine.provider().to_string(),
            provider_name: engine.definition.name.clone(),
            domain: engine.settings.trimmed_domain(),
            target_url: link.to_string(),
            expires_at: expires_at.filter(|_| engine.capabilities().supports_expiration()),
        }),
        Err(error) => Attempt::Failed(message(&error, engine.token.as_deref())),
    }
}

/// Stores a short link made by `shorten` for the upload `upload_id`.
pub fn store(core: &SharedCore, pending: Pending, upload_id: &str) -> ShortLink {
    let link = ShortLink {
        id: crate::util::new_id(),
        upload_id: upload_id.to_string(),
        provider: pending.provider,
        provider_name: pending.provider_name,
        provider_id: pending.created.provider_id,
        domain: pending.domain,
        short_url: pending.created.short_url,
        target_url: pending.target_url,
        created_at: crate::util::now_millis(),
        expires_at: pending.expires_at,
        status: Status::Active,
        clicks: None,
        last_click_at: None,
        stats_checked_at: None,
    };
    core.history.short_links().insert(&link);
    link
}

/// Create Short Link and Retry: a short link for the upload's link,
/// whatever its length. A destination that copies temporary links gets one
/// for a fresh temporary link when it shortens those, otherwise for the
/// public URL. Returns the stored link, or why it couldn't be made.
pub async fn create_for_record(core: &SharedCore, record: &UploadRecord) -> Result<ShortLink, String> {
    let destination = destination(core, &record.destination_id).ok_or_else(|| t!("This upload’s destination was removed."))?;
    let mut link = record.public_url.clone();
    let mut temporary_expires_at = None;
    let settings = destination.short_links.as_ref();
    if let (Some(seconds), true) = (
        destination.temporary_link,
        settings.is_some_and(|settings| {
            settings.shorten_temporary_links && rules::can_shorten_temporary_links(settings.definition().map(|definition| &definition.capabilities))
        }),
    ) {
        if let Ok(signed) = crate::uploads::temporary_url(core, record, seconds).await {
            link = signed;
            temporary_expires_at = Some(crate::util::now_millis() + seconds as i64 * 1000);
        }
    }
    let shortening = Shortening { temporary_expires_at, upload_expires_at: record.expires_at, is_folder_file: false, explicit: true };
    let result = match shorten(&destination, &link, shortening).await {
        Attempt::Created(pending) => Ok(store(core, pending, &record.id)),
        Attempt::Failed(message) => Err(message),
        Attempt::Skipped => Err(ShortLinkError::Unsupported.to_string()),
    };
    core.notify(events::HISTORY_CHANGED);
    result
}

/// Retry on a failed short link's notification, and Create Short Link in
/// menus: makes one for the upload and copies it as its destination's
/// "Copy as" says. Failing again says so again.
pub async fn retry(core: &SharedCore, upload_id: &str) {
    let Some(record) = core.history.get(upload_id) else { return };
    match create_for_record(core, &record).await {
        Ok(link) => {
            let copied = match destination(core, &record.destination_id) {
                Some(destination) => crate::uploads::format_link(core, &destination, &link.target_url, Some(&link.short_url), &record.local_filename),
                None => link.short_url.clone(),
            };
            crate::clipboard::copy(&copied);
            crate::uploads::show_notification(core, &record.local_filename, &t!("Short link created and copied"));
        }
        Err(reason) => notify_failed(core, &record.local_filename, &reason, &record.id),
    }
}

// MARK: - Reading

pub fn links(core: &SharedCore, upload_id: &str) -> Vec<ShortLink> {
    core.history.short_links().for_upload(upload_id)
}

/// The short link shown and copied for an upload (rule 4), if any.
pub fn active(core: &SharedCore, upload_id: &str) -> Option<ShortLink> {
    active_among(links(core, upload_id))
}

fn active_among(links: Vec<ShortLink>) -> Option<ShortLink> {
    let snapshots: Vec<_> = links.iter().map(ShortLink::snapshot).collect();
    let id = rules::active(&snapshots, crate::util::now_millis())?.id.clone();
    links.into_iter().find(|link| link.id == id)
}

/// Whether History offers Create Short Link: the destination has a
/// shortener, and the upload has no active link made with it (none yet,
/// or the provider was switched since).
pub fn can_create(core: &SharedCore, record: &UploadRecord) -> bool {
    let Some(settings) = destination(core, &record.destination_id).and_then(|destination| destination.short_links) else { return false };
    if settings.definition().is_none() {
        return false;
    }
    active(core, &record.id).is_none_or(|active| active.provider != settings.provider_id)
}

/// Whether the provider of `link` reports clicks.
pub fn has_stats(core: &SharedCore, link: &ShortLink) -> bool {
    settings_for_upload(core, &link.upload_id)
        .filter(|settings| settings.provider_id == link.provider)
        .and_then(|settings| settings.definition().map(|definition| definition.capabilities.has_stats()))
        .unwrap_or(false)
}

fn settings_for_upload(core: &SharedCore, upload_id: &str) -> Option<ShortLinkSettings> {
    let record = core.history.get(upload_id)?;
    destination(core, &record.destination_id)?.short_links
}

fn engine_for_upload(core: &SharedCore, upload_id: &str) -> Option<Engine> {
    let record = core.history.get(upload_id)?;
    engine_for(&destination(core, &record.destination_id)?)
}

/// Fetches clicks and the last click when the provider has them and the
/// cached ones are older than `STATS_LIFETIME_MS` (or `force`). Failures
/// keep what was there. Returns the link as it is now.
pub async fn refresh_stats(core: &SharedCore, link: ShortLink, force: bool) -> ShortLink {
    let now = crate::util::now_millis();
    if !force && link.stats_checked_at.is_some_and(|checked| now - checked < STATS_LIFETIME_MS) {
        return link;
    }
    if !matches!(link.status, Status::Active | Status::Unknown) {
        return link;
    }
    let (Some(target), Some(engine)) = (link.snapshot().target(), engine_for_upload(core, &link.upload_id)) else { return link };
    if engine.provider() != link.provider || !engine.capabilities().has_stats() {
        return link;
    }
    let Ok(stats) = engine.stats(&target).await else { return link };
    let clicks = stats.clicks.or(link.clicks);
    let last_click_at = stats.last_click_at.or(link.last_click_at);
    core.history.short_links().set_stats(&link.id, clicks, last_click_at, now);
    ShortLink { clicks, last_click_at, stats_checked_at: Some(now), ..link }
}

// MARK: - Deleting

/// Delete Short Link: deletes it at the provider. Fails when it can't
/// (nothing changes then).
pub async fn delete(core: &SharedCore, link_id: &str) -> Result<(), String> {
    let link = core.history.short_links().get(link_id).ok_or_else(|| ShortLinkError::NotConfigured.to_string())?;
    let engine = engine_for_upload(core, &link.upload_id);
    let status = rules::delete(&link.snapshot(), engine.as_ref())
        .await
        .map_err(|error| message(&error, engine.as_ref().and_then(|engine| engine.token.as_deref())))?;
    core.history.short_links().set_statuses(&HashMap::from([(link.id.clone(), status)]));
    core.notify(events::HISTORY_CHANGED);
    Ok(())
}

/// After the upload's file was deleted (rule 5): its short links are
/// deleted too, in the background. Ones that couldn't be are marked
/// orphaned and the user is told; the file's delete stands either way.
pub fn clean_up_after_file_deleted(core: &SharedCore, upload_id: &str, destination: Option<&DestinationConfig>, filename: &str) {
    let snapshots: Vec<_> = links(core, upload_id).iter().map(ShortLink::snapshot).collect();
    if !snapshots.iter().any(|link| matches!(link.status, Status::Active | Status::Unknown | Status::Expired)) {
        return;
    }
    let engine = destination.and_then(engine_for);
    let (core, filename) = (core.clone(), filename.to_string());
    tauri::async_runtime::spawn(async move {
        let statuses = rules::delete_all(&snapshots, engine.as_ref()).await;
        core.history.short_links().set_statuses(&statuses);
        core.notify(events::HISTORY_CHANGED);
        if statuses.values().any(|status| *status == Status::Orphaned) {
            crate::uploads::show_notification(
                &core,
                &t!("File deleted ✓ / Short link cleanup failed ⚠"),
                &t!("{0}: the short link may still exist.", filename),
            );
        }
    });
}

/// The short links of the uploads at `key`, after that object was deleted
/// (the bucket view, the local API).
pub fn clean_up_object(core: &SharedCore, destination: &DestinationConfig, key: &str) {
    for record in core.history.with_object(&destination.id, key) {
        clean_up_after_file_deleted(core, &record.id, Some(destination), &record.local_filename);
    }
}

/// Remove from History: the upload's short links are forgotten here too.
/// Nothing is deleted at the provider; the file stays, and so do its links.
pub fn forget(core: &SharedCore, upload_id: &str) {
    core.history.short_links().delete_for_upload(upload_id);
}

// MARK: - Moving

/// What moving the object at `key` would mean for its uploads' short links
/// (rule 7).
pub fn move_plan(core: &SharedCore, destination: &DestinationConfig, key: &str) -> MovePlan {
    let records = core.history.with_object(&destination.id, key);
    let snapshots: Vec<_> = records.iter().flat_map(|record| links(core, &record.id)).map(|link| link.snapshot()).collect();
    rules::move_plan(&snapshots, engine_for(destination).as_ref(), crate::util::now_millis())
}

/// What happened to the short links of a moved object, as the local API
/// reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MoveStatus {
    None,
    Updated,
    Orphaned,
    /// An update failed, so the original object was kept and the link
    /// still works.
    NotUpdated,
}

/// Renames or moves an object with rule 7 applied: short links that can
/// follow are pointed at the new key between the copy and the delete (and
/// if one can't be, the original stays so it keeps working); the others
/// are marked orphaned. The bucket view asks first for those (see
/// `move_plan`); the local API doesn't.
pub async fn move_object(
    core: &SharedCore,
    destination: &DestinationConfig,
    storage: &S3Provider,
    from: &str,
    new_key: &str,
    prefixes: &[String],
) -> Result<MoveStatus, crate::bucket::MoveError> {
    let plan = move_plan(core, destination, from);
    let records = core.history.with_object(&destination.id, from);
    crate::bucket::copy_for_move(storage, from, new_key, prefixes).await?;
    let mut status = MoveStatus::None;
    if plan == MovePlan::Update {
        let new_url = crate::output::resolve_public_url(&destination.public_base_url, new_key);
        status = if update_targets(core, &records, &new_url, destination).await { MoveStatus::Updated } else { MoveStatus::NotUpdated };
    }
    if status != MoveStatus::NotUpdated {
        crate::bucket::remove_moved(storage, from, prefixes).await?;
    }
    if plan == MovePlan::Warn {
        for record in &records {
            let snapshots: Vec<_> = links(core, &record.id).iter().map(ShortLink::snapshot).collect();
            core.history.short_links().set_statuses(&rules::orphan_all(&snapshots));
        }
        status = MoveStatus::Orphaned;
    }
    Ok(status)
}

/// The object was copied to `new_url`: points the active short links of
/// `records` there. False when one couldn't be updated: those are unknown,
/// and the old object has to stay.
async fn update_targets(core: &SharedCore, records: &[UploadRecord], new_url: &str, destination: &DestinationConfig) -> bool {
    let Some(engine) = engine_for(destination) else { return false };
    let mut all_updated = true;
    for record in records {
        let links = links(core, &record.id);
        let snapshots: Vec<_> = links.iter().map(ShortLink::snapshot).collect();
        let (statuses, updated) = rules::update_all(&snapshots, new_url, &engine, crate::util::now_millis()).await;
        core.history.short_links().set_statuses(&statuses);
        for (id, _) in statuses.iter().filter(|(_, status)| **status == Status::Active) {
            core.history.short_links().set_target(id, new_url);
        }
        all_updated &= updated;
    }
    all_updated
}

// MARK: - Expiry

/// The upload expired (rule 10).
pub fn mark_expired(core: &SharedCore, upload_id: &str) {
    let snapshots: Vec<_> = links(core, upload_id).iter().map(ShortLink::snapshot).collect();
    core.history.short_links().set_statuses(&rules::expire_all(&snapshots));
}

// MARK: - Notifications

/// Rule 3: the short link couldn't be made, so the original link was
/// copied. Retry makes one and copies it.
pub fn notify_failed(core: &SharedCore, filename: &str, reason: &str, upload_id: &str) {
    let subtitle = t!("Short link couldn't be created. The original link was copied instead.");
    #[cfg(windows)]
    match retry_toast(core, filename, &subtitle, reason, upload_id) {
        Ok(()) => return,
        Err(error) => log::warn!("Could not show a notification with Retry: {error}"),
    }
    #[cfg(not(windows))]
    let _ = upload_id;
    crate::uploads::show_notification(core, filename, &format!("{subtitle}\n{reason}"));
}

/// A toast with a Retry button, under the same identity as the app's other
/// notifications: the package's in the Microsoft Store build, the app's
/// identifier when installed, PowerShell's in a dev build.
#[cfg(windows)]
fn retry_toast(core: &SharedCore, title: &str, subtitle: &str, reason: &str, upload_id: &str) -> Result<(), String> {
    use tauri_winrt_notification::Toast;
    let app_id = if crate::package::is_packaged() {
        windows::ApplicationModel::AppInfo::Current()
            .and_then(|info| info.AppUserModelId())
            .map_err(|error| error.to_string())?
            .to_string()
    } else if tauri::is_dev() {
        Toast::POWERSHELL_APP_ID.to_string()
    } else {
        core.app.config().identifier.clone()
    };
    let (retry_core, upload_id) = (core.clone(), upload_id.to_string());
    Toast::new(&app_id)
        .title(title)
        .text1(subtitle)
        .text2(reason)
        .add_button(&t!("Retry"), "retry")
        .on_activated(move |action| {
            if action.as_deref() == Some("retry") {
                let (core, upload_id) = (retry_core.clone(), upload_id.clone());
                tauri::async_runtime::spawn(async move { retry(&core, &upload_id).await });
            }
            Ok(())
        })
        .show()
        .map_err(|error| error.to_string())
}

// MARK: - For the windows and the local API

/// A short link as History shows it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShortLinkView {
    #[serde(flatten)]
    pub link: ShortLink,
    /// Active but past its expiry shows as expired.
    pub display_status: Status,
}

/// An upload's short links for its details.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShortLinkInfo {
    /// Newest first.
    pub links: Vec<ShortLinkView>,
    pub active_id: Option<String>,
    pub can_create: bool,
    /// The provider reports clicks for the active link.
    pub has_stats: bool,
}

/// `refresh`: the active link's stats are fetched first when they're
/// older than a few minutes.
pub async fn info(core: &SharedCore, record: &UploadRecord, refresh: bool) -> ShortLinkInfo {
    if refresh {
        if let Some(active) = active(core, &record.id) {
            refresh_stats(core, active, false).await;
        }
    }
    let links = links(core, &record.id);
    let active = active_among(links.clone());
    let now = crate::util::now_millis();
    ShortLinkInfo {
        active_id: active.as_ref().map(|link| link.id.clone()),
        can_create: can_create(core, record),
        has_stats: active.as_ref().is_some_and(|link| has_stats(core, link)),
        links: links.into_iter().map(|link| ShortLinkView { display_status: link.display_status(now), link }).collect(),
    }
}

/// `ShortLink` in the local API, with nulls written out, the same as the
/// Mac app's.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShortLinkDto {
    pub id: String,
    pub short_url: String,
    pub target_url: String,
    pub provider: String,
    pub provider_name: String,
    pub status: Status,
    pub created_at: String,
    pub expires_at: Option<String>,
    pub clicks: Option<i64>,
    pub last_click_at: Option<String>,
}

impl ShortLinkDto {
    pub fn new(link: &ShortLink) -> Self {
        Self {
            id: link.id.clone(),
            short_url: link.short_url.clone(),
            target_url: link.target_url.clone(),
            provider: link.provider.clone(),
            provider_name: link.provider_name.clone(),
            status: link.display_status(crate::util::now_millis()),
            created_at: crate::util::iso8601(link.created_at),
            expires_at: link.expires_at.map(crate::util::iso8601),
            clicks: link.clicks,
            last_click_at: link.last_click_at.map(crate::util::iso8601),
        }
    }
}
