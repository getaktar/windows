//! The user-customizable, system-wide shortcut that pastes and uploads the
//! clipboard without opening the panel. Registered with RegisterHotKey (via
//! the global-shortcut plugin), which needs no special permission and only
//! fires on a real key press.

use std::str::FromStr;

use tauri::AppHandle;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

use crate::t;

pub fn plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri_plugin_global_shortcut::Builder::new()
        .with_handler(|app, _shortcut, event| {
            if event.state() == ShortcutState::Pressed {
                crate::uploads::upload_clipboard(&crate::core::core(app));
            }
        })
        .build()
}

/// Replaces the registered shortcut. `None` just clears it.
pub fn register(app: &AppHandle, accelerator: Option<&str>) -> Result<(), String> {
    let shortcuts = app.global_shortcut();
    let _ = shortcuts.unregister_all();
    let Some(accelerator) = accelerator else { return Ok(()) };
    let shortcut = parse(accelerator)?;
    shortcuts
        .register(shortcut)
        .map_err(|_| t!("This shortcut is already used by another app. Try a different one."))
}

/// Accepts accelerators like "Ctrl+Shift+Alt+KeyU" and requires Ctrl, Alt,
/// or the Windows key, so a bare letter can't take over typing. Function
/// keys may go without a modifier.
pub fn parse(accelerator: &str) -> Result<Shortcut, String> {
    let parts: Vec<String> = accelerator.split('+').map(|part| part.trim().to_ascii_lowercase()).collect();
    let key = parts.last().cloned().unwrap_or_default();
    let has_modifier = parts[..parts.len().saturating_sub(1)]
        .iter()
        .any(|part| matches!(part.as_str(), "ctrl" | "control" | "alt" | "super" | "win" | "cmdorctrl" | "commandorcontrol"));
    let is_function_key = key.len() > 1 && key.starts_with('f') && key[1..].parse::<u8>().is_ok();
    if !has_modifier && !is_function_key {
        return Err(t!("Include Ctrl, Alt, or the Windows key in the shortcut."));
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
}
