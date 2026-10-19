//! The IPC types of exports (M6 spec §9.2, §9.3, D136).
//!
//! Two walks review them:
//!
//! - **The export walk** (`guard_walk`):
//!   the five commands' inputs, the draft, the preview channel's events,
//!   the preview's counts and the destination (`ts_guard::M6_EXPORT_TYPES`).
//!   Its only `string`s are the
//!   brands [`DraftId`] (`^[0-9a-f]{32}$`), [`ZoneName`] and [`LocalDate`]
//!   (validated in Rust), the destination's `displayName` (the viewer's
//!   `UntrustedText`: a name the user chose in a native dialog, rendered
//!   by interpolation only), the archive id and job id the shell handed
//!   out. It is disjoint from the archive, backups and viewer walks
//!   except for `ts_guard::M6_SHARED_TYPES`.
//! - **`exportFinished`** ([`ExportSummaryDto`], `ts_guard::M6_EVENT_TYPES`) is a
//!   `JobEvent`, so it stays inside the **archive guard's** walk, which
//!   bans `name`, `path` and `text` and every free string: counts and
//!   codes only.
//!
//! Inputs use `deny_unknown_fields` and are parsed from the raw request
//! (`viewer::input::parse_raw`), so a malformed body is `invalidInput` with
//! a fixed sentence that echoes nothing. No input carries a path, a file
//! name, a message id or a query: the scope names a conversation, dates,
//! the viewer's live search by its id, or the whole archive (I4).

use oma_archive::{ExcludedHits, ExportPreview};
use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::backups::dto::clamp;
use crate::error::ErrorCode;
use crate::viewer::dto::{SafeId, UntrustedText};
use crate::viewer::input::InId;

// ------------------------------------------------------------------ brands

/// A draft's id: 128 random bits as 32 lowercase hex digits. Made in Rust;
/// the webview only echoes it. `Debug` prints no digits.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, TS)]
#[serde(transparent)]
#[ts(export, type = "string & { readonly __draftId: unique symbol }")]
pub struct DraftId(String);

impl DraftId {
    /// A fresh id from the operating system's generator.
    pub fn random() -> Option<DraftId> {
        let mut b = [0u8; 16];
        getrandom::fill(&mut b).ok()?;
        let mut s = String::with_capacity(32);
        for x in b {
            s.push_str(&format!("{x:02x}"));
        }
        Some(DraftId(s))
    }

    /// `Some` only for exactly 32 lowercase hex digits.
    pub fn parse(s: &str) -> Option<DraftId> {
        (s.len() == 32 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
            .then(|| DraftId(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for DraftId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DraftId(..)")
    }
}

impl<'de> Deserialize<'de> for DraftId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        DraftId::parse(&s).ok_or_else(|| de::Error::custom("not a draft id"))
    }
}

/// A local calendar date `YYYY-MM-DD` (years 1900 to 9999), parsed in
/// Rust ([`LocalDate::civil`]).
#[derive(Clone, PartialEq, Eq, Deserialize, TS)]
#[serde(transparent)]
#[ts(export, type = "string")]
pub struct LocalDate(pub String);

impl std::fmt::Debug for LocalDate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LocalDate(..)")
    }
}

impl LocalDate {
    /// The date, when it is `YYYY-MM-DD` with a year from 1900 to 9999
    /// and exists in the proleptic Gregorian calendar.
    pub fn civil(&self) -> Option<oma_export::CivilDate> {
        let s = self.0.as_bytes();
        let digits = |r: std::ops::Range<usize>| -> Option<i64> {
            let part = s.get(r)?;
            if !part.iter().all(u8::is_ascii_digit) {
                return None;
            }
            std::str::from_utf8(part).ok()?.parse().ok()
        };
        if s.len() != 10 || s.get(4) != Some(&b'-') || s.get(7) != Some(&b'-') {
            return None;
        }
        let (y, m, d) = (digits(0..4)?, digits(5..7)?, digits(8..10)?);
        if !(1900..=9999).contains(&y) {
            return None;
        }
        oma_export::CivilDate::new(y, u8::try_from(m).ok()?, u8::try_from(d).ok()?)
    }
}

/// An IANA zone name from the webview (`^[A-Za-z0-9_+\-/]{1,64}$`,
/// checked by `oma_export::ZoneTable::new`). Never logged.
#[derive(Clone, PartialEq, Eq, Deserialize, TS)]
#[serde(transparent)]
#[ts(export, type = "string")]
pub struct ZoneName(pub String);

impl std::fmt::Debug for ZoneName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ZoneName(..)")
    }
}

// ------------------------------------------------------------------ inputs

/// An output format (spec §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ExportFormatInput {
    Txt,
    Csv,
    Json,
    Html,
    /// Refused with `exportUnavailable` until slice 6j.
    Pdf,
    /// Refused with `exportUnavailable` until slice 6i.
    Xlsx,
}

/// What an export covers (spec §9.2). Never a message id.
#[derive(Debug, Clone, Deserialize, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
#[ts(export)]
pub enum ExportScopeInput {
    /// One conversation's whole timeline.
    Conversation {
        archive_id: String,
        #[ts(as = "SafeId")]
        chat_id: InId,
    },
    /// Local dates `[from, until)` in the export's zone, in one
    /// conversation or across the archive.
    DateRange {
        archive_id: String,
        from_local_date: LocalDate,
        until_local_date: LocalDate,
        #[serde(default)]
        #[ts(optional, as = "Option<SafeId>")]
        chat_id: Option<InId>,
    },
    /// The viewer's live search result set, by its id (never the query).
    SearchResults {
        viewer: u32,
        search_id: u32,
        all_matches: bool,
    },
    /// Every conversation, then the messages in no conversation.
    Archive { archive_id: String },
    /// "Save a copy" of one attachment: refused with `exportUnavailable`
    /// in this version.
    Attachment {
        viewer: u32,
        #[ts(as = "SafeId")]
        attachment_id: InId,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum AttachmentModeInput {
    None,
    CopyOriginals,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ClockInput {
    H23,
    H12,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum LineEndingsInput {
    Lf,
    Crlf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum CsvProfileInput {
    Spreadsheet,
    DataTools,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum PageSizeInput {
    A4,
    Letter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ZoneChoiceInput {
    /// This computer's zone (the webview builds its table).
    System,
    Utc,
    /// Another IANA zone the webview supports.
    Named,
}

/// The options of an export (spec §9.2). Every field is required: the
/// dialog sends its whole state, so nothing is defaulted silently.
/// Combinations that do not apply are `invalidInput`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ExportOptionsInput {
    pub attachments: AttachmentModeInput,
    pub keep_extensions: bool,
    pub previews: bool,
    pub larger_previews: bool,
    pub history: bool,
    pub reactions: bool,
    pub events: bool,
    pub reveal_one_time_codes: bool,
    pub reveal_earlier_content: bool,
    pub hidden_character_markers: bool,
    pub seconds: bool,
    pub include_identifiers: bool,
    pub clock: ClockInput,
    pub line_endings: LineEndingsInput,
    /// Required with CSV, refused without it.
    #[serde(default)]
    #[ts(optional)]
    pub csv_profile: Option<CsvProfileInput>,
    /// Required with PDF, refused without it.
    #[serde(default)]
    #[ts(optional)]
    pub page_size: Option<PageSizeInput>,
    pub zone: ZoneChoiceInput,
}

/// The zone offset table the webview builds (spec §4.6): the zone's name,
/// the window it covers and its offsets, each `[utcMs, offsetSeconds]` in
/// force from that instant on (at most 4,096).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ZoneTableInputDto {
    pub name: ZoneName,
    pub window_utc_ms: [i64; 2],
    pub transitions: Vec<[i64; 2]>,
}

/// `preview_export`.
#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PreviewExportRequest {
    pub scope: ExportScopeInput,
    /// One to six, each once.
    pub formats: Vec<ExportFormatInput>,
    pub options: ExportOptionsInput,
    /// Only for a date range in a zone other than UTC, after the preview
    /// asked for it (`needsZoneTable`): its local days need the table
    /// before anything can be counted.
    #[serde(default)]
    #[ts(optional)]
    pub zone: Option<ZoneTableInputDto>,
}

/// `discard_export_draft`.
#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct DraftRef {
    pub draft_id: DraftId,
}

/// `choose_export_destination`.
#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ChooseExportDestinationRequest {
    pub draft_id: DraftId,
    /// Required when the draft's zone is not UTC and it has no table yet;
    /// it must cover `zoneWindowUtcMs` of the preview.
    #[serde(default)]
    #[ts(optional)]
    pub zone: Option<ZoneTableInputDto>,
}

/// The confirmed reveal count ("N messages", D147).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct RevealConfirmationInput {
    pub confirmed_up_to: u64,
}

/// What the user confirmed in Review (spec §9.2).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ExportConfirmationsInput {
    /// Every destination warning that needs a confirmation.
    pub warnings: Vec<ExportWarningDto>,
    /// "Replace '<name>'?" was confirmed for an existing single file.
    pub replace: bool,
    /// Required exactly when a reveal is on.
    pub reveal: Option<RevealConfirmationInput>,
    /// Required exactly when "Keep original file extensions" is on.
    pub keep_risky_extension: bool,
    /// Remove the unfinished exports listed when the folder was chosen.
    pub remove_leftovers: bool,
}

/// `start_export`.
#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct StartExportRequest {
    pub draft_id: DraftId,
    pub confirmations: ExportConfirmationsInput,
}

/// `show_export_in_folder`.
#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ShowExportInFolderRequest {
    pub job_id: String,
}

// ----------------------------------------------------------------- outputs

/// `preview_export`'s answer: the draft is made at once; its counts follow
/// on the channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ExportDraftDto {
    pub draft_id: DraftId,
}

/// One file, or a folder with `README.txt`, `manifest.json` and
/// `SHA256SUMS` (D126).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum PackagingDto {
    SingleFile,
    Bundle,
}

impl From<oma_export::Packaging> for PackagingDto {
    fn from(p: oma_export::Packaging) -> Self {
        match p {
            oma_export::Packaging::SingleFile => PackagingDto::SingleFile,
            oma_export::Packaging::Bundle => PackagingDto::Bundle,
        }
    }
}

/// Search hits an export leaves out, by what would include them (§3.6).
/// Shown in Review and the result only; never written to an output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ExcludedHitsDto {
    pub one_time_codes: u64,
    pub earlier_content: u64,
    pub both: u64,
    pub not_written: u64,
}

impl From<ExcludedHits> for ExcludedHitsDto {
    fn from(e: ExcludedHits) -> Self {
        ExcludedHitsDto {
            one_time_codes: clamp(e.one_time_codes).0,
            earlier_content: clamp(e.earlier_content).0,
            both: clamp(e.both).0,
            not_written: clamp(e.not_written).0,
        }
    }
}

/// A search scope's facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SearchPreviewDto {
    pub kept: u64,
    pub excluded: ExcludedHitsDto,
    pub capped: bool,
    pub newest_kept: bool,
    pub coverage_complete: bool,
    /// The set was re-run with the export's cap ("all matches").
    pub all_matches: bool,
}

/// A preview's exact counts (spec §3.7): counts and sizes only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ExportPreviewDto {
    pub packaging: PackagingDto,
    pub conversations: u64,
    pub timeline_rows: u64,
    pub distinct_messages: u64,
    pub folded_reaction_rows: u64,
    pub undated_excluded: u64,
    pub unassigned: u64,
    pub attachments: u64,
    pub attachments_with_file: u64,
    pub attachments_withheld_unsent: u64,
    pub distinct_files: u64,
    pub file_bytes: u64,
    pub largest_file_bytes: u64,
    /// The reveal-codes confirmation's N.
    pub messages_with_flagged_code: u64,
    /// The reveal-earlier confirmation's N.
    pub messages_with_earlier_content: u64,
    /// A rough size of the output, copied files included (§8.2's space
    /// check uses it); never shown as exact.
    pub estimated_bytes: u64,
    /// The earliest and latest dated timeline row, Unix ms.
    pub span_utc_ms: Option<[i64; 2]>,
    /// The window a zone table must cover (the span and now, clamped);
    /// `null` when the zone is UTC or the table is already attached.
    pub zone_window_utc_ms: Option<[i64; 2]>,
    /// For a search scope.
    pub search: Option<SearchPreviewDto>,
    /// A count above 2^53 − 1 was clamped.
    pub clamped: bool,
}

/// Bytes per timeline row each format adds beyond the text, for the
/// estimate only (rounded up from the goldens' sizes).
fn row_overhead(f: oma_export::Format) -> u64 {
    match f {
        oma_export::Format::Txt => 160,
        oma_export::Format::Csv => 400,
        oma_export::Format::Json => 1_200,
        oma_export::Format::Html => 800,
        oma_export::Format::Pdf | oma_export::Format::Xlsx => 600,
    }
}

impl ExportPreviewDto {
    pub fn build(
        p: &ExportPreview,
        plan: &oma_export::ExportPlan,
        zone_window: Option<(i64, i64)>,
        search: Option<SearchPreviewDto>,
    ) -> ExportPreviewDto {
        let mut clamped = false;
        let mut c = |v: u64| {
            let (v, cut) = clamp(v);
            clamped |= cut;
            v
        };
        let copies = plan.options().attachments == oma_export::AttachmentMode::CopyOriginals;
        let estimate = plan
            .formats()
            .iter()
            .map(|f| {
                p.text_bytes
                    .saturating_add(p.timeline_rows.saturating_mul(row_overhead(*f)))
            })
            .fold(0u64, u64::saturating_add)
            .saturating_add(if copies { p.blob_bytes } else { 0 });
        ExportPreviewDto {
            packaging: plan.packaging().into(),
            conversations: c(p.conversations),
            timeline_rows: c(p.timeline_rows),
            distinct_messages: c(p.distinct_messages),
            folded_reaction_rows: c(p.folded_reaction_rows),
            undated_excluded: c(p.undated_excluded),
            unassigned: c(p.unassigned),
            attachments: c(p.attachments),
            attachments_with_file: c(p.attachments_with_file),
            attachments_withheld_unsent: c(p.attachments_withheld_unsent),
            distinct_files: c(p.distinct_blobs),
            file_bytes: c(p.blob_bytes),
            largest_file_bytes: c(p.largest_blob_bytes),
            messages_with_flagged_code: c(p.messages_with_flagged_code),
            messages_with_earlier_content: c(p.messages_with_earlier_content),
            estimated_bytes: c(estimate),
            span_utc_ms: p.span_utc_ms.map(|(a, b)| [a, b]),
            zone_window_utc_ms: zone_window.map(|(a, b)| [a, b]),
            search,
            clamped,
        }
    }
}

/// What `preview_export`'s channel carries. Exactly one of `ready`,
/// `needsZoneTable` and `failed` ends a draft's count; a discarded draft's
/// count ends with nothing.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum PreviewEvent {
    /// Timeline rows counted so far.
    Progress {
        draft_id: DraftId,
        rows: u64,
    },
    Ready {
        draft_id: DraftId,
        preview: ExportPreviewDto,
    },
    /// A date range in a zone other than UTC: build the zone table for
    /// `windowUtcMs`, then call `preview_export` again with it. Nothing
    /// was counted (`spanUtcMs` is `null`).
    NeedsZoneTable {
        draft_id: DraftId,
        span_utc_ms: Option<[i64; 2]>,
        window_utc_ms: [i64; 2],
    },
    Failed {
        draft_id: DraftId,
        code: ErrorCode,
    },
}

/// A destination warning (§8.2), a closed code. Every one but `fatVolume`
/// must come back in `start_export.confirmations.warnings`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ExportWarningDto {
    /// Linux: other accounts can change this folder while the export is
    /// written (group- or other-writable, no sticky bit).
    SharedFolder,
    /// A cloud sync client's folder: files are uploaded, and cancelling
    /// cannot recall them.
    SyncFolder,
    /// A network or virtualised drive.
    NetworkOrVirtual,
    /// FAT: files of 4 GiB or more are left out (no confirmation needed).
    FatVolume,
    /// The free space may not be enough for the estimate.
    MayNotFit,
}

impl ExportWarningDto {
    /// Whether `start_export` needs it confirmed.
    pub fn needs_confirmation(self) -> bool {
        self != ExportWarningDto::FatVolume
    }
}

/// The sync client whose folder this is (a closed code, never a name the
/// path supplied).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum SyncProviderDto {
    OneDrive,
    Dropbox,
    ICloudDrive,
    GoogleDrive,
    PCloud,
    Nextcloud,
    OwnCloud,
    Mega,
    SyncCom,
    Other,
}

impl SyncProviderDto {
    /// From `oma_fs::cloud_sync_provider`'s fixed table.
    pub fn of(name: &str) -> SyncProviderDto {
        match name {
            "OneDrive" => SyncProviderDto::OneDrive,
            "Dropbox" => SyncProviderDto::Dropbox,
            "iCloud Drive" => SyncProviderDto::ICloudDrive,
            "Google Drive" => SyncProviderDto::GoogleDrive,
            "pCloud" => SyncProviderDto::PCloud,
            "Nextcloud" => SyncProviderDto::Nextcloud,
            "ownCloud" => SyncProviderDto::OwnCloud,
            "MEGA" => SyncProviderDto::Mega,
            "Sync.com" => SyncProviderDto::SyncCom,
            _ => SyncProviderDto::Other,
        }
    }
}

/// Free space as a coarse bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum FreeSpaceDto {
    Under100Mb,
    Under1Gb,
    Under10Gb,
    Under100Gb,
    AtLeast100Gb,
}

impl FreeSpaceDto {
    pub fn of(bytes: u64) -> FreeSpaceDto {
        const MB: u64 = 1 << 20;
        match bytes {
            b if b < 100 * MB => FreeSpaceDto::Under100Mb,
            b if b < 1024 * MB => FreeSpaceDto::Under1Gb,
            b if b < 10 * 1024 * MB => FreeSpaceDto::Under10Gb,
            b if b < 100 * 1024 * MB => FreeSpaceDto::Under100Gb,
            _ => FreeSpaceDto::AtLeast100Gb,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum SpaceVerdictDto {
    Yes,
    No,
    Unknown,
}

/// `choose_export_destination`'s answer (spec §8.1). The location itself
/// stays in Rust; `displayName` is the bundle's or the file's name only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ExportDestinationDto {
    pub packaging: PackagingDto,
    /// The name the export will be published under (a single file's name
    /// with its format's extension appended when needed, or the bundle's
    /// first free name). Render by interpolation only.
    pub display_name: UntrustedText,
    /// A single file of that name exists: "Replace '<name>'?" is needed.
    pub target_exists: bool,
    pub warnings: Vec<ExportWarningDto>,
    pub sync_provider: Option<SyncProviderDto>,
    /// `null`: unknown.
    pub free_space: Option<FreeSpaceDto>,
    pub enough: SpaceVerdictDto,
    /// Unfinished exports found in this folder (§8.5).
    pub leftovers: u32,
}

// ------------------------------------------------- the terminal job event

/// How an export ended when it published its output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ExportOutcomeDto {
    Completed,
    /// Published, but something in scope was left out: an attachment's
    /// file, undated messages of a date range, or search hits that match
    /// only hidden content.
    CompletedWithExclusions,
}

/// The size of everything published, as a bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ExportSizeBucketDto {
    Under1Mb,
    Under10Mb,
    Under100Mb,
    Under1Gb,
    Under10Gb,
    AtLeast10Gb,
}

impl ExportSizeBucketDto {
    pub fn of(bytes: u64) -> ExportSizeBucketDto {
        const MB: u64 = 1 << 20;
        match bytes {
            b if b < MB => ExportSizeBucketDto::Under1Mb,
            b if b < 10 * MB => ExportSizeBucketDto::Under10Mb,
            b if b < 100 * MB => ExportSizeBucketDto::Under100Mb,
            b if b < 1024 * MB => ExportSizeBucketDto::Under1Gb,
            b if b < 10 * 1024 * MB => ExportSizeBucketDto::Under10Gb,
            _ => ExportSizeBucketDto::AtLeast10Gb,
        }
    }
}

/// Attachment records whose file was not written, by why (§4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AttachmentExclusionsDto {
    /// Attachments of unsent parts (withheld unless revealed, §3.8).
    pub withheld_unsent: u64,
    /// The archive holds no file for it (not in the backup, not copied,
    /// unreadable, …).
    pub no_stored_file: u64,
    pub checksum_mismatch: u64,
    /// Over 4 GiB on a FAT volume.
    pub too_large_for_volume: u64,
    pub write_failed: u64,
}

/// `exportFinished`'s summary (spec §9.3): counts and codes only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ExportSummaryDto {
    pub items: u64,
    pub folded_reaction_rows: u64,
    pub conversations: u64,
    /// Every file published (conversation files, tables, copies,
    /// provenance).
    pub files_written: u64,
    /// Distinct attachment files copied.
    pub attachments_copied: u64,
    /// Of those, written with `.blocked` appended (owner Q3b).
    pub attachments_blocked: u64,
    pub attachments_excluded: AttachmentExclusionsDto,
    pub one_time_codes_masked: u64,
    pub earlier_text_withheld: u64,
    pub edit_text_withheld: u64,
    pub alternative_text_withheld: u64,
    pub hidden_characters_marked: u64,
    pub controls_escaped: u64,
    pub neutralised_cells: u64,
    pub times_outside_zone_window: u64,
    pub undated_excluded: u64,
    /// For a search scope: hits left out because they match only hidden
    /// content (shown in the result only, never written).
    pub search_hits_excluded: Option<u64>,
    /// A folder, or one file.
    pub bundle: bool,
    pub size: ExportSizeBucketDto,
    /// A count above 2^53 − 1 was clamped.
    pub clamped: bool,
}

// ------------------------------------------------------------- the guards

/// Every declared type reachable from the export commands' inputs and
/// outputs (the export guard's walk).
#[cfg(test)]
pub(crate) fn guard_walk() -> std::collections::BTreeMap<String, String> {
    let mut w = crate::ts_guard::Walk::new();
    w.root::<PreviewExportRequest>();
    w.root::<ExportDraftDto>();
    w.root::<PreviewEvent>();
    w.root::<ChooseExportDestinationRequest>();
    w.root::<ExportDestinationDto>();
    w.root::<StartExportRequest>();
    w.root::<DraftRef>();
    w.root::<ShowExportInFolderRequest>();
    w.decls()
}
