//! A destination's "After Upload" hooks: the same webhooks and scripts as a
//! watched folder's Automation, run after each upload to the destination
//! that isn't from a watched folder (those run their folder's own), and
//! after a file there is replaced. Never for a reused link. A failing hook
//! never fails the upload; it's said in a notification, at most once a
//! minute for each destination.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use serde_json::{json, Value};

use crate::core::SharedCore;
use crate::destinations::DestinationConfig;
use crate::t;
use crate::watched::hooks;
use crate::watched::model::Hook;

const NOTIFY_INTERVAL_MS: i64 = 60_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Uploaded,
    Replaced,
}

impl Event {
    fn name(self) -> &'static str {
        match self {
            Event::Uploaded => "upload.succeeded",
            Event::Replaced => "upload.replaced",
        }
    }
}

/// What a hook gets: the watched-folder payload's fields, with the
/// destination instead of a folder. `upload.shortUrl` is the upload's short
/// link, null when it has none; `upload.url` stays the original link.
#[allow(clippy::too_many_arguments)]
pub fn payload(
    event: Event,
    destination: &DestinationConfig,
    file: &Path,
    file_name: &str,
    size: i64,
    key: &str,
    url: &str,
    short_url: Option<&str>,
) -> Value {
    json!({
        "event": event.name(),
        "destination": { "id": destination.id, "name": destination.name },
        "file": { "path": file.to_string_lossy(), "name": file_name, "size": size },
        "upload": { "key": key, "url": url, "destinationID": destination.id, "reused": false, "shortUrl": short_url },
    })
}

/// Runs the destination's enabled hooks with `payload`, in the background.
pub fn run(core: &SharedCore, destination: &DestinationConfig, payload: Value) {
    for hook in destination.enabled_hooks() {
        let (core, destination, payload) = (core.clone(), destination.clone(), payload.clone());
        tauri::async_runtime::spawn(async move {
            if let Err(message) = hooks::send(&core, &hook, &payload).await {
                failed(&core, &destination, &hooks::failure(&hook, &message));
            }
        });
    }
}

/// "Test": a made-up upload of a file to the destination.
pub async fn test(core: &SharedCore, destination: &DestinationConfig, hook: &Hook) -> Result<(), String> {
    let file = std::env::temp_dir().join("example.png");
    let payload =
        payload(Event::Uploaded, destination, &file, "example.png", 12345, "example/example.png", "https://example.com/example/example.png", None);
    hooks::send(core, hook, &payload).await.map_err(|message| hooks::failure(hook, &message))
}

fn failed(core: &SharedCore, destination: &DestinationConfig, message: &str) {
    static LAST: OnceLock<Mutex<HashMap<String, i64>>> = OnceLock::new();
    let now = crate::util::now_millis();
    let due = {
        let mut last = LAST.get_or_init(Default::default).lock().unwrap();
        let due = last.get(&destination.id).is_none_or(|at| now - at >= NOTIFY_INTERVAL_MS);
        if due {
            last.insert(destination.id.clone(), now);
        }
        due
    };
    log::warn!("{}: {message}", destination.name);
    if due {
        crate::uploads::show_notification(core, &t!("After Upload: {0}", destination.name), message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_names_the_destination() {
        let destination: DestinationConfig = serde_json::from_value(json!({
            "id": "D1", "name": "Screenshots", "preset": "minIO", "endpoint": "e", "region": "r", "bucket": "b",
            "publicBaseURL": "https://p", "objectPathTemplate": "{uuid}.{ext}", "forcePathStyle": true,
        }))
        .unwrap();
        let value = payload(Event::Replaced, &destination, Path::new("C:\\shots\\a.png"), "a.png", 5, "2026/a.png", "https://p/2026/a.png", None);
        assert_eq!(value["event"], "upload.replaced");
        assert_eq!(value["destination"], json!({ "id": "D1", "name": "Screenshots" }));
        assert_eq!(value["upload"]["destinationID"], "D1");
        assert_eq!(value["upload"]["reused"], false);
        assert_eq!(value["file"]["name"], "a.png");
        assert!(value.get("folder").is_none());
        // No short link: null, written out.
        assert!(value["upload"]["shortUrl"].is_null());
        assert!(value["upload"].as_object().unwrap().contains_key("shortUrl"));
        let shortened = payload(Event::Uploaded, &destination, Path::new("a"), "a", 0, "k", "u", Some("https://s.example.com/A1"));
        assert_eq!(shortened["event"], "upload.succeeded");
        assert_eq!(shortened["upload"]["shortUrl"], "https://s.example.com/A1");
        assert_eq!(shortened["upload"]["url"], "u");
    }
}
