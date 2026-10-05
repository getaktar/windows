//! Clearing Cloudflare's cache for a replaced file, so its link shows the
//! new version right away instead of after the cache time runs out. Uses a
//! token the user makes in the Cloudflare dashboard with only
//! Zone > Cache Purge, kept in Credential Manager with the destination's
//! keys, and the zone ID from the destination's settings.

use std::time::Duration;

use serde_json::Value;

use crate::t;

pub(crate) const API: &str = "https://api.cloudflare.com/client/v4";
const TIMEOUT: Duration = Duration::from_secs(20);

pub(crate) fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder().timeout(TIMEOUT).build().map_err(|error| error.to_string())
}

/// Cloudflare answers `{"success": bool, "errors": [{"message"}], "result": ...}`:
/// `result`, or the errors' messages (none when it didn't give any).
pub(crate) async fn result(response: reqwest::Response) -> Result<Value, Option<String>> {
    let status = response.status();
    let bytes = response.bytes().await.unwrap_or_default();
    let mut body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if status.is_success() && body["success"].as_bool() == Some(true) {
        return Ok(body["result"].take());
    }
    let messages: Vec<&str> = body["errors"].as_array().into_iter().flatten().filter_map(|error| error["message"].as_str()).collect();
    Err(Some(messages.join(" ")).filter(|message| !message.is_empty()))
}

/// Cloudflare's answer: `success`, or the errors' messages.
async fn outcome(response: reqwest::Response) -> Result<(), String> {
    let status = response.status();
    result(response).await.map(drop).map_err(|message| message.unwrap_or_else(|| format!("HTTP {}", status.as_u16())))
}

/// "Check" in the destination form: whether the token is valid and active.
pub async fn verify(token: &str) -> Result<(), String> {
    let token = token.trim();
    if token.is_empty() {
        return Err(t!("Enter a Cloudflare API token."));
    }
    let response = client()?
        .get(format!("{API}/user/tokens/verify"))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    outcome(response).await
}

/// Clears `urls` from the zone's cache.
pub async fn purge(zone_id: &str, token: &str, urls: &[String]) -> Result<(), String> {
    let response = client()?
        .post(format!("{API}/zones/{zone_id}/purge_cache"))
        .bearer_auth(token.trim())
        .header("Content-Type", "application/json")
        .body(serde_json::json!({ "files": urls }).to_string())
        .send()
        .await
        .map_err(|error| error.to_string())?;
    outcome(response).await
}

/// After a file was replaced: clears its public URL from Cloudflare's cache
/// when the destination is set up for it. A failure never undoes the
/// replace; it's said in a notification.
pub fn purge_in_background(core: &crate::core::SharedCore, destination: &crate::destinations::DestinationConfig, public_url: &str) {
    let Some(zone_id) = destination.cloudflare_zone_id.clone() else { return };
    let Some(token) = crate::credentials::load(&destination.id).ok().and_then(|keys| keys.cloudflare_token).filter(|token| !token.is_empty())
    else {
        return;
    };
    let (core, url) = (core.clone(), public_url.to_string());
    tauri::async_runtime::spawn(async move {
        if let Err(message) = purge(&zone_id, &token, std::slice::from_ref(&url)).await {
            crate::uploads::show_notification(
                &core,
                &t!("Cloudflare's cache couldn't be cleared"),
                &t!("The file was replaced, but Cloudflare's cache couldn't be cleared: {0}", message),
            );
        }
    });
}
