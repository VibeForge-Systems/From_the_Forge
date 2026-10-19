//! Errors (M6 spec §4.4). None carries a path, a name, an id or content:
//! every variant is a kind, and its `Display` names the kind only.
//!
//! Slice 6d defines the errors the record core can return. The file,
//! placement and helper errors of §4.4 (`Write`, `NoSpace`, `TargetExists`,
//! `NameRefused`, `DestinationChanged`, `HelperFailed`,
//! `HelperOutputRejected`) arrive with the slices that can raise them.

use oma_archive::ArchiveError;

/// Why an export stopped.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ExportError {
    /// The job's cancel token was set.
    #[error("the export was cancelled")]
    Cancelled,
    /// An archive read failed (its `Display` is content-free too).
    #[error("the archive could not be read: {0}")]
    Archive(ArchiveError),
    /// An instant inside the zone window falls outside the table
    /// (`exportZoneTableStale`, spec §4.6); nothing has been written.
    #[error("the time zone table does not cover this export")]
    ZoneTableStale,
    /// The zone table from the webview failed validation.
    #[error("the time zone table was refused: {0:?}")]
    ZoneTable(ZoneTableRefusal),
    /// A local date range could not be turned into instants.
    #[error("the date range was refused: {0:?}")]
    DateRange(DateRangeRefusal),
    /// The plan failed validation.
    #[error("the export plan was refused: {0:?}")]
    Plan(PlanRefusal),
    /// The pre-count under the lock found more messages to reveal than
    /// the user confirmed (`exportConfirmationStale`, D147).
    #[error("the confirmed counts are out of date")]
    ConfirmationStale,
    /// An internal invariant failed; the export refuses rather than write
    /// a wrong or cut value.
    #[error("an export invariant failed: {0:?}")]
    Limit(LimitKind),
}

impl From<ArchiveError> for ExportError {
    fn from(e: ArchiveError) -> ExportError {
        match e {
            ArchiveError::Cancelled => ExportError::Cancelled,
            other => ExportError::Archive(other),
        }
    }
}

/// The internal invariants of spec §4.4 that the record core checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LimitKind {
    /// An item's extras (chips, reply, reply count) were not loaded.
    ExtrasNotLoaded,
    /// A value assembled under `Caps::EXPORT` still carries a cap marker.
    TruncatedUnderExport,
    /// An `oma-archive` variant this build has no export code for (its
    /// enums are `#[non_exhaustive]`): never exported as something else.
    UnmappedVariant,
    /// A message, attachment or conversation the record references is not
    /// where the scope index or the plan says it is.
    ScopeMismatch,
    /// The assembled lists of one message disagree in length, or a count
    /// disagrees with the rows behind it.
    RowCountMismatch,
    /// A column the record needs was not read (an identifier the opt-in
    /// asked for came back empty).
    ColumnNotRead,
}

/// Why a zone table was refused (spec §4.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ZoneTableRefusal {
    /// The name does not match `^[A-Za-z0-9_+\-/]{1,64}$`.
    BadName,
    /// No transitions, or more than 4,096.
    TransitionCount,
    /// The transitions are not strictly increasing.
    NotIncreasing,
    /// An offset is beyond ±18 hours.
    OffsetOutOfRange,
    /// The window is empty or inverted.
    BadWindow,
    /// The first transition is after the window's start, so the offset
    /// at the start is not known.
    StartNotCovered,
    /// The window does not cover the export's clamped dated span.
    SpanNotCovered,
}

/// Why a local date range was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DateRangeRefusal {
    /// A date that does not exist (month 13, February 30, …).
    InvalidDate,
    /// The end is not after the start.
    Empty,
    /// A bound lies outside the zone table's window.
    OutsideWindow,
}

/// Why a plan was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PlanRefusal {
    /// No format was chosen.
    NoFormat,
    /// A format was listed twice.
    DuplicateFormat,
    /// The identifier opt-in (D141) was chosen without a format that can
    /// carry it (JSON, CSV or XLSX).
    IdentifiersWithoutTabularOrJson,
    /// A reveal was chosen without the count it was confirmed against.
    RevealNotConfirmed,
    /// A count was confirmed for a reveal that is off.
    ConfirmationWithoutReveal,
    /// A CSV profile was chosen without CSV, or CSV without a profile.
    CsvProfileMismatch,
    /// An option was set that does not apply to the chosen formats or
    /// attachments (previews without HTML or PDF, a page size without
    /// PDF, kept extensions without copied files, …).
    OptionDoesNotApply,
}
