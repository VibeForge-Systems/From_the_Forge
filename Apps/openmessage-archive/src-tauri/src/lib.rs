//! OpenMessage Archive desktop shell.
//!
//! This crate wires the libraries in `crates/` to the Tauri runtime. It owns
//! IPC commands, application state, and settings; backup parsing,
//! extraction, storage, and export live in their own crates so they can be
//! tested without a window.

pub mod archives;
pub mod backups;
pub mod commands;
pub mod error;
pub mod panic_hook;
pub mod settings;
#[cfg(test)]
mod ts_guard;
pub mod viewer;
pub mod window;

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
    // Panic payloads never reach the log (M3 §16.6).
    panic_hook::install();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "starting");

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        // Attachment media (M5 §6). Custom schemes bypass the capability
        // ACL, so the handler authorises every request itself; it never
        // blocks (on WebKitGTK it runs on the GTK main loop).
        .register_asynchronous_uri_scheme_protocol(viewer::media::SCHEME, |ctx, req, responder| {
            media_request(&ctx, &req, responder);
        })
        .setup(|app| {
            let config_dir = app.path().app_config_dir()?;
            let store = SettingsStore::load(config_dir.join(SETTINGS_FILE));
            // Leftovers from a crash or a failed cleanup, in the background.
            commands::archive_location::sweep_in_background(
                store.settings().archive_directory.clone(),
            );
            let viewer_prefs = store.settings().viewer;
            app.manage(AppState::new(store));
            // The main window is built here, not from the config, so its
            // navigation, new-window, download and permission hooks are set
            // (M5 §6.7).
            let (window, origins) = window::build_main(app)?;
            app.state::<AppState>().viewer.set_origins(origins);
            commands::settings::apply_window_effects(&window, &viewer_prefs);
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
            commands::jobs::check_extraction,
            commands::jobs::cancel_job,
            commands::archives::list_archives,
            commands::archives::create_archive,
            commands::archives::get_archive_details,
            commands::archives::start_import,
            commands::archives::confirm_import,
            commands::archives::verify_archive,
            commands::viewer::open_viewer,
            commands::viewer::close_viewer,
            commands::viewer::hide_viewer,
            commands::viewer::list_conversations,
            commands::viewer::get_conversations,
            commands::viewer::get_thread_page,
            commands::viewer::get_reply_thread,
            commands::viewer::get_message_detail,
            commands::viewer::get_message_text,
            commands::viewer::get_day_counts,
            commands::viewer::seek_date,
            commands::viewer::search_messages,
            commands::viewer::get_search_results,
            commands::viewer::cancel_search,
            commands::viewer::inspect_attachment,
        ])
        .build(tauri::generate_context!())
        .expect("the Tauri runtime failed to start")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                // Cancel running jobs (an import waiting at its gate wakes
                // and declines; a batch in flight finishes or rolls back)
                // and let them remove their staged copies. A kill or crash
                // skips this; the startup sweep and the next write open of
                // the archive cover that case.
                if let Some(state) = app.try_state::<AppState>() {
                    state.viewer.close_any();
                    let clean = state.jobs.cancel_all_and_wait(EXIT_WAIT);
                    tracing::info!(clean, "exit");
                }
            }
        });
}

/// How long the exit handler waits for running jobs to clean up.
const EXIT_WAIT: Duration = Duration::from_secs(5);

/// The `oma-media` adapter: Tauri's request and responder in, the
/// Tauri-free core (`viewer::media`) does the rest.
fn media_request<R: tauri::Runtime>(
    ctx: &tauri::UriSchemeContext<'_, R>,
    req: &tauri::http::Request<Vec<u8>>,
    responder: tauri::UriSchemeResponder,
) {
    use viewer::media::{MediaRequest, MediaResponse, Reply};
    let reply = Reply::new(Box::new(move |r: MediaResponse| {
        let mut b = tauri::http::Response::builder().status(r.status);
        for (k, v) in &r.headers {
            b = b.header(*k, v.as_str());
        }
        match b.body(r.body) {
            Ok(resp) => responder.respond(resp),
            Err(_) => {
                let empty = tauri::http::Response::builder()
                    .status(500)
                    .body(Vec::new())
                    .unwrap_or_default();
                responder.respond(empty);
            }
        }
    }));
    let Some(state) = ctx.app_handle().try_state::<AppState>() else {
        return reply.send(viewer::media::Outcome::Refused, MediaResponse::refused());
    };
    let values = |name: &str| -> Vec<&str> {
        req.headers()
            .get_all(name)
            .iter()
            .map(|v| v.to_str().unwrap_or(""))
            .collect()
    };
    let ranges = values("range");
    let origins = values("origin");
    let playback_allowed = state
        .settings()
        .map(|s| s.settings().viewer.media_playback != settings::MediaPlayback::Off)
        .unwrap_or(false);
    let target = req.uri().path_and_query().map_or("", |p| p.as_str());
    viewer::media::dispatch(
        &state.viewer,
        &MediaRequest {
            label: ctx.webview_label(),
            method: req.method().as_str(),
            target,
            ranges: &ranges,
            origins: &origins,
            playback_allowed,
        },
        reply,
    );
}
