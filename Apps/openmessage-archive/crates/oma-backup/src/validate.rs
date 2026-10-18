//! The validation report (§7.3, §7.4, §8).
//!
//! Counters hold numbers only: no relativePath, domain value or fileID of any
//! user file appears in the report. The only path ever named is the
//! well-known `HomeDomain/Library/SMS/sms.db`.
#![deny(clippy::arithmetic_side_effects)]

use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::cancel::{JobCtx, Phase};
use crate::cbc_stream::probe_first_and_last_block;
use crate::error::{BackupError, FileUnavailable, IoOp};
use crate::guard::{open_source_file, probe, BlobDirGuard, DirState, OpenFailure, Probe};
use crate::inspect::SQLITE_HEADER;
use crate::keybag::KeybagNote;
use crate::keys::unwrap_file_key;
use crate::manifest_db::{check_row, EntryKind, ManifestEntry, RowCheck};
use crate::open::{ChangeStamp, OpenBackup};
use crate::plists::BackupFile;

/// The well-known location of the Messages database.
pub const SMS_DB_DOMAIN: &str = "HomeDomain";
/// The well-known relative path of the Messages database.
pub const SMS_DB_PATH: &str = "Library/SMS/sms.db";
/// `SHA1("HomeDomain-Library/SMS/sms.db")`.
pub const SMS_DB_FILE_ID: &str = "3d0d7e5fb2ce288813306e4d4636395e047a3d28";

/// Cancellation is checked every this many rows.
const CANCEL_EVERY_ROWS: u64 = 256;
/// Progress is reported every this many rows.
const PROGRESS_EVERY_ROWS: u64 = 1024;

/// Which check (§8.1), in report order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CheckId {
    /// SnapshotState.
    Status,
    /// Backup format.
    Format,
    /// Unlocking.
    Encryption,
    /// Staging Manifest.db.
    ManifestCopy,
    /// `quick_check`.
    ManifestIntegrity,
    /// Table and column structure.
    ManifestSchema,
    /// MBFile records and storage classes.
    FileRecords,
    /// fileID shape and hash.
    FileIds,
    /// Blob presence and type.
    Blobs,
    /// Blob sizes against `Size` (informational).
    BlobSizes,
    /// Names that need adjusting when exported (informational).
    Paths,
    /// Class keys for the files (encrypted only).
    KeyClasses,
    /// sms.db, checked in place.
    MessagesDb,
    /// The backup did not change during the job.
    Unchanged,
}

impl CheckId {
    /// The stable kebab-case id used in the UI.
    pub fn as_str(self) -> &'static str {
        match self {
            CheckId::Status => "status",
            CheckId::Format => "format",
            CheckId::Encryption => "encryption",
            CheckId::ManifestCopy => "manifest-copy",
            CheckId::ManifestIntegrity => "manifest-integrity",
            CheckId::ManifestSchema => "manifest-schema",
            CheckId::FileRecords => "file-records",
            CheckId::FileIds => "file-ids",
            CheckId::Blobs => "blobs",
            CheckId::BlobSizes => "blob-sizes",
            CheckId::Paths => "paths",
            CheckId::KeyClasses => "key-classes",
            CheckId::MessagesDb => "messages-db",
            CheckId::Unchanged => "unchanged",
        }
    }
}

/// Severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CheckLevel {
    /// Passed (possibly with informational findings).
    Ok,
    /// Passed with problems that do not block import.
    Warning,
    /// Failed: the backup cannot be imported.
    Error,
}

/// A kind-specific observation attached to a check. Numbers and fixed
/// strings only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Finding {
    /// The backup is not encrypted.
    NotEncrypted,
    /// This many keybag notes were recorded.
    KeybagNotes(u32),
    /// A `-wal` was present and applied to the staged copy.
    WalApplied,
    /// A `-wal` was present and not applied.
    WalNotApplied,
    /// `quick_check` did not return `ok`.
    QuickCheckFailed,
    /// A schema problem (missing or hidden column, virtual or non-table `Files`).
    SchemaProblem(&'static str),
    /// `Files` has no rows.
    NoRows,
    /// No `Properties` table.
    PropertiesAbsent,
    /// Views in the schema (never executed).
    Views(u32),
    /// Triggers in the schema (never fired).
    Triggers(u32),
    /// MBFile records that did not decode.
    UnreadableRecords(u64),
    /// Rows with wrong storage classes, invalid UTF-8 or embedded NUL.
    InvalidRows(u64),
    /// fileIDs that are not 40 lowercase hex characters.
    InvalidFileIds(u64),
    /// fileIDs that do not match their domain and path.
    FileIdMismatches(u64),
    /// Blobs missing from the backup.
    MissingBlobs(u64),
    /// Blobs that are links, directories or devices (never read).
    UnsafeBlobs(u64),
    /// Blobs, or their `xx/` folders, that could not be examined (for
    /// example permission denied).
    UnreadableBlobs(u64),
    /// Blob sizes that differ from `Size`; this is routine.
    SizeMismatches(u64),
    /// The declared-bytes total saturated.
    DeclaredBytesSaturated,
    /// Names that will need adjusting when exported.
    PathsUnusualForExport(u64),
    /// Encrypted files with content but no EncryptionKey (or no record).
    FilesWithoutKey(u64),
    /// Encrypted files whose class key is unavailable.
    FilesWithUnavailableClass(u64),
    /// sms.db's class key is unavailable.
    SmsClassUnavailable,
    /// sms.db is not listed.
    SmsNotListed,
    /// sms.db is listed more than once.
    SmsAmbiguous,
    /// The sms.db row is invalid (wrong storage classes or not a file).
    SmsInvalidEntry,
    /// sms.db's blob is missing.
    SmsBlobMissing,
    /// sms.db's blob is a link, directory or device.
    SmsBlobNotRegular,
    /// sms.db has no usable EncryptionKey.
    SmsNoKey,
    /// sms.db's file key does not unwrap.
    SmsKeyDoesNotUnwrap,
    /// sms.db's first block is not the SQLite header.
    SmsHeaderNotSqlite,
    /// sms.db's last block has invalid padding, or its length is not a
    /// non-zero multiple of 16.
    SmsBadPadding,
    /// sms.db's size differs from `Size`.
    SmsSizeDiffers,
    /// The backup changed while it was being checked.
    BackupChanged,
}

/// One check result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// Which check.
    pub id: CheckId,
    /// Its severity.
    pub level: CheckLevel,
    /// What was found.
    pub findings: Vec<Finding>,
}

impl Check {
    fn new(id: CheckId) -> Self {
        Check {
            id,
            level: CheckLevel::Ok,
            findings: Vec::new(),
        }
    }

    fn raise(&mut self, level: CheckLevel, finding: Finding) {
        self.level = self.level.max(level);
        self.findings.push(finding);
    }
}

/// Numbers from the row pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ManifestStats {
    /// Every row in `Files`.
    pub total_rows: u64,
    /// `flags = 1`.
    pub files: u64,
    /// `flags = 2`.
    pub directories: u64,
    /// `flags = 4`.
    pub symlinks: u64,
    /// Any other flags.
    pub other_flags: u64,
    /// Sum of `Size` over file rows (saturating).
    pub declared_bytes: u64,
    /// `declared_bytes` saturated.
    pub declared_bytes_saturated: bool,
    /// Blobs not in the backup.
    pub missing_blobs: u64,
    /// Blob sizes that differ from `Size`.
    pub size_mismatches: u64,
    /// Blobs that are not plain files (never read).
    pub unsafe_blobs: u64,
    /// Blobs whose metadata, or whose folder's, could not be read.
    pub unreadable_blobs: u64,
    /// Rows with wrong storage classes.
    pub invalid_rows: u64,
    /// fileIDs that are not 40 lowercase hex characters.
    pub invalid_file_ids: u64,
    /// fileIDs that do not match their domain and path.
    pub file_id_mismatches: u64,
    /// Names that would need adjusting when exported.
    pub paths_unusual_for_export: u64,
    /// MBFile records that did not decode.
    pub unreadable_records: u64,
    /// Encrypted files with content and no key.
    pub files_without_key: u64,
    /// Encrypted files whose class key is unavailable.
    pub files_with_unavailable_class: u64,
    /// A `Properties` table exists.
    pub has_properties_table: bool,
    /// Views in the schema.
    pub views: u32,
    /// Triggers in the schema.
    pub triggers: u32,
}

/// sms.db, checked in place (D11).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SmsDbStatus {
    /// Exactly one valid row with sms.db's fileID.
    pub listed: bool,
    /// More than one row with that fileID.
    pub ambiguous: bool,
    /// The blob is present, when listed.
    pub blob_present: Option<bool>,
    /// `Size` from its record.
    pub declared_size: Option<u64>,
    /// The blob length matches `Size`.
    pub size_consistent: Option<bool>,
    /// The first plaintext block is `SQLite format 3\0`.
    pub header_ok: Option<bool>,
    /// Encrypted only: strict PKCS#7 on the last block.
    pub padding_ok: Option<bool>,
    /// `Library/SMS/sms.db-wal` is listed.
    pub wal_listed: bool,
    /// `Library/SMS/sms.db-shm` is listed.
    pub shm_listed: bool,
}

/// The result of [`OpenBackup::validate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationReport {
    /// Ordered checks.
    pub checks: Vec<Check>,
    /// Row-pass numbers.
    pub stats: ManifestStats,
    /// sms.db.
    pub sms_db: SmsDbStatus,
    /// Keybag notes (encrypted backups).
    pub keybag_notes: Vec<KeybagNote>,
    /// How long validation took.
    pub elapsed: Duration,
}

impl ValidationReport {
    /// `true` when no check is at error level: M3 should be able to extract.
    pub fn importable(&self) -> bool {
        self.checks.iter().all(|c| c.level != CheckLevel::Error)
    }

    /// The check with this id.
    pub fn check(&self, id: CheckId) -> Option<&Check> {
        self.checks.iter().find(|c| c.id == id)
    }
}

/// What a failure to unwrap sms.db's file key means for the report: the
/// finding, and whether sms.db's class key is unavailable (a `key-classes`
/// error). A keybag problem with the class (for example a `WPKY` of the wrong
/// length) is the class being unavailable, not the file key failing to
/// unwrap. Errors that are not about the key are passed on.
fn sms_key_failure(e: BackupError) -> Result<(Finding, bool), BackupError> {
    match e {
        BackupError::FileUnavailable(FileUnavailable::ClassKeyUnavailable)
        | BackupError::Unsupported(_)
        | BackupError::Malformed {
            file: BackupFile::Keybag,
            ..
        } => Ok((Finding::SmsNoKey, true)),
        BackupError::Malformed { .. } => Ok((Finding::SmsKeyDoesNotUnwrap, false)),
        e => Err(e),
    }
}

/// The ciphertext length for a plaintext of `size` bytes with PKCS#7:
/// `(size / 16 + 1) × 16`. `None` on overflow.
pub(crate) fn padded_len(size: u64) -> Option<u64> {
    size.checked_div(16)?.checked_add(1)?.checked_mul(16)
}

fn inc(n: &mut u64) {
    *n = n.saturating_add(1);
}

impl OpenBackup {
    /// Validate the staged index and check sms.db in place (§7.3–§8).
    pub fn validate(&self, ctx: &mut JobCtx<'_>) -> Result<ValidationReport, BackupError> {
        let started = Instant::now();
        let db = self.db()?;
        let mut checks = Vec::new();
        let mut stats = ManifestStats::default();
        let mut sms = SmsDbStatus::default();

        checks.push(Check::new(CheckId::Status));
        checks.push(Check::new(CheckId::Format));

        let mut encryption = Check::new(CheckId::Encryption);
        let keybag_notes = self.keybag_notes().to_vec();
        if self.keys.is_none() {
            encryption.findings.push(Finding::NotEncrypted);
        } else if !keybag_notes.is_empty() {
            encryption.raise(
                CheckLevel::Warning,
                Finding::KeybagNotes(u32::try_from(keybag_notes.len()).unwrap_or(u32::MAX)),
            );
        }
        checks.push(encryption);

        let mut copy = Check::new(CheckId::ManifestCopy);
        if self.copy.wal_applied {
            copy.findings.push(Finding::WalApplied);
        } else if self.copy.wal_present {
            let level = if self.copy.header_wal_mode {
                CheckLevel::Warning
            } else {
                CheckLevel::Ok
            };
            copy.raise(level, Finding::WalNotApplied);
        }
        checks.push(copy);

        // Structure.
        ctx.check()?;
        ctx.report(Phase::CheckingDatabase, 0, None);
        let mut integrity = Check::new(CheckId::ManifestIntegrity);
        let intact = db.quick_check()?;
        if !intact {
            integrity.raise(CheckLevel::Error, Finding::QuickCheckFailed);
        }
        checks.push(integrity);

        let mut proceed = intact;
        if proceed {
            ctx.check()?;
            let facts = db.check_schema(&self.limits)?;
            stats.has_properties_table = facts.has_properties_table;
            stats.views = facts.views;
            stats.triggers = facts.triggers;
            let mut schema = Check::new(CheckId::ManifestSchema);
            match facts.problem {
                Some(crate::error::MalformedDetail::Empty) => {
                    schema.raise(CheckLevel::Error, Finding::NoRows);
                    proceed = false;
                }
                Some(crate::error::MalformedDetail::Schema(what)) => {
                    schema.raise(CheckLevel::Error, Finding::SchemaProblem(what));
                    proceed = false;
                }
                Some(_) => {
                    schema.raise(CheckLevel::Error, Finding::SchemaProblem("unexpected"));
                    proceed = false;
                }
                None => {}
            }
            if !facts.has_properties_table {
                schema.findings.push(Finding::PropertiesAbsent);
            }
            if facts.views > 0 {
                schema.findings.push(Finding::Views(facts.views));
            }
            if facts.triggers > 0 {
                schema.findings.push(Finding::Triggers(facts.triggers));
            }
            checks.push(schema);

            if proceed {
                self.row_pass(facts.rows, &mut stats, ctx)?;
                checks.extend(stats_checks(&stats, self.keys.is_some()));
                let mut messages = Check::new(CheckId::MessagesDb);
                let sms_class_unavailable = self.check_sms(&mut sms, &mut messages, ctx)?;
                if let Some(key_classes) = checks.iter_mut().find(|c| c.id == CheckId::KeyClasses) {
                    if sms_class_unavailable {
                        key_classes.raise(CheckLevel::Error, Finding::SmsClassUnavailable);
                    }
                }
                checks.push(messages);
            }
        }

        let mut unchanged = Check::new(CheckId::Unchanged);
        if ChangeStamp::take(self.inspection.dir()) != self.before {
            unchanged.raise(CheckLevel::Error, Finding::BackupChanged);
        }
        checks.push(unchanged);

        let report = ValidationReport {
            checks,
            stats,
            sms_db: sms,
            keybag_notes,
            elapsed: started.elapsed(),
        };
        tracing::info!(
            backup_id = self.inspection.id.as_str(),
            importable = report.importable(),
            rows = report.stats.total_rows,
            "validation finished"
        );
        Ok(report)
    }

    /// The row pass (§7.4), progress in rows.
    fn row_pass(
        &self,
        total: u64,
        stats: &mut ManifestStats,
        ctx: &mut JobCtx<'_>,
    ) -> Result<(), BackupError> {
        let db = self.db()?;
        let root = self.inspection.dir().to_path_buf();
        let mut guard = BlobDirGuard::new(root.clone());
        let encrypted = self.keys.as_ref();
        ctx.report(Phase::CheckingFiles, 0, Some(total));
        let mut seen: u64 = 0;
        db.for_each_row(|row| {
            seen = seen.saturating_add(1);
            if seen.is_multiple_of(CANCEL_EVERY_ROWS) {
                ctx.check()?;
            }
            if seen.is_multiple_of(PROGRESS_EVERY_ROWS) {
                ctx.report(Phase::CheckingFiles, seen, Some(total));
            }
            inc(&mut stats.total_rows);
            let (entry, id_shape_ok, id_matches, record_unreadable) =
                match check_row(row, &self.limits) {
                    RowCheck::InvalidRow => {
                        inc(&mut stats.invalid_rows);
                        return Ok(());
                    }
                    RowCheck::Parsed {
                        entry,
                        id_shape_ok,
                        id_matches,
                        record_unreadable,
                    } => (entry, id_shape_ok, id_matches, record_unreadable),
                };
            match entry.kind {
                EntryKind::File => inc(&mut stats.files),
                EntryKind::Directory => inc(&mut stats.directories),
                EntryKind::Symlink => inc(&mut stats.symlinks),
                EntryKind::Other(_) => inc(&mut stats.other_flags),
            }
            if !id_shape_ok {
                inc(&mut stats.invalid_file_ids);
            } else if !id_matches {
                inc(&mut stats.file_id_mismatches);
            }
            if record_unreadable {
                inc(&mut stats.unreadable_records);
            }
            // relativePath is a lookup key, not a path (D19): unusual names
            // are counted, never refused.
            if !entry.relative_path.is_empty()
                && oma_fs::resolve_within(Path::new("x"), &entry.relative_path).is_err()
            {
                inc(&mut stats.paths_unusual_for_export);
            }
            if entry.kind != EntryKind::File {
                return Ok(());
            }
            let size = entry.record.as_ref().and_then(|r| r.size);
            if let Some(s) = size {
                match stats.declared_bytes.checked_add(s) {
                    Some(t) => stats.declared_bytes = t,
                    None => {
                        stats.declared_bytes = u64::MAX;
                        stats.declared_bytes_saturated = true;
                    }
                }
            }
            if let Some(keys) = encrypted {
                match &entry.record {
                    None => inc(&mut stats.files_without_key),
                    Some(r) => {
                        if r.wrapped_key().is_none() {
                            if size.unwrap_or(0) > 0 {
                                inc(&mut stats.files_without_key);
                            }
                        } else if !r.protection_class.is_some_and(|c| keys.class_keys.has(c)) {
                            inc(&mut stats.files_with_unavailable_class);
                        }
                    }
                }
            }
            if !id_shape_ok {
                return Ok(());
            }
            // One unreadable blob or folder is counted, not fatal: the rest
            // of the report still matters. sms.db itself is checked by
            // `check_sms`, where an I/O error stays an error.
            let state = match self.blob_state(&root, &mut guard, &entry.file_id) {
                Ok(state) => state,
                Err(BackupError::Io { .. }) => {
                    inc(&mut stats.unreadable_blobs);
                    return Ok(());
                }
                Err(e) => return Err(e),
            };
            match state {
                BlobState::Missing => inc(&mut stats.missing_blobs),
                BlobState::Unsafe => inc(&mut stats.unsafe_blobs),
                BlobState::Present(len) => {
                    if let Some(s) = size {
                        let expected = if encrypted.is_some() {
                            padded_len(s)
                        } else {
                            Some(s)
                        };
                        if expected != Some(len) {
                            inc(&mut stats.size_mismatches);
                        }
                    }
                }
            }
            Ok(())
        })?;
        ctx.report(Phase::CheckingFiles, seen, Some(total));
        Ok(())
    }

    /// The guards of §7.4 for one blob.
    pub(crate) fn blob_state(
        &self,
        root: &Path,
        guard: &mut BlobDirGuard,
        file_id: &str,
    ) -> Result<BlobState, BackupError> {
        let prefix = file_id
            .get(..2)
            .ok_or(BackupError::Internal("fileID prefix"))?;
        let dir = oma_fs::resolve_within(root, prefix)
            .map_err(|_| BackupError::Internal("blob directory path"))?;
        let blob = blob_path(root, file_id)?;
        match guard
            .check(prefix, &dir)
            .map_err(|e| BackupError::source_io(IoOp::Metadata, &e))?
        {
            DirState::Missing => return Ok(BlobState::Missing),
            DirState::Unsafe => return Ok(BlobState::Unsafe),
            DirState::Ok => {}
        }
        Ok(
            match probe(&blob).map_err(|e| BackupError::source_io(IoOp::Metadata, &e))? {
                Probe::Missing => BlobState::Missing,
                Probe::NotRegular => BlobState::Unsafe,
                Probe::Regular { len } => BlobState::Present(len),
            },
        )
    }

    /// The `messages-db` check (D11). Returns whether sms.db's class key is
    /// unavailable (a `key-classes` error).
    fn check_sms(
        &self,
        sms: &mut SmsDbStatus,
        check: &mut Check,
        ctx: &mut JobCtx<'_>,
    ) -> Result<bool, BackupError> {
        ctx.check()?;
        ctx.report(Phase::CheckingMessagesDb, 0, None);
        sms.wal_listed = self.is_listed(SMS_DB_DOMAIN, "Library/SMS/sms.db-wal")?;
        sms.shm_listed = self.is_listed(SMS_DB_DOMAIN, "Library/SMS/sms.db-shm")?;

        let entry = match self.find_file(SMS_DB_DOMAIN, SMS_DB_PATH) {
            Ok(Some(e)) => e,
            Ok(None) => {
                check.raise(CheckLevel::Error, Finding::SmsNotListed);
                return Ok(false);
            }
            Err(BackupError::FileUnavailable(FileUnavailable::Ambiguous)) => {
                sms.ambiguous = true;
                check.raise(CheckLevel::Error, Finding::SmsAmbiguous);
                return Ok(false);
            }
            Err(BackupError::FileUnavailable(FileUnavailable::InvalidEntry)) => {
                check.raise(CheckLevel::Error, Finding::SmsInvalidEntry);
                return Ok(false);
            }
            Err(e) => return Err(e),
        };
        sms.listed = true;
        sms.declared_size = entry.record.as_ref().and_then(|r| r.size);
        if entry.kind != EntryKind::File {
            check.raise(CheckLevel::Error, Finding::SmsInvalidEntry);
            return Ok(false);
        }
        let root = self.inspection.dir().to_path_buf();
        let mut guard = BlobDirGuard::new(root.clone());
        let len = match self.blob_state(&root, &mut guard, &entry.file_id)? {
            BlobState::Missing => {
                sms.blob_present = Some(false);
                check.raise(CheckLevel::Error, Finding::SmsBlobMissing);
                return Ok(false);
            }
            BlobState::Unsafe => {
                sms.blob_present = Some(true);
                check.raise(CheckLevel::Error, Finding::SmsBlobNotRegular);
                return Ok(false);
            }
            BlobState::Present(len) => len,
        };
        sms.blob_present = Some(true);
        let (mut file, len_now) = match open_source_file(&blob_path(&root, &entry.file_id)?) {
            Ok(f) => f,
            Err(OpenFailure::Missing) => {
                sms.blob_present = Some(false);
                check.raise(CheckLevel::Error, Finding::SmsBlobMissing);
                return Ok(false);
            }
            Err(OpenFailure::NotRegular) => {
                check.raise(CheckLevel::Error, Finding::SmsBlobNotRegular);
                return Ok(false);
            }
            Err(OpenFailure::Io(e)) => return Err(BackupError::source_io(IoOp::Open, &e)),
        };
        let len = len.min(len_now);

        match &self.keys {
            None => {
                let mut header = [0u8; 16];
                let ok = match file.read_exact(&mut header) {
                    Ok(()) => &header == SQLITE_HEADER,
                    Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => false,
                    Err(e) => return Err(BackupError::source_io(IoOp::Read, &e)),
                };
                sms.header_ok = Some(ok);
                sms.size_consistent = sms.declared_size.map(|s| s == len);
            }
            Some(keys) => {
                sms.size_consistent = sms.declared_size.map(|s| padded_len(s) == Some(len));
                let record = entry.record.as_ref();
                let wrapped = record.and_then(|r| r.wrapped_key());
                let class = record.and_then(|r| r.protection_class);
                let (Some(wrapped), Some(class)) = (wrapped, class) else {
                    check.raise(CheckLevel::Error, Finding::SmsNoKey);
                    return Ok(false);
                };
                let file_key = match unwrap_file_key(&keys.class_keys, &keys.keybag, class, wrapped)
                {
                    Ok(k) => k,
                    Err(e) => {
                        let (finding, class_unavailable) = sms_key_failure(e)?;
                        check.raise(CheckLevel::Error, finding);
                        return Ok(class_unavailable);
                    }
                };
                if len == 0 || !len.is_multiple_of(16) {
                    sms.header_ok = Some(false);
                    sms.padding_ok = Some(false);
                } else {
                    match probe_first_and_last_block(&file_key, &mut file, len) {
                        Ok(p) => {
                            sms.header_ok = Some(&*p.first_block == SQLITE_HEADER);
                            sms.padding_ok = Some(p.padding_ok);
                        }
                        Err(BackupError::Malformed { .. }) => {
                            sms.header_ok = Some(false);
                            sms.padding_ok = Some(false);
                        }
                        Err(e) => return Err(e),
                    }
                }
            }
        }
        if sms.header_ok == Some(false) {
            check.raise(CheckLevel::Error, Finding::SmsHeaderNotSqlite);
        }
        if sms.padding_ok == Some(false) {
            check.raise(CheckLevel::Error, Finding::SmsBadPadding);
        }
        if sms.size_consistent == Some(false) {
            check.findings.push(Finding::SmsSizeDiffers);
        }
        Ok(false)
    }

    fn is_listed(&self, domain: &str, path: &str) -> Result<bool, BackupError> {
        match self.find_file(domain, path) {
            Ok(found) => Ok(found.is_some()),
            Err(BackupError::FileUnavailable(_)) => Ok(true),
            Err(e) => Err(e),
        }
    }
}

/// `<root>/<xx>/<fileID>`, through `resolve_within`. The fileID must already
/// be hex-checked, so this cannot fail in practice.
pub(crate) fn blob_path(root: &Path, file_id: &str) -> Result<std::path::PathBuf, BackupError> {
    let prefix = file_id
        .get(..2)
        .ok_or(BackupError::Internal("fileID prefix"))?;
    oma_fs::resolve_within(root, &format!("{prefix}/{file_id}"))
        .map_err(|_| BackupError::Internal("blob path"))
}

/// What a blob is, after the guards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BlobState {
    Missing,
    Unsafe,
    Present(u64),
}

/// The checks computed from the row-pass numbers.
fn stats_checks(s: &ManifestStats, encrypted: bool) -> Vec<Check> {
    let mut records = Check::new(CheckId::FileRecords);
    if s.unreadable_records > 0 {
        records.raise(
            CheckLevel::Warning,
            Finding::UnreadableRecords(s.unreadable_records),
        );
    }
    if s.invalid_rows > 0 {
        records.raise(CheckLevel::Warning, Finding::InvalidRows(s.invalid_rows));
    }
    let mut ids = Check::new(CheckId::FileIds);
    if s.invalid_file_ids > 0 {
        ids.raise(
            CheckLevel::Warning,
            Finding::InvalidFileIds(s.invalid_file_ids),
        );
    }
    if s.file_id_mismatches > 0 {
        ids.raise(
            CheckLevel::Warning,
            Finding::FileIdMismatches(s.file_id_mismatches),
        );
    }
    let mut blobs = Check::new(CheckId::Blobs);
    if s.missing_blobs > 0 {
        blobs.raise(CheckLevel::Warning, Finding::MissingBlobs(s.missing_blobs));
    }
    if s.unsafe_blobs > 0 {
        blobs.raise(CheckLevel::Warning, Finding::UnsafeBlobs(s.unsafe_blobs));
    }
    if s.unreadable_blobs > 0 {
        blobs.raise(
            CheckLevel::Warning,
            Finding::UnreadableBlobs(s.unreadable_blobs),
        );
    }
    let mut sizes = Check::new(CheckId::BlobSizes);
    if s.size_mismatches > 0 {
        sizes
            .findings
            .push(Finding::SizeMismatches(s.size_mismatches));
    }
    if s.declared_bytes_saturated {
        sizes.findings.push(Finding::DeclaredBytesSaturated);
    }
    let mut paths = Check::new(CheckId::Paths);
    if s.paths_unusual_for_export > 0 {
        paths
            .findings
            .push(Finding::PathsUnusualForExport(s.paths_unusual_for_export));
    }
    let mut keys = Check::new(CheckId::KeyClasses);
    if encrypted {
        if s.files_without_key > 0 {
            keys.raise(
                CheckLevel::Warning,
                Finding::FilesWithoutKey(s.files_without_key),
            );
        }
        if s.files_with_unavailable_class > 0 {
            keys.raise(
                CheckLevel::Warning,
                Finding::FilesWithUnavailableClass(s.files_with_unavailable_class),
            );
        }
    } else {
        keys.findings.push(Finding::NotEncrypted);
    }
    vec![records, ids, blobs, sizes, paths, keys]
}

/// Used by extraction too.
pub(crate) fn entry_is_consistent(entry: &ManifestEntry) -> bool {
    crate::manifest_db::is_file_id_shape(&entry.file_id)
        && crate::manifest_db::file_id_for(&entry.domain, &entry.relative_path) == entry.file_id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padded_lengths() {
        assert_eq!(padded_len(0), Some(16));
        assert_eq!(padded_len(15), Some(16));
        assert_eq!(padded_len(16), Some(32));
        assert_eq!(padded_len(100), Some(112));
        assert_eq!(padded_len(u64::MAX), None);
    }

    #[test]
    fn a_keybag_problem_with_the_sms_class_is_the_class_being_unavailable() {
        use crate::error::{MalformedDetail, UnsupportedReason};
        // A WPKY of the wrong length on sms.db's class (keys.rs class_key).
        let bad_wpky = BackupError::malformed(
            BackupFile::Keybag,
            MalformedDetail::BadLength { tag: *b"WPKY" },
        );
        assert!(matches!(
            sms_key_failure(bad_wpky),
            Ok((Finding::SmsNoKey, true))
        ));
        assert!(matches!(
            sms_key_failure(BackupError::FileUnavailable(
                FileUnavailable::ClassKeyUnavailable
            )),
            Ok((Finding::SmsNoKey, true))
        ));
        assert!(matches!(
            sms_key_failure(BackupError::Unsupported(UnsupportedReason::KeyType(1))),
            Ok((Finding::SmsNoKey, true))
        ));
        // The per-file key itself failing stays SmsKeyDoesNotUnwrap.
        let blob = BackupError::malformed(
            BackupFile::Blob,
            MalformedDetail::Inconsistent("file key does not unwrap"),
        );
        assert!(matches!(
            sms_key_failure(blob),
            Ok((Finding::SmsKeyDoesNotUnwrap, false))
        ));
        assert!(sms_key_failure(BackupError::WrongPassword).is_err());
    }

    #[test]
    fn check_ids_are_kebab_case() {
        assert_eq!(CheckId::ManifestCopy.as_str(), "manifest-copy");
        assert_eq!(CheckId::MessagesDb.as_str(), "messages-db");
    }

    #[test]
    fn importable_means_no_error() {
        let mut r = ValidationReport {
            checks: vec![Check::new(CheckId::Status)],
            stats: ManifestStats::default(),
            sms_db: SmsDbStatus::default(),
            keybag_notes: vec![],
            elapsed: Duration::ZERO,
        };
        assert!(r.importable());
        if let Some(c) = r.checks.first_mut() {
            c.raise(CheckLevel::Warning, Finding::NoRows);
        }
        assert!(r.importable());
        if let Some(c) = r.checks.first_mut() {
            c.raise(CheckLevel::Error, Finding::NoRows);
        }
        assert!(!r.importable());
    }
}
