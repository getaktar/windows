//! Automatic updates, the Windows counterpart of Sparkle on the Mac. The
//! feed is `latest.json` on the latest GitHub release, and every installer
//! is verified against the public key in tauri.conf.json before it runs.

use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::core::{events, SharedCore};
use crate::uploads::JobState;
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
    /// Downloaded in the background; installs once Aktar is idle, or when
    /// it quits.
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
    // The Store updates its own packages.
    if crate::package::is_packaged() {
        return;
    }
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
        install_when_idle(core).await;
    }
}

/// A tray app is rarely quit on purpose (it's closed at sign-out, which
/// gives no installer a chance to run), so an update downloaded in the
/// background is installed as soon as nothing is going on: no upload in
/// progress and no window open. Aktar restarts quietly afterwards.
async fn install_when_idle(core: &SharedCore) {
    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;
        // Installed from the update window, or failed, meanwhile.
        if !matches!(status(core), UpdateStatus::ReadyToInstall { .. }) {
            return;
        }
        if !is_idle(core) {
            continue;
        }
        let Some(bytes) = core.updater.downloaded.lock().unwrap().take() else { return };
        let Some(update) = core.updater.pending.lock().unwrap().clone() else { return };
        core.settings.update(|settings| settings.quiet_next_launch = true);
        if let Err(error) = run_installer(core, &update, bytes) {
            core.settings.update(|settings| settings.quiet_next_launch = false);
            set_status(core, UpdateStatus::Failed { message: error.to_string() });
        }
        return;
    }
}

fn is_idle(core: &SharedCore) -> bool {
    let uploading = core
        .uploads
        .snapshot()
        .iter()
        .any(|job| matches!(job.state, JobState::Waiting | JobState::Uploading { .. }));
    // The panel always exists (hidden); every other window only while open.
    let window_open = core
        .app
        .webview_windows()
        .iter()
        .any(|(label, window)| label != crate::panel::LABEL || window.is_visible().unwrap_or(false));
    !uploading && !window_open
}

/// On Windows the installer ends this process as it starts, without the
/// usual cleanup, so the tray icon is taken down first; otherwise a dead
/// icon stays behind until the pointer passes over it.
fn run_installer(core: &SharedCore, update: &Update, bytes: Vec<u8>) -> Result<(), tauri_plugin_updater::Error> {
    crate::tray::remove(&core.app);
    let result = update.install(bytes);
    if result.is_err() {
        let _ = crate::tray::create(&core.app);
    }
    result
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
    if let Err(error) = run_installer(core, &update, bytes) {
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
    run_installer(core, &update.restart_after_install(false), bytes).is_ok()
}

/// Checks at launch and then about once a day while "Automatically check
/// for updates" is on.
pub fn schedule(core: &SharedCore) {
    if crate::package::is_packaged() {
        return;
    }
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
