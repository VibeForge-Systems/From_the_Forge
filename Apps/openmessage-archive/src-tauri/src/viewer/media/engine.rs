//! A viewer session's media side (M5 spec §6.2, §5.5, §5.8): the token
//! table, the rendition and failure caches, and two executors of its own.
//!
//! | Executor | Workers | Queue | Work |
//! |---|---|---|---|
//! | decode | [`DECODE_WORKERS`], each with its own reader and helper | [`DECODE_QUEUE`], newest first; a full queue drops its oldest entry with `503` | `thumb`, `display` |
//! | stream | [`STREAM_WORKERS`], each with its own reader | [`STREAM_QUEUE`], FIFO; full is `503` | `stream` chunks |
//!
//! The third executor, verify, is the session's `verify` reader behind
//! its own gate (`ViewerSession::verify_slot`): one worker, four waiting.
//! So a slow decode or a multi-GB verify cannot stall playback or
//! thumbnails. Readers are opened on a worker's first job and helpers are
//! started on a decode worker's first miss; both end with the session.
//!
//! **Images are decoded only from the buffer `read_blob_verified` read to
//! the end through the hashing reader**, and only in a helper process. A
//! failure of any kind is remembered per `(blob, rendition)`, so a hostile
//! image is decoded at most once per session. After [`CRASH_CAP`] helper
//! crashes in a session, previews are off for the rest of it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use oma_archive::media::MAX_INPUT_BYTES;
use oma_archive::{
    ArchiveError, ArchiveReader, AttachmentId, BlobId, BlobIdentity, BlobProblem, RenderLimits,
    Rendition,
};

use super::cache::{CachedImage, RenditionCache, CACHE_BYTES};
use super::exec::{Executor, Order, Pushed};
use super::helper::{BackendFactory, RenderBackend, RenderOutcome};
use super::tokens::{Inspected, TokenBytes, TokenEntry, TokenTable, TOKEN_LIMIT};
use super::{MediaResponse, Outcome, Reply};

pub const DECODE_WORKERS: usize = 2;
pub const DECODE_QUEUE: usize = 64;
pub const STREAM_WORKERS: usize = 2;
pub const STREAM_QUEUE: usize = 16;
/// Helper crashes after which a session's previews are off.
pub const CRASH_CAP: u32 = 5;

/// Opens another reader of the session's archive (each repeats the M4
/// open checks).
pub type ReaderSource = Arc<dyn Fn() -> Result<ArchiveReader, ArchiveError> + Send + Sync>;

/// How sessions decode: the helper processes in the app, a fake in tests.
#[derive(Clone)]
pub struct MediaConfig {
    pub backend: BackendFactory,
    pub crash_cap: u32,
}

impl std::fmt::Debug for MediaConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaConfig")
            .field("crash_cap", &self.crash_cap)
            .finish_non_exhaustive()
    }
}

impl Default for MediaConfig {
    fn default() -> MediaConfig {
        MediaConfig {
            backend: super::helper::process_factory(),
            crash_cap: CRASH_CAP,
        }
    }
}

/// A rendition to make.
pub struct DecodeJob {
    pub blob: BlobId,
    pub rendition: Rendition,
    pub reply: Reply,
}

/// A stream chunk to read: bytes `start..=end`, answered `206`, or the
/// whole (small) file, answered `200`.
pub struct StreamJob {
    pub blob: BlobId,
    pub ident: BlobIdentity,
    pub mime: &'static str,
    pub start: u64,
    pub end: u64,
    pub whole: bool,
    pub reply: Reply,
}

#[derive(Default)]
struct Shared {
    cache: Mutex<RenditionCache>,
    sizes: Mutex<HashMap<BlobId, (u32, u32)>>,
    crashes: AtomicU32,
    previews_off: AtomicBool,
}

impl Shared {
    fn cached(&self, key: (BlobId, Rendition)) -> Option<CachedImage> {
        self.cache.lock().ok()?.get(key)
    }

    fn failed(&self, key: (BlobId, Rendition)) -> bool {
        self.cache.lock().map(|c| c.failed(key)).unwrap_or(true)
    }

    fn fail(&self, key: (BlobId, Rendition)) {
        if let Ok(mut c) = self.cache.lock() {
            c.fail(key);
        }
    }
}

/// The media side of one viewer session.
pub struct MediaEngine {
    tokens: Mutex<TokenTable>,
    shared: Arc<Shared>,
    decode: Executor<DecodeJob>,
    stream: Executor<StreamJob>,
}

impl std::fmt::Debug for MediaEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaEngine").finish_non_exhaustive()
    }
}

fn reader<'a>(
    slot: &'a mut Option<ArchiveReader>,
    source: &ReaderSource,
) -> Option<&'a ArchiveReader> {
    if slot.is_none() {
        *slot = source().ok();
    }
    slot.as_ref()
}

impl MediaEngine {
    pub fn new(config: &MediaConfig, source: ReaderSource) -> MediaEngine {
        let shared = Arc::new(Shared {
            cache: Mutex::new(RenditionCache::new(CACHE_BYTES)),
            ..Shared::default()
        });
        let cap = config.crash_cap.max(1);
        let decoders = (0..DECODE_WORKERS)
            .map(|_| {
                let source = Arc::clone(&source);
                let factory = Arc::clone(&config.backend);
                let shared = Arc::clone(&shared);
                let mut rd: Option<ArchiveReader> = None;
                let mut backend: Option<Box<dyn RenderBackend>> = None;
                move |job: DecodeJob| {
                    decode_one(job, &shared, &source, &mut rd, &mut backend, &factory, cap);
                }
            })
            .collect();
        let streamers = (0..STREAM_WORKERS)
            .map(|_| {
                let source = Arc::clone(&source);
                let mut rd: Option<ArchiveReader> = None;
                move |job: StreamJob| stream_one(job, &source, &mut rd)
            })
            .collect();
        MediaEngine {
            tokens: Mutex::new(TokenTable::new(TOKEN_LIMIT)),
            shared,
            decode: Executor::start(
                "oma-media-decode",
                DECODE_QUEUE,
                Order::NewestFirst,
                decoders,
            ),
            stream: Executor::start("oma-media-stream", STREAM_QUEUE, Order::Fifo, streamers),
        }
    }

    /// The attachment's stable token (minted on first use).
    pub fn mint(&self, attachment: AttachmentId, blob: BlobId) -> Option<TokenBytes> {
        self.tokens.lock().ok()?.mint(attachment, blob)
    }

    pub fn lookup(&self, token: TokenBytes) -> Option<TokenEntry> {
        self.tokens.lock().ok()?.lookup(token)
    }

    pub fn set_inspected(&self, attachment: AttachmentId, found: Inspected) {
        if let Ok(mut t) = self.tokens.lock() {
            t.set_inspected(attachment, found);
        }
    }

    pub fn token_count(&self) -> usize {
        self.tokens.lock().map(|t| t.len()).unwrap_or(0)
    }

    /// The original's size, as the helper reported it for a rendition.
    pub fn source_size(&self, blob: BlobId) -> Option<(u32, u32)> {
        self.shared.sizes.lock().ok()?.get(&blob).copied()
    }

    pub fn previews_off(&self) -> bool {
        self.shared.previews_off.load(Ordering::Relaxed)
    }

    pub fn crashes(&self) -> u32 {
        self.shared.crashes.load(Ordering::Relaxed)
    }

    /// Answer a rendition request: at once from the cache, a refusal for
    /// a remembered failure, otherwise through the decode queue.
    pub fn request_rendition(&self, blob: BlobId, rendition: Rendition, reply: Reply) {
        let key = (blob, rendition);
        if self.previews_off() || self.shared.failed(key) {
            return reply.send(Outcome::Refused, MediaResponse::refused());
        }
        if let Some(hit) = self.shared.cached(key) {
            return reply.send(
                Outcome::Cached,
                MediaResponse::image(hit.format.mime(), hit.bytes.to_vec()),
            );
        }
        match self.decode.push(DecodeJob {
            blob,
            rendition,
            reply,
        }) {
            Pushed::Queued => {}
            Pushed::Displaced(old) => old.reply.send(Outcome::Dropped, MediaResponse::busy()),
            Pushed::Refused(job) => job.reply.send(Outcome::Dropped, MediaResponse::busy()),
        }
    }

    /// Queue a stream chunk; a full queue is `503`.
    pub fn request_stream(&self, job: StreamJob) {
        if let Pushed::Refused(job) | Pushed::Displaced(job) = self.stream.push(job) {
            job.reply.send(Outcome::Dropped, MediaResponse::busy());
        }
    }

    /// Jobs waiting in the decode queue (for tests).
    pub fn decode_queued(&self) -> usize {
        self.decode.queued()
    }

    /// End the session's media: waiting requests answer `503`, the
    /// workers stop after the job each holds (dropping their readers and
    /// helper processes), and the caches are cleared.
    pub fn close(&self) {
        self.decode.close();
        self.stream.close();
        if let Ok(mut c) = self.shared.cache.lock() {
            c.clear();
        }
        if let Ok(mut s) = self.shared.sizes.lock() {
            s.clear();
        }
    }

    /// Wait for the workers to stop (tests; never from a command).
    #[cfg(test)]
    pub fn join(&self) {
        self.decode.join();
        self.stream.join();
    }
}

impl Drop for MediaEngine {
    fn drop(&mut self) {
        self.close();
    }
}

fn decode_one(
    job: DecodeJob,
    shared: &Shared,
    source: &ReaderSource,
    rd: &mut Option<ArchiveReader>,
    backend: &mut Option<Box<dyn RenderBackend>>,
    factory: &BackendFactory,
    cap: u32,
) {
    let key = (job.blob, job.rendition);
    if let Some(hit) = shared.cached(key) {
        return job.reply.send(
            Outcome::Cached,
            MediaResponse::image(hit.format.mime(), hit.bytes.to_vec()),
        );
    }
    if shared.previews_off.load(Ordering::Relaxed) || shared.failed(key) {
        return job.reply.send(Outcome::Refused, MediaResponse::refused());
    }
    let Some(r) = reader(rd, source) else {
        return job
            .reply
            .send(Outcome::Unreadable, MediaResponse::refused());
    };
    let bytes = match r.read_blob_verified(job.blob, MAX_INPUT_BYTES) {
        Ok(b) => b,
        Err(ArchiveError::Blob(BlobProblem::OverLimit | BlobProblem::DigestMismatch)) => {
            shared.fail(key);
            return job
                .reply
                .send(Outcome::DecodeFailed, MediaResponse::refused());
        }
        Err(_) => {
            return job
                .reply
                .send(Outcome::Unreadable, MediaResponse::refused())
        }
    };
    let b = backend.get_or_insert_with(|| factory());
    let outcome = b.render(bytes.expose(), job.rendition, &RenderLimits::default());
    drop(bytes);
    match outcome {
        RenderOutcome::Image(img) => {
            if let Ok(mut s) = shared.sizes.lock() {
                s.insert(job.blob, (img.source_width, img.source_height));
            }
            let bytes = Arc::new(img.bytes);
            if let Ok(mut c) = shared.cache.lock() {
                c.insert(
                    key,
                    CachedImage {
                        format: img.format,
                        bytes: Arc::clone(&bytes),
                    },
                );
            }
            job.reply.send(
                Outcome::Decoded,
                MediaResponse::image(img.format.mime(), bytes.to_vec()),
            );
        }
        RenderOutcome::Refused(_) => {
            shared.fail(key);
            job.reply
                .send(Outcome::DecodeFailed, MediaResponse::refused());
        }
        RenderOutcome::Crashed => {
            shared.fail(key);
            let n = shared
                .crashes
                .fetch_add(1, Ordering::Relaxed)
                .saturating_add(1);
            if n >= cap {
                shared.previews_off.store(true, Ordering::Relaxed);
            }
            tracing::warn!(
                crashes = n,
                previews_off = n >= cap,
                "decode helper crashed"
            );
            job.reply
                .send(Outcome::HelperCrashed, MediaResponse::refused());
        }
    }
}

fn stream_one(job: StreamJob, source: &ReaderSource, rd: &mut Option<ArchiveReader>) {
    let Some(r) = reader(rd, source) else {
        return job
            .reply
            .send(Outcome::Unreadable, MediaResponse::refused());
    };
    let Ok(mut range) = r.open_range(job.blob, &job.ident) else {
        return job
            .reply
            .send(Outcome::Unreadable, MediaResponse::refused());
    };
    let len = range.len();
    if job.whole && len == 0 {
        return job.reply.send(
            Outcome::Streamed,
            MediaResponse::whole(job.mime, Vec::new()),
        );
    }
    let Some(n) = job
        .end
        .checked_sub(job.start)
        .and_then(|d| d.checked_add(1))
        .and_then(|n| usize::try_from(n).ok())
    else {
        return job.reply.send(Outcome::Refused, MediaResponse::refused());
    };
    let mut buf = vec![0u8; n];
    match range.read_at(job.start, &mut buf) {
        Ok(got) if got == n => {}
        _ => {
            return job
                .reply
                .send(Outcome::Unreadable, MediaResponse::refused())
        }
    }
    let resp = if job.whole {
        MediaResponse::whole(job.mime, buf)
    } else {
        MediaResponse::partial(job.mime, job.start, job.end, len, buf)
    };
    job.reply.send(Outcome::Streamed, resp);
}
