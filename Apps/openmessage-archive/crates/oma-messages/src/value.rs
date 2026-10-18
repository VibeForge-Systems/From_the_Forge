//! Typed column readers and issue accounting (M3 §5.2).
//!
//! Every reader takes the `ValueRef` SQLite returned and never uses
//! `Row::get::<T>()`, so a storage class that does not fit the column's kind
//! becomes [`Col::Unreadable`], never a conversion error or a silently
//! coerced value. `tests/source_rules.rs` enforces the rule.
//!
//! | Expected | `Integer` | `Real` | `Text` | `Blob` | `Null` |
//! |---|---|---|---|---|---|
//! | Integer / code | `Value` | integral and within `i64`: `Value`, counted `real_in_integer_column`; otherwise `Unreadable(TypeMismatch)` | `Unreadable(TypeMismatch)` | `Unreadable(TypeMismatch)` | `Null` |
//! | Bool | 0 → false, 1 → true, otherwise `Unreadable(BoolOutOfRange)` | as for Integer, then the bool rule | `Unreadable` | `Unreadable` | `Null` |
//! | Time (raw `i64`) | `Value` | as for Integer | `Unreadable` | `Unreadable` | `Null` |
//! | Text | `Unreadable` | `Unreadable` | `SqlText`, counted `invalid_utf8_text` if needed | `SqlText`, counted `blob_in_text_column` | `Null` |
//! | Blob | `Unreadable` | `Unreadable` | bytes, counted `text_in_blob_column` | `Value` | `Null` |
#![deny(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::unreachable,
    clippy::unimplemented,
    clippy::string_slice
)]

use std::collections::BTreeMap;

use rusqlite::types::ValueRef;

use crate::col::{Col, IssueKind};
use crate::redact::{RawBlob, Redacted, SqlText};
use crate::schema::KnownColumn;
use crate::time::CocoaTime;

/// SQLite's storage class of a value (`typeof`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SqlType {
    /// `integer`.
    Integer,
    /// `real`.
    Real,
    /// `text`.
    Text,
    /// `blob`.
    Blob,
}

/// The presence and size of a blob column that is not read (in M3,
/// `chat.properties`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlobSize {
    /// The value's size in bytes (`octet_length`).
    pub bytes: u64,
}

/// Counts of value-level issues in one pass. None of them stops the pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ValueIssues {
    /// Integral reals read as integers.
    pub real_in_integer_column: u64,
    /// Text values that are not valid UTF-8 (kept as bytes, never repaired).
    pub invalid_utf8_text: u64,
    /// Blob values in text columns (kept as bytes).
    pub blob_in_text_column: u64,
    /// Text values in blob columns (kept as bytes).
    pub text_in_blob_column: u64,
    /// Values whose storage class does not fit the column
    /// ([`IssueKind::TypeMismatch`]).
    pub type_mismatch: u64,
    /// Boolean values other than 0 and 1 ([`IssueKind::BoolOutOfRange`]).
    pub bool_out_of_range: u64,
    /// Property lists that do not parse ([`IssueKind::PlistInvalid`]).
    pub plist_invalid: u64,
    /// Property lists over a reading budget ([`IssueKind::PlistOverBudget`]).
    pub plist_over_budget: u64,
    /// Property lists whose root is not a dictionary
    /// ([`IssueKind::PlistNotDictionary`]).
    pub plist_not_dictionary: u64,
    /// Values over their column's cap, in total.
    pub value_oversized: u64,
    /// Values over their column's cap, per column.
    pub oversized_by_column: BTreeMap<KnownColumn, u64>,
}

fn bump(n: &mut u64) {
    *n = n.saturating_add(1);
}

impl ValueIssues {
    /// `true` when nothing was counted.
    pub fn is_empty(&self) -> bool {
        *self == ValueIssues::default()
    }

    /// Add another pass's counts to these.
    pub fn merge(&mut self, other: &ValueIssues) {
        self.real_in_integer_column = self
            .real_in_integer_column
            .saturating_add(other.real_in_integer_column);
        self.invalid_utf8_text = self
            .invalid_utf8_text
            .saturating_add(other.invalid_utf8_text);
        self.blob_in_text_column = self
            .blob_in_text_column
            .saturating_add(other.blob_in_text_column);
        self.text_in_blob_column = self
            .text_in_blob_column
            .saturating_add(other.text_in_blob_column);
        self.type_mismatch = self.type_mismatch.saturating_add(other.type_mismatch);
        self.bool_out_of_range = self
            .bool_out_of_range
            .saturating_add(other.bool_out_of_range);
        self.plist_invalid = self.plist_invalid.saturating_add(other.plist_invalid);
        self.plist_over_budget = self
            .plist_over_budget
            .saturating_add(other.plist_over_budget);
        self.plist_not_dictionary = self
            .plist_not_dictionary
            .saturating_add(other.plist_not_dictionary);
        self.value_oversized = self.value_oversized.saturating_add(other.value_oversized);
        for (c, n) in &other.oversized_by_column {
            let e = self.oversized_by_column.entry(*c).or_insert(0);
            *e = e.saturating_add(*n);
        }
    }

    pub(crate) fn unreadable<T>(&mut self, kind: IssueKind) -> Col<T> {
        match kind {
            IssueKind::TypeMismatch => bump(&mut self.type_mismatch),
            IssueKind::BoolOutOfRange => bump(&mut self.bool_out_of_range),
            IssueKind::PlistInvalid => bump(&mut self.plist_invalid),
            IssueKind::PlistOverBudget => bump(&mut self.plist_over_budget),
            IssueKind::PlistNotDictionary => bump(&mut self.plist_not_dictionary),
        }
        Col::Unreadable(kind)
    }

    fn oversized<T>(&mut self, col: KnownColumn, bytes: u64) -> Col<T> {
        bump(&mut self.value_oversized);
        bump(self.oversized_by_column.entry(col).or_insert(0));
        Col::Oversized { bytes }
    }
}

/// A real that is integral and within `i64`, as that integer.
fn integral(f: f64) -> Option<i64> {
    // -2^63 is exactly representable; 2^63 is the first value out of range.
    const LO: f64 = -9_223_372_036_854_775_808.0;
    const HI: f64 = 9_223_372_036_854_775_808.0;
    if f.is_finite() && f.fract() == 0.0 && (LO..HI).contains(&f) {
        // Integral and in range, so the conversion is exact.
        #[allow(clippy::cast_possible_truncation)]
        Some(f as i64)
    } else {
        None
    }
}

/// The "Integer / code" row.
pub(crate) fn integer(v: ValueRef<'_>, issues: &mut ValueIssues) -> Col<i64> {
    match v {
        ValueRef::Null => Col::Null,
        ValueRef::Integer(i) => Col::Value(i),
        ValueRef::Real(f) => match integral(f) {
            Some(i) => {
                bump(&mut issues.real_in_integer_column);
                Col::Value(i)
            }
            None => issues.unreadable(IssueKind::TypeMismatch),
        },
        ValueRef::Text(_) | ValueRef::Blob(_) => issues.unreadable(IssueKind::TypeMismatch),
    }
}

/// The "Bool" row.
pub(crate) fn boolean(v: ValueRef<'_>, issues: &mut ValueIssues) -> Col<bool> {
    match integer(v, issues) {
        Col::Value(0) => Col::Value(false),
        Col::Value(1) => Col::Value(true),
        Col::Value(_) => issues.unreadable(IssueKind::BoolOutOfRange),
        other => other.map(|_| false),
    }
}

/// The "Time" row: the raw integer, classified per value (§8.1).
pub(crate) fn time(v: ValueRef<'_>, issues: &mut ValueIssues) -> Col<CocoaTime> {
    integer(v, issues).map(CocoaTime::classify)
}

/// The size half of a capped pair: `octet_length("c")`.
fn byte_count(len: ValueRef<'_>) -> u64 {
    match len {
        ValueRef::Integer(n) => u64::try_from(n).unwrap_or(0),
        _ => 0,
    }
}

/// The "Text" row, from `CASE WHEN octet_length("c") <= cap THEN "c" END`
/// and `octet_length("c")`. A NULL value with a non-NULL length is over the
/// cap and was not read.
pub(crate) fn text(
    v: ValueRef<'_>,
    len: ValueRef<'_>,
    col: KnownColumn,
    issues: &mut ValueIssues,
) -> Col<Redacted<SqlText>> {
    match v {
        ValueRef::Null if matches!(len, ValueRef::Null) => Col::Null,
        ValueRef::Null => issues.oversized(col, byte_count(len)),
        ValueRef::Text(b) => {
            if std::str::from_utf8(b).is_err() {
                bump(&mut issues.invalid_utf8_text);
            }
            Col::Value(Redacted::new(SqlText::from_bytes(b)))
        }
        ValueRef::Blob(b) => {
            bump(&mut issues.blob_in_text_column);
            Col::Value(Redacted::new(SqlText::from_bytes(b)))
        }
        ValueRef::Integer(_) | ValueRef::Real(_) => issues.unreadable(IssueKind::TypeMismatch),
    }
}

/// The "Blob" row, from the same capped pair as [`text`].
pub(crate) fn blob(
    v: ValueRef<'_>,
    len: ValueRef<'_>,
    col: KnownColumn,
    issues: &mut ValueIssues,
) -> Col<Redacted<RawBlob>> {
    match v {
        ValueRef::Null if matches!(len, ValueRef::Null) => Col::Null,
        ValueRef::Null => issues.oversized(col, byte_count(len)),
        ValueRef::Blob(b) => Col::Value(Redacted::new(RawBlob::from_bytes(b))),
        ValueRef::Text(b) => {
            bump(&mut issues.text_in_blob_column);
            Col::Value(Redacted::new(RawBlob::from_bytes(b)))
        }
        ValueRef::Integer(_) | ValueRef::Real(_) => issues.unreadable(IssueKind::TypeMismatch),
    }
}

/// A blob read for presence and size only, from `typeof("c")` and
/// `octet_length("c")`. The value is never materialised; over the cap it is
/// still [`Col::Oversized`], so every capped column reports alike.
pub(crate) fn blob_size(
    ty: ValueRef<'_>,
    len: ValueRef<'_>,
    col: KnownColumn,
    issues: &mut ValueIssues,
) -> Col<BlobSize> {
    let bytes = byte_count(len);
    let fits = col.cap().is_none_or(|cap| bytes <= u64::from(cap));
    match sql_type(ty) {
        Some(None) => Col::Null,
        Some(Some(SqlType::Blob)) if fits => Col::Value(BlobSize { bytes }),
        Some(Some(SqlType::Text)) if fits => {
            bump(&mut issues.text_in_blob_column);
            Col::Value(BlobSize { bytes })
        }
        Some(Some(SqlType::Blob | SqlType::Text)) => issues.oversized(col, bytes),
        Some(Some(SqlType::Integer | SqlType::Real)) | None => {
            issues.unreadable(IssueKind::TypeMismatch)
        }
    }
}

/// A column read for its storage class only, from `typeof("c")`.
pub(crate) fn type_only(ty: ValueRef<'_>, issues: &mut ValueIssues) -> Col<SqlType> {
    match sql_type(ty) {
        Some(None) => Col::Null,
        Some(Some(t)) => Col::Value(t),
        None => issues.unreadable(IssueKind::TypeMismatch),
    }
}

/// Parse a `typeof()` result: `Some(None)` for `null`, `None` for anything
/// that is not a `typeof()` result.
fn sql_type(ty: ValueRef<'_>) -> Option<Option<SqlType>> {
    match ty {
        ValueRef::Text(b"null") => Some(None),
        ValueRef::Text(b"integer") => Some(Some(SqlType::Integer)),
        ValueRef::Text(b"real") => Some(Some(SqlType::Real)),
        ValueRef::Text(b"text") => Some(Some(SqlType::Text)),
        ValueRef::Text(b"blob") => Some(Some(SqlType::Blob)),
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)]
mod tests {
    use super::*;
    use crate::schema::known::{chat, handle, message};
    use crate::schema::KnownTable;
    use crate::time::TimeInterp;

    fn id_col() -> KnownColumn {
        KnownColumn::new(KnownTable::Handle, handle::ID)
    }

    /// The Integer, Bool and Time rows of the §5.2 matrix.
    #[test]
    fn the_integer_rows() {
        let mut is = ValueIssues::default();
        assert_eq!(integer(ValueRef::Integer(-4), &mut is), Col::Value(-4));
        assert_eq!(integer(ValueRef::Null, &mut is), Col::Null);
        assert!(is.is_empty());
        assert_eq!(integer(ValueRef::Real(3.0), &mut is), Col::Value(3));
        assert_eq!(is.real_in_integer_column, 1);
        for bad in [
            ValueRef::Real(3.5),
            ValueRef::Real(f64::NAN),
            ValueRef::Real(f64::INFINITY),
            ValueRef::Real(9.3e18),
            ValueRef::Text(b"1"),
            ValueRef::Blob(b"\x01"),
        ] {
            assert_eq!(
                integer(bad, &mut is),
                Col::Unreadable(IssueKind::TypeMismatch)
            );
        }
        assert_eq!(is.type_mismatch, 6);
        assert_eq!(
            integer(ValueRef::Real(-9_223_372_036_854_775_808.0), &mut is),
            Col::Value(i64::MIN)
        );

        let mut is = ValueIssues::default();
        assert_eq!(boolean(ValueRef::Integer(0), &mut is), Col::Value(false));
        assert_eq!(boolean(ValueRef::Integer(1), &mut is), Col::Value(true));
        assert_eq!(boolean(ValueRef::Real(1.0), &mut is), Col::Value(true));
        assert_eq!(boolean(ValueRef::Null, &mut is), Col::Null);
        for out in [2, -1, i64::MAX] {
            assert_eq!(
                boolean(ValueRef::Integer(out), &mut is),
                Col::Unreadable(IssueKind::BoolOutOfRange)
            );
        }
        assert_eq!(
            boolean(ValueRef::Text(b"1"), &mut is),
            Col::Unreadable(IssueKind::TypeMismatch)
        );
        assert_eq!(is.bool_out_of_range, 3);
        assert_eq!(is.type_mismatch, 1);
        assert_eq!(is.real_in_integer_column, 1);

        let mut is = ValueIssues::default();
        let t = time(ValueRef::Integer(0), &mut is);
        assert_eq!(t.value().map(|t| t.interp), Some(TimeInterp::Zero));
        let t = time(ValueRef::Real(1.0), &mut is);
        assert_eq!(t.value().map(|t| t.raw), Some(1));
        assert_eq!(
            time(ValueRef::Blob(b""), &mut is),
            Col::Unreadable(IssueKind::TypeMismatch)
        );
    }

    /// The Text and Blob rows, with the cap pair.
    #[test]
    fn the_text_and_blob_rows() {
        let c = id_col();
        let mut is = ValueIssues::default();
        let v = text(ValueRef::Text(b"abc"), ValueRef::Integer(3), c, &mut is);
        assert_eq!(v.value().map(|t| t.expose().as_bytes()), Some(&b"abc"[..]));
        assert!(is.is_empty());
        assert_eq!(text(ValueRef::Null, ValueRef::Null, c, &mut is), Col::Null);
        assert!(is.is_empty());
        let v = text(ValueRef::Text(&[0xff]), ValueRef::Integer(1), c, &mut is);
        assert_eq!(v.value().map(|t| t.expose().to_str()), Some(None));
        assert_eq!(is.invalid_utf8_text, 1);
        let v = text(ValueRef::Blob(b"xy"), ValueRef::Integer(2), c, &mut is);
        assert!(v.value().is_some());
        assert_eq!(is.blob_in_text_column, 1);
        assert_eq!(
            text(ValueRef::Null, ValueRef::Integer(4097), c, &mut is),
            Col::Oversized { bytes: 4097 }
        );
        assert_eq!(is.value_oversized, 1);
        assert_eq!(is.oversized_by_column.get(&c), Some(&1));
        for bad in [ValueRef::Integer(1), ValueRef::Real(1.5)] {
            assert_eq!(
                text(bad, ValueRef::Integer(1), c, &mut is),
                Col::Unreadable(IssueKind::TypeMismatch)
            );
        }

        let b = KnownColumn::new(KnownTable::Message, message::ATTRIBUTED_BODY);
        let mut is = ValueIssues::default();
        let v = blob(
            ValueRef::Blob(b"\x04\x0b"),
            ValueRef::Integer(2),
            b,
            &mut is,
        );
        assert_eq!(v.value().map(|x| x.expose().len()), Some(2));
        let v = blob(ValueRef::Text(b"t"), ValueRef::Integer(1), b, &mut is);
        assert!(v.value().is_some());
        assert_eq!(is.text_in_blob_column, 1);
        assert_eq!(blob(ValueRef::Null, ValueRef::Null, b, &mut is), Col::Null);
        assert_eq!(
            blob(ValueRef::Null, ValueRef::Integer(1 << 30), b, &mut is),
            Col::Oversized { bytes: 1 << 30 }
        );
        assert_eq!(
            blob(ValueRef::Integer(1), ValueRef::Integer(1), b, &mut is),
            Col::Unreadable(IssueKind::TypeMismatch)
        );
    }

    #[test]
    fn presence_only_readers() {
        let p = KnownColumn::new(KnownTable::Chat, chat::PROPERTIES);
        let cap = u64::from(p.cap().expect("capped"));
        let mut is = ValueIssues::default();
        assert_eq!(
            blob_size(ValueRef::Text(b"blob"), ValueRef::Integer(10), p, &mut is),
            Col::Value(BlobSize { bytes: 10 })
        );
        assert_eq!(
            blob_size(
                ValueRef::Text(b"blob"),
                ValueRef::Integer(i64::try_from(cap).expect("small")),
                p,
                &mut is
            ),
            Col::Value(BlobSize { bytes: cap })
        );
        assert!(is.is_empty());
        assert_eq!(
            blob_size(
                ValueRef::Text(b"blob"),
                ValueRef::Integer(i64::try_from(cap + 1).expect("small")),
                p,
                &mut is
            ),
            Col::Oversized { bytes: cap + 1 }
        );
        assert_eq!(
            blob_size(ValueRef::Text(b"null"), ValueRef::Null, p, &mut is),
            Col::Null
        );
        assert_eq!(
            blob_size(ValueRef::Text(b"text"), ValueRef::Integer(3), p, &mut is),
            Col::Value(BlobSize { bytes: 3 })
        );
        assert_eq!(is.text_in_blob_column, 1);
        assert_eq!(
            blob_size(ValueRef::Text(b"integer"), ValueRef::Integer(1), p, &mut is),
            Col::Unreadable(IssueKind::TypeMismatch)
        );

        let mut is = ValueIssues::default();
        assert_eq!(
            type_only(ValueRef::Text(b"blob"), &mut is),
            Col::Value(SqlType::Blob)
        );
        assert_eq!(type_only(ValueRef::Text(b"null"), &mut is), Col::Null);
        assert_eq!(
            type_only(ValueRef::Text(b"other"), &mut is),
            Col::Unreadable(IssueKind::TypeMismatch)
        );
    }

    #[test]
    fn issues_merge() {
        let c = id_col();
        let mut a = ValueIssues::default();
        let _ = text(ValueRef::Null, ValueRef::Integer(5000), c, &mut a);
        let mut b = a.clone();
        b.merge(&a);
        assert_eq!(b.value_oversized, 2);
        assert_eq!(b.oversized_by_column.get(&c), Some(&2));
    }
}
