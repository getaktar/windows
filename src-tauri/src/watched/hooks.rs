//! A watched folder's automation: after each successful upload, its
//! webhooks get the upload as JSON, and its scripts get it on standard
//! input. Hooks never hold an upload up; one that fails or takes longer
//! than 10 s becomes the folder's last error, with a notification.

use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncWriteExt;

use super::engine::AfterSuccess;
use super::model::{Hook, HookKind, WatchedFolder};
use super::platform;
use crate::core::SharedCore;
use crate::t;

const TIMEOUT: Duration = Duration::from_secs(10);

pub fn run(core: &SharedCore, after: AfterSuccess) {
    for hook in after.hooks {
        let core = core.clone();
        let folder = after.folder.clone();
        let payload = after.payload.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(message) = send(&core, &hook, &payload).await {
                core.watched.engine.problem(&folder.id, failure(&hook, &message));
            }
        });
    }
}

/// "Test": a made-up upload of a file in the folder.
pub async fn test(core: &SharedCore, folder: &WatchedFolder, hook: &Hook) -> Result<(), String> {
    let path = folder.path.join("example.png");
    let payload = serde_json::json!({
        "event": "upload.succeeded",
        "folder": { "id": folder.id, "name": folder.name, "path": folder.path.to_string_lossy() },
        "file": { "path": path.to_string_lossy(), "name": "example.png", "size": 12345 },
        "upload": { "key": "example/example.png", "url": "https://example.com/example/example.png", "destinationID": folder.destination_id.clone().unwrap_or_default(), "reused": false },
    });
    send(core, hook, &payload).await.map_err(|message| failure(hook, &message))
}

fn failure(hook: &Hook, message: &str) -> String {
    match hook.kind {
        HookKind::Webhook => t!("Webhook {0} failed: {1}", hook.target, message),
        HookKind::Script => t!("Script {0} failed: {1}", crate::util::last_component(&hook.target), message),
    }
}

async fn send(core: &SharedCore, hook: &Hook, payload: &serde_json::Value) -> Result<(), String> {
    match tokio::time::timeout(TIMEOUT, async {
        match hook.kind {
            HookKind::Webhook => webhook(core, &hook.target, payload).await,
            HookKind::Script => script(&hook.target, payload).await,
        }
    })
    .await
    {
        Ok(result) => result,
        Err(_) => Err(t!("It took longer than 10 seconds.")),
    }
}

/// Whether a webhook may be sent to `url`: https:// anywhere, plain
/// http:// only to this PC or the local network, where nothing it sends
/// crosses the internet unencrypted.
pub fn check_webhook_url(url: &str) -> Result<(), String> {
    let parsed = url::Url::parse(url.trim()).map_err(|_| t!("The webhook address isn't a valid http or https URL."))?;
    match parsed.scheme() {
        "https" if parsed.host().is_some() => Ok(()),
        "http" if parsed.host().is_some_and(|host| is_local(&host)) => Ok(()),
        "http" if parsed.host().is_some() => Err(t!("Use https:// for this webhook. Plain http:// only works for this PC or your local network.")),
        _ => Err(t!("The webhook address isn't a valid http or https URL.")),
    }
}

/// This PC or the local network: localhost, `.local` names, and loopback,
/// private and link-local addresses.
fn is_local(host: &url::Host<&str>) -> bool {
    match host {
        url::Host::Domain(name) => {
            let name = name.trim_end_matches('.').to_ascii_lowercase();
            name == "localhost" || name.ends_with(".localhost") || name.ends_with(".local")
        }
        url::Host::Ipv4(address) => address.is_loopback() || address.is_private() || address.is_link_local(),
        url::Host::Ipv6(address) => {
            let first = address.segments()[0];
            address.is_loopback() || (first & 0xFE00) == 0xFC00 || (first & 0xFFC0) == 0xFE80
        }
    }
}

async fn webhook(core: &SharedCore, url: &str, payload: &serde_json::Value) -> Result<(), String> {
    check_webhook_url(url)?;
    // A redirect is only followed on the same host (and never to plain
    // http:// on the internet), so the upload's details aren't sent on to
    // somewhere else.
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let same_host = attempt.previous().first().is_some_and(|first| first.host_str() == attempt.url().host_str());
            if attempt.previous().len() > 5 || !same_host || check_webhook_url(attempt.url().as_str()).is_err() {
                attempt.stop()
            } else {
                attempt.follow()
            }
        }))
        .build()
        .map_err(|error| error.to_string())?;
    let response = client
        .post(url)
        .header("Content-Type", "application/json")
        .header("User-Agent", format!("Aktar/{}", core.app.package_info().version))
        .body(payload.to_string())
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!("HTTP {}", response.status().as_u16()))
    }
}

/// `.ps1` through PowerShell, anything else (`.bat` and `.cmd` included) run
/// as it is; without a console window either way. Batch files are spawned
/// directly rather than through `cmd /C` so the standard library quotes the
/// arguments, which can hold `&` (presigned URLs, file names).
///
/// The arguments are url, key, file and folder, as the Mac app passes them
/// (its sandboxed scripts can't get environment variables), so one script
/// works on both.
async fn script(path: &str, payload: &serde_json::Value) -> Result<(), String> {
    let extension = crate::util::split_extension(crate::util::last_component(path)).1.to_ascii_lowercase();
    let mut command = match extension.as_str() {
        "ps1" => {
            let mut command = tokio::process::Command::new("powershell");
            command.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", path]);
            command
        }
        _ => tokio::process::Command::new(path),
    };
    let upload = &payload["upload"];
    let url = upload["url"].as_str().unwrap_or_default();
    let key = upload["key"].as_str().unwrap_or_default();
    let file = payload["file"]["path"].as_str().unwrap_or_default();
    let folder = payload["folder"]["path"].as_str().unwrap_or_default();
    command
        .args([url, key, file, folder])
        .env("AKTAR_URL", url)
        .env("AKTAR_KEY", key)
        .env("AKTAR_FILE", file)
        .env("AKTAR_FOLDER", folder)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    platform::hide_console(&mut command);
    let mut child = command.spawn().map_err(|error| error.to_string())?;
    if let Some(mut stdin) = child.stdin.take() {
        // A script that doesn't read its input is fine.
        let _ = stdin.write_all(payload.to_string().as_bytes()).await;
    }
    let output = child.wait_with_output().await.map_err(|error| error.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr).trim().lines().last().unwrap_or_default().to_string();
    match (output.status.code(), detail.is_empty()) {
        (Some(code), true) => Err(t!("It exited with code {0}.", code)),
        (Some(code), false) => Err(format!("{} {detail}", t!("It exited with code {0}.", code))),
        (None, _) => Err(detail),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_sends_plain_http_nearby() {
        for allowed in [
            "https://hooks.example.com/x",
            "http://localhost:8080/hook",
            "http://127.0.0.1/hook",
            "http://192.168.1.20/hook",
            "http://10.0.0.5:3000",
            "http://172.20.1.1",
            "http://169.254.10.10",
            "http://nas.local/hook",
            "http://[::1]:9000/",
            "http://[fd12::1]/",
            "http://[fe80::1]/",
        ] {
            assert!(check_webhook_url(allowed).is_ok(), "{allowed}");
        }
        for refused in ["http://hooks.example.com/x", "http://8.8.8.8/", "http://172.32.0.1/", "http://[2001:db8::1]/", "ftp://x.dev", "hooks.example.com", "https://"] {
            assert!(check_webhook_url(refused).is_err(), "{refused}");
        }
    }
}
