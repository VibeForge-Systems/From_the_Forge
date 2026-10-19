//! Every sentence of the viewer commands, and the mapping from
//! `oma_archive` errors to IPC errors (M5 spec §7.5). No sentence names an
//! id, a query, a filter or a value from the archive, and none echoes an
//! input.

use oma_archive::{ArchiveError, QueryProblem};
use oma_backup::Limits;

use crate::archives::messages as am;
use crate::error::{ErrorCode, IpcError};

pub const VIEWER_CLOSED: &str =
    "The viewer was closed or reopened. Open the archive in the viewer again.";
pub const QUERY_TOO_BROAD: &str =
    "That request needs more work than the viewer allows at once. Narrow it and try again.";
pub const SEARCH_UNAVAILABLE: &str = "Search is not available for this archive because its search index cannot be read. You can still browse conversations. Verify the archive to learn more.";
pub const SEARCH_SUPERSEDED: &str = "A newer search replaced this one.";
pub const MESSAGE_NOT_FOUND: &str = "That message is not in this archive.";
pub const ANCHOR_NOT_FOUND: &str =
    "That message is not in this conversation. Reload the conversation.";
pub const ATTACHMENT_UNAVAILABLE: &str =
    "That attachment is not available in this viewer session. Reload the conversation.";
pub const INVALID_VIEWER_INPUT: &str =
    "The viewer sent a request this version does not accept. Reload the viewer.";
pub const INVALID_ID: &str =
    "The viewer sent an identifier this version does not accept. Reload the viewer.";
pub const INVALID_FILTER: &str = "Conversation filters can be up to 128 bytes long, cannot contain control characters, and cannot be only spaces.";
pub const QUERY_TOO_LONG: &str = "Searches can be up to 512 bytes long.";
pub const QUERY_TOO_MANY_TERMS: &str = "Searches can have up to 16 words.";
pub const QUERY_TERM_TOO_LONG: &str = "Each search word can be up to 128 bytes long.";
pub const QUERY_CONTROL: &str = "Searches cannot contain control characters.";
pub const QUERY_NO_TERMS: &str = "Type a word or a number to search for.";
pub const QUERY_NEEDLE_TOO_LONG: &str = "Exact-text searches can be up to 64 characters long.";
pub const BUSY_VIEWER_LOCATION: &str = "Close the viewer to change the archive location.";

/// Every sentence here (distinct; checked by a test).
pub const ALL: &[&str] = &[
    VIEWER_CLOSED,
    QUERY_TOO_BROAD,
    SEARCH_UNAVAILABLE,
    SEARCH_SUPERSEDED,
    MESSAGE_NOT_FOUND,
    ANCHOR_NOT_FOUND,
    ATTACHMENT_UNAVAILABLE,
    INVALID_VIEWER_INPUT,
    INVALID_ID,
    INVALID_FILTER,
    QUERY_TOO_LONG,
    QUERY_TOO_MANY_TERMS,
    QUERY_TERM_TOO_LONG,
    QUERY_CONTROL,
    QUERY_NO_TERMS,
    QUERY_NEEDLE_TOO_LONG,
    BUSY_VIEWER_LOCATION,
];

pub fn closed() -> IpcError {
    IpcError::new(ErrorCode::ViewerClosed, VIEWER_CLOSED)
}

pub fn invalid_input() -> IpcError {
    IpcError::new(ErrorCode::InvalidInput, INVALID_VIEWER_INPUT)
}

pub fn invalid_id() -> IpcError {
    IpcError::new(ErrorCode::InvalidInput, INVALID_ID)
}

pub fn superseded() -> IpcError {
    IpcError::new(ErrorCode::SearchSuperseded, SEARCH_SUPERSEDED)
}

pub fn attachment_unavailable() -> IpcError {
    IpcError::new(ErrorCode::AttachmentUnavailable, ATTACHMENT_UNAVAILABLE)
}

pub fn query_sentence(p: QueryProblem) -> &'static str {
    match p {
        QueryProblem::TooLong => QUERY_TOO_LONG,
        QueryProblem::TooManyTerms => QUERY_TOO_MANY_TERMS,
        QueryProblem::TermTooLong => QUERY_TERM_TOO_LONG,
        QueryProblem::ControlCharacter => QUERY_CONTROL,
        QueryProblem::NeedleTooLong => QUERY_NEEDLE_TOO_LONG,
        _ => QUERY_NO_TERMS,
    }
}

/// What `ArchiveError::AnchorNotFound` means for a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotFound {
    /// A page anchor: `anchorNotFound`.
    Anchor,
    /// A message, body or attachment id: `messageNotFound`.
    Message,
}

/// A viewer read's error as an IPC error. `Cancelled` outside a search
/// means the session was closing.
pub fn to_ipc(e: &ArchiveError, not_found: NotFound) -> IpcError {
    use ErrorCode as C;
    match e {
        ArchiveError::QueryTooBroad => IpcError::new(C::QueryTooBroad, QUERY_TOO_BROAD),
        ArchiveError::InvalidQuery(p) => IpcError::new(C::InvalidQuery, query_sentence(*p)),
        ArchiveError::SearchUnavailable => IpcError::new(C::SearchUnavailable, SEARCH_UNAVAILABLE),
        ArchiveError::AnchorNotFound => match not_found {
            NotFound::Anchor => IpcError::new(C::AnchorNotFound, ANCHOR_NOT_FOUND),
            NotFound::Message => IpcError::new(C::MessageNotFound, MESSAGE_NOT_FOUND),
        },
        ArchiveError::Cancelled => closed(),
        ArchiveError::Blob(_) => attachment_unavailable(),
        other => am::to_ipc(other, &Limits::default()),
    }
}

/// The fixed sentence of each viewer code.
pub fn sentence(code: ErrorCode) -> Option<&'static str> {
    use ErrorCode as C;
    Some(match code {
        C::ViewerClosed => VIEWER_CLOSED,
        C::QueryTooBroad => QUERY_TOO_BROAD,
        C::SearchUnavailable => SEARCH_UNAVAILABLE,
        C::SearchSuperseded => SEARCH_SUPERSEDED,
        C::MessageNotFound => MESSAGE_NOT_FOUND,
        C::AnchorNotFound => ANCHOR_NOT_FOUND,
        C::AttachmentUnavailable => ATTACHMENT_UNAVAILABLE,
        C::InvalidQuery => QUERY_NO_TERMS,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use oma_archive::BlobProblem;

    #[test]
    fn every_viewer_error_maps_to_its_code() {
        use ErrorCode as C;
        let cases = [
            (
                ArchiveError::QueryTooBroad,
                NotFound::Message,
                C::QueryTooBroad,
            ),
            (
                ArchiveError::InvalidQuery(QueryProblem::TooLong),
                NotFound::Message,
                C::InvalidQuery,
            ),
            (
                ArchiveError::SearchUnavailable,
                NotFound::Message,
                C::SearchUnavailable,
            ),
            (
                ArchiveError::AnchorNotFound,
                NotFound::Anchor,
                C::AnchorNotFound,
            ),
            (
                ArchiveError::AnchorNotFound,
                NotFound::Message,
                C::MessageNotFound,
            ),
            (ArchiveError::Cancelled, NotFound::Message, C::ViewerClosed),
            (
                ArchiveError::Blob(BlobProblem::Missing),
                NotFound::Message,
                C::AttachmentUnavailable,
            ),
            (ArchiveError::Locked, NotFound::Message, C::ArchiveLocked),
            (
                ArchiveError::NeedsMigration { found: 0 },
                NotFound::Message,
                C::ArchiveNeedsUpgrade,
            ),
        ];
        for (e, nf, code) in cases {
            let ipc = to_ipc(&e, nf);
            assert_eq!(ipc.code, code, "{e:?}");
            assert!(!ipc.message.is_empty());
        }
        for p in [
            QueryProblem::TooLong,
            QueryProblem::TooManyTerms,
            QueryProblem::TermTooLong,
            QueryProblem::ControlCharacter,
            QueryProblem::NoTerms,
            QueryProblem::NeedleTooLong,
        ] {
            assert!(ALL.contains(&query_sentence(p)));
        }
    }

    #[test]
    fn every_viewer_code_has_a_distinct_sentence() {
        use ErrorCode as C;
        for c in [
            C::ViewerClosed,
            C::QueryTooBroad,
            C::InvalidQuery,
            C::SearchUnavailable,
            C::SearchSuperseded,
            C::MessageNotFound,
            C::AnchorNotFound,
            C::AttachmentUnavailable,
        ] {
            let s = sentence(c).expect("sentence");
            assert!(ALL.contains(&s), "{c:?}");
        }
        let all: std::collections::HashSet<&str> = ALL.iter().copied().collect();
        assert_eq!(all.len(), ALL.len(), "sentences are distinct");
        for s in ALL {
            assert!(!am::ALL.contains(s), "not an archive sentence");
        }
    }
}
