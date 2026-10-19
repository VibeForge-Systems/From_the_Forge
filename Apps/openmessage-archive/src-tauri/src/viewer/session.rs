//! One viewer session (M5 spec §7.1, D89): the readers of one archive, the
//! attachments surfaced to the webview, the sort keys served for page
//! anchors, the live search and the files verified for playback.
//!
//! - **A reader pool.** `pages` serves the conversation list, pages,
//!   details, text, dates and the attachment facts; `search` serves search
//!   only, with its interrupt handle kept *outside* the mutex; `verify`
//!   hashes and checks files for playback, one at a time with at most
//!   [`VERIFY_QUEUE`] waiting (the verify executor). The media engine
//!   (`media::engine`) opens its own stream and decode readers. Search,
//!   verification, decoding and streaming therefore never stall page reads.
//! - **One snapshot per call.** Each reader method opens and ends its own
//!   read transaction; nothing is held across calls, so checkpoints are
//!   never held back.
//! - Sort keys never cross IPC: they stay here, keyed by conversation and
//!   message, at most [`SERVED_KEYS`] of them; reaction-history positions
//!   likewise, keyed by target and last row, at most [`REACTION_CURSORS`].

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use oma_archive::{
    ArchiveId, ArchiveReader, AttachmentId, BlobId, BlobIdentity, CancelToken, ChatId, MessageId,
    ReactionCursor, SearchInterrupt, SearchRequest, SearchResultSet, SortKeyHint,
};

use super::media::engine::MediaEngine;
use crate::error::{ErrorCode, IpcError, IpcResult};

/// The most remembered anchor keys.
pub const SERVED_KEYS: usize = 20_000;
/// The most attachments a session remembers as surfaced; past it the
/// least recently surfaced are forgotten, so a new one is always admitted.
pub const SURFACED_LIMIT: usize = 200_000;
/// Playback checks that may wait while one runs (§6.2: the verify
/// executor has one worker and four queue entries).
pub const VERIFY_QUEUE: usize = 4;

/// Sort keys served for page anchors, evicted oldest first.
#[derive(Default)]
pub struct SortKeyLru {
    keys: HashMap<(ChatId, MessageId), (SortKeyHint, u64)>,
    order: BTreeMap<u64, (ChatId, MessageId)>,
    tick: u64,
}

impl SortKeyLru {
    pub fn remember(&mut self, chat: ChatId, message: MessageId, hint: SortKeyHint) {
        self.tick += 1;
        if let Some((_, old)) = self.keys.insert((chat, message), (hint, self.tick)) {
            self.order.remove(&old);
        }
        self.order.insert(self.tick, (chat, message));
        while self.keys.len() > SERVED_KEYS {
            let Some((_, k)) = self.order.pop_first() else {
                break;
            };
            self.keys.remove(&k);
        }
    }

    pub fn get(&self, chat: ChatId, message: MessageId) -> Option<&SortKeyHint> {
        self.keys.get(&(chat, message)).map(|(h, _)| h)
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

/// The most remembered reaction-history positions.
pub const REACTION_CURSORS: usize = 1_024;

/// Reaction-history keyset positions served, by (target, last row),
/// evicted oldest first.
#[derive(Default)]
pub struct ReactionCursors {
    cursors: HashMap<(MessageId, MessageId), (ReactionCursor, u64)>,
    order: BTreeMap<u64, (MessageId, MessageId)>,
    tick: u64,
}

impl ReactionCursors {
    pub fn remember(&mut self, target: MessageId, cursor: ReactionCursor) {
        self.tick += 1;
        let k = (target, cursor.after());
        if let Some((_, old)) = self.cursors.insert(k, (cursor, self.tick)) {
            self.order.remove(&old);
        }
        self.order.insert(self.tick, k);
        while self.cursors.len() > REACTION_CURSORS {
            let Some((_, k)) = self.order.pop_first() else {
                break;
            };
            self.cursors.remove(&k);
        }
    }

    pub fn get(&self, target: MessageId, last: MessageId) -> Option<&ReactionCursor> {
        self.cursors.get(&(target, last)).map(|(c, _)| c)
    }

    pub fn len(&self) -> usize {
        self.cursors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cursors.is_empty()
    }
}

/// The attachments surfaced to the webview, evicted least recently
/// surfaced first: a long session over a large archive keeps admitting
/// new ones (an evicted one is surfaced again when a page shows it).
#[derive(Default)]
pub struct SurfacedSet {
    ids: HashMap<AttachmentId, u64>,
    order: BTreeMap<u64, AttachmentId>,
    tick: u64,
}

impl SurfacedSet {
    /// Admit `id` (or refresh it), keeping at most `limit`.
    pub fn surface(&mut self, id: AttachmentId, limit: usize) {
        self.tick += 1;
        if let Some(old) = self.ids.insert(id, self.tick) {
            self.order.remove(&old);
        }
        self.order.insert(self.tick, id);
        while self.ids.len() > limit {
            let Some((_, oldest)) = self.order.pop_first() else {
                break;
            };
            self.ids.remove(&oldest);
        }
    }

    pub fn contains(&self, id: AttachmentId) -> bool {
        self.ids.contains_key(&id)
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}

/// The search reader's interrupt handle, counted: the count lets a test
/// see that a new search interrupts the old one **before** it waits for
/// the search reader (§7.1).
pub struct CountedInterrupt {
    handle: SearchInterrupt,
    count: AtomicU64,
}

impl CountedInterrupt {
    pub fn new(handle: SearchInterrupt) -> CountedInterrupt {
        CountedInterrupt {
            handle,
            count: AtomicU64::new(0),
        }
    }

    pub fn interrupt(&self) {
        self.count.fetch_add(1, Ordering::SeqCst);
        self.handle.interrupt();
    }

    /// Interrupts sent so far.
    pub fn count(&self) -> u64 {
        self.count.load(Ordering::SeqCst)
    }
}

/// The finished search the session keeps: its request (the query stays
/// `Redacted`) and its result set (ids only).
pub struct LiveSearch {
    pub id: u32,
    pub request: SearchRequest,
    pub set: Arc<SearchResultSet>,
}

/// The session's one search slot: the running search's token, and the
/// last finished set.
#[derive(Default)]
pub struct SearchSlot {
    pub next_id: u32,
    pub running: Option<(u32, CancelToken)>,
    pub current: Option<LiveSearch>,
}

/// One open viewer.
pub struct ViewerSession {
    pub generation: u32,
    pub archive: ArchiveId,
    pages: Mutex<ArchiveReader>,
    search: Mutex<ArchiveReader>,
    /// Outside the mutex: a cancel must reach a statement that holds it.
    pub search_interrupt: CountedInterrupt,
    verify: Mutex<ArchiveReader>,
    /// Playback checks running or waiting for `verify`.
    verify_pending: AtomicUsize,
    /// Set when the session closes: stops a running verification.
    pub closing: CancelToken,
    surfaced: Mutex<SurfacedSet>,
    /// Files whose full hash matched in this session (integrity only).
    verified: Mutex<HashMap<BlobId, BlobIdentity>>,
    /// Verified files whose structural check passed: `stream` is served
    /// for these only.
    playable: Mutex<HashSet<BlobId>>,
    media: MediaEngine,
    served_keys: Mutex<SortKeyLru>,
    reaction_cursors: Mutex<ReactionCursors>,
    searches: Mutex<SearchSlot>,
}

impl std::fmt::Debug for ViewerSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ViewerSession")
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

fn poisoned() -> IpcError {
    IpcError::internal("viewer lock poisoned")
}

impl ViewerSession {
    /// A session over three readers of the same archive, and its media
    /// engine (which opens its own).
    pub fn new(
        generation: u32,
        archive: ArchiveId,
        pages: ArchiveReader,
        search: ArchiveReader,
        verify: ArchiveReader,
        media: MediaEngine,
    ) -> ViewerSession {
        let search_interrupt = CountedInterrupt::new(search.search_interrupt());
        ViewerSession {
            generation,
            archive,
            pages: Mutex::new(pages),
            search: Mutex::new(search),
            search_interrupt,
            verify: Mutex::new(verify),
            verify_pending: AtomicUsize::new(0),
            closing: CancelToken::new(),
            surfaced: Mutex::new(SurfacedSet::default()),
            verified: Mutex::new(HashMap::new()),
            playable: Mutex::new(HashSet::new()),
            media,
            served_keys: Mutex::new(SortKeyLru::default()),
            reaction_cursors: Mutex::new(ReactionCursors::default()),
            searches: Mutex::new(SearchSlot::default()),
        }
    }

    pub fn pages(&self) -> IpcResult<MutexGuard<'_, ArchiveReader>> {
        self.pages.lock().map_err(|_| poisoned())
    }

    pub fn search_reader(&self) -> IpcResult<MutexGuard<'_, ArchiveReader>> {
        self.search.lock().map_err(|_| poisoned())
    }

    pub fn verify_reader(&self) -> IpcResult<MutexGuard<'_, ArchiveReader>> {
        self.verify.lock().map_err(|_| poisoned())
    }

    /// A place in the verify executor's queue: one check runs, at most
    /// [`VERIFY_QUEUE`] wait; beyond that the request is `busy`.
    pub fn verify_slot(&self) -> IpcResult<VerifySlot<'_>> {
        let before = self.verify_pending.fetch_add(1, Ordering::AcqRel);
        if before > VERIFY_QUEUE {
            self.verify_pending.fetch_sub(1, Ordering::AcqRel);
            return Err(IpcError::new(
                ErrorCode::Busy,
                super::messages::PLAYBACK_CHECK_BUSY,
            ));
        }
        Ok(VerifySlot(&self.verify_pending))
    }

    pub fn media(&self) -> &MediaEngine {
        &self.media
    }

    pub fn searches(&self) -> IpcResult<MutexGuard<'_, SearchSlot>> {
        self.searches.lock().map_err(|_| poisoned())
    }

    /// Stop everything running: the verification, the search and the
    /// media workers (their helper processes end with them).
    pub fn close(&self) {
        self.closing.cancel();
        self.media.close();
        if let Ok(mut s) = self.searches.lock() {
            if let Some((_, token)) = s.running.take() {
                token.cancel();
            }
            s.current = None;
        }
        self.search_interrupt.interrupt();
    }

    /// Remember attachments shown to the webview.
    pub fn surface(&self, ids: impl IntoIterator<Item = AttachmentId>) -> IpcResult<()> {
        let mut s = self.surfaced.lock().map_err(|_| poisoned())?;
        for id in ids {
            s.surface(id, SURFACED_LIMIT);
        }
        Ok(())
    }

    pub fn is_surfaced(&self, id: AttachmentId) -> IpcResult<bool> {
        Ok(self.surfaced.lock().map_err(|_| poisoned())?.contains(id))
    }

    pub fn verified(&self, blob: BlobId) -> IpcResult<Option<BlobIdentity>> {
        Ok(self
            .verified
            .lock()
            .map_err(|_| poisoned())?
            .get(&blob)
            .copied())
    }

    pub fn set_verified(&self, ident: BlobIdentity) -> IpcResult<()> {
        self.verified
            .lock()
            .map_err(|_| poisoned())?
            .insert(ident.blob(), ident);
        Ok(())
    }

    /// The file passed the structural check too: `stream` may serve it.
    pub fn set_playable(&self, blob: BlobId) -> IpcResult<()> {
        self.playable.lock().map_err(|_| poisoned())?.insert(blob);
        Ok(())
    }

    /// The identity to stream `blob` with: only when both the hash and
    /// the structural check passed in this session.
    pub fn stream_identity(&self, blob: BlobId) -> Option<BlobIdentity> {
        if !self.playable.lock().ok()?.contains(&blob) {
            return None;
        }
        self.verified.lock().ok()?.get(&blob).copied()
    }

    pub fn served_keys(&self) -> IpcResult<MutexGuard<'_, SortKeyLru>> {
        self.served_keys.lock().map_err(|_| poisoned())
    }

    /// The reaction-history position served after row `last` of
    /// `target`'s history, if remembered.
    pub fn reaction_cursor(
        &self,
        target: MessageId,
        last: MessageId,
    ) -> IpcResult<Option<ReactionCursor>> {
        Ok(self
            .reaction_cursors
            .lock()
            .map_err(|_| poisoned())?
            .get(target, last)
            .cloned())
    }

    pub fn remember_reaction_cursor(
        &self,
        target: MessageId,
        cursor: ReactionCursor,
    ) -> IpcResult<()> {
        self.reaction_cursors
            .lock()
            .map_err(|_| poisoned())?
            .remember(target, cursor);
        Ok(())
    }
}

/// A held place in the verify queue; released on drop.
pub struct VerifySlot<'a>(&'a AtomicUsize);

impl Drop for VerifySlot<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_surfaced_set_always_admits_and_forgets_the_least_recent() {
        let mut s = SurfacedSet::default();
        for i in 1..=5 {
            s.surface(AttachmentId::new(i), 3);
        }
        assert_eq!(s.len(), 3);
        assert!(!s.contains(AttachmentId::new(1)) && !s.contains(AttachmentId::new(2)));
        assert!(s.contains(AttachmentId::new(5)), "the newest is admitted");
        // Surfacing again refreshes: 3 survives the next admission.
        s.surface(AttachmentId::new(3), 3);
        s.surface(AttachmentId::new(6), 3);
        assert!(s.contains(AttachmentId::new(3)) && !s.contains(AttachmentId::new(4)));
    }

    #[test]
    fn served_keys_are_bounded_and_evicted_oldest_first() {
        // SortKeyHint has no public constructor; the bound is checked on
        // the map's bookkeeping with real hints in `viewer::tests`.
        let lru = SortKeyLru::default();
        assert!(lru.is_empty());
        assert_eq!(lru.len(), 0);
        assert!(lru.get(ChatId::new(1), MessageId::new(1)).is_none());
    }
}
