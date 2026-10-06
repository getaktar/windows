//! The behavior rules of the Mac repo's docs/short-links.md, apart from
//! the app around them so they can be tested.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::definition::{Capabilities, ShortLinkSettings};
use super::engine::{Operations, ShortLinkError, Target};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    Active,
    /// Its upload (or the temporary link it points at) expired.
    Expired,
    Deleted,
    /// The file is gone or moved, and the short link may still exist.
    Orphaned,
    /// Updating its target failed: it may point at the old place.
    Unknown,
}

impl Status {
    pub fn raw_value(self) -> &'static str {
        match self {
            Status::Active => "active",
            Status::Expired => "expired",
            Status::Deleted => "deleted",
            Status::Orphaned => "orphaned",
            Status::Unknown => "unknown",
        }
    }

    /// Anything this version doesn't know reads as unknown.
    pub fn from_raw(raw: &str) -> Self {
        match raw {
            "active" => Status::Active,
            "expired" => Status::Expired,
            "deleted" => Status::Deleted,
            "orphaned" => Status::Orphaned,
            _ => Status::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Skip,
    /// With this expiry (Unix milliseconds; none: none).
    Create { expires_at: Option<i64> },
}

/// What a link to be shortened is, for `decide`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Shortening {
    /// The link is a temporary (presigned) one, valid until then. Those are
    /// shortened only with `shorten_temporary_links` and a provider that can
    /// expire links, and the short link expires with it, so it never
    /// outlives its target.
    pub temporary_expires_at: Option<i64>,
    /// The upload's "Delete after", passed on when the provider can expire
    /// links.
    pub upload_expires_at: Option<i64>,
    /// One file of a folder uploaded with its structure; only the link Aktar
    /// copies for a whole upload is shortened.
    pub is_folder_file: bool,
    /// Asked for by hand (Create Short Link, Retry, the local API's
    /// short=1), so the length limit doesn't apply.
    pub explicit: bool,
}

/// Whether a link gets a short link (rules 2 and 6), and when that expires.
pub fn decide(
    settings: Option<&ShortLinkSettings>,
    capabilities: Option<&Capabilities>,
    link: &str,
    shortening: Shortening,
    now: i64,
) -> Decision {
    let (Some(settings), Some(capabilities)) = (settings, capabilities) else { return Decision::Skip };
    if shortening.is_folder_file {
        return Decision::Skip;
    }
    if !shortening.explicit && settings.only_longer_than > 0 && link.chars().count() as i64 <= settings.only_longer_than {
        return Decision::Skip;
    }
    let expires_at = match shortening.temporary_expires_at {
        Some(temporary) => {
            if !settings.shorten_temporary_links || !capabilities.supports_expiration() {
                return Decision::Skip;
            }
            Some(temporary.min(shortening.upload_expires_at.unwrap_or(temporary)))
        }
        None => shortening.upload_expires_at.filter(|_| capabilities.supports_expiration()),
    };
    // A relative expiry is at least a minute.
    if expires_at.is_some_and(|at| at - now < 60_000) {
        return Decision::Skip;
    }
    Decision::Create { expires_at }
}

/// Whether "Also shorten temporary links" can be turned on.
pub fn can_shorten_temporary_links(capabilities: Option<&Capabilities>) -> bool {
    capabilities.is_some_and(Capabilities::supports_expiration)
}

/// What the rules need to know about a stored short link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub id: String,
    pub provider: String,
    pub provider_id: Option<String>,
    pub domain: Option<String>,
    pub status: Status,
    pub created_at: i64,
    pub expires_at: Option<i64>,
}

impl Snapshot {
    pub fn target(&self) -> Option<Target> {
        self.provider_id.as_ref().map(|id| Target { provider_id: id.clone(), domain: self.domain.clone() })
    }

    pub fn is_past_expiry(&self, now: i64) -> bool {
        self.expires_at.is_some_and(|at| at <= now)
    }

    fn is_live(&self, now: i64) -> bool {
        self.status == Status::Active && !self.is_past_expiry(now)
    }
}

/// The short link shown and copied for an upload (rule 4): the newest
/// active one that hasn't run out. (`History`'s queries pick the same one
/// in SQL for the lists.)
pub fn active(links: &[Snapshot], now: i64) -> Option<&Snapshot> {
    links.iter().filter(|link| link.is_live(now)).max_by_key(|link| link.created_at)
}

/// Active, but past its expiry: shown as expired.
pub fn display_status(link: &Snapshot, now: i64) -> Status {
    if link.status == Status::Active && link.is_past_expiry(now) {
        Status::Expired
    } else {
        link.status
    }
}

/// What moving a file means for its short links (rule 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MovePlan {
    /// No active short link: nothing to do.
    Nothing,
    /// Create the new object, update the targets, delete the old one.
    Update,
    /// The provider can't change a target (or isn't the one set up any
    /// more): warn first, and orphan the links if the user goes ahead.
    Warn,
}

pub fn move_plan<O: Operations>(links: &[Snapshot], operations: Option<&O>, now: i64) -> MovePlan {
    let active: Vec<&Snapshot> = links.iter().filter(|link| link.is_live(now)).collect();
    if active.is_empty() {
        return MovePlan::Nothing;
    }
    match operations {
        Some(operations)
            if operations.capabilities().update_destination
                && active.iter().all(|link| link.provider == operations.provider() && link.provider_id.is_some()) =>
        {
            MovePlan::Update
        }
        _ => MovePlan::Warn,
    }
}

// MARK: - Lifecycle: what each link's status becomes. File operations never
// wait on, or fail because of, these.

/// After the file was deleted (rule 5): every short link that may still
/// work is deleted at the provider. One that can't be (no delete support,
/// another provider, or the request failed) is orphaned; an expired one
/// that can't be stays expired. Links already deleted or orphaned aren't in
/// the result.
pub async fn delete_all<O: Operations>(links: &[Snapshot], operations: Option<&O>) -> HashMap<String, Status> {
    let mut result = HashMap::new();
    for link in links.iter().filter(|link| matches!(link.status, Status::Active | Status::Unknown | Status::Expired)) {
        let failed = if link.status == Status::Expired { Status::Expired } else { Status::Orphaned };
        let usable = operations.filter(|operations| operations.provider() == link.provider && operations.capabilities().delete);
        let status = match (usable, link.target()) {
            (Some(operations), Some(target)) => match operations.delete(&target).await {
                Ok(()) => Status::Deleted,
                Err(_) => failed,
            },
            _ => failed,
        };
        result.insert(link.id.clone(), status);
    }
    result
}

/// One short link deleted by hand: deleted, or the error.
pub async fn delete<O: Operations>(link: &Snapshot, operations: Option<&O>) -> Result<Status, ShortLinkError> {
    let operations = operations.filter(|operations| operations.provider() == link.provider).ok_or(ShortLinkError::NotConfigured)?;
    let target = link.target().filter(|_| operations.capabilities().delete).ok_or(ShortLinkError::Unsupported)?;
    operations.delete(&target).await?;
    Ok(Status::Deleted)
}

/// A move with `MovePlan::Update` (rule 7): each active link is pointed at
/// `new_url`. `all_updated` false means at least one may still point at the
/// old object, which is then kept; that one is unknown.
pub async fn update_all<O: Operations>(links: &[Snapshot], new_url: &str, operations: &O, now: i64) -> (HashMap<String, Status>, bool) {
    let mut statuses = HashMap::new();
    let mut all_updated = true;
    for link in links.iter().filter(|link| link.is_live(now)) {
        let status = match link.target().filter(|_| link.provider == operations.provider()) {
            Some(target) => match operations.update(&target, new_url).await {
                Ok(()) => Status::Active,
                Err(_) => Status::Unknown,
            },
            None => Status::Unknown,
        };
        if status == Status::Unknown {
            all_updated = false;
        }
        statuses.insert(link.id.clone(), status);
    }
    (statuses, all_updated)
}

/// The user moved a file anyway: its active links may now point at
/// nothing.
pub fn orphan_all(links: &[Snapshot]) -> HashMap<String, Status> {
    links.iter().filter(|link| link.status == Status::Active).map(|link| (link.id.clone(), Status::Orphaned)).collect()
}

/// The upload expired (rule 10): its links are marked expired here; the
/// provider expires them itself when it supports that.
pub fn expire_all(links: &[Snapshot]) -> HashMap<String, Status> {
    links
        .iter()
        .filter(|link| matches!(link.status, Status::Active | Status::Unknown))
        .map(|link| (link.id.clone(), Status::Expired))
        .collect()
}
