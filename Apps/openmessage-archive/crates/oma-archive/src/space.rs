//! Free-space probes and the space rules (spec §12, §5.2, §7.2, D68).
//!
//! Space comes through [`SpaceProbe`], real in production ([`FsSpace`],
//! through `oma-fs` and `fs4`) and fake in tests. An error from a probe
//! means "unknown", never zero.
//!
//! - **The estimate** ([`SpaceEstimate`]) is computed in the preflight. It
//!   uses a growth factor `K` that is measured only on synthetic data, so an
//!   estimate above the available space is a warning, never a refusal.
//! - **The one refusal before writing:** available space below
//!   `reserve + wal_allowance`.
//! - **The guards during the import** ([`SpaceGuard`]): before every write
//!   transaction, available space must be at least `reserve +
//!   wal_allowance`, and `max_page_count` is set to what the rest allows and
//!   read back; before every attachment copy, the file must fit above the
//!   same floor. A shortfall stops the import in a resumable state.
#![deny(clippy::arithmetic_side_effects)]

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::Connection;

/// One gibibyte.
const GIB: u64 = 1 << 30;

/// The smallest reserve kept free (§12).
pub const RESERVE_MIN: u64 = GIB;
/// The largest reserve kept free (§12).
pub const RESERVE_MAX: u64 = 10 << 30;
/// Space needed to create an archive (§5.2 step 1).
pub const CREATE_MIN_AVAILABLE: u64 = 64 << 20;

/// Reads free and total space for the volume holding a path.
pub trait SpaceProbe: Send + Sync {
    /// Bytes available to this user.
    fn available(&self, path: &Path) -> io::Result<u64>;
    /// The volume's total size.
    fn total(&self, path: &Path) -> io::Result<u64>;
}

/// The real probe.
#[derive(Debug, Clone, Copy, Default)]
pub struct FsSpace;

impl SpaceProbe for FsSpace {
    fn available(&self, path: &Path) -> io::Result<u64> {
        oma_fs::available_space(path)
    }

    fn total(&self, path: &Path) -> io::Result<u64> {
        oma_fs::total_space(path)
    }
}

/// The reserve: 5 % of the volume, clamped to 1–10 GiB. An unknown total
/// gives the minimum.
pub fn reserve_for(total: Option<u64>) -> u64 {
    total
        .map_or(RESERVE_MIN, |t| t.checked_div(20).unwrap_or(0))
        .clamp(RESERVE_MIN, RESERVE_MAX)
}

/// The space a migration needs (§7.2 step 3): three times the database
/// (the pre-migration copy, plus a rewrite in one transaction, about twice
/// the size in WAL and new pages) plus the reserve. Saturating.
pub fn migration_needs(database_bytes: u64, reserve: u64) -> u64 {
    database_bytes.saturating_mul(3).saturating_add(reserve)
}

/// The growth factor `K` of the estimate, in tenths: the archive database
/// grows by about `K ×` the staged sms.db. **Provisional:** measured only
/// on synthetic data (`tests/perf.rs`, recorded in `compatibility.md`).
pub const GROWTH_K_TENTHS: u64 = 30;

/// FTS merge headroom in the estimate, in hundredths of the staged size,
/// counted twice (spec §12: `2 × 0.35 × staged_bytes`).
const FTS_HEADROOM_HUNDREDTHS: u64 = 35;

/// The WAL allowance: two batch byte budgets plus the WAL's size limit
/// after a checkpoint (spec §12).
pub fn wal_allowance(batch_bytes: u64, journal_size_limit: u64) -> u64 {
    batch_bytes
        .saturating_mul(2)
        .saturating_add(journal_size_limit)
}

/// What the preflight estimates an import needs (spec §12). Content-free.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct SpaceEstimate {
    /// The staged sms.db and its `-wal`.
    pub staged_bytes: u64,
    /// Attachment bytes still to copy (Recently Deleted-only attachments
    /// and already-archived files left out; for encrypted files without a
    /// usable `Size`, the stored length, an upper bound).
    pub bytes_to_copy: u64,
    /// Attachment files still to copy.
    pub files_to_copy: u64,
    /// The largest of them (one temporary copy in `.incoming`).
    pub largest_file: u64,
    /// The free space kept in reserve: 5 % of the volume, 1–10 GiB.
    pub reserve: u64,
    /// Room kept for the WAL.
    pub wal_allowance: u64,
    /// The whole estimate: database growth, attachments, the largest
    /// temporary file, FTS merge headroom, the WAL allowance and the
    /// reserve.
    pub needed: u64,
    /// Available space, `None` when the probe could not tell.
    pub available: Option<u64>,
}

impl SpaceEstimate {
    /// Compute the estimate from its parts.
    pub(crate) fn new(
        staged_bytes: u64,
        bytes_to_copy: u64,
        files_to_copy: u64,
        largest_file: u64,
        reserve: u64,
        wal_allowance: u64,
        available: Option<u64>,
    ) -> SpaceEstimate {
        let growth = staged_bytes
            .saturating_mul(GROWTH_K_TENTHS)
            .checked_div(10)
            .unwrap_or(0);
        let fts = staged_bytes
            .saturating_mul(FTS_HEADROOM_HUNDREDTHS.saturating_mul(2))
            .checked_div(100)
            .unwrap_or(0);
        let needed = [
            growth,
            bytes_to_copy,
            largest_file,
            fts,
            wal_allowance,
            reserve,
        ]
        .iter()
        .fold(0u64, |a, b| a.saturating_add(*b));
        SpaceEstimate {
            staged_bytes,
            bytes_to_copy,
            files_to_copy,
            largest_file,
            reserve,
            wal_allowance,
            needed,
            available,
        }
    }

    /// The floor the import never writes below: `reserve + wal_allowance`.
    pub fn floor(&self) -> u64 {
        self.reserve.saturating_add(self.wal_allowance)
    }

    /// The estimate is above the available space: a warning at the gate,
    /// not a refusal (`K` is unmeasured on real data). `false` when the
    /// available space is unknown; the gate states that separately.
    pub fn warns(&self) -> bool {
        self.available.is_some_and(|a| a < self.needed)
    }

    /// Available space is below the floor: the import is refused before
    /// anything is written.
    pub fn refuses(&self) -> bool {
        self.available.is_some_and(|a| a < self.floor())
    }
}

/// SQLite's largest `max_page_count` on every build this crate may meet
/// (the pre-3.45 limit); a request above it is not compared exactly.
const MAX_PAGE_COUNT_SAFE: i64 = 1_073_741_823;

/// Why a guard stopped the import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GuardStop {
    /// Available space is below the floor, or `max_page_count` read back
    /// a value other than the one requested.
    Space,
    /// Reading the page count or setting the limit failed.
    Sqlite(Option<i32>),
}

/// The space guards of spec §12, for one import.
#[derive(Clone)]
pub(crate) struct SpaceGuard {
    probe: Arc<dyn SpaceProbe>,
    root: PathBuf,
    reserve: u64,
    wal_allowance: u64,
}

impl std::fmt::Debug for SpaceGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpaceGuard")
            .field("reserve", &self.reserve)
            .field("wal_allowance", &self.wal_allowance)
            .finish_non_exhaustive()
    }
}

impl SpaceGuard {
    pub(crate) fn new(
        probe: Arc<dyn SpaceProbe>,
        root: &Path,
        reserve: u64,
        wal_allowance: u64,
    ) -> SpaceGuard {
        SpaceGuard {
            probe,
            root: root.to_path_buf(),
            reserve,
            wal_allowance,
        }
    }

    /// The floor: `reserve + wal_allowance`.
    pub(crate) fn floor(&self) -> u64 {
        self.reserve.saturating_add(self.wal_allowance)
    }

    /// Available space now, `None` when unknown.
    pub(crate) fn available(&self) -> Option<u64> {
        self.probe.available(&self.root).ok()
    }

    /// Before a file of `len` bytes is copied: `available − len ≥ floor`.
    /// Unknown space passes (the per-file write errors still stop).
    pub(crate) fn file_fits(&self, len: u64) -> bool {
        self.available()
            .is_none_or(|a| a.saturating_sub(len) >= self.floor() && a >= len)
    }

    /// Before every write transaction (spec §12 guards 1 and 2): stop when
    /// available space is below the floor; otherwise set `max_page_count`
    /// to the current page count plus what the space above the floor
    /// allows, and read it back. SQLite never sets the limit below the
    /// current size, so a value read back other than the one requested
    /// also stops. Unknown space lifts the limit instead.
    pub(crate) fn before_write(&self, conn: &Connection) -> Result<(), GuardStop> {
        let sql = |e: rusqlite::Error| GuardStop::Sqlite(crate::error::primary_code(&e));
        let pages: i64 = conn
            .query_row("PRAGMA page_count", [], |r| r.get(0))
            .map_err(sql)?;
        let page_size: i64 = conn
            .query_row("PRAGMA page_size", [], |r| r.get(0))
            .map_err(sql)?;
        let room = match (crate::fault::max_page_room(), self.available()) {
            (Some(room), _) => Some(room),
            (None, Some(avail)) => {
                if avail < self.floor() {
                    return Err(GuardStop::Space);
                }
                let spare = avail.saturating_sub(self.floor());
                let page = u64::try_from(page_size.max(512)).unwrap_or(4096);
                Some(i64::try_from(spare.checked_div(page).unwrap_or(0)).unwrap_or(i64::MAX))
            }
            (None, None) => None,
        };
        let requested = match room {
            Some(r) => pages.saturating_add(r),
            None => i64::from(u32::MAX),
        };
        // The value is a computed integer, never input.
        let reported: i64 = conn
            .query_row(&format!("PRAGMA max_page_count = {requested}"), [], |r| {
                r.get(0)
            })
            .map_err(sql)?;
        if room.is_some() && (reported > requested || reported < requested.min(MAX_PAGE_COUNT_SAFE))
        {
            return Err(GuardStop::Space);
        }
        Ok(())
    }

    /// Lift the page limit (after an import), best effort.
    pub(crate) fn lift(conn: &Connection) {
        let _ = conn.query_row(&format!("PRAGMA max_page_count = {}", u32::MAX), [], |r| {
            r.get::<_, i64>(0)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reserve_is_clamped() {
        assert_eq!(reserve_for(None), RESERVE_MIN);
        assert_eq!(reserve_for(Some(0)), RESERVE_MIN);
        assert_eq!(reserve_for(Some(20 * GIB)), RESERVE_MIN);
        assert_eq!(reserve_for(Some(100 * GIB)), 5 * GIB);
        assert_eq!(reserve_for(Some(10_000 * GIB)), RESERVE_MAX);
        assert_eq!(reserve_for(Some(u64::MAX)), RESERVE_MAX);
    }

    #[test]
    fn migration_space_saturates() {
        assert_eq!(migration_needs(10, 1), 31);
        assert_eq!(migration_needs(u64::MAX, 1), u64::MAX);
        assert_eq!(migration_needs(1, u64::MAX), u64::MAX);
    }

    #[test]
    fn the_estimate_adds_its_parts_and_only_warns() {
        let e = SpaceEstimate::new(100, 50, 2, 30, 1000, 200, Some(1_500));
        // 3.0 × 100 + 50 + 30 + 70 + 200 + 1000.
        assert_eq!(e.needed, 1_650);
        assert!(e.warns() && !e.refuses());
        let low = SpaceEstimate::new(100, 50, 2, 30, 1000, 200, Some(1_199));
        assert!(low.refuses());
        let unknown = SpaceEstimate::new(100, 50, 2, 30, 1000, 200, None);
        assert!(!unknown.warns() && !unknown.refuses());
        let huge = SpaceEstimate::new(u64::MAX, u64::MAX, 1, 1, 1, 1, Some(0));
        assert_eq!(huge.needed, u64::MAX);
        assert_eq!(wal_allowance(32 << 20, 64 << 20), 128 << 20);
        assert_eq!(wal_allowance(u64::MAX, 1), u64::MAX);
    }

    #[test]
    fn the_real_probe_reads_the_temp_volume() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let avail = FsSpace.available(tmp.path()).expect("available");
        let total = FsSpace.total(tmp.path()).expect("total");
        assert!(total >= avail, "{total} < {avail}");
    }
}
