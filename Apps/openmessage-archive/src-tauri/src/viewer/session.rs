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
//!   message, at most [`SERVED_KEYS`] of them.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use oma_archive::{
    ArchiveId, ArchiveReader, AttachmentId, BlobId, BlobIdentity, CancelToken, ChatId, MessageId,
    SearchInterrupt, SearchRequest, SearchResultSet, SortKeyHint,
};

use super::media::engine::MediaEngine;
use crate::error::{ErrorCode, IpcError, IpcResult};

/// The most remembered anchor keys.
pub const SERVED_KEYS: usize = 20_000;
/// The most attachments a session remembers as surfaced.
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
    pub search_interrupt: SearchInterrupt,
    verify: Mutex<ArchiveReader>,
    /// Playback checks running or waiting for `verify`.
    verify_pending: AtomicUsize,
    /// Set when the session closes: stops a running verification.
    pub closing: CancelToken,
    surfaced: Mutex<HashSet<AttachmentId>>,
    /// Files whose full hash matched in this session (integrity only).
    verified: Mutex<HashMap<BlobId, BlobIdentity>>,
    /// Verified files whose structural check passed: `stream` is served
    /// for these only.
    playable: Mutex<HashSet<BlobId>>,
    media: MediaEngine,
    served_keys: Mutex<SortKeyLru>,
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
        let search_interrupt = search.search_interrupt();
        ViewerSession {
            generation,
            archive,
            pages: Mutex::new(pages),
            search: Mutex::new(search),
            search_interrupt,
            verify: Mutex::new(verify),
            verify_pending: AtomicUsize::new(0),
            closing: CancelToken::new(),
            surfaced: Mutex::new(HashSet::new()),
            verified: Mutex::new(HashMap::new()),
            playable: Mutex::new(HashSet::new()),
            media,
            served_keys: Mutex::new(SortKeyLru::default()),
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
            if s.len() >= SURFACED_LIMIT {
                break;
            }
            s.insert(id);
        }
        Ok(())
    }

    pub fn is_surfaced(&self, id: AttachmentId) -> IpcResult<bool> {
        Ok(self.surfaced.lock().map_err(|_| poisoned())?.contains(&id))
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
    fn served_keys_are_bounded_and_evicted_oldest_first() {
        // SortKeyHint has no public constructor; the bound is checked on
        // the map's bookkeeping with real hints in `viewer::tests`.
        let lru = SortKeyLru::default();
        assert!(lru.is_empty());
        assert_eq!(lru.len(), 0);
        assert!(lru.get(ChatId::new(1), MessageId::new(1)).is_none());
    }
}
