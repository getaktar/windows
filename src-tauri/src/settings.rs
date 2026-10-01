//! App preferences, kept as JSON next to the other app data. Nothing secret
//! goes here: storage credentials and the local API token live in Windows
//! Credential Manager (see `credentials`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::expiry::RulesCheck;
use crate::output::OutputMode;

pub const DEFAULT_SHORTCUT: &str = "Ctrl+Shift+Alt+KeyU";
pub const DEFAULT_LOCAL_API_PORT: u16 = 47913;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub output_mode: OutputMode,
    pub custom_template: String,
    pub show_notification: bool,
    pub close_panel_after_upload: bool,
    /// A language code from `i18n::SUPPORTED`, or none to follow Windows.
    pub language: Option<String>,
    /// Global "paste & upload" shortcut as an accelerator string, or none
    /// when the user cleared it.
    pub shortcut: Option<String>,
    /// Global "rename and upload clipboard" shortcut, which asks for the
    /// upload's name first. None (the default) when it isn't set.
    pub rename_shortcut: Option<String>,
    /// Copies the link of an earlier upload of the same bytes to the same
    /// destination instead of uploading them again.
    pub reuse_duplicate_links: bool,
    pub local_api_enabled: bool,
    pub local_api_port: u16,
    pub auto_check_updates: bool,
    pub auto_install_updates: bool,
    /// Unix milliseconds of the last automatic update check.
    pub last_update_check: Option<i64>,
    /// "Delete after" for uploads from the panel, the shortcut, and
    /// everything else that uses the destination's path template: one of
    /// `expiry::DURATIONS`, or 0 to keep them.
    pub delete_after_days: u32,
    /// Whether each destination's lifecycle rules are set up, by ID.
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub expiry_rules: HashMap<String, RulesCheck>,
    /// Set before an update installs on its own, so the relaunch it ends
    /// with stays in the tray instead of opening the panel.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub quiet_next_launch: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            output_mode: OutputMode::Url,
            custom_template: "![{filename}]({url})".into(),
            show_notification: true,
            close_panel_after_upload: true,
            language: None,
            shortcut: Some(DEFAULT_SHORTCUT.into()),
            rename_shortcut: None,
            reuse_duplicate_links: true,
            local_api_enabled: false,
            local_api_port: DEFAULT_LOCAL_API_PORT,
            auto_check_updates: true,
            auto_install_updates: false,
            last_update_check: None,
            delete_after_days: 0,
            expiry_rules: HashMap::new(),
            quiet_next_launch: false,
        }
    }
}

/// The fields the Settings window may change directly. Everything with side
/// effects beyond saving (shortcut, language, local API) has its own command.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SettingsPatch {
    pub output_mode: Option<OutputMode>,
    pub custom_template: Option<String>,
    pub show_notification: Option<bool>,
    pub close_panel_after_upload: Option<bool>,
    pub reuse_duplicate_links: Option<bool>,
    pub auto_check_updates: Option<bool>,
    pub auto_install_updates: Option<bool>,
    pub delete_after_days: Option<u32>,
}

pub struct SettingsStore {
    path: PathBuf,
    settings: Mutex<Settings>,
}

impl SettingsStore {
    pub fn load(directory: &Path) -> Self {
        let path = directory.join("settings.json");
        let mut settings: Settings = std::fs::read(&path)
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default();
        if settings.local_api_port < 1024 {
            settings.local_api_port = DEFAULT_LOCAL_API_PORT;
        }
        if !crate::expiry::is_valid(settings.delete_after_days) {
            settings.delete_after_days = 0;
        }
        Self { path, settings: Mutex::new(settings) }
    }

    pub fn get(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    pub fn update(&self, change: impl FnOnce(&mut Settings)) -> Settings {
        let mut settings = self.settings.lock().unwrap();
        change(&mut settings);
        // Written under the lock, so two quick updates can't land on disk
        // in the wrong order.
        crate::util::write_json_atomically(&self.path, &*settings);
        settings.clone()
    }

    pub fn apply_patch(&self, patch: SettingsPatch) -> Settings {
        self.update(|settings| {
            if let Some(value) = patch.output_mode {
                settings.output_mode = value;
            }
            if let Some(value) = patch.custom_template {
                settings.custom_template = value;
            }
            if let Some(value) = patch.show_notification {
                settings.show_notification = value;
            }
            if let Some(value) = patch.close_panel_after_upload {
                settings.close_panel_after_upload = value;
            }
            if let Some(value) = patch.reuse_duplicate_links {
                settings.reuse_duplicate_links = value;
            }
            if let Some(value) = patch.auto_check_updates {
                settings.auto_check_updates = value;
            }
            if let Some(value) = patch.auto_install_updates {
                settings.auto_install_updates = value;
            }
            if let Some(value) = patch.delete_after_days.filter(|days| *days == 0 || crate::expiry::is_valid(*days)) {
                settings.delete_after_days = value;
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn older_settings_reuse_links_and_have_no_rename_shortcut() {
        let settings: Settings = serde_json::from_str(r#"{"outputMode":"url","shortcut":"Ctrl+Shift+Alt+KeyU"}"#).unwrap();
        assert!(settings.reuse_duplicate_links);
        assert_eq!(settings.rename_shortcut, None);
        assert_eq!(settings.shortcut.as_deref(), Some(DEFAULT_SHORTCUT));
    }
}
