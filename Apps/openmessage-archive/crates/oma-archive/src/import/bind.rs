//! Source values to archive cells (spec §6.1).
//!
//! Every source field `Col<T>` becomes one [`Cell`]: the value column `x`
//! (or `x_raw` for a timestamp), its state `x_st`, the derived unit and
//! instant of a timestamp, and the `value_issue` rows that belong to it:
//!
//! | `Col` | `x` | `x_st` | `value_issue` rows |
//! |---|---|---|---|
//! | `Value` | the value | NULL | kind 3 when SQLite returned a text cell as a BLOB (R7) |
//! | `Null` | NULL | 1 | none |
//! | `Absent` | NULL | 2 | none |
//! | `Oversized` | NULL | 3 | kind 1 with the byte count |
//! | `Unreadable` | NULL | 4 | kind 16+ (the reason), and kind 2 with the raw value when the reader kept it (R2) |
//!
//! Text is stored as a BLOB of the exact bytes SQLite returned, unrepaired.
//! A cell's `Debug` prints its state only, never a value.

use std::cmp::Ordering;

use oma_messages::{
    BlobSize, CocoaTime, Col, IssueKind, RawBlob, RawSql, Redacted, SqlText, TextClass, TimeInterp,
};
use rusqlite::types::{ToSqlOutput, ValueRef};
use rusqlite::ToSql;
use zeroize::Zeroizing;

use super::keys::{encode, Field};
use crate::schema::codes::{TimeUnit, UnreadableKind, ValueIssueKind, ValueState};

/// A stored value: the bytes of a text or blob column, or an integer.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum Val {
    Bytes(Zeroizing<Vec<u8>>),
    Int(i64),
}

impl Val {
    pub(crate) fn bytes(b: &[u8]) -> Val {
        Val::Bytes(Zeroizing::new(b.to_vec()))
    }

    fn field(&self) -> Field<'_> {
        match self {
            Val::Bytes(b) => Field::Bytes(b),
            Val::Int(i) => Field::Int(*i),
        }
    }

    /// The bytes, for a text or blob column.
    pub(crate) fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Val::Bytes(b) => Some(b),
            Val::Int(_) => None,
        }
    }
}

impl ToSql for Val {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(match self {
            Val::Bytes(b) => ToSqlOutput::Borrowed(ValueRef::Blob(b)),
            Val::Int(i) => ToSqlOutput::Borrowed(ValueRef::Integer(*i)),
        })
    }
}

/// A value of the `ANY` columns `value_issue.raw` and `value_history.value`,
/// in its storage class. Text keeps its exact bytes, even when they are not
/// UTF-8.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum Raw {
    Null,
    Int(i64),
    /// A real, by its bit pattern (so equality is reflexive).
    Real(u64),
    Text(Zeroizing<Vec<u8>>),
    Blob(Zeroizing<Vec<u8>>),
}

impl Raw {
    pub(crate) fn from_ref(v: ValueRef<'_>) -> Raw {
        match v {
            ValueRef::Null => Raw::Null,
            ValueRef::Integer(i) => Raw::Int(i),
            ValueRef::Real(f) => Raw::Real(f.to_bits()),
            ValueRef::Text(b) => Raw::Text(Zeroizing::new(b.to_vec())),
            ValueRef::Blob(b) => Raw::Blob(Zeroizing::new(b.to_vec())),
        }
    }

    fn from_sql(r: &RawSql) -> Raw {
        match r {
            RawSql::Integer(i) => Raw::Int(*i),
            RawSql::Real(f) => Raw::Real(f.to_bits()),
            RawSql::Text(t) => Raw::Text(Zeroizing::new(t.expose().as_bytes().to_vec())),
            RawSql::Blob(b) => Raw::Blob(Zeroizing::new(b.expose().as_bytes().to_vec())),
        }
    }

    /// An order over raw values, for sorting issue rows deterministically.
    fn order(&self) -> (u8, &[u8], i64, u64) {
        match self {
            Raw::Null => (0, &[], 0, 0),
            Raw::Int(i) => (1, &[], *i, 0),
            Raw::Real(b) => (2, &[], 0, *b),
            Raw::Text(b) => (3, b, 0, 0),
            Raw::Blob(b) => (4, b, 0, 0),
        }
    }
}

impl ToSql for Raw {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::Borrowed(match self {
            Raw::Null => ValueRef::Null,
            Raw::Int(i) => ValueRef::Integer(*i),
            Raw::Real(b) => ValueRef::Real(f64::from_bits(*b)),
            Raw::Text(b) => ValueRef::Text(b),
            Raw::Blob(b) => ValueRef::Blob(b),
        }))
    }
}

/// One `value_issue` row of a cell.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Issue {
    pub(crate) kind: i64,
    pub(crate) bytes: Option<i64>,
    pub(crate) raw: Raw,
}

impl Issue {
    fn cmp_key(&self, other: &Issue) -> Ordering {
        self.kind
            .cmp(&other.kind)
            .then(self.bytes.cmp(&other.bytes))
            .then_with(|| self.raw.order().cmp(&other.raw.order()))
    }
}

/// One source field, as the archive stores it.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Cell {
    /// `x`, or `x_raw` of a timestamp; `None` when `st` says why.
    pub(crate) value: Option<Val>,
    /// `x_st`; `None` when `value` holds the value.
    pub(crate) st: Option<ValueState>,
    /// `x_unit` of a timestamp with a value.
    pub(crate) unit: Option<TimeUnit>,
    /// `x_utc_ns` of a timestamp in seconds or nanoseconds.
    pub(crate) utc_ns: Option<i64>,
    /// The cell's `value_issue` rows, sorted.
    pub(crate) issues: Vec<Issue>,
}

impl std::fmt::Debug for Cell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = match self.st {
            None => "value",
            Some(ValueState::Null) => "null",
            Some(ValueState::Absent) => "absent",
            Some(ValueState::Oversized) => "oversized",
            Some(ValueState::Unreadable) => "unreadable",
        };
        write!(f, "Cell({state}, {} issues)", self.issues.len())
    }
}

/// The `value_issue` code of an unreadable kind.
pub(crate) fn unreadable_kind(k: IssueKind) -> UnreadableKind {
    match k {
        IssueKind::TypeMismatch => UnreadableKind::TypeMismatch,
        IssueKind::BoolOutOfRange => UnreadableKind::BoolOutOfRange,
        IssueKind::PlistInvalid => UnreadableKind::PlistInvalid,
        IssueKind::PlistOverBudget => UnreadableKind::PlistOverBudget,
        IssueKind::PlistNotDictionary => UnreadableKind::PlistNotDictionary,
        _ => UnreadableKind::Unrecognised,
    }
}

/// The `oma-messages` issue kind of a stored unreadable code (the inverse
/// of [`unreadable_kind`]; an unrecognised code reads as a type mismatch,
/// which no derivation distinguishes).
fn issue_kind(code: i64) -> IssueKind {
    match UnreadableKind::from_code(code) {
        Some(UnreadableKind::BoolOutOfRange) => IssueKind::BoolOutOfRange,
        Some(UnreadableKind::PlistInvalid) => IssueKind::PlistInvalid,
        Some(UnreadableKind::PlistOverBudget) => IssueKind::PlistOverBudget,
        Some(UnreadableKind::PlistNotDictionary) => IssueKind::PlistNotDictionary,
        _ => IssueKind::TypeMismatch,
    }
}

impl Cell {
    /// The cell's state as a `Col` with no value: `None` for a value.
    fn col_state<T>(&self) -> Option<Col<T>> {
        match self.st? {
            ValueState::Null => Some(Col::Null),
            ValueState::Absent => Some(Col::Absent),
            ValueState::Oversized => Some(Col::Oversized {
                bytes: self
                    .issue(ValueIssueKind::Oversized.code())
                    .and_then(|i| i.bytes)
                    .and_then(|b| u64::try_from(b).ok())
                    .unwrap_or(0),
            }),
            ValueState::Unreadable => Some(Col::Unreadable(issue_kind(
                self.issues
                    .iter()
                    .find(|i| i.kind >= crate::schema::codes::ISSUE_KIND_BASE)
                    .map_or(0, |i| i.kind),
            ))),
        }
    }

    /// An integer cell (stored or observed) as the `Col` the reader would
    /// give, for re-deriving a column from stored raw values.
    pub(crate) fn as_col_i64(&self) -> Col<i64> {
        match (&self.value, self.col_state()) {
            (Some(Val::Int(i)), _) => Col::Value(*i),
            (_, Some(c)) => c,
            // A byte value in an integer column cannot be stored (STRICT).
            (_, None) => Col::Unreadable(IssueKind::TypeMismatch),
        }
    }

    /// A text cell as the `Col` the reader would give, with its storage
    /// class.
    pub(crate) fn as_col_text(&self) -> Col<Redacted<SqlText>> {
        match (&self.value, self.col_state()) {
            (Some(Val::Bytes(b)), _) => {
                let class = if self.blob_class() {
                    TextClass::Blob
                } else {
                    TextClass::Text
                };
                Col::Value(Redacted::new(SqlText::from_bytes_with_class(b, class)))
            }
            (_, Some(c)) => c,
            (_, None) => Col::Unreadable(IssueKind::TypeMismatch),
        }
    }

    /// The instant of a timestamp cell in seconds or nanoseconds,
    /// re-derived from its raw integer (a stored cell carries no unit).
    pub(crate) fn time_utc_ns(&self) -> Option<i64> {
        match self.value {
            Some(Val::Int(raw)) => match CocoaTime::classify(raw).interp {
                TimeInterp::Seconds { unix_ns } | TimeInterp::Nanoseconds { unix_ns } => {
                    Some(unix_ns)
                }
                _ => None,
            },
            _ => None,
        }
    }

    fn with_state<T>(c: &Col<T>, value: impl FnOnce(&T) -> (Val, Vec<Issue>)) -> Cell {
        let (value, st, issues) = match c {
            Col::Value(v) => {
                let (val, issues) = value(v);
                (Some(val), None, issues)
            }
            Col::Null => (None, Some(ValueState::Null), Vec::new()),
            Col::Absent => (None, Some(ValueState::Absent), Vec::new()),
            Col::Oversized { bytes } => (
                None,
                Some(ValueState::Oversized),
                vec![Issue {
                    kind: ValueIssueKind::Oversized.code(),
                    bytes: Some(i64::try_from(*bytes).unwrap_or(i64::MAX)),
                    raw: Raw::Null,
                }],
            ),
            Col::Unreadable(k) => (
                None,
                Some(ValueState::Unreadable),
                vec![Issue {
                    kind: unreadable_kind(*k).code(),
                    bytes: None,
                    raw: Raw::Null,
                }],
            ),
        };
        Cell {
            value,
            st,
            unit: None,
            utc_ns: None,
            issues,
        }
    }

    /// A text column: the exact bytes, and a kind-3 issue for a BLOB cell.
    pub(crate) fn text(c: &Col<Redacted<SqlText>>) -> Cell {
        Cell::with_state(c, |t| {
            let t = t.expose();
            let issues = if t.class() == TextClass::Blob {
                vec![Issue {
                    kind: ValueIssueKind::TextStoredAsBlob.code(),
                    bytes: None,
                    raw: Raw::Null,
                }]
            } else {
                Vec::new()
            };
            (Val::bytes(t.as_bytes()), issues)
        })
    }

    /// An integer column.
    pub(crate) fn int(c: &Col<i64>) -> Cell {
        Cell::with_state(c, |i| (Val::Int(*i), Vec::new()))
    }

    /// A boolean column, stored as 0 or 1.
    pub(crate) fn boolean(c: &Col<bool>) -> Cell {
        Cell::with_state(c, |b| (Val::Int(i64::from(*b)), Vec::new()))
    }

    /// A blob column: the exact bytes. Blob columns take either storage
    /// class, so no class issue is recorded.
    pub(crate) fn blob(c: &Col<Redacted<RawBlob>>) -> Cell {
        Cell::with_state(c, |b| (Val::bytes(b.expose().as_bytes()), Vec::new()))
    }

    /// A column read for its size only (`chat.properties`).
    pub(crate) fn blob_size(c: &Col<BlobSize>) -> Cell {
        Cell::with_state(c, |s| {
            (
                Val::Int(i64::try_from(s.bytes).unwrap_or(i64::MAX)),
                Vec::new(),
            )
        })
    }

    /// A timestamp: the raw integer, its per-value unit and, for seconds and
    /// nanoseconds, the instant.
    pub(crate) fn time(c: &Col<CocoaTime>) -> Cell {
        let mut cell = Cell::with_state(c, |t| (Val::Int(t.raw), Vec::new()));
        if let Col::Value(t) = c {
            let (unit, utc) = match t.interp {
                TimeInterp::Zero => (TimeUnit::Zero, None),
                TimeInterp::Negative => (TimeUnit::Negative, None),
                TimeInterp::Seconds { unix_ns } => (TimeUnit::Seconds, Some(unix_ns)),
                TimeInterp::Nanoseconds { unix_ns } => (TimeUnit::Nanoseconds, Some(unix_ns)),
                // Ambiguous, or a classification added later: no instant.
                _ => (TimeUnit::Ambiguous, None),
            };
            cell.unit = Some(unit);
            cell.utc_ns = utc;
        }
        cell
    }

    /// Add the raw value an unreadable cell had (R2), as a kind-2 issue.
    pub(crate) fn keep_raw(&mut self, raw: &RawSql) {
        if self.st == Some(ValueState::Unreadable) {
            self.issues.push(Issue {
                kind: ValueIssueKind::RawKept.code(),
                bytes: None,
                raw: Raw::from_sql(raw),
            });
            self.sort_issues();
        }
    }

    pub(crate) fn sort_issues(&mut self) {
        self.issues.sort_by(Issue::cmp_key);
    }

    /// The state code stored in `x_st`.
    pub(crate) fn st_code(&self) -> Option<i64> {
        self.st.map(ValueState::code)
    }

    /// A Value or Null: an observation that may replace a stored value.
    pub(crate) fn is_strong(&self) -> bool {
        matches!(self.st, None | Some(ValueState::Null))
    }

    /// A text value SQLite returned as a BLOB.
    pub(crate) fn blob_class(&self) -> bool {
        self.issues
            .iter()
            .any(|i| i.kind == ValueIssueKind::TextStoredAsBlob.code())
    }

    /// The tagged field of this cell (the key and history encodings).
    pub(crate) fn field(&self) -> Field<'_> {
        match (&self.value, self.st) {
            (Some(v), _) => v.field(),
            (None, Some(ValueState::Null)) => Field::Null,
            (None, Some(ValueState::Oversized)) => Field::Oversized(
                self.issue(ValueIssueKind::Oversized.code())
                    .and_then(|i| i.bytes)
                    .and_then(|b| u64::try_from(b).ok())
                    .unwrap_or(0),
            ),
            (None, Some(ValueState::Unreadable)) => Field::Unreadable(
                self.issues
                    .iter()
                    .find(|i| i.kind >= crate::schema::codes::ISSUE_KIND_BASE)
                    .and_then(|i| u8::try_from(i.kind).ok())
                    .unwrap_or(0),
            ),
            (None, _) => Field::Absent,
        }
    }

    fn issue(&self, kind: i64) -> Option<&Issue> {
        self.issues.iter().find(|i| i.kind == kind)
    }

    /// The pair and the storage class, as the history identity of a
    /// Value or Null observation: the tagged field, then one class byte.
    pub(crate) fn history_encoding(&self) -> Vec<u8> {
        let mut out = Vec::new();
        encode(&mut out, self.field());
        out.push(u8::from(self.blob_class()));
        out
    }

    /// The value as `value_history.value` stores it: text in its storage
    /// class, an integer, or NULL for an observed SQL NULL.
    pub(crate) fn history_value(&self) -> Raw {
        match &self.value {
            Some(Val::Bytes(b)) if self.blob_class() => Raw::Blob(b.clone()),
            Some(Val::Bytes(b)) => Raw::Text(b.clone()),
            Some(Val::Int(i)) => Raw::Int(*i),
            None => Raw::Null,
        }
    }

    /// Append this cell to a fingerprint input: the column code, the tagged
    /// field, the storage class of a text value, and the raw value of an
    /// unreadable cell, so rows with equal fingerprints are identical in
    /// every raw field.
    pub(crate) fn fp_encode(&self, code: u16, out: &mut Vec<u8>) {
        out.extend_from_slice(&code.to_be_bytes());
        encode(out, self.field());
        out.push(u8::from(self.blob_class()));
        if let Some(i) = self.issue(ValueIssueKind::RawKept.code()) {
            match &i.raw {
                Raw::Null => out.push(0),
                Raw::Int(v) => {
                    out.push(1);
                    out.extend_from_slice(&v.to_be_bytes());
                }
                Raw::Real(b) => {
                    out.push(2);
                    out.extend_from_slice(&b.to_be_bytes());
                }
                Raw::Text(b) | Raw::Blob(b) => {
                    out.push(if matches!(i.raw, Raw::Text(_)) { 3 } else { 4 });
                    encode(out, Field::Bytes(b));
                }
            }
        } else {
            out.push(0xff);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oma_messages::SqlText;

    fn text(b: &[u8], class: TextClass) -> Col<Redacted<SqlText>> {
        Col::Value(Redacted::new(SqlText::from_bytes_with_class(b, class)))
    }

    #[test]
    fn every_state_becomes_the_right_pair_and_issues() {
        let v = Cell::text(&text(b"SYNTH-TEXT", TextClass::Text));
        assert_eq!(
            v.value.as_ref().and_then(Val::as_bytes),
            Some(&b"SYNTH-TEXT"[..])
        );
        assert_eq!(v.st_code(), None);
        assert!(v.issues.is_empty());

        let b = Cell::text(&text(b"\xff\xfe", TextClass::Blob));
        assert_eq!(
            b.value.as_ref().and_then(Val::as_bytes),
            Some(&b"\xff\xfe"[..])
        );
        assert_eq!(b.issues.len(), 1);
        assert_eq!(b.issues[0].kind, 3);
        assert!(b.blob_class());
        assert_ne!(
            b.history_encoding(),
            Cell::text(&text(b"\xff\xfe", TextClass::Text)).history_encoding()
        );

        let n = Cell::text(&Col::Null);
        assert_eq!((n.value.is_none(), n.st_code()), (true, Some(1)));
        let a = Cell::int(&Col::Absent);
        assert_eq!(a.st_code(), Some(2));
        assert!(!a.is_strong() && n.is_strong() && v.is_strong());
        let o = Cell::text(&Col::Oversized { bytes: 300 });
        assert_eq!(o.st_code(), Some(3));
        assert_eq!((o.issues[0].kind, o.issues[0].bytes), (1, Some(300)));
        let mut u = Cell::int(&Col::Unreadable(IssueKind::TypeMismatch));
        assert_eq!(u.st_code(), Some(4));
        assert_eq!(u.issues[0].kind, 16);
        u.keep_raw(&RawSql::Real(1.5));
        assert_eq!(u.issues.len(), 2);
        assert_eq!(u.issues[0].kind, 2, "sorted by kind");
        assert!(u.issues[0].raw == Raw::Real(1.5f64.to_bits()));
        // keep_raw on a readable cell does nothing.
        let mut v2 = v.clone();
        v2.keep_raw(&RawSql::Integer(1));
        assert_eq!(v2, v);
        // Booleans are 0 or 1; sizes are byte counts.
        assert!(Cell::boolean(&Col::Value(true)).value == Some(Val::Int(1)));
        assert!(Cell::blob_size(&Col::Value(BlobSize { bytes: 7 })).value == Some(Val::Int(7)));
        // Debug shows the state only.
        assert_eq!(format!("{v:?}"), "Cell(value, 0 issues)");
        assert!(!format!("{b:?} {u:?}").contains("SYNTH"));
    }

    #[test]
    fn every_time_interpretation_becomes_a_quad() {
        let cases = [
            (0i64, TimeUnit::Zero, false),
            (-1, TimeUnit::Negative, false),
            (700_000_000, TimeUnit::Seconds, true),
            (700_000_000_000_000_000, TimeUnit::Nanoseconds, true),
            (5_000_000_000, TimeUnit::Ambiguous, false),
            (i64::MAX, TimeUnit::Ambiguous, false),
            (i64::MIN, TimeUnit::Negative, false),
        ];
        for (raw, unit, has_utc) in cases {
            let c = Cell::time(&Col::Value(CocoaTime::classify(raw)));
            assert!(c.value == Some(Val::Int(raw)), "{raw}");
            assert_eq!(c.unit, Some(unit), "{raw}");
            assert_eq!(c.utc_ns.is_some(), has_utc, "{raw}");
        }
        let n = Cell::time(&Col::Null);
        assert_eq!((n.unit, n.utc_ns, n.st_code()), (None, None, Some(1)));
    }

    #[test]
    fn cells_convert_back_to_the_reader_cols() {
        for c in [
            Col::Value(7i64),
            Col::Null,
            Col::Absent,
            Col::Oversized { bytes: 12 },
            Col::Unreadable(IssueKind::BoolOutOfRange),
        ] {
            assert_eq!(Cell::int(&c).as_col_i64(), c);
        }
        for c in [
            text(b"SYNTH-T", TextClass::Text),
            text(b"SYNTH-B", TextClass::Blob),
            Col::Null,
            Col::Absent,
            Col::Oversized { bytes: 99 },
            Col::Unreadable(IssueKind::TypeMismatch),
        ] {
            let back = Cell::text(&c).as_col_text();
            assert_eq!(back.shape(), c.shape());
            if let (Col::Value(a), Col::Value(b)) = (&back, &c) {
                assert_eq!(a.expose().as_bytes(), b.expose().as_bytes());
                assert_eq!(a.expose().class(), b.expose().class());
            }
        }
        let ns = Cell::time(&Col::Value(CocoaTime::classify(700_000_000)));
        let stored = Cell {
            unit: None,
            utc_ns: None,
            ..ns.clone()
        };
        assert_eq!(stored.time_utc_ns(), ns.utc_ns);
        assert_eq!(
            Cell::time(&Col::Value(CocoaTime::classify(0))).time_utc_ns(),
            None
        );
        let b = Cell::blob(&Col::Value(Redacted::new(RawBlob::from_bytes(b"\x00\x01"))));
        assert!(b.issues.is_empty() && b.value == Some(Val::bytes(b"\x00\x01")));
    }

    #[test]
    fn integer_extremes_round_trip() {
        for i in [i64::MIN, -1, 0, 1, i64::MAX] {
            let c = Cell::int(&Col::Value(i));
            assert!(c.value == Some(Val::Int(i)));
            assert!(c.history_value() == Raw::Int(i));
        }
    }

    #[test]
    fn fingerprint_inputs_separate_states_classes_and_raw_values() {
        let enc = |c: &Cell| {
            let mut v = Vec::new();
            c.fp_encode(101, &mut v);
            v
        };
        let mut u1 = Cell::int(&Col::Unreadable(IssueKind::TypeMismatch));
        let mut u2 = u1.clone();
        u1.keep_raw(&RawSql::Real(1.5));
        u2.keep_raw(&RawSql::Real(2.5));
        let cells = [
            Cell::text(&text(b"", TextClass::Text)),
            Cell::text(&text(b"", TextClass::Blob)),
            Cell::text(&Col::Null),
            Cell::text(&Col::Absent),
            Cell::int(&Col::Unreadable(IssueKind::TypeMismatch)),
            u1,
            u2,
        ];
        for (i, a) in cells.iter().enumerate() {
            for b in cells.iter().skip(i + 1) {
                assert_ne!(enc(a), enc(b), "{a:?} {b:?}");
            }
        }
    }

    /// The persisted unkeyed fingerprints, pinned byte for byte: one handle
    /// row and one chat row with every cell state (Value, Null, Absent,
    /// Oversized, Unreadable with and without a kept raw value, BLOB-class
    /// text). The expected digests were computed outside this crate from
    /// the documented encoding (Python hashlib over the tagged fields), so
    /// a change to `fp_encode`, the column codes or the domain fails here.
    /// Such a change would make a re-import insert a second copy of every
    /// unkeyed row.
    #[test]
    fn unkeyed_fingerprints_are_pinned_byte_for_byte() {
        use crate::import::keys::fingerprint;
        use crate::schema::codes::{Entity, ARCHIVE_COLUMNS};
        use oma_messages::{CocoaTime, RawSql};
        use sha2::{Digest, Sha256};
        let unreadable = |raw: Option<RawSql>| {
            let mut c = Cell::text(&Col::Unreadable(IssueKind::TypeMismatch));
            if let Some(r) = raw {
                c.keep_raw(&r);
            }
            c
        };
        let codes = |e: Entity| -> Vec<u16> {
            ARCHIVE_COLUMNS
                .iter()
                .filter(|c| c.entity == e)
                .map(|c| c.code)
                .collect()
        };
        let encode_row = |codes: &[u16], cells: &[Cell]| {
            assert_eq!(codes.len(), cells.len());
            let mut out = Vec::new();
            for (code, cell) in codes.iter().zip(cells) {
                cell.fp_encode(*code, &mut out);
            }
            out
        };
        let handle_codes = codes(Entity::Handle);
        assert_eq!(handle_codes, [101, 102, 103, 104, 105]);
        let handle = [
            Cell::text(&Col::Null),
            Cell::text(&text(b"SMS", TextClass::Text)),
            Cell::text(&Col::Oversized { bytes: 300 }),
            unreadable(Some(RawSql::Integer(7))),
            Cell::text(&text(b"p", TextClass::Blob)),
        ];
        let chat_codes = codes(Entity::Chat);
        assert_eq!(chat_codes, (201..=214).collect::<Vec<u16>>());
        let chat = [
            Cell::text(&Col::Absent),
            Cell::int(&Col::Value(45)),
            Cell::text(&text(b"", TextClass::Text)),
            unreadable(Some(RawSql::Real(1.5))),
            Cell::text(&Col::Null),
            Cell::text(&text(b"r\xff", TextClass::Text)),
            unreadable(Some(RawSql::Text(Redacted::new(SqlText::from_bytes(b"t"))))),
            Cell::blob_size(&Col::Oversized { bytes: 5000 }),
            Cell::boolean(&Col::Value(true)),
            Cell::boolean(&Col::Absent),
            Cell::boolean(&Col::Null),
            Cell::boolean(&Col::Value(false)),
            Cell::boolean(&Col::Unreadable(IssueKind::BoolOutOfRange)),
            Cell::time(&Col::Value(CocoaTime::classify(0))),
        ];
        let hex = |b: &[u8]| crate::id::to_hex(b);
        for (entity, enc, len, enc_sha, fp) in [
            (
                Entity::Handle,
                encode_row(&handle_codes, &handle),
                62,
                "7c79fe442ffbfae9788aa75a71fea8d8667f20e337f97c873bcbea4014ba7a3f",
                "18bfd2acdbfc95ac5567aa0c9109b3077c7e8eb43fa08796e178df25db805f83",
            ),
            (
                Entity::Chat,
                encode_row(&chat_codes, &chat),
                149,
                "855713b2a4cc67dc5250a7a2fcbf1cc8a6f22b63845860fa5ec7dca1b0c0e4c2",
                "dd731250156acdc25375d3b32482a8217b79cbad01f9accf84e02aeac83d6218",
            ),
        ] {
            assert_eq!(enc.len(), len, "{entity:?}");
            assert_eq!(hex(&Sha256::digest(&enc)), enc_sha, "{entity:?} fp_encode");
            let code = u8::try_from(entity.code()).expect("code");
            assert_eq!(hex(&fingerprint(code, &enc)), fp, "{entity:?} fingerprint");
        }
        // The handle row's bytes, spelled out: code, tag, class, raw.
        let h = encode_row(&handle_codes, &handle);
        assert_eq!(&h[..5], [0, 101, 1, 0, 0xff]);
        assert_eq!(
            &h[h.len() - 14..],
            [0, 105, 4, 0, 0, 0, 0, 0, 0, 0, 1, b'p', 1, 0xff]
        );
    }

    #[test]
    fn every_known_issue_kind_has_its_own_code() {
        let kinds = [
            IssueKind::TypeMismatch,
            IssueKind::BoolOutOfRange,
            IssueKind::PlistInvalid,
            IssueKind::PlistOverBudget,
            IssueKind::PlistNotDictionary,
        ];
        let codes: std::collections::BTreeSet<i64> =
            kinds.iter().map(|k| unreadable_kind(*k).code()).collect();
        assert_eq!(codes.len(), kinds.len());
        assert!(!codes.contains(&UnreadableKind::Unrecognised.code()));
    }
}
