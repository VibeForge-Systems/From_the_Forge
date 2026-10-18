//! Coded values, mapped only where a source documents the meaning (M3 §5.3).
//! Every record keeps the raw value next to any mapped one.
//!
//! Kept raw, with no enum: `chat.is_filtered`, `message.filter_action`,
//! `filter_sub_action`, `chat.style`, `transfer_state`, `share_status`,
//! `schedule_type`, `schedule_state` and `error`. Their meanings across eras
//! are **UNVERIFIED**.
#![deny(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::unreachable,
    clippy::unimplemented,
    clippy::string_slice
)]

use crate::col::Col;

/// The class of a `service` or `service_name` value.
///
/// A case-insensitive ASCII match against `imessage`, `sms`, `rcs` and
/// `imessagelite`; the empty string is [`ServiceClass::Empty`] and anything
/// else is [`ServiceClass::Other`]. The raw value is kept in the record.
/// `IMessageLite` is a literal match of the string `iMessageLite`; no
/// meaning (such as satellite messaging) is claimed for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ServiceClass {
    /// `iMessage`.
    IMessage,
    /// `SMS`.
    Sms,
    /// `RCS`.
    Rcs,
    /// `iMessageLite`.
    IMessageLite,
    /// The empty string.
    Empty,
    /// Any other value; counted, never echoed.
    Other,
}

impl ServiceClass {
    /// Classify raw bytes from the database.
    pub fn classify(raw: &[u8]) -> Self {
        if raw.is_empty() {
            ServiceClass::Empty
        } else if raw.eq_ignore_ascii_case(b"imessage") {
            ServiceClass::IMessage
        } else if raw.eq_ignore_ascii_case(b"sms") {
            ServiceClass::Sms
        } else if raw.eq_ignore_ascii_case(b"rcs") {
            ServiceClass::Rcs
        } else if raw.eq_ignore_ascii_case(b"imessagelite") {
            ServiceClass::IMessageLite
        } else {
            ServiceClass::Other
        }
    }
}

/// A tapback reaction (S2, corroborated by S8). The codes 2000–2005 are
/// love, like, dislike, laugh, emphasize and question; 2006 is an emoji
/// tapback (the emoji is in `associated_message_emoji`) and 2007 a sticker
/// tapback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Reaction {
    /// 2000 / 3000.
    Love,
    /// 2001 / 3001.
    Like,
    /// 2002 / 3002.
    Dislike,
    /// 2003 / 3003.
    Laugh,
    /// 2004 / 3004.
    Emphasize,
    /// 2005 / 3005.
    Question,
    /// 2006 / 3006: an emoji tapback.
    Emoji,
    /// 2007 / 3007: a sticker tapback.
    Sticker,
}

impl Reaction {
    /// The reaction for an offset 0..=7 from 2000 or 3000.
    fn from_offset(offset: i64) -> Option<Self> {
        Some(match offset {
            0 => Reaction::Love,
            1 => Reaction::Like,
            2 => Reaction::Dislike,
            3 => Reaction::Laugh,
            4 => Reaction::Emphasize,
            5 => Reaction::Question,
            6 => Reaction::Emoji,
            7 => Reaction::Sticker,
            _ => return None,
        })
    }
}

/// `message.associated_message_type`, mapped where S2 documents the code
/// (corroborated by S8). The raw value is always kept beside it.
///
/// | Raw | Kind |
/// |---|---|
/// | 0 | `NoAssociation` |
/// | 1000 | `StickerLegacy` |
/// | 2000–2007 | `Tapback { removal: false }` |
/// | 3000–3007 | `Tapback { removal: true }`, removing the matching 2000–2007 |
/// | 4000 | `PollVote` |
/// | anything else, including 2 and 3 | `Unknown(raw)` |
///
/// S2 treats types 2 and 3 as normal messages, but their meaning is
/// **UNVERIFIED**, so they stay `Unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AssociatedKind {
    /// 0.
    NoAssociation,
    /// 1000: a sticker (legacy form).
    StickerLegacy,
    /// 2000–2007 add a reaction; 3000–3007 remove one.
    Tapback {
        /// Which reaction.
        reaction: Reaction,
        /// `true` for 3000–3007.
        removal: bool,
    },
    /// 4000: a vote in a poll.
    PollVote,
    /// Any other value, kept raw.
    Unknown(i64),
}

impl AssociatedKind {
    /// Map a raw `associated_message_type`.
    pub fn from_raw(raw: i64) -> Self {
        let tapback = |base: i64, removal: bool| {
            raw.checked_sub(base)
                .and_then(Reaction::from_offset)
                .map(|reaction| AssociatedKind::Tapback { reaction, removal })
        };
        match raw {
            0 => AssociatedKind::NoAssociation,
            1000 => AssociatedKind::StickerLegacy,
            4000 => AssociatedKind::PollVote,
            2000..=2007 => tapback(2000, false).unwrap_or(AssociatedKind::Unknown(raw)),
            3000..=3007 => tapback(3000, true).unwrap_or(AssociatedKind::Unknown(raw)),
            _ => AssociatedKind::Unknown(raw),
        }
    }
}

/// What a `message` row is, from `item_type` and `group_action_type` (S2,
/// M3 §11.4). Both raw values stay in the record.
///
/// | `item_type` | `group_action_type` | Kind |
/// |---|---|---|
/// | 0 | any | `Message` |
/// | 1 | 0 | `ParticipantAdded`, or `OwnNumberChanged` (derived; below) |
/// | 1 | 1 | `ParticipantRemoved` |
/// | 2 | any | `GroupRenamed` (the title is in `group_title`) |
/// | 3 | 0 / 1 / 2 / 4 / 6 | `ParticipantLeft` / `GroupPhotoChanged` / `GroupPhotoRemoved` / `BackgroundChanged` / `BackgroundRemoved` |
/// | 4 | any | `LegacySharedLocation` |
/// | 5 | any | `AudioKept` |
/// | 6 | any | `SharePlay` |
/// | anything else, NULL, absent or unreadable | | `Unknown` |
///
/// "Any" means `group_action_type` is not read for that row; it is still
/// kept raw. `OwnNumberChanged` is derived **only** when `handle_id` and
/// `other_handle` are both readable, both non-zero and equal.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ItemKind {
    /// An ordinary message.
    Message,
    /// A participant was added to a group.
    ParticipantAdded,
    /// Derived: a "participant added" row naming the same handle on both
    /// sides.
    OwnNumberChanged,
    /// A participant was removed.
    ParticipantRemoved,
    /// The group was renamed.
    GroupRenamed,
    /// A participant left.
    ParticipantLeft,
    /// The group photo was changed.
    GroupPhotoChanged,
    /// The group photo was removed.
    GroupPhotoRemoved,
    /// The chat background was changed.
    BackgroundChanged,
    /// The chat background was removed.
    BackgroundRemoved,
    /// A shared location (legacy form).
    LegacySharedLocation,
    /// An audio message was kept.
    AudioKept,
    /// A SharePlay row.
    SharePlay,
    /// Any combination the table does not map.
    Unknown {
        /// `item_type`, as read.
        item_type: Col<i64>,
        /// `group_action_type`, as read.
        group_action_type: Col<i64>,
    },
}

impl ItemKind {
    /// Classify a row.
    pub fn classify(
        item_type: &Col<i64>,
        group_action_type: &Col<i64>,
        handle_id: &Col<i64>,
        other_handle: &Col<i64>,
    ) -> Self {
        let gat = group_action_type.value().copied();
        let mapped = match item_type.value().copied() {
            Some(0) => Some(ItemKind::Message),
            Some(1) => match gat {
                Some(0) => Some(match (handle_id.value(), other_handle.value()) {
                    (Some(&h), Some(&o)) if h != 0 && h == o => ItemKind::OwnNumberChanged,
                    _ => ItemKind::ParticipantAdded,
                }),
                Some(1) => Some(ItemKind::ParticipantRemoved),
                _ => None,
            },
            Some(2) => Some(ItemKind::GroupRenamed),
            Some(3) => match gat {
                Some(0) => Some(ItemKind::ParticipantLeft),
                Some(1) => Some(ItemKind::GroupPhotoChanged),
                Some(2) => Some(ItemKind::GroupPhotoRemoved),
                Some(4) => Some(ItemKind::BackgroundChanged),
                Some(6) => Some(ItemKind::BackgroundRemoved),
                _ => None,
            },
            Some(4) => Some(ItemKind::LegacySharedLocation),
            Some(5) => Some(ItemKind::AudioKept),
            Some(6) => Some(ItemKind::SharePlay),
            _ => None,
        };
        mapped.unwrap_or_else(|| ItemKind::Unknown {
            item_type: item_type.clone(),
            group_action_type: group_action_type.clone(),
        })
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    #[test]
    fn service_classes() {
        for (raw, want) in [
            (&b"iMessage"[..], ServiceClass::IMessage),
            (b"IMESSAGE", ServiceClass::IMessage),
            (b"imessage", ServiceClass::IMessage),
            (b"SMS", ServiceClass::Sms),
            (b"sms", ServiceClass::Sms),
            (b"RCS", ServiceClass::Rcs),
            (b"iMessageLite", ServiceClass::IMessageLite),
            (b"", ServiceClass::Empty),
            (b" SMS", ServiceClass::Other),
            (b"SMS ", ServiceClass::Other),
            (b"iMessage\0", ServiceClass::Other),
            (b"satellite", ServiceClass::Other),
            (&[0xff, 0xfe][..], ServiceClass::Other),
            // Non-ASCII case folding is not applied: U+0130 is not "I".
            ("\u{130}message".as_bytes(), ServiceClass::Other),
        ] {
            assert_eq!(ServiceClass::classify(raw), want, "{raw:?}");
        }
    }

    #[test]
    fn associated_kinds() {
        use AssociatedKind as K;
        let tb = |reaction, removal| K::Tapback { reaction, removal };
        let all = [
            Reaction::Love,
            Reaction::Like,
            Reaction::Dislike,
            Reaction::Laugh,
            Reaction::Emphasize,
            Reaction::Question,
            Reaction::Emoji,
            Reaction::Sticker,
        ];
        for (i, r) in (0i64..).zip(all) {
            assert_eq!(K::from_raw(2000 + i), tb(r, false));
            assert_eq!(K::from_raw(3000 + i), tb(r, true));
        }
        for (raw, want) in [
            (0, K::NoAssociation),
            (1000, K::StickerLegacy),
            (4000, K::PollVote),
            // Both sides of every boundary.
            (-1, K::Unknown(-1)),
            (1, K::Unknown(1)),
            (2, K::Unknown(2)),
            (3, K::Unknown(3)),
            (999, K::Unknown(999)),
            (1001, K::Unknown(1001)),
            (1999, K::Unknown(1999)),
            (2008, K::Unknown(2008)),
            (2999, K::Unknown(2999)),
            (3008, K::Unknown(3008)),
            (3999, K::Unknown(3999)),
            (4001, K::Unknown(4001)),
            (-2000, K::Unknown(-2000)),
            (i64::MIN, K::Unknown(i64::MIN)),
            (i64::MAX, K::Unknown(i64::MAX)),
        ] {
            assert_eq!(K::from_raw(raw), want, "{raw}");
        }
    }

    /// The whole §11.4 table.
    #[test]
    fn item_kinds() {
        use ItemKind as I;
        let v = Col::Value;
        let (absent, null) = (Col::<i64>::Absent, Col::<i64>::Null);
        let bad = Col::<i64>::Unreadable(crate::IssueKind::TypeMismatch);
        let any_gat = [absent.clone(), null.clone(), v(0), v(1), v(7), v(-1)];
        let plain = |it: &Col<i64>, gat: &Col<i64>| ItemKind::classify(it, gat, &v(5), &v(6));
        let unknown = |it: &Col<i64>, gat: &Col<i64>| I::Unknown {
            item_type: it.clone(),
            group_action_type: gat.clone(),
        };
        for gat in &any_gat {
            assert_eq!(plain(&v(0), gat), I::Message);
            assert_eq!(plain(&v(2), gat), I::GroupRenamed);
            assert_eq!(plain(&v(4), gat), I::LegacySharedLocation);
            assert_eq!(plain(&v(5), gat), I::AudioKept);
            assert_eq!(plain(&v(6), gat), I::SharePlay);
            for it in [
                v(7),
                v(-1),
                v(i64::MIN),
                v(i64::MAX),
                null.clone(),
                absent.clone(),
                bad.clone(),
            ] {
                assert_eq!(plain(&it, gat), unknown(&it, gat));
            }
        }
        assert_eq!(plain(&v(1), &v(0)), I::ParticipantAdded);
        assert_eq!(plain(&v(1), &v(1)), I::ParticipantRemoved);
        for gat in [null.clone(), absent.clone(), bad.clone(), v(2), v(-1)] {
            assert_eq!(plain(&v(1), &gat), unknown(&v(1), &gat));
        }
        for (gat, want) in [
            (0, I::ParticipantLeft),
            (1, I::GroupPhotoChanged),
            (2, I::GroupPhotoRemoved),
            (4, I::BackgroundChanged),
            (6, I::BackgroundRemoved),
        ] {
            assert_eq!(plain(&v(3), &v(gat)), want);
        }
        for gat in [v(3), v(5), v(7), null, absent.clone()] {
            assert_eq!(plain(&v(3), &gat), unknown(&v(3), &gat));
        }

        // OwnNumberChanged: both readable, both non-zero, equal.
        let own = |h: Col<i64>, o: Col<i64>| ItemKind::classify(&v(1), &v(0), &h, &o);
        assert_eq!(own(v(9), v(9)), I::OwnNumberChanged);
        assert_eq!(own(v(0), v(0)), I::ParticipantAdded);
        assert_eq!(own(v(9), v(8)), I::ParticipantAdded);
        assert_eq!(own(v(9), absent.clone()), I::ParticipantAdded);
        assert_eq!(own(Col::Null, v(9)), I::ParticipantAdded);
        assert_eq!(own(bad.clone(), bad), I::ParticipantAdded);
        // Only item_type 1 / group_action_type 0 derives it.
        assert_eq!(
            ItemKind::classify(&v(3), &v(0), &v(9), &v(9)),
            I::ParticipantLeft
        );
    }
}
