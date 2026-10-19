//! The 14 viewer commands (M5 spec §7.2). Each validates its input, names
//! its session by generation (a stale one is `viewerClosed`) and runs its
//! reads on the blocking pool; the work itself is in `crate::viewer`,
//! which has no Tauri types.
//!
//! `list_conversations` and `search_messages` carry free text, so they
//! take the raw request and parse it in Rust: a malformed body is
//! `invalidInput` with a fixed sentence, never an error that echoes it.

use std::sync::Arc;

use tauri::ipc::{Channel, InvokeBody, Request};
use tauri::State;

use super::archives::{blocking, require_library};
use super::AppState;
use crate::error::IpcResult;
use crate::settings::ViewerPreferences;
use crate::viewer::dto::{
    AttachmentInspectionDto, ConversationIndexDto, ConversationRowsDto, DayCountsDto,
    MessageDetailDto, ScanProgressDto, SearchHitsDto, SearchStartedDto, SeekResultDto,
    TextChunkDto, ThreadPageDto, ViewerSessionDto,
};
use crate::viewer::input::{
    parse_raw, CancelSearchRequest, GetConversationsRequest, GetDayCountsRequest,
    GetMessageDetailRequest, GetMessageTextRequest, GetReplyThreadRequest, GetSearchResultsRequest,
    GetThreadPageRequest, InspectAttachmentRequest, ListConversationsRequest, OpenViewerRequest,
    SearchMessagesRequest, SeekDateRequest, ViewerRef,
};
use crate::viewer::{self, with_session};

fn prefs(state: &AppState) -> IpcResult<ViewerPreferences> {
    Ok(state.settings()?.settings().viewer)
}

fn json_body<'a>(request: &'a Request<'_>) -> Option<&'a serde_json::Value> {
    match request.body() {
        InvokeBody::Json(v) => Some(v),
        InvokeBody::Raw(_) => None,
    }
}

/// Open an archive in the viewer, closing any open session first.
#[tauri::command]
pub async fn open_viewer(
    state: State<'_, AppState>,
    request: OpenViewerRequest,
) -> IpcResult<ViewerSessionDto> {
    let library = require_library(&state)?;
    let env = state.archive_env.clone();
    let jobs = state.jobs.clone();
    let host = Arc::clone(&state.viewer);
    blocking(move || viewer::open(&host, &library, &env, &jobs, &request.archive_id)).await
}

/// Close the viewer session (a stale generation is ignored).
#[tauri::command]
pub fn close_viewer(state: State<'_, AppState>, request: ViewerRef) -> IpcResult<()> {
    state.viewer.close(request.viewer)
}

/// The conversation ids in order, optionally filtered by name.
#[tauri::command]
pub async fn list_conversations(
    state: State<'_, AppState>,
    request: Request<'_>,
) -> IpcResult<ConversationIndexDto> {
    let req: ListConversationsRequest = parse_raw(json_body(&request))?;
    let host = Arc::clone(&state.viewer);
    blocking(move || with_session(&host, req.viewer, |s| viewer::list_conversations(s, &req))).await
}

/// The rows of up to 200 conversations.
#[tauri::command]
pub async fn get_conversations(
    state: State<'_, AppState>,
    request: GetConversationsRequest,
) -> IpcResult<ConversationRowsDto> {
    let previews = prefs(&state)?.list_previews;
    let host = Arc::clone(&state.viewer);
    blocking(move || {
        with_session(&host, request.viewer, |s| {
            viewer::get_conversations(s, &request, previews)
        })
    })
    .await
}

/// One page of a conversation.
#[tauri::command]
pub async fn get_thread_page(
    state: State<'_, AppState>,
    request: GetThreadPageRequest,
) -> IpcResult<ThreadPageDto> {
    let host = Arc::clone(&state.viewer);
    blocking(move || with_session(&host, request.viewer, |s| viewer::thread_page(s, &request)))
        .await
}

/// A message and its replies across conversations.
#[tauri::command]
pub async fn get_reply_thread(
    state: State<'_, AppState>,
    request: GetReplyThreadRequest,
) -> IpcResult<ThreadPageDto> {
    let host = Arc::clone(&state.viewer);
    blocking(move || with_session(&host, request.viewer, |s| viewer::reply_thread(s, &request)))
        .await
}

/// A message's details.
#[tauri::command]
pub async fn get_message_detail(
    state: State<'_, AppState>,
    request: GetMessageDetailRequest,
) -> IpcResult<MessageDetailDto> {
    let host = Arc::clone(&state.viewer);
    blocking(move || {
        with_session(&host, request.viewer, |s| {
            viewer::message_detail(s, &request)
        })
    })
    .await
}

/// A chunk of a body's text.
#[tauri::command]
pub async fn get_message_text(
    state: State<'_, AppState>,
    request: GetMessageTextRequest,
) -> IpcResult<TextChunkDto> {
    let host = Arc::clone(&state.viewer);
    blocking(move || with_session(&host, request.viewer, |s| viewer::message_text(s, &request)))
        .await
}

/// Visible items per interval of a conversation.
#[tauri::command]
pub async fn get_day_counts(
    state: State<'_, AppState>,
    request: GetDayCountsRequest,
) -> IpcResult<DayCountsDto> {
    let host = Arc::clone(&state.viewer);
    blocking(move || with_session(&host, request.viewer, |s| viewer::day_counts(s, &request))).await
}

/// The visible item nearest an instant.
#[tauri::command]
pub async fn seek_date(
    state: State<'_, AppState>,
    request: SeekDateRequest,
) -> IpcResult<SeekResultDto> {
    let host = Arc::clone(&state.viewer);
    blocking(move || with_session(&host, request.viewer, |s| viewer::seek_date(s, &request))).await
}

/// Search the archive; an exact-text scan reports progress on `on_progress`.
#[tauri::command]
pub async fn search_messages(
    state: State<'_, AppState>,
    request: Request<'_>,
    on_progress: Channel<ScanProgressDto>,
) -> IpcResult<SearchStartedDto> {
    let req: SearchMessagesRequest = parse_raw(json_body(&request))?;
    let host = Arc::clone(&state.viewer);
    blocking(move || {
        with_session(&host, req.viewer, |s| {
            viewer::search(s, &req, &mut |p| {
                // A closed channel only loses progress.
                let _ = on_progress.send(p);
            })
        })
    })
    .await
}

/// A page of the live search's results.
#[tauri::command]
pub async fn get_search_results(
    state: State<'_, AppState>,
    request: GetSearchResultsRequest,
) -> IpcResult<SearchHitsDto> {
    let host = Arc::clone(&state.viewer);
    blocking(move || {
        with_session(&host, request.viewer, |s| {
            viewer::search_results(s, &request)
        })
    })
    .await
}

/// Cancel a search: its token, then its statement.
#[tauri::command]
pub fn cancel_search(state: State<'_, AppState>, request: CancelSearchRequest) -> IpcResult<()> {
    let s = state.viewer.get(request.viewer)?;
    viewer::cancel_search(&s, request.search_id)
}

/// An attachment's facts, its sniffed content and, `forPlayback`, the
/// integrity and structural checks.
#[tauri::command]
pub async fn inspect_attachment(
    state: State<'_, AppState>,
    request: InspectAttachmentRequest,
) -> IpcResult<AttachmentInspectionDto> {
    let playback = prefs(&state)?.media_playback;
    let host = Arc::clone(&state.viewer);
    blocking(move || {
        with_session(&host, request.viewer, |s| {
            viewer::inspect(s, &request, playback)
        })
    })
    .await
}
