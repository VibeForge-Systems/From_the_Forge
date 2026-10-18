//! OpenMessage Archive desktop shell.
//!
//! This crate wires the libraries in `crates/` to the Tauri runtime. It owns
//! IPC commands, application state, and settings; backup parsing,
//! extraction, storage, and export live in their own crates so they can be
//! tested without a window.

pub mod backups;
pub mod commands;
pub mod error;
pub mod settings;

use std::time::Duration;

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
            // Leftovers from a crash or a failed cleanup, in the background.
            commands::archive_location::sweep_in_background(
                store.settings().archive_directory.clone(),
            );
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
            commands::backups::list_backups,
            commands::backups::add_backup_folder,
            commands::backups::forget_backup_folder,
            commands::backups::get_backup_details,
            commands::jobs::open_backup,
            commands::jobs::cancel_job,
        ])
        .build(tauri::generate_context!())
        .expect("the Tauri runtime failed to start")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                // Cancel running checks and let them remove their staged
                // copies. A kill or crash skips this; the startup sweep
                // covers that case.
                if let Some(state) = app.try_state::<AppState>() {
                    let clean = state.jobs.cancel_all_and_wait(EXIT_WAIT);
                    tracing::info!(clean, "exit");
                }
            }
        });
}

/// How long the exit handler waits for running jobs to clean up.
const EXIT_WAIT: Duration = Duration::from_secs(5);
