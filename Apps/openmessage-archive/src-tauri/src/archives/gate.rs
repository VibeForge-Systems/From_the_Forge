//! The shell's side of the import gate (spec §15.1, D65): the worker blocks
//! here, after the preflight reads and before the archive's first write,
//! until `confirm_import` answers, the job is cancelled, the app exits or
//! two minutes pass.
//!
//! A `Mutex` and a `Condvar`. The deadline comes from a [`GateClock`], so
//! tests inject one and advance it; the wait wakes at least every
//! [`GATE_POLL`] to re-read it. Every ending other than an answer to
//! proceed declines, and a declined import writes nothing.

use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use oma_archive::LineageChoice;
use oma_backup::CancelToken;

/// How long the gate waits for an answer (spec §2, D65).
pub const GATE_TIMEOUT: Duration = Duration::from_secs(120);
/// The longest single wait before the clock and the cancel token are read
/// again.
pub const GATE_POLL: Duration = Duration::from_millis(250);

/// A monotonic clock for the gate's deadline.
pub trait GateClock: Send + Sync {
    /// Time since an arbitrary fixed origin.
    fn now(&self) -> Duration;
}

/// The host's monotonic clock.
#[derive(Debug, Clone, Copy)]
pub struct MonotonicClock(Instant);

impl Default for MonotonicClock {
    fn default() -> Self {
        MonotonicClock(Instant::now())
    }
}

impl GateClock for MonotonicClock {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }
}

/// The user's answer through `confirm_import`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateAnswer {
    Proceed(LineageChoice),
    Decline,
}

/// How a wait at the gate ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateEnding {
    Answered(GateAnswer),
    /// The job was cancelled (or the app is exiting).
    Cancelled,
    /// No answer within the timeout.
    TimedOut,
}

/// Why an answer was not taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotWaiting {
    /// The job is not at the gate (not yet, or no longer).
    NotWaiting,
    /// An answer was already given and not yet taken.
    AlreadyAnswered,
}

#[derive(Debug, Default)]
struct State {
    waiting: bool,
    answer: Option<GateAnswer>,
}

/// One import job's gate.
pub struct Gate {
    state: Mutex<State>,
    cv: Condvar,
    clock: Arc<dyn GateClock>,
    timeout: Duration,
}

impl std::fmt::Debug for Gate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Gate")
            .field("waiting", &self.is_waiting())
            .finish_non_exhaustive()
    }
}

impl Gate {
    pub fn new(clock: Arc<dyn GateClock>, timeout: Duration) -> Gate {
        Gate {
            state: Mutex::new(State::default()),
            cv: Condvar::new(),
            clock,
            timeout,
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // A poisoned gate still holds a usable state: the worst case is a
        // decline.
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The job is waiting for an answer.
    pub fn is_waiting(&self) -> bool {
        self.lock().waiting
    }

    /// Start accepting an answer, before the preflight is announced, so an
    /// answer that arrives before [`Gate::wait`] runs is kept.
    pub fn open(&self) {
        let mut s = self.lock();
        s.waiting = true;
        s.answer = None;
    }

    /// Block until an answer, a cancel or the timeout. Opens the gate if
    /// [`Gate::open`] was not called.
    pub fn wait(&self, cancel: &CancelToken) -> GateEnding {
        let deadline = self.clock.now().saturating_add(self.timeout);
        let mut s = self.lock();
        s.waiting = true;
        let ending = loop {
            if cancel.is_cancelled() {
                break GateEnding::Cancelled;
            }
            if let Some(a) = s.answer.take() {
                break GateEnding::Answered(a);
            }
            let now = self.clock.now();
            if now >= deadline {
                break GateEnding::TimedOut;
            }
            let wait = deadline.saturating_sub(now).min(GATE_POLL);
            s = match self.cv.wait_timeout(s, wait) {
                Ok((g, _)) => g,
                Err(p) => p.into_inner().0,
            };
        };
        s.waiting = false;
        s.answer = None;
        ending
    }

    /// Give the waiting job its answer.
    pub fn answer(&self, a: GateAnswer) -> Result<(), NotWaiting> {
        let mut s = self.lock();
        if !s.waiting {
            return Err(NotWaiting::NotWaiting);
        }
        if s.answer.is_some() {
            return Err(NotWaiting::AlreadyAnswered);
        }
        s.answer = Some(a);
        drop(s);
        self.cv.notify_all();
        Ok(())
    }

    /// Make the waiting job re-check its cancel token and the clock (after
    /// a cancel, at exit, or after a test advanced its clock).
    pub fn wake(&self) {
        let _guard = self.lock();
        self.cv.notify_all();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A clock tests advance by hand.
    #[derive(Debug, Default)]
    pub(crate) struct FakeClock(AtomicU64);

    impl FakeClock {
        pub(crate) fn advance(&self, d: Duration) {
            let ms = u64::try_from(d.as_millis()).unwrap_or(u64::MAX);
            self.0.fetch_add(ms, Ordering::SeqCst);
        }
    }

    impl GateClock for FakeClock {
        fn now(&self) -> Duration {
            Duration::from_millis(self.0.load(Ordering::SeqCst))
        }
    }

    fn until_waiting(g: &Gate) {
        for _ in 0..2000 {
            if g.is_waiting() {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("the gate never waited");
    }

    fn gate() -> (Arc<FakeClock>, Arc<Gate>) {
        let clock = Arc::new(FakeClock::default());
        let g = Arc::new(Gate::new(clock.clone(), GATE_TIMEOUT));
        (clock, g)
    }

    #[test]
    fn an_answer_ends_the_wait() {
        for answer in [GateAnswer::Proceed(LineageChoice::New), GateAnswer::Decline] {
            let (_, g) = gate();
            assert_eq!(g.answer(answer), Err(NotWaiting::NotWaiting));
            let w = g.clone();
            let h = std::thread::spawn(move || w.wait(&CancelToken::new()));
            until_waiting(&g);
            g.answer(answer).expect("waiting");
            assert_eq!(h.join().expect("join"), GateEnding::Answered(answer));
            assert!(!g.is_waiting());
            assert_eq!(g.answer(answer), Err(NotWaiting::NotWaiting));
        }
    }

    #[test]
    fn an_answer_before_the_wait_is_kept() {
        let (_, g) = gate();
        g.open();
        g.answer(GateAnswer::Decline).expect("open");
        assert_eq!(
            g.answer(GateAnswer::Decline),
            Err(NotWaiting::AlreadyAnswered)
        );
        assert_eq!(
            g.wait(&CancelToken::new()),
            GateEnding::Answered(GateAnswer::Decline)
        );
    }

    #[test]
    fn a_cancel_wakes_the_gate() {
        let (_, g) = gate();
        let token = CancelToken::new();
        let (w, t) = (g.clone(), token.clone());
        let h = std::thread::spawn(move || w.wait(&t));
        until_waiting(&g);
        token.cancel();
        g.wake();
        assert_eq!(h.join().expect("join"), GateEnding::Cancelled);
    }

    #[test]
    fn the_wait_times_out_after_two_minutes_of_the_injected_clock() {
        let (clock, g) = gate();
        let w = g.clone();
        let h = std::thread::spawn(move || w.wait(&CancelToken::new()));
        until_waiting(&g);
        clock.advance(GATE_TIMEOUT - Duration::from_millis(1));
        g.wake();
        std::thread::sleep(Duration::from_millis(20));
        assert!(g.is_waiting(), "one millisecond is left");
        clock.advance(Duration::from_millis(1));
        g.wake();
        assert_eq!(h.join().expect("join"), GateEnding::TimedOut);
    }
}
