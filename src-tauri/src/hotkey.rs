//! The user-customizable, system-wide shortcuts: one pastes and uploads the
//! clipboard without opening the panel; the others (unset by default) ask
//! for the upload's name first, upload the clipboard and paste the link
//! where the user is typing, capture the screen (and paste its link), and
//! undo the last upload. Each destination can also have its own that
//! uploads the clipboard straight there. Registered with RegisterHotKey (via
//! the global-shortcut plugin), which needs no special permission and only
//! fires on a real key press.

use std::collections::BTreeMap;
use std::str::FromStr;

use serde::Deserialize;
use tauri::AppHandle;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

use crate::core::events;
use crate::quick_share::{PasteTarget, PASTE_WINDOW_MS};
use crate::settings::Settings;
use crate::t;

/// The app's own shortcuts, in the order they're registered (and shown).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AppShortcut {
    /// "Paste & upload from anywhere".
    Upload,
    /// "Upload clipboard and paste the link".
    UploadAndPaste,
    /// "Rename and upload clipboard".
    Rename,
    /// "Capture and upload".
    Capture,
    /// "Capture, upload and paste the link".
    CaptureAndPaste,
    /// "Undo the last upload".
    Undo,
}

impl AppShortcut {
    pub const ALL: [AppShortcut; 6] =
        [AppShortcut::Upload, AppShortcut::UploadAndPaste, AppShortcut::Rename, AppShortcut::Capture, AppShortcut::CaptureAndPaste, AppShortcut::Undo];

    pub fn get(self, settings: &Settings) -> Option<&str> {
        match self {
            AppShortcut::Upload => settings.shortcut.as_deref(),
            AppShortcut::UploadAndPaste => settings.paste_shortcut.as_deref(),
            AppShortcut::Rename => settings.rename_shortcut.as_deref(),
            AppShortcut::Capture => settings.capture_shortcut.as_deref(),
            AppShortcut::CaptureAndPaste => settings.capture_paste_shortcut.as_deref(),
            AppShortcut::Undo => settings.undo_shortcut.as_deref(),
        }
    }

    /// The shortcut's name, as Settings shows it.
    pub fn title(self) -> String {
        match self {
            AppShortcut::Upload => t!("Paste & upload from anywhere"),
            AppShortcut::UploadAndPaste => t!("Upload clipboard and paste the link"),
            AppShortcut::Rename => t!("Rename and upload clipboard"),
            AppShortcut::Capture => t!("Capture and upload"),
            AppShortcut::CaptureAndPaste => t!("Capture, upload and paste the link"),
            AppShortcut::Undo => t!("Undo the last upload"),
        }
    }

    pub fn set(self, settings: &mut Settings, accelerator: Option<String>) {
        let slot = match self {
            AppShortcut::Upload => &mut settings.shortcut,
            AppShortcut::UploadAndPaste => &mut settings.paste_shortcut,
            AppShortcut::Rename => &mut settings.rename_shortcut,
            AppShortcut::Capture => &mut settings.capture_shortcut,
            AppShortcut::CaptureAndPaste => &mut settings.capture_paste_shortcut,
            AppShortcut::Undo => &mut settings.undo_shortcut,
        };
        *slot = accelerator;
    }
}

/// The app's shortcuts that are set.
fn app_accelerators(settings: &Settings) -> Vec<&str> {
    AppShortcut::ALL.iter().filter_map(|kind| kind.get(settings)).collect()
}

pub fn plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri_plugin_global_shortcut::Builder::new()
        .with_handler(|app, shortcut, event| {
            if event.state() != ShortcutState::Pressed {
                return;
            }
            let core = crate::core::core(app);
            let settings = core.settings.get();
            // A destination's own shortcut uploads there, never rerouted.
            let destination = settings
                .destination_shortcuts
                .iter()
                .find(|(_, accelerator)| parse(accelerator).ok().as_ref() == Some(shortcut))
                .and_then(|(id, _)| core.destinations.find(Some(id)).filter(|destination| destination.id == *id));
            if let Some(destination) = destination {
                crate::uploads::upload_clipboard_to_in_background(&core, destination);
                return;
            }
            let pressed = AppShortcut::ALL.into_iter().find(|kind| kind.get(&settings).and_then(|accelerator| parse(accelerator).ok()).as_ref() == Some(shortcut));
            // Reading and encoding a large screenshot takes a while; this
            // handler runs on the UI thread, so everything goes on in the
            // background.
            match pressed {
                Some(AppShortcut::Upload) | None => crate::uploads::upload_clipboard_in_background(&core, false),
                Some(AppShortcut::Rename) => crate::uploads::upload_clipboard_in_background(&core, true),
                Some(AppShortcut::UploadAndPaste) => {
                    crate::win32::mask_menu_key();
                    crate::uploads::upload_clipboard_and_paste(&core, false, PasteTarget::foreground(PASTE_WINDOW_MS));
                }
                Some(AppShortcut::Capture) => crate::capture::start(&core, false),
                Some(AppShortcut::CaptureAndPaste) => {
                    crate::win32::mask_menu_key();
                    crate::capture::start(&core, true);
                }
                Some(AppShortcut::Undo) => {
                    if !crate::quick_share::undo_latest(&core) {
                        crate::win32::beep();
                    }
                }
            }
        })
        .build()
}

/// Replaces the registered shortcuts with the app's and the destinations'
/// own, as `settings` has them. When one can't be registered, the others
/// still are; the app's that couldn't be are returned, with why.
pub fn register(app: &AppHandle, settings: &Settings) -> Vec<(AppShortcut, String)> {
    let results = register_app_shortcuts(app, settings);
    register_destination_shortcuts(app, &settings.destination_shortcuts, &app_accelerators(settings));
    results.into_iter().filter_map(|(kind, result)| result.err().map(|message| (kind, message))).collect()
}

/// The destinations' own shortcuts, on top of the app's. One that's the
/// same as another, or taken by another app, is skipped (the form checked
/// it when it was set).
fn register_destination_shortcuts(app: &AppHandle, destinations: &BTreeMap<String, String>, app_shortcuts: &[&str]) {
    let taken: Vec<Shortcut> = app_shortcuts.iter().filter_map(|accelerator| parse(accelerator).ok()).collect();
    let mut registered: Vec<Shortcut> = Vec::new();
    for accelerator in destinations.values() {
        let Ok(shortcut) = parse(accelerator) else { continue };
        if taken.contains(&shortcut) || registered.contains(&shortcut) {
            continue;
        }
        if app.global_shortcut().register(shortcut).is_ok() {
            registered.push(shortcut);
        }
    }
}

/// Sets (or with `None` clears) one of the app's shortcuts, refusing one
/// that's already another of the app's or a destination's, or that another
/// app has taken. A failed change leaves the shortcuts as they were.
pub fn set_app_shortcut(app: &AppHandle, kind: AppShortcut, accelerator: Option<String>) -> Result<(), String> {
    let core = crate::core::core(app);
    let previous = core.settings.get();
    if let Some(shortcut) = accelerator.as_deref().and_then(|accelerator| parse(accelerator).ok()) {
        let taken = previous.destination_shortcuts.values().any(|other| parse(other).ok() == Some(shortcut));
        if taken {
            let _ = register(app, &previous);
            return Err(t!("This shortcut is already in use. Try a different one."));
        }
    }
    let mut candidate = previous.clone();
    kind.set(&mut candidate, accelerator.clone());
    let results = register_app_shortcuts(app, &candidate);
    register_destination_shortcuts(app, &candidate.destination_shortcuts, &app_accelerators(&candidate));
    if let Some((_, Err(message))) = results.into_iter().find(|(other, _)| *other == kind) {
        // Put the old ones back so a failed change doesn't leave none.
        let _ = register(app, &previous);
        return Err(message);
    }
    core.settings.update(|settings| kind.set(settings, accelerator));
    core.notify(events::SETTINGS_CHANGED);
    Ok(())
}

/// Sets (or with `None` clears) a destination's own shortcut, refusing one
/// that's already the app's or another destination's, or that another app
/// has taken.
pub fn set_destination(app: &AppHandle, destination_id: &str, accelerator: Option<&str>) -> Result<(), String> {
    let core = crate::core::core(app);
    let settings = core.settings.get();
    let app_shortcuts = app_accelerators(&settings);
    if let Some(accelerator) = accelerator {
        let shortcut = parse(accelerator)?;
        let in_use = app_shortcuts
            .iter()
            .copied()
            .chain(settings.destination_shortcuts.iter().filter(|(id, _)| id.as_str() != destination_id).map(|(_, accelerator)| accelerator.as_str()))
            .any(|other| parse(other).ok() == Some(shortcut));
        if in_use {
            return Err(t!("This shortcut is already in use. Try a different one."));
        }
    }
    let mut destinations = settings.destination_shortcuts.clone();
    match accelerator {
        Some(accelerator) => destinations.insert(destination_id.to_string(), accelerator.to_string()),
        None => destinations.remove(destination_id),
    };
    let _ = register_app_shortcuts(app, &settings);
    if let Some(shortcut) = accelerator.and_then(|accelerator| parse(accelerator).ok()) {
        if app.global_shortcut().register(shortcut).is_err() {
            register_destination_shortcuts(app, &settings.destination_shortcuts, &app_shortcuts);
            return Err(t!("This shortcut is already used by another app. Try a different one."));
        }
        let _ = app.global_shortcut().unregister(shortcut);
    }
    register_destination_shortcuts(app, &destinations, &app_shortcuts);
    core.settings.update(|settings| settings.destination_shortcuts = destinations);
    Ok(())
}

/// Registers the app's shortcuts after unregistering everything, and says
/// how each went. Two of them set to the same keys is an error for the
/// later one.
fn register_app_shortcuts(app: &AppHandle, settings: &Settings) -> Vec<(AppShortcut, Result<(), String>)> {
    let shortcuts = app.global_shortcut();
    let _ = shortcuts.unregister_all();
    let mut registered: Vec<Shortcut> = Vec::new();
    let mut results = Vec::new();
    for kind in AppShortcut::ALL {
        let Some(accelerator) = kind.get(settings) else { continue };
        let result = match parse(accelerator) {
            Ok(shortcut) if registered.contains(&shortcut) => Err(t!("This shortcut is already in use. Try a different one.")),
            Ok(shortcut) => match shortcuts.register(shortcut) {
                Ok(()) => {
                    registered.push(shortcut);
                    Ok(())
                }
                Err(_) => Err(t!("This shortcut is already used by another app. Try a different one.")),
            },
            Err(message) => Err(message),
        };
        results.push((kind, result));
    }
    results
}

/// Switched off while the Settings window records a new shortcut, so the
/// current one can be pressed (and recorded) without uploading anything.
pub fn set_paused(app: &AppHandle, paused: bool) {
    if paused {
        let _ = app.global_shortcut().unregister_all();
    } else {
        let saved = crate::core::core(app).settings.get();
        let _ = register(app, &saved);
    }
}

/// Accepts accelerators like "Ctrl+Shift+Alt+KeyU" and requires Ctrl, Alt,
/// or the Windows key, so a bare letter can't take over typing. Function
/// keys may go without a modifier.
pub fn parse(accelerator: &str) -> Result<Shortcut, String> {
    let parts: Vec<String> = accelerator.split('+').map(|part| part.trim().to_ascii_lowercase()).collect();
    let key = parts.last().cloned().unwrap_or_default();
    let modifiers = &parts[..parts.len().saturating_sub(1)];
    let has = |names: &[&str]| modifiers.iter().any(|part| names.contains(&part.as_str()));
    let has_modifier = has(&["ctrl", "control", "alt", "super", "win", "cmdorctrl", "commandorcontrol"]);
    let is_function_key = key.len() > 1 && key.starts_with('f') && key[1..].parse::<u8>().is_ok();
    if !has_modifier && !is_function_key {
        return Err(t!("Include Ctrl, Alt, or the Windows key in the shortcut."));
    }
    // Ctrl+Alt is AltGr, which types @, €, { and the like on most non-US
    // layouts (Turkish, German, French...). Registered, it would swallow
    // that character everywhere.
    let ctrl = has(&["ctrl", "control", "cmdorctrl", "commandorcontrol"]);
    if ctrl && has(&["alt"]) && !has(&["shift", "super", "win"]) && !is_function_key {
        return Err(t!("On many keyboards Ctrl+Alt types characters like @ or €. Add Shift or the Windows key."));
    }
    Shortcut::from_str(accelerator).map_err(|_| t!("That key combination can’t be used as a shortcut."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_a_modifier() {
        assert!(parse("Ctrl+Shift+Alt+KeyU").is_ok());
        assert!(parse("Alt+Digit1").is_ok());
        assert!(parse("F9").is_ok());
        assert!(parse("Shift+KeyU").is_err());
        assert!(parse("KeyU").is_err());
    }

    #[test]
    fn rejects_altgr_combinations() {
        assert!(parse("Ctrl+Alt+KeyQ").is_err());
        assert!(parse("Ctrl+Alt+Super+KeyQ").is_ok());
        assert!(parse("Ctrl+Alt+F9").is_ok());
    }

    #[test]
    fn reads_and_writes_each_shortcut() {
        let mut settings = Settings::default();
        assert_eq!(app_accelerators(&settings), vec![crate::settings::DEFAULT_SHORTCUT]);
        for (index, kind) in AppShortcut::ALL.into_iter().enumerate() {
            kind.set(&mut settings, Some(format!("Ctrl+Shift+Alt+Digit{index}")));
        }
        for (index, kind) in AppShortcut::ALL.into_iter().enumerate() {
            assert_eq!(kind.get(&settings), Some(format!("Ctrl+Shift+Alt+Digit{index}").as_str()));
        }
        assert_eq!(app_accelerators(&settings).len(), 6);
        let kind: AppShortcut = serde_json::from_str("\"captureAndPaste\"").unwrap();
        assert_eq!(kind, AppShortcut::CaptureAndPaste);
    }
}
