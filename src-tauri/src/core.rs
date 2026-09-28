//! App-wide state shared by the commands, the tray, the local API, and the
//! upload pipeline. Rust owns every piece of state; windows read it through
//! commands and refresh when one of the events below fires.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::destinations::DestinationStore;
use crate::history::History;
use crate::local_api::LocalApiService;
use crate::settings::SettingsStore;
use crate::updater::UpdateService;
use crate::uploads::UploadManager;

pub mod events {
    pub const DESTINATIONS_CHANGED: &str = "destinations-changed";
    pub const JOBS_CHANGED: &str = "jobs-changed";
    pub const HISTORY_CHANGED: &str = "history-changed";
    pub const SETTINGS_CHANGED: &str = "settings-changed";
    pub const LANGUAGE_CHANGED: &str = "language-changed";
    pub const LOCAL_API_CHANGED: &str = "local-api-changed";
    pub const UPDATE_CHANGED: &str = "update-changed";
    /// Payload: `UploadSucceeded`. The bucket browser reloads the folder an
    /// upload landed in.
    pub const UPLOAD_SUCCEEDED: &str = "upload-succeeded";
    pub const PANEL_SHOWN: &str = "panel-shown";
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadSucceeded {
    pub destination_id: String,
    pub object_key: String,
    pub byte_size: i64,
}

pub struct Core {
    pub app: AppHandle,
    pub settings: SettingsStore,
    pub destinations: DestinationStore,
    pub history: History,
    pub uploads: UploadManager,
    pub local_api: LocalApiService,
    pub updater: UpdateService,
    pub thumbnails_dir: PathBuf,
    /// When the app started, to tell a deep link that launched Aktar from
    /// one sent to an already running copy.
    pub launched_at: std::time::Instant,
}

pub type SharedCore = Arc<Core>;

impl Core {
    pub fn new(app: &AppHandle) -> Result<SharedCore, Box<dyn std::error::Error>> {
        let data_dir = app.path().app_data_dir()?;
        let local_data_dir = app.path().app_local_data_dir()?;
        let cache_dir = app.path().app_cache_dir()?;
        std::fs::create_dir_all(&data_dir)?;
        std::fs::create_dir_all(&local_data_dir)?;
        let thumbnails_dir = cache_dir.join("thumbnails");
        std::fs::create_dir_all(&thumbnails_dir)?;
        // Settings and destinations roam with the user; the history database
        // stays on this PC. SQLite locks and WAL files don't survive a
        // roaming or redirected (network) AppData, common in companies.
        let history_dir = move_history(&data_dir, &local_data_dir);

        Ok(Arc::new(Core {
            app: app.clone(),
            settings: SettingsStore::load(&data_dir),
            destinations: DestinationStore::load(&data_dir),
            history: History::open(&history_dir, thumbnails_dir.clone())?,
            uploads: UploadManager::default(),
            local_api: LocalApiService::default(),
            updater: UpdateService::default(),
            thumbnails_dir,
            launched_at: std::time::Instant::now(),
        }))
    }

    pub fn emit<S: Serialize + Clone>(&self, event: &str, payload: S) {
        let _ = self.app.emit(event, payload);
    }

    pub fn notify(&self, event: &str) {
        self.emit(event, ());
    }
}

/// Brings the history of a version that kept it in Roaming AppData over,
/// unless there's one in the new place already. Returns the directory to
/// open it from: the old one if it couldn't be moved.
fn move_history(from: &Path, to: &Path) -> PathBuf {
    if from == to || to.join("history.sqlite").exists() || !from.join("history.sqlite").exists() {
        return to.to_path_buf();
    }
    // The database and its WAL go together or not at all: copied first, and
    // the originals removed only once every copy is in place.
    let names = ["history.sqlite", "history.sqlite-wal", "history.sqlite-shm"];
    let present: Vec<&str> = names.into_iter().filter(|name| from.join(name).exists()).collect();
    for name in &present {
        if let Err(error) = std::fs::copy(from.join(name), to.join(name)) {
            log::warn!("Could not move the upload history: {error}");
            for copied in &present {
                let _ = std::fs::remove_file(to.join(copied));
            }
            return from.to_path_buf();
        }
    }
    for name in &present {
        let _ = std::fs::remove_file(from.join(name));
    }
    to.to_path_buf()
}

pub fn core(app: &AppHandle) -> SharedCore {
    app.state::<SharedCore>().inner().clone()
}
