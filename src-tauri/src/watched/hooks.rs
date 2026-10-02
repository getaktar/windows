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

async fn webhook(core: &SharedCore, url: &str, payload: &serde_json::Value) -> Result<(), String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(t!("The URL must start with https:// or http://."));
    }
    let client = reqwest::Client::builder().timeout(TIMEOUT).build().map_err(|error| error.to_string())?;
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
