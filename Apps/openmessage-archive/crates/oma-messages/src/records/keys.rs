//! Key-only rows (M4 R6): what the archive import's duplicate-key scan
//! reads, and nothing else. No body, summary, filename or other content
//! column is selected, so the scan costs one narrow pass per table.

use crate::col::Col;
use crate::redact::{Redacted, SqlText};

use super::shape_debug;

/// The key and `guid` of one `message`, `chat` or `attachment` row.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct KeyRow {
    /// `ROWID`.
    pub rowid: i64,
    /// `guid`, under its cap, with its storage class.
    pub guid: Col<Redacted<SqlText>>,
}

shape_debug!(KeyRow { guid });

/// The key, `id` and `service` of one `handle` row: sms.db's own
/// uniqueness is `(id, service)`.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct HandleKeyRow {
    /// `ROWID`.
    pub rowid: i64,
    /// `id`: the address.
    pub id: Col<Redacted<SqlText>>,
    /// `service`, raw.
    pub service: Col<Redacted<SqlText>>,
}

shape_debug!(HandleKeyRow { id, service });
