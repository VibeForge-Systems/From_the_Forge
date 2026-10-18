//! Layer 2, part 2: the attributed string, its runs and parts
//! (M3 §10.7–§10.8).
//!
//! 1. The root's class chain must contain `NSAttributedString`; otherwise
//!    `NotAttributedString`.
//! 2. Its first group is an `@` whose chain contains `NSString`. That
//!    string's `+` bytes, decoded as strict UTF-8, are the text; otherwise
//!    `InvalidUtf8`.
//! 3. Then come the runs. Each is an `iI` group: (dictionary number, length
//!    in UTF-16 units, T4). A dictionary follows only when the number is the
//!    count of dictionaries seen so far plus one.
//! 4. Run problems are validated but never discard the text. The runs that
//!    are valid up to the first fault are kept; the rest are missing, and
//!    [`AttributedBody::run_issue`] says so. Nothing is clamped:
//!    - a number outside `1..=seen+1`, a number 0, a nil dictionary or a
//!      group that is not a run where one is expected: `BadRunIndex`;
//!    - a run past the end of the text: `RunOverflow`;
//!    - a boundary inside a surrogate pair: `RunSplitsSurrogate`;
//!    - runs that end short of the text: `RunLengthMismatch`.
//! 5. UTF-16 offsets are mapped to UTF-8 by one forward walk over the text,
//!    since runs are contiguous. Consecutive runs that use the same
//!    dictionary object are merged; consecutive runs with the same
//!    `__kIMMessagePartAttributeName` value form a part.
//!
//! Each dictionary and each attribute string is interpreted at most once,
//! memoised by object-table index; runs and parts refer to them by index.
//! Every byte copied out is charged to the materialisation budget first.
#![deny(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::unreachable,
    clippy::unimplemented,
    clippy::string_slice
)]

use std::ops::Range;

use zeroize::Zeroizing;

use super::foundation::{self, Kind};
use super::keys::{AttrFlags, AttrKey, AttrKeysPresent};
use super::node::{Doc, Group, SlotId, Val};
use super::stats::Meter;
use super::{TsError, TsErrorKind};
use crate::redact::Redacted;

/// An index into [`AttributedBody::values`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ValueId(u32);

impl ValueId {
    /// The index.
    pub fn index(self) -> usize {
        usize::try_from(self.0).unwrap_or(usize::MAX)
    }
}

/// An attribute value, interpreted once and shared by every run that uses
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SharedValue {
    /// An `NSString` attribute value: a transfer GUID or a mentioned handle.
    Text(Redacted<Zeroizing<String>>),
}

/// The attributes of one dictionary, as M3 reads them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RunAttrs {
    /// `__kIMMessagePartAttributeName`, when it is an integer `NSNumber`.
    pub part_index: Option<i64>,
    /// `__kIMFileTransferGUIDAttributeName`, when it is an `NSString`.
    pub attachment_guid: Option<ValueId>,
    /// `__kIMMentionConfirmedMention`, when it is an `NSString`.
    pub mention: Option<ValueId>,
    /// Flags set from non-zero `NSNumber` values.
    pub flags: AttrFlags,
    /// Every known key present, whatever its value.
    pub keys_present: AttrKeysPresent,
    /// `__kIMTextEffectAttributeName`, when it is an integer `NSNumber`.
    pub text_effect: Option<i64>,
}

/// One run after merging: a span of the text and its attributes.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct BodyRun {
    /// The span in UTF-16 code units.
    pub utf16: Range<u32>,
    /// The same span in UTF-8 bytes of [`AttributedBody::text`].
    pub utf8: Range<u32>,
    /// An index into [`AttributedBody::attrs`].
    pub attrs: u32,
}

/// Consecutive runs with the same part index.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct BodyPart {
    /// The part index, when the runs carry one.
    pub part_index: Option<i64>,
    /// The span in UTF-16 code units.
    pub utf16: Range<u32>,
    /// The span in UTF-8 bytes.
    pub utf8: Range<u32>,
    /// The first transfer GUID among the part's runs.
    pub attachment_guid: Option<ValueId>,
    /// The first mention among the part's runs.
    pub mention: Option<ValueId>,
    /// The union of the runs' flags.
    pub flags: AttrFlags,
    /// The union of the runs' present keys.
    pub keys_present: AttrKeysPresent,
    /// The first text effect among the part's runs.
    pub text_effect: Option<i64>,
    /// The part's runs, as indexes into [`AttributedBody::runs`].
    pub runs: Range<u32>,
}

/// The first run fault. Runs from `run` on are missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct RunIssue {
    /// One of the non-fatal kinds.
    pub kind: TsErrorKind,
    /// The index, among the runs as written, of the run where it was found
    /// (the run count for `RunLengthMismatch`).
    pub run: u32,
}

/// A decoded `attributedBody`. Its `Debug` output carries lengths, ranges
/// and counts only.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct AttributedBody {
    /// The text.
    pub text: Redacted<Zeroizing<String>>,
    /// Attribute values, each interpreted once.
    pub values: Vec<SharedValue>,
    /// One entry per dictionary object interpreted (a dictionary is read
    /// when a run introduces it, even if that run then faults).
    pub attrs: Vec<RunAttrs>,
    /// The kept runs, merged.
    pub runs: Vec<BodyRun>,
    /// The kept runs, grouped by part index.
    pub parts: Vec<BodyPart>,
    /// The first run fault, if any.
    pub run_issue: Option<RunIssue>,
    /// Dictionary entries whose key is not a known key (names not kept).
    pub unknown_attribute_keys: u32,
    /// Dictionary values whose class is outside the closed set (names not
    /// kept).
    pub unknown_attribute_classes: u32,
    /// Values of known keys that cannot be read as the key needs (wrong
    /// class, nil, an integer that does not fit, or text that is not
    /// UTF-8).
    pub unreadable_attribute_values: u32,
}

const NONE: u32 = u32::MAX;

fn bytes_of<T>() -> u64 {
    u64::try_from(std::mem::size_of::<T>()).unwrap_or(u64::MAX)
}

fn usize_len(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

struct Interp<'d, 'a, 'm> {
    doc: &'d Doc<'a>,
    meter: &'m mut Meter,
    at: usize,
    value_memo: Vec<u32>,
    dict_memo: Vec<u32>,
    values: Vec<SharedValue>,
    attrs: Vec<RunAttrs>,
    unknown_keys: u32,
    unknown_classes: u32,
    unreadable: u32,
}

impl Interp<'_, '_, '_> {
    fn err(&self, kind: TsErrorKind) -> TsError {
        TsError::new(kind, self.at)
    }

    fn memo_get(memo: &mut Vec<u32>, slots: usize, id: SlotId) -> Option<u32> {
        if memo.is_empty() {
            memo.resize(slots, NONE);
        }
        memo.get(usize::try_from(id).ok()?).copied()
    }

    fn memo_set(memo: &mut [u32], id: SlotId, v: u32) {
        if let Some(m) = usize::try_from(id).ok().and_then(|i| memo.get_mut(i)) {
            *m = v;
        }
    }

    /// An `NSString` attribute value, interpreted once per object.
    fn string_value(&mut self, obj: SlotId) -> Result<Option<ValueId>, TsError> {
        let slots = self.doc.slot_count();
        match Self::memo_get(&mut self.value_memo, slots, obj) {
            Some(NONE) => {}
            Some(id) => return Ok(Some(ValueId(id))),
            None => return Ok(None),
        }
        let Some(text) =
            foundation::string_bytes(self.doc, obj).and_then(|b| std::str::from_utf8(b).ok())
        else {
            return Ok(None);
        };
        self.meter.materialise(
            usize_len(text.len()).saturating_add(bytes_of::<SharedValue>()),
            self.at,
        )?;
        let id = u32::try_from(self.values.len()).map_err(|_| self.err(TsErrorKind::Budget))?;
        self.values
            .push(SharedValue::Text(Redacted::new(Zeroizing::new(
                text.to_owned(),
            ))));
        Self::memo_set(&mut self.value_memo, obj, id);
        Ok(Some(ValueId(id)))
    }

    /// The attributes of a dictionary object, interpreted once per object.
    fn dict_attrs(&mut self, dict: SlotId) -> Result<Option<u32>, TsError> {
        let slots = self.doc.slot_count();
        match Self::memo_get(&mut self.dict_memo, slots, dict) {
            Some(NONE) => {}
            Some(id) => return Ok(Some(id)),
            None => return Ok(None),
        }
        let doc = self.doc;
        let Some(entries) = foundation::dict_entries(doc, dict) else {
            return Ok(None);
        };
        // Every entry must be a single object (or nil) before any is used.
        if entries
            .iter()
            .any(|g| foundation::object_of(doc, *g).is_none())
        {
            return Ok(None);
        }
        let mut a = RunAttrs::default();
        let mut seen = AttrKeysPresent::default();
        let mut i: usize = 0;
        while let (Some(kg), Some(vg)) = (entries.get(i), entries.get(i.saturating_add(1))) {
            i = i.saturating_add(2);
            let key = foundation::object_of(doc, *kg).flatten();
            let value = foundation::object_of(doc, *vg).flatten();
            self.entry(&mut a, &mut seen, key, value)?;
        }
        self.meter.materialise(bytes_of::<RunAttrs>(), self.at)?;
        let id = u32::try_from(self.attrs.len()).map_err(|_| self.err(TsErrorKind::Budget))?;
        self.attrs.push(a);
        Self::memo_set(&mut self.dict_memo, dict, id);
        Ok(Some(id))
    }

    fn entry(
        &mut self,
        a: &mut RunAttrs,
        seen: &mut AttrKeysPresent,
        key: Option<SlotId>,
        value: Option<SlotId>,
    ) -> Result<(), TsError> {
        let doc = self.doc;
        if let Some(v) = value {
            if foundation::kind(doc, v) == Kind::Other {
                self.unknown_classes = self.unknown_classes.saturating_add(1);
            }
        }
        let Some(k) = key
            .and_then(|k| foundation::string_bytes(doc, k))
            .and_then(AttrKey::from_name)
        else {
            self.unknown_keys = self.unknown_keys.saturating_add(1);
            return Ok(());
        };
        let first = !seen.contains(k);
        seen.insert(k);
        a.keys_present.insert(k);
        if !first {
            return Ok(());
        }
        let number = value.and_then(|v| foundation::number(doc, v));
        match k {
            AttrKey::MessagePart | AttrKey::TextEffect => {
                match number.and_then(foundation::Number::as_i64) {
                    Some(n) if k == AttrKey::MessagePart => a.part_index = Some(n),
                    Some(n) => a.text_effect = Some(n),
                    None => self.unreadable = self.unreadable.saturating_add(1),
                }
            }
            AttrKey::FileTransferGuid | AttrKey::Mention => {
                let id = match value {
                    Some(v) => self.string_value(v)?,
                    None => None,
                };
                match id {
                    Some(id) if k == AttrKey::Mention => a.mention = Some(id),
                    Some(id) => a.attachment_guid = Some(id),
                    None => self.unreadable = self.unreadable.saturating_add(1),
                }
            }
            _ => {
                if let (Some(flag), Some(n)) = (k.flag(), number) {
                    if n.is_nonzero() {
                        a.flags.insert(flag);
                    }
                }
            }
        }
        Ok(())
    }
}

/// The run header of a group: `iI` (dictionary number, UTF-16 length).
fn run_header(doc: &Doc<'_>, g: Group) -> Option<(i64, u64)> {
    if doc.type_bytes(g)? != b"iI" {
        return None;
    }
    match doc.values(g)? {
        [Val::Signed(i), Val::Unsigned(n)] => Some((*i, *n)),
        _ => None,
    }
}

/// A forward walk from UTF-16 offsets to UTF-8 offsets.
struct Cursor<'t> {
    chars: std::str::CharIndices<'t>,
    pos16: u64,
    pos8: usize,
}

impl Cursor<'_> {
    /// Advance to `end16`. `Err(())` when it falls inside a surrogate pair.
    fn advance(&mut self, end16: u64) -> Result<(), ()> {
        while self.pos16 < end16 {
            let (i, c) = self.chars.next().ok_or(())?;
            self.pos16 = self.pos16.saturating_add(usize_len(c.len_utf16()));
            self.pos8 = i.saturating_add(c.len_utf8());
        }
        if self.pos16 == end16 {
            Ok(())
        } else {
            Err(())
        }
    }
}

/// Interpret the root object `root` of a walked stream.
pub(crate) fn interpret(
    doc: &Doc<'_>,
    root_group: u32,
    meter: &mut Meter,
    at: usize,
) -> Result<AttributedBody, TsError> {
    let not = TsError::new(TsErrorKind::NotAttributedString, at);
    let root = match doc.group(root_group).and_then(|g| doc.single(g, b"@")) {
        Some(Val::Object(Some(o))) => o,
        _ => return Err(not),
    };
    if foundation::kind(doc, root) != Kind::Attributed {
        return Err(not);
    }
    let groups = doc.object_groups(root).ok_or(not)?;
    let (first, rest) = groups.split_first().ok_or(not)?;
    let Some(Some(string)) = foundation::object_of(doc, *first) else {
        return Err(not);
    };
    let bytes = foundation::string_bytes(doc, string).ok_or(not)?;
    let text =
        std::str::from_utf8(bytes).map_err(|_| TsError::new(TsErrorKind::InvalidUtf8, at))?;
    meter.materialise(usize_len(text.len()), at)?;
    let total16: u64 = text.chars().map(|c| usize_len(c.len_utf16())).sum();

    let mut it = Interp {
        doc,
        meter,
        at,
        value_memo: Vec::new(),
        dict_memo: Vec::new(),
        values: Vec::new(),
        attrs: Vec::new(),
        unknown_keys: 0,
        unknown_classes: 0,
        unreadable: 0,
    };
    let mut seen: Vec<u32> = Vec::new();
    let mut runs: Vec<BodyRun> = Vec::new();
    let mut cur = Cursor {
        chars: text.char_indices(),
        pos16: 0,
        pos8: 0,
    };
    let mut issue: Option<RunIssue> = None;
    let mut run_no: u32 = 0;
    let mut gi: usize = 0;
    let to32 = |n: u64| u32::try_from(n).unwrap_or(u32::MAX);
    let to32u = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
    while let Some(g) = rest.get(gi) {
        gi = gi.saturating_add(1);
        let fault = |kind| Some(RunIssue { kind, run: run_no });
        let Some((idx, len)) = run_header(doc, *g) else {
            issue = fault(TsErrorKind::BadRunIndex);
            break;
        };
        it.meter.run(at)?;
        let seen_len = i64::try_from(seen.len()).unwrap_or(i64::MAX);
        let attrs_id = if Some(idx) == seen_len.checked_add(1) {
            let dict = rest
                .get(gi)
                .and_then(|d| foundation::object_of(doc, *d))
                .flatten();
            let id = match dict {
                Some(d) => it.dict_attrs(d)?,
                None => None,
            };
            let Some(id) = id else {
                issue = fault(TsErrorKind::BadRunIndex);
                break;
            };
            gi = gi.saturating_add(1);
            seen.push(id);
            id
        } else {
            let pick = usize::try_from(idx)
                .ok()
                .and_then(|i| i.checked_sub(1))
                .and_then(|i| seen.get(i).copied());
            let Some(id) = pick else {
                issue = fault(TsErrorKind::BadRunIndex);
                break;
            };
            id
        };
        let start16 = cur.pos16;
        let start8 = cur.pos8;
        let Some(end16) = start16.checked_add(len).filter(|e| *e <= total16) else {
            issue = fault(TsErrorKind::RunOverflow);
            break;
        };
        if cur.advance(end16).is_err() {
            issue = fault(TsErrorKind::RunSplitsSurrogate);
            break;
        }
        run_no = run_no.saturating_add(1);
        let (s16, e16, s8, e8) = (to32(start16), to32(end16), to32u(start8), to32u(cur.pos8));
        match runs.last_mut() {
            Some(last) if last.attrs == attrs_id && last.utf16.end == s16 => {
                last.utf16.end = e16;
                last.utf8.end = e8;
            }
            _ => {
                it.meter.materialise(bytes_of::<BodyRun>(), at)?;
                runs.push(BodyRun {
                    utf16: s16..e16,
                    utf8: s8..e8,
                    attrs: attrs_id,
                });
            }
        }
    }
    if issue.is_none() && cur.pos16 != total16 {
        issue = Some(RunIssue {
            kind: TsErrorKind::RunLengthMismatch,
            run: run_no,
        });
    }

    let mut parts: Vec<BodyPart> = Vec::new();
    for (i, r) in runs.iter().enumerate() {
        let a = it
            .attrs
            .get(usize::try_from(r.attrs).unwrap_or(usize::MAX))
            .copied()
            .unwrap_or_default();
        let i = to32u(i);
        match parts.last_mut() {
            Some(p) if p.part_index == a.part_index => {
                p.utf16.end = r.utf16.end;
                p.utf8.end = r.utf8.end;
                p.runs.end = i.saturating_add(1);
                p.attachment_guid = p.attachment_guid.or(a.attachment_guid);
                p.mention = p.mention.or(a.mention);
                p.text_effect = p.text_effect.or(a.text_effect);
                p.flags = p.flags.union(a.flags);
                p.keys_present = p.keys_present.union(a.keys_present);
            }
            _ => {
                it.meter.materialise(bytes_of::<BodyPart>(), at)?;
                parts.push(BodyPart {
                    part_index: a.part_index,
                    utf16: r.utf16.clone(),
                    utf8: r.utf8.clone(),
                    attachment_guid: a.attachment_guid,
                    mention: a.mention,
                    flags: a.flags,
                    keys_present: a.keys_present,
                    text_effect: a.text_effect,
                    runs: i..i.saturating_add(1),
                });
            }
        }
    }

    Ok(AttributedBody {
        text: Redacted::new(Zeroizing::new(text.to_owned())),
        values: it.values,
        attrs: it.attrs,
        runs,
        parts,
        run_issue: issue,
        unknown_attribute_keys: it.unknown_keys,
        unknown_attribute_classes: it.unknown_classes,
        unreadable_attribute_values: it.unreadable,
    })
}
