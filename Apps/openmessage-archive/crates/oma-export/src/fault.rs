//! Crash points (M6 spec §14.2 "Crash", the `tests/crash.rs` pattern of
//! `oma-archive`).
//!
//! [`point`] marks a moment a crash test kills the process at. Without the
//! non-default `fault-injection` feature it is an empty inline function and
//! the [`hooks`] module does not exist; only this crate's own tests enable
//! the feature (a self dev-dependency). With it, each hit is counted per
//! thread, and [`hooks::arm`] runs an action at the n-th hit of one point
//! (the crash child prints `FAULT <point> <n>` and blocks until the parent
//! kills it).

/// A moment a crash test can stop an export at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum FaultPoint {
    /// The staging folder exists (0700); its marker is not created yet.
    StagingCreated,
    /// The first file inside the staging folder was created.
    FirstFileStaged,
    /// Every content file is closed and synced; `README.txt` is next.
    BeforeReadme,
    /// `SHA256SUMS` is written and the staging folder flushed; the
    /// publish rename is next.
    AfterSums,
    /// The publish rename happened; the parent's flush is next.
    PublishedBeforeDirFlush,
    /// The parent is flushed; the marker's removal is next.
    PublishedBeforeMarkerRemoval,
    /// A single file's `.partial-file` is written and synced; its
    /// placement is next.
    SingleFileStaged,
    /// An attachment's copy has its first bytes in its final name inside
    /// staging; the rest of the stream is next.
    MidAttachmentCopy,
}

impl FaultPoint {
    /// Every point, in a fixed order.
    pub const ALL: [FaultPoint; 8] = [
        FaultPoint::StagingCreated,
        FaultPoint::FirstFileStaged,
        FaultPoint::BeforeReadme,
        FaultPoint::AfterSums,
        FaultPoint::PublishedBeforeDirFlush,
        FaultPoint::PublishedBeforeMarkerRemoval,
        FaultPoint::SingleFileStaged,
        FaultPoint::MidAttachmentCopy,
    ];

    /// The point's name in `OMA_EXPORT_TEST_FAULT` and in `FAULT` lines.
    pub fn name(self) -> &'static str {
        match self {
            FaultPoint::StagingCreated => "staging-created",
            FaultPoint::FirstFileStaged => "first-file-staged",
            FaultPoint::BeforeReadme => "before-readme",
            FaultPoint::AfterSums => "after-sums",
            FaultPoint::PublishedBeforeDirFlush => "published-before-dir-flush",
            FaultPoint::PublishedBeforeMarkerRemoval => "published-before-marker-removal",
            FaultPoint::SingleFileStaged => "single-file-staged",
            FaultPoint::MidAttachmentCopy => "mid-attachment-copy",
        }
    }

    /// The point with this name.
    pub fn from_name(name: &str) -> Option<FaultPoint> {
        FaultPoint::ALL.into_iter().find(|p| p.name() == name)
    }

    /// Whether a kill here leaves a published export (the publish rename
    /// happened).
    pub fn after_publish(self) -> bool {
        matches!(
            self,
            FaultPoint::PublishedBeforeDirFlush | FaultPoint::PublishedBeforeMarkerRemoval
        )
    }
}

/// Mark a crash point. Does nothing without the `fault-injection` feature.
#[inline]
pub(crate) fn point(p: FaultPoint) {
    #[cfg(feature = "fault-injection")]
    hooks::hit(p);
    #[cfg(not(feature = "fault-injection"))]
    let _ = p;
}

/// Test hooks: per-thread hit counts and an action at the n-th hit.
#[cfg(feature = "fault-injection")]
pub mod hooks {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    pub use super::FaultPoint;

    /// What runs at the armed hit.
    pub type Action = Box<dyn FnMut(FaultPoint, u32)>;

    #[derive(Default)]
    struct State {
        hits: BTreeMap<FaultPoint, u32>,
        armed: Option<(FaultPoint, u32, Action)>,
    }

    thread_local! {
        static STATE: RefCell<State> = RefCell::new(State::default());
    }

    pub(crate) fn hit(p: FaultPoint) {
        let fire = STATE.with(|s| {
            let mut s = s.borrow_mut();
            let n = {
                let c = s.hits.entry(p).or_insert(0);
                *c += 1;
                *c
            };
            match s.armed.take() {
                Some((at, nth, action)) if at == p && nth == n => Some((action, n)),
                other => {
                    s.armed = other;
                    None
                }
            }
        });
        if let Some((mut action, n)) = fire {
            action(p, n);
        }
    }

    /// Disarms the action and clears the counts when dropped.
    #[must_use]
    pub struct Armed(());

    impl Drop for Armed {
        fn drop(&mut self) {
            STATE.with(|s| *s.borrow_mut() = State::default());
        }
    }

    /// Run `action` at the `nth` hit (from 1) of `point` on this thread.
    pub fn arm(point: FaultPoint, nth: u32, action: Action) -> Armed {
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            s.hits.clear();
            s.armed = Some((point, nth, action));
        });
        Armed(())
    }

    /// The hits of `point` on this thread since the last [`arm`] or
    /// [`reset`].
    pub fn hits(point: FaultPoint) -> u32 {
        STATE.with(|s| s.borrow().hits.get(&point).copied().unwrap_or(0))
    }

    /// Clear this thread's counts (and any armed action).
    pub fn reset() {
        STATE.with(|s| *s.borrow_mut() = State::default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip_and_are_unique() {
        let mut names: Vec<&str> = FaultPoint::ALL.iter().map(|p| p.name()).collect();
        for p in FaultPoint::ALL {
            assert_eq!(FaultPoint::from_name(p.name()), Some(p));
        }
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), FaultPoint::ALL.len());
        assert_eq!(FaultPoint::from_name("nope"), None);
    }

    #[cfg(feature = "fault-injection")]
    #[test]
    fn the_armed_action_runs_once_at_the_nth_hit() {
        use std::cell::Cell;
        use std::rc::Rc;
        let fired = Rc::new(Cell::new(0u32));
        let f = Rc::clone(&fired);
        let _armed = hooks::arm(
            FaultPoint::FirstFileStaged,
            2,
            Box::new(move |_, n| f.set(n)),
        );
        point(FaultPoint::FirstFileStaged);
        assert_eq!(fired.get(), 0);
        point(FaultPoint::AfterSums);
        point(FaultPoint::FirstFileStaged);
        assert_eq!(fired.get(), 2);
        point(FaultPoint::FirstFileStaged);
        assert_eq!(hooks::hits(FaultPoint::FirstFileStaged), 3);
        assert_eq!(hooks::hits(FaultPoint::AfterSums), 1);
    }
}
