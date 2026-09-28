// Keeps a console window from opening next to the app in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    aktar_lib::run()
}
