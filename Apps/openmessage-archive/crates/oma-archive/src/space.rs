//! Free-space probes and the space rules M4 has so far (spec §12, §5.2,
//! §7.2).
//!
//! Space comes through [`SpaceProbe`], real in production ([`FsSpace`],
//! through `oma-fs` and `fs4`) and fake in tests. An error from a probe
//! means "unknown", never zero.
#![deny(clippy::arithmetic_side_effects)]

use std::io;
use std::path::Path;

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
    fn the_real_probe_reads_the_temp_volume() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let avail = FsSpace.available(tmp.path()).expect("available");
        let total = FsSpace.total(tmp.path()).expect("total");
        assert!(total >= avail, "{total} < {avail}");
    }
}
