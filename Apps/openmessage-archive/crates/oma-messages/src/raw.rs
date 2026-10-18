//! The raw value of an unreadable cell (M4 R2, gap G4).
//!
//! A [`crate::Col::Unreadable`] says that a cell could not be read as its
//! column's kind (text in an integer column, a real that is not integral, a
//! boolean outside 0 and 1). With raw values kept, the record also carries
//! what SQLite returned for that cell, as a [`RawIssue`], so a later reader
//! can store it without a second pass over the source.
//!
//! - Only cells the reader actually received are kept. Columns read for
//!   presence only (`typeof` and `octet_length`, such as `chat.properties`)
//!   never materialise their value, so they have no raw value.
//! - A text or blob value is kept only up to the column's cap, or
//!   [`crate::CAP_RAW_UNREADABLE`] for a column without one. A larger value
//!   is not kept; the record's `Col` still says it is unreadable.
//! - A property list that does not parse (`message_summary_info`) is not a
//!   raw issue: its bytes are the record's `raw_message_summary_info`.
//!
//! `Debug` prints the storage class and, for text and blobs, the length;
//! never a value, not even an integer.

use std::fmt;

use rusqlite::types::ValueRef;

use crate::caps::CAP_RAW_UNREADABLE;
use crate::redact::{RawBlob, Redacted, SqlText};
use crate::schema::KnownColumn;
use crate::value::SqlType;

/// One value exactly as SQLite returned it, in its storage class.
#[derive(Clone)]
pub enum RawSql {
    /// An `integer`.
    Integer(i64),
    /// A `real`. Equality compares the bit patterns, so it is reflexive
    /// (NaN equals itself).
    Real(f64),
    /// A `text`, as the bytes SQLite returned.
    Text(Redacted<SqlText>),
    /// A `blob`.
    Blob(Redacted<RawBlob>),
}

impl RawSql {
    /// The storage class.
    pub fn sql_type(&self) -> SqlType {
        match self {
            RawSql::Integer(_) => SqlType::Integer,
            RawSql::Real(_) => SqlType::Real,
            RawSql::Text(_) => SqlType::Text,
            RawSql::Blob(_) => SqlType::Blob,
        }
    }

    /// What SQLite returned for a cell, when it is not NULL and fits `cap`
    /// bytes.
    pub(crate) fn capture(v: ValueRef<'_>, cap: u32) -> Option<RawSql> {
        let fits = |len: usize| u64::try_from(len).is_ok_and(|n| n <= u64::from(cap));
        match v {
            ValueRef::Null => None,
            ValueRef::Integer(i) => Some(RawSql::Integer(i)),
            ValueRef::Real(f) => Some(RawSql::Real(f)),
            ValueRef::Text(b) if fits(b.len()) => {
                Some(RawSql::Text(Redacted::new(SqlText::from_bytes(b))))
            }
            ValueRef::Blob(b) if fits(b.len()) => {
                Some(RawSql::Blob(Redacted::new(RawBlob::from_bytes(b))))
            }
            ValueRef::Text(_) | ValueRef::Blob(_) => None,
        }
    }
}

impl PartialEq for RawSql {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (RawSql::Integer(a), RawSql::Integer(b)) => a == b,
            (RawSql::Real(a), RawSql::Real(b)) => a.to_bits() == b.to_bits(),
            (RawSql::Text(a), RawSql::Text(b)) => a == b,
            (RawSql::Blob(a), RawSql::Blob(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for RawSql {}

impl fmt::Debug for RawSql {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RawSql::Integer(_) => f.write_str("Integer"),
            RawSql::Real(_) => f.write_str("Real"),
            RawSql::Text(t) => write!(f, "Text({t:?})"),
            RawSql::Blob(b) => write!(f, "Blob({b:?})"),
        }
    }
}

/// The raw value of one unreadable cell of a record. A record lists its
/// raw issues in the order of their columns' codes.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RawIssue {
    /// The cell's column.
    pub column: KnownColumn,
    /// What SQLite returned, within the column's cap.
    pub raw: RawSql,
}

/// The largest raw value kept for an unreadable cell of `col`.
pub(crate) fn raw_cap(col: KnownColumn) -> u32 {
    col.cap().unwrap_or(CAP_RAW_UNREADABLE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::known::{chat, message};
    use crate::schema::KnownTable;

    #[test]
    fn capture_keeps_the_class_under_the_cap() {
        assert_eq!(RawSql::capture(ValueRef::Null, 8), None);
        assert_eq!(
            RawSql::capture(ValueRef::Integer(-5), 0),
            Some(RawSql::Integer(-5))
        );
        let nan = RawSql::capture(ValueRef::Real(f64::NAN), 0).expect("real");
        assert_eq!(nan, nan.clone());
        assert_eq!(nan.sql_type(), SqlType::Real);
        assert_ne!(RawSql::Real(0.0), RawSql::Real(-0.0));
        assert_ne!(RawSql::Integer(1), RawSql::Real(1.0));
        let t = RawSql::capture(ValueRef::Text(b"SYNTH-TEXT"), 10).expect("fits");
        assert_eq!(t.sql_type(), SqlType::Text);
        assert_eq!(RawSql::capture(ValueRef::Text(b"SYNTH-TEXT"), 9), None);
        let b = RawSql::capture(ValueRef::Blob(b"\x00\x01"), 2).expect("fits");
        assert_eq!(b.sql_type(), SqlType::Blob);
        assert_eq!(RawSql::capture(ValueRef::Blob(b"\x00\x01"), 1), None);
        // Debug prints the class and a length, never the value.
        let shown = format!("{t:?} {b:?} {:?} {nan:?}", RawSql::Integer(9_876_543));
        assert!(
            !shown.contains("SYNTH") && !shown.contains("9876543"),
            "{shown}"
        );
        assert_eq!(
            shown,
            "Text(<redacted: 10 bytes>) Blob(<redacted: 2 bytes>) Integer Real"
        );
    }

    #[test]
    fn caps_follow_the_column() {
        let style = KnownColumn::new(KnownTable::Chat, chat::STYLE);
        assert_eq!(raw_cap(style), CAP_RAW_UNREADABLE);
        let text = KnownColumn::new(KnownTable::Message, message::TEXT);
        assert_eq!(Some(raw_cap(text)), text.cap());
    }
}
