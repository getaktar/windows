// Keeps a console window from opening next to the app in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if aktar_lib::serve_explorer_command() {
        return;
    }
    aktar_lib::run()
}
