//! Listing backups, adding and forgetting backup folders, and the details
//! panel (§11.3). No command takes a path: added folders come from a native
//! picker Rust opens, and everything else is an id Rust computed.

use std::path::{Path, PathBuf};

use oma_backup::{
    classify_added_folder, default_backup_roots, inspect, AddedFolder, BackupError, Candidate,
    Limits,
};
use tauri::{AppHandle, Manager, State, WebviewWindow};
use tauri_plugin_dialog::DialogExt;

use super::AppState;
use crate::backups::dto::{
    AddBackupFolderResult, AddFolderOutcome, BackupDetails, BackupList, BackupRef,
    ForgetBackupFolderResult, LocationRef,
};
use crate::backups::listing::{self, location_id_for, ArchiveFacts, ListInputs};
use crate::backups::{messages, BackupRegistry, ListingGuard};
use crate::error::{ErrorCode, IpcError, IpcResult};
use crate::settings::{
    inspect_location, AddedFolderEntry, AddedFolderKind, SettingsStore, MAX_BACKUP_FOLDERS,
};

/// The standard locations. Scanned on Windows only; Linux has none (§3.1).
fn default_roots(app: &AppHandle) -> Vec<oma_backup::DefaultRoot> {
    if cfg!(windows) {
        let paths = app.path();
        default_backup_roots(
            paths.home_dir().ok().as_deref(),
            paths.data_dir().ok().as_deref(),
        )
    } else {
        default_backup_roots(None, None)
    }
}

fn list_inputs(app: &AppHandle, store: &SettingsStore) -> ListInputs {
    ListInputs {
        default_roots: default_roots(app),
        added: store.settings().backup_folders.0.clone(),
        library: store
            .settings()
            .archive_directory
            .as_deref()
            .map(PathBuf::from),
        limits: Limits::default(),
    }
}

/// How often a listing is rebuilt because the settings changed while it ran.
const RELIST_ATTEMPTS: usize = 4;

/// The settings a listing was built from still match the current settings.
fn inputs_current(
    store: &SettingsStore,
    added: &[AddedFolderEntry],
    library: Option<&Path>,
) -> bool {
    let s = store.settings();
    s.backup_folders.0.as_slice() == added
        && s.archive_directory.as_deref().map(Path::new) == library
}

/// Scan and inspect on the blocking pool, then publish the new registry.
async fn relist(app: &AppHandle, state: &AppState) -> IpcResult<BackupList> {
    relist_with(state, |store| list_inputs(app, store)).await
}

/// `relist` with the inputs supplied by the caller. A folder added or
/// forgotten (or an archive location changed) while the scan ran makes the
/// scan stale: it is rebuilt from the new settings rather than published over
/// the change. The check and the publish happen under the settings lock, which
/// every command that changes those settings holds while it changes them.
async fn relist_with(
    state: &AppState,
    mut make_inputs: impl FnMut(&SettingsStore) -> ListInputs + Send,
) -> IpcResult<BackupList> {
    for _ in 0..RELIST_ATTEMPTS {
        let inputs = make_inputs(&*state.settings()?);
        let added = inputs.added.clone();
        let library = inputs.library.clone();
        let reg = tauri::async_runtime::spawn_blocking(move || listing::build_registry(&inputs))
            .await
            .map_err(|_| IpcError::internal("backup listing task failed"))?;
        let store = state.settings()?;
        if inputs_current(&store, &added, library.as_deref()) {
            let list = reg.list(&state.jobs);
            *state.registry()? = reg;
            return Ok(list);
        }
        drop(store);
        tracing::debug!("backup folders changed during listing; scanning again");
    }
    Err(IpcError::new(ErrorCode::Busy, messages::BUSY_LISTING))
}

/// Scan the standard locations and the added folders. Runs only when the
/// Backups page opens or the user clicks Refresh, never at startup.
#[tauri::command]
pub async fn list_backups(app: AppHandle, state: State<'_, AppState>) -> IpcResult<BackupList> {
    let Some(_guard) = ListingGuard::try_acquire(&state.listing) else {
        return Err(IpcError::new(ErrorCode::Busy, messages::BUSY_LISTING));
    };
    relist(&app, &state).await
}

/// Location ids computed off the settings lock: canonicalizing a path can
/// block for a long time on a disconnected network drive.
struct ListedIds {
    picked_location: Option<String>,
    picked_backup: Option<String>,
    /// Added folder path → its location id.
    added: Vec<(String, Option<String>)>,
    defaults: Vec<Option<String>>,
}

fn listed_ids(
    picked: &Path,
    added: Vec<String>,
    defaults: &[oma_backup::DefaultRoot],
) -> ListedIds {
    let id = |p: &Path| location_id_for(p).map(|l| l.as_str().to_owned());
    ListedIds {
        picked_location: id(picked),
        picked_backup: oma_backup::BackupId::for_path(picked).map(|b| b.as_str().to_owned()),
        added: added
            .into_iter()
            .map(|p| {
                let l = id(Path::new(&p));
                (p, l)
            })
            .collect(),
        defaults: defaults.iter().map(|d| id(&d.path)).collect(),
    }
}

/// No file I/O: every id was computed by `listed_ids`. An added folder that
/// appeared after the ids were computed is compared by path only.
fn already_listed(
    ids: &ListedIds,
    picked: &Path,
    registry: &BackupRegistry,
    added: &[AddedFolderEntry],
) -> bool {
    if ids
        .picked_backup
        .as_deref()
        .is_some_and(|b| registry.backups.contains_key(b))
    {
        return true;
    }
    let Some(id) = ids.picked_location.as_deref() else {
        return false;
    };
    added
        .iter()
        .any(|e| match ids.added.iter().find(|(p, _)| *p == e.path) {
            Some((_, l)) => l.as_deref() == Some(id),
            None => Path::new(&e.path) == picked,
        })
        || ids.defaults.iter().any(|l| l.as_deref() == Some(id))
}

/// Open a native folder picker and add what the user chose: a backup, the
/// backup that encloses the chosen folder, or a folder that contains
/// backups. `None` when the user cancels.
#[tauri::command]
pub async fn add_backup_folder(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> IpcResult<Option<AddBackupFolderResult>> {
    let (start, library) = {
        let store = state.settings()?;
        (
            store
                .settings()
                .backup_folders
                .last()
                .map(|e| PathBuf::from(&e.path)),
            store
                .settings()
                .archive_directory
                .as_deref()
                .map(PathBuf::from),
        )
    };

    let picker_app = app.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || {
        let mut dialog = picker_app
            .dialog()
            .file()
            .set_title("Choose an iPhone backup folder, or a folder that contains backups")
            .set_parent(&window);
        if let Some(dir) = start.as_deref().filter(|p| p.is_dir()) {
            dialog = dialog.set_directory(dir);
        }
        dialog.blocking_pick_folder()
    })
    .await
    .map_err(|_| IpcError::internal("folder picker task failed"))?;
    let Some(picked) = picked else {
        return Ok(None);
    };
    let picked: PathBuf = picked.into_path().map_err(|_| {
        IpcError::new(
            ErrorCode::Platform,
            "The selected location is not a local folder.",
        )
    })?;

    let classified = tauri::async_runtime::spawn_blocking(move || {
        let limits = Limits::default();
        classify_added_folder(&picked, library.as_deref(), &limits).map(|added| {
            let (path, kind, outcome) = match &added {
                AddedFolder::Backup(c) => (
                    c.display_canonical.clone(),
                    AddedFolderKind::Backup,
                    AddFolderOutcome::Added,
                ),
                AddedFolder::BackupAncestor { backup } => (
                    backup.display_canonical.clone(),
                    AddedFolderKind::Backup,
                    AddFolderOutcome::AddedAncestor,
                ),
                AddedFolder::Parent(scan) => (
                    scan.display_canonical
                        .clone()
                        .unwrap_or_else(|| dunce::simplified(&picked).to_path_buf()),
                    AddedFolderKind::Parent,
                    AddFolderOutcome::Added,
                ),
            };
            (path, kind, outcome)
        })
    })
    .await
    .map_err(|_| IpcError::internal("folder classification task failed"))?;

    let refused = |message: &str| -> IpcResult<Option<AddBackupFolderResult>> {
        let list = state.registry()?.list(&state.jobs);
        Ok(Some(AddBackupFolderResult {
            outcome: AddFolderOutcome::Refused,
            message: message.to_owned(),
            saved: false,
            list,
        }))
    };

    let (path, kind, outcome) = match classified {
        Ok(found) => found,
        Err(reason) => {
            tracing::info!(reason = ?reason, "backup folder refused");
            return refused(messages::refusal(reason));
        }
    };
    let Some(path_text) = path.to_str().map(str::to_owned) else {
        return refused(messages::refusal(oma_backup::RefusalReason::NonUnicodePath));
    };

    let defaults = default_roots(&app);
    let added_paths: Vec<String> = state
        .settings()?
        .settings()
        .backup_folders
        .iter()
        .map(|e| e.path.clone())
        .collect();
    let id_path = path.clone();
    let ids =
        tauri::async_runtime::spawn_blocking(move || listed_ids(&id_path, added_paths, &defaults))
            .await
            .map_err(|_| IpcError::internal("backup folder lookup task failed"))?;
    let duplicate = {
        let store = state.settings()?;
        let registry = state.registry()?;
        already_listed(&ids, &path, &registry, &store.settings().backup_folders)
    };
    if duplicate {
        let list = state.registry()?.list(&state.jobs);
        return Ok(Some(AddBackupFolderResult {
            outcome: AddFolderOutcome::AlreadyListed,
            message: "That folder is already listed.".to_owned(),
            saved: false,
            list,
        }));
    }

    let saved = {
        let mut store = state.settings()?;
        if store.settings().backup_folders.len() >= MAX_BACKUP_FOLDERS {
            drop(store);
            return refused(messages::TOO_MANY_FOLDERS);
        }
        let entry = AddedFolderEntry {
            path: path_text,
            kind,
        };
        match store.update(|s| s.backup_folders.push(entry)) {
            Ok(()) => true,
            Err(e) if e.code == ErrorCode::SettingsReadOnly => false,
            Err(e) => return Err(e),
        }
    };
    tracing::info!(kind = ?kind, saved, "backup folder added");

    let list = match ListingGuard::try_acquire(&state.listing) {
        Some(_guard) => relist(&app, &state).await?,
        None => state.registry()?.list(&state.jobs),
    };
    let message = match outcome {
        AddFolderOutcome::AddedAncestor => {
            "You chose a folder inside a backup. The backup folder itself was added."
        }
        _ => "Backup folder added.",
    };
    Ok(Some(AddBackupFolderResult {
        outcome,
        message: message.to_owned(),
        saved,
        list,
    }))
}

/// Remove an added folder from the list. Nothing on disk is touched. A
/// default location cannot be forgotten.
///
/// Async, so it runs off the UI thread: matching the id canonicalizes every
/// added folder, which can block on a disconnected drive. That happens on the
/// blocking pool with no lock held.
#[tauri::command]
pub async fn forget_backup_folder(
    state: State<'_, AppState>,
    request: LocationRef,
) -> IpcResult<ForgetBackupFolderResult> {
    let paths: Vec<String> = state
        .settings()?
        .settings()
        .backup_folders
        .iter()
        .map(|e| e.path.clone())
        .collect();
    let wanted = request.location_id.clone();
    let path = tauri::async_runtime::spawn_blocking(move || {
        paths
            .into_iter()
            .find(|p| location_id_for(Path::new(p)).is_some_and(|id| id.as_str() == wanted))
    })
    .await
    .map_err(|_| IpcError::internal("backup folder lookup task failed"))?
    .ok_or_else(|| IpcError::new(ErrorCode::BackupNotFound, messages::BACKUP_NOT_FOUND))?;
    forget_path(&state, &request.location_id, &path)
}

/// Forget the added folder at `path` (listed as `location_id`). No file I/O
/// apart from saving the settings. The registry changes only when the
/// settings did: on a write failure both stay as they were. Read-only
/// settings still forget the folder for this session (`saved` is false), as
/// `add_backup_folder` does.
fn forget_path(
    state: &AppState,
    location_id: &str,
    path: &str,
) -> IpcResult<ForgetBackupFolderResult> {
    let mut store = state.settings()?;
    // Re-checked under the lock: the folder may have been forgotten meanwhile.
    let index = store
        .settings()
        .backup_folders
        .iter()
        .position(|e| e.path == path)
        .ok_or_else(|| IpcError::new(ErrorCode::BackupNotFound, messages::BACKUP_NOT_FOUND))?;
    {
        let registry = state.registry()?;
        let busy = registry.backups.values().any(|b| {
            b.summary.location_id == location_id && state.jobs.active_for(&b.summary.id).is_some()
        });
        if busy {
            return Err(IpcError::new(ErrorCode::Busy, messages::BUSY_FORGET));
        }
    }
    let saved = match store.update(|s| {
        s.backup_folders.remove(index);
    }) {
        Ok(()) => true,
        Err(e) if e.code == ErrorCode::SettingsReadOnly => false,
        Err(e) => return Err(e),
    };
    // Still under the settings lock, so a listing cannot publish in between.
    let mut registry = state.registry()?;
    registry.forget_location(location_id);
    drop(store);
    tracing::info!(saved, "backup folder forgotten");
    Ok(ForgetBackupFolderResult {
        saved,
        list: registry.list(&state.jobs),
    })
}

/// Facts about the archive location for the details panel.
pub(super) fn archive_facts(library: Option<&str>) -> ArchiveFacts {
    let library = library.map(PathBuf::from);
    let (configured, sync_provider) = match &library {
        Some(lib) => {
            let status = inspect_location(lib);
            (
                status.is_usable(),
                oma_fs::cloud_sync_provider(lib).map(str::to_owned),
            )
        }
        None => (false, None),
    };
    ArchiveFacts {
        library,
        configured,
        sync_provider,
    }
}

/// Inspect one backup again, against the current disk state. This is also
/// how a `notInspected` backup gets inspected.
#[tauri::command]
pub async fn get_backup_details(
    state: State<'_, AppState>,
    request: BackupRef,
) -> IpcResult<BackupDetails> {
    let not_found = || IpcError::new(ErrorCode::BackupNotFound, messages::BACKUP_NOT_FOUND);
    let (candidate, location_id) = {
        let registry = state.registry()?;
        let entry = registry
            .backups
            .get(&request.backup_id)
            .ok_or_else(not_found)?;
        (entry.candidate.clone(), entry.summary.location_id.clone())
    };
    let library = state.settings()?.settings().archive_directory.clone();
    let jobs = state.jobs.clone();
    let details = tauri::async_runtime::spawn_blocking(move || {
        let limits = Limits::default();
        let insp = match inspect(&candidate, &limits, &mut 0) {
            Ok(i) => i,
            Err(BackupError::Io {
                kind: std::io::ErrorKind::NotFound,
                ..
            }) => {
                return Err(not_found());
            }
            Err(BackupError::NotABackup(_)) => return Err(not_found()),
            Err(e) => return Err(messages::to_ipc(&e, &limits)),
        };
        let facts = archive_facts(library.as_deref());
        Ok(listing::details(
            &insp,
            &location_id,
            &facts,
            &jobs,
            &limits,
        ))
    })
    .await
    .map_err(|_| IpcError::internal("backup inspection task failed"))??;
    state.registry()?.update_summary(details.summary.clone());
    Ok(details)
}

/// The candidate for a backup id, or `backupNotFound`.
pub(super) fn candidate_for(state: &AppState, backup_id: &str) -> IpcResult<(Candidate, String)> {
    let registry = state.registry()?;
    registry
        .backups
        .get(backup_id)
        .map(|b| (b.candidate.clone(), b.summary.location_id.clone()))
        .ok_or_else(|| IpcError::new(ErrorCode::BackupNotFound, messages::BACKUP_NOT_FOUND))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backups::dto::{BackupLocation, BackupLocationKind, BackupLocationState};
    use crate::backups::LocationEntry;
    use std::fs;

    fn settings_file(tmp: &Path) -> PathBuf {
        tmp.join("config").join("settings.json")
    }

    fn entry(folder: &Path) -> AddedFolderEntry {
        AddedFolderEntry {
            path: folder.to_string_lossy().into_owned(),
            kind: AddedFolderKind::Parent,
        }
    }

    fn state_with_folder(tmp: &Path, folder: &Path) -> AppState {
        let mut store = SettingsStore::load(settings_file(tmp));
        store
            .update(|s| s.backup_folders.push(entry(folder)))
            .expect("save");
        AppState::new(store)
    }

    fn inputs(store: &SettingsStore) -> ListInputs {
        ListInputs {
            default_roots: Vec::new(),
            added: store.settings().backup_folders.0.clone(),
            library: store
                .settings()
                .archive_directory
                .as_deref()
                .map(PathBuf::from),
            limits: Limits::default(),
        }
    }

    fn location_id(folder: &Path) -> String {
        location_id_for(folder).expect("id").as_str().to_owned()
    }

    fn register(state: &AppState, folder: &Path) {
        state
            .registry()
            .expect("registry")
            .locations
            .push(LocationEntry {
                dto: BackupLocation {
                    id: location_id(folder),
                    kind: BackupLocationKind::AddedParent,
                    path: folder.to_string_lossy().into_owned(),
                    via_link: false,
                    state: BackupLocationState::Present,
                    backup_count: 0,
                    message: None,
                },
                added: true,
            });
    }

    #[test]
    fn a_listing_built_from_stale_settings_is_rebuilt_not_published() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let folder = tmp.path().join("backups");
        fs::create_dir(&folder).expect("mkdir");
        let state = state_with_folder(tmp.path(), &folder);
        let stale = state
            .settings()
            .expect("settings")
            .settings()
            .backup_folders
            .0
            .clone();
        // The folder is forgotten while a listing that still holds it runs.
        state
            .settings()
            .expect("settings")
            .update(|s| {
                s.backup_folders.remove(0);
            })
            .expect("save");
        let mut calls = 0;
        let list = tauri::async_runtime::block_on(relist_with(&state, |store| {
            calls += 1;
            let mut i = inputs(store);
            if calls == 1 {
                i.added = stale.clone();
            }
            i
        }))
        .expect("list");
        assert_eq!(calls, 2, "the stale scan was rebuilt");
        assert!(list.locations.is_empty(), "the forgotten folder stays gone");
        assert!(state.registry().expect("registry").locations.is_empty());
    }

    #[test]
    fn a_failed_settings_write_leaves_the_registry_alone() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let folder = tmp.path().join("backups");
        fs::create_dir(&folder).expect("mkdir");
        let state = state_with_folder(tmp.path(), &folder);
        register(&state, &folder);
        // A non-empty directory where the settings file goes: the write fails.
        let file = settings_file(tmp.path());
        fs::remove_file(&file).expect("rm");
        fs::create_dir(&file).expect("mkdir");
        fs::write(file.join("keep"), b"").expect("write");

        let id = location_id(&folder);
        let path = folder.to_string_lossy().into_owned();
        let err = forget_path(&state, &id, &path).expect_err("write fails");
        assert_eq!(err.code, ErrorCode::Io);
        assert!(state.registry().expect("registry").location(&id).is_some());
        assert_eq!(
            state
                .settings()
                .expect("settings")
                .settings()
                .backup_folders
                .len(),
            1
        );
    }

    #[test]
    fn read_only_settings_still_forget_for_the_session() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let folder = tmp.path().join("backups");
        fs::create_dir(&folder).expect("mkdir");
        let file = settings_file(tmp.path());
        fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
        fs::write(&file, br#"{"schemaVersion":99}"#).expect("write");
        let mut store = SettingsStore::load(file);
        assert!(store.is_read_only());
        let _ = store.update(|s| s.backup_folders.push(entry(&folder)));
        let state = AppState::new(store);
        register(&state, &folder);

        let id = location_id(&folder);
        let path = folder.to_string_lossy().into_owned();
        let result = forget_path(&state, &id, &path).expect("forgotten");
        assert!(!result.saved);
        assert!(result.list.locations.is_empty());
        assert!(state
            .settings()
            .expect("settings")
            .settings()
            .backup_folders
            .is_empty());
    }

    #[test]
    fn already_listed_compares_precomputed_ids() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let folder = tmp.path().join("backups");
        fs::create_dir(&folder).expect("mkdir");
        let added = vec![entry(&folder)];
        let registry = BackupRegistry::default();
        let ids = listed_ids(&folder, vec![added[0].path.clone()], &[]);
        assert!(already_listed(&ids, &folder, &registry, &added));
        let other = tmp.path().join("other");
        let ids = listed_ids(&other, vec![added[0].path.clone()], &[]);
        assert!(!already_listed(&ids, &other, &registry, &added));
        // An entry added after the ids were computed is compared by path.
        let ids = listed_ids(&folder, Vec::new(), &[]);
        assert!(already_listed(&ids, &folder, &registry, &added));
    }
}
