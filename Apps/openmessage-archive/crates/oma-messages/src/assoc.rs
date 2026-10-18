//! Associated messages: tapbacks, stickers and poll votes (M3 §11.1).
//!
//! A row is associated with another through `associated_message_type` and
//! `associated_message_guid`. The GUID names its target in one of these
//! forms (S2, corroborated by S8):
//!
//! | Form | Target |
//! |---|---|
//! | `p:N/<guid>` | part `N` of the message `<guid>` |
//! | `bp:<guid>` | the whole bubble of the message `<guid>` |
//! | `<guid>` | a bare GUID: counted; its shape is **UNVERIFIED** |
//!
//! `N` must be ASCII decimal and fit in `u32`, and the GUID must be
//! non-empty and hold no NUL byte. Anything else that starts with `p:` or
//! `bp:` is [`AssocTarget::Unparsed`]; nothing is guessed. A NULL or empty
//! value is `Col::Null`, and a value over its 256-byte cap is
//! `Col::Oversized` (it is never read).
//!
//! "Only the latest tapback state is stored" (S2) is **UNVERIFIED**:
//! separate 3000–3007 removal rows exist. M3 replays no history.
#![deny(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::unreachable,
    clippy::unimplemented,
    clippy::string_slice
)]

use crate::codes::AssociatedKind;
use crate::col::Col;
use crate::redact::{Redacted, SqlText};

/// The message an associated row points at.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AssocTarget {
    /// `p:N/<guid>`: part `N` of a message.
    Part {
        /// The part index.
        part: u32,
        /// The target message's GUID.
        guid: Redacted<SqlText>,
    },
    /// `bp:<guid>`: a whole bubble.
    Bubble {
        /// The target message's GUID.
        guid: Redacted<SqlText>,
    },
    /// No prefix: a bare GUID. Its shape is **UNVERIFIED**.
    Bare {
        /// The value, taken as a GUID.
        guid: Redacted<SqlText>,
    },
    /// A `p:` or `bp:` value that does not parse, or a bare value holding a
    /// NUL byte.
    Unparsed,
}

impl AssocTarget {
    /// Parse a non-empty `associated_message_guid`.
    pub fn parse(raw: &[u8]) -> Self {
        if let Some(rest) = raw.strip_prefix(b"bp:") {
            return guid(rest).map_or(AssocTarget::Unparsed, |guid| AssocTarget::Bubble { guid });
        }
        if let Some(rest) = raw.strip_prefix(b"p:") {
            let Some(slash) = rest.iter().position(|b| *b == b'/') else {
                return AssocTarget::Unparsed;
            };
            let (digits, tail) = (rest.get(..slash), rest.get(slash.saturating_add(1)..));
            return match (digits.and_then(part_index), tail.and_then(guid)) {
                (Some(part), Some(guid)) => AssocTarget::Part { part, guid },
                _ => AssocTarget::Unparsed,
            };
        }
        guid(raw).map_or(AssocTarget::Unparsed, |guid| AssocTarget::Bare { guid })
    }

    /// The target GUID, when the value parsed.
    pub fn guid(&self) -> Option<&Redacted<SqlText>> {
        match self {
            AssocTarget::Part { guid, .. }
            | AssocTarget::Bubble { guid }
            | AssocTarget::Bare { guid } => Some(guid),
            AssocTarget::Unparsed => None,
        }
    }
}

/// A target GUID: non-empty, no NUL.
fn guid(b: &[u8]) -> Option<Redacted<SqlText>> {
    (!b.is_empty() && !b.contains(&0)).then(|| Redacted::new(SqlText::from_bytes(b)))
}

/// ASCII decimal that fits in `u32`. Leading zeros are accepted.
fn part_index(b: &[u8]) -> Option<u32> {
    if b.is_empty() {
        return None;
    }
    b.iter().try_fold(0u32, |n, d| {
        if d.is_ascii_digit() {
            n.checked_mul(10)?
                .checked_add(u32::from(d.checked_sub(b'0')?))
        } else {
            None
        }
    })
}

/// The five association columns of one `message` row, exactly as read
/// (M4 R4). Always filled, whether or not the row has an [`Association`]:
/// `associated_message_guid = ''` stays `Value("")` here, where
/// [`Association`] maps it to NULL, and a row with no association still
/// shows which columns were absent, NULL or unreadable.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AssocRaw {
    /// `associated_message_type`.
    pub raw_type: Col<i64>,
    /// `associated_message_guid`, unparsed.
    pub raw_guid: Col<Redacted<SqlText>>,
    /// `associated_message_emoji`.
    pub emoji: Col<Redacted<SqlText>>,
    /// `associated_message_range_location`.
    pub range_location: Col<i64>,
    /// `associated_message_range_length`.
    pub range_length: Col<i64>,
}

impl std::fmt::Debug for AssocRaw {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AssocRaw")
            .field("raw_type", &self.raw_type.shape())
            .field("raw_guid", &self.raw_guid.shape())
            .field("emoji", &self.emoji.shape())
            .field("range_location", &self.range_location.shape())
            .field("range_length", &self.range_length.shape())
            .finish_non_exhaustive()
    }
}

impl AssocRaw {
    /// The five columns as a caller holds them, for example as an archive
    /// stores them (M4 re-derives the association from stored raw values).
    pub fn new(
        raw_type: Col<i64>,
        raw_guid: Col<Redacted<SqlText>>,
        emoji: Col<Redacted<SqlText>>,
        range_location: Col<i64>,
        range_length: Col<i64>,
    ) -> AssocRaw {
        AssocRaw {
            raw_type,
            raw_guid,
            emoji,
            range_location,
            range_length,
        }
    }

    /// The association these columns describe, or `None` (§11.1).
    pub fn association(&self) -> Option<Association> {
        Association::from_columns(
            self.raw_type.clone(),
            self.raw_guid.clone(),
            self.emoji.clone(),
            self.range_location.clone(),
            self.range_length.clone(),
        )
    }
}

/// The association of one `message` row (§11.1).
///
/// A record has one when `associated_message_type` holds a non-zero value,
/// or when `associated_message_guid` holds anything but NULL or the empty
/// string. Both columns are kept as read, so a row with a GUID and type 0
/// is visible as such.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Association {
    /// `associated_message_type`, raw.
    pub raw_type: Col<i64>,
    /// The mapped kind, when `raw_type` is a value.
    pub kind: Option<AssociatedKind>,
    /// `associated_message_guid`, parsed. NULL or empty is `Col::Null`.
    pub target: Col<AssocTarget>,
    /// `associated_message_emoji` (an emoji tapback's emoji).
    pub emoji: Col<Redacted<SqlText>>,
    /// `associated_message_range_location`.
    pub range_location: Col<i64>,
    /// `associated_message_range_length`.
    pub range_length: Col<i64>,
}

impl Association {
    /// Build the association of a row, or `None` when it has none.
    pub(crate) fn from_columns(
        raw_type: Col<i64>,
        guid: Col<Redacted<SqlText>>,
        emoji: Col<Redacted<SqlText>>,
        range_location: Col<i64>,
        range_length: Col<i64>,
    ) -> Option<Self> {
        let target = match guid {
            Col::Value(g) if g.expose().is_empty() => Col::Null,
            Col::Value(g) => Col::Value(AssocTarget::parse(g.expose().as_bytes())),
            other => other.map(|_| AssocTarget::Unparsed),
        };
        let typed = matches!(raw_type, Col::Value(t) if t != 0);
        let targeted = !matches!(target, Col::Absent | Col::Null);
        if !typed && !targeted {
            return None;
        }
        Some(Association {
            kind: raw_type.value().copied().map(AssociatedKind::from_raw),
            raw_type,
            target,
            emoji,
            range_location,
            range_length,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codes::Reaction;
    use crate::col::IssueKind;

    fn bytes(t: &AssocTarget) -> Option<&[u8]> {
        t.guid().map(|g| g.expose().as_bytes())
    }

    /// §19.1 `assoc`.
    #[test]
    fn target_forms() {
        let p = AssocTarget::parse(b"p:0/G");
        assert!(matches!(p, AssocTarget::Part { part: 0, .. }), "{p:?}");
        assert_eq!(bytes(&p), Some(&b"G"[..]));
        let p = AssocTarget::parse(b"p:12/SYNTH-GUID-1");
        assert!(matches!(p, AssocTarget::Part { part: 12, .. }));
        assert_eq!(bytes(&p), Some(&b"SYNTH-GUID-1"[..]));
        assert!(matches!(
            AssocTarget::parse(b"p:4294967295/G"),
            AssocTarget::Part { part: u32::MAX, .. }
        ));
        assert!(matches!(
            AssocTarget::parse(b"p:007/G"),
            AssocTarget::Part { part: 7, .. }
        ));
        let b = AssocTarget::parse(b"bp:G");
        assert!(matches!(b, AssocTarget::Bubble { .. }));
        assert_eq!(bytes(&b), Some(&b"G"[..]));
        let bare = AssocTarget::parse(b"G");
        assert!(matches!(bare, AssocTarget::Bare { .. }));
        assert_eq!(bytes(&bare), Some(&b"G"[..]));
        // A GUID holding a further slash keeps it: only the first one splits.
        assert_eq!(bytes(&AssocTarget::parse(b"p:1/a/b")), Some(&b"a/b"[..]));

        for bad in [
            &b"p:/G"[..],
            b"p:x/G",
            b"p:-1/G",
            b"p:+1/G",
            b"p: 1/G",
            b"p:4294967296/G",
            b"p:99999999999999999999/G",
            b"p:1/",
            b"p:1",
            b"p:",
            b"bp:",
            b"p:1/G\0H",
            b"bp:G\0",
            b"\0",
            b"G\0",
        ] {
            assert_eq!(
                AssocTarget::parse(bad),
                AssocTarget::Unparsed,
                "{:?}",
                String::from_utf8_lossy(bad)
            );
        }
        // Not UTF-8 is still a GUID's bytes; nothing is repaired.
        assert_eq!(bytes(&AssocTarget::parse(b"bp:\xff")), Some(&b"\xff"[..]));
        // Debug never shows the GUID.
        let shown = format!("{:?}", AssocTarget::parse(b"p:3/SYNTH-GUID-SECRET"));
        assert!(!shown.contains("SECRET"), "{shown}");
    }

    fn text(s: &str) -> Col<Redacted<SqlText>> {
        Col::Value(Redacted::new(SqlText::from_bytes(s.as_bytes())))
    }

    /// R4: the raw columns keep what `Association` folds together.
    #[test]
    fn raw_columns_keep_the_states_association_folds() {
        let raw = |t: Col<i64>, g: Col<Redacted<SqlText>>| {
            AssocRaw::new(t, g, Col::Absent, Col::Null, Col::Value(3))
        };
        // '' and NULL both give no association, but stay apart raw.
        let empty = raw(Col::Value(0), text(""));
        let null = raw(Col::Value(0), Col::Null);
        assert!(empty.association().is_none() && null.association().is_none());
        assert_ne!(empty, null);
        assert_eq!(
            empty.raw_guid.value().map(|g| g.expose().as_bytes()),
            Some(&b""[..])
        );
        // An absent type column and a type 0 also stay apart.
        assert_ne!(raw(Col::Absent, Col::Null), null);
        // With a target, the association is the one from_columns builds.
        let tapback = raw(Col::Value(2001), text("p:0/SYNTH-MSG-GUID-1"));
        let a = tapback.association().expect("tapback");
        assert!(matches!(
            a.target,
            Col::Value(AssocTarget::Part { part: 0, .. })
        ));
        assert_eq!(a.range_length, Col::Value(3));
        let shown = format!("{tapback:?}");
        assert!(
            !shown.contains("SYNTH") && !shown.contains("2001"),
            "{shown}"
        );
        assert!(
            shown.starts_with("AssocRaw { raw_type: Value, raw_guid: Value"),
            "{shown}"
        );
    }

    #[test]
    fn when_a_row_has_an_association() {
        let none = |t: Col<i64>, g: Col<Redacted<SqlText>>| {
            Association::from_columns(t, g, Col::Absent, Col::Absent, Col::Absent)
        };
        assert!(none(Col::Value(0), Col::Null).is_none());
        assert!(none(Col::Value(0), text("")).is_none());
        assert!(none(Col::Null, Col::Null).is_none());
        assert!(none(Col::Absent, Col::Absent).is_none());

        let a = none(Col::Value(2001), text("p:0/G")).expect("tapback");
        assert_eq!(
            a.kind,
            Some(AssociatedKind::Tapback {
                reaction: Reaction::Like,
                removal: false
            })
        );
        assert!(matches!(
            a.target,
            Col::Value(AssocTarget::Part { part: 0, .. })
        ));

        // A type without a GUID: the target is NULL, never invented.
        let a = none(Col::Value(3000), Col::Null).expect("typed");
        assert_eq!(a.target, Col::Null);
        let a = none(Col::Value(2000), text("")).expect("typed");
        assert_eq!(a.target, Col::Null);

        // A GUID with type 0 or no type column is still visible.
        let a = none(Col::Value(0), text("bp:G")).expect("targeted");
        assert_eq!(a.kind, Some(AssociatedKind::NoAssociation));
        let a = none(Col::Absent, text("G")).expect("targeted");
        assert_eq!((a.raw_type.clone(), a.kind), (Col::Absent, None));

        // Oversized and unreadable GUIDs keep their shape.
        let a = none(Col::Value(2000), Col::Oversized { bytes: 257 }).expect("typed");
        assert_eq!(a.target, Col::Oversized { bytes: 257 });
        let a = none(Col::Null, Col::Unreadable(IssueKind::TypeMismatch)).expect("targeted");
        assert_eq!(a.target, Col::Unreadable(IssueKind::TypeMismatch));
        let a = none(Col::Value(2), Col::Null).expect("typed");
        assert_eq!(a.kind, Some(AssociatedKind::Unknown(2)));
    }
}
