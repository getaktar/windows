//! Capture (the Mac app's docs/capture-and-instant-links.md, "Capture"):
//! Windows' own snipping bar (Windows+Shift+S, the Snipping Tool) picks an
//! area, a window or the whole screen and puts the snip on the clipboard,
//! where Aktar takes it and uploads it like a pasted image.
//!
//! Snipping Tool's newer `ms-screenclip://capture` protocol only answers
//! packaged apps (and the old `ms-screenclip:` launch is retired), so the
//! snipping bar is opened with its shortcut and the clipboard watched: the
//! first image put there while the bar is up, or within a moment after it
//! closes, is the snip. A bar closed without a snip (Esc) ends the wait
//! without uploading anything, and so does a minute without one.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::core::SharedCore;
use crate::quick_share::{Origin, PasteTarget, PASTE_WINDOW_MS};
use crate::win32;

static RUNNING: AtomicBool = AtomicBool::new(false);
/// Asked for again while one waits: the snipping bar opens again.
static AGAIN: AtomicBool = AtomicBool::new(false);

/// How long after the snipping bar closed a snip may still turn up on the
/// clipboard.
const BAR_GRACE: Duration = Duration::from_secs(3);
/// How long the panel takes to hand the foreground back once it's hidden.
const PANEL_SETTLE: Duration = Duration::from_millis(250);
/// The longest a capture is waited for.
const MAX_WAIT: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(150);

/// Starts a capture; one that's waiting opens the snipping bar again. With
/// `paste`, the link is pasted into the window that was in front.
pub fn start(core: &SharedCore, paste: bool) {
    if RUNNING.swap(true, Ordering::SeqCst) {
        AGAIN.store(true, Ordering::SeqCst);
        return;
    }
    AGAIN.store(false, Ordering::SeqCst);
    let target = if paste { win32::foreground_window() } else { None };
    crate::panel::hide(&core.app);
    let core = core.clone();
    tauri::async_runtime::spawn(async move {
        capture(&core, target).await;
        RUNNING.store(false, Ordering::SeqCst);
    });
}

async fn capture(core: &SharedCore, target: Option<isize>) {
    // What's on the clipboard now is what Undo puts back: the snip itself
    // (with the password in it) never is.
    let snapshot = tauri::async_runtime::spawn_blocking(win32::take_snapshot).await.unwrap_or(win32::Snapshot::Unavailable);
    let mut sequence = win32::clipboard_sequence();
    let Some(mut before) = open_snipping_bar(core).await else { return };
    let mut started = Instant::now();
    // The snipping bar takes the foreground while it's up, and hands it back
    // to the window that had it once it closes, with a snip or without.
    let mut bar_seen = false;
    let mut back_since: Option<Instant> = None;
    while started.elapsed() < MAX_WAIT {
        tokio::time::sleep(POLL).await;
        if AGAIN.swap(false, Ordering::SeqCst) {
            let Some(front) = open_snipping_bar(core).await else { return };
            (before, started, bar_seen, back_since) = (front, Instant::now(), false, None);
            continue;
        }
        let current = win32::clipboard_sequence();
        if current != sequence {
            sequence = current;
            log::debug!("Clipboard changed during a capture (by {:?})", win32::clipboard_owner_process());
            if let Some(input) = tauri::async_runtime::spawn_blocking(crate::clipboard::read_capture).await.ok().flatten() {
                let paste = target.map(|window| PasteTarget { window, deadline: crate::util::now_millis() + PASTE_WINDOW_MS });
                crate::uploads::enqueue_with(core, vec![input], None, Origin { paste, snapshot: Some(snapshot) });
                return;
            }
        }
        let front = win32::foreground_handle();
        if front != before {
            bar_seen = true;
            back_since = None;
        } else if bar_seen {
            // Closed without a snip (Esc, or a click away).
            let since = *back_since.get_or_insert_with(Instant::now);
            if since.elapsed() > BAR_GRACE {
                return;
            }
        }
    }
}

/// Presses Windows+Shift+S once the shortcut's own keys are up (they would
/// make it another shortcut), after the panel had a moment to hand the
/// foreground back to the window behind it. Returns the window in front
/// before the bar opened, or None when the keys couldn't be pressed.
async fn open_snipping_bar(core: &SharedCore) -> Option<isize> {
    tokio::time::sleep(PANEL_SETTLE).await;
    for _ in 0..30 {
        if !win32::modifiers_down() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let before = win32::foreground_handle();
    if !win32::send_screen_snip() {
        crate::uploads::show_notification(
            core,
            &crate::t!("Capture couldn’t start"),
            &crate::t!("Windows’ snipping tool didn’t open. Try again, or take the screenshot with Windows+Shift+S and paste it into Aktar."),
        );
        return None;
    }
    Some(before)
}
