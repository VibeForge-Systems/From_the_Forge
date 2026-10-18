//! Choosing, checking, and opening the archive library location.

use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::{AppHandle, State, WebviewWindow};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;
use ts_rs::TS;

use super::AppState;
use crate::backups::messages::BUSY_LOCATION;
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
    refuse_during_job(&state)?;
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
    // The picker was open for a while; a job may have started meanwhile.
    // Checked while holding the settings lock, which `open_backup` takes
    // after reserving its slot, so the two cannot interleave.
    refuse_during_job(&state)?;
    let saved = match (status.is_usable(), status.path.clone()) {
        (true, Some(canonical)) => {
            let old = store.settings().archive_directory.clone();
            store.update(|s| s.archive_directory = Some(canonical.clone()))?;
            tracing::info!(state = ?status.state, "archive location saved");
            if old.as_deref() != Some(canonical.as_str()) {
                sweep_in_background(old);
            }
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
    clear_inner(&state).map(|(snapshot, _)| snapshot)
}

fn clear_inner(
    state: &AppState,
) -> IpcResult<(SettingsSnapshot, Option<std::thread::JoinHandle<()>>)> {
    let mut store = state.settings()?;
    // Under the settings lock: see `choose_archive_directory`.
    refuse_during_job(state)?;
    let old = store.settings().archive_directory.clone();
    store.update(|s| s.archive_directory = None)?;
    let sweep = sweep_in_background(old);
    Ok((store.snapshot(), sweep))
}

/// The archive location holds a job's staging directory, so it cannot change
/// while a job runs.
fn refuse_during_job(state: &AppState) -> IpcResult<()> {
    if state.jobs.any_active() {
        return Err(IpcError::new(ErrorCode::Busy, BUSY_LOCATION));
    }
    Ok(())
}

/// Remove leftover staging sessions from a location that is no longer the
/// archive location (§10.4). Runs on its own thread; logs counts only.
pub(crate) fn sweep_in_background(library: Option<String>) -> Option<std::thread::JoinHandle<()>> {
    let library = library?;
    let spawned = std::thread::Builder::new()
        .name("oma-sweep".into())
        .spawn(move || {
            // The crate logs the counts.
            let _ = oma_backup::sweep_stale(Path::new(&library));
        });
    if spawned.is_err() {
        tracing::warn!("staging sweep thread could not start");
    }
    spawned.ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::SettingsStore;
    use std::fs;
    use std::time::{Duration, SystemTime};

    fn state_with_library(tmp: &Path, library: &Path) -> AppState {
        let mut store = SettingsStore::load(tmp.join("config").join("settings.json"));
        let lib = library.to_string_lossy().into_owned();
        store
            .update(|s| s.archive_directory = Some(lib))
            .expect("save");
        AppState::new(store)
    }

    #[test]
    fn location_changes_are_refused_while_a_job_runs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let library = tmp.path().join("lib");
        fs::create_dir(&library).expect("mkdir");
        let state = state_with_library(tmp.path(), &library);
        let r = state
            .jobs
            .reserve("some-backup", crate::backups::dto::JobKind::Validate)
            .expect("reserve");
        let err = clear_inner(&state).expect_err("busy");
        assert_eq!(err.code, ErrorCode::Busy);
        assert_eq!(err.message, BUSY_LOCATION);
        assert!(refuse_during_job(&state).is_err());
        state.jobs.finish(&r.job_id, Default::default());
        assert!(refuse_during_job(&state).is_ok());
    }

    #[test]
    fn the_old_location_is_swept_after_a_change() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let library = tmp.path().join("lib");
        let stale = library.join(".oma-staging").join("s0123456789abcdef");
        fs::create_dir_all(&stale).expect("mkdir");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                library.join(".oma-staging"),
                fs::Permissions::from_mode(0o700),
            )
            .expect("chmod");
        }
        fs::write(stale.join(".lock"), b"").expect("lock file");
        let old = SystemTime::now() - Duration::from_secs(3600);
        #[cfg(windows)]
        let dir = {
            use std::os::windows::fs::OpenOptionsExt;
            // FILE_WRITE_ATTRIBUTES, plus FILE_FLAG_BACKUP_SEMANTICS, without
            // which Windows refuses to open a directory handle.
            fs::OpenOptions::new()
                .access_mode(0x0100)
                .custom_flags(0x0200_0000)
                .open(&stale)
        };
        #[cfg(not(windows))]
        let dir = fs::File::open(&stale);
        dir.and_then(|f| f.set_modified(old)).expect("age");
        let state = state_with_library(tmp.path(), &library);
        let (snapshot, sweep) = clear_inner(&state).expect("clear");
        assert!(snapshot.settings.archive_directory.is_none());
        sweep.expect("sweep thread").join().expect("join");
        assert!(!stale.exists(), "the stale session was removed");
    }
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
