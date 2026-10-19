// Declaring the application's commands here makes tauri-build generate an
// `allow-<command>` / `deny-<command>` permission pair for each one. Once a
// manifest is declared, a command is callable from the webview only if a
// capability in capabilities/ grants it — least privilege by construction.
const COMMANDS: &[&str] = &[
    "get_settings",
    "update_settings",
    "choose_archive_directory",
    "get_archive_location_status",
    "clear_archive_directory",
    "open_archive_directory",
    "get_app_info",
    "list_backups",
    "add_backup_folder",
    "forget_backup_folder",
    "get_backup_details",
    "open_backup",
    "check_extraction",
    "cancel_job",
    "list_archives",
    "create_archive",
    "get_archive_details",
    "start_import",
    "confirm_import",
    "verify_archive",
    "open_viewer",
    "close_viewer",
    "hide_viewer",
    "list_conversations",
    "get_conversations",
    "get_thread_page",
    "get_reply_thread",
    "get_message_detail",
    "get_message_text",
    "get_day_counts",
    "seek_date",
    "search_messages",
    "get_search_results",
    "cancel_search",
    "inspect_attachment",
];

fn main() {
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("tauri-build failed");
}
