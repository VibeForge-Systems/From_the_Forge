//! Every cap the crate enforces, in one place.
//!
//! The values in [`Limits::default`] are the production values. Tests inject
//! smaller ones so that each cap can be exercised with small fixtures. Most of
//! the caps are provisional: real file sizes for very large backups are
//! **UNVERIFIED** (see `docs/supported-formats.md`).

/// Size, count and depth caps applied to everything read from a backup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    /// `Status.plist` size cap in bytes (1 MiB).
    pub status_plist_bytes: u64,
    /// `Manifest.plist` size cap in bytes (32 MiB).
    pub manifest_plist_bytes: u64,
    /// `Info.plist` size cap in bytes (64 MiB). Exceeding it is not fatal.
    pub info_plist_bytes: u64,
    /// Size cap for one MBFile record in `Manifest.db` (100 KiB).
    pub mbfile_bytes: u64,
    /// Maximum nesting depth of any plist (32).
    pub plist_depth: u32,
    /// Maximum entries in an MBFile `$objects` array (256).
    pub mbfile_objects: u32,
    /// `BackupKeyBag` size cap in bytes (64 KiB).
    pub keybag_bytes: u64,
    /// Maximum class records in a keybag (64).
    pub keybag_classes: u32,
    /// Maximum first-stage (PBKDF2-HMAC-SHA256) iterations (20,000,000).
    pub max_dpic: u32,
    /// Maximum second-stage (PBKDF2-HMAC-SHA1) iterations (1,000,000).
    pub max_iter: u32,
    /// `Manifest.db` size cap in bytes (4 GiB).
    pub manifest_db_bytes: u64,
    /// Maximum rows in the `Files` table (5,000,000).
    pub manifest_rows: u64,
    /// Directory entries read per scanned root (1024).
    pub root_entries: u32,
    /// Candidates inspected per root (256).
    pub backups_per_root: u32,
    /// Candidates inspected per refresh (512).
    pub list_backups_total: u32,
    /// Plist bytes read per refresh (256 MiB).
    pub list_plist_bytes_total: u64,
    /// Maximum password length in UTF-8 bytes (1024).
    pub password_bytes: usize,
    /// Cap on a staged database's source blob plus its `-wal` blob, in bytes
    /// (32 GiB), checked before anything is copied. Real sms.db sizes are
    /// **UNVERIFIED**; the cap protects the archive volume (D40).
    pub sms_db_bytes: u64,
}

/// The production value of [`Limits::sms_db_bytes`]: 32 GiB (D40).
pub const SMS_DB_BYTES_DEFAULT: u64 = 32 << 30;

impl Default for Limits {
    fn default() -> Self {
        Limits {
            status_plist_bytes: 1 << 20,
            manifest_plist_bytes: 32 << 20,
            info_plist_bytes: 64 << 20,
            mbfile_bytes: 100 << 10,
            plist_depth: 32,
            mbfile_objects: 256,
            keybag_bytes: 64 << 10,
            keybag_classes: 64,
            max_dpic: 20_000_000,
            max_iter: 1_000_000,
            manifest_db_bytes: 4 << 30,
            manifest_rows: 5_000_000,
            root_entries: 1024,
            backups_per_root: 256,
            list_backups_total: 512,
            list_plist_bytes_total: 256 << 20,
            password_bytes: 1024,
            sms_db_bytes: SMS_DB_BYTES_DEFAULT,
        }
    }
}

/// Which cap was exceeded. Carried by [`crate::BackupError::LimitExceeded`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LimitKind {
    /// Keybag `DPIC` outside `1..=max_dpic`.
    Dpic,
    /// Keybag `ITER` outside `1..=max_iter`.
    Iter,
    /// Keybag larger than `keybag_bytes`.
    KeybagBytes,
    /// More class records than `keybag_classes`.
    KeybagClasses,
    /// A plist's cumulative string and data bytes exceeded twice its size.
    PlistBytes,
    /// A plist produced more events than its size allows (a reference bomb).
    PlistEvents,
    /// A plist nested deeper than `plist_depth`.
    PlistDepth,
    /// An MBFile `$objects` array longer than `mbfile_objects`.
    MbFileObjects,
    /// `Manifest.db` larger than `manifest_db_bytes`.
    ManifestDbBytes,
    /// More `Files` rows than `manifest_rows`.
    ManifestRows,
    /// An MBFile record larger than `mbfile_bytes`.
    MbFileBytes,
    /// More directory entries than `root_entries`.
    RootEntries,
    /// A password longer than `password_bytes`.
    PasswordBytes,
    /// A staged database and its `-wal` together larger than `sms_db_bytes`.
    SmsDbBytes,
    /// A `message_summary_info` array longer than its cap. Reported by
    /// `oma-messages` through [`crate::bplist::ReadOpts::array_cap`].
    SummaryArray,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_documented_production_values() {
        let l = Limits::default();
        assert_eq!(l.status_plist_bytes, 1_048_576);
        assert_eq!(l.manifest_plist_bytes, 33_554_432);
        assert_eq!(l.info_plist_bytes, 67_108_864);
        assert_eq!(l.mbfile_bytes, 102_400);
        assert_eq!(l.plist_depth, 32);
        assert_eq!(l.mbfile_objects, 256);
        assert_eq!(l.keybag_bytes, 65_536);
        assert_eq!(l.keybag_classes, 64);
        assert_eq!(l.max_dpic, 20_000_000);
        assert_eq!(l.max_iter, 1_000_000);
        assert_eq!(l.manifest_db_bytes, 4_294_967_296);
        assert_eq!(l.manifest_rows, 5_000_000);
        assert_eq!(l.root_entries, 1024);
        assert_eq!(l.backups_per_root, 256);
        assert_eq!(l.list_backups_total, 512);
        assert_eq!(l.list_plist_bytes_total, 268_435_456);
        assert_eq!(l.password_bytes, 1024);
        assert_eq!(l.sms_db_bytes, 34_359_738_368);
    }
}
