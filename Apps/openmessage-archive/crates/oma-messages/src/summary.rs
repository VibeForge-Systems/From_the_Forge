//! `message.message_summary_info`: edits, unsent parts and translations
//! (M3 §11.3).
//!
//! The column arrives already capped at 1 MiB (§5.4); a larger value is
//! `Col::Oversized` and never read. The rest is parsed with
//! `oma_backup::bplist::parse_bounded` (D17) under its event and byte
//! budgets, a depth of 16 and an array cap of 4096
//! (`LimitKind::SummaryArray`). XML and binary property lists are both
//! accepted, as the reader detects.
//!
//! The keys read (S2, S8; `ec` also S4):
//!
//! | Key | Read as | Meaning |
//! |---|---|---|
//! | `otr` | a dictionary keyed by part index; its entry count | the original parts |
//! | `ec` | a dictionary keyed by part index, each an array of entries with an optional `d` (time) and `t` (body) | the edit history |
//! | `rp` | an array of part indexes | the unsent ("retracted") parts |
//! | `tmp` | presence only | a translation; not decoded |
//!
//! Every other root key is counted in `unknown_keys` (`ust`, `ams`, `amc`
//! and `euh` among them; their meanings are **UNVERIFIED**) and its name is
//! never kept. The layouts of `otr`'s values and of the edit bodies `t` are
//! not read: decoding `t` is deferred (§23.2).
//!
//! **A bad field spoils only itself.** A wrong type anywhere inside `ec`
//! makes `ec` [`FieldOutcome::Bad`], and `otr`, `rp` and `tmp` are still
//! read. Only a property list that does not parse, goes over a budget, or
//! whose root is not a dictionary makes the whole column unreadable.
//!
//! "Entries are oldest first, and entry 0 is the original" is
//! **UNVERIFIED**: entries are kept in stored order and claimed as nothing
//! more.
#![deny(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::unreachable,
    clippy::unimplemented,
    clippy::string_slice
)]

use std::time::SystemTime;

use oma_backup::bplist::{parse_bounded, PValue, PlistReadError, ReadOpts};
use oma_backup::LimitKind;

use crate::col::IssueKind;
use crate::time::SummaryTime;

/// The depth `message_summary_info` is read to.
pub const SUMMARY_MAX_DEPTH: u32 = 16;
/// The longest array `message_summary_info` may hold.
pub const SUMMARY_ARRAY_CAP: u32 = 4096;

/// One field of the summary.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldOutcome<T> {
    /// The key is not there.
    Missing,
    /// The key's value, read.
    Value(T),
    /// The key is there, but its value does not have the expected shape.
    Bad(ShapeError),
}

impl<T> FieldOutcome<T> {
    /// The value, when the field was read.
    pub fn value(&self) -> Option<&T> {
        match self {
            FieldOutcome::Value(v) => Some(v),
            _ => None,
        }
    }

    /// `true` for [`FieldOutcome::Bad`].
    pub fn is_bad(&self) -> bool {
        matches!(self, FieldOutcome::Bad(_))
    }
}

/// Why a field of the summary was not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ShapeError {
    /// A value of the wrong plist type.
    WrongType,
    /// A dictionary key that is not an ASCII-decimal part index fitting
    /// `u32`.
    BadPartKey,
    /// An integer that does not fit where it is used.
    OutOfRange,
}

/// The parsed summary.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct SummaryInfo {
    /// `otr`: the number of original parts.
    pub otr: FieldOutcome<u32>,
    /// `ec`: the edit history, per part, in stored order.
    pub ec: FieldOutcome<Vec<EditedPart>>,
    /// `rp`: the unsent parts, in stored order.
    pub rp: FieldOutcome<Vec<u32>>,
    /// Whether the `tmp` key (a translation) is present. It is not decoded.
    pub has_tmp: bool,
    /// Root keys other than `otr`, `ec`, `rp` and `tmp`. Their names are
    /// never kept.
    pub unknown_keys: u32,
}

impl SummaryInfo {
    /// Parse a `message_summary_info` value (one already within its cap)
    /// under the budgets above. A property list that does not parse, goes
    /// over a budget or has a non-dictionary root is an error of the
    /// matching [`IssueKind`]; anything else is a value whose fields may be
    /// [`FieldOutcome::Bad`].
    pub fn parse(bytes: &[u8]) -> Result<SummaryInfo, IssueKind> {
        parse(bytes)
    }

    /// The number of `ec` parts, when `ec` was read.
    pub fn edited_parts(&self) -> Option<u32> {
        self.ec.value().map(|v| len_u32(v.len()))
    }

    /// The number of `rp` entries, when `rp` was read.
    pub fn unsent_parts(&self) -> Option<u32> {
        self.rp.value().map(|v| len_u32(v.len()))
    }
}

/// One edited part.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct EditedPart {
    /// The part index (the dictionary key).
    pub part: u32,
    /// Its entries, in stored order; their meaning is **UNVERIFIED**.
    pub entries: Vec<EditEntry>,
}

/// One entry of an edited part.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct EditEntry {
    /// `d`, when present.
    pub at: Option<SummaryTime>,
    /// Whether `t` (the body, a data value) is present. It is not decoded.
    pub body_present: bool,
}

fn len_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Parse a `message_summary_info` value that is within its cap.
pub(crate) fn parse(bytes: &[u8]) -> Result<SummaryInfo, IssueKind> {
    let opts = ReadOpts {
        skip: &[],
        max_depth: SUMMARY_MAX_DEPTH,
        array_cap: Some((SUMMARY_ARRAY_CAP, LimitKind::SummaryArray)),
    };
    let root = parse_bounded(bytes, opts).map_err(|e| match e {
        PlistReadError::Limit { .. } => IssueKind::PlistOverBudget,
        PlistReadError::Syntax
        | PlistReadError::Missing
        | PlistReadError::NotRegular
        | PlistReadError::TooLarge { .. }
        | PlistReadError::Io(_) => IssueKind::PlistInvalid,
    })?;
    let entries = root.as_dict().ok_or(IssueKind::PlistNotDictionary)?;
    let unknown = entries
        .iter()
        .filter(|(k, _)| !matches!(k.as_str(), "otr" | "ec" | "rp" | "tmp"))
        .count();
    Ok(SummaryInfo {
        otr: field(root.get("otr"), otr),
        ec: field(root.get("ec"), ec),
        rp: field(root.get("rp"), rp),
        has_tmp: root.get("tmp").is_some(),
        unknown_keys: len_u32(unknown),
    })
}

fn field<T>(v: Option<&PValue>, read: fn(&PValue) -> Result<T, ShapeError>) -> FieldOutcome<T> {
    match v {
        None => FieldOutcome::Missing,
        Some(v) => match read(v) {
            Ok(t) => FieldOutcome::Value(t),
            Err(e) => FieldOutcome::Bad(e),
        },
    }
}

/// A part-index key: ASCII decimal, fitting `u32`.
fn part_key(k: &str) -> Result<u32, ShapeError> {
    if k.is_empty() || !k.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ShapeError::BadPartKey);
    }
    k.parse::<u32>().map_err(|_| ShapeError::BadPartKey)
}

fn otr(v: &PValue) -> Result<u32, ShapeError> {
    let parts = v.as_dict().ok_or(ShapeError::WrongType)?;
    for (k, _) in parts {
        part_key(k)?;
    }
    u32::try_from(parts.len()).map_err(|_| ShapeError::OutOfRange)
}

fn ec(v: &PValue) -> Result<Vec<EditedPart>, ShapeError> {
    let parts = v.as_dict().ok_or(ShapeError::WrongType)?;
    let mut out = Vec::new();
    for (k, entries) in parts {
        let part = part_key(k)?;
        let PValue::Array(items) = entries else {
            return Err(ShapeError::WrongType);
        };
        let mut read = Vec::new();
        for item in items {
            read.push(edit_entry(item)?);
        }
        out.push(EditedPart {
            part,
            entries: read,
        });
    }
    Ok(out)
}

fn edit_entry(v: &PValue) -> Result<EditEntry, ShapeError> {
    if v.as_dict().is_none() {
        return Err(ShapeError::WrongType);
    }
    let at = match v.get("d") {
        None => None,
        Some(PValue::Integer(i)) => Some(SummaryTime::from_integer(
            i.as_signed().ok_or(ShapeError::OutOfRange)?,
        )),
        Some(PValue::Real(r)) => Some(SummaryTime::from_real(*r)),
        Some(PValue::Date(d)) => Some(SummaryTime::from_date(SystemTime::from(*d))),
        Some(_) => return Err(ShapeError::WrongType),
    };
    let body_present = match v.get("t") {
        None => false,
        Some(PValue::Data(_)) => true,
        Some(_) => return Err(ShapeError::WrongType),
    };
    Ok(EditEntry { at, body_present })
}

fn rp(v: &PValue) -> Result<Vec<u32>, ShapeError> {
    let PValue::Array(items) = v else {
        return Err(ShapeError::WrongType);
    };
    let mut out = Vec::new();
    for item in items {
        let PValue::Integer(i) = item else {
            return Err(ShapeError::WrongType);
        };
        let n = i
            .as_unsigned()
            .and_then(|n| u32::try_from(n).ok())
            .ok_or(ShapeError::OutOfRange)?;
        out.push(n);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::{SummaryTimeRaw, TimeInterp};

    fn xml(body: &str) -> Vec<u8> {
        format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">{body}</plist>")
            .into_bytes()
    }

    #[test]
    fn a_full_summary() {
        let s = parse(&xml(
            "<dict>\
               <key>otr</key><dict><key>0</key><dict/><key>1</key><dict/></dict>\
               <key>ec</key><dict>\
                 <key>0</key><array>\
                   <dict><key>d</key><integer>700000000</integer><key>t</key><data>AAEC</data></dict>\
                   <dict><key>d</key><real>700000100.5</real></dict>\
                 </array>\
                 <key>2</key><array><dict><key>d</key><date>2024-01-01T00:00:00Z</date></dict></array>\
               </dict>\
               <key>rp</key><array><integer>1</integer><integer>3</integer></array>\
               <key>tmp</key><dict><key>x</key><string>SYNTH-TRANSLATION</string></dict>\
               <key>ust</key><true/>\
               <key>SYNTH-UNKNOWN-KEY</key><integer>1</integer>\
             </dict>",
        ))
        .expect("parse");
        assert_eq!(s.otr, FieldOutcome::Value(2));
        assert_eq!(s.rp, FieldOutcome::Value(vec![1, 3]));
        assert!(s.has_tmp);
        assert_eq!(s.unknown_keys, 2);
        assert_eq!((s.edited_parts(), s.unsent_parts()), (Some(2), Some(2)));
        let ec = s.ec.value().expect("ec");
        assert_eq!(ec.len(), 2);
        assert_eq!(ec[0].part, 0);
        assert_eq!(ec[0].entries.len(), 2);
        assert!(ec[0].entries[0].body_present);
        assert!(!ec[0].entries[1].body_present);
        let d0 = ec[0].entries[0].at.expect("d");
        assert_eq!(d0.raw, SummaryTimeRaw::Integer(700_000_000));
        assert!(matches!(d0.interp, TimeInterp::Seconds { .. }));
        let d1 = ec[0].entries[1].at.expect("d");
        assert_eq!(d1.raw, SummaryTimeRaw::Real(700_000_100.5));
        assert_eq!(ec[1].part, 2);
        let d2 = ec[1].entries[0].at.expect("d");
        assert_eq!(d2.raw, SummaryTimeRaw::Date);
        // 2024-01-01T00:00:00Z.
        assert_eq!(d2.unix_ns(), Some(1_704_067_200_000_000_000));
        // Debug carries no content.
        let shown = format!("{s:?}");
        assert!(!shown.contains("SYNTH"), "{shown}");
    }

    #[test]
    fn missing_fields_and_an_empty_dictionary() {
        let s = parse(&xml("<dict/>")).expect("parse");
        assert_eq!(s.otr, FieldOutcome::Missing);
        assert_eq!(s.ec, FieldOutcome::Missing);
        assert_eq!(s.rp, FieldOutcome::Missing);
        assert!(!s.has_tmp);
        assert_eq!(s.unknown_keys, 0);
        assert_eq!((s.edited_parts(), s.unsent_parts()), (None, None));
    }

    /// A bad element spoils only its own field.
    #[test]
    fn bad_fields_stay_local() {
        let s = parse(&xml(
            "<dict><key>otr</key><dict><key>0</key><dict/></dict>\
               <key>rp</key><array><integer>1</integer><string>x</string></array>\
               <key>ec</key><dict><key>0</key><array><dict/></array></dict></dict>",
        ))
        .expect("parse");
        assert_eq!(s.rp, FieldOutcome::Bad(ShapeError::WrongType));
        assert_eq!(s.otr, FieldOutcome::Value(1));
        assert_eq!(
            s.ec.value().map(|e| e[0].entries[0]),
            Some(EditEntry {
                at: None,
                body_present: false
            })
        );

        for (body, field_is_bad) in [
            // Wrong types at every level.
            ("<key>otr</key><array/>", "otr"),
            ("<key>otr</key><dict><key>x</key><dict/></dict>", "otr"),
            ("<key>ec</key><array/>", "ec"),
            ("<key>ec</key><dict><key>0</key><dict/></dict>", "ec"),
            ("<key>ec</key><dict><key>-1</key><array/></dict>", "ec"),
            ("<key>ec</key><dict><key>4294967296</key><array/></dict>", "ec"),
            ("<key>ec</key><dict><key>0</key><array><string>x</string></array></dict>", "ec"),
            ("<key>ec</key><dict><key>0</key><array><dict><key>d</key><string>x</string></dict></array></dict>", "ec"),
            ("<key>ec</key><dict><key>0</key><array><dict><key>t</key><string>x</string></dict></array></dict>", "ec"),
            ("<key>ec</key><dict><key>0</key><array><dict><key>d</key><integer>18446744073709551615</integer></dict></array></dict>", "ec"),
            ("<key>rp</key><dict/>", "rp"),
            ("<key>rp</key><array><integer>-1</integer></array>", "rp"),
            ("<key>rp</key><array><integer>4294967296</integer></array>", "rp"),
        ] {
            let s = parse(&xml(&format!("<dict>{body}</dict>"))).expect(body);
            let bad = [("otr", s.otr.is_bad()), ("ec", s.ec.is_bad()), ("rp", s.rp.is_bad())];
            for (name, is_bad) in bad {
                assert_eq!(is_bad, name == field_is_bad, "{body}: {name}");
            }
        }
    }

    #[test]
    fn unreadable_summaries() {
        assert_eq!(parse(b""), Err(IssueKind::PlistInvalid));
        assert_eq!(parse(b"bplist00garbage"), Err(IssueKind::PlistInvalid));
        assert_eq!(parse(&xml("<array/>")), Err(IssueKind::PlistNotDictionary));
        assert_eq!(
            parse(&xml("<string>x</string>")),
            Err(IssueKind::PlistNotDictionary)
        );
        let wide: String = "<integer>0</integer>".repeat(4097);
        assert_eq!(
            parse(&xml(&format!(
                "<dict><key>rp</key><array>{wide}</array></dict>"
            ))),
            Err(IssueKind::PlistOverBudget)
        );
        let ok: String = "<integer>0</integer>".repeat(4096);
        let s = parse(&xml(&format!(
            "<dict><key>rp</key><array>{ok}</array></dict>"
        )))
        .expect("4096 is within the cap");
        assert_eq!(s.unsent_parts(), Some(4096));
    }
}
