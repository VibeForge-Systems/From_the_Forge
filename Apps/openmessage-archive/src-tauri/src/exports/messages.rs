//! The fixed sentences of the export codes (M6 spec §9.5), and how an
//! export error becomes an IPC error. No sentence names a path, a file
//! name, a zone, an id, a count or content (I6).

use oma_archive::ArchiveError;
use oma_export::{ExportError, LimitKind};

use crate::error::{ErrorCode, IpcError};
use crate::viewer::messages::{self as vm, NotFound};

pub const DESTINATION_REFUSED: &str =
    "This folder cannot take an export. Choose a folder outside iPhone backups and outside the archive folder that you can write to.";
pub const DESTINATION_NOT_A_FOLDER: &str =
    "The chosen location is not a folder on this computer, or its path is not plain text.";
pub const DESTINATION_NOT_WRITABLE: &str = "The export cannot create files in this folder.";
pub const DESTINATION_INSIDE_BACKUP: &str =
    "This folder is inside an iPhone backup. Exports are never written into a backup.";
pub const DESTINATION_INSIDE_LIBRARY: &str =
    "This folder is inside the archive folder. Choose a folder outside it.";
pub const DESTINATION_CHANGED: &str =
    "The chosen folder changed since it was chosen. Choose where to save again.";
pub const TARGET_EXISTS: &str =
    "An export of that name already exists, and nothing was replaced. Choose another name or confirm the replacement.";
pub const NAME_REFUSED: &str = "Choose a simpler name.";
pub const DRAFT_EXPIRED: &str =
    "This export is no longer prepared (it was closed, replaced or left for 30 minutes). Review the export again.";
pub const CONFIRMATION_STALE: &str =
    "The archive changed or more messages qualify than you confirmed; review the export again.";
pub const NEW_WARNING: &str =
    "Something about the chosen folder changed since it was checked. Review the export again.";
pub const ZONE_TABLE_STALE: &str =
    "The time zone information does not cover this export. Review the export again.";
pub const ZONE_TABLE_REFUSED: &str =
    "The time zone information for this export was refused. Choose another zone or use UTC.";
pub const ZONE_TABLE_NEEDED: &str =
    "This export needs the time zone information first. Review the export again.";
pub const DATE_RANGE_REFUSED: &str =
    "Choose dates from 1900 to one year from now, with the end after the start.";
pub const WRITE_FAILED: &str = "An export file could not be written. Nothing was published.";
pub const NO_SPACE: &str = "The destination is full. Nothing was published.";
pub const NO_SPACE_FOR_FILES: &str =
    "This folder's drive has less free space than the attachment files this export copies. Free some space or choose another folder.";
pub const HELPER_FAILED: &str =
    "A helper process of the export failed, or its output was refused. Nothing was published.";
pub const UNAVAILABLE_FORMAT: &str =
    "PDF and spreadsheet (XLSX) exports are not available in this version.";
pub const UNAVAILABLE_ATTACHMENT: &str =
    "Saving a copy of one attachment is not available in this version.";
pub const UNAVAILABLE_EVENTS: &str =
    "Leaving events out of an export is not available in this version.";
pub const INVALID_EXPORT_INPUT: &str =
    "The export request is not one this version accepts. Review the export again.";
pub const OPTIONS_DO_NOT_APPLY: &str =
    "These export options do not go together. Review the export again.";
pub const REVEAL_NOT_CONFIRMED: &str =
    "Including hidden content needs its count confirmed. Review the export again.";
pub const WARNINGS_NOT_CONFIRMED: &str =
    "The warnings about the chosen folder must be confirmed. Review the export again.";
pub const EXTENSIONS_NOT_CONFIRMED: &str =
    "Keeping original file extensions must be confirmed. Review the export again.";
pub const NOT_PREPARED: &str =
    "Wait for the export's counts and choose where to save it before starting.";
pub const NOT_FOUND: &str =
    "The conversation or message to export is not in this archive. Review the export again.";
pub const NO_FINISHED_EXPORT: &str =
    "There is no finished export of that job to show. Exports are remembered only until the app closes.";
pub const FOLDER_NOT_OPENED: &str = "The file manager could not be opened.";

pub fn new(code: ErrorCode, sentence: &str) -> IpcError {
    IpcError::new(code, sentence)
}

pub fn invalid() -> IpcError {
    IpcError::new(ErrorCode::InvalidInput, INVALID_EXPORT_INPUT)
}

pub fn expired() -> IpcError {
    IpcError::new(ErrorCode::ExportDraftExpired, DRAFT_EXPIRED)
}

pub fn stale() -> IpcError {
    IpcError::new(ErrorCode::ExportConfirmationStale, CONFIRMATION_STALE)
}

pub fn unavailable(sentence: &str) -> IpcError {
    IpcError::new(ErrorCode::ExportUnavailable, sentence)
}

/// An archive error as an export sees it: a missing conversation or
/// message is the scope's fault, not the viewer's.
pub fn archive(e: &ArchiveError) -> IpcError {
    match e {
        ArchiveError::AnchorNotFound => IpcError::new(ErrorCode::InvalidInput, NOT_FOUND),
        ArchiveError::Cancelled => IpcError::internal("cancel reported as an error"),
        other => vm::to_ipc(other, NotFound::Message),
    }
}

/// An export error as an IPC error. `Cancelled` is not an error: the job
/// sends `cancelled` for it, and this maps it to `internal`.
pub fn to_ipc(e: &ExportError) -> IpcError {
    use ErrorCode as C;
    match e {
        ExportError::Archive(a) => archive(a),
        ExportError::ZoneTableStale => IpcError::new(C::ExportZoneTableStale, ZONE_TABLE_STALE),
        ExportError::ZoneTable(_) => IpcError::new(C::InvalidInput, ZONE_TABLE_REFUSED),
        ExportError::DateRange(_) => IpcError::new(C::InvalidInput, DATE_RANGE_REFUSED),
        ExportError::Plan(_) => IpcError::new(C::InvalidInput, OPTIONS_DO_NOT_APPLY),
        ExportError::ConfirmationStale => stale(),
        ExportError::Write { .. } => IpcError::new(C::ExportWriteFailed, WRITE_FAILED),
        ExportError::NoSpace => IpcError::new(C::InsufficientSpace, NO_SPACE),
        ExportError::TargetExists => IpcError::new(C::ExportTargetExists, TARGET_EXISTS),
        ExportError::NameRefused => IpcError::new(C::ExportNameRefused, NAME_REFUSED),
        ExportError::DestinationChanged => {
            IpcError::new(C::ExportDestinationRefused, DESTINATION_CHANGED)
        }
        ExportError::HelperOutputRejected
        | ExportError::HelperFailed(_)
        | ExportError::Limit(LimitKind::FrameTooLarge) => {
            IpcError::new(C::ExportHelperFailed, HELPER_FAILED)
        }
        ExportError::Cancelled => IpcError::internal("cancel reported as an error"),
        ExportError::Limit(_) => IpcError::internal("export invariant"),
        // `ExportError` is non-exhaustive.
        _ => IpcError::internal("unknown export error"),
    }
}

/// The fixed sentence of each export code (the default one where a code
/// has several).
pub fn sentence(code: ErrorCode) -> Option<&'static str> {
    use ErrorCode as C;
    Some(match code {
        C::ExportDestinationRefused => DESTINATION_REFUSED,
        C::ExportTargetExists => TARGET_EXISTS,
        C::ExportNameRefused => NAME_REFUSED,
        C::ExportDraftExpired => DRAFT_EXPIRED,
        C::ExportConfirmationStale => CONFIRMATION_STALE,
        C::ExportZoneTableStale => ZONE_TABLE_STALE,
        C::ExportWriteFailed => WRITE_FAILED,
        C::ExportHelperFailed => HELPER_FAILED,
        C::ExportUnavailable => UNAVAILABLE_FORMAT,
        _ => return None,
    })
}

/// Every sentence here, for the hygiene tests.
#[cfg(test)]
pub(crate) const ALL: &[&str] = &[
    DESTINATION_REFUSED,
    DESTINATION_NOT_A_FOLDER,
    DESTINATION_NOT_WRITABLE,
    DESTINATION_INSIDE_BACKUP,
    DESTINATION_INSIDE_LIBRARY,
    DESTINATION_CHANGED,
    TARGET_EXISTS,
    NAME_REFUSED,
    DRAFT_EXPIRED,
    CONFIRMATION_STALE,
    NEW_WARNING,
    ZONE_TABLE_STALE,
    ZONE_TABLE_REFUSED,
    ZONE_TABLE_NEEDED,
    DATE_RANGE_REFUSED,
    WRITE_FAILED,
    NO_SPACE,
    NO_SPACE_FOR_FILES,
    HELPER_FAILED,
    UNAVAILABLE_FORMAT,
    UNAVAILABLE_ATTACHMENT,
    UNAVAILABLE_EVENTS,
    INVALID_EXPORT_INPUT,
    OPTIONS_DO_NOT_APPLY,
    REVEAL_NOT_CONFIRMED,
    WARNINGS_NOT_CONFIRMED,
    EXTENSIONS_NOT_CONFIRMED,
    NOT_PREPARED,
    NOT_FOUND,
    NO_FINISHED_EXPORT,
    FOLDER_NOT_OPENED,
];
