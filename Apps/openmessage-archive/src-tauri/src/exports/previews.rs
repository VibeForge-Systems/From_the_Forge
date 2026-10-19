//! The export's decode-helper pool (M6 spec §6.5, D113, D133), slice 6h2.
//!
//! An export with previews on starts its own pool of decode helpers,
//! separate from any viewer session's: `min(2, available_parallelism − 1)`
//! workers and at least one ([`pool_size`]). Each worker owns one
//! [`RenderBackend`] (in the app, a `--oma-decode-helper` process started
//! from this binary, `CREATE_NO_WINDOW` on Windows) and answers one
//! `RenditionPair` request at a time: the thumbnail and, with "Larger
//! previews", the display rendition from a single decode. The bytes are
//! the verified buffer the pipeline read; the renditions go back to
//! `oma_export`'s feed unchecked, and the HTML writer checks each one
//! (`preview::check_rendition`) before it is embedded or written.
//!
//! - **Outcomes.** A refusal (over a limit, not decodable, HEIC without
//!   the feature) is "Preview unavailable"; a crash, a hang past M5's 20 s
//!   timeout or garbage is a crash, which the feed counts towards turning
//!   previews off after three in a row.
//! - **The end.** [`HelperPool::shutdown`] closes the queue; on cancel or
//!   failure it also kills every running helper through its
//!   [`KillSwitch`], so no helper outlives the export by more than its
//!   kill. Requests still queued are dropped unanswered. Dropping the pool
//!   does the same as a killing shutdown.
//! - **Logs (I6).** The helpers' stderr is drained and counted, never kept
//!   or forwarded; the shutdown returns its byte count for the job's log
//!   (as a bucket). Nothing here logs.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use oma_archive::RenderLimits;
use oma_export::preview::{PoolOutcome, RenditionBytes, RenditionPool, RenditionTicket};

use crate::viewer::media::helper::{BackendFactory, KillSwitch, PairOutcome, RenderBackend};

/// `min(2, parallelism − 1)`, at least 1 (spec §6.5).
pub fn pool_size(parallelism: usize) -> usize {
    parallelism.saturating_sub(1).clamp(1, 2)
}

/// The pool size on this machine.
pub fn default_pool_size() -> usize {
    pool_size(std::thread::available_parallelism().map_or(1, usize::from))
}

struct Request {
    bytes: Vec<u8>,
    display: bool,
    reply: Sender<PoolOutcome>,
}

struct Shared {
    queue: Mutex<Receiver<Request>>,
    stopping: AtomicBool,
    stderr: AtomicU64,
    switches: Mutex<Vec<KillSwitch>>,
}

/// The pool behind one export's previews.
pub struct HelperPool {
    sender: Mutex<Option<Sender<Request>>>,
    shared: Arc<Shared>,
    workers: Vec<JoinHandle<()>>,
}

impl std::fmt::Debug for HelperPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HelperPool")
            .field("workers", &self.workers.len())
            .finish_non_exhaustive()
    }
}

fn outcome(o: PairOutcome, display: bool) -> PoolOutcome {
    match o {
        PairOutcome::Images(t, d) => PoolOutcome::Ready {
            thumb: RenditionBytes(t.bytes),
            display: display.then_some(RenditionBytes(d.bytes)),
        },
        PairOutcome::Refused(_) => PoolOutcome::Unavailable,
        PairOutcome::Crashed => PoolOutcome::Crashed,
    }
}

/// One worker: its own backend, one request at a time until the queue
/// closes or the pool stops.
fn work(shared: &Shared, mut backend: Box<dyn RenderBackend>) {
    if let Some(k) = backend.kill_switch() {
        if let Ok(mut s) = shared.switches.lock() {
            s.push(k);
        }
    }
    let limits = RenderLimits::default();
    let mut counted = 0u64;
    loop {
        let next = match shared.queue.lock() {
            Ok(q) => q.recv(),
            Err(_) => break,
        };
        let Ok(req) = next else { break };
        if shared.stopping.load(Ordering::SeqCst) {
            // Dropped unanswered: nobody waits for it any more.
            continue;
        }
        let o = if req.display {
            backend.render_pair(&req.bytes, &limits)
        } else {
            match backend.render(&req.bytes, oma_archive::Rendition::Thumb, &limits) {
                crate::viewer::media::helper::RenderOutcome::Image(t) => {
                    PairOutcome::Images(t.clone(), t)
                }
                crate::viewer::media::helper::RenderOutcome::Refused(e) => PairOutcome::Refused(e),
                crate::viewer::media::helper::RenderOutcome::Crashed => PairOutcome::Crashed,
            }
        };
        drop(req.bytes);
        let now = backend.stderr_bytes();
        shared
            .stderr
            .fetch_add(now.saturating_sub(counted), Ordering::Relaxed);
        counted = now;
        // A receiver that went away (a cancelled wait) only loses it.
        let _ = req.reply.send(outcome(o, req.display));
    }
    drop(backend);
}

impl HelperPool {
    /// Start `size` workers, each with a backend from `factory`.
    pub fn start(factory: &BackendFactory, size: usize) -> HelperPool {
        let (tx, rx) = mpsc::channel::<Request>();
        let shared = Arc::new(Shared {
            queue: Mutex::new(rx),
            stopping: AtomicBool::new(false),
            stderr: AtomicU64::new(0),
            switches: Mutex::new(Vec::new()),
        });
        let mut workers = Vec::new();
        for _ in 0..size.max(1) {
            let backend = factory();
            let s = Arc::clone(&shared);
            if let Ok(h) = std::thread::Builder::new()
                .name("oma-export-previews".to_owned())
                .spawn(move || work(&s, backend))
            {
                workers.push(h);
            }
        }
        HelperPool::assemble(tx, shared, workers)
    }

    /// The pool over `workers`. With none (every thread failed to spawn)
    /// the queue is closed at once: nobody would ever answer a request, so
    /// each submit is a crash, and after three the feed turns previews off
    /// rather than the export waiting forever.
    fn assemble(
        tx: Sender<Request>,
        shared: Arc<Shared>,
        workers: Vec<JoinHandle<()>>,
    ) -> HelperPool {
        let sender = if workers.is_empty() {
            drop(tx);
            None
        } else {
            Some(tx)
        };
        HelperPool {
            sender: Mutex::new(sender),
            shared,
            workers,
        }
    }

    /// The workers that started.
    pub fn workers(&self) -> usize {
        self.workers.len()
    }

    /// Close the queue. Without `kill` (the export published: nothing is
    /// in flight) wait for the workers, whose helpers exit with them. With
    /// `kill` (cancel, failure) stop and kill every running helper and do
    /// not wait, so the job's end is never held up by a helper. Returns the
    /// bytes the helpers wrote to stderr so far.
    pub fn shutdown(mut self, kill: bool) -> u64 {
        self.stop(kill);
        let workers: Vec<JoinHandle<()>> = self.workers.drain(..).collect();
        if !kill {
            for h in workers {
                let _ = h.join();
            }
        }
        self.shared.stderr.load(Ordering::Relaxed)
    }

    fn stop(&self, kill: bool) {
        if kill {
            self.shared.stopping.store(true, Ordering::SeqCst);
        }
        if let Ok(mut s) = self.sender.lock() {
            s.take();
        }
        if kill {
            if let Ok(s) = self.shared.switches.lock() {
                for k in s.iter() {
                    k.kill();
                }
            }
        }
    }
}

impl Drop for HelperPool {
    fn drop(&mut self) {
        self.stop(true);
    }
}

impl RenditionPool for HelperPool {
    fn submit(&self, bytes: Vec<u8>, display: bool) -> RenditionTicket {
        let (reply, ticket) = RenditionTicket::channel();
        let sent = self.sender.lock().ok().and_then(|s| {
            s.as_ref().map(|tx| {
                tx.send(Request {
                    bytes,
                    display,
                    reply,
                })
                .is_ok()
            })
        });
        match sent {
            Some(true) => ticket,
            // No worker will answer: a crash (counted by the feed).
            _ => RenditionTicket::ready(PoolOutcome::Crashed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pool whose threads all failed to start answers every request at
    /// once as a crash, never with a ticket that waits forever.
    #[test]
    fn a_pool_without_workers_answers_every_request_as_a_crash() {
        let (tx, rx) = mpsc::channel::<Request>();
        let shared = Arc::new(Shared {
            queue: Mutex::new(rx),
            stopping: AtomicBool::new(false),
            stderr: AtomicU64::new(0),
            switches: Mutex::new(Vec::new()),
        });
        let pool = HelperPool::assemble(tx, shared, Vec::new());
        assert_eq!(pool.workers(), 0);
        let cancel = oma_archive::CancelToken::new();
        for _ in 0..3 {
            let t = pool.submit(vec![0u8; 8], false);
            assert!(matches!(t.wait(&cancel), Ok(PoolOutcome::Crashed)));
        }
        assert_eq!(pool.shutdown(false), 0);
    }
}
