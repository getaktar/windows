//! Expiring uploads ("Delete after N days"). An expiring upload goes under
//! `tmp/{N}d/` in front of the key the destination's template produces, and
//! one bucket lifecycle rule per duration deletes everything under that
//! prefix after N days. The Mac and mobile apps use the same prefixes and
//! rule IDs, so a bucket shared between devices works the same everywhere.
//!
//! The rules are what deletes the files, so "Delete after" is only available
//! for a destination whose bucket is known to have them (status "active").
//! Uploads to any other destination never expire, whatever the setting
//! says. The app also sweeps its own history every hour to clear out entries
//! for uploads that have expired.

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::core::{events, SharedCore};
use crate::credentials::StorageCredentials;
use crate::destinations::DestinationConfig;
use crate::storage::S3Provider;

/// The only durations offered, in days. There are no arbitrary values:
/// each one needs its own lifecycle rule.
pub const DURATIONS: [u32; 4] = [1, 7, 14, 30];

pub const PREFIX_ROOT: &str = "tmp/";
const RULE_ID_PREFIX: &str = "aktar-expire-";
const DAY_MS: i64 = 24 * 60 * 60 * 1000;
const SWEEP_INTERVAL: Duration = Duration::from_secs(60 * 60);

pub fn is_valid(days: u32) -> bool {
    DURATIONS.contains(&days)
}

/// "tmp/7d/"
pub fn prefix(days: u32) -> String {
    format!("{PREFIX_ROOT}{days}d/")
}

/// "aktar-expire-7d"
pub fn rule_id(days: u32) -> String {
    format!("{RULE_ID_PREFIX}{days}d")
}

/// The key an upload expiring after `days` goes to: the one the template
/// produced, under that duration's prefix.
pub fn expiring_key(key: &str, days: u32) -> String {
    format!("{}{key}", prefix(days))
}

/// The duration a key's prefix stands for, if it's under one of them.
pub fn days_in_key(key: &str) -> Option<u32> {
    let rest = key.strip_prefix(PREFIX_ROOT)?;
    DURATIONS.into_iter().find(|days| rest.starts_with(&format!("{days}d/")))
}

/// Unix milliseconds `days` after `created_at`.
pub fn expires_at(created_at: i64, days: u32) -> i64 {
    created_at + i64::from(days) * DAY_MS
}

// MARK: - Lifecycle rules

/// Bucket lifecycle configurations are handled as XML text, not decoded
/// into the SDK's typed rules. Providers don't all write rules the way the
/// AWS model expects (R2's "Default Multipart Abort Rule" has no `Filter`),
/// and a PUT replaces the whole configuration, so every rule that isn't
/// Aktar's goes back exactly as the bucket returned it. The Mac app does
/// the same (`LifecycleXML`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlRule {
    pub id: Option<String>,
    /// `Filter/Prefix`, or the legacy rule-level `Prefix`.
    pub prefix: Option<String>,
    pub status: Option<String>,
    pub expiration_days: Option<u32>,
    /// The rule's inner XML, as returned.
    pub raw: String,
}

/// The `<Rule>` blocks of a lifecycle configuration.
pub fn parse_rules(xml: &str) -> Vec<XmlRule> {
    blocks("Rule", xml)
        .into_iter()
        .map(|raw| {
            let prefix = match blocks("Filter", raw).first() {
                Some(filter) => value("Prefix", filter),
                None => value("Prefix", raw),
            };
            XmlRule {
                id: value("ID", raw),
                prefix,
                status: value("Status", raw),
                expiration_days: blocks("Expiration", raw)
                    .first()
                    .and_then(|expiration| value("Days", expiration))
                    .and_then(|days| days.parse().ok()),
                raw: raw.to_string(),
            }
        })
        .collect()
}

/// The rules of a GET ?lifecycle response, or None when it doesn't read as
/// a lifecycle configuration whose every rule could be parsed: a proxy's
/// HTML page, a provider answering with something else, or elements this
/// parser doesn't follow (namespace prefixes). Writing Aktar's rules on top
/// of a misread configuration would replace the bucket's own rules, so
/// nothing is written then.
pub fn parse_configuration(xml: &str) -> Option<Vec<XmlRule>> {
    if blocks("LifecycleConfiguration", xml).len() != 1 {
        return None;
    }
    let rules = parse_rules(xml);
    let opened = xml.match_indices("<Rule").filter(|(index, _)| is_tag_end(&xml[index + "<Rule".len()..])).count();
    (rules.len() == opened).then_some(rules)
}

fn is_tag_end(rest: &str) -> bool {
    rest.starts_with(|c: char| c == '>' || c == '/' || c.is_ascii_whitespace())
}

/// Whether all of Aktar's rules are in `rules`, exactly as installed.
pub fn rules_in_place(rules: &[XmlRule]) -> bool {
    DURATIONS.iter().all(|days| rules.iter().any(|rule| matches_aktar_rule(rule, *days)))
}

/// The durations whose rule isn't in place yet.
pub fn missing_durations(rules: &[XmlRule]) -> Vec<u32> {
    DURATIONS.into_iter().filter(|days| !rules.iter().any(|rule| matches_aktar_rule(rule, *days))).collect()
}

fn is_aktar_rule(rule: &XmlRule) -> bool {
    rule.id.as_deref().is_some_and(|id| DURATIONS.iter().any(|days| rule_id(*days) == id))
}

/// Whether `rule` is exactly what Aktar installs for `days`.
fn matches_aktar_rule(rule: &XmlRule, days: u32) -> bool {
    rule.id.as_deref() == Some(rule_id(days).as_str())
        && rule.prefix.as_deref() == Some(prefix(days).as_str())
        && rule.status.as_deref() == Some("Enabled")
        && rule.expiration_days == Some(days)
}

fn aktar_rule_xml(days: u32) -> String {
    format!(
        "<ID>{}</ID><Filter><Prefix>{}</Prefix></Filter><Status>Enabled</Status><Expiration><Days>{days}</Days></Expiration>",
        rule_id(days),
        prefix(days)
    )
}

fn document(rules: impl IntoIterator<Item = String>) -> String {
    let rules: String = rules.into_iter().map(|rule| format!("<Rule>{rule}</Rule>")).collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><LifecycleConfiguration xmlns="http://s3.amazonaws.com/doc/2006-03-01/">{rules}</LifecycleConfiguration>"#
    )
}

/// The configuration to PUT: the bucket's other rules untouched, then
/// Aktar's four (replacing older versions of them). None when all four are
/// already in place, so nothing needs writing.
pub fn merged_rules(existing: &[XmlRule]) -> Option<String> {
    if rules_in_place(existing) {
        return None;
    }
    let kept = existing.iter().filter(|rule| !is_aktar_rule(rule)).map(|rule| rule.raw.clone());
    Some(document(kept.chain(DURATIONS.into_iter().map(aktar_rule_xml))))
}

/// What taking Aktar's rules out of a configuration comes to.
#[derive(Debug, PartialEq, Eq)]
pub enum Removal {
    /// There were none.
    Nothing,
    /// No rules would be left, and S3 doesn't take an empty configuration:
    /// the whole configuration is deleted instead.
    DeleteConfiguration,
    /// The XML to PUT, with the bucket's other rules untouched.
    Put(String),
}

pub fn without_aktar_rules(existing: &[XmlRule]) -> Removal {
    let kept: Vec<String> = existing.iter().filter(|rule| !is_aktar_rule(rule)).map(|rule| rule.raw.clone()).collect();
    if kept.len() == existing.len() {
        Removal::Nothing
    } else if kept.is_empty() {
        Removal::DeleteConfiguration
    } else {
        Removal::Put(document(kept))
    }
}

/// The `Code` and `Message` of an S3 error response.
pub fn error_code(xml: &str) -> (Option<String>, Option<String>) {
    (value("Code", xml), value("Message", xml))
}

/// The inner XML of each `<tag>...</tag>` (or `<tag attr="...">...</tag>`)
/// in `xml`, at any depth. Lifecycle elements don't nest within themselves,
/// so the first closing tag always ends a block. `<tag/>` is empty.
fn blocks<'a>(tag: &str, xml: &'a str) -> Vec<&'a str> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut result = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find(&open) {
        let after_name = &rest[start + open.len()..];
        // `<Rules>` isn't a `<Rule>`.
        if !is_tag_end(after_name) {
            rest = after_name;
            continue;
        }
        let Some(tag_end) = after_name.find('>') else { break };
        if after_name[..tag_end].ends_with('/') {
            result.push("");
            rest = &after_name[tag_end + 1..];
            continue;
        }
        let content = &after_name[tag_end + 1..];
        let Some(end) = content.find(&close) else { break };
        result.push(&content[..end]);
        rest = &content[end + close.len()..];
    }
    result
}

fn value(tag: &str, xml: &str) -> Option<String> {
    blocks(tag, xml).first().map(|text| unescape(text).trim().to_string())
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RulesStatus {
    Active,
    /// The key can't read or change lifecycle rules.
    Denied { message: String },
    /// The provider doesn't do lifecycle rules (or refused them for
    /// another reason).
    Unsupported { message: String },
}

/// The last result of setting up a destination's rules, kept in settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RulesCheck {
    pub status: RulesStatus,
    /// Unix milliseconds.
    pub checked_at: i64,
}

pub fn cached(core: &SharedCore, destination_id: &str) -> Option<RulesCheck> {
    core.settings.get().expiry_rules.get(destination_id).cloned()
}

/// Whether the destination's bucket is known to have the rules, which is
/// what makes "Delete after" apply to uploads there.
pub fn is_active(core: &SharedCore, destination_id: &str) -> bool {
    cached(core, destination_id).is_some_and(|check| check.status == RulesStatus::Active)
}

/// Records whether a destination's rules are active: a check's result, or
/// None for "not active" (turned off, or never checked). Settings carry the
/// statuses to the windows, so a change there is what enables or disables
/// the panel's "Delete after" choices.
pub fn record(core: &SharedCore, destination_id: &str, check: Option<RulesCheck>) {
    let current = cached(core, destination_id);
    if current == check {
        return;
    }
    core.settings.update(|settings| match check {
        Some(check) => {
            settings.expiry_rules.insert(destination_id.to_string(), check);
        }
        None => {
            settings.expiry_rules.remove(destination_id);
        }
    });
    core.notify(events::SETTINGS_CHANGED);
}

/// What the destination form found out about the rules while it was open,
/// recorded when the destination is saved.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum FormRules {
    /// Nothing was checked, set up, or turned off in the form.
    #[default]
    NotChecked,
    /// The last result there; None when auto-delete was turned off.
    /// `connection` is the bucket it was about: the connection fields can
    /// still change afterwards, and a result about another bucket must not
    /// be saved for this one.
    Checked {
        check: Option<RulesCheck>,
        #[serde(default)]
        connection: Option<RulesConnection>,
    },
}

/// The bucket a form's rules result was obtained for.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RulesConnection {
    pub endpoint: String,
    pub bucket: String,
    pub region: String,
}

impl RulesConnection {
    /// Whether it's the bucket `config` saves.
    pub fn is_for(&self, config: &DestinationConfig) -> bool {
        self.endpoint.trim() == config.endpoint.trim()
            && self.bucket.trim() == config.bucket.trim()
            && self.region.trim() == config.region.trim()
    }
}

/// Sets the rules up. A network or credentials failure isn't a status of
/// the bucket, so it's returned as an error. The result is recorded right
/// away when `record` is set, i.e. when it was checked with the saved
/// destination's bucket and keys; the form records its own when it's saved.
pub async fn set_up(
    core: &SharedCore,
    destination: DestinationConfig,
    credentials: StorageCredentials,
    record_result: bool,
) -> Result<RulesCheck, String> {
    let id = destination.id.clone();
    let status = S3Provider::new(destination, credentials)
        .ensure_expiry_rules()
        .await
        .map_err(|error| error.to_string())?;
    let check = RulesCheck { status, checked_at: crate::util::now_millis() };
    if record_result {
        record(core, &id, Some(check.clone()));
    }
    Ok(check)
}

/// Takes Aktar's rules back out of the bucket, keeping its other rules.
/// Files already under `tmp/` then stay for good, as the Turn Off dialog
/// promises: with `record` set (the saved destination's bucket and keys),
/// the destination is marked not active and its uploads stop expiring in
/// history, so the sweep leaves them alone too.
pub async fn remove(
    core: &SharedCore,
    destination: DestinationConfig,
    credentials: StorageCredentials,
    record_result: bool,
) -> Result<(), String> {
    let id = destination.id.clone();
    S3Provider::new(destination, credentials)
        .remove_expiry_rules()
        .await
        .map_err(|error| error.to_string())?;
    if record_result {
        record(core, &id, None);
        rules_removed(core, &id);
    }
    Ok(())
}

/// The bucket of `destination_id` no longer has Aktar's rules, so its
/// uploads stay for good.
pub fn rules_removed(core: &SharedCore, destination_id: &str) {
    core.history.clear_expiry(destination_id);
    core.notify(events::HISTORY_CHANGED);
}

// MARK: - Sweep

/// On launch and every hour after: clears history entries of uploads past
/// their expiry date. The bucket's lifecycle rules have normally deleted
/// the file already. See `sweep_one` for when Aktar deletes it itself. A
/// failure (offline) leaves the entry for the next sweep.
pub fn schedule_sweep(core: &SharedCore) {
    let core = core.clone();
    tauri::async_runtime::spawn(async move {
        // Out of the way of everything else starting up.
        tokio::time::sleep(Duration::from_secs(5)).await;
        loop {
            sweep(&core).await;
            tokio::time::sleep(SWEEP_INTERVAL).await;
        }
    });
}

async fn sweep(core: &SharedCore) {
    let mut failures: HashMap<String, usize> = HashMap::new();
    for record in core.history.expired(crate::util::now_millis()) {
        // One destination that can't be reached shouldn't cost a timeout
        // per file.
        if failures.get(&record.destination_id).is_some_and(|count| *count >= 3) {
            continue;
        }
        match sweep_one(core, &record).await {
            // Its short links expired with it (they're kept, as expired).
            Ok(()) => crate::short_links::mark_expired(core, &record.id),
            Err(message) => {
                log::warn!("Could not clear expired upload {}: {message}", record.object_key);
                *failures.entry(record.destination_id).or_default() += 1;
            }
        }
    }
}

/// What the sweep does with one expired upload. Aktar deletes the file
/// itself only as a stand-in for the bucket's own rule, so only where that
/// rule is known to be in place, and only the upload it recorded:
/// - The destination is gone: nothing to delete from; the entry goes.
/// - Its rules aren't active (turned off, or the destination now points at
///   another bucket): whatever the bucket does with the file is up to its
///   rules; the entry goes, the file isn't touched.
/// - The object at that key was written after this upload (the same name
///   uploaded again): it's someone else's upload now, so it stays.
/// - Otherwise the file is deleted like "Delete Remote File", which
///   succeeds for one the rule already deleted.
async fn sweep_one(core: &SharedCore, record: &crate::history::UploadRecord) -> Result<(), String> {
    let destination = core.destinations.all().into_iter().find(|destination| destination.id == record.destination_id);
    let credentials = destination.as_ref().and_then(|destination| crate::credentials::load(&destination.id).ok());
    let (Some(destination), Some(credentials)) = (destination, credentials) else {
        return drop_entry(core, record);
    };
    if !is_active(core, &destination.id) {
        return drop_entry(core, record);
    }
    let storage = S3Provider::new(destination, credentials);
    let written = storage.last_modified(&record.object_key).await.map_err(|error| error.to_string())?;
    match written {
        None => drop_entry(core, record),
        Some(written) if uploaded_again(record, written) => drop_entry(core, record),
        Some(_) => crate::uploads::delete_expired(core, &record.id).await,
    }
}

/// Whether the object's last write is newer than this upload (or its last
/// replace): S3 keeps whole seconds, so anything more than a minute later
/// is a later upload to the same key.
fn uploaded_again(record: &crate::history::UploadRecord, written_at: i64) -> bool {
    written_at > record.last_write() + 60_000
}

fn drop_entry(core: &SharedCore, record: &crate::history::UploadRecord) -> Result<(), String> {
    core.history.delete(&record.id);
    core.notify(events::HISTORY_CHANGED);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_keys() {
        assert_eq!(expiring_key("2026/09/abc.png", 7), "tmp/7d/2026/09/abc.png");
        assert_eq!(expiring_key("shot.png", 30), "tmp/30d/shot.png");
        assert_eq!(rule_id(14), "aktar-expire-14d");
        assert_eq!(days_in_key("tmp/7d/2026/09/abc.png"), Some(7));
        assert_eq!(days_in_key("tmp/1d/a"), Some(1));
        assert_eq!(days_in_key("tmp/3d/a"), None);
        assert_eq!(days_in_key("tmp/14dx/a"), None);
        assert_eq!(days_in_key("2026/tmp/7d/a"), None);
        assert!(is_valid(1) && is_valid(30) && !is_valid(0) && !is_valid(2));
        assert_eq!(expires_at(1_000, 1), 1_000 + 86_400_000);
    }

    /// What R2 returns for a bucket it created: its own rule, with no
    /// `Filter`, which a typed round trip can't keep as it is.
    const R2_RULE: &str = "<ID>Default Multipart Abort Rule</ID><Status>Enabled</Status><AbortIncompleteMultipartUpload><DaysAfterInitiation>7</DaysAfterInitiation></AbortIncompleteMultipartUpload>";
    const USER_RULE: &str = "\n    <ID>user-logs</ID>\n    <Filter><And><Prefix>logs/</Prefix><Tag><Key>a&amp;b</Key><Value>1</Value></Tag></And></Filter>\n    <Status>Enabled</Status>\n    <Expiration><Days>90</Days></Expiration>\n  ";

    fn configuration(rules: &[&str]) -> String {
        let rules: String = rules.iter().map(|rule| format!("<Rule>{rule}</Rule>")).collect();
        format!(r#"<?xml version="1.0" encoding="UTF-8"?><LifecycleConfiguration xmlns="http://s3.amazonaws.com/doc/2006-03-01/">{rules}</LifecycleConfiguration>"#)
    }

    #[test]
    fn refuses_what_it_cant_read_as_a_configuration() {
        // A real one, with rules or without.
        let rules = parse_configuration(&configuration(&[R2_RULE, USER_RULE])).unwrap();
        assert_eq!(rules.len(), 2);
        assert_eq!(parse_configuration("<LifecycleConfiguration/>"), Some(Vec::new()));
        // A proxy's page, or another listing, answered with 200.
        assert_eq!(parse_configuration("<html><body>Sign in to the Wi-Fi</body></html>"), None);
        assert_eq!(parse_configuration("<ListBucketResult><Contents><Key>a</Key></Contents></ListBucketResult>"), None);
        // Namespace-prefixed elements this parser doesn't follow.
        assert_eq!(
            parse_configuration("<s3:LifecycleConfiguration><s3:Rule><s3:ID>x</s3:ID></s3:Rule></s3:LifecycleConfiguration>"),
            None
        );
        // A rule that doesn't close is one the rewrite would lose.
        assert_eq!(parse_configuration("<LifecycleConfiguration><Rule><ID>x</ID></LifecycleConfiguration>"), None);
    }

    #[test]
    fn reports_missing_rules() {
        let rules = parse_rules(&configuration(&[&aktar_rule_xml(1), &aktar_rule_xml(7), R2_RULE]));
        assert!(!rules_in_place(&rules));
        assert_eq!(missing_durations(&rules), vec![14, 30]);
        let all: Vec<String> = all_aktar_rules();
        let all: Vec<&str> = all.iter().map(String::as_str).collect();
        let rules = parse_rules(&configuration(&all));
        assert!(rules_in_place(&rules));
        assert!(missing_durations(&rules).is_empty());
    }

    fn all_aktar_rules() -> Vec<String> {
        DURATIONS.into_iter().map(aktar_rule_xml).collect()
    }

    #[test]
    fn parses_rules() {
        let legacy = "<ID>aktar-expire-7d</ID><Prefix>tmp/7d/</Prefix><Status>Enabled</Status><Expiration><Days>7</Days></Expiration>";
        let rules = parse_rules(&configuration(&[R2_RULE, USER_RULE, legacy, "<ID>empty</ID><Filter/><Status>Disabled</Status>"]));
        assert_eq!(rules.len(), 4);

        assert_eq!(rules[0].id.as_deref(), Some("Default Multipart Abort Rule"));
        assert_eq!(rules[0].prefix, None);
        assert_eq!(rules[0].status.as_deref(), Some("Enabled"));
        // DaysAfterInitiation isn't an expiration.
        assert_eq!(rules[0].expiration_days, None);
        assert_eq!(rules[0].raw, R2_RULE);

        assert_eq!(rules[1].id.as_deref(), Some("user-logs"));
        assert_eq!(rules[1].prefix.as_deref(), Some("logs/"));
        assert_eq!(rules[1].expiration_days, Some(90));
        assert_eq!(rules[1].raw, USER_RULE);

        // The legacy rule-level prefix counts.
        assert!(matches_aktar_rule(&rules[2], 7));
        // `<Filter/>` means no prefix, not the rule's.
        assert_eq!(rules[3].prefix, None);

        assert!(parse_rules(r#"<LifecycleConfiguration xmlns="x"/>"#).is_empty());
        assert_eq!(
            error_code("<Error><Code>NoSuchLifecycleConfiguration</Code><Message>none &amp; more</Message></Error>"),
            (Some("NoSuchLifecycleConfiguration".into()), Some("none & more".into()))
        );
    }

    #[test]
    fn adds_rules_and_keeps_foreign_ones_verbatim() {
        // An outdated Aktar rule gets replaced, not duplicated.
        let stale = "<ID>aktar-expire-7d</ID><Filter><Prefix>tmp/7d/</Prefix></Filter><Status>Disabled</Status><Expiration><Days>7</Days></Expiration>";
        let merged = merged_rules(&parse_rules(&configuration(&[R2_RULE, stale, USER_RULE]))).expect("rules are missing");
        let mut expected = vec![R2_RULE.to_string(), USER_RULE.to_string()];
        expected.extend(all_aktar_rules());
        let expected: Vec<&str> = expected.iter().map(String::as_str).collect();
        assert_eq!(merged, configuration(&expected));
        // The R2 rule goes back byte for byte.
        assert!(merged.contains(&format!("<Rule>{R2_RULE}</Rule>")));
        assert_eq!(
            aktar_rule_xml(7),
            "<ID>aktar-expire-7d</ID><Filter><Prefix>tmp/7d/</Prefix></Filter><Status>Enabled</Status><Expiration><Days>7</Days></Expiration>"
        );

        let reparsed = parse_rules(&merged);
        for days in DURATIONS {
            let matching: Vec<_> = reparsed.iter().filter(|rule| rule.id == Some(rule_id(days))).collect();
            assert_eq!(matching.len(), 1);
            assert!(matches_aktar_rule(matching[0], days));
        }
    }

    #[test]
    fn leaves_complete_rules_alone() {
        let mut rules = vec![R2_RULE.to_string()];
        rules.extend(all_aktar_rules());
        let rules: Vec<&str> = rules.iter().map(String::as_str).collect();
        assert_eq!(merged_rules(&parse_rules(&configuration(&rules))), None);
        // Applying the merge's own result changes nothing further.
        let merged = merged_rules(&parse_rules(&configuration(&[R2_RULE]))).unwrap();
        assert_eq!(merged_rules(&parse_rules(&merged)), None);
        assert_eq!(merged_rules(&[]).map(|xml| parse_rules(&xml).len()), Some(4));
    }

    #[test]
    fn removes_only_aktar_rules() {
        let mut rules = vec![R2_RULE.to_string(), USER_RULE.to_string()];
        rules.extend(all_aktar_rules());
        let rules: Vec<&str> = rules.iter().map(String::as_str).collect();
        assert_eq!(
            without_aktar_rules(&parse_rules(&configuration(&rules))),
            Removal::Put(configuration(&[R2_RULE, USER_RULE]))
        );

        let aktar = all_aktar_rules();
        let aktar: Vec<&str> = aktar.iter().map(String::as_str).collect();
        assert_eq!(without_aktar_rules(&parse_rules(&configuration(&aktar))), Removal::DeleteConfiguration);
        assert_eq!(without_aktar_rules(&parse_rules(&configuration(&[R2_RULE]))), Removal::Nothing);
        assert_eq!(without_aktar_rules(&[]), Removal::Nothing);
    }
}
