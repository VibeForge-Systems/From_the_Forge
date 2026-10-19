//! Errors (M6 spec §4.4). None carries a path, a name, an id or content:
//! every variant is a kind, and its `Display` names the kind only.
//!
//! Slice 6d defined the errors the record core can return; slice 6e adds
//! the file and placement errors (`Write`, `NoSpace`, `TargetExists`,
//! `NameRefused`, `DestinationChanged`). Slice 6g adds
//! `HelperOutputRejected` for a decode-helper rendition that fails the
//! checks of §5.7; `HelperFailed` arrives with slice 6i.

use std::io;

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
    /// A file or folder could not be written, read back or removed. The
    /// kind only: never a path.
    #[error("an export file could not be written ({kind:?})")]
    Write { kind: io::ErrorKind },
    /// The destination's volume is full (or over a quota).
    #[error("the destination is full")]
    NoSpace,
    /// Every name the export may publish under is taken (` (2)` to ` (99)`
    /// for a bundle; the chosen name for a single file without a confirmed
    /// replace), or a replace was asked for a target that is not a
    /// regular file.
    #[error("the export's name is already taken")]
    TargetExists,
    /// A name the export was asked to publish under is not one it may
    /// write (`exportNameRefused`): one `sanitize_filename` would change,
    /// or one starting with `.oma-export-`.
    #[error("the export's name was refused")]
    NameRefused,
    /// The destination changed under the export (spec §8.3, D143): the
    /// parent vanished, moved or became a link, or the staging folder is
    /// no longer the one the export created.
    #[error("the destination changed while the export was written")]
    DestinationChanged,
    /// A helper's output failed the main process's checks (spec §5.7): a
    /// decode-helper rendition that is not a well-formed JPEG or PNG
    /// within its size limits. Nothing of it was written.
    #[error("a helper's output was refused")]
    HelperOutputRejected,
}

impl ExportError {
    /// A file-system error as an export error: `NoSpace` for a full volume
    /// or quota, `Write { kind }` otherwise. Never carries the path.
    pub fn io(e: &io::Error) -> ExportError {
        if is_no_space(e) {
            ExportError::NoSpace
        } else {
            ExportError::Write { kind: e.kind() }
        }
    }
}

/// `true` for a full volume or an exceeded quota.
fn is_no_space(e: &io::Error) -> bool {
    if matches!(
        e.kind(),
        io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded
    ) {
        return true;
    }
    // ENOSPC and EDQUOT on Linux; ERROR_HANDLE_DISK_FULL and
    // ERROR_DISK_FULL on Windows (std maps most of these already).
    #[cfg(target_os = "linux")]
    {
        matches!(e.raw_os_error(), Some(28 | 122))
    }
    #[cfg(windows)]
    {
        matches!(e.raw_os_error(), Some(39 | 112))
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        false
    }
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
    /// The writers were driven out of order (an item outside a
    /// conversation file, a conversation inside another, a second
    /// `begin`), or a single-file export asked for a second output.
    RowOrder,
    /// A staged file was still open when the export was finished or
    /// published.
    FileStillOpen,
    /// Two files of one export would have the same name ignoring ASCII
    /// case (a defensive check: ordinal prefixes make names unique).
    NameCollision,
    /// A value could not be serialised (a map key that is not a string).
    NotSerializable,
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
