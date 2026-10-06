//! "Set Up Cloudflare R2": one API token, made from a link that fills in
//! its permissions, is all it takes. With it Aktar finds the account,
//! creates the bucket, turns on a public link (r2.dev or a domain on
//! Cloudflare) and derives the S3 keys: an R2 token's ID is the access key
//! and the SHA-256 of its value the secret
//! (https://developers.cloudflare.com/r2/api/tokens/). The token itself
//! isn't kept; only the derived keys go to Credential Manager. Same steps
//! and calls as the Mac app (mac/docs/cloudflare-setup.md).

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::cloudflare::{client, result, API};
use crate::credentials::StorageCredentials;
use crate::destinations::{DestinationConfig, ProviderPreset};
use crate::output::{CLEAN_URL_TEMPLATE, DEFAULT_OBJECT_PATH_TEMPLATE};
use crate::t;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Zone {
    pub id: String,
    pub name: String,
}

/// The Cloudflare page that creates the token, with R2 (to create the
/// bucket and upload) and zone read (to list domains for public links)
/// already chosen.
pub fn token_url() -> String {
    let permissions = r#"[{"key":"workers_r2","type":"edit"},{"key":"zone","type":"read"}]"#;
    url::Url::parse_with_params(
        "https://dash.cloudflare.com/profile/api-tokens",
        [("permissionGroupKeys", permissions), ("accountId", "*"), ("zoneId", "all"), ("name", "Aktar")],
    )
    .map(String::from)
    .unwrap_or_default()
}

/// The token's ID, which is also its S3 access key ID.
pub async fn verify(token: &str) -> Result<String, String> {
    let result = request(reqwest::Method::GET, "user/tokens/verify", token, None).await?;
    let Some(id) = result["id"].as_str() else {
        return Err(t!("Cloudflare refused the request."));
    };
    if result["status"].as_str().unwrap_or("active") != "active" {
        return Err(t!("This token isn’t active. Create a new one with the button above."));
    }
    Ok(id.to_string())
}

pub async fn accounts(token: &str) -> Result<Vec<Account>, String> {
    let result = request(reqwest::Method::GET, "accounts?per_page=50", token, None).await?;
    Ok(items(&result)
        .filter_map(|item| {
            let id = item["id"].as_str().filter(|id| is_valid_id(id))?;
            Some(Account { id: id.to_string(), name: item["name"].as_str().unwrap_or(id).to_string() })
        })
        .collect())
}

pub async fn buckets(account: &str, token: &str) -> Result<Vec<String>, String> {
    let path = format!("accounts/{}/r2/buckets?per_page=1000", checked_id(account)?);
    let result = request(reqwest::Method::GET, &path, token, None).await?;
    let mut names: Vec<String> = items(&result["buckets"]).filter_map(|item| item["name"].as_str().map(str::to_string)).collect();
    names.sort();
    Ok(names)
}

pub async fn create_bucket(name: &str, account: &str, token: &str) -> Result<(), String> {
    let path = format!("accounts/{}/r2/buckets", checked_id(account)?);
    request(reqwest::Method::POST, &path, token, Some(json!({ "name": checked_bucket(name)? }))).await.map(drop)
}

/// Turns on the bucket's r2.dev address and returns it as a base URL.
pub async fn enable_public_dev_url(bucket: &str, account: &str, token: &str) -> Result<String, String> {
    let path = format!("accounts/{}/r2/buckets/{}/domains/managed", checked_id(account)?, checked_bucket(bucket)?);
    let result = request(reqwest::Method::PUT, &path, token, Some(json!({ "enabled": true }))).await?;
    match result["domain"].as_str().filter(|domain| !domain.is_empty()) {
        Some(domain) => Ok(format!("https://{domain}")),
        None => Err(t!("Cloudflare didn’t return the bucket’s r2.dev address.")),
    }
}

/// The account's active domains, for a public link like files.example.com.
pub async fn zones(account: &str, token: &str) -> Result<Vec<Zone>, String> {
    let path = format!("zones?account.id={}&status=active&per_page=50", checked_id(account)?);
    let result = request(reqwest::Method::GET, &path, token, None).await?;
    let mut zones: Vec<Zone> = items(&result)
        .filter_map(|item| {
            let id = item["id"].as_str().filter(|id| is_valid_id(id))?;
            Some(Zone { id: id.to_string(), name: item["name"].as_str()?.to_string() })
        })
        .collect();
    zones.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(zones)
}

/// The domains already connected to the bucket.
pub async fn custom_domains(bucket: &str, account: &str, token: &str) -> Result<Vec<String>, String> {
    let path = format!("accounts/{}/r2/buckets/{}/domains/custom", checked_id(account)?, checked_bucket(bucket)?);
    let result = request(reqwest::Method::GET, &path, token, None).await?;
    Ok(items(&result["domains"]).filter_map(|item| item["domain"].as_str().map(str::to_string)).collect())
}

/// Connects `domain` (on `zone`) to the bucket. Cloudflare adds the DNS
/// record and certificate; it takes a few minutes to become active.
pub async fn attach_domain(domain: &str, zone: &Zone, bucket: &str, account: &str, token: &str) -> Result<(), String> {
    let path = format!("accounts/{}/r2/buckets/{}/domains/custom", checked_id(account)?, checked_bucket(bucket)?);
    let body = json!({ "domain": domain, "zoneId": checked_id(&zone.id)?, "enabled": true, "minTLS": "1.2" });
    request(reqwest::Method::POST, &path, token, Some(body)).await.map(drop)
}

/// The S3 keys an R2 token stands for.
pub fn credentials(token_id: &str, token: &str) -> StorageCredentials {
    let digest = <sha2::Sha256 as sha2::Digest>::digest(token.as_bytes());
    StorageCredentials {
        access_key_id: token_id.to_string(),
        secret_access_key: crate::util::hex(&digest),
        session_token: None,
        cloudflare_token: None,
        short_link_token: None,
    }
}

/// The destination the setup saves: a normal R2 one. Links on a domain of
/// the user's own (`own_domain`) get the shortest paths.
pub fn destination(name: &str, account: &str, bucket: &str, public_base_url: &str, own_domain: bool) -> DestinationConfig {
    DestinationConfig {
        id: crate::util::new_id(),
        name: name.trim().to_string(),
        preset: ProviderPreset::CloudflareR2,
        account_id: Some(account.to_string()),
        endpoint: format!("https://{account}.r2.cloudflarestorage.com"),
        region: "auto".into(),
        bucket: bucket.to_string(),
        public_base_url: public_base_url.to_string(),
        object_path_template: if own_domain { CLEAN_URL_TEMPLATE } else { DEFAULT_OBJECT_PATH_TEMPLATE }.into(),
        force_path_style: false,
        is_default: false,
        output_mode: None,
        expiry_days: None,
        temporary_link: None,
        image_metadata: None,
        folder_upload: None,
        image_processing: None,
        thumbnails: None,
        thumbnail_prefix: None,
        use_for: None,
        short_cache: None,
        cloudflare_zone_id: None,
        hooks: None,
        short_links: None,
    }
}

/// A bucket name Cloudflare accepts: 3 to 63 lowercase letters, digits
/// and hyphens, starting and ending with a letter or digit.
pub fn is_valid_bucket_name(name: &str) -> bool {
    (3..=63).contains(&name.len())
        && name.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

/// A host name inside `zone`: the zone itself or a subdomain of it.
pub fn is_valid_domain(domain: &str, zone: &str) -> bool {
    let host = domain.to_ascii_lowercase();
    if host != zone && !host.ends_with(&format!(".{zone}")) {
        return false;
    }
    let labels: Vec<&str> = host.split('.').collect();
    let Some((top, names)) = labels.split_last() else { return false };
    !names.is_empty()
        && top.len() >= 2
        && top.bytes().all(|byte| byte.is_ascii_lowercase())
        && names.iter().all(|label| {
            !label.is_empty()
                && label.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
                && !label.starts_with('-')
                && !label.ends_with('-')
        })
}

// MARK: - Requests

/// An account or zone ID as Cloudflare gives them (32 hex digits), so
/// nothing else ends up in a request path.
fn is_valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

fn checked_id(id: &str) -> Result<&str, String> {
    if is_valid_id(id) {
        Ok(id)
    } else {
        Err(t!("Cloudflare refused the request."))
    }
}

fn checked_bucket(name: &str) -> Result<&str, String> {
    if is_valid_bucket_name(name) {
        Ok(name)
    } else {
        Err(t!("Use 3 to 63 lowercase letters, numbers and hyphens, starting and ending with a letter or number."))
    }
}

/// The objects in a list `result`.
fn items(list: &Value) -> impl Iterator<Item = &Value> {
    list.as_array().into_iter().flatten()
}

/// Sends a request with the token and returns Cloudflare's `result`. The
/// token only goes into the Authorization header, never into a message.
async fn request(method: reqwest::Method, path: &str, token: &str, body: Option<Value>) -> Result<Value, String> {
    let mut request = client()?.request(method, format!("{API}/{path}")).bearer_auth(token.trim());
    if let Some(body) = body {
        request = request.header("Content-Type", "application/json").body(body.to_string());
    }
    let response = request.send().await.map_err(|error| t!("Cloudflare didn’t answer. {0}", error.without_url()))?;
    result(response).await.map_err(|message| message.unwrap_or_else(|| t!("Cloudflare refused the request.")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_url_fills_in_the_permissions() {
        let url = url::Url::parse(&token_url()).unwrap();
        assert_eq!(url.host_str(), Some("dash.cloudflare.com"));
        assert_eq!(url.path(), "/profile/api-tokens");
        let query: Vec<(String, String)> = url.query_pairs().map(|(key, value)| (key.into_owned(), value.into_owned())).collect();
        assert_eq!(
            query,
            [
                ("permissionGroupKeys".into(), r#"[{"key":"workers_r2","type":"edit"},{"key":"zone","type":"read"}]"#.into()),
                ("accountId".into(), "*".into()),
                ("zoneId".into(), "all".into()),
                ("name".into(), "Aktar".into()),
            ]
        );
    }

    #[test]
    fn credentials_are_the_token_id_and_its_sha256() {
        let keys = credentials("token-id", "abc");
        assert_eq!(keys.access_key_id, "token-id");
        // SHA-256 of "abc", the standard test vector.
        assert_eq!(keys.secret_access_key, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"); // gitleaks:allow
        assert!(keys.session_token.is_none() && keys.cloudflare_token.is_none());
    }

    #[test]
    fn bucket_names() {
        for name in ["aktar", "aktar-2", "a1b", &"a".repeat(63)] {
            assert!(is_valid_bucket_name(name), "{name}");
        }
        for name in ["", "ab", "-aktar", "aktar-", "Aktar", "akt_ar", "akt.ar", &"a".repeat(64)] {
            assert!(!is_valid_bucket_name(name), "{name}");
        }
    }

    #[test]
    fn domains_inside_the_zone() {
        assert!(is_valid_domain("files.example.com", "example.com"));
        assert!(is_valid_domain("example.com", "example.com"));
        assert!(is_valid_domain("a.b.example.co", "example.co"));
        assert!(!is_valid_domain("files.example.org", "example.com"));
        assert!(!is_valid_domain("filesexample.com", "example.com"));
        assert!(!is_valid_domain("-files.example.com", "example.com"));
        assert!(!is_valid_domain("fi_les.example.com", "example.com"));
        assert!(!is_valid_domain("a..example.com", "example.com"));
        assert!(!is_valid_domain("localhost", "localhost"));
    }

    #[test]
    fn destination_is_a_normal_r2_one() {
        let config = destination(" Cloudflare R2 ", "0123456789abcdef0123456789abcdef", "aktar", "https://pub-1.r2.dev", false);
        assert_eq!(config.name, "Cloudflare R2");
        assert_eq!(config.preset, ProviderPreset::CloudflareR2);
        assert_eq!(config.endpoint, "https://0123456789abcdef0123456789abcdef.r2.cloudflarestorage.com");
        assert_eq!(config.region, "auto");
        assert_eq!(config.object_path_template, "{year}/{month}/{short}.{ext}");
        assert!(!config.force_path_style && !config.id.is_empty());
        let own = destination("R2", "0123456789abcdef0123456789abcdef", "aktar", "https://files.example.com", true);
        assert_eq!(own.object_path_template, "{short}.{ext}");
    }

    #[test]
    fn ids_stay_out_of_paths_unless_plain() {
        assert!(is_valid_id("0123456789abcdef0123456789abcdef"));
        assert!(!is_valid_id("abc/../zones"));
        assert!(!is_valid_id(""));
    }
}
