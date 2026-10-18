//! Every IPC type of the Backups page (D1: DTOs live in the shell, so
//! `oma-backup` stays free of ts-rs and the shell is the redaction boundary).
//!
//! Integers: `TS_RS_LARGE_INT = "number"` makes every `u64` a TypeScript
//! `number`, so every value is clamped to 2^53 − 1 before it enters a DTO
//! ([`clamp`], [`safe_opt`]). Strings that came from a backup arrive here as
//! `oma_backup::DisplayText` and are already bounded and sanitised.

use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use oma_backup::Zeroizing;
use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::extraction::{ExtractionOutcomeDto, ExtractionReportDto};
use crate::archives::dto::{
    GateEndingDto, ImportOutcomeDto, NotImportableDto, StopReasonDto, VerifyOutcomeDto,
};
use crate::error::IpcError;
use crate::settings::CheckLevel;

/// `Number.MAX_SAFE_INTEGER`.
pub const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

/// A counter for a DTO: values above 2^53 − 1 become `MAX_SAFE_INTEGER`, and
/// the second value says whether that happened.
pub fn clamp(v: u64) -> (u64, bool) {
    if v > MAX_SAFE_INTEGER {
        (MAX_SAFE_INTEGER, true)
    } else {
        (v, false)
    }
}

/// An optional value for a DTO: values above 2^53 − 1 become `None`.
pub fn safe_opt(v: Option<u64>) -> Option<u64> {
    v.filter(|v| *v <= MAX_SAFE_INTEGER)
}

/// RFC 3339 UTC (`YYYY-MM-DDTHH:MM:SSZ`), computed without `plist::Date`'s
/// formatter (which may panic on values it did not parse) and without a
/// date crate. Uses the days-from-civil inverse for the proleptic Gregorian
/// calendar.
pub fn rfc3339(t: SystemTime) -> String {
    let secs: i64 = match t.duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
        Err(e) => i64::try_from(e.duration().as_secs())
            .map(|s| -s)
            .unwrap_or(i64::MIN),
    };
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days.saturating_add(719_468);
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe.saturating_add(era.saturating_mul(400)) + i64::from(mo <= 2);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Milliseconds, clamped.
pub fn millis(d: Duration) -> u64 {
    clamp(u64::try_from(d.as_millis()).unwrap_or(u64::MAX)).0
}

// ---------------------------------------------------------------- listing

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct BackupList {
    pub locations: Vec<BackupLocation>,
    pub backups: Vec<BackupSummary>,
    /// Some backups were not inspected because the per-refresh budget ran out.
    pub budget_exhausted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum BackupLocationKind {
    /// `<home>\Apple\MobileSync\Backup` (Windows).
    AppleDevices,
    /// `<roaming>\Apple Computer\MobileSync\Backup` (Windows).
    ItunesDesktop,
    /// `<roaming>\iMazing\Backups` (Windows): iMazing's default location.
    IMazing,
    /// A folder the user added that contains backups.
    AddedParent,
    /// A backup folder the user added.
    AddedBackup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum BackupLocationState {
    Present,
    Absent,
    Unreadable,
    TooManyEntries,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct BackupLocation {
    pub id: String,
    pub kind: BackupLocationKind,
    /// A folder the user chose or a standard location; never a backup's own
    /// folder unless the user added that backup directly.
    pub path: String,
    pub via_link: bool,
    pub state: BackupLocationState,
    pub backup_count: u64,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum BackupState {
    Ready,
    NeedsPassword,
    Incomplete,
    Unsupported,
    Unreadable,
    NotInspected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum SourceFile {
    InfoPlist,
    StatusPlist,
    ManifestPlist,
    ManifestPlistLockdown,
}

/// A value read from the backup, with the file it came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SourcedText {
    pub value: String,
    pub source: SourceFile,
}

/// One backup in the list. It has no path: the list shows device and
/// location only (D14).
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct BackupSummary {
    pub id: String,
    pub location_id: String,
    pub via_link: bool,
    pub device_name: Option<SourcedText>,
    pub product_type: Option<SourcedText>,
    pub product_name: Option<SourcedText>,
    pub ios_version: Option<SourcedText>,
    /// RFC 3339 UTC, from Info.plist only.
    pub last_backup_date: Option<String>,
    pub encrypted: Option<bool>,
    pub state: BackupState,
    pub state_reason: String,
    pub active_job_id: Option<String>,
    pub last_report_importable: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum AddFolderOutcome {
    Added,
    AddedAncestor,
    AlreadyListed,
    Refused,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AddBackupFolderResult {
    pub outcome: AddFolderOutcome,
    /// User-facing; includes the refusal reason.
    pub message: String,
    /// False when settings are read-only this session.
    pub saved: bool,
    pub list: BackupList,
}

/// The result of `forget_backup_folder`.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ForgetBackupFolderResult {
    /// False when settings are read-only this session: the folder is
    /// forgotten until the app closes, but the change was not saved.
    pub saved: bool,
    pub list: BackupList,
}

// ---------------------------------------------------------------- details

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct BackupDetails {
    pub summary: BackupSummary,
    /// The backup folder. Its name is usually the device's identifier
    /// (docs/privacy.md). Shown in the details panel only; never logged.
    pub path: String,
    pub device: DeviceDetails,
    pub status: StatusDetails,
    pub encryption: EncryptionDetails,
    pub layout: LayoutDetails,
    /// Parse, layout and location notes, as checks.
    pub notes: Vec<ValidationCheck>,
    /// Set when the archive folder looks synchronized; drives the warning
    /// shown before every validation (D13).
    pub archive_sync_provider: Option<String>,
    pub archive_configured: bool,
    pub last_report: Option<ValidationReportDto>,
    /// The last extraction check's report, from the same job as
    /// `lastReport`. A later validation clears it (§16.1).
    pub last_extraction_report: Option<ExtractionReportDto>,
    /// The backup's iOS version is not one the extraction check has been
    /// tested on with a real backup (none are, in M3).
    pub ios_version_untested: bool,
    /// From the last finished, failed or cancelled job.
    pub last_cleanup: Option<CleanupDto>,
    pub active_job: Option<JobSummary>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DeviceDetails {
    pub device_name: Option<SourcedText>,
    pub display_name: Option<SourcedText>,
    pub product_type: Option<SourcedText>,
    pub product_name: Option<SourcedText>,
    pub ios_version: Option<SourcedText>,
    pub build_version: Option<SourcedText>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct StatusDetails {
    /// The sanitised raw SnapshotState, for display only.
    pub snapshot_state: Option<String>,
    pub is_full_backup: Option<bool>,
    pub format_version: Option<String>,
    pub date: Option<String>,
    pub backup_state: Option<String>,
    pub snapshot_folder_present: bool,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct EncryptionDetails {
    pub is_encrypted: Option<bool>,
    pub has_keybag: bool,
    pub has_manifest_key: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum BackupFormat {
    /// Format 3.x (`Manifest.db`).
    V3,
    /// Format 2.4 (`Manifest.mbdb`).
    LegacyMbdb,
    Unknown,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct LayoutDetails {
    pub format: BackupFormat,
    pub manifest_db_wal_present: bool,
    pub manifest_db_shm_present: bool,
    pub non_regular_markers: u64,
}

/// One check or note. Reuses M1's `CheckLevel` (ok | warning | error).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ValidationCheck {
    pub id: String,
    pub level: CheckLevel,
    pub message: String,
}

/// Every `ManifestStats` field. Counters above 2^53 − 1 are clamped and set
/// `saturated`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ManifestStatsDto {
    pub total_rows: u64,
    pub files: u64,
    pub directories: u64,
    pub symlinks: u64,
    pub other_flags: u64,
    pub declared_bytes: u64,
    pub declared_bytes_saturated: bool,
    pub missing_blobs: u64,
    pub size_mismatches: u64,
    pub unsafe_blobs: u64,
    pub unreadable_blobs: u64,
    pub invalid_rows: u64,
    pub invalid_file_ids: u64,
    pub file_id_mismatches: u64,
    pub paths_unusual_for_export: u64,
    pub unreadable_records: u64,
    pub files_without_key: u64,
    pub files_with_unavailable_class: u64,
    pub has_properties_table: bool,
    pub views: u32,
    pub triggers: u32,
    /// At least one counter was clamped to `Number.MAX_SAFE_INTEGER`.
    pub saturated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SmsDbStatusDto {
    pub listed: bool,
    pub ambiguous: bool,
    pub blob_present: Option<bool>,
    pub declared_size: Option<u64>,
    pub size_consistent: Option<bool>,
    pub header_ok: Option<bool>,
    pub padding_ok: Option<bool>,
    pub wal_listed: bool,
    pub shm_listed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ValidationReportDto {
    /// No check is at error level.
    pub importable: bool,
    pub checks: Vec<ValidationCheck>,
    pub stats: ManifestStatsDto,
    pub sms_db: SmsDbStatusDto,
    pub duration_ms: u64,
    /// RFC 3339 UTC.
    pub validated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CleanupDto {
    pub staging_removed: bool,
}

// ---------------------------------------------------------------- jobs

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum JobPhase {
    CopyingManifest,
    DerivingKey,
    UnwrappingKeys,
    DecryptingManifest,
    CheckingDatabase,
    CheckingFiles,
    CheckingMessagesDb,
    /// Extracting a file (the M3 extraction API). Validation never reports
    /// it; it exists so the crate's phase set maps one to one.
    Extracting,
    /// P6 of an import (M4): the attachment rows and their files. `done`
    /// and `total` count rows, not bytes (see `docs/architecture.md`).
    CopyingAttachments,
    // The extraction check (M3), one to one with `MessagesPhase`.
    StagingMessagesDb,
    ScanningSchema,
    CheckingIntegrity,
    ReadingSchema,
    ReadingHandles,
    ReadingChats,
    ReadingChatLinks,
    ReadingMessages,
    CheckingAttachments,
    Summarising,
    // The import (M4 §8.12); `stagingMessagesDb`, `readingSchema` and
    // `copyingAttachments` above are shared.
    ScanningRecentlyDeleted,
    ScanningKeys,
    EstimatingAttachments,
    /// Waiting at the gate for the user's answer.
    AwaitingConfirmation,
    PreparingArchive,
    ImportingHandles,
    ImportingChats,
    ImportingMessages,
    ImportingChatLinks,
    ImportingAttachmentLinks,
    ImportingRecentlyDeleted,
    Finalizing,
    OptimizingIndex,
    Checkpointing,
    // The verify (M4 §11).
    VerifyingDatabase,
    VerifyingIndex,
    /// Rows in a quick verify, bytes re-hashed in a full one.
    VerifyingFiles,
}

impl From<oma_messages::MessagesPhase> for JobPhase {
    fn from(p: oma_messages::MessagesPhase) -> Self {
        use oma_messages::MessagesPhase as P;
        match p {
            P::StagingMessagesDb => JobPhase::StagingMessagesDb,
            P::ScanningSchema => JobPhase::ScanningSchema,
            P::CheckingIntegrity => JobPhase::CheckingIntegrity,
            P::ReadingSchema => JobPhase::ReadingSchema,
            P::ReadingHandles => JobPhase::ReadingHandles,
            P::ReadingChats => JobPhase::ReadingChats,
            P::ReadingChatLinks => JobPhase::ReadingChatLinks,
            P::ReadingMessages => JobPhase::ReadingMessages,
            P::CheckingAttachments => JobPhase::CheckingAttachments,
            // `MessagesPhase` is non-exhaustive; a phase added later shows as
            // the last one until it has its own label.
            _ => JobPhase::Summarising,
        }
    }
}

/// What a job does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum JobKind {
    /// Validate the backup (M2).
    Validate,
    /// Validate, then check that the Messages database can be read (M3).
    ExtractionCheck,
    /// Import a backup into an archive (M4).
    Import,
    /// Verify an archive (M4).
    Verify,
}

impl From<oma_backup::Phase> for JobPhase {
    fn from(p: oma_backup::Phase) -> Self {
        use oma_backup::Phase as P;
        match p {
            P::CopyingManifest => JobPhase::CopyingManifest,
            P::DerivingKey => JobPhase::DerivingKey,
            P::UnwrappingKeys => JobPhase::UnwrappingKeys,
            P::DecryptingManifest => JobPhase::DecryptingManifest,
            P::CheckingDatabase => JobPhase::CheckingDatabase,
            P::CheckingFiles => JobPhase::CheckingFiles,
            P::CheckingMessagesDb => JobPhase::CheckingMessagesDb,
            P::Extracting => JobPhase::Extracting,
            P::Copying => JobPhase::CopyingAttachments,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct JobStarted {
    pub job_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct JobSummary {
    pub job_id: String,
    pub backup_id: String,
    pub kind: JobKind,
    pub phase: Option<JobPhase>,
}

/// Delivered over a `tauri::ipc::Channel` (D2). Exactly one terminal event
/// (`finished`, `extractionFinished`, `importFinished`, `verifyFinished`,
/// `failed` or `cancelled`) ends every job. `progress` and
/// `importPreflight` are the only non-terminal events. Every event stays
/// under 1 KiB (D47): reports and the preflight are fetched with
/// `get_backup_details` and `get_archive_details`.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum JobEvent {
    Progress {
        job_id: String,
        phase: JobPhase,
        done: u64,
        total: Option<u64>,
    },
    Finished {
        job_id: String,
        // Boxed: the report is much larger than the other variants.
        report: Box<ValidationReportDto>,
        staging_removed: bool,
    },
    /// The end of an extraction check: a summary only (D47). The reports
    /// are fetched with `get_backup_details`, an invoke response, so this
    /// event stays far below the channel's 8 KiB direct-delivery size.
    ExtractionFinished {
        job_id: String,
        outcome: ExtractionOutcomeDto,
        source_changed: bool,
        staging_removed: bool,
        plaintext_removed_after_row_pass: bool,
    },
    Failed {
        job_id: String,
        error: IpcError,
        staging_removed: bool,
    },
    Cancelled {
        job_id: String,
        staging_removed: bool,
    },
    /// An import waits at the gate (M4 §15.3). Non-terminal; the
    /// preflight is fetched with `get_archive_details`, then answered with
    /// `confirm_import` within two minutes.
    ImportPreflight { job_id: String },
    /// The end of an import, except a cancel during the passes, which ends
    /// with `cancelled`. Counts are fetched with `get_archive_details`.
    ImportFinished {
        job_id: String,
        outcome: ImportOutcomeDto,
        /// For `stopped`.
        stop_reason: Option<StopReasonDto>,
        /// For `notImportable`.
        not_importable: Option<NotImportableDto>,
        /// For `cancelledAtGate`.
        gate: Option<GateEndingDto>,
        /// The session, for `completed` and `stopped`.
        session: Option<i64>,
        /// The sentence for `stopped` and `notImportable`.
        notice: Option<IpcError>,
        /// The staging folder (the backup's file index and the Messages
        /// database copy) was removed.
        staging_removed: bool,
        /// The plaintext Messages database copy was removed after the last
        /// pass, before finalize; `null` when no session ran.
        plaintext_removed: Option<bool>,
    },
    /// The end of a verify. The report is fetched with
    /// `get_archive_details`.
    VerifyFinished {
        job_id: String,
        outcome: VerifyOutcomeDto,
        /// The result was recorded in the archive.
        recorded: bool,
    },
}

impl JobEvent {
    /// Every event but `progress` and `importPreflight`.
    pub fn is_terminal(&self) -> bool {
        !matches!(
            self,
            JobEvent::Progress { .. } | JobEvent::ImportPreflight { .. }
        )
    }

    /// The job the event belongs to.
    pub fn job_id(&self) -> &str {
        match self {
            JobEvent::Progress { job_id, .. }
            | JobEvent::Finished { job_id, .. }
            | JobEvent::ExtractionFinished { job_id, .. }
            | JobEvent::Failed { job_id, .. }
            | JobEvent::Cancelled { job_id, .. }
            | JobEvent::ImportPreflight { job_id }
            | JobEvent::ImportFinished { job_id, .. }
            | JobEvent::VerifyFinished { job_id, .. } => job_id,
        }
    }
}

// ---------------------------------------------------------------- inputs

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct BackupRef {
    pub backup_id: String,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct LocationRef {
    pub location_id: String,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct JobRef {
    pub job_id: String,
}

/// The only IPC input that carries a secret. No `Debug` derive: the
/// hand-written one prints `password: <redacted>`.
#[derive(Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct OpenBackupRequest {
    pub backup_id: String,
    #[ts(as = "Option<String>")]
    #[serde(default)]
    pub password: Option<BackupPassword>,
}

/// Start an extraction check. Like [`OpenBackupRequest`], the only other
/// IPC input that carries a secret; no `Debug` derive.
#[derive(Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct CheckExtractionRequest {
    pub backup_id: String,
    #[ts(as = "Option<String>")]
    #[serde(default)]
    pub password: Option<BackupPassword>,
}

impl fmt::Debug for CheckExtractionRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CheckExtractionRequest")
            .field("backup_id", &self.backup_id)
            .field("password", &"<redacted>")
            .finish()
    }
}

impl fmt::Debug for OpenBackupRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenBackupRequest")
            .field("backup_id", &self.backup_id)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// Larger inputs are refused in the visitor; the 1 KiB product limit is
/// enforced in the command.
pub const PASSWORD_INPUT_MAX_BYTES: usize = 64 * 1024;

/// A password as it crosses IPC. Deserialization copies the string straight
/// into `Zeroizing<String>`. Errors use fixed messages only, so serde's
/// error text can never echo the value. No `Clone`, no `Serialize`.
pub struct BackupPassword(Zeroizing<String>);

impl BackupPassword {
    pub fn into_inner(self) -> Zeroizing<String> {
        self.0
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    /// UTF-8 length; never logged.
    pub fn byte_len(&self) -> usize {
        self.0.len()
    }
}

impl fmt::Debug for BackupPassword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

struct PasswordVisitor;

impl Visitor<'_> for PasswordVisitor {
    type Value = BackupPassword;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a password string")
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
        if v.len() > PASSWORD_INPUT_MAX_BYTES {
            return Err(E::custom("password input too large"));
        }
        let mut s = Zeroizing::new(String::with_capacity(v.len()));
        s.push_str(v);
        Ok(BackupPassword(s))
    }

    fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
        // Owned: move it into Zeroizing without a copy, so it is wiped on drop
        // either way.
        let v = Zeroizing::new(v);
        if v.len() > PASSWORD_INPUT_MAX_BYTES {
            return Err(E::custom("password input too large"));
        }
        Ok(BackupPassword(v))
    }
}

impl<'de> Deserialize<'de> for BackupPassword {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_str(PasswordVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    const SENTINEL: &str = "synthetic-test-password-sentinel";

    #[test]
    fn clamping() {
        assert_eq!(clamp(5), (5, false));
        assert_eq!(clamp(MAX_SAFE_INTEGER), (MAX_SAFE_INTEGER, false));
        assert_eq!(clamp(u64::MAX), (MAX_SAFE_INTEGER, true));
        assert_eq!(safe_opt(Some(u64::MAX)), None);
        assert_eq!(safe_opt(Some(7)), Some(7));
        assert_eq!(millis(Duration::MAX), MAX_SAFE_INTEGER);
    }

    #[test]
    fn rfc3339_known_values() {
        assert_eq!(rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        assert_eq!(
            rfc3339(UNIX_EPOCH + Duration::from_secs(951_782_400)),
            "2000-02-29T00:00:00Z"
        );
        assert_eq!(
            rfc3339(UNIX_EPOCH + Duration::from_secs(1_792_281_599)),
            "2026-10-17T23:59:59Z"
        );
        assert_eq!(
            rfc3339(UNIX_EPOCH - Duration::from_secs(86_400)),
            "1969-12-31T00:00:00Z"
        );
        assert_eq!(
            rfc3339(UNIX_EPOCH - Duration::from_secs(978_307_200)),
            "1939-01-01T00:00:00Z"
        );
    }

    #[test]
    fn open_request_rejects_unknown_fields_and_redacts() {
        let bad = format!(r#"{{"backupId":"a","password":"{SENTINEL}","path":"/etc"}}"#);
        let err =
            serde_json::from_str::<OpenBackupRequest>(&bad).expect_err("unknown field rejected");
        assert!(!err.to_string().contains(SENTINEL));

        let ok = format!(r#"{{"backupId":"a","password":"{SENTINEL}"}}"#);
        let req: OpenBackupRequest = serde_json::from_str(&ok).expect("valid");
        let shown = format!("{req:?}");
        assert!(!shown.contains(SENTINEL), "{shown}");
        assert!(shown.contains("<redacted>"));
        assert_eq!(
            req.password.as_ref().map(BackupPassword::byte_len),
            Some(SENTINEL.len())
        );

        let none: OpenBackupRequest =
            serde_json::from_str(r#"{"backupId":"a","password":null}"#).expect("valid");
        assert!(none.password.is_none());
        let absent: OpenBackupRequest = serde_json::from_str(r#"{"backupId":"a"}"#).expect("valid");
        assert!(absent.password.is_none());
    }

    #[test]
    fn password_errors_never_echo_the_value() {
        let huge = SENTINEL.repeat(PASSWORD_INPUT_MAX_BYTES / SENTINEL.len() + 2);
        let raw = format!(r#"{{"backupId":"a","password":"{huge}"}}"#);
        let err = serde_json::from_str::<OpenBackupRequest>(&raw).expect_err("too large");
        assert!(!err.to_string().contains(SENTINEL));

        // The same through a borrowed Value, as Tauri deserializes arguments.
        let value: serde_json::Value = serde_json::from_str(&raw).expect("json");
        let err = OpenBackupRequest::deserialize(&value).expect_err("too large");
        assert!(!err.to_string().contains(SENTINEL));

        for wrong in [
            r#"{"backupId":"a","password":12345}"#,
            r#"{"backupId":"a","password":["synthetic-test-password-sentinel"]}"#,
            r#"{"backupId":"a","password":{"x":"synthetic-test-password-sentinel"}}"#,
        ] {
            let err = serde_json::from_str::<OpenBackupRequest>(wrong).expect_err("wrong type");
            assert!(!err.to_string().contains(SENTINEL), "{err}");
        }
    }

    #[test]
    fn job_events_round_trip_in_camel_case() {
        let report = ValidationReportDto {
            importable: true,
            checks: vec![],
            stats: ManifestStatsDto {
                total_rows: 1,
                files: 1,
                directories: 0,
                symlinks: 0,
                other_flags: 0,
                declared_bytes: 0,
                declared_bytes_saturated: false,
                missing_blobs: 0,
                size_mismatches: 0,
                unsafe_blobs: 0,
                unreadable_blobs: 0,
                invalid_rows: 0,
                invalid_file_ids: 0,
                file_id_mismatches: 0,
                paths_unusual_for_export: 0,
                unreadable_records: 0,
                files_without_key: 0,
                files_with_unavailable_class: 0,
                has_properties_table: true,
                views: 0,
                triggers: 0,
                saturated: false,
            },
            sms_db: SmsDbStatusDto {
                listed: true,
                ambiguous: false,
                blob_present: Some(true),
                declared_size: Some(4096),
                size_consistent: Some(true),
                header_ok: Some(true),
                padding_ok: None,
                wal_listed: false,
                shm_listed: false,
            },
            duration_ms: 12,
            validated_at: "2026-10-18T00:00:00Z".into(),
        };
        let cases = [
            (
                JobEvent::Progress {
                    job_id: "job-1".into(),
                    phase: JobPhase::CheckingFiles,
                    done: 3,
                    total: Some(9),
                },
                serde_json::json!({"kind":"progress","jobId":"job-1","phase":"checkingFiles","done":3,"total":9}),
            ),
            (
                JobEvent::Finished {
                    job_id: "job-1".into(),
                    report: Box::new(report.clone()),
                    staging_removed: true,
                },
                serde_json::json!({"kind":"finished","jobId":"job-1","report":serde_json::to_value(&report).expect("json"),"stagingRemoved":true}),
            ),
            (
                JobEvent::Failed {
                    job_id: "job-2".into(),
                    error: IpcError::new(ErrorCode::WrongPassword, "x"),
                    staging_removed: false,
                },
                serde_json::json!({"kind":"failed","jobId":"job-2","error":{"code":"wrongPassword","message":"x"},"stagingRemoved":false}),
            ),
            (
                JobEvent::Cancelled {
                    job_id: "job-3".into(),
                    staging_removed: true,
                },
                serde_json::json!({"kind":"cancelled","jobId":"job-3","stagingRemoved":true}),
            ),
        ];
        for (event, expected) in cases {
            assert_eq!(serde_json::to_value(&event).expect("json"), expected);
        }
        let v = serde_json::to_value(&report).expect("json");
        assert!(v.get("smsDb").is_some() && v.get("durationMs").is_some());
        assert!(v["stats"].get("filesWithUnavailableClass").is_some());
    }

    /// Guards on the generated declarations (§11.6), through `decl()` per
    /// type rather than a scan of the generated folder, which would race with
    /// ts-rs's parallel export tests.
    #[test]
    fn generated_types_never_carry_identifiers_or_bigint() {
        let cfg = ts_rs::Config::from_env();
        let decls = [
            BackupList::decl(&cfg),
            BackupLocation::decl(&cfg),
            BackupSummary::decl(&cfg),
            AddBackupFolderResult::decl(&cfg),
            ForgetBackupFolderResult::decl(&cfg),
            BackupDetails::decl(&cfg),
            DeviceDetails::decl(&cfg),
            StatusDetails::decl(&cfg),
            EncryptionDetails::decl(&cfg),
            LayoutDetails::decl(&cfg),
            ValidationCheck::decl(&cfg),
            ManifestStatsDto::decl(&cfg),
            SmsDbStatusDto::decl(&cfg),
            ValidationReportDto::decl(&cfg),
            CleanupDto::decl(&cfg),
            JobStarted::decl(&cfg),
            JobSummary::decl(&cfg),
            JobEvent::decl(&cfg),
            OpenBackupRequest::decl(&cfg),
            CheckExtractionRequest::decl(&cfg),
            ExtractionReportDto::decl(&cfg),
            crate::settings::AddedFolderEntry::decl(&cfg),
        ];
        for d in &decls {
            let lower = d.to_lowercase();
            assert!(!lower.contains("bigint"), "{d}");
            for forbidden in [
                "udid",
                "serial",
                "imei",
                "iccid",
                "meid",
                "phonenumber",
                "uniqueidentifier",
            ] {
                assert!(!lower.contains(forbidden), "{forbidden} in {d}");
            }
        }
        let summary = BackupSummary::decl(&cfg);
        assert!(!summary.contains("path"), "{summary}");
        assert!(OpenBackupRequest::decl(&cfg).contains("password: string | null"));
        assert!(CheckExtractionRequest::decl(&cfg).contains("password: string | null"));
    }

    #[test]
    fn check_extraction_request_rejects_unknown_fields_and_redacts() {
        let bad = format!(r#"{{"backupId":"a","password":"{SENTINEL}","path":"/etc"}}"#);
        let err = serde_json::from_str::<CheckExtractionRequest>(&bad)
            .expect_err("unknown field rejected");
        assert!(!err.to_string().contains(SENTINEL));
        let ok = format!(r#"{{"backupId":"a","password":"{SENTINEL}"}}"#);
        let req: CheckExtractionRequest = serde_json::from_str(&ok).expect("valid");
        let shown = format!("{req:?}");
        assert!(!shown.contains(SENTINEL), "{shown}");
        assert!(shown.contains("<redacted>"));
        let absent: CheckExtractionRequest =
            serde_json::from_str(r#"{"backupId":"a"}"#).expect("valid");
        assert!(absent.password.is_none());
    }

    #[test]
    fn every_messages_phase_has_its_own_job_phase() {
        use oma_messages::MessagesPhase as P;
        let phases = [
            P::StagingMessagesDb,
            P::ScanningSchema,
            P::CheckingIntegrity,
            P::ReadingSchema,
            P::ReadingHandles,
            P::ReadingChats,
            P::ReadingChatLinks,
            P::ReadingMessages,
            P::CheckingAttachments,
            P::Summarising,
        ];
        let mapped: std::collections::HashSet<String> = phases
            .iter()
            .map(|p| serde_json::to_string(&JobPhase::from(*p)).expect("json"))
            .collect();
        assert_eq!(mapped.len(), phases.len());
        assert_eq!(
            serde_json::to_value(JobPhase::from(P::ReadingHandles)).expect("json"),
            "readingHandles"
        );
    }
}
