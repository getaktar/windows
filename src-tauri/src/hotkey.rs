//! The user-customizable, system-wide shortcuts: one pastes and uploads the
//! clipboard without opening the panel, the other (unset by default) asks
//! for the upload's name first. Registered with RegisterHotKey (via the
//! global-shortcut plugin), which needs no special permission and only
//! fires on a real key press.

use std::str::FromStr;

use tauri::AppHandle;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

use crate::t;

pub fn plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri_plugin_global_shortcut::Builder::new()
        .with_handler(|app, shortcut, event| {
            if event.state() == ShortcutState::Pressed {
                let core = crate::core::core(app);
                let rename = core.settings.get().rename_shortcut.and_then(|accelerator| parse(&accelerator).ok());
                // Reading and encoding a large screenshot takes a while; this
                // handler runs on the UI thread.
                crate::uploads::upload_clipboard_in_background(&core, rename.as_ref() == Some(shortcut));
            }
        })
        .build()
}

/// Replaces the registered shortcuts: "paste & upload" and "rename and
/// upload". `None` leaves that one unset. When one can't be registered, the
/// other still is, and the error is returned.
pub fn register(app: &AppHandle, upload: Option<&str>, rename: Option<&str>) -> Result<(), String> {
    let shortcuts = app.global_shortcut();
    let _ = shortcuts.unregister_all();
    let taken = || t!("This shortcut is already used by another app. Try a different one.");
    let mut result = Ok(());
    let upload = match upload.map(parse).transpose() {
        Ok(shortcut) => shortcut,
        Err(message) => {
            result = Err(message);
            None
        }
    };
    if let Some(shortcut) = upload {
        if shortcuts.register(shortcut).is_err() {
            result = Err(taken());
        }
    }
    match rename.map(parse).transpose() {
        Ok(Some(shortcut)) if Some(shortcut) == upload => {
            result = result.and(Err(t!("This shortcut is already in use. Try a different one.")));
        }
        Ok(Some(shortcut)) => {
            if shortcuts.register(shortcut).is_err() {
                result = result.and(Err(taken()));
            }
        }
        Ok(None) => {}
        Err(message) => result = result.and(Err(message)),
    }
    result
}

/// Switched off while the Settings window records a new shortcut, so the
/// current one can be pressed (and recorded) without uploading anything.
pub fn set_paused(app: &AppHandle, paused: bool) {
    if paused {
        let _ = app.global_shortcut().unregister_all();
    } else {
        let saved = crate::core::core(app).settings.get();
        let _ = register(app, saved.shortcut.as_deref(), saved.rename_shortcut.as_deref());
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
}
