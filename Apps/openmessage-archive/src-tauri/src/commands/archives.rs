//! The archive commands (M4 §15.2): list, create and inspect archives, and
//! start, confirm and verify. Each takes ids, booleans, a lineage number
//! and an optional label, never a path (I4); work that touches the disk
//! runs on the blocking pool. Progress and the terminal event of the two
//! jobs travel over a `Channel<JobEvent>`, as for the backup jobs.

use std::path::PathBuf;

use oma_archive::{LineageChoice, LineageId};
use tauri::ipc::Channel;
use tauri::State;

use super::jobs::{confirm_library_unchanged, prepare_job, ChannelSink};
use super::AppState;
use crate::archives::dto::{
    ArchiveDetailsDto, ArchiveListingDto, ArchiveRef, ArchiveSummaryDto, ConfirmImportRequest,
    CreateArchiveRequest, LineageInput, StartImportRequest, VerifyArchiveRequest,
};
use crate::archives::gate::GateAnswer;
use crate::archives::worker::{run_import, run_verify, ImportSpec, VerifySpec};
use crate::archives::{self, messages, parse_id, parse_label};
use crate::backups::dto::{BackupSummary, JobEvent, JobKind, JobStarted};
use crate::backups::{worker, JobSubject};
use crate::error::{ErrorCode, IpcError, IpcResult};
use crate::settings::inspect_location;

fn configured_library(state: &AppState) -> IpcResult<Option<PathBuf>> {
    Ok(state
        .settings()?
        .settings()
        .archive_directory
        .clone()
        .map(PathBuf::from))
}

pub(crate) fn require_library(state: &AppState) -> IpcResult<PathBuf> {
    configured_library(state)?
        .ok_or_else(|| IpcError::new(ErrorCode::NotConfigured, messages::NOT_CONFIGURED))
}

pub(crate) async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> IpcResult<T> + Send + 'static,
) -> IpcResult<T> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|_| IpcError::internal("archive task failed"))?
}

/// The archives in the archive folder, with this app's jobs merged in.
#[tauri::command]
pub async fn list_archives(state: State<'_, AppState>) -> IpcResult<ArchiveListingDto> {
    let library = configured_library(&state)?;
    let env = state.archive_env.clone();
    let jobs = state.jobs.clone();
    blocking(move || Ok(archives::listing(library.as_deref(), &env, &jobs))).await
}

/// Create an archive in the archive folder. It is stored unencrypted.
#[tauri::command]
pub async fn create_archive(
    state: State<'_, AppState>,
    request: CreateArchiveRequest,
) -> IpcResult<ArchiveSummaryDto> {
    let label = parse_label(request.label.as_deref())?;
    let library = require_library(&state)?;
    let env = state.archive_env.clone();
    let summary = blocking(move || archives::create(&library, &env, label)).await?;
    tracing::info!("archive created");
    Ok(summary)
}

/// The listed backups, for `matchingBackupIds`.
fn listed_backups(state: &AppState) -> IpcResult<Vec<BackupSummary>> {
    let reg = state.registry()?;
    Ok(reg
        .order
        .iter()
        .filter_map(|id| reg.backups.get(id))
        .map(|b| b.summary.clone())
        .collect())
}

/// An archive's details, read without its lock (works during an import).
#[tauri::command]
pub async fn get_archive_details(
    state: State<'_, AppState>,
    request: ArchiveRef,
) -> IpcResult<ArchiveDetailsDto> {
    let id = parse_id(&request.archive_id)?;
    let library = require_library(&state)?;
    let backups = listed_backups(&state)?;
    let env = state.archive_env.clone();
    let jobs = state.jobs.clone();
    blocking(move || archives::details(&library, &env, &id, &jobs, &backups)).await
}

/// Start importing a backup into an archive. Returns once the job has
/// started; it stops at the gate (`importPreflight`) until
/// `confirm_import`, and its outcome arrives on `on_event`.
#[tauri::command]
pub async fn start_import(
    state: State<'_, AppState>,
    request: StartImportRequest,
    on_event: Channel<JobEvent>,
) -> IpcResult<JobStarted> {
    let StartImportRequest {
        backup_id,
        archive_id,
        password,
        options,
    } = request;
    let id = parse_id(&archive_id)?;
    // A read-only look first, so an archive that cannot take an import is
    // refused before the backup is inspected or a password checked.
    let library = require_library(&state)?;
    let env = state.archive_env.clone();
    let check_id = id.clone();
    blocking(move || {
        let reader = env
            .library(&library)
            .and_then(|lib| lib.open_read(&check_id))
            .map_err(|e| messages::to_ipc(&e, &oma_backup::Limits::default()))?;
        if reader.info().is_ok_and(|i| i.import_blocked.is_some()) {
            return Err(IpcError::new(
                ErrorCode::ArchiveNewerRules,
                messages::ARCHIVE_NEWER_RULES,
            ));
        }
        Ok(())
    })
    .await?;
    let subject = JobSubject::import(&backup_id, id.as_str());
    let (job, done, gate) =
        prepare_job(&state, backup_id, password, JobKind::Import, subject).await?;
    let job_id = job.job_id.clone();
    let Some(gate) = gate else {
        state.jobs.release(&job_id);
        return Err(IpcError::internal("import job without a gate"));
    };
    let spec = ImportSpec {
        job,
        archive_id: id,
        options,
        gate,
        env: state.archive_env.clone(),
    };
    if worker::spawn_with(
        &job_id,
        state.jobs.clone(),
        Box::new(ChannelSink(on_event)),
        done,
        move |jobs, sink| run_import(spec, jobs, sink),
    )
    .is_err()
    {
        state.jobs.release(&job_id);
        return Err(IpcError::internal("job thread could not start"));
    }
    tracing::info!(job_id = %job_id, "import started");
    Ok(JobStarted { job_id })
}

/// Answer an import waiting at its gate: proceed with a lineage, or
/// decline (nothing is written). `jobNotWaiting` when it is not waiting.
#[tauri::command]
pub fn confirm_import(state: State<'_, AppState>, request: ConfirmImportRequest) -> IpcResult<()> {
    confirm(&state, request)
}

fn confirm(state: &AppState, request: ConfirmImportRequest) -> IpcResult<()> {
    let answer = match (request.proceed, request.lineage) {
        (false, _) => GateAnswer::Decline,
        (true, None) => {
            return Err(IpcError::new(
                ErrorCode::InvalidInput,
                messages::LINEAGE_REQUIRED,
            ))
        }
        (true, Some(LineageInput::New)) => GateAnswer::Proceed(LineageChoice::New),
        (true, Some(LineageInput::Existing { lineage_id })) => {
            GateAnswer::Proceed(LineageChoice::Existing(LineageId::new(lineage_id)))
        }
    };
    state.jobs.confirm(&request.job_id, answer)?;
    tracing::info!(job_id = %request.job_id, proceed = request.proceed, "import confirmed");
    Ok(())
}

/// Verify an archive (report only). Its outcome arrives on `on_event`; the
/// report is fetched with `get_archive_details`.
#[tauri::command]
pub async fn verify_archive(
    state: State<'_, AppState>,
    request: VerifyArchiveRequest,
    on_event: Channel<JobEvent>,
) -> IpcResult<JobStarted> {
    let id = parse_id(&request.archive_id)?;
    let configured = state.settings()?.settings().archive_directory.clone();
    let library = configured
        .clone()
        .map(PathBuf::from)
        .ok_or_else(|| IpcError::new(ErrorCode::NotConfigured, messages::NOT_CONFIGURED))?;
    let check = library.clone();
    let usable = blocking(move || Ok(inspect_location(&check).is_usable())).await?;
    if !usable {
        return Err(IpcError::new(
            ErrorCode::NotConfigured,
            messages::NOT_CONFIGURED,
        ));
    }
    let reservation = state
        .jobs
        .reserve(JobSubject::archive(id.as_str()), JobKind::Verify)?;
    let job_id = reservation.job_id.clone();
    confirm_library_unchanged(&state, &job_id, configured.as_deref())?;
    let spec = VerifySpec {
        job_id: job_id.clone(),
        archive_id: id,
        library,
        depth: request.depth.into(),
        cancel: reservation.cancel,
        env: state.archive_env.clone(),
    };
    if worker::spawn_with(
        &job_id,
        state.jobs.clone(),
        Box::new(ChannelSink(on_event)),
        reservation.done,
        move |jobs, sink| run_verify(spec, jobs, sink),
    )
    .is_err()
    {
        state.jobs.release(&job_id);
        return Err(IpcError::internal("job thread could not start"));
    }
    tracing::info!(job_id = %job_id, "verify started");
    Ok(JobStarted { job_id })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archives::dto::{
        DeviceEvidenceDto, FreshReasonDto, ImportPreflightDto, KeyCountDto, KeyFactsDto,
        LineageChoiceDto, LineageDto, OrderFactsDto, RdFactsDto, ResumePlanDto, SpaceEstimateDto,
    };
    use crate::archives::gate::GateEnding;
    use crate::settings::SettingsStore;

    fn state() -> (tempfile::TempDir, AppState) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = SettingsStore::load(tmp.path().join("settings.json"));
        (tmp, AppState::new(store))
    }

    pub(crate) fn preflight_with(lineages: &[i64]) -> ImportPreflightDto {
        let k = KeyCountDto {
            rows: 0,
            keyable: 0,
            duplicated_keys: 0,
            unkeyed: 0,
        };
        ImportPreflightDto {
            resume: ResumePlanDto::Fresh {
                supersedes: 0,
                reason: FreshReasonDto::NothingToContinue,
            },
            same_content_completed: false,
            device: DeviceEvidenceDto::FirstImport,
            lineages: lineages
                .iter()
                .map(|id| LineageDto {
                    lineage_id: *id,
                    sessions: 1,
                    full_sessions: 1,
                    last_backup_ms: None,
                    model_differs: None,
                    name_differs: None,
                    device_matches: false,
                })
                .collect(),
            suggested_lineage: LineageChoiceDto::New,
            keys: KeyFactsDto {
                handles: k,
                chats: k,
                messages: k,
                attachments: k,
                messages_already_archived: 0,
            },
            recently_deleted: RdFactsDto {
                messages: 0,
                excluded: 0,
                already_archived: 0,
                attachments_excluded: 0,
            },
            space: SpaceEstimateDto {
                staged_bytes: 0,
                bytes_to_copy: 0,
                files_to_copy: 0,
                largest_file: 0,
                reserve: 0,
                wal_allowance: 0,
                needed: 0,
                available: None,
                warns: false,
            },
            order: OrderFactsDto {
                dated: false,
                source: None,
                out_of_range: false,
                sorts_before_latest: false,
                source_differs: false,
            },
            encrypted_source: false,
            sync_folder_warning: false,
            windows_acl_inherited: false,
            qualifiers: Vec::new(),
            provisional: false,
            saturated: false,
        }
    }

    fn request(job_id: &str, proceed: bool, lineage: Option<LineageInput>) -> ConfirmImportRequest {
        ConfirmImportRequest {
            job_id: job_id.to_owned(),
            proceed,
            lineage,
        }
    }

    /// `confirm_import`: a lineage is required to proceed, an unknown
    /// lineage is refused, a job not at the gate is `jobNotWaiting`, and a
    /// valid answer reaches the waiting worker.
    #[test]
    fn confirm_validates_and_answers_the_gate() {
        let (_tmp, state) = state();
        let r = state
            .jobs
            .reserve(JobSubject::import("b", "a1"), JobKind::Import)
            .expect("reserve");
        let gate = r.gate.clone().expect("an import has a gate");
        let err = confirm(&state, request(&r.job_id, false, None)).expect_err("not waiting");
        assert_eq!(err.code, ErrorCode::JobNotWaiting);

        let (g, cancel) = (gate.clone(), r.cancel.clone());
        let waiter = std::thread::spawn(move || g.wait(&cancel));
        state
            .jobs
            .set_preflight(&r.job_id, Some(preflight_with(&[1, 2])));
        while !gate.is_waiting() {
            std::thread::yield_now();
        }
        let err = confirm(&state, request(&r.job_id, true, None)).expect_err("no lineage");
        assert_eq!(err.code, ErrorCode::InvalidInput);
        let err = confirm(
            &state,
            request(
                &r.job_id,
                true,
                Some(LineageInput::Existing { lineage_id: 7 }),
            ),
        )
        .expect_err("unknown lineage");
        assert_eq!(err.code, ErrorCode::InvalidInput);
        assert_eq!(
            confirm(&state, request("job-99", false, None))
                .expect_err("unknown job")
                .code,
            ErrorCode::JobNotWaiting
        );
        confirm(
            &state,
            request(
                &r.job_id,
                true,
                Some(LineageInput::Existing { lineage_id: 2 }),
            ),
        )
        .expect("answered");
        assert_eq!(
            waiter.join().expect("join"),
            GateEnding::Answered(GateAnswer::Proceed(LineageChoice::Existing(
                LineageId::new(2)
            )))
        );
        // A validation job has no gate.
        state.jobs.finish(&r.job_id, Default::default());
        let v = state.jobs.reserve("b", JobKind::Validate).expect("reserve");
        assert!(v.gate.is_none());
        assert_eq!(
            confirm(&state, request(&v.job_id, false, None))
                .expect_err("no gate")
                .code,
            ErrorCode::JobNotWaiting
        );
    }
}
