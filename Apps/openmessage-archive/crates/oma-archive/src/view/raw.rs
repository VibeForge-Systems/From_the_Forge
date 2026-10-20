//! Raw codes and states as recorded (M6 spec §3.9), for exports.
//!
//! The viewer's types collapse what this build cannot name: an unknown
//! item kind, edit label, service or unit is `None`, an unknown `_st`
//! state reads `Unreadable`, `is_from_me` other than 0 and 1 is "not
//! recorded", and presence flags (`balloon_bundle_id`,
//! `expressive_send_style_id`) read `false` when the column is absent.
//! An export must not lose those numbers (I5: never approximated), so the
//! assembly also returns them here, read from the same row:
//!
//! - a coded column is a [`Coded`]: the known code, or the unknown number;
//! - a raw column with a state is a [`RawInt`], a [`RawTime`] or a
//!   [`StateBool`], whose state is itself coded;
//! - direction has four tokens ([`RawDirection`]): `unknownValue` is never
//!   `notRecorded`.
//!
//! The viewer is unchanged: `Caps::PAGE` pages still carry only the M5
//! view types, and these values never reach a thread page or a detail.

use crate::schema::codes::{
    AssocKindCode, AssocTargetShape, AttributedOutcome, DisplayNoneReason, DisplaySource,
    EditLabelCode, ItemKindCode, ServiceClassCode, TimeUnit, TsErrorCode, ValueState,
};

/// A coded column: a code this build knows, or the number it does not.
pub type Coded<T> = Result<T, i64>;

/// `v` read through `of`: the known code, or `Err(v)`.
pub(crate) fn coded<T>(v: i64, of: fn(i64) -> Option<T>) -> Coded<T> {
    of(v).ok_or(v)
}

/// An optional coded column: `None` for SQL NULL.
pub(crate) fn opt_coded<T>(v: Option<i64>, of: fn(i64) -> Option<T>) -> Option<Coded<T>> {
    v.map(|v| coded(v, of))
}

/// A `_st` column: `None` when the value column holds the value.
pub(crate) fn state(st: Option<i64>) -> Option<Coded<ValueState>> {
    opt_coded(st, ValueState::from_code)
}

/// A raw integer column and its state (`schedule_type`, `total_bytes`,
/// `is_sticker`, `item_type`, …). Its meaning is not claimed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct RawInt {
    /// The value, when the column holds one.
    pub raw: Option<i64>,
    /// Why there is no value; `None` with one.
    pub state: Option<Coded<ValueState>>,
}

impl RawInt {
    pub(crate) fn of(raw: Option<i64>, st: Option<i64>) -> RawInt {
        RawInt {
            raw,
            state: state(st),
        }
    }
}

/// A timestamp quad exactly as stored (`x_raw`, `x_st`, `x_unit`,
/// `x_utc_ns`): the unit and the state keep unknown codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct RawTime {
    pub raw: Option<i64>,
    pub state: Option<Coded<ValueState>>,
    /// How the import read the raw value; `None` without a value.
    pub unit: Option<Coded<TimeUnit>>,
    /// The instant the import derived (seconds or nanoseconds units only).
    pub utc_ns: Option<i64>,
}

impl RawTime {
    pub(crate) fn of(
        raw: Option<i64>,
        st: Option<i64>,
        unit: Option<i64>,
        utc_ns: Option<i64>,
    ) -> RawTime {
        RawTime {
            raw,
            state: state(st),
            unit: opt_coded(unit, TimeUnit::from_code),
            utc_ns,
        }
    }
}

/// A fact derived from a column's presence (`balloon_bundle_id`: app
/// content; `expressive_send_style_id`: an effect), with the column's
/// state: absent or unreadable is never `false`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct StateBool {
    /// With a value: whether it is non-empty.
    pub value: Option<bool>,
    /// Why there is no value; `None` with one.
    pub state: Option<Coded<ValueState>>,
}

impl StateBool {
    /// From the value's length (NULL without a value) and its state.
    pub(crate) fn of(len: Option<i64>, st: Option<i64>) -> StateBool {
        StateBool {
            value: if st.is_some() {
                None
            } else {
                len.map(|n| n > 0)
            },
            state: state(st),
        }
    }
}

/// `message.is_from_me` with four tokens (§3.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RawDirection {
    /// `is_from_me = 1`.
    FromMe,
    /// `is_from_me = 0`.
    Received,
    /// NULL in the source, the column absent, or unreadable: the state.
    /// `None` only for a row without either (which v1's `CHECK` refuses).
    NotRecorded { state: Option<Coded<ValueState>> },
    /// Any other recorded value, kept as is.
    UnknownValue { raw: i64 },
}

impl RawDirection {
    pub(crate) fn of(is_from_me: Option<i64>, st: Option<i64>) -> RawDirection {
        match (is_from_me, st) {
            (Some(1), None) => RawDirection::FromMe,
            (Some(0), None) => RawDirection::Received,
            (Some(raw), None) => RawDirection::UnknownValue { raw },
            (_, st) => RawDirection::NotRecorded { state: state(st) },
        }
    }
}

/// The current body's codes as stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct BodyCodes {
    /// `None` when the body has no displayed text.
    pub display_source: Option<Coded<DisplaySource>>,
    pub display_none_reason: Option<Coded<DisplayNoneReason>>,
    pub attributed_outcome: Coded<AttributedOutcome>,
    pub attributed_error: Option<Coded<TsErrorCode>>,
    /// "Formatting could not be read".
    pub run_issue: Option<Coded<TsErrorCode>>,
    /// The plain `text` column's state.
    pub text_raw_state: Option<Coded<ValueState>>,
    pub edit_label: Coded<EditLabelCode>,
}

/// One attachment's raw values, in the order of
/// [`AssembledMessage::attachments`](crate::AssembledMessage::attachments).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct AttachmentCodes {
    /// `total_bytes`, declared.
    pub declared_bytes: RawInt,
    /// `is_sticker`.
    pub sticker: RawInt,
    /// `blob.byte_len`; `None` when no file is stored.
    pub stored_bytes: Option<i64>,
}

/// The association's codes as stored (`assoc_present` 1 or 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct AssociationCodes {
    /// 1 yes, 2 not determinable.
    pub present: i64,
    pub kind: Option<Coded<AssocKindCode>>,
    pub shape: Option<Coded<AssocTargetShape>>,
}

/// What the viewer's types collapse, kept for an export (§3.9).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RawCodes {
    pub item: Coded<ItemKindCode>,
    pub direction: RawDirection,
    /// The derived service class; `None` when NULL.
    pub service: Option<Coded<ServiceClassCode>>,
    pub date: RawTime,
    /// `item_type` and `group_action_type`, raw.
    pub item_type: RawInt,
    pub group_action_type: RawInt,
    pub schedule_type: RawInt,
    pub app_content: StateBool,
    pub effect: StateBool,
    pub association: Option<AssociationCodes>,
    /// `None` without a current body row.
    pub body: Option<BodyCodes>,
    pub attachments: Vec<AttachmentCodes>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_numbers_and_states_are_kept() {
        assert_eq!(coded(1, ItemKindCode::from_code), Ok(ItemKindCode::Message));
        assert_eq!(coded(99, ItemKindCode::from_code), Err(99));
        assert_eq!(state(None), None);
        assert_eq!(state(Some(2)), Some(Ok(ValueState::Absent)));
        assert_eq!(state(Some(9)), Some(Err(9)));
        let t = RawTime::of(Some(5), None, Some(9), None);
        assert_eq!((t.raw, t.state, t.unit), (Some(5), None, Some(Err(9))));
        let t = RawTime::of(None, Some(7), None, None);
        assert_eq!((t.raw, t.state, t.unit), (None, Some(Err(7)), None));
    }

    #[test]
    fn direction_has_four_tokens() {
        assert_eq!(RawDirection::of(Some(1), None), RawDirection::FromMe);
        assert_eq!(RawDirection::of(Some(0), None), RawDirection::Received);
        assert_eq!(
            RawDirection::of(Some(2), None),
            RawDirection::UnknownValue { raw: 2 }
        );
        assert_eq!(
            RawDirection::of(Some(-1), None),
            RawDirection::UnknownValue { raw: -1 }
        );
        assert_eq!(
            RawDirection::of(None, Some(1)),
            RawDirection::NotRecorded {
                state: Some(Ok(ValueState::Null))
            }
        );
        assert_eq!(
            RawDirection::of(None, Some(42)),
            RawDirection::NotRecorded {
                state: Some(Err(42))
            }
        );
        assert_ne!(
            RawDirection::of(Some(2), None),
            RawDirection::of(None, Some(4))
        );
    }

    #[test]
    fn presence_booleans_keep_their_state() {
        assert_eq!(StateBool::of(Some(3), None).value, Some(true));
        assert_eq!(StateBool::of(Some(0), None).value, Some(false));
        let absent = StateBool::of(None, Some(2));
        assert_eq!(
            (absent.value, absent.state),
            (None, Some(Ok(ValueState::Absent)))
        );
        let odd = StateBool::of(None, Some(8));
        assert_eq!((odd.value, odd.state), (None, Some(Err(8))));
        let i = RawInt::of(None, Some(4));
        assert_eq!((i.raw, i.state), (None, Some(Ok(ValueState::Unreadable))));
    }
}
