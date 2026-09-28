//! The regular app windows. Each one is created on demand and destroyed
//! when closed; Aktar itself keeps running in the notification area.

use std::time::Duration;

use tauri::window::Color;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::t;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppWindow {
    Library,
    Settings,
    Onboarding,
    Update,
}

impl AppWindow {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "library" => Some(AppWindow::Library),
            "settings" => Some(AppWindow::Settings),
            "onboarding" => Some(AppWindow::Onboarding),
            "update" => Some(AppWindow::Update),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AppWindow::Library => "library",
            AppWindow::Settings => "settings",
            AppWindow::Onboarding => "onboarding",
            AppWindow::Update => "update",
        }
    }

    fn title(self) -> String {
        match self {
            AppWindow::Library => format!("{} - Aktar", t!("Library")),
            AppWindow::Settings => format!("{} - Aktar", t!("Settings")),
            AppWindow::Onboarding => t!("Welcome to Aktar"),
            AppWindow::Update => t!("Software Update"),
        }
    }
}

/// Creating a webview from the main thread (a sync command, a tray or
/// menu handler) deadlocks on Windows, so windows are opened from the
/// async runtime.
pub fn open(app: &AppHandle, which: AppWindow) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move { open_now(&app, which) });
}

fn open_now(app: &AppHandle, which: AppWindow) {
    crate::panel::hide(app);
    if let Some(window) = app.get_webview_window(which.label()) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }

    let url = WebviewUrl::App(format!("index.html#/{}", which.label()).into());
    let builder = WebviewWindowBuilder::new(app, which.label(), url)
        .title(which.title())
        .background_color(background_color())
        .center();
    let builder = match which {
        AppWindow::Library => builder.inner_size(1100.0, 680.0).min_inner_size(860.0, 500.0),
        AppWindow::Settings => builder.inner_size(880.0, 640.0).min_inner_size(720.0, 480.0),
        AppWindow::Onboarding => builder
            .inner_size(480.0, 400.0)
            .resizable(false)
            .maximizable(false)
            .minimizable(false),
        AppWindow::Update => builder
            .inner_size(520.0, 360.0)
            .resizable(false)
            .maximizable(false)
            .minimizable(false)
            .always_on_top(true),
    };
    match builder.build() {
        Ok(window) => {
            let _ = window.set_focus();
        }
        Err(error) => log::error!("Could not open the {} window: {error}", which.label()),
    }
}

/// Fluent's colorNeutralBackground2 for the current app mode (styles.css
/// sets the same on the page), so a new window doesn't flash white in dark
/// mode before its page has loaded.
pub fn background_color() -> Color {
    if crate::system::apps_use_light_theme() {
        Color(0xfa, 0xfa, 0xfa, 0xff)
    } else {
        Color(0x1f, 0x1f, 0x1f, 0xff)
    }
}

/// After the first destination is saved: Welcome closes, then the panel
/// shows where Aktar lives from now on. In that order, and with a moment in
/// between, so focus moving on from the closed window doesn't count as a
/// click outside the panel and close it again.
pub fn finish_onboarding(app: &AppHandle) {
    close(app, AppWindow::Onboarding);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(250)).await;
        crate::panel::show(&app);
    });
}

pub fn close(app: &AppHandle, which: AppWindow) {
    if let Some(window) = app.get_webview_window(which.label()) {
        let _ = window.close();
    }
}

/// Window titles are set natively, so they're updated when the language
/// changes (the page contents re-render on their own).
pub fn retitle_all(app: &AppHandle) {
    for which in [AppWindow::Library, AppWindow::Settings, AppWindow::Onboarding, AppWindow::Update] {
        if let Some(window) = app.get_webview_window(which.label()) {
            let _ = window.set_title(&which.title());
        }
    }
}
