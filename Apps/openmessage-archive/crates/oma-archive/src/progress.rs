//! The context long archive operations run in: cancellation, progress and
//! the clock (spec §4.3).
//!
//! Time comes from a [`Clock`] so tests can inject one. Every timestamp the
//! archive stores is Unix nanoseconds from the host clock; nothing is stored
//! in local time.

use std::time::{SystemTime, UNIX_EPOCH};

pub use oma_backup::CancelToken;

/// A source of the current time.
pub trait Clock: Send + Sync {
    /// Unix time in nanoseconds. A clock before 1970 or past 2262 saturates.
    fn now_ns(&self) -> i64;
}

/// The host clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ns(&self) -> i64 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(d) => i64::try_from(d.as_nanos()).unwrap_or(i64::MAX),
            Err(_) => 0,
        }
    }
}

/// A fixed clock, for tests and deterministic fixtures.
#[derive(Debug, Clone, Copy)]
pub struct FixedClock(pub i64);

impl Clock for FixedClock {
    fn now_ns(&self) -> i64 {
        self.0
    }
}

/// A progress event. Content-free.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ArchiveEvent {
    /// A migration step is starting.
    MigrationStep { from: u32, to: u32 },
}

/// Receives progress events.
pub trait ArchiveSink {
    fn event(&mut self, event: ArchiveEvent);
}

/// A sink that drops every event.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoProgress;

impl ArchiveSink for NoProgress {
    fn event(&mut self, _event: ArchiveEvent) {}
}

/// Cancellation, progress and the clock for one operation.
pub struct ArchiveCtx<'a> {
    pub cancel: &'a CancelToken,
    pub progress: &'a mut dyn ArchiveSink,
    pub clock: &'a dyn Clock,
}

impl std::fmt::Debug for ArchiveCtx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArchiveCtx")
            .field("cancelled", &self.cancel.is_cancelled())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_clock_is_after_2020() {
        assert!(SystemClock.now_ns() > 1_577_836_800_000_000_000);
        assert_eq!(FixedClock(7).now_ns(), 7);
    }
}
