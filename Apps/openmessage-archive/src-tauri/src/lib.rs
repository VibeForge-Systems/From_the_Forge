//! OpenMessage Archive desktop shell.
//!
//! This crate wires the libraries in `crates/` to the Tauri runtime. It owns
//! IPC commands, application state, and settings; backup parsing,
//! extraction, storage, and export live in their own crates so they can be
//! tested without a window.

pub mod commands;
pub mod error;
pub mod settings;

use tauri::Manager;
use tracing_subscriber::EnvFilter;

use commands::AppState;
use settings::SettingsStore;

const SETTINGS_FILE: &str = "settings.json";

/// Diagnostic logging goes to stderr only. Set `OMA_LOG=debug` for more
/// detail. Log statements record kinds and states, never message bodies,
/// contact details, or passwords; see docs/privacy.md.
fn init_tracing() {
    let filter = EnvFilter::try_from_env("OMA_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

pub fn run() {
    init_tracing();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "starting");

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let config_dir = app.path().app_config_dir()?;
            let store = SettingsStore::load(config_dir.join(SETTINGS_FILE));
            app.manage(AppState::new(store));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::settings::get_settings,
            commands::settings::update_settings,
            commands::archive_location::choose_archive_directory,
            commands::archive_location::get_archive_location_status,
            commands::archive_location::clear_archive_directory,
            commands::archive_location::open_archive_directory,
            commands::system::get_app_info,
        ])
        .run(tauri::generate_context!())
        .expect("the Tauri runtime failed to start");
}
