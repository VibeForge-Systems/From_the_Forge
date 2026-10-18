//! Layer 2, part 1: a closed set of Foundation classes (M3 §10.5).
//!
//! | Class (or a subclass) | Groups |
//! |---|---|
//! | `NSString`, `NSMutableString` | one `+` group: UTF-8 bytes |
//! | `NSNumber` (→ `NSValue`) | a `*` group (the objCType text), then one group of that type |
//! | `NSDictionary` | an `i` count, then count × (an `@` key group, an `@` value group) |
//! | `NSData` | an `i` length, then a `[Nc]` group of that length |
//! | `NSAttributedString`, `NSMutableAttributedString` | see `attributed.rs` |
//!
//! Anything else is `Other`. A class is recognised by the first known name
//! on its superclass chain, most derived first; class names are never
//! copied out. An object whose groups do not have the shape its class
//! needs reads as `None` here, never as a guessed value.
#![deny(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::unreachable,
    clippy::unimplemented,
    clippy::string_slice
)]

use super::node::{Doc, Group, SlotId, Val};

/// The closed set of classes layer 2 interprets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    String,
    Number,
    Dictionary,
    Data,
    Attributed,
    Other,
}

/// An `NSNumber`'s value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Number {
    Signed(i64),
    Unsigned(u64),
    Real(f64),
}

impl Number {
    /// The value as an `i64`, when it is an integer that fits.
    pub(crate) fn as_i64(self) -> Option<i64> {
        match self {
            Number::Signed(v) => Some(v),
            Number::Unsigned(v) => i64::try_from(v).ok(),
            Number::Real(_) => None,
        }
    }

    pub(crate) fn is_nonzero(self) -> bool {
        match self {
            Number::Signed(v) => v != 0,
            Number::Unsigned(v) => v != 0,
            Number::Real(v) => v != 0.0,
        }
    }
}

const BASES: [(&[u8], Kind); 5] = [
    (b"NSString", Kind::String),
    (b"NSNumber", Kind::Number),
    (b"NSDictionary", Kind::Dictionary),
    (b"NSData", Kind::Data),
    (b"NSAttributedString", Kind::Attributed),
];

/// The kind a class name gives by itself, when it is one of the bases.
pub(crate) fn base_kind(name: &[u8]) -> Option<Kind> {
    BASES.iter().find(|(n, _)| *n == name).map(|(_, k)| *k)
}

/// The class kind of an object slot: the first known name on its class's
/// superclass chain, most derived first. The walk resolved it once per
/// class when the chain was read, so this is O(1) however long the chain.
pub(crate) fn kind(doc: &Doc<'_>, obj: SlotId) -> Kind {
    doc.object_class(obj)
        .map_or(Kind::Other, |c| doc.class_kind(c))
}

/// An `NSString`'s bytes (not yet checked as UTF-8).
pub(crate) fn string_bytes<'a>(doc: &Doc<'a>, obj: SlotId) -> Option<&'a [u8]> {
    if kind(doc, obj) != Kind::String {
        return None;
    }
    match doc.object_groups(obj)? {
        [g] => match doc.single(*g, b"+")? {
            Val::Bytes(s) => doc.span(s),
            _ => None,
        },
        _ => None,
    }
}

/// An `NSNumber`'s value. The objCType text must equal the value group's
/// type, and the value must be a single scalar.
pub(crate) fn number(doc: &Doc<'_>, obj: SlotId) -> Option<Number> {
    if kind(doc, obj) != Kind::Number {
        return None;
    }
    let [ty_group, value_group] = doc.object_groups(obj)? else {
        return None;
    };
    let Val::CStr(Some(ptr)) = doc.single(*ty_group, b"*")? else {
        return None;
    };
    let objc_type = doc.cstr_bytes(ptr)?;
    match doc.single(*value_group, objc_type)? {
        Val::Signed(v) => Some(Number::Signed(v)),
        Val::Unsigned(v) => Some(Number::Unsigned(v)),
        Val::Real(v) => Some(Number::Real(v)),
        _ => None,
    }
}

/// An `NSDictionary`'s entry groups: `2 × count` groups, alternating key
/// and value. `None` when the count does not match the groups that follow
/// it. Read each with [`object_of`].
pub(crate) fn dict_entries<'d>(doc: &'d Doc<'_>, obj: SlotId) -> Option<&'d [Group]> {
    if kind(doc, obj) != Kind::Dictionary {
        return None;
    }
    let (count_group, rest) = doc.object_groups(obj)?.split_first()?;
    let Val::Signed(count) = doc.single(*count_group, b"i")? else {
        return None;
    };
    let pairs = usize::try_from(count).ok()?;
    (pairs.checked_mul(2)? == rest.len()).then_some(rest)
}

/// The object of a group that holds exactly one `@` value: `Some(None)`
/// for nil, `None` when the group is not a single object.
pub(crate) fn object_of(doc: &Doc<'_>, g: Group) -> Option<Option<SlotId>> {
    match doc.single(g, b"@")? {
        Val::Object(o) => Some(o),
        _ => None,
    }
}

/// An `NSData`'s bytes. M3 reads no `NSData` attribute value; the unit
/// tests use this to check the generic walk.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn data_bytes<'a>(doc: &Doc<'a>, obj: SlotId) -> Option<&'a [u8]> {
    if kind(doc, obj) != Kind::Data {
        return None;
    }
    let [len_group, bytes_group] = doc.object_groups(obj)? else {
        return None;
    };
    let Val::Signed(len) = doc.single(*len_group, b"i")? else {
        return None;
    };
    let vals = doc.values(*bytes_group)?;
    let [Val::Bytes(span)] = vals else {
        return None;
    };
    if i64::from(span.len()) != len {
        return None;
    }
    doc.span(*span)
}
