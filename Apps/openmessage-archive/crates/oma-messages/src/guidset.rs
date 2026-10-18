//! GUID sets: "reply originator not found" and "association target not
//! found", counted in Rust rather than SQL (M3 §7.3, D46).
//!
//! A GUID check in SQL would make SQLite build an automatic index on an
//! attacker-sized `guid` column. Instead, during the message pass,
//! [`GuidSets`] collects SHA-256 of each GUID truncated to 64 bits:
//!
//! - every `message.guid`;
//! - the reply-originator targets (`thread_originator_guid`);
//! - the association targets (the GUID inside `associated_message_guid`).
//!
//! Only values within their 256-byte cap are hashed; empty values are not.
//! At the end the vectors are sorted, and each target is looked up among
//! the message GUIDs. A target counts once per row that names it.
//!
//! - **Reproducible.** The hash is fixed (truncated SHA-256), so the counts
//!   are the same across runs, machines and Rust versions. A sorted
//!   `Vec<u64>` has no buckets to flood; its cost is O(n log n) whatever the
//!   input.
//! - **A crafted collision can change a count**, never a record (§25).
//! - **Capped.** The message vector holds at most [`GUID_SET_CAP`] entries,
//!   and the two target vectors together at most as many again: about
//!   2 × 32 MiB of entries. Past a cap, the affected counts are `None`.
#![deny(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::unreachable,
    clippy::unimplemented,
    clippy::string_slice
)]

use sha2::{Digest, Sha256};

/// The most entries the message vector, and the two target vectors
/// together, may hold: 4,194,304 (32 MiB of `u64`).
pub const GUID_SET_CAP: usize = 4_194_304;

/// SHA-256 of `guid`, truncated to its first 64 bits (big-endian).
fn hash(guid: &[u8]) -> u64 {
    let digest = Sha256::digest(guid);
    digest
        .first_chunk::<8>()
        .map_or(0, |b| u64::from_be_bytes(*b))
}

/// The GUID sets of one message pass.
#[derive(Clone, PartialEq, Eq)]
pub struct GuidSets {
    cap: usize,
    messages: Vec<u64>,
    replies: Vec<u64>,
    associations: Vec<u64>,
    messages_overflowed: bool,
    targets_overflowed: bool,
}

impl std::fmt::Debug for GuidSets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Hashes of identifiers are not printed; only the sizes.
        f.debug_struct("GuidSets")
            .field("messages", &self.messages.len())
            .field("replies", &self.replies.len())
            .field("associations", &self.associations.len())
            .field("messages_overflowed", &self.messages_overflowed)
            .field("targets_overflowed", &self.targets_overflowed)
            .finish()
    }
}

impl Default for GuidSets {
    fn default() -> Self {
        Self::new()
    }
}

/// The counts the sets produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct GuidCounts {
    /// Distinct message GUIDs collected, when the message vector stayed
    /// within its cap.
    pub distinct_messages: Option<u64>,
    /// Message rows whose GUID is also another row's (by hash), when the
    /// message vector stayed within its cap.
    pub duplicate_messages: Option<u64>,
    /// Reply rows whose originator GUID matches no message GUID.
    pub reply_originator_not_found: Option<u64>,
    /// Associated rows whose target GUID matches no message GUID.
    pub association_target_not_found: Option<u64>,
}

impl GuidSets {
    /// Empty sets with the default cap, [`GUID_SET_CAP`].
    pub fn new() -> Self {
        Self::with_cap(GUID_SET_CAP)
    }

    /// Empty sets with a smaller cap (clamped to `1..=GUID_SET_CAP`), for
    /// tests.
    pub fn with_cap(cap: usize) -> Self {
        GuidSets {
            cap: cap.clamp(1, GUID_SET_CAP),
            messages: Vec::new(),
            replies: Vec::new(),
            associations: Vec::new(),
            messages_overflowed: false,
            targets_overflowed: false,
        }
    }

    /// Add one `message.guid`.
    pub fn add_message(&mut self, guid: &[u8]) {
        if guid.is_empty() {
            return;
        }
        if self.messages.len() >= self.cap {
            self.messages_overflowed = true;
            return;
        }
        self.messages.push(hash(guid));
    }

    /// Add one reply's `thread_originator_guid`.
    pub fn add_reply_target(&mut self, guid: &[u8]) {
        if !guid.is_empty() && self.target_room() {
            self.replies.push(hash(guid));
        }
    }

    /// Add one association's target GUID.
    pub fn add_association_target(&mut self, guid: &[u8]) {
        if !guid.is_empty() && self.target_room() {
            self.associations.push(hash(guid));
        }
    }

    fn target_room(&mut self) -> bool {
        let used = self.replies.len().saturating_add(self.associations.len());
        if used >= self.cap {
            self.targets_overflowed = true;
            false
        } else {
            true
        }
    }

    /// Whether every GUID offered was kept.
    pub fn complete(&self) -> bool {
        !self.messages_overflowed && !self.targets_overflowed
    }

    /// Sort and merge-count. The sets stay usable; counting twice gives the
    /// same result.
    pub fn counts(&mut self) -> GuidCounts {
        self.messages.sort_unstable();
        self.replies.sort_unstable();
        self.associations.sort_unstable();
        if self.messages_overflowed {
            return GuidCounts::default();
        }
        let rows = len_u64(self.messages.len());
        let distinct = distinct(&self.messages);
        let targets_ok = !self.targets_overflowed;
        GuidCounts {
            distinct_messages: Some(distinct),
            duplicate_messages: Some(rows.saturating_sub(distinct)),
            reply_originator_not_found: targets_ok
                .then(|| not_found(&self.messages, &self.replies)),
            association_target_not_found: targets_ok
                .then(|| not_found(&self.messages, &self.associations)),
        }
    }
}

fn len_u64(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

/// Distinct values in a sorted slice.
fn distinct(sorted: &[u64]) -> u64 {
    let mut n: u64 = 0;
    let mut prev = None;
    for v in sorted {
        if prev != Some(*v) {
            n = n.saturating_add(1);
            prev = Some(*v);
        }
    }
    n
}

/// Entries of the sorted `targets` absent from the sorted `set`: one merge
/// walk.
fn not_found(set: &[u64], targets: &[u64]) -> u64 {
    let mut missing: u64 = 0;
    let mut s = set.iter().peekable();
    for t in targets {
        while s.next_if(|v| *v < t).is_some() {}
        if s.peek() != Some(&t) {
            missing = missing.saturating_add(1);
        }
    }
    missing
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sets(messages: &[&str], replies: &[&str], assocs: &[&str]) -> GuidSets {
        let mut g = GuidSets::new();
        for m in messages {
            g.add_message(m.as_bytes());
        }
        for r in replies {
            g.add_reply_target(r.as_bytes());
        }
        for a in assocs {
            g.add_association_target(a.as_bytes());
        }
        g
    }

    #[test]
    fn merge_counting() {
        let mut g = sets(
            &["A", "B", "C", "C", ""],
            &["A", "X", "X", "C", ""],
            &["B", "Y", "A"],
        );
        let c = g.counts();
        assert_eq!(c.distinct_messages, Some(3));
        assert_eq!(c.duplicate_messages, Some(1));
        // Each reply row naming a missing GUID counts once.
        assert_eq!(c.reply_originator_not_found, Some(2));
        assert_eq!(c.association_target_not_found, Some(1));
        assert_eq!(g.counts(), c, "counting twice gives the same result");
        assert!(g.complete());

        let mut empty = GuidSets::default();
        assert_eq!(
            empty.counts(),
            GuidCounts {
                distinct_messages: Some(0),
                duplicate_messages: Some(0),
                reply_originator_not_found: Some(0),
                association_target_not_found: Some(0),
            }
        );
    }

    /// The same GUIDs in any order give the same counts, and the hash is a
    /// fixed function.
    #[test]
    fn determinism() {
        let msgs: Vec<String> = (0..200).map(|i| format!("SYNTH-GUID-{i}")).collect();
        let targets: Vec<String> = (150..260).map(|i| format!("SYNTH-GUID-{i}")).collect();
        let build = |rev: bool| {
            let mut g = GuidSets::new();
            let mut m: Vec<&String> = msgs.iter().collect();
            let mut t: Vec<&String> = targets.iter().collect();
            if rev {
                m.reverse();
                t.reverse();
            }
            for x in m {
                g.add_message(x.as_bytes());
            }
            for x in t {
                g.add_reply_target(x.as_bytes());
                g.add_association_target(x.as_bytes());
            }
            g.counts()
        };
        let a = build(false);
        assert_eq!(a, build(true));
        assert_eq!(a.reply_originator_not_found, Some(60));
        assert_eq!(a.association_target_not_found, Some(60));
        // SHA-256("abc") starts ba7816bf 8f01cfea.
        assert_eq!(hash(b"abc"), 0xba78_16bf_8f01_cfea);
    }

    #[test]
    fn caps_give_none() {
        let mut g = GuidSets::with_cap(3);
        for m in ["A", "B", "C"] {
            g.add_message(m.as_bytes());
        }
        g.add_reply_target(b"A");
        g.add_association_target(b"Z");
        g.add_reply_target(b"B");
        assert!(g.complete());
        let c = g.counts();
        assert_eq!(c.reply_originator_not_found, Some(0));
        assert_eq!(c.association_target_not_found, Some(1));

        // A fourth target overflows the shared target cap.
        g.add_association_target(b"Q");
        assert!(!g.complete());
        let c = g.counts();
        assert_eq!(c.distinct_messages, Some(3));
        assert_eq!(c.reply_originator_not_found, None);
        assert_eq!(c.association_target_not_found, None);

        // A fourth message overflows the message cap: every count is None.
        g.add_message(b"D");
        assert_eq!(g.counts(), GuidCounts::default());
        assert_eq!(GuidSets::with_cap(0).cap, 1);
        assert_eq!(GuidSets::with_cap(usize::MAX).cap, GUID_SET_CAP);
        assert!(!format!("{g:?}").contains('A'));
    }
}
