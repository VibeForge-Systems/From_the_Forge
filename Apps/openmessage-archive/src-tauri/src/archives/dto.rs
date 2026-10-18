//! Every IPC type of the archive commands and jobs (spec §15.4).
//!
//! The rules, each enforced by a test in this module:
//!
//! - numbers go through [`clamp`] and [`safe_opt`]; a struct whose counter
//!   was clamped says so in `saturated`, and `Option<u64>` crosses as
//!   `null`, never 0;
//! - no GUID, digest, rank, rowid, cursor, path, device name or product
//!   type is a field. Archive ids cross as `a<32hex>`, lineage, session and
//!   verify-run numbers as small integers, and dates as Unix milliseconds;
//! - device facts cross as booleans only (`modelDiffers`, `nameDiffers`,
//!   `deviceMatches`);
//! - **the only free-text string is `ArchiveSummaryDto.label`**, the user's
//!   own text, re-validated when read. Every other string is an id the
//!   shell made or a sentence it wrote.

use std::fmt;

use oma_archive as oa;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::backups::dto::{clamp, safe_opt, BackupPassword, JobKind, JobPhase};
use crate::error::IpcError;

/// Unix nanoseconds as Unix milliseconds. Always within ±(2^53 − 1).
pub fn ms(ns: i64) -> i64 {
    ns.div_euclid(1_000_000)
}

/// Clamps counters and remembers whether any was clamped.
#[derive(Debug, Default)]
pub struct Counter {
    pub saturated: bool,
}

impl Counter {
    pub fn n(&mut self, v: u64) -> u64 {
        let (v, s) = clamp(v);
        self.saturated |= s;
        v
    }
}

// ---------------------------------------------------------------- inputs

/// `create_archive`. The label is checked by `oma_archive::Label::parse`:
/// at most 200 UTF-8 bytes, no control or bidirectional formatting
/// characters, not empty.
#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct CreateArchiveRequest {
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ArchiveRef {
    pub archive_id: String,
}

/// The import's two choices (spec §15.2). `rehashExisting` is not
/// accepted (C3.20).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ImportOptionsInput {
    pub include_recently_deleted: bool,
    pub copy_attachments: bool,
}

/// `start_import`. Carries a secret: no derived `Debug` (the manual one
/// redacts the password), no `Clone`.
#[derive(Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct StartImportRequest {
    pub backup_id: String,
    pub archive_id: String,
    #[ts(as = "Option<String>")]
    #[serde(default)]
    pub password: Option<BackupPassword>,
    pub options: ImportOptionsInput,
}

impl fmt::Debug for StartImportRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StartImportRequest")
            .field("options", &self.options)
            .field("password", &"<redacted>")
            .finish_non_exhaustive()
    }
}

/// The lineage chosen at the gate (D80): `{ kind: "existing", lineageId }`
/// or `{ kind: "new" }`. Deserialized through a strict raw form, so an
/// unknown field or a missing or extra `lineageId` is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, TS)]
#[ts(
    export,
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum LineageInput {
    /// The same phone as this lineage.
    Existing { lineage_id: i64 },
    /// A different phone or source.
    New,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum LineageKindRaw {
    Existing,
    New,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LineageInputRaw {
    kind: LineageKindRaw,
    #[serde(default)]
    lineage_id: Option<i64>,
}

impl<'de> Deserialize<'de> for LineageInput {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = LineageInputRaw::deserialize(d)?;
        match (raw.kind, raw.lineage_id) {
            (LineageKindRaw::Existing, Some(lineage_id)) => {
                Ok(LineageInput::Existing { lineage_id })
            }
            (LineageKindRaw::New, None) => Ok(LineageInput::New),
            (LineageKindRaw::Existing, None) => Err(serde::de::Error::custom(
                "an existing lineage needs lineageId",
            )),
            (LineageKindRaw::New, Some(_)) => {
                Err(serde::de::Error::custom("a new lineage takes no lineageId"))
            }
        }
    }
}

/// `confirm_import`. `lineage` is required when `proceed` is true.
#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ConfirmImportRequest {
    pub job_id: String,
    pub proceed: bool,
    #[serde(default)]
    pub lineage: Option<LineageInput>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum VerifyDepthDto {
    Quick,
    Full,
}

impl From<VerifyDepthDto> for oa::VerifyDepth {
    fn from(d: VerifyDepthDto) -> Self {
        match d {
            VerifyDepthDto::Quick => oa::VerifyDepth::Quick,
            VerifyDepthDto::Full => oa::VerifyDepth::Full,
        }
    }
}

impl From<oa::VerifyDepth> for VerifyDepthDto {
    fn from(d: oa::VerifyDepth) -> Self {
        match d {
            oa::VerifyDepth::Quick => VerifyDepthDto::Quick,
            oa::VerifyDepth::Full => VerifyDepthDto::Full,
        }
    }
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VerifyArchiveRequest {
    pub archive_id: String,
    pub depth: VerifyDepthDto,
}

// ---------------------------------------------------------------- listing

/// The library as `list_archives` found it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum LibraryStateDto {
    Ready,
    /// No archive folder is chosen.
    NotConfigured,
    /// The archive folder was refused or could not be read; the listing's
    /// `libraryError` says why.
    Unusable,
}

/// One archive's status badge (spec §16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ArchiveStatusDto {
    Ready,
    /// This app is importing into it.
    Importing,
    /// This app is verifying it.
    Verifying,
    /// Another program (or another copy of the app) holds its lock.
    InUse,
    NeedsUpgrade,
    NewerVersion,
    Damaged,
    NotAnArchive,
    Refused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum DamageKindDto {
    SchemaMismatch,
    MetaMissing,
    MetaInconsistent,
    MigrationFailed,
    Unreadable,
    WorkBudget,
    Other,
}

impl From<oa::DamageKind> for DamageKindDto {
    fn from(k: oa::DamageKind) -> Self {
        use oa::DamageKind as K;
        match k {
            K::SchemaMismatch => DamageKindDto::SchemaMismatch,
            K::MetaMissing => DamageKindDto::MetaMissing,
            K::MetaInconsistent => DamageKindDto::MetaInconsistent,
            K::MigrationFailed => DamageKindDto::MigrationFailed,
            K::Unreadable => DamageKindDto::Unreadable,
            K::WorkBudget => DamageKindDto::WorkBudget,
            // `DamageKind` is non-exhaustive.
            _ => DamageKindDto::Other,
        }
    }
}

/// Why a directory in the library is listed as refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum RefusalDto {
    Link,
    Permissions,
    Volume,
    Io,
}

impl From<oa::RefusalKind> for RefusalDto {
    fn from(k: oa::RefusalKind) -> Self {
        use oa::RefusalKind as K;
        match k {
            K::Link => RefusalDto::Link,
            K::Permissions(_) => RefusalDto::Permissions,
            K::Volume => RefusalDto::Volume,
            _ => RefusalDto::Io,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum SessionStateDto {
    Running,
    Completed,
    Cancelled,
    Failed,
    Interrupted,
    Superseded,
    /// Stopped in a resumable state (D88): "stopped; can continue".
    Stopped,
}

impl From<oa::SessionState> for SessionStateDto {
    fn from(s: oa::SessionState) -> Self {
        use oa::SessionState as S;
        match s {
            S::Running => SessionStateDto::Running,
            S::Completed => SessionStateDto::Completed,
            S::Cancelled => SessionStateDto::Cancelled,
            S::Failed => SessionStateDto::Failed,
            S::Interrupted => SessionStateDto::Interrupted,
            S::Superseded => SessionStateDto::Superseded,
            S::Stopped => SessionStateDto::Stopped,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ArchiveStatsDto {
    pub handles: u64,
    pub chats: u64,
    pub messages: u64,
    pub body_versions: u64,
    pub attachments: u64,
    pub blobs: u64,
    pub blob_bytes: u64,
    pub computed_at_ms: i64,
    pub saturated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SessionCountsDto {
    pub running: u64,
    pub completed: u64,
    pub cancelled: u64,
    pub failed: u64,
    pub interrupted: u64,
    pub superseded: u64,
    pub stopped: u64,
}

/// What the list and the details show about one archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ArchiveSummaryDto {
    pub archive_id: String,
    /// The user's label: the only free text in any archive DTO. Render it
    /// with text interpolation only (I7).
    pub label: Option<String>,
    /// A label is stored but is not a valid label; it is not shown.
    pub label_unreadable: bool,
    pub schema_version: u32,
    pub created_at_ms: i64,
    /// `null` until the first import finishes.
    pub stats: Option<ArchiveStatsDto>,
    pub sessions: SessionCountsDto,
    /// The newest session is unfinished (running, cancelled, interrupted
    /// or stopped).
    pub resumable_session: Option<i64>,
    pub resumable_state: Option<SessionStateDto>,
    /// Written by newer rules: readable and verifiable, not importable.
    pub newer_rules: bool,
    pub stat_tables: bool,
}

pub fn summary_dto(i: &oa::ArchiveInfo) -> ArchiveSummaryDto {
    let mut c = Counter::default();
    let stats = i.stats.map(|s| ArchiveStatsDto {
        handles: c.n(s.handles),
        chats: c.n(s.chats),
        messages: c.n(s.messages),
        body_versions: c.n(s.body_versions),
        attachments: c.n(s.attachments),
        blobs: c.n(s.blobs),
        blob_bytes: c.n(s.blob_bytes),
        computed_at_ms: ms(s.computed_at_ns),
        saturated: false,
    });
    let stats = stats.map(|s| ArchiveStatsDto {
        saturated: c.saturated,
        ..s
    });
    let s = &i.sessions;
    let mut k = Counter::default();
    let (label, label_unreadable) = match &i.label {
        oa::LabelState::Ok(l) => (Some(l.as_str().to_owned()), false),
        oa::LabelState::Unreadable => (None, true),
        _ => (None, false),
    };
    ArchiveSummaryDto {
        archive_id: i.id.as_str().to_owned(),
        label,
        label_unreadable,
        schema_version: i.schema_version,
        created_at_ms: ms(i.created_at_ns),
        stats,
        sessions: SessionCountsDto {
            running: k.n(s.running),
            completed: k.n(s.completed),
            cancelled: k.n(s.cancelled),
            failed: k.n(s.failed),
            interrupted: k.n(s.interrupted),
            superseded: k.n(s.superseded),
            stopped: k.n(s.stopped),
        },
        resumable_session: i.resumable.map(|r| r.session),
        resumable_state: i.resumable.map(|r| r.state.into()),
        newer_rules: i.import_blocked.is_some(),
        stat_tables: i.stat_tables,
    }
}

/// One archive in the list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ArchiveEntryDto {
    pub archive_id: String,
    pub status: ArchiveStatusDto,
    /// For `needsUpgrade` and `newerVersion`.
    pub schema_version: Option<u32>,
    pub damage: Option<DamageKindDto>,
    pub refusal: Option<RefusalDto>,
    /// For `ready`.
    pub summary: Option<ArchiveSummaryDto>,
}

/// `list_archives`.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ArchiveListingDto {
    pub entries: Vec<ArchiveEntryDto>,
    pub library_state: LibraryStateDto,
    /// Why the library is unusable.
    pub library_error: Option<IpcError>,
}

// ---------------------------------------------------------------- details

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SessionDto {
    pub session: i64,
    pub state: SessionStateDto,
    pub started_at_ms: i64,
    pub ended_at_ms: Option<i64>,
    pub schema_version: u32,
    pub resumes: u32,
    /// Set at a terminal state (spec §8.7); `null` before.
    pub full_observation: Option<bool>,
}

impl From<&oa::SessionInfo> for SessionDto {
    fn from(s: &oa::SessionInfo) -> Self {
        SessionDto {
            session: s.id,
            state: s.state.into(),
            started_at_ms: ms(s.started_at_ns),
            ended_at_ms: s.ended_at_ns.map(ms),
            schema_version: s.schema_version,
            resumes: s.resumes,
            full_observation: s.full_observation,
        }
    }
}

/// Declares a counts DTO with the same fields as an `oma_archive` counts
/// struct, every one clamped, plus `saturated`.
macro_rules! counts_dto {
    ($(#[$m:meta])* $name:ident from $src:path { $($f:ident),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
        #[serde(rename_all = "camelCase")]
        #[ts(export)]
        pub struct $name {
            $(pub $f: u64,)+
            pub saturated: bool,
        }

        impl $name {
            pub fn of(s: &$src) -> $name {
                let mut c = Counter::default();
                $(let $f = c.n(s.$f);)+
                $name { $($f,)+ saturated: c.saturated }
            }
        }
    };
}

counts_dto! {
    /// One entity's pass (P1, P2, P3, P6 rows).
    EntityCountsDto from oa::EntityCounts {
        seen, inserted, already_present, unkeyed, cells_replaced, not_applied,
        history_added, value_issue_cells,
    }
}

counts_dto! {
    /// P3's body versions.
    BodyCountsDto from oa::BodyCounts {
        added, unchanged, rederived, current_moved, display_attributed_body,
        display_text, display_text_lossy, none_attributed_body_unusable,
        none_text_empty, none_no_source, sources_differ, decode_failed, run_issues,
    }
}

counts_dto! {
    /// One link pass (P4, P5, P7).
    LinkCountsDto from oa::LinkCounts {
        seen, inserted, already_present, unresolved, unresolved_first,
        unresolved_second, skipped_recently_deleted,
    }
}

counts_dto! {
    /// P6's attachment files.
    AttachmentFileCountsDto from oa::AttachmentFileCounts {
        copied, replaced, deduplicated, adopted, orphan_replaced, already_archived,
        store_conflicts, not_requested, not_looked_up, not_in_backup, blob_unavailable,
        over_limit, too_large_for_volume, read_failed, write_failed, size_mismatch,
        partials_left, bytes_copied, fsyncs, blobs_added, current_moved,
    }
}

counts_dto! {
    /// The Recently Deleted policy and markers.
    RecentlyDeletedCountsDto from oa::RecentlyDeletedCounts {
        messages, excluded, not_updated, imported, attachments_excluded, marked, not_marked,
    }
}

/// Everything an import counted (`ImportCounts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ImportCountsDto {
    pub handles: EntityCountsDto,
    pub chats: EntityCountsDto,
    pub messages: EntityCountsDto,
    pub bodies: BodyCountsDto,
    pub chat_handles: LinkCountsDto,
    pub chat_messages: LinkCountsDto,
    pub attachments: EntityCountsDto,
    pub attachment_files: AttachmentFileCountsDto,
    pub message_attachments: LinkCountsDto,
    pub recently_deleted: RecentlyDeletedCountsDto,
    pub batches: u64,
    pub resumes: u64,
    /// Any counter here, or in a nested struct, was clamped.
    pub saturated: bool,
}

impl From<&oa::ImportCounts> for ImportCountsDto {
    fn from(c: &oa::ImportCounts) -> Self {
        let mut k = Counter::default();
        let d = ImportCountsDto {
            handles: EntityCountsDto::of(&c.handles),
            chats: EntityCountsDto::of(&c.chats),
            messages: EntityCountsDto::of(&c.messages),
            bodies: BodyCountsDto::of(&c.bodies),
            chat_handles: LinkCountsDto::of(&c.chat_handles),
            chat_messages: LinkCountsDto::of(&c.chat_messages),
            attachments: EntityCountsDto::of(&c.attachments),
            attachment_files: AttachmentFileCountsDto::of(&c.attachment_files),
            message_attachments: LinkCountsDto::of(&c.message_attachments),
            recently_deleted: RecentlyDeletedCountsDto::of(&c.recently_deleted),
            batches: k.n(c.batches),
            resumes: k.n(c.resumes),
            saturated: false,
        };
        let nested = [
            d.handles.saturated,
            d.chats.saturated,
            d.messages.saturated,
            d.bodies.saturated,
            d.chat_handles.saturated,
            d.chat_messages.saturated,
            d.attachments.saturated,
            d.attachment_files.saturated,
            d.message_attachments.saturated,
            d.recently_deleted.saturated,
        ];
        ImportCountsDto {
            saturated: k.saturated || nested.iter().any(|s| *s),
            ..d
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum StopReasonDto {
    InsufficientSpace,
    SourceChanged,
    Other,
}

impl From<oa::StopReason> for StopReasonDto {
    fn from(r: oa::StopReason) -> Self {
        match r {
            oa::StopReason::InsufficientSpace => StopReasonDto::InsufficientSpace,
            oa::StopReason::SourceChanged => StopReasonDto::SourceChanged,
            _ => StopReasonDto::Other,
        }
    }
}

/// The newest import session (persisted in the archive, not in the shell).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct LastImportDto {
    pub session: i64,
    pub lineage_id: i64,
    pub state: SessionStateDto,
    pub started_at_ms: i64,
    pub ended_at_ms: Option<i64>,
    pub resumes: u32,
    pub stop_reason: Option<StopReasonDto>,
    /// Derived values are provisional (untested iOS version, D79).
    pub provisional: bool,
    /// `null` before the first batch, or when the stored counts do not
    /// parse.
    pub counts: Option<ImportCountsDto>,
}

impl From<&oa::LastImport> for LastImportDto {
    fn from(l: &oa::LastImport) -> Self {
        LastImportDto {
            session: l.session,
            lineage_id: l.lineage.get(),
            state: l.state.into(),
            started_at_ms: ms(l.started_at_ns),
            ended_at_ms: l.ended_at_ns.map(ms),
            resumes: l.resumes,
            stop_reason: l.stop_reason.map(Into::into),
            provisional: l.ios_version_untested,
            counts: l.counts.as_ref().map(Into::into),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum VerifyOutcomeDto {
    Ok,
    Problems,
    Cancelled,
    Incomplete,
}

impl From<oa::VerifyOutcome> for VerifyOutcomeDto {
    fn from(o: oa::VerifyOutcome) -> Self {
        match o {
            oa::VerifyOutcome::Ok => VerifyOutcomeDto::Ok,
            oa::VerifyOutcome::Problems => VerifyOutcomeDto::Problems,
            oa::VerifyOutcome::Cancelled => VerifyOutcomeDto::Cancelled,
            oa::VerifyOutcome::Incomplete => VerifyOutcomeDto::Incomplete,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum CheckStateDto {
    NotRun,
    Ok,
    Failed,
    Cancelled,
}

impl From<oa::CheckState> for CheckStateDto {
    fn from(s: oa::CheckState) -> Self {
        match s {
            oa::CheckState::Ok => CheckStateDto::Ok,
            oa::CheckState::Failed => CheckStateDto::Failed,
            oa::CheckState::Cancelled => CheckStateDto::Cancelled,
            _ => CheckStateDto::NotRun,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum SchemaCheckDto {
    NotRun,
    Ok,
    NotAnArchive,
    VersionChanged,
    Mismatch,
    MetaMissing,
    MetaInconsistent,
    Unreadable,
}

impl From<oa::SchemaCheck> for SchemaCheckDto {
    fn from(s: oa::SchemaCheck) -> Self {
        use oa::SchemaCheck as S;
        match s {
            S::Ok => SchemaCheckDto::Ok,
            S::NotAnArchive => SchemaCheckDto::NotAnArchive,
            S::VersionChanged => SchemaCheckDto::VersionChanged,
            S::Mismatch => SchemaCheckDto::Mismatch,
            S::MetaMissing => SchemaCheckDto::MetaMissing,
            S::MetaInconsistent => SchemaCheckDto::MetaInconsistent,
            S::Unreadable => SchemaCheckDto::Unreadable,
            _ => SchemaCheckDto::NotRun,
        }
    }
}

/// What a verify found. The per-table foreign-key breakdown stays in the
/// archive: only the total crosses, so no table name is a DTO string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct VerifyCountsDto {
    pub schema: SchemaCheckDto,
    pub newer_rules: bool,
    pub rules_above_high_water: u64,
    pub stat_tables: bool,
    pub fts_secure_delete: CheckStateDto,
    pub sqlite_check: CheckStateDto,
    pub sqlite_check_messages: u64,
    pub foreign_key_violations: u64,
    pub value_issue_orphans: u64,
    pub value_history_orphans: u64,
    pub label_unreadable: bool,
    pub sessions_running: u64,
    pub blob_intents: u64,
    pub fts_integrity: CheckStateDto,
    pub blobs: u64,
    pub blob_bad_name: u64,
    pub blob_missing: u64,
    pub blob_not_regular: u64,
    pub blob_length_mismatch: u64,
    pub blobs_hashed: u64,
    pub bytes_hashed: u64,
    pub blob_hash_mismatch: u64,
    pub blob_unreadable: u64,
    pub store_entries: u64,
    pub store_unexpected: u64,
    pub store_orphans: u64,
    pub store_links: u64,
    pub store_walk_truncated: bool,
    pub partial_files: u64,
    pub provenance_files: u64,
    pub provenance_unreadable: u64,
    pub provenance_tmp_files: u64,
    pub imports_scan_truncated: bool,
    pub not_private: u64,
    pub checks_not_run: u64,
    pub has_problems: bool,
    pub saturated: bool,
}

impl From<&oa::VerifyCounts> for VerifyCountsDto {
    fn from(v: &oa::VerifyCounts) -> Self {
        let mut c = Counter::default();
        let d = VerifyCountsDto {
            schema: v.schema.into(),
            newer_rules: v.newer_rules,
            rules_above_high_water: c.n(v.rules_above_high_water),
            stat_tables: v.stat_tables,
            fts_secure_delete: v.fts_secure_delete.into(),
            sqlite_check: v.sqlite_check.into(),
            sqlite_check_messages: c.n(v.sqlite_check_messages),
            foreign_key_violations: c.n(v.foreign_key_violations),
            value_issue_orphans: c.n(v.value_issue_orphans),
            value_history_orphans: c.n(v.value_history_orphans),
            label_unreadable: v.label_unreadable,
            sessions_running: c.n(v.sessions_running),
            blob_intents: c.n(v.blob_intents),
            fts_integrity: v.fts_integrity.into(),
            blobs: c.n(v.blobs),
            blob_bad_name: c.n(v.blob_bad_name),
            blob_missing: c.n(v.blob_missing),
            blob_not_regular: c.n(v.blob_not_regular),
            blob_length_mismatch: c.n(v.blob_length_mismatch),
            blobs_hashed: c.n(v.blobs_hashed),
            bytes_hashed: c.n(v.bytes_hashed),
            blob_hash_mismatch: c.n(v.blob_hash_mismatch),
            blob_unreadable: c.n(v.blob_unreadable),
            store_entries: c.n(v.store_entries),
            store_unexpected: c.n(v.store_unexpected),
            store_orphans: c.n(v.store_orphans),
            store_links: c.n(v.store_links),
            store_walk_truncated: v.store_walk_truncated,
            partial_files: c.n(v.partial_files),
            provenance_files: c.n(v.provenance_files),
            provenance_unreadable: c.n(v.provenance_unreadable),
            provenance_tmp_files: c.n(v.provenance_tmp_files),
            imports_scan_truncated: v.imports_scan_truncated,
            not_private: c.n(v.not_private),
            checks_not_run: c.n(v.checks_not_run),
            has_problems: v.has_problems(),
            saturated: false,
        };
        VerifyCountsDto {
            saturated: c.saturated,
            ..d
        }
    }
}

/// The newest verify run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct VerifySummaryDto {
    pub run: i64,
    pub depth: VerifyDepthDto,
    pub started_at_ms: i64,
    /// `null` when a crash ended the verify.
    pub ended_at_ms: Option<i64>,
    pub outcome: Option<VerifyOutcomeDto>,
    /// `null` when the stored counts do not parse.
    pub counts: Option<VerifyCountsDto>,
}

impl From<&oa::VerifySummary> for VerifySummaryDto {
    fn from(v: &oa::VerifySummary) -> Self {
        VerifySummaryDto {
            run: v.run,
            depth: v.depth.into(),
            started_at_ms: ms(v.started_at_ns),
            ended_at_ms: v.ended_at_ns.map(ms),
            outcome: v.outcome.map(Into::into),
            counts: v.counts.as_ref().map(Into::into),
        }
    }
}

/// A job of this app on an archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ArchiveJobDto {
    pub job_id: String,
    pub kind: JobKind,
    pub phase: Option<JobPhase>,
    /// The backup an import reads.
    pub backup_id: Option<String>,
    /// The import is waiting at the gate; `preflight` holds what to show.
    pub waiting_at_gate: bool,
}

/// `get_archive_details`, read through the lock-free reader, so it works
/// during an import.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ArchiveDetailsDto {
    pub summary: ArchiveSummaryDto,
    /// Newest first, at most 1,000.
    pub sessions: Vec<SessionDto>,
    pub last_import: Option<LastImportDto>,
    pub last_verify: Option<VerifySummaryDto>,
    pub active_job: Option<ArchiveJobDto>,
    /// While an import waits at the gate: what it asks (re-shown after a
    /// reload).
    pub preflight: Option<ImportPreflightDto>,
    /// The newest session is unfinished, so "Continue importing…" applies.
    pub resumable: bool,
    /// Listed backups that look like the unfinished session's source,
    /// computed in Rust (C2.13). Pre-selection only.
    pub matching_backup_ids: Vec<String>,
}

// ---------------------------------------------------------------- the gate

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum PassDto {
    Handles,
    Chats,
    Messages,
    ChatHandles,
    ChatMessages,
    Attachments,
    MessageAttachments,
    RecentlyDeleted,
    Finalize,
}

impl From<oa::Pass> for PassDto {
    fn from(p: oa::Pass) -> Self {
        use oa::Pass as P;
        match p {
            P::Handles => PassDto::Handles,
            P::Chats => PassDto::Chats,
            P::Messages => PassDto::Messages,
            P::ChatHandles => PassDto::ChatHandles,
            P::ChatMessages => PassDto::ChatMessages,
            P::Attachments => PassDto::Attachments,
            P::MessageAttachments => PassDto::MessageAttachments,
            P::RecentlyDeleted => PassDto::RecentlyDeleted,
            P::Finalize => PassDto::Finalize,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum FreshReasonDto {
    NothingToContinue,
    /// The backup changed since the interrupted import, or the app was
    /// updated: that import is set aside and everything re-read (C3.18).
    SourceOrBuildChanged,
}

/// Continue or start over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum ResumePlanDto {
    /// Session `session` continues from `fromPass`, if `lineageId` is
    /// chosen.
    Continue {
        session: i64,
        lineage_id: i64,
        from_pass: PassDto,
    },
    Fresh {
        supersedes: u32,
        reason: FreshReasonDto,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum DeviceEvidenceDto {
    FirstImport,
    Compared,
}

/// A lineage choice, as the gate suggests it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum LineageChoiceDto {
    Existing { lineage_id: i64 },
    New,
}

impl From<oa::LineageChoice> for LineageChoiceDto {
    fn from(c: oa::LineageChoice) -> Self {
        match c {
            oa::LineageChoice::Existing(id) => LineageChoiceDto::Existing {
                lineage_id: id.get(),
            },
            oa::LineageChoice::New => LineageChoiceDto::New,
        }
    }
}

/// One import set (lineage) at the gate. Device facts are booleans.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct LineageDto {
    pub lineage_id: i64,
    pub sessions: u64,
    pub full_sessions: u64,
    /// The backup date of its newest full observation.
    pub last_backup_ms: Option<i64>,
    /// `null` when either side lacks the fact.
    pub model_differs: Option<bool>,
    pub name_differs: Option<bool>,
    pub device_matches: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct KeyCountDto {
    pub rows: u64,
    pub keyable: u64,
    pub duplicated_keys: u64,
    pub unkeyed: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct KeyFactsDto {
    pub handles: KeyCountDto,
    pub chats: KeyCountDto,
    pub messages: KeyCountDto,
    pub attachments: KeyCountDto,
    pub messages_already_archived: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RdFactsDto {
    pub messages: u64,
    pub excluded: u64,
    pub already_archived: u64,
    pub attachments_excluded: u64,
}

/// The space estimate, worded as an estimate at the gate (spec §12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SpaceEstimateDto {
    pub staged_bytes: u64,
    pub bytes_to_copy: u64,
    pub files_to_copy: u64,
    pub largest_file: u64,
    pub reserve: u64,
    pub wal_allowance: u64,
    pub needed: u64,
    /// `null` when the free space could not be read.
    pub available: Option<u64>,
    /// The estimate is above the available space (a warning, not a
    /// refusal).
    pub warns: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum OrderSourceDto {
    StatusPlistDate,
    InfoPlistLastBackupDate,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct OrderFactsDto {
    pub dated: bool,
    pub source: Option<OrderSourceDto>,
    pub out_of_range: bool,
    /// "This backup is older than the newest one in this archive."
    pub sorts_before_latest: bool,
    pub source_differs: bool,
}

/// A recorded qualifier (closed kinds, code order).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum QualifierCodeDto {
    BackupDateOutOfRange,
    WalSkipped,
    WalStateUnknown,
    WalFramesInvalid,
    NonUtf8Encoding,
    SizeMismatch,
    IosVersionUntested,
    UnknownTables,
    UnknownColumns,
    WrongCaseColumns,
}

impl From<oa::QualifierBit> for QualifierCodeDto {
    fn from(q: oa::QualifierBit) -> Self {
        use oa::QualifierBit as Q;
        match q {
            Q::BackupDateOutOfRange => QualifierCodeDto::BackupDateOutOfRange,
            Q::WalSkipped => QualifierCodeDto::WalSkipped,
            Q::WalStateUnknown => QualifierCodeDto::WalStateUnknown,
            Q::WalFramesInvalid => QualifierCodeDto::WalFramesInvalid,
            Q::NonUtf8Encoding => QualifierCodeDto::NonUtf8Encoding,
            Q::SizeMismatch => QualifierCodeDto::SizeMismatch,
            Q::IosVersionUntested => QualifierCodeDto::IosVersionUntested,
            Q::UnknownTables => QualifierCodeDto::UnknownTables,
            Q::UnknownColumns => QualifierCodeDto::UnknownColumns,
            Q::WrongCaseColumns => QualifierCodeDto::WrongCaseColumns,
        }
    }
}

/// What the gate shows (spec §16). Fetched with `get_archive_details`
/// while the job waits; the `importPreflight` event only announces it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ImportPreflightDto {
    pub resume: ResumePlanDto,
    /// A completed session already imported this exact content.
    pub same_content_completed: bool,
    pub device: DeviceEvidenceDto,
    /// Oldest first, at most 1,000.
    pub lineages: Vec<LineageDto>,
    pub suggested_lineage: LineageChoiceDto,
    pub keys: KeyFactsDto,
    pub recently_deleted: RdFactsDto,
    pub space: SpaceEstimateDto,
    pub order: OrderFactsDto,
    pub encrypted_source: bool,
    pub sync_folder_warning: bool,
    pub windows_acl_inherited: bool,
    pub qualifiers: Vec<QualifierCodeDto>,
    /// `iosVersionUntested` is recorded: "Derived details for this import
    /// are provisional" (D79).
    pub provisional: bool,
    pub saturated: bool,
}

fn differs(m: Option<bool>) -> Option<bool> {
    m.map(|m| !m)
}

pub fn preflight_dto(p: &oa::Preflight) -> ImportPreflightDto {
    let mut c = Counter::default();
    let resume = match p.resume {
        oa::ResumePlan::Continue {
            session,
            lineage,
            from,
        } => ResumePlanDto::Continue {
            session,
            lineage_id: lineage.get(),
            from_pass: from.into(),
        },
        oa::ResumePlan::Fresh { supersedes, reason } => ResumePlanDto::Fresh {
            supersedes,
            reason: match reason {
                oa::FreshReason::NothingToContinue => FreshReasonDto::NothingToContinue,
                _ => FreshReasonDto::SourceOrBuildChanged,
            },
        },
        // `ResumePlan` is non-exhaustive: an unknown plan is shown as a
        // fresh start, which is what the crate does when nothing matches.
        _ => ResumePlanDto::Fresh {
            supersedes: 0,
            reason: FreshReasonDto::SourceOrBuildChanged,
        },
    };
    let lineages = p
        .lineages
        .iter()
        .map(|l| LineageDto {
            lineage_id: l.id.get(),
            sessions: c.n(l.sessions),
            full_sessions: c.n(l.full_sessions),
            last_backup_ms: l.last_backup_ns.map(ms),
            model_differs: differs(l.model_matches),
            name_differs: differs(l.name_matches),
            device_matches: l.device_matches(),
        })
        .collect();
    let mut key = |k: &oa::KeyCountFacts| KeyCountDto {
        rows: c.n(k.rows),
        keyable: c.n(k.keyable),
        duplicated_keys: c.n(k.duplicated_keys),
        unkeyed: c.n(k.unkeyed),
    };
    let keys = KeyFactsDto {
        handles: key(&p.keys.handles),
        chats: key(&p.keys.chats),
        messages: key(&p.keys.messages),
        attachments: key(&p.keys.attachments),
        messages_already_archived: 0,
    };
    let keys = KeyFactsDto {
        messages_already_archived: c.n(p.keys.messages_already_archived),
        ..keys
    };
    let rd = &p.recently_deleted;
    let recently_deleted = RdFactsDto {
        messages: c.n(rd.messages),
        excluded: c.n(rd.excluded),
        already_archived: c.n(rd.already_archived),
        attachments_excluded: c.n(rd.attachments_excluded),
    };
    let s = &p.space;
    let space = SpaceEstimateDto {
        staged_bytes: c.n(s.staged_bytes),
        bytes_to_copy: c.n(s.bytes_to_copy),
        files_to_copy: c.n(s.files_to_copy),
        largest_file: c.n(s.largest_file),
        reserve: c.n(s.reserve),
        wal_allowance: c.n(s.wal_allowance),
        needed: c.n(s.needed),
        available: safe_opt(s.available),
        warns: s.warns(),
    };
    let o = &p.order;
    let order = OrderFactsDto {
        dated: o.dated,
        source: o.source.map(|s| match s {
            oa::OrderSource::StatusPlistDate => OrderSourceDto::StatusPlistDate,
            oa::OrderSource::InfoPlistLastBackupDate => OrderSourceDto::InfoPlistLastBackupDate,
            _ => OrderSourceDto::Other,
        }),
        out_of_range: o.out_of_range,
        sorts_before_latest: o.sorts_before_latest,
        source_differs: o.source_differs,
    };
    let qualifiers: Vec<QualifierCodeDto> = p.qualifiers.iter().map(|q| (*q).into()).collect();
    ImportPreflightDto {
        resume,
        same_content_completed: p.same_content_completed,
        device: match p.device {
            oa::DeviceEvidence::FirstImport => DeviceEvidenceDto::FirstImport,
            _ => DeviceEvidenceDto::Compared,
        },
        lineages,
        suggested_lineage: p.suggested_lineage.into(),
        keys,
        recently_deleted,
        space,
        order,
        encrypted_source: p.encrypted_source,
        sync_folder_warning: p.sync_folder_warning,
        windows_acl_inherited: p.windows_acl_inherited,
        provisional: qualifiers.contains(&QualifierCodeDto::IosVersionUntested),
        qualifiers,
        saturated: c.saturated,
    }
}

// ---------------------------------------------------------------- events

/// How an import ended, in `importFinished`. A cancel during the passes
/// ends with the `cancelled` event instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ImportOutcomeDto {
    Completed,
    /// Stopped in a resumable state: "stopped; can continue", never
    /// "failed" (D88).
    Stopped,
    /// Declined, cancelled or timed out at the gate: nothing was written.
    CancelledAtGate,
    /// The backup cannot be imported; nothing was written.
    NotImportable,
}

/// How the gate ended, for `cancelledAtGate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum GateEndingDto {
    Declined,
    Cancelled,
    TimedOut,
}

/// Why a backup cannot be imported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum NotImportableDto {
    /// Validation found an error (the backup's own report says which).
    BackupNotImportable,
    SchemaExceedsLimits,
    Unsupported,
    Damaged,
    Incomplete,
    /// A keyed table has more than 4,194,304 rows.
    OverKeyScanCap,
    /// Recently Deleted is over its cap.
    OverRecentlyDeletedCap,
    Other,
}

impl From<oa::NotImportable> for NotImportableDto {
    fn from(n: oa::NotImportable) -> Self {
        use oa::NotImportable as N;
        match n {
            N::SchemaExceedsLimits => NotImportableDto::SchemaExceedsLimits,
            N::Unsupported => NotImportableDto::Unsupported,
            N::Damaged => NotImportableDto::Damaged,
            N::Incomplete => NotImportableDto::Incomplete,
            N::OverKeyScanCap(_) => NotImportableDto::OverKeyScanCap,
            N::OverRecentlyDeletedCap => NotImportableDto::OverRecentlyDeletedCap,
            _ => NotImportableDto::Other,
        }
    }
}

/// The phases of an import (spec §8.12). `None` for a phase this build
/// does not know (`ImportPhase` is non-exhaustive): such an event is not
/// forwarded, so the UI keeps the last phase it knew.
pub fn import_phase(p: oa::ImportPhase) -> Option<JobPhase> {
    use oa::ImportPhase as P;
    Some(match p {
        P::StagingMessagesDb => JobPhase::StagingMessagesDb,
        P::ReadingSchema => JobPhase::ReadingSchema,
        P::ScanningRecentlyDeleted => JobPhase::ScanningRecentlyDeleted,
        P::ScanningKeys => JobPhase::ScanningKeys,
        P::EstimatingAttachments => JobPhase::EstimatingAttachments,
        P::AwaitingConfirmation => JobPhase::AwaitingConfirmation,
        P::PreparingArchive => JobPhase::PreparingArchive,
        P::ImportingHandles => JobPhase::ImportingHandles,
        P::ImportingChats => JobPhase::ImportingChats,
        P::ImportingMessages => JobPhase::ImportingMessages,
        P::ImportingChatLinks => JobPhase::ImportingChatLinks,
        P::CopyingAttachments => JobPhase::CopyingAttachments,
        P::ImportingAttachmentLinks => JobPhase::ImportingAttachmentLinks,
        P::ImportingRecentlyDeleted => JobPhase::ImportingRecentlyDeleted,
        P::Finalizing => JobPhase::Finalizing,
        P::OptimizingIndex => JobPhase::OptimizingIndex,
        P::Checkpointing => JobPhase::Checkpointing,
        _ => return None,
    })
}

/// The phases of a verify (spec §11). `None` for an unknown phase.
pub fn verify_phase(p: oa::VerifyPhase) -> Option<JobPhase> {
    use oa::VerifyPhase as P;
    Some(match p {
        P::CheckingDatabase => JobPhase::VerifyingDatabase,
        P::CheckingIndex => JobPhase::VerifyingIndex,
        P::CheckingFiles => JobPhase::VerifyingFiles,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backups::dto::{JobEvent, MAX_SAFE_INTEGER};
    use crate::error::ErrorCode;
    use crate::ts_guard;
    use std::collections::{BTreeMap, BTreeSet};
    use ts_rs::{Config, TS};

    const SENTINEL: &str = "synthetic-test-password-sentinel";

    #[test]
    fn inputs_reject_unknown_fields_and_redact() {
        let bad = format!(
            r#"{{"backupId":"b","archiveId":"a","password":"{SENTINEL}","options":{{"includeRecentlyDeleted":false,"copyAttachments":true}},"path":"/etc"}}"#
        );
        let err = serde_json::from_str::<StartImportRequest>(&bad).expect_err("unknown field");
        assert!(!err.to_string().contains(SENTINEL));
        let ok = format!(
            r#"{{"backupId":"b","archiveId":"a","password":"{SENTINEL}","options":{{"includeRecentlyDeleted":true,"copyAttachments":false}}}}"#
        );
        let req: StartImportRequest = serde_json::from_str(&ok).expect("valid");
        let shown = format!("{req:?}");
        assert!(!shown.contains(SENTINEL), "{shown}");
        assert!(req.options.include_recently_deleted && !req.options.copy_attachments);
        // rehashExisting is not accepted (C3.20).
        let rehash = r#"{"backupId":"b","archiveId":"a","options":{"includeRecentlyDeleted":false,"copyAttachments":true,"rehashExisting":true}}"#;
        assert!(serde_json::from_str::<StartImportRequest>(rehash).is_err());
        for bad in [r#"{"label":"x","path":"/"}"#, r#"{"label":7}"#] {
            assert!(
                serde_json::from_str::<CreateArchiveRequest>(bad).is_err(),
                "{bad}"
            );
        }
        assert!(serde_json::from_str::<ArchiveRef>(r#"{"archiveId":"a","x":1}"#).is_err());
        assert!(serde_json::from_str::<VerifyArchiveRequest>(
            r#"{"archiveId":"a","depth":"deep"}"#
        )
        .is_err());
        let v: VerifyArchiveRequest =
            serde_json::from_str(r#"{"archiveId":"a","depth":"full"}"#).expect("valid");
        assert_eq!(v.depth, VerifyDepthDto::Full);
    }

    #[test]
    fn confirm_input_takes_a_tagged_lineage() {
        let c: ConfirmImportRequest = serde_json::from_str(
            r#"{"jobId":"job-1","proceed":true,"lineage":{"kind":"existing","lineageId":3}}"#,
        )
        .expect("existing");
        assert_eq!(c.lineage, Some(LineageInput::Existing { lineage_id: 3 }));
        let c: ConfirmImportRequest =
            serde_json::from_str(r#"{"jobId":"job-1","proceed":true,"lineage":{"kind":"new"}}"#)
                .expect("new");
        assert_eq!(c.lineage, Some(LineageInput::New));
        let c: ConfirmImportRequest =
            serde_json::from_str(r#"{"jobId":"job-1","proceed":false}"#).expect("decline");
        assert!(c.lineage.is_none() && !c.proceed);
        for bad in [
            r#"{"jobId":"job-1","proceed":true,"lineage":{"kind":"existing"}}"#,
            r#"{"jobId":"job-1","proceed":true,"lineage":{"kind":"other"}}"#,
            r#"{"jobId":"job-1","proceed":true,"lineage":{"kind":"new","lineageId":1}}"#,
            r#"{"jobId":"job-1","proceed":true,"lineage":{"kind":"existing","lineageId":1,"guid":"x"}}"#,
            r#"{"jobId":"job-1","proceed":"yes"}"#,
        ] {
            assert!(
                serde_json::from_str::<ConfirmImportRequest>(bad).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn every_import_and_verify_phase_has_its_own_job_phase() {
        use oa::ImportPhase as I;
        use oa::VerifyPhase as V;
        let import = [
            I::StagingMessagesDb,
            I::ReadingSchema,
            I::ScanningRecentlyDeleted,
            I::ScanningKeys,
            I::EstimatingAttachments,
            I::AwaitingConfirmation,
            I::PreparingArchive,
            I::ImportingHandles,
            I::ImportingChats,
            I::ImportingMessages,
            I::ImportingChatLinks,
            I::CopyingAttachments,
            I::ImportingAttachmentLinks,
            I::ImportingRecentlyDeleted,
            I::Finalizing,
            I::OptimizingIndex,
            I::Checkpointing,
        ];
        let verify = [V::CheckingDatabase, V::CheckingIndex, V::CheckingFiles];
        let mut seen = BTreeSet::new();
        for p in import {
            let j = import_phase(p).expect("known");
            assert!(
                seen.insert(serde_json::to_string(&j).expect("json")),
                "{p:?}"
            );
        }
        for p in verify {
            let j = verify_phase(p).expect("known");
            assert!(
                seen.insert(serde_json::to_string(&j).expect("json")),
                "{p:?}"
            );
        }
        assert_eq!(seen.len(), import.len() + verify.len());
        // The verify phases do not reuse the validation phase names.
        assert_eq!(
            serde_json::to_value(verify_phase(V::CheckingDatabase)).expect("json"),
            "verifyingDatabase"
        );
    }

    #[test]
    fn counts_are_clamped_and_say_so() {
        let mut c = oa::ImportCounts::default();
        c.attachment_files.bytes_copied = u64::MAX;
        c.batches = 3;
        let d = ImportCountsDto::from(&c);
        assert_eq!(d.attachment_files.bytes_copied, MAX_SAFE_INTEGER);
        assert!(d.attachment_files.saturated && d.saturated);
        assert!(!d.handles.saturated);
        assert_eq!(d.batches, 3);
        let mut v = oa::VerifyCounts::default();
        v.bytes_hashed = u64::MAX;
        let d = VerifyCountsDto::from(&v);
        assert!(d.saturated);
        assert_eq!(d.bytes_hashed, MAX_SAFE_INTEGER);
        assert_eq!(ms(1_500_000_000), 1500);
        assert_eq!(ms(i64::MAX), i64::MAX / 1_000_000);
        assert!(ms(i64::MIN).unsigned_abs() <= MAX_SAFE_INTEGER);
    }

    // ------------------------------------------ structural guard (§15.4)

    /// The only `string` fields reachable from the archive DTOs, by type
    /// and key: `ArchiveSummaryDto.label` is the one free text; the rest
    /// are ids the shell made and sentences it wrote.
    const STRING_FIELDS: &[&str] = &[
        "ArchiveSummaryDto.label",
        "ArchiveSummaryDto.archiveId",
        "ArchiveEntryDto.archiveId",
        "ArchiveJobDto.jobId",
        "ArchiveJobDto.backupId",
        "ArchiveDetailsDto.matchingBackupIds",
        "IpcError.message",
        "JobEvent.jobId",
        // Inputs: ids and the password (never echoed).
        "ArchiveRef.archiveId",
        "CreateArchiveRequest.label",
        "StartImportRequest.backupId",
        "StartImportRequest.archiveId",
        "StartImportRequest.password",
        "ConfirmImportRequest.jobId",
        "VerifyArchiveRequest.archiveId",
    ];

    const FORBIDDEN_KEYS: &[&str] = &[
        "guid",
        "digest",
        "rank",
        "rowid",
        "cursor",
        "path",
        "name",
        "deviceName",
        "productType",
        "body",
        "text",
        "handle",
        "udid",
        "storageName",
        "sha256",
    ];

    fn walk() -> BTreeMap<String, String> {
        let mut w = ts_guard::Walk::new();
        w.root::<ArchiveListingDto>();
        w.root::<ArchiveDetailsDto>();
        w.root::<ArchiveSummaryDto>();
        w.root::<JobEvent>();
        w.root::<CreateArchiveRequest>();
        w.root::<ArchiveRef>();
        w.root::<StartImportRequest>();
        w.root::<ConfirmImportRequest>();
        w.root::<VerifyArchiveRequest>();
        w.decls()
    }

    #[test]
    fn archive_dtos_carry_no_free_string_but_the_label() {
        let decls = walk();
        let mut checked = 0;
        for (ty, decl) in &decls {
            assert!(!decl.contains("bigint"), "{decl}");
            // JobEvent reaches the M2/M3 report types; the backups guard
            // covers those.
            if ts_guard::M2_M3_TYPES.contains(&ty.as_str()) {
                continue;
            }
            for (key, field_ty) in ts_guard::fields(decl) {
                let qualified = format!("{ty}.{key}");
                assert!(
                    !FORBIDDEN_KEYS.contains(&key.as_str()),
                    "forbidden key {qualified}"
                );
                if ts_guard::is_string(&field_ty) {
                    assert!(
                        STRING_FIELDS.contains(&qualified.as_str()),
                        "{qualified} is typed {field_ty}"
                    );
                }
                checked += 1;
            }
        }
        assert!(checked > 150, "the parser found the fields ({checked})");
        // Exactly one free-text field among the outputs.
        let free: Vec<&&str> = STRING_FIELDS
            .iter()
            .filter(|f| f.ends_with(".label") && !f.contains("Request"))
            .collect();
        assert_eq!(free, vec![&"ArchiveSummaryDto.label"]);
        let lineage = LineageDto::decl(&Config::from_env());
        for b in ["modelDiffers", "nameDiffers", "deviceMatches"] {
            assert!(lineage.contains(&format!("{b}: boolean")), "{lineage}");
        }
    }

    #[test]
    fn every_archive_event_is_under_1_kib() {
        let long = format!("job-{}", u64::MAX);
        let worst_notice = crate::archives::messages::longest_sentence();
        let events = [
            JobEvent::ImportPreflight {
                job_id: long.clone(),
            },
            JobEvent::ImportFinished {
                job_id: long.clone(),
                outcome: ImportOutcomeDto::CancelledAtGate,
                stop_reason: Some(StopReasonDto::InsufficientSpace),
                not_importable: Some(NotImportableDto::OverRecentlyDeletedCap),
                gate: Some(GateEndingDto::TimedOut),
                session: Some(i64::try_from(MAX_SAFE_INTEGER).expect("fits")),
                notice: Some(IpcError::new(ErrorCode::SourceTooLarge, worst_notice)),
                staging_removed: false,
                plaintext_removed: Some(false),
            },
            JobEvent::VerifyFinished {
                job_id: long,
                outcome: VerifyOutcomeDto::Incomplete,
                recorded: false,
            },
        ];
        for e in &events {
            let json = serde_json::to_string(e).expect("json");
            assert!(json.len() < 1024, "{} bytes: {json}", json.len());
            assert!(e.is_terminal() != matches!(e, JobEvent::ImportPreflight { .. }));
        }
    }
}
