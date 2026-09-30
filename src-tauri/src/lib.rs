//! Aktar for Windows: upload files to your own S3-compatible storage from
//! the notification area and get a link on your clipboard.

mod bucket;
mod clipboard;
mod commands;
mod core;
mod credentials;
mod deeplink;
mod destinations;
mod expiry;
mod history;
mod hotkey;
mod i18n;
mod local_api;
mod output;
mod package;
mod panel;
mod settings;
mod storage;
mod system;
mod thumbnails;
mod tray;
mod updater;
mod uploads;
mod util;
mod windows;

use tauri::{App, AppHandle, Manager, RunEvent};
use tauri_plugin_deep_link::DeepLinkExt;

use crate::core::Core;
use crate::panel::PanelState;
use crate::windows::AppWindow;

/// Passed by the login item, so a launch at sign-in stays in the tray
/// instead of opening the panel.
const AUTOSTART_FLAG: &str = "--autostart";

pub fn run() {
    let mut builder = tauri::Builder::default();

    // Must come first: a second launch (from the Start menu, or an aktar://
    // link) hands its arguments to the running copy and exits.
    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            // Links are delivered through the deep-link plugin's
            // on_open_url; a plain second launch just shows the panel.
            if !argv.iter().any(|arg| arg.to_ascii_lowercase().starts_with("aktar:")) {
                panel::show(app);
            }
        }));
    }

    let app = builder
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![AUTOSTART_FLAG]),
        ))
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(hotkey::plugin())
        .setup(|app| {
            setup(app)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            commands::list_destinations,
            commands::save_destination,
            commands::remove_destination,
            commands::set_default_destination,
            commands::test_connection,
            commands::expiry_rules_status,
            commands::set_up_expiry_rules,
            commands::expiry_prefixes_in_use,
            commands::remove_expiry_rules,
            commands::upload_files,
            commands::upload_clipboard,
            commands::list_jobs,
            commands::retry_job,
            commands::cancel_job,
            commands::dismiss_job,
            commands::list_history,
            commands::thumbnails_dir,
            commands::delete_remote,
            commands::remove_from_history,
            commands::list_objects,
            commands::bucket_delete,
            commands::bucket_move,
            commands::bucket_create_folder,
            commands::bucket_presign,
            commands::bucket_upload,
            commands::fetch_remote,
            commands::get_settings,
            commands::update_settings,
            commands::set_shortcut,
            commands::set_shortcut_paused,
            commands::set_language,
            commands::get_launch_at_login,
            commands::set_launch_at_login,
            commands::local_api_state,
            commands::set_local_api_enabled,
            commands::set_local_api_port,
            commands::regenerate_api_token,
            commands::copy_text,
            commands::open_url,
            commands::open_window,
            commands::close_window,
            commands::show_panel,
            commands::finish_onboarding,
            commands::hide_panel,
            commands::set_panel_showing_dialog,
            commands::set_panel_height,
            commands::quit_app,
            commands::update_status,
            commands::check_for_updates,
            commands::install_update,
        ])
        .build(tauri::generate_context!());
    let app = match app {
        Ok(app) => app,
        // Setup failed, e.g. the history database is locked or damaged.
        Err(error) => {
            system::show_fatal_error(&t!("Aktar couldn’t start. {0}", error));
            std::process::exit(1);
        }
    };
    app.run(|_app, event| {
        // Closing the last window must not quit: Aktar lives in the
        // notification area. Only an explicit exit (Quit) gets through.
        if let RunEvent::ExitRequested { api, code: None, .. } = event {
            api.prevent_exit();
        }
    });
}

fn setup(app: &mut App) -> Result<(), Box<dyn std::error::Error>> {
    let handle = app.handle().clone();
    // The system language until settings are loaded, so an error opening
    // them still comes out translated.
    i18n::apply(None);
    let core = Core::new(&handle)?;
    app.manage(core.clone());
    app.manage(PanelState::default());

    let settings = core.settings.get();
    i18n::apply(settings.language.as_deref());
    clipboard::remove_leftovers();

    panel::create(&handle)?;
    tray::create(&handle)?;
    tray::follow_taskbar_theme(&handle);
    if let Err(message) = hotkey::register(&handle, settings.shortcut.as_deref()) {
        // Taken by another app since it was set: without this, the shortcut
        // would just seem broken.
        uploads::show_notification(&core, &t!("Paste & upload from anywhere"), &message);
    }
    local_api::start(&core);
    updater::schedule(&core);
    expiry::schedule_sweep(&core);

    // The installer registers aktar:// for installed builds; a dev build
    // registers itself so links can be tested.
    #[cfg(all(debug_assertions, windows))]
    let _ = app.deep_link().register_all();

    let launch_links = app.deep_link().get_current().ok().flatten().unwrap_or_default();
    for link in &launch_links {
        deeplink::handle(&core, link.as_str(), true);
    }
    let link_core = core.clone();
    app.deep_link().on_open_url(move |event| {
        let launched = deeplink::is_launch_link(&link_core);
        for link in event.urls() {
            deeplink::handle(&link_core, link.as_str(), launched);
        }
    });

    // Tray icons land in the hidden overflow by default, so a launch the
    // user started gets something visible: Welcome the first time, the
    // upload panel afterwards. Launches at sign-in stay quiet.
    let autostarted = std::env::args().any(|arg| arg == AUTOSTART_FLAG) || package::launched_at_sign_in();
    // Restarted by an update installed while the user was away.
    let after_update = settings.quiet_next_launch;
    if after_update {
        core.settings.update(|settings| settings.quiet_next_launch = false);
    }
    if !autostarted && !after_update && launch_links.is_empty() {
        if core.destinations.all().is_empty() {
            windows::open(&handle, AppWindow::Onboarding);
        } else {
            panel::show(&handle);
        }
    }
    Ok(())
}

/// Quits for real, installing an update downloaded in the background first.
pub fn quit(app: &AppHandle) {
    let core = crate::core::core(app);
    if updater::install_pending_on_quit(&core) {
        return;
    }
    app.exit(0);
}
