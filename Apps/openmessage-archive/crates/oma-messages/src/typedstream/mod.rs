//! A clean-room, budgeted, fail-closed decoder for Apple's typedstream
//! (`NSArchiver`) format, as used by sms.db's `message.attributedBody`
//! (M3 §10).
//!
//! Written from format facts only: C. Sardegna's 2025 write-up of the
//! format (T1), the libmagic "Apple/NeXT typedstream" entry (T2), and
//! Apple's type-encoding table and `NSString`/`NSArchiver` documentation
//! (T3, T4). No GPL or LGPL implementation was opened while writing it; see
//! `scripts/fixtures/PROVENANCE.md` and the crate README.
//!
//! - [`reader`]: bytes, the header, integers.
//! - [`types`]: type strings, parsed as a grammar.
//! - [`node`]: layer 1, a generic walk into an index-based tree.
//! - [`foundation`] and [`attributed`]: layer 2, a closed set of Foundation
//!   classes and the attributed string's runs and parts.
//! - [`keys`]: the known attribute keys.
//! - [`stats`]: [`TsBudget`] and [`TsStats`].
//!
//! Every failure is one of the closed [`TsErrorKind`]s, with the byte
//! offset where it was found. The offset is for counting and tests; it is
//! never logged, and `TsError`'s `Debug` and `Display` omit it.
#![deny(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::unreachable,
    clippy::unimplemented,
    clippy::string_slice
)]

mod attributed;
mod foundation;
mod keys;
mod node;
mod reader;
mod stats;
mod types;

use std::fmt;

pub use attributed::{AttributedBody, BodyPart, BodyRun, RunAttrs, RunIssue, SharedValue, ValueId};
pub use keys::{AttrFlag, AttrFlags, AttrKey, AttrKeysPresent};
pub use stats::{TsBudget, TsStats};

/// Why a blob could not be decoded, or why its runs are incomplete. The set
/// is closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TsErrorKind {
    // ---- fatal for the blob ----
    /// Not version 4 with the `streamtyped` signature and system version
    /// 1000 (big-endian `typedstream` included).
    UnsupportedHeader,
    /// The input ends inside a value, or a length runs past the end.
    Truncated,
    /// A head byte that is not valid where it appears.
    UnexpectedTag,
    /// A reference outside its table, to the wrong kind of entry, or to an
    /// entry still under construction.
    BadReference,
    /// A type the decoder does not accept: unions, bitfields, pointers,
    /// arrays of anything but `c`/`C`, and unknown codes.
    UnsupportedType,
    /// Objects plus structs nested deeper than the budget allows.
    TooDeep,
    /// A count over its budget: input size, slots, shared strings, nodes or
    /// runs.
    Budget,
    /// More bytes would be copied out than the budget allows.
    MaterialisationBudget,
    /// Bytes after the root value.
    TrailingBytes,
    /// The root is not an `NSAttributedString` holding an `NSString`.
    NotAttributedString,
    /// The text is not valid UTF-8.
    InvalidUtf8,
    // ---- non-fatal run issues (AttributedBody::run_issue) ----
    /// The runs end short of the text.
    RunLengthMismatch,
    /// A run reaches past the end of the text.
    RunOverflow,
    /// A run boundary falls inside a surrogate pair.
    RunSplitsSurrogate,
    /// A run's dictionary number is out of range or 0, or its dictionary is
    /// nil or unreadable.
    BadRunIndex,
}

impl TsErrorKind {
    /// Every kind, fatal ones first.
    pub const ALL: [TsErrorKind; 15] = [
        TsErrorKind::UnsupportedHeader,
        TsErrorKind::Truncated,
        TsErrorKind::UnexpectedTag,
        TsErrorKind::BadReference,
        TsErrorKind::UnsupportedType,
        TsErrorKind::TooDeep,
        TsErrorKind::Budget,
        TsErrorKind::MaterialisationBudget,
        TsErrorKind::TrailingBytes,
        TsErrorKind::NotAttributedString,
        TsErrorKind::InvalidUtf8,
        TsErrorKind::RunLengthMismatch,
        TsErrorKind::RunOverflow,
        TsErrorKind::RunSplitsSurrogate,
        TsErrorKind::BadRunIndex,
    ];

    /// `true` for the kinds that fail the whole blob.
    pub const fn is_fatal(self) -> bool {
        !matches!(
            self,
            TsErrorKind::RunLengthMismatch
                | TsErrorKind::RunOverflow
                | TsErrorKind::RunSplitsSurrogate
                | TsErrorKind::BadRunIndex
        )
    }

    /// The kind's name, as in the source.
    pub const fn name(self) -> &'static str {
        match self {
            TsErrorKind::UnsupportedHeader => "UnsupportedHeader",
            TsErrorKind::Truncated => "Truncated",
            TsErrorKind::UnexpectedTag => "UnexpectedTag",
            TsErrorKind::BadReference => "BadReference",
            TsErrorKind::UnsupportedType => "UnsupportedType",
            TsErrorKind::TooDeep => "TooDeep",
            TsErrorKind::Budget => "Budget",
            TsErrorKind::MaterialisationBudget => "MaterialisationBudget",
            TsErrorKind::TrailingBytes => "TrailingBytes",
            TsErrorKind::NotAttributedString => "NotAttributedString",
            TsErrorKind::InvalidUtf8 => "InvalidUtf8",
            TsErrorKind::RunLengthMismatch => "RunLengthMismatch",
            TsErrorKind::RunOverflow => "RunOverflow",
            TsErrorKind::RunSplitsSurrogate => "RunSplitsSurrogate",
            TsErrorKind::BadRunIndex => "BadRunIndex",
        }
    }
}

/// A fatal decode failure: its kind, and the byte offset where it was
/// found. `Debug` and `Display` show the kind only.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct TsError {
    kind: TsErrorKind,
    offset: usize,
}

impl TsError {
    pub(crate) fn new(kind: TsErrorKind, offset: usize) -> Self {
        TsError { kind, offset }
    }

    /// The kind.
    pub fn kind(&self) -> TsErrorKind {
        self.kind
    }

    /// The byte offset. Count it; never log it.
    pub fn offset(&self) -> usize {
        self.offset
    }
}

impl fmt::Debug for TsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TsError")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for TsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "typedstream decode failed: {}", self.kind.name())
    }
}

impl std::error::Error for TsError {}

/// Decode an `attributedBody` blob within `budget`.
pub fn decode_attributed_body(data: &[u8], budget: &TsBudget) -> Result<AttributedBody, TsError> {
    decode_attributed_body_with_stats(data, budget).0
}

/// [`decode_attributed_body`], also returning what the decode used. The
/// statistics are returned on failure too; every field stays within
/// `budget`.
pub fn decode_attributed_body_with_stats(
    data: &[u8],
    budget: &TsBudget,
) -> (Result<AttributedBody, TsError>, TsStats) {
    let mut meter = stats::Meter::new(budget, data.len());
    let result = decode(data, &mut meter);
    (result, meter.stats())
}

fn decode(data: &[u8], meter: &mut stats::Meter) -> Result<AttributedBody, TsError> {
    let max = usize::try_from(meter.budget().max_input()).unwrap_or(usize::MAX);
    if data.len() > max {
        return Err(TsError::new(TsErrorKind::Budget, max));
    }
    let mut w = node::Walker::new(data, meter);
    w.header()?;
    let root = w.top_group()?;
    let end = w.pos();
    if !w.at_end() {
        return Err(TsError::new(TsErrorKind::TrailingBytes, end));
    }
    let doc = w.into_doc();
    attributed::interpret(&doc, root, meter, end)
}

#[cfg(test)]
mod vector_tests;
