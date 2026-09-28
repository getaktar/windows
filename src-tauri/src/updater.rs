//! Automatic updates, the Windows counterpart of Sparkle on the Mac. The
//! feed is `latest.json` on the latest GitHub release, and every installer
//! is verified against the public key in tauri.conf.json before it runs.

use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tauri::AppHandle;
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::core::{events, SharedCore};
use crate::windows::AppWindow;

const CHECK_INTERVAL_MS: i64 = 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum UpdateStatus {
    Idle,
    Checking,
    UpToDate,
    Available { version: String, notes: Option<String> },
    Downloading { version: String, progress: Option<f64> },
    /// Downloaded in the background; installs when Aktar quits.
    ReadyToInstall { version: String },
    Failed { message: String },
}

pub struct UpdateService {
    status: Mutex<UpdateStatus>,
    pending: Mutex<Option<Update>>,
    downloaded: Mutex<Option<Vec<u8>>>,
}

impl Default for UpdateService {
    fn default() -> Self {
        Self { status: Mutex::new(UpdateStatus::Idle), pending: Mutex::new(None), downloaded: Mutex::new(None) }
    }
}

pub fn status(core: &SharedCore) -> UpdateStatus {
    core.updater.status.lock().unwrap().clone()
}

fn set_status(core: &SharedCore, status: UpdateStatus) {
    *core.updater.status.lock().unwrap() = status;
    core.notify(events::UPDATE_CHANGED);
}

/// "Check for Updates…": shows the update window and checks right away.
pub fn check_now(app: &AppHandle) {
    let core = crate::core::core(app);
    crate::windows::open(app, AppWindow::Update);
    if matches!(status(&core), UpdateStatus::Downloading { .. } | UpdateStatus::ReadyToInstall { .. }) {
        return;
    }
    tauri::async_runtime::spawn(async move { check(&core, true).await });
}

async fn check(core: &SharedCore, user_initiated: bool) {
    if matches!(status(core), UpdateStatus::Checking) {
        return;
    }
    set_status(core, UpdateStatus::Checking);
    core.settings.update(|settings| settings.last_update_check = Some(crate::util::now_millis()));

    let result = match core.app.updater() {
        Ok(updater) => updater.check().await,
        Err(error) => Err(error),
    };
    match result {
        Ok(Some(update)) => {
            let version = update.version.clone();
            set_status(core, UpdateStatus::Available { version: version.clone(), notes: update.body.clone() });
            *core.updater.pending.lock().unwrap() = Some(update);
            if !user_initiated {
                if core.settings.get().auto_install_updates {
                    download_in_background(core).await;
                } else {
                    // Aktar has no main window to show this in, so bring the
                    // update window forward on its own.
                    crate::windows::open(&core.app, AppWindow::Update);
                }
            }
        }
        Ok(None) => set_status(core, if user_initiated { UpdateStatus::UpToDate } else { UpdateStatus::Idle }),
        Err(error) => {
            log::warn!("Update check failed: {error}");
            set_status(core, if user_initiated { UpdateStatus::Failed { message: error.to_string() } } else { UpdateStatus::Idle });
        }
    }
}

async fn download(core: &SharedCore) -> Option<Vec<u8>> {
    let update = core.updater.pending.lock().unwrap().clone()?;
    let version = update.version.clone();
    set_status(core, UpdateStatus::Downloading { version: version.clone(), progress: None });

    let progress_core = core.clone();
    let progress_version = version.clone();
    let mut received: u64 = 0;
    let result = update
        .download(
            move |chunk, total| {
                received += chunk as u64;
                let progress = total.filter(|total| *total > 0).map(|total| received as f64 / total as f64);
                *progress_core.updater.status.lock().unwrap() =
                    UpdateStatus::Downloading { version: progress_version.clone(), progress };
                progress_core.notify(events::UPDATE_CHANGED);
            },
            || {},
        )
        .await;
    match result {
        Ok(bytes) => Some(bytes),
        Err(error) => {
            set_status(core, UpdateStatus::Failed { message: error.to_string() });
            None
        }
    }
}

async fn download_in_background(core: &SharedCore) {
    if let Some(bytes) = download(core).await {
        let version = core.updater.pending.lock().unwrap().as_ref().map(|u| u.version.clone()).unwrap_or_default();
        *core.updater.downloaded.lock().unwrap() = Some(bytes);
        set_status(core, UpdateStatus::ReadyToInstall { version });
    }
}

/// "Install and Relaunch" in the update window. On Windows the installer
/// closes Aktar itself and starts the new version when it's done.
pub async fn install(core: &SharedCore) {
    let already_downloaded = core.updater.downloaded.lock().unwrap().take();
    let bytes = match already_downloaded {
        Some(bytes) => bytes,
        None => match download(core).await {
            Some(bytes) => bytes,
            None => return,
        },
    };
    let Some(update) = core.updater.pending.lock().unwrap().clone() else { return };
    if let Err(error) = update.install(bytes) {
        set_status(core, UpdateStatus::Failed { message: error.to_string() });
        return;
    }
    core.app.restart();
}

/// An update downloaded in the background is installed when Aktar quits.
/// Returns true when the installer took over.
pub fn install_pending_on_quit(core: &SharedCore) -> bool {
    let Some(bytes) = core.updater.downloaded.lock().unwrap().take() else { return false };
    let Some(update) = core.updater.pending.lock().unwrap().clone() else { return false };
    // The user asked to quit, so the installer shouldn't start Aktar again.
    update.restart_after_install(false).install(bytes).is_ok()
}

/// Checks at launch and then about once a day while "Automatically check
/// for updates" is on.
pub fn schedule(core: &SharedCore) {
    let core = core.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(10)).await;
        loop {
            let settings = core.settings.get();
            let due = settings
                .last_update_check
                .is_none_or(|last| crate::util::now_millis() - last >= CHECK_INTERVAL_MS);
            if settings.auto_check_updates && due && matches!(status(&core), UpdateStatus::Idle | UpdateStatus::UpToDate | UpdateStatus::Failed { .. }) {
                check(&core, false).await;
            }
            tokio::time::sleep(Duration::from_secs(60 * 60)).await;
        }
    });
}
