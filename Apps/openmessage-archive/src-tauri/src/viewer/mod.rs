//! The viewer: the shell side of milestone 5 (spec §7).
//!
//! Nothing here depends on Tauri, so the session, the operations behind
//! the 14 viewer commands, the DTOs and their guards are tested without a
//! window. `commands::viewer` adds the blocking pool, the raw-request
//! parsing and the progress `Channel`.
//!
//! - **At most one session.** [`ViewerHost`] holds it with a generation
//!   number; every request names the generation, and a stale one is
//!   `viewerClosed`. Opening closes the previous session first.
//! - **A session counts as an open archive (D114):** the archive location
//!   cannot change while one is open.
//! - **Masking and retraction are enforced in Rust (D115):** one-time
//!   codes and earlier text cross only when the request's `reveal` asks,
//!   which the UI sends only after the user's click for that message.
//! - **Logs carry kinds, buckets and elapsed time only (I6)**: never query
//!   or filter text, ids, labels, addresses, names or text, exact byte
//!   counts, or SQLite error text.

pub mod dto;
pub mod input;
pub mod media;
pub mod messages;
pub mod session;
#[cfg(test)]
pub(crate) mod tests;

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use oma_archive as oa;
use oma_archive::{ArchiveError, AttachmentId, BlobId, ChatId, MessageId};

use crate::archives::{self, ArchiveEnv};
use crate::backups::dto::JobKind;
use crate::backups::Jobs;
use crate::error::{ErrorCode, IpcError, IpcResult};
use crate::settings::MediaPlayback;
use dto::{
    AttachmentFactsDto, AttachmentInspectionDto, ContentKindDto, Conv, ConversationIndexDto,
    ConversationRowDto, ConversationRowsDto, DayCountsDto, ImageSizeDto, MediaToken,
    MessageDetailDto, MessageDto, PlaybackDto, ScanProgressDto, SearchHitDto, SearchHitsDto,
    SearchStartedDto, SeekResultDto, TextChunkDto, ThreadPageDto, ViewerSessionDto,
};
use input::{
    AnchorInput, ConversationSortInput, DirectionInput, GetConversationsRequest,
    GetDayCountsRequest, GetMessageDetailRequest, GetMessageTextRequest, GetReplyThreadRequest,
    GetSearchResultsRequest, GetThreadPageRequest, InspectAttachmentRequest,
    ListConversationsRequest, RevealInput, SearchMessagesRequest, SearchModeInput, SeekDateRequest,
    SeekDirInput, SenderFilterInput,
};
use media::engine::{MediaConfig, MediaEngine, ReaderSource};
use media::tokens::Inspected;
use messages::NotFound;
use session::{LiveSearch, ViewerSession};

/// The largest serialised page or detail (§7.3).
pub const PAGE_JSON_MAX: usize = 1 << 20;

/// The one viewer session, if any, and how sessions decode media.
#[derive(Debug, Default)]
pub struct ViewerHost {
    slot: Mutex<Option<Arc<ViewerSession>>>,
    generation: AtomicU32,
    media: MediaConfig,
    /// The page origins the media protocol accepts in an `Origin` header
    /// (set once at startup; the platform's app origin until then).
    origins: OnceLock<Vec<String>>,
}

impl ViewerHost {
    /// A host whose sessions decode through `media` (tests use a fake).
    pub fn with_media(media: MediaConfig) -> ViewerHost {
        ViewerHost {
            media,
            ..ViewerHost::default()
        }
    }

    /// Set the accepted page origins (once, at startup).
    pub fn set_origins(&self, origins: Vec<String>) {
        let _ = self.origins.set(origins);
    }

    /// `origin` is the app's own page origin.
    pub fn origin_allowed(&self, origin: &str) -> bool {
        match self.origins.get() {
            Some(list) => list.iter().any(|o| o == origin),
            None => origin == crate::window::app_origin(),
        }
    }

    /// The open session, whatever its generation (the media protocol).
    pub fn current(&self) -> Option<Arc<ViewerSession>> {
        self.slot.lock().ok()?.as_ref().map(Arc::clone)
    }

    fn lock(&self) -> IpcResult<std::sync::MutexGuard<'_, Option<Arc<ViewerSession>>>> {
        self.slot
            .lock()
            .map_err(|_| IpcError::internal("viewer lock poisoned"))
    }

    /// A session is open (D114). A poisoned lock counts as open.
    pub fn is_open(&self) -> bool {
        self.slot.lock().map(|s| s.is_some()).unwrap_or(true)
    }

    /// The next generation number (never 0).
    fn next_generation(&self) -> u32 {
        loop {
            let g = self
                .generation
                .fetch_add(1, Ordering::Relaxed)
                .wrapping_add(1);
            if g != 0 {
                return g;
            }
        }
    }

    /// The session named by `viewer`, or `viewerClosed`.
    pub fn get(&self, viewer: u32) -> IpcResult<Arc<ViewerSession>> {
        match self.lock()?.as_ref() {
            Some(s) if s.generation == viewer => Ok(Arc::clone(s)),
            _ => Err(messages::closed()),
        }
    }

    /// Close the session named by `viewer`; a stale or closed one is fine.
    pub fn close(&self, viewer: u32) -> IpcResult<()> {
        let taken = {
            let mut slot = self.lock()?;
            match slot.as_ref() {
                Some(s) if s.generation == viewer => slot.take(),
                _ => None,
            }
        };
        if let Some(s) = taken {
            s.close();
            tracing::info!(outcome = "closed", "viewer closed");
        }
        Ok(())
    }

    /// Close whatever session is open (exit, location changes, reload).
    pub fn close_any(&self) {
        let taken = self.slot.lock().ok().and_then(|mut s| s.take());
        if let Some(s) = taken {
            s.close();
            tracing::info!(outcome = "closed", "viewer closed");
        }
    }

    fn install(&self, s: ViewerSession) -> IpcResult<Arc<ViewerSession>> {
        let s = Arc::new(s);
        let old = self.lock()?.replace(Arc::clone(&s));
        if let Some(old) = old {
            old.close();
        }
        Ok(s)
    }
}

/// Run `f` on the session named by `viewer`. When the archive has gone
/// (its directory disappeared), the session is closed and the answer is
/// `viewerClosed`.
pub fn with_session<T>(
    host: &ViewerHost,
    viewer: u32,
    f: impl FnOnce(&ViewerSession) -> IpcResult<T>,
) -> IpcResult<T> {
    let s = host.get(viewer)?;
    match f(&s) {
        Err(e) if e.code == ErrorCode::ArchiveNotFound => {
            host.close(viewer)?;
            Err(messages::closed())
        }
        other => other,
    }
}

/// A count as a coarse bucket, for logs.
pub fn bucket(n: usize) -> &'static str {
    match n {
        0 => "0",
        1..=9 => "1-9",
        10..=99 => "10-99",
        100..=999 => "100-999",
        _ => "1000+",
    }
}

fn err(e: &ArchiveError, nf: NotFound) -> IpcError {
    messages::to_ipc(e, nf)
}

// ------------------------------------------------------------------- open

/// `open_viewer`: close any open session, open three readers of the
/// archive (each repeats the M4 checks: permissions, volume, header,
/// fingerprint and meta), and describe it.
pub fn open(
    host: &ViewerHost,
    library: &Path,
    env: &ArchiveEnv,
    jobs: &Jobs,
    archive_id: &str,
) -> IpcResult<ViewerSessionDto> {
    let id = archives::parse_id(archive_id)?;
    host.close_any();
    let lib = env
        .library(library)
        .map_err(|e| err(&e, NotFound::Message))?;
    let reader = || lib.open_read(&id).map_err(|e| err(&e, NotFound::Message));
    let pages = reader()?;
    let search = reader()?;
    let verify = reader()?;
    let source: ReaderSource = {
        let (library, env, id) = (library.to_path_buf(), env.clone(), id.clone());
        Arc::new(move || env.library(&library)?.open_read(&id))
    };
    let media = MediaEngine::new(&host.media, source);
    let generation = host.next_generation();
    let s = host.install(ViewerSession::new(
        generation,
        id.clone(),
        pages,
        search,
        verify,
        media,
    ))?;
    let described = describe(&s, jobs);
    match &described {
        Ok(_) => tracing::info!(outcome = "opened", "viewer opened"),
        Err(e) => {
            tracing::info!(outcome = ?e.code, "viewer not opened");
            host.close(generation)?;
        }
    }
    described
}

fn describe(s: &ViewerSession, jobs: &Jobs) -> IpcResult<ViewerSessionDto> {
    let (info, sessions, last) = {
        let r = s.pages()?;
        let e = |e: ArchiveError| err(&e, NotFound::Message);
        (
            r.info().map_err(e)?,
            r.sessions().map_err(e)?,
            r.last_import().map_err(e)?,
        )
    };
    let import_running = jobs
        .active_for_archive(s.archive.as_str())
        .is_some_and(|(j, _)| j.kind == JobKind::Import);
    let unsearchable = match s.search_reader()?.unsearchable_count() {
        Ok(n) => Some(n),
        Err(e) => {
            tracing::debug!(outcome = ?err(&e, NotFound::Message).code, "unsearchable count");
            None
        }
    };
    Ok(ViewerSessionDto::build(
        s.generation,
        import_running,
        last.as_ref().is_some_and(|l| l.ios_version_untested),
        &sessions,
        info.stats,
        unsearchable,
    ))
}

// ---------------------------------------------------------- conversations

/// `list_conversations`. An empty or blank filter means none.
pub fn list_conversations(
    s: &ViewerSession,
    req: &ListConversationsRequest,
) -> IpcResult<ConversationIndexDto> {
    let filter = match req.filter.as_deref() {
        None => None,
        Some(text) => match oa::NameFilter::new(text) {
            Ok(f) => Some(f),
            Err(oa::NameFilterError::Empty) => None,
            Err(_) => {
                return Err(IpcError::new(
                    ErrorCode::InvalidInput,
                    messages::INVALID_FILTER,
                ))
            }
        },
    };
    let sort = match req.sort {
        ConversationSortInput::Recent => oa::ConversationSort::Recent,
        ConversationSortInput::Name => oa::ConversationSort::Name,
        ConversationSortInput::Count => oa::ConversationSort::Count,
    };
    let started = Instant::now();
    let index = s
        .pages()?
        .conversation_index(sort, filter.as_ref())
        .map_err(|e| err(&e, NotFound::Message))?;
    tracing::debug!(
        ids = bucket(index.ids.len()),
        filtered = filter.is_some(),
        elapsed_ms = elapsed_ms(started),
        "conversation index"
    );
    Ok((&index).into())
}

/// `get_conversations`: at most 200 ids, in the order asked.
pub fn get_conversations(
    s: &ViewerSession,
    req: &GetConversationsRequest,
    previews: bool,
) -> IpcResult<ConversationRowsDto> {
    if req.ids.len() > oa::CONVERSATIONS_PER_CALL {
        return Err(messages::invalid_input());
    }
    let ids = req
        .ids
        .iter()
        .map(|i| i.get().map(ChatId::new))
        .collect::<IpcResult<Vec<_>>>()?;
    let rows = s
        .pages()?
        .conversations(&ids)
        .map_err(|e| err(&e, NotFound::Message))?;
    let mut cx = Conv::default();
    let rows = rows
        .iter()
        .filter_map(|c| ConversationRowDto::build(c, previews, &mut cx))
        .collect();
    Ok(ConversationRowsDto {
        rows,
        skipped_rows: cx.skipped,
    })
}

// ------------------------------------------------------------------ pages

/// Every attachment a message shows: its list and the ones placed inline.
fn attachment_ids(m: &oa::AssembledMessage) -> impl Iterator<Item = AttachmentId> + '_ {
    let inline = match &m.body {
        oa::BodyView::Text(t) => t
            .segments
            .iter()
            .filter_map(|s| match s.kind {
                oa::SegmentKind::Attachment(a) => Some(a),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    m.attachments.iter().map(|a| a.id).chain(inline)
}

fn surface_page(s: &ViewerSession, p: &oa::ThreadPage) -> IpcResult<()> {
    s.surface(p.items.iter().flat_map(attachment_ids))
}

/// Give every listed attachment with a stored file its session token
/// (§6.3: one stable token per attachment, so remounts reuse the URL).
fn mint_tokens<'a>(
    s: &ViewerSession,
    assembled: impl IntoIterator<Item = &'a oa::AssembledMessage>,
    dtos: &mut [MessageDto],
) {
    let blobs: HashMap<i64, BlobId> = assembled
        .into_iter()
        .flat_map(|m| m.attachments.iter())
        .filter_map(|a| Some((a.id.get(), a.blob?)))
        .collect();
    for a in dtos.iter_mut().flat_map(|m| m.attachments.iter_mut()) {
        if let Some(blob) = blobs.get(&a.id.get()) {
            a.token = s
                .media()
                .mint(AttachmentId::new(a.id.get()), *blob)
                .map(|t| MediaToken::of(&t));
        }
    }
}

/// Keep the serialised page within [`PAGE_JSON_MAX`]: drop the items
/// farthest from where the page was read from, moving its anchor so the
/// next page returns them.
pub fn fit_page(p: &mut ThreadPageDto, newest_side: bool) -> IpcResult<()> {
    let size = |p: &ThreadPageDto| {
        serde_json::to_vec(p)
            .map(|v| v.len())
            .map_err(|_| IpcError::internal("page serialisation"))
    };
    while size(p)? > PAGE_JSON_MAX && p.items.len() > 1 {
        p.cut_for_size = true;
        if newest_side {
            p.items.pop();
            p.has_newer = true;
            p.newer_anchor = p.items.last().map(|m| m.id);
        } else {
            p.items.remove(0);
            p.has_older = true;
            p.older_anchor = p.items.first().map(|m| m.id);
        }
    }
    if size(p)? > PAGE_JSON_MAX {
        return Err(IpcError::new(
            ErrorCode::QueryTooBroad,
            messages::QUERY_TOO_BROAD,
        ));
    }
    Ok(())
}

/// After [`fit_page`] moved an anchor onto an item, remember that item's
/// key, so the next page from it is still checked for a moved anchor.
fn remember_moved_anchors(
    s: &ViewerSession,
    chat: ChatId,
    page: &oa::ThreadPage,
    dto: &ThreadPageDto,
) -> IpcResult<()> {
    if !dto.cut_for_size {
        return Ok(());
    }
    let mut keys = s.served_keys()?;
    for anchor in [dto.older_anchor, dto.newer_anchor].into_iter().flatten() {
        let id = MessageId::new(anchor.get());
        if let Some(k) = page.key_of(id) {
            keys.remember(chat, id, k.clone());
        }
    }
    Ok(())
}

/// `get_thread_page`.
pub fn thread_page(s: &ViewerSession, req: &GetThreadPageRequest) -> IpcResult<ThreadPageDto> {
    if !(1..=oa::PAGE_ITEMS).contains(&req.limit) {
        return Err(messages::invalid_input());
    }
    let chat = ChatId::new(req.chat_id.get()?);
    let (anchor, message) = match req.anchor {
        AnchorInput::Newest => (oa::Anchor::Newest, None),
        AnchorInput::Oldest => (oa::Anchor::Oldest, None),
        AnchorInput::Undated => (oa::Anchor::Undated, None),
        AnchorInput::Message { message, include } => {
            let id = MessageId::new(message.get()?);
            (
                oa::Anchor::Message {
                    id,
                    include: include == Some(true),
                },
                Some(id),
            )
        }
    };
    let dir = match req.direction {
        DirectionInput::Older => oa::Direction::Older,
        DirectionInput::Newer => oa::Direction::Newer,
        DirectionInput::Around => oa::Direction::Around,
    };
    let hint = match message {
        Some(m) => s.served_keys()?.get(chat, m).cloned(),
        None => None,
    };
    let started = Instant::now();
    let page = s
        .pages()?
        .thread_page(
            chat,
            anchor,
            dir,
            req.limit,
            hint.as_ref(),
            RevealInput::reveal(req.reveal),
        )
        .map_err(|e| err(&e, NotFound::Anchor))?;
    {
        let mut keys = s.served_keys()?;
        for (m, k) in page.served_keys() {
            keys.remember(chat, *m, k.clone());
        }
    }
    surface_page(s, &page)?;
    let mut dto = ThreadPageDto::build(&page);
    // Trim the side farthest from where the page was read from.
    let newest_side = match anchor {
        oa::Anchor::Oldest | oa::Anchor::Undated => true,
        oa::Anchor::Message { .. } => dir == oa::Direction::Newer,
        _ => false,
    };
    mint_tokens(s, &page.items, &mut dto.items);
    fit_page(&mut dto, newest_side)?;
    remember_moved_anchors(s, chat, &page, &dto)?;
    tracing::debug!(
        items = bucket(dto.items.len()),
        partial = dto.partial,
        extras_incomplete = dto.extras_incomplete,
        cut_for_size = dto.cut_for_size,
        elapsed_ms = elapsed_ms(started),
        "thread page"
    );
    Ok(dto)
}

/// `get_reply_thread`.
pub fn reply_thread(s: &ViewerSession, req: &GetReplyThreadRequest) -> IpcResult<ThreadPageDto> {
    let id = MessageId::new(req.message_id.get()?);
    let page = s
        .pages()?
        .reply_thread(id, RevealInput::reveal(req.reveal))
        .map_err(|e| err(&e, NotFound::Message))?;
    surface_page(s, &page)?;
    let mut dto = ThreadPageDto::build(&page);
    mint_tokens(s, &page.items, &mut dto.items);
    fit_page(&mut dto, true)?;
    tracing::debug!(items = bucket(dto.items.len()), "reply thread");
    Ok(dto)
}

// ----------------------------------------------------------------- detail

/// `get_message_detail`, with a page of the reaction history when asked.
pub fn message_detail(
    s: &ViewerSession,
    req: &GetMessageDetailRequest,
) -> IpcResult<MessageDetailDto> {
    let id = MessageId::new(req.message_id.get()?);
    let reveal = RevealInput::reveal(req.reveal);
    let (detail, history) = {
        let r = s.pages()?;
        let detail = r
            .message_detail(id, reveal)
            .map_err(|e| err(&e, NotFound::Message))?;
        let history = match req.reaction_page {
            None => None,
            Some(page) => {
                // The keyset position of the previous page's last row,
                // remembered when it was served; an unknown one (never
                // served, or evicted) is refused rather than restarted.
                let after = match page.after {
                    None => None,
                    Some(last) => Some(
                        s.reaction_cursor(id, MessageId::new(last.get()?))?
                            .ok_or_else(messages::invalid_input)?,
                    ),
                };
                let h = r
                    .reaction_history(id, after.as_ref())
                    .map_err(|e| err(&e, NotFound::Message))?;
                if let Some(next) = &h.next {
                    s.remember_reaction_cursor(id, next.clone())?;
                }
                Some(h)
            }
        };
        (detail, history)
    };
    s.surface(attachment_ids(&detail.message).chain(detail.attachments.iter().map(|a| a.id)))?;
    let mut dto =
        MessageDetailDto::build(&detail, history.as_ref()).ok_or_else(messages::invalid_id)?;
    mint_tokens(s, [&detail.message], std::slice::from_mut(&mut dto.message));
    fit_detail(&mut dto)?;
    Ok(dto)
}

/// Keep a detail within [`PAGE_JSON_MAX`] by leaving out the oldest
/// non-current versions (counted in `moreVersions`).
pub fn fit_detail(d: &mut MessageDetailDto) -> IpcResult<()> {
    let size = |d: &MessageDetailDto| {
        serde_json::to_vec(d)
            .map(|v| v.len())
            .map_err(|_| IpcError::internal("detail serialisation"))
    };
    while size(d)? > PAGE_JSON_MAX {
        let Some(i) = d.versions.iter().position(|v| !v.current) else {
            return Err(IpcError::new(
                ErrorCode::QueryTooBroad,
                messages::QUERY_TOO_BROAD,
            ));
        };
        d.versions.remove(i);
        d.more_versions = d.more_versions.saturating_add(1);
        d.cut_for_size = true;
    }
    Ok(())
}

/// `get_message_text`: the offset is clamped by the crate.
pub fn message_text(s: &ViewerSession, req: &GetMessageTextRequest) -> IpcResult<TextChunkDto> {
    let body = oa::BodyId::new(req.body_id.get()?);
    let chunk = s
        .pages()?
        .message_text(body, req.from_utf16, RevealInput::reveal(req.reveal))
        .map_err(|e| err(&e, NotFound::Message))?;
    TextChunkDto::build(&chunk).ok_or_else(messages::invalid_id)
}

// ------------------------------------------------------------------ dates

/// `get_day_counts`: 2 to 43 strictly increasing instants.
pub fn day_counts(s: &ViewerSession, req: &GetDayCountsRequest) -> IpcResult<DayCountsDto> {
    let chat = ChatId::new(req.chat_id.get()?);
    let bounds = req
        .bounds_utc_ms
        .iter()
        .map(|b| b.ns())
        .collect::<IpcResult<Vec<i64>>>()?;
    if bounds.len() < 2 || bounds.len() > oa::DAY_BOUNDS || bounds.windows(2).any(|w| w[0] >= w[1])
    {
        return Err(messages::invalid_input());
    }
    let counts = s
        .pages()?
        .day_counts(chat, &bounds)
        .map_err(|e| err(&e, NotFound::Anchor))?;
    Ok(DayCountsDto {
        counts: counts.counts,
    })
}

/// `seek_date`: a visible item, or none.
pub fn seek_date(s: &ViewerSession, req: &SeekDateRequest) -> IpcResult<SeekResultDto> {
    let chat = ChatId::new(req.chat_id.get()?);
    let ns = req.utc_ms.ns()?;
    let dir = match req.dir {
        SeekDirInput::OnOrAfter => oa::SeekDir::OnOrAfter,
        SeekDirInput::Before => oa::SeekDir::Before,
    };
    let found = s
        .pages()?
        .seek_date(chat, ns, dir)
        .map_err(|e| err(&e, NotFound::Anchor))?;
    let mut cx = Conv::default();
    Ok(SeekResultDto {
        message_id: found.and_then(|m| cx.id(m.get())),
    })
}

// ----------------------------------------------------------------- search

fn search_request(req: &SearchMessagesRequest) -> IpcResult<oa::SearchRequest> {
    let mut q = oa::SearchRequest::new(&req.query);
    q.mode = match req.mode {
        SearchModeInput::Auto => oa::SearchMode::Auto,
        SearchModeInput::Exact => oa::SearchMode::Exact,
    };
    let f = &req.filters;
    let mut filters = oa::SearchFilters::default();
    filters.chat = match f.chat_id {
        Some(c) => Some(ChatId::new(c.get()?)),
        None => None,
    };
    filters.from_utc_ns = f.from_utc_ms.map(|m| m.ns()).transpose()?;
    filters.until_utc_ns = f.until_utc_ms.map(|m| m.ns()).transpose()?;
    if let (Some(a), Some(b)) = (filters.from_utc_ns, filters.until_utc_ns) {
        if a >= b {
            return Err(messages::invalid_input());
        }
    }
    filters.has_attachment = f.has_attachment == Some(true);
    filters.sender = match f.sender {
        None | Some(SenderFilterInput::Anyone) => oa::SenderFilter::Anyone,
        Some(SenderFilterInput::Me) => oa::SenderFilter::Me,
        Some(SenderFilterInput::Others) => oa::SenderFilter::Others,
    };
    q.filters = filters;
    q.include_earlier = req.include_earlier;
    q.include_events = req.include_events;
    Ok(q)
}

/// `search_messages`. The previous search's token is set and its
/// statement interrupted **before** this one waits for the search reader
/// (§7.1). A superseded search ends in `searchSuperseded`.
pub fn search(
    s: &ViewerSession,
    req: &SearchMessagesRequest,
    progress: &mut dyn FnMut(ScanProgressDto),
) -> IpcResult<SearchStartedDto> {
    let q = search_request(req)?;
    let ran = q.resolved_mode().map_err(|e| err(&e, NotFound::Message))?;
    let token = oa::CancelToken::new();
    let id = {
        let mut slot = s.searches()?;
        if let Some((_, old)) = slot.running.take() {
            old.cancel();
        }
        slot.next_id = slot.next_id.wrapping_add(1).max(1);
        let id = slot.next_id;
        slot.running = Some((id, token.clone()));
        // Interrupt while the slot is held: a newer search can start a
        // statement only after it takes this lock, so this interrupt can
        // never land on it (with no statement running, it is a no-op).
        s.search_interrupt.interrupt();
        id
    };
    let started = Instant::now();
    let result = {
        let r = s.search_reader()?;
        r.search(&q, &token, &mut |p| progress(ScanProgressDto::new(id, p)))
    };
    let mut slot = s.searches()?;
    if slot.running.as_ref().is_some_and(|(r, _)| *r == id) {
        slot.running = None;
    }
    if token.is_cancelled() {
        return Err(messages::superseded());
    }
    let set = match result {
        Ok(set) => set,
        Err(ArchiveError::Cancelled) => return Err(messages::superseded()),
        Err(e) => {
            let e = err(&e, NotFound::Message);
            tracing::info!(outcome = ?e.code, elapsed_ms = elapsed_ms(started), "search");
            return Err(e);
        }
    };
    let dto = SearchStartedDto::build(id, &set);
    tracing::info!(
        mode = ?ran,
        plan = ?set.plan,
        matches = bucket(set.entries.len()),
        capped = set.capped,
        elapsed_ms = elapsed_ms(started),
        "search"
    );
    slot.current = Some(LiveSearch {
        id,
        request: q,
        set: Arc::new(set),
    });
    Ok(dto)
}

/// `get_search_results`: a page of the live set (at most 50 hits).
pub fn search_results(
    s: &ViewerSession,
    req: &GetSearchResultsRequest,
) -> IpcResult<SearchHitsDto> {
    let limit = usize::try_from(req.limit).unwrap_or(usize::MAX);
    if !(1..=oa::HITS_PER_CALL).contains(&limit) {
        return Err(messages::invalid_input());
    }
    let (request, set) = {
        let slot = s.searches()?;
        match slot.current.as_ref() {
            Some(live) if live.id == req.search_id => (live.request.clone(), Arc::clone(&live.set)),
            _ => return Err(messages::superseded()),
        }
    };
    let start = usize::try_from(req.offset).unwrap_or(usize::MAX);
    let hits = s
        .search_reader()?
        .search_hits(&set, start..start.saturating_add(limit), &request)
        .map_err(|e| match e {
            ArchiveError::Cancelled => messages::superseded(),
            other => err(&other, NotFound::Message),
        })?;
    let mut cx = Conv::default();
    let hits: Vec<SearchHitDto> = hits
        .iter()
        .filter_map(|h| SearchHitDto::build(h, &mut cx))
        .collect();
    tracing::debug!(hits = bucket(hits.len()), "search results");
    Ok(SearchHitsDto {
        hits,
        skipped_rows: cx.skipped,
    })
}

/// `cancel_search`: stop the running search with that id (its token, then
/// its statement) and forget its set.
pub fn cancel_search(s: &ViewerSession, search_id: u32) -> IpcResult<()> {
    let mut slot = s.searches()?;
    if slot.current.as_ref().is_some_and(|l| l.id == search_id) {
        slot.current = None;
    }
    match slot.running.take() {
        Some((id, token)) if id == search_id => {
            token.cancel();
            // Under the slot's lock, as in `search`: a later search's
            // statement cannot have started yet.
            s.search_interrupt.interrupt();
        }
        other => slot.running = other,
    }
    Ok(())
}

// ------------------------------------------------------------ attachments

/// `inspect_attachment`: only for attachments surfaced in this session.
/// Sniffs the stored file and records its kind for the attachment's
/// media token (no rendition is served before that); `forPlayback` then
/// hashes it in full (integrity only, once per session), checks its
/// structure and, when both pass, enables `stream` for it. Playback set
/// to `off` is refused here, in Rust, and by the protocol.
pub fn inspect(
    s: &ViewerSession,
    req: &InspectAttachmentRequest,
    playback: MediaPlayback,
) -> IpcResult<AttachmentInspectionDto> {
    let id = AttachmentId::new(req.attachment_id.get()?);
    if !s.is_surfaced(id)? {
        return Err(messages::attachment_unavailable());
    }
    let (facts, sniffed) = {
        let r = s.pages()?;
        let facts = r.attachment(id).map_err(|e| match e {
            ArchiveError::AnchorNotFound => messages::attachment_unavailable(),
            other => err(&other, NotFound::Message),
        })?;
        let sniffed = facts.blob.map(|b| (b.id, r.inspect_blob(b.id)));
        (facts, sniffed)
    };
    let mut cx = Conv::default();
    let facts_dto =
        AttachmentFactsDto::build(&facts, &mut cx).ok_or_else(messages::attachment_unavailable)?;
    let (content, kind) = match &sniffed {
        None => (ContentKindDto::NoFile, None),
        Some((_, Ok(sn))) => (ContentKindDto::sniffed(sn), Some(*sn)),
        Some((_, Err(ArchiveError::Blob(p)))) => (
            ContentKindDto::Unavailable {
                problem: dto::file_problem(*p),
            },
            None,
        ),
        Some((_, Err(e))) => return Err(err(e, NotFound::Message)),
    };
    // The token, and what its file is: until this is recorded the
    // protocol serves no rendition for it.
    let token = match (&sniffed, kind) {
        (Some((blob, _)), Some(sn)) => {
            let t = s.media().mint(id, *blob);
            s.media().set_inspected(
                id,
                Inspected {
                    blob: *blob,
                    kind: sn.kind,
                },
            );
            t.map(|t| MediaToken::of(&t))
        }
        _ => None,
    };
    let original_size = sniffed
        .as_ref()
        .and_then(|(b, _)| s.media().source_size(*b))
        .map(|(width, height)| ImageSizeDto { width, height });
    let playback = if !req.for_playback {
        None
    } else if playback == MediaPlayback::Off {
        Some(PlaybackDto::Off)
    } else {
        match (sniffed.as_ref().map(|(b, _)| *b), kind) {
            (Some(blob), Some(sn)) if matches!(sn.class, oa::MediaClass::Playable { .. }) => {
                Some(check_playback(s, blob, sn.kind)?)
            }
            _ => Some(PlaybackDto::NotPlayable),
        }
    };
    tracing::debug!(
        for_playback = req.for_playback,
        playback_checked = playback.is_some(),
        "attachment inspected"
    );
    Ok(AttachmentInspectionDto {
        facts: facts_dto,
        content,
        playback,
        token,
        original_size,
        previews_off: s.media().previews_off(),
    })
}

fn check_playback(
    s: &ViewerSession,
    blob: oa::BlobId,
    kind: oa::SniffedKind,
) -> IpcResult<PlaybackDto> {
    let _slot = s.verify_slot()?;
    let r = s.verify_reader()?;
    let ident = match s.verified(blob)? {
        Some(i) => i,
        None => match r.verify_blob(blob, &s.closing) {
            // The full hash is checked once per session (integrity only).
            Ok(i) => {
                s.set_verified(i)?;
                i
            }
            Err(ArchiveError::Blob(p)) => {
                return Ok(PlaybackDto::Unavailable {
                    problem: dto::file_problem(p),
                })
            }
            Err(e) => return Err(err(&e, NotFound::Message)),
        },
    };
    let verdict = match r.check_container(blob, &ident, kind, &s.closing) {
        Ok(v) => PlaybackDto::verdict(v),
        Err(ArchiveError::Blob(p)) => PlaybackDto::Unavailable {
            problem: dto::file_problem(p),
        },
        Err(e) => return Err(err(&e, NotFound::Message)),
    };
    if matches!(verdict, PlaybackDto::Allowed { .. }) {
        // Only now does the protocol serve `stream` for it.
        s.set_playable(blob)?;
    }
    Ok(verdict)
}

fn elapsed_ms(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}
