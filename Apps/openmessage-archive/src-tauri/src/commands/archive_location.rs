//! Choosing, checking, and opening the archive library location.

use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::{AppHandle, State, WebviewWindow};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;
use ts_rs::TS;

use super::AppState;
use crate::error::{ErrorCode, IpcError, IpcResult};
use crate::settings::{inspect_location, ArchiveLocationStatus, SettingsSnapshot};

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ArchiveLocationChoice {
    pub status: ArchiveLocationStatus,
    /// False when a check failed and the previous location was kept.
    pub saved: bool,
    pub settings: SettingsSnapshot,
}

/// Open a native folder picker, validate the choice, and save it if usable.
/// Returns `None` when the user cancels.
#[tauri::command]
pub async fn choose_archive_directory(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> IpcResult<Option<ArchiveLocationChoice>> {
    let start = state.settings()?.settings().archive_directory.clone();

    let picked = tauri::async_runtime::spawn_blocking(move || {
        let mut dialog = app
            .dialog()
            .file()
            .set_title("Choose where OpenMessage Archive stores archives")
            .set_can_create_directories(true)
            .set_parent(&window);
        if let Some(dir) = start.as_deref().map(Path::new).filter(|p| p.is_dir()) {
            dialog = dialog.set_directory(dir);
        }
        dialog.blocking_pick_folder()
    })
    .await
    .map_err(|_| IpcError::internal("folder picker task failed"))?;

    let Some(picked) = picked else {
        return Ok(None);
    };
    let path: PathBuf = picked.into_path().map_err(|_| {
        IpcError::new(
            ErrorCode::Platform,
            "The selected location is not a local folder.",
        )
    })?;

    let status = inspect_location(&path);
    let mut store = state.settings()?;
    let saved = match (status.is_usable(), status.path.clone()) {
        (true, Some(canonical)) => {
            store.update(|s| s.archive_directory = Some(canonical))?;
            tracing::info!(state = ?status.state, "archive location saved");
            true
        }
        _ => {
            tracing::info!(state = ?status.state, "archive location rejected");
            false
        }
    };
    Ok(Some(ArchiveLocationChoice {
        status,
        saved,
        settings: store.snapshot(),
    }))
}

/// Re-run every check against the configured location. A location that was
/// fine yesterday can be gone today (unplugged drive, revoked permission).
#[tauri::command]
pub fn get_archive_location_status(state: State<'_, AppState>) -> IpcResult<ArchiveLocationStatus> {
    let configured = state.settings()?.settings().archive_directory.clone();
    Ok(match configured {
        Some(dir) => inspect_location(Path::new(&dir)),
        None => ArchiveLocationStatus::unset(),
    })
}

/// Forget the configured location. Nothing on disk is deleted.
#[tauri::command]
pub fn clear_archive_directory(state: State<'_, AppState>) -> IpcResult<SettingsSnapshot> {
    let mut store = state.settings()?;
    store.update(|s| s.archive_directory = None)?;
    Ok(store.snapshot())
}

/// Show the configured location in the system file manager. Only the
/// configured, validated directory can be opened this way.
#[tauri::command]
pub fn open_archive_directory(app: AppHandle, state: State<'_, AppState>) -> IpcResult<()> {
    let configured = state.settings()?.settings().archive_directory.clone();
    let Some(dir) = configured else {
        return Err(IpcError::new(
            ErrorCode::NotConfigured,
            "No archive location is configured.",
        ));
    };
    let status = inspect_location(Path::new(&dir));
    if !status.is_usable() {
        return Err(IpcError::new(
            ErrorCode::Io,
            "The archive location is not available. Check it in Settings.",
        ));
    }
    app.opener()
        .open_path(dir, None::<&str>)
        .map_err(|_| IpcError::new(ErrorCode::Platform, "The file manager could not be opened."))
}
