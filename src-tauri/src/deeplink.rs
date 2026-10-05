//! Handles `aktar://` links:
//!
//! ```text
//! aktar://upload-clipboard   upload whatever is on the clipboard (only
//!                            while Aktar is already running, and once
//!                            the user says so)
//! aktar://library            open the Library window
//! aktar://settings           open Settings
//! aktar://watch              open Settings on Watched Folders
//! aktar://watch/pause?minutes=60
//!                            pause watched folders (for good without
//!                            minutes), once the user says so
//! aktar://watch/resume       resume them
//! aktar://connect?callback=raycast://extensions/<author>/<extension>/<command>
//! aktar://import#<data>      open "Import from Another Device" with this
//!                            transfer link (see `transfer`)
//! ```
//!
//! `connect` is how the Raycast extension pairs: after the user approves,
//! the local API is turned on and its port and token are handed back to the
//! callback as Raycast launch context. Only callbacks into the official
//! extension (`merttopuz/aktar`) are accepted, so a link from a web page or
//! another extension can't collect the token.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::AppHandle;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_opener::OpenerExt;
use url::Url;

use crate::core::SharedCore;
use crate::local_api::Status;
use crate::windows::AppWindow;
use crate::t;

/// `launched_app` is true when this link is what started Aktar. A web page
/// can open aktar:// links, so uploading the clipboard only works in an app
/// the user already had running, never as a side effect of launching it.
pub fn handle(core: &SharedCore, link: &str, launched_app: bool) {
    if is_repeat(link) {
        return;
    }
    let Ok(url) = Url::parse(link) else { return };
    if !url.scheme().eq_ignore_ascii_case("aktar") {
        return;
    }
    let action = url.host_str().unwrap_or_default().to_ascii_lowercase();
    match action.as_str() {
        // Asked about every time: a web page can open these links.
        "upload-clipboard" if !launched_app => {
            let core = core.clone();
            tauri::async_runtime::spawn(async move { upload_clipboard(&core).await });
        }
        "library" => crate::windows::open(&core.app, AppWindow::Library),
        "settings" => crate::windows::open(&core.app, AppWindow::Settings),
        "watch" => match url.path().trim_matches('/').to_ascii_lowercase().as_str() {
            // Off the UI thread: they write the folder list. Pausing is
            // asked about first; minutes that aren't a number from 1 up
            // make the link do nothing.
            "pause" => {
                let Ok(minutes) = minutes(&url) else { return };
                let core = core.clone();
                tauri::async_runtime::spawn(async move {
                    let message = match minutes {
                        Some(minutes) => t!("A link asked Aktar to pause watched folders for {0} minutes.", minutes),
                        None => t!("A link asked Aktar to pause watched folders until you resume them."),
                    };
                    if ask(&core.app, t!("Pause Watched Folders?"), message, t!("Pause")).await {
                        let _ = tauri::async_runtime::spawn_blocking(move || crate::watched::pause(&core, minutes)).await;
                    }
                });
            }
            "resume" => {
                let core = core.clone();
                tauri::async_runtime::spawn_blocking(move || crate::watched::resume(&core));
            }
            _ => crate::watched::show_settings(core),
        },
        // Only opens the import with the link filled in, also when it
        // launched the app: the transfer code still has to be typed, and
        // Import pressed, before anything is saved.
        "import" => crate::watched::show_import(core, link.trim()),
        "connect" => {
            let core = core.clone();
            tauri::async_runtime::spawn(async move { connect(&core, &url).await });
        }
        _ => {}
    }
}

/// `minutes=` of aktar://watch/pause: None without one (until resumed),
/// a whole number from 1 (up to a year), or Err for anything else.
fn minutes(url: &Url) -> Result<Option<u64>, ()> {
    match url.query_pairs().find(|(name, _)| name == "minutes") {
        None => Ok(None),
        Some((_, value)) => crate::watched::pause_minutes_text(&value).map(Some).ok_or(()),
    }
}

/// aktar://upload-clipboard: says what's on the clipboard and where it
/// would go, and uploads it only once the user says so.
async fn upload_clipboard(core: &SharedCore) {
    let inputs = tauri::async_runtime::spawn_blocking(crate::clipboard::read_inputs).await.unwrap_or_default();
    if inputs.is_empty() {
        crate::uploads::show_notification(core, "Aktar", &t!("The clipboard has no file or image to upload."));
        return;
    }
    let what = match inputs.as_slice() {
        [input] if input.temporary => t!("an image from the clipboard"),
        [input] => format!("“{}”", input.original_filename),
        _ => t!("{0} files", inputs.len()),
    };
    // Named as "Use for" routes it, not just the default destination.
    let names = crate::uploads::routed_destination_names(core, &inputs);
    if names.is_empty() {
        // Nowhere to upload to: `enqueue` says so and opens Welcome.
        crate::uploads::enqueue(core, inputs, None);
        return;
    }
    let message = match names.as_slice() {
        [name] => t!("A link asked Aktar to upload {0} from the clipboard to “{1}”.", what, name),
        _ => {
            let destinations = names.iter().map(|name| format!("“{name}”")).collect::<Vec<_>>().join(", ");
            t!("A link asked Aktar to upload {0} from the clipboard, split by Use For between {1}.", what, destinations)
        }
    };
    if ask(&core.app, t!("Upload the Clipboard?"), message, t!("Upload")).await {
        crate::uploads::enqueue(core, inputs, None);
    } else {
        inputs.iter().for_each(crate::uploads::UploadInput::remove_if_temporary);
    }
}

/// The launch link can show up both in the launch arguments and as an
/// "opened" event; running it twice would, for example, ask to connect
/// Raycast twice.
fn is_repeat(link: &str) -> bool {
    static LAST: Mutex<Option<(String, Instant)>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap();
    let repeat = last
        .as_ref()
        .is_some_and(|(previous, at)| previous == link && at.elapsed() < Duration::from_secs(2));
    *last = Some((link.to_string(), Instant::now()));
    repeat
}

/// A link delivered within a couple of seconds of launch is treated as the
/// one that launched the app.
pub fn is_launch_link(core: &SharedCore) -> bool {
    core.launched_at.elapsed() < Duration::from_secs(2)
}

/// Raycast deeplinks are raycast://extensions/<author>/<extension>/<command>.
/// The whole path is checked, not just its start, so something like
/// "/merttopuz/aktar/../../other/extension/command" can't slip through.
pub fn is_allowed_extension_path(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() != 4 || !parts[0].is_empty() {
        return false;
    }
    if !parts[1].eq_ignore_ascii_case("merttopuz") || !parts[2].eq_ignore_ascii_case("aktar") {
        return false;
    }
    let command = parts[3];
    !command.is_empty() && command.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn allowed_callback(url: &Url) -> Option<Url> {
    let raw = url.query_pairs().find(|(name, _)| name == "callback")?.1.into_owned();
    let callback = Url::parse(&raw).ok()?;
    let valid = callback.scheme().eq_ignore_ascii_case("raycast")
        && callback.host_str().is_some_and(|host| host.eq_ignore_ascii_case("extensions"))
        && is_allowed_extension_path(callback.path());
    valid.then_some(callback)
}

async fn connect(core: &SharedCore, url: &Url) {
    let Some(mut callback) = allowed_callback(url) else { return };

    let extension = callback.path().trim_matches('/').to_string();
    let approved = ask(
        &core.app,
        t!("Connect Raycast to Aktar?"),
        t!("The Raycast extension “{0}” wants to upload files, browse your buckets, and manage your upload history through Aktar. This turns on Aktar’s local API, which you can turn off any time in Settings > Integrations.", extension),
        t!("Connect"),
    )
    .await;
    if !approved {
        return;
    }

    crate::local_api::set_enabled(core, true);
    // Only hand the connection over once the server is actually listening;
    // if the port is taken, say so here instead of pairing Raycast with a
    // server that isn't there.
    let mut status = crate::local_api::state(core).status;
    for _ in 0..40 {
        if matches!(status, Status::Running | Status::Failed { .. }) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        status = crate::local_api::state(core).status;
    }
    if let Status::Failed { message } = status {
        core.app
            .dialog()
            .message(message)
            .title(t!("Something went wrong"))
            .kind(MessageDialogKind::Error)
            .show(|_| {});
        return;
    }
    let (port, token) = (core.settings.get().local_api_port, core.local_api.token());
    let context = serde_json::json!({ "aktar": { "port": port, "token": token } }).to_string();

    // Everything the extension put on the callback (like its pairing nonce
    // in fallbackText) goes back untouched. Encoded like the Mac app does
    // (URLComponents), with spaces as %20: form encoding's "+" would come
    // back to Raycast as a literal plus.
    let mut pairs: Vec<(String, String)> = callback
        .query_pairs()
        .filter(|(name, _)| name != "launchContext")
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect();
    pairs.push(("launchContext".into(), context));
    let query = pairs
        .iter()
        .map(|(name, value)| format!("{}={}", query_component(name), query_component(value)))
        .collect::<Vec<_>>()
        .join("&");
    callback.set_query(Some(&query));
    if let Err(error) = core.app.opener().open_url(callback.as_str(), None::<&str>) {
        log::warn!("Could not open the Raycast callback: {error}");
    }
}

/// Percent-encodes everything but unreserved characters.
fn query_component(text: &str) -> String {
    const RESERVED: &percent_encoding::AsciiSet =
        &percent_encoding::NON_ALPHANUMERIC.remove(b'-').remove(b'.').remove(b'_').remove(b'~');
    percent_encoding::utf8_percent_encode(text, RESERVED).to_string()
}

/// A native dialog of the app's own, which no window can answer for the
/// user. True when `action` was picked.
async fn ask(app: &AppHandle, title: String, message: String, action: String) -> bool {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.dialog()
        .message(message)
        .title(title)
        .kind(MessageDialogKind::Info)
        .buttons(MessageDialogButtons::OkCancelCustom(action, t!("Cancel")))
        .show(move |approved| {
            let _ = sender.send(approved);
        });
    receiver.await.unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_accepts_the_official_extension() {
        assert!(is_allowed_extension_path("/merttopuz/aktar/connect"));
        assert!(is_allowed_extension_path("/MertTopuz/Aktar/connect-2"));
        assert!(!is_allowed_extension_path("/merttopuz/aktar/../../x/y/z"));
        assert!(!is_allowed_extension_path("/someone/aktar/connect"));
        assert!(!is_allowed_extension_path("/merttopuz/aktar/"));
        assert!(!is_allowed_extension_path("/merttopuz/aktar/con nect"));
    }

    #[test]
    fn validates_callbacks() {
        let good = Url::parse("aktar://connect?callback=raycast%3A%2F%2Fextensions%2Fmerttopuz%2Faktar%2Fconnect%3FfallbackText%3Dabc").unwrap();
        let callback = allowed_callback(&good).unwrap();
        assert_eq!(callback.query_pairs().next().unwrap().1, "abc");
        let bad = Url::parse("aktar://connect?callback=https%3A%2F%2Fevil.example%2Fmerttopuz%2Faktar%2Fconnect").unwrap();
        assert!(allowed_callback(&bad).is_none());
    }

    #[test]
    fn reads_pause_minutes() {
        assert_eq!(minutes(&Url::parse("aktar://watch/pause?minutes=60").unwrap()), Ok(Some(60)));
        assert_eq!(minutes(&Url::parse("aktar://watch/pause").unwrap()), Ok(None));
        assert_eq!(minutes(&Url::parse("aktar://watch/pause?minutes=99999999").unwrap()), Ok(Some(525_600)));
        for invalid in ["soon", "0", "-5", ""] {
            assert_eq!(minutes(&Url::parse(&format!("aktar://watch/pause?minutes={invalid}")).unwrap()), Err(()), "{invalid}");
        }
    }

    #[test]
    fn keeps_the_transfer_in_the_fragment() {
        let url = Url::parse("aktar://import#AQABAgMEBQYHCAkKCwwNDg-goaKj").unwrap();
        assert_eq!(url.host_str(), Some("import"));
        assert_eq!(url.fragment(), Some("AQABAgMEBQYHCAkKCwwNDg-goaKj"));
    }

    #[test]
    fn encodes_spaces_as_percent_20() {
        assert_eq!(query_component("a b+c/{\"x\":1}"), "a%20b%2Bc%2F%7B%22x%22%3A1%7D");
    }
}
