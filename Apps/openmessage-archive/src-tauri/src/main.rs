// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // The image decode helper (M5 §5.5) is this binary started with one
    // argument; it runs before Tauri, logging or settings are touched.
    if openmessage_archive_lib::viewer::media::helper::is_helper_invocation() {
        std::process::exit(openmessage_archive_lib::viewer::media::helper::run_helper());
    }
    openmessage_archive_lib::run();
}
