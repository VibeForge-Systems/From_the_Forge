//! The five export commands (M6 spec §9.2), each through the five IPC
//! steps. Every one takes the raw request and parses it in Rust
//! (`parse_raw`), so a malformed body is `invalidInput` with a fixed
//! sentence that echoes nothing. The work is in `crate::exports`, which
//! has no Tauri types; this layer adds the native dialogs, the blocking
//! pool, the channels and the file manager.
//!
//! - `preview_export` returns a draft id at once; the exact counts follow
//!   on its channel (`progress`, then `ready`, `needsZoneTable` or
//!   `failed`).
//! - `choose_export_destination` opens a native save dialog (one file) or
//!   folder picker (a folder export), whose title names every sensitive
//!   option of the draft, then checks the location in its canonical form.
//!   The path stays in Rust (I4).
//! - `start_export` checks the confirmations, the archive generation and
//!   the location again, reserves the job on the archive, takes its lock
//!   and starts the worker; events arrive on its channel.
//! - `discard_export_draft` drops the draft and stops its count.
//! - `show_export_in_folder` opens the folder that holds a finished
//!   export (never the export itself).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tauri::ipc::{Channel, Request};
use tauri::{AppHandle, Manager, State, WebviewWindow};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

use super::archives::{blocking, require_library};
use super::jobs::{confirm_library_unchanged, ChannelSink};
use super::viewer::json_body;
use super::AppState;
use crate::backups::dto::{JobEvent, JobKind, JobStarted};
use crate::backups::{worker as job_worker, JobSubject};
use crate::error::{ErrorCode, IpcError, IpcResult};
use crate::exports::destination::{Picked, RealProbe};
use crate::exports::dto::{
    ChooseExportDestinationRequest, DraftRef, ExportDestinationDto, ExportDraftDto, PreviewEvent,
    PreviewExportRequest, ShowExportInFolderRequest, StartExportRequest,
};
use crate::exports::messages as em;
use crate::exports::worker::{self, ExportSpec};
use crate::exports::{self};
use crate::settings::inspect_location;
use crate::viewer::input::parse_raw;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

/// Prepare an export: a draft at once, its exact counts on `on_event`.
#[tauri::command]
pub async fn preview_export(
    state: State<'_, AppState>,
    request: Request<'_>,
    on_event: Channel<PreviewEvent>,
) -> IpcResult<ExportDraftDto> {
    let req: PreviewExportRequest = parse_raw(json_body(&request))?;
    let library = require_library(&state)?;
    let work = exports::begin_preview(&state.exports, &state.viewer, &req, now_ms())?;
    let draft_id = work.draft.clone();
    let host = Arc::clone(&state.exports);
    let env = state.archive_env.clone();
    // The count runs on the blocking pool; a discard stops it.
    drop(tauri::async_runtime::spawn_blocking(move || {
        exports::run_count(&host, &env, &library, work, now_ms(), &mut |e| {
            // A closed channel only loses the event; the draft stays.
            let _ = on_event.send(e);
        });
    }));
    Ok(ExportDraftDto { draft_id })
}

/// Choose where the export goes, in a native dialog Rust opens. `null`
/// when the dialog was cancelled.
#[tauri::command]
pub async fn choose_export_destination(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
    request: Request<'_>,
) -> IpcResult<Option<ExportDestinationDto>> {
    let req: ChooseExportDestinationRequest = parse_raw(json_body(&request))?;
    let plan =
        exports::prepare_destination(&state.exports, &req.draft_id, req.zone.as_ref(), now_ms())?;
    let start = app.path().document_dir().ok().filter(|d| d.is_dir());
    let dialog_plan = plan.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || {
        let mut dialog = app
            .dialog()
            .file()
            .set_title(dialog_plan.title.as_str())
            .set_parent(&window);
        if let Some(dir) = start {
            dialog = dialog.set_directory(dir);
        }
        match dialog_plan.picked {
            Picked::File(f) => dialog
                .set_file_name(dialog_plan.default_name.as_str())
                .add_filter(f.token().to_uppercase(), &[f.extension()])
                .blocking_save_file(),
            Picked::Folder => dialog
                .set_can_create_directories(true)
                .blocking_pick_folder(),
        }
    })
    .await
    .map_err(|_| IpcError::internal("export dialog task failed"))?;
    let Some(picked) = picked else {
        return Ok(None);
    };
    let path: PathBuf = picked.into_path().map_err(|_| {
        IpcError::new(
            ErrorCode::ExportDestinationRefused,
            em::DESTINATION_NOT_A_FOLDER,
        )
    })?;
    let library = state
        .settings()?
        .settings()
        .archive_directory
        .clone()
        .map(PathBuf::from);
    let host = Arc::clone(&state.exports);
    let id = req.draft_id;
    blocking(move || {
        exports::attach_destination(&host, &id, &path, &plan, library.as_deref(), &RealProbe)
    })
    .await
    .map(Some)
}

/// Start the export of a prepared draft (which it consumes). Its events
/// arrive on `on_event`.
#[tauri::command]
pub async fn start_export(
    state: State<'_, AppState>,
    request: Request<'_>,
    on_event: Channel<JobEvent>,
) -> IpcResult<JobStarted> {
    let req: StartExportRequest = parse_raw(json_body(&request))?;
    let draft = state
        .exports
        .take_for_start(&req.draft_id, &req.destination_token)?;
    let prepared = worker::prepare(draft, &req.confirmations)?;
    let configured = state.settings()?.settings().archive_directory.clone();
    let library = require_library(&state)?;
    let env = state.archive_env.clone();
    let warnings = req.confirmations.warnings.clone();
    let check = (library.clone(), env.clone());
    // 1–3: the location, the archive and its generation, the destination.
    let prepared = blocking(move || {
        let (library, env) = check;
        if !inspect_location(&library).is_usable() {
            return Err(IpcError::new(
                ErrorCode::NotConfigured,
                crate::archives::messages::NOT_CONFIGURED,
            ));
        }
        worker::recheck(&prepared, &env, &library, &warnings, &RealProbe)?;
        Ok(prepared)
    })
    .await?;
    // 4: the job slot on the archive, and the archive's lock.
    let archive = prepared.archive.clone();
    let reservation = state
        .jobs
        .reserve(JobSubject::archive(archive.as_str()), JobKind::Export)?;
    let job_id = reservation.job_id.clone();
    confirm_library_unchanged(&state, &job_id, configured.as_deref())?;
    let (hold_env, hold_lib, hold_id) = (env.clone(), library.clone(), archive.clone());
    let hold = blocking(move || {
        hold_env
            .library(&hold_lib)
            .and_then(|l| l.hold_for_export(&hold_id))
            .map_err(|e| em::archive(&e))
    })
    .await;
    let hold = match hold {
        Ok(h) => h,
        Err(e) => {
            state.jobs.release(&job_id);
            return Err(e);
        }
    };
    // 5: the worker.
    let spec = ExportSpec {
        job_id: job_id.clone(),
        prepared,
        library,
        env,
        hold,
        cancel: reservation.cancel,
        host: Arc::clone(&state.exports),
        renderers: crate::viewer::media::helper::process_factory(),
    };
    if job_worker::spawn_with(
        &job_id,
        state.jobs.clone(),
        Box::new(ChannelSink(on_event)),
        reservation.done,
        move |jobs, sink| worker::run_export(spec, jobs, sink),
    )
    .is_err()
    {
        state.jobs.release(&job_id);
        return Err(IpcError::internal("job thread could not start"));
    }
    tracing::info!(job_id = %job_id, "export started");
    Ok(JobStarted { job_id })
}

/// Drop a draft and stop its count (an unknown draft is fine).
#[tauri::command]
pub fn discard_export_draft(state: State<'_, AppState>, request: Request<'_>) -> IpcResult<()> {
    let req: DraftRef = parse_raw(json_body(&request))?;
    if state.exports.discard(&req.draft_id) {
        tracing::info!(outcome = "discarded", "export draft");
    }
    Ok(())
}

/// Open the folder that holds a finished export, in the file manager.
#[tauri::command]
pub fn show_export_in_folder(
    app: AppHandle,
    state: State<'_, AppState>,
    request: Request<'_>,
) -> IpcResult<()> {
    let req: ShowExportInFolderRequest = parse_raw(json_body(&request))?;
    let Some(folder) = state.exports.finished_folder(&req.job_id) else {
        return Err(IpcError::new(
            ErrorCode::InvalidInput,
            em::NO_FINISHED_EXPORT,
        ));
    };
    let Some(folder) = folder.to_str().map(str::to_owned) else {
        return Err(IpcError::new(ErrorCode::Platform, em::FOLDER_NOT_OPENED));
    };
    app.opener()
        .open_path(folder, None::<&str>)
        .map_err(|_| IpcError::new(ErrorCode::Platform, em::FOLDER_NOT_OPENED))
}
