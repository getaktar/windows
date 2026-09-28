//! App-wide state shared by the commands, the tray, the local API, and the
//! upload pipeline. Rust owns every piece of state; windows read it through
//! commands and refresh when one of the events below fires.

use std::path::PathBuf;
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
        let cache_dir = app.path().app_cache_dir()?;
        std::fs::create_dir_all(&data_dir)?;
        let thumbnails_dir = cache_dir.join("thumbnails");
        std::fs::create_dir_all(&thumbnails_dir)?;

        Ok(Arc::new(Core {
            app: app.clone(),
            settings: SettingsStore::load(&data_dir),
            destinations: DestinationStore::load(&data_dir),
            history: History::open(&data_dir, thumbnails_dir.clone())?,
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

pub fn core(app: &AppHandle) -> SharedCore {
    app.state::<SharedCore>().inner().clone()
}
