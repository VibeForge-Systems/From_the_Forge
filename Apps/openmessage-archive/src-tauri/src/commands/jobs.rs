//! Starting and cancelling jobs: validation (§11.3) and the extraction check
//! (M3 §16.2). Progress and the terminal event travel over a
//! `tauri::ipc::Channel<JobEvent>` (D2), which needs no `core:*` permission.

use std::path::PathBuf;

use oma_backup::{inspect, library_relation, LibraryRelation, Limits, Password};
use tauri::ipc::Channel;
use tauri::State;

use super::backups::candidate_for;
use super::AppState;
use crate::backups::dto::{
    BackupPassword, CheckExtractionRequest, JobEvent, JobKind, JobRef, JobStarted,
    OpenBackupRequest,
};
use crate::backups::worker::{self, JobSpec};
use crate::backups::{messages, JobEventSink};
use crate::error::{ErrorCode, IpcError, IpcResult};
use crate::settings::inspect_location;

/// `Channel<JobEvent>` as the worker's sink. A failed delivery is logged by
/// kind only and never cancels the job: the UI recovers by polling.
struct ChannelSink(Channel<JobEvent>);

impl JobEventSink for ChannelSink {
    fn send(&mut self, event: JobEvent) {
        if self.0.send(event).is_err() {
            tracing::debug!("job event not delivered");
        }
    }
}

/// Start validating a backup. Returns once the job has started; its outcome
/// arrives on `on_event`.
#[tauri::command]
pub async fn open_backup(
    state: State<'_, AppState>,
    request: OpenBackupRequest,
    on_event: Channel<JobEvent>,
) -> IpcResult<JobStarted> {
    let OpenBackupRequest {
        backup_id,
        password,
    } = request;
    start_job(&state, backup_id, password, JobKind::Validate, on_event).await
}

/// Start an extraction check (M3 §2): validate, then, if the backup is
/// importable, copy the Messages database into staging, read it to count
/// what can be extracted, and delete the copy. Returns once the job has
/// started; a summary of its outcome arrives on `on_event`, and the reports
/// are fetched with `get_backup_details` (D47).
#[tauri::command]
pub async fn check_extraction(
    state: State<'_, AppState>,
    request: CheckExtractionRequest,
    on_event: Channel<JobEvent>,
) -> IpcResult<JobStarted> {
    let CheckExtractionRequest {
        backup_id,
        password,
    } = request;
    start_job(
        &state,
        backup_id,
        password,
        JobKind::ExtractionCheck,
        on_event,
    )
    .await
}

async fn start_job(
    state: &AppState,
    backup_id: String,
    password: Option<BackupPassword>,
    kind: JobKind,
    on_event: Channel<JobEvent>,
) -> IpcResult<JobStarted> {
    let (spec, done) = prepare_job(state, backup_id, password, kind).await?;
    let job_id = spec.job_id.clone();
    if worker::spawn(
        spec,
        state.jobs.clone(),
        Box::new(ChannelSink(on_event)),
        done,
    )
    .is_err()
    {
        state.jobs.release(&job_id);
        return Err(IpcError::internal("job thread could not start"));
    }
    Ok(JobStarted { job_id })
}

/// Steps 1–5 of starting a job, shared by both kinds: re-inspect, the
/// password, the archive location, the job slot (D7) and
/// `confirm_library_unchanged`. On success the slot is reserved and the
/// caller must start the worker or release it.
async fn prepare_job(
    state: &AppState,
    backup_id: String,
    password: Option<BackupPassword>,
    kind: JobKind,
) -> IpcResult<(JobSpec, std::sync::mpsc::Sender<()>)> {
    let limits = Limits::default();

    // 1. The id must be one Rust handed out.
    let (candidate, _location) = candidate_for(state, &backup_id)?;
    let configured_library = state.settings()?.settings().archive_directory.clone();
    let library = configured_library.clone().map(PathBuf::from);

    // 2. Re-inspect against the current disk state, and check the archive
    //    location (step 4), on the blocking pool.
    let check_limits = limits.clone();
    let check_library = library.clone();
    let (inspection, library_usable, library_inside) =
        tauri::async_runtime::spawn_blocking(move || {
            let inspection = inspect(&candidate, &check_limits, &mut 0);
            let usable = check_library
                .as_deref()
                .is_some_and(|l| inspect_location(l).is_usable());
            let inside = check_library.as_deref().is_some_and(|l| {
                library_relation(&candidate, l) == LibraryRelation::LibraryInsideBackup
            });
            (inspection, usable, inside)
        })
        .await
        .map_err(|_| IpcError::internal("backup inspection task failed"))?;
    let inspection = match inspection {
        Ok(i) => i,
        Err(oma_backup::BackupError::Io {
            kind: std::io::ErrorKind::NotFound,
            ..
        }) => {
            return Err(IpcError::new(
                ErrorCode::BackupNotFound,
                messages::BACKUP_NOT_FOUND,
            ))
        }
        Err(e) => return Err(messages::to_ipc(&e, &limits)),
    };
    if let Some(e) = messages::classification_error(&inspection.classification, &limits) {
        return Err(e);
    }

    // 3. The password: required when encrypted, refused when too long, and
    //    dropped (zeroized) when the backup is not encrypted. Never logged.
    let encrypted = inspection.is_encrypted() == Some(true);
    let password = match (encrypted, password) {
        (false, _) => None,
        (true, None) => {
            return Err(IpcError::new(
                ErrorCode::PasswordRequired,
                messages::PASSWORD_REQUIRED,
            ))
        }
        (true, Some(p)) if p.is_empty() => {
            return Err(IpcError::new(
                ErrorCode::PasswordRequired,
                messages::PASSWORD_REQUIRED,
            ))
        }
        (true, Some(p)) => match Password::new(p.into_inner(), &limits) {
            Ok(p) => Some(p),
            Err(e) => return Err(messages::to_ipc(&e, &limits)),
        },
    };

    // 4. A usable archive location, which is not inside this backup.
    let library = match (library, library_usable) {
        (Some(l), true) => l,
        _ => {
            return Err(IpcError::new(
                ErrorCode::NotConfigured,
                messages::NOT_CONFIGURED,
            ))
        }
    };
    if library_inside {
        return Err(IpcError::new(
            ErrorCode::NotConfigured,
            messages::LIBRARY_INSIDE_BACKUP,
        ));
    }

    // 5. The job slot. The archive location may have changed while step 2
    //    ran (nothing was reserved yet, so the location commands allowed it);
    //    staging must never go to a location the user just left.
    let reservation = state.jobs.reserve(&backup_id, kind)?;
    let job_id = reservation.job_id.clone();
    confirm_library_unchanged(state, &job_id, configured_library.as_deref())?;

    Ok((
        JobSpec {
            job_id,
            backup_id,
            kind,
            inspection,
            library,
            password,
            limits,
            cancel: reservation.cancel,
        },
        reservation.done,
    ))
}

/// After the job slot is reserved, check under the settings lock that the
/// archive location is still the one that was validated. The location
/// commands check for a job under the same lock before they change it, so
/// either they see this reservation and refuse, or this sees their change and
/// gives the slot back.
fn confirm_library_unchanged(
    state: &AppState,
    job_id: &str,
    validated: Option<&str>,
) -> IpcResult<()> {
    let current = match state.settings() {
        Ok(store) => store.settings().archive_directory.clone(),
        Err(e) => {
            state.jobs.release(job_id);
            return Err(e);
        }
    };
    if current.as_deref() != validated {
        state.jobs.release(job_id);
        return Err(IpcError::new(
            ErrorCode::NotConfigured,
            messages::LOCATION_CHANGED,
        ));
    }
    Ok(())
}

/// Request cancellation. False when the job is unknown or already over. The
/// job stops at its next checkpoint and reports `cancelled`.
#[tauri::command]
pub fn cancel_job(state: State<'_, AppState>, request: JobRef) -> bool {
    let cancelled = state.jobs.cancel(&request.job_id);
    if cancelled {
        tracing::info!(job_id = %request.job_id, "job cancellation requested");
    }
    cancelled
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::SettingsStore;

    fn state_with_library(tmp: &std::path::Path, library: &str) -> AppState {
        let mut store = SettingsStore::load(tmp.join("config").join("settings.json"));
        store
            .update(|s| s.archive_directory = Some(library.to_owned()))
            .expect("save");
        AppState::new(store)
    }

    #[test]
    fn a_location_change_during_inspection_releases_the_slot() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = state_with_library(tmp.path(), "/old/library");
        // open_backup captured "/old/library", then the user changed it
        // while the backup was being inspected.
        state
            .settings()
            .expect("settings")
            .update(|s| s.archive_directory = Some("/new/library".into()))
            .expect("save");
        let r = state
            .jobs
            .reserve("backup-a", JobKind::Validate)
            .expect("reserve");
        let err = confirm_library_unchanged(&state, &r.job_id, Some("/old/library"))
            .expect_err("changed");
        assert_eq!(err.code, ErrorCode::NotConfigured);
        assert!(!state.jobs.any_active(), "the slot was released");

        // Cleared rather than changed.
        state
            .settings()
            .expect("settings")
            .update(|s| s.archive_directory = None)
            .expect("save");
        let r = state
            .jobs
            .reserve("backup-a", JobKind::Validate)
            .expect("reserve");
        assert!(confirm_library_unchanged(&state, &r.job_id, Some("/new/library")).is_err());
        assert!(!state.jobs.any_active());
    }

    #[test]
    fn an_unchanged_location_keeps_the_slot() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = state_with_library(tmp.path(), "/lib");
        let r = state
            .jobs
            .reserve("backup-a", JobKind::Validate)
            .expect("reserve");
        confirm_library_unchanged(&state, &r.job_id, Some("/lib")).expect("unchanged");
        assert!(state.jobs.any_active());
    }
}
