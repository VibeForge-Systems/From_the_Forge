//! Closed numeric codes stored in `archive.sqlite`, and the column-rule
//! table of spec §8.7.
//!
//! **A code is never renumbered or reused.** Every code is pinned by the
//! tests below; a value that leaves the list retires its number. The codes
//! here are the ones `v1.sql` constrains with a CHECK or names in a
//! comment. Codes that belong to later import steps (passes, issue codes,
//! WAL outcomes, text encodings) are added with those steps.
//!
//! [`ARCHIVE_COLUMNS`] gives every source-value column of schema v1 (each
//! `x` with an `x_st`, and each timestamp quad) a stable code, which
//! `value_issue.col` and `value_history.col` store, and the merge rule that
//! applies to it. A test checks the list against `v1.sql` in both
//! directions.

/// Declares a closed code enum with `code()`, `from_code()` and `ALL`.
macro_rules! closed_code {
    (
        $(#[$meta:meta])*
        $name:ident { $( $(#[$vmeta:meta])* $variant:ident = $code:literal ),+ $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name {
            $( $(#[$vmeta])* $variant ),+
        }

        impl $name {
            /// Every value, in code order.
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            /// The stored code.
            pub const fn code(self) -> i64 {
                match self {
                    $($name::$variant => $code),+
                }
            }

            /// The value with this stored code, if any.
            pub const fn from_code(code: i64) -> Option<$name> {
                match code {
                    $($code => Some($name::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

closed_code! {
    /// `x_st`: why `x` holds no value (spec §6.1). `x_st IS NULL` means `x`
    /// holds the value.
    ValueState {
        /// SQL NULL in the source.
        Null = 1,
        /// The column does not exist in the source.
        Absent = 2,
        /// Withheld: over the read cap (byte count in `value_issue`).
        Oversized = 3,
        /// Unreadable (kind and, when kept, the raw value in `value_issue`).
        Unreadable = 4,
    }
}

closed_code! {
    /// `*_unit` of a timestamp quad (M3 `time.rs`).
    TimeUnit {
        Zero = 0,
        Negative = 1,
        Seconds = 2,
        Nanoseconds = 3,
        Ambiguous = 4,
    }
}

closed_code! {
    /// The entity a `value_issue`, `value_history`, `import_dup_key`,
    /// `import_occurrence`, `import_rowid_map` or `lineage_seen` row is
    /// about. Codes 1 to 4 are also the `src` codes of `import_rowid_map`.
    Entity {
        Handle = 1,
        Chat = 2,
        Message = 3,
        Attachment = 4,
        MessageBody = 5,
        ChatMessage = 6,
        MessageRecentlyDeleted = 7,
    }
}

closed_code! {
    /// `key_kind` (spec §6.2).
    KeyKind {
        /// The exact GUID bytes, or the tagged (address, service) of a handle.
        Exact = 1,
        /// No usable or unique key: a fingerprint and an occurrence number.
        Unkeyed = 2,
    }
}

closed_code! {
    /// `import_session.state`.
    SessionState {
        Running = 0,
        Completed = 1,
        Cancelled = 2,
        Failed = 3,
        Interrupted = 4,
        Superseded = 5,
        /// Resumable stop (space, source change, volume file limit; D88).
        Stopped = 6,
    }
}

closed_code! {
    /// `value_issue.kind` below [`ISSUE_KIND_BASE`].
    ValueIssueKind {
        /// Over the read cap; `bytes` holds SQLite's byte count.
        Oversized = 1,
        /// The raw value is kept in `raw`.
        RawKept = 2,
        /// SQLite reported a BLOB in a text column (R7).
        TextStoredAsBlob = 3,
    }
}

/// `value_issue.kind` codes from this value up carry an `oma-messages`
/// issue kind (added with the import passes).
pub const ISSUE_KIND_BASE: i64 = 16;

closed_code! {
    /// `message_body.attributed_outcome`.
    AttributedOutcome {
        Absent = 0,
        Null = 1,
        Oversized = 2,
        Unreadable = 3,
        NotDecoded = 4,
        Decoded = 5,
        DecodeFailed = 6,
    }
}

closed_code! {
    /// `message_body.summary_outcome`.
    SummaryOutcome {
        Absent = 0,
        Null = 1,
        Oversized = 2,
        Unreadable = 3,
        Parsed = 4,
    }
}

closed_code! {
    /// `message_body.ec_state` and `rp_state`.
    FieldOutcome {
        Missing = 0,
        Value = 1,
        Bad = 2,
    }
}

closed_code! {
    /// `message_body.display_source`.
    DisplaySource {
        /// The decoded `attributedBody`.
        AttributedBody = 1,
        /// The `text` column, valid UTF-8.
        Text = 2,
        /// The `text` column, decoded lossily.
        TextLossy = 3,
    }
}

closed_code! {
    /// `message.assoc_present`.
    AssocPresent {
        No = 0,
        Yes = 1,
        Undeterminable = 2,
    }
}

closed_code! {
    /// `message.assoc_target_shape`.
    AssocTargetShape {
        Part = 1,
        Bubble = 2,
        Bare = 3,
        Unparsed = 4,
    }
}

closed_code! {
    /// `import_session.order_class`.
    OrderClass {
        Undated = 0,
        Dated = 1,
    }
}

closed_code! {
    /// `import_session.order_key_source`.
    OrderKeySource {
        /// Status.plist `Date`.
        StatusDate = 1,
        /// Info.plist `Last Backup Date`.
        InfoLastBackupDate = 2,
    }
}

closed_code! {
    /// `verify_run.depth`.
    VerifyDepthCode {
        Quick = 0,
        Full = 1,
    }
}

closed_code! {
    /// `verify_run.outcome`.
    VerifyOutcome {
        Ok = 0,
        Problems = 1,
        Cancelled = 2,
        Incomplete = 3,
    }
}

/// The merge rule of a source-value column (spec §8.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColumnRule {
    /// Part of the row's identity: immutable.
    Key,
    /// Raw value plus `_st`: a Value or Null observation replaces the stored
    /// pair at a rank at least `last_rank`, or when the stored state is 2,
    /// 3 or 4; states 2 to 4 never replace a Value or Null.
    Raw,
    /// [`ColumnRule::Raw`], plus every distinct observed Value or Null that
    /// is not the current one is kept in `value_history` (D82).
    History,
    /// A timestamp quad: the raw integer follows [`ColumnRule::Raw`]; the
    /// unit and instant are derived from it.
    Time,
    /// A raw input of one body version (`message_body`): fixed by the
    /// version's `raw_hash`, so immutable.
    BodyRaw,
}

/// One source-value column of schema v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveColumn {
    pub entity: Entity,
    /// The column's base name: `x` of the pair `x`/`x_st`, or `x` of the
    /// quad `x_raw`/`x_st`/`x_unit`/`x_utc_ns`.
    pub name: &'static str,
    /// `entity.code() × 100` plus a number written beside the column below.
    pub code: u16,
    pub rule: ColumnRule,
}

const fn col(entity: Entity, name: &'static str, code: u16, rule: ColumnRule) -> ArchiveColumn {
    ArchiveColumn {
        entity,
        name,
        code,
        rule,
    }
}

/// Every source-value column of schema v1, with its code and rule. The
/// history columns are the closed list of D82.
pub const ARCHIVE_COLUMNS: &[ArchiveColumn] = &[
    col(Entity::Handle, "address", 101, ColumnRule::Key),
    col(Entity::Handle, "service", 102, ColumnRule::Key),
    col(Entity::Handle, "country", 103, ColumnRule::Raw),
    col(
        Entity::Handle,
        "uncanonicalized_id",
        104,
        ColumnRule::History,
    ),
    col(
        Entity::Handle,
        "person_centric_id",
        105,
        ColumnRule::History,
    ),
    col(Entity::Chat, "guid", 201, ColumnRule::Key),
    col(Entity::Chat, "style", 202, ColumnRule::Raw),
    col(Entity::Chat, "chat_identifier", 203, ColumnRule::Raw),
    col(Entity::Chat, "service_name", 204, ColumnRule::Raw),
    col(Entity::Chat, "display_name", 205, ColumnRule::History),
    col(Entity::Chat, "room_name", 206, ColumnRule::History),
    col(Entity::Chat, "group_id", 207, ColumnRule::Raw),
    col(Entity::Chat, "properties_bytes", 208, ColumnRule::Raw),
    col(Entity::Chat, "is_archived", 209, ColumnRule::Raw),
    col(Entity::Chat, "is_filtered", 210, ColumnRule::Raw),
    col(Entity::Chat, "is_blackholed", 211, ColumnRule::Raw),
    col(Entity::Chat, "is_pending_review", 212, ColumnRule::Raw),
    col(Entity::Chat, "is_recovered", 213, ColumnRule::Raw),
    col(Entity::Chat, "last_read", 214, ColumnRule::Time),
    col(Entity::Message, "guid", 301, ColumnRule::Key),
    col(Entity::Message, "is_from_me", 302, ColumnRule::Raw),
    col(Entity::Message, "handle_rowid", 303, ColumnRule::Raw),
    col(Entity::Message, "other_handle_rowid", 304, ColumnRule::Raw),
    col(
        Entity::Message,
        "destination_caller_id",
        305,
        ColumnRule::History,
    ),
    col(Entity::Message, "service", 306, ColumnRule::Raw),
    col(Entity::Message, "date", 307, ColumnRule::Time),
    col(Entity::Message, "date_read", 308, ColumnRule::Time),
    col(Entity::Message, "date_delivered", 309, ColumnRule::Time),
    col(Entity::Message, "date_played", 310, ColumnRule::Time),
    col(Entity::Message, "date_edited", 311, ColumnRule::Time),
    col(Entity::Message, "date_retracted", 312, ColumnRule::Time),
    col(Entity::Message, "is_read", 313, ColumnRule::Raw),
    col(Entity::Message, "is_delivered", 314, ColumnRule::Raw),
    col(Entity::Message, "is_sent", 315, ColumnRule::Raw),
    col(Entity::Message, "is_audio_message", 316, ColumnRule::Raw),
    col(
        Entity::Message,
        "cache_has_attachments",
        317,
        ColumnRule::Raw,
    ),
    col(Entity::Message, "subject", 318, ColumnRule::History),
    col(Entity::Message, "item_type", 319, ColumnRule::Raw),
    col(Entity::Message, "group_action_type", 320, ColumnRule::Raw),
    col(Entity::Message, "group_title", 321, ColumnRule::History),
    col(Entity::Message, "share_status", 322, ColumnRule::Raw),
    col(Entity::Message, "share_direction", 323, ColumnRule::Raw),
    col(Entity::Message, "assoc_type", 324, ColumnRule::Raw),
    col(Entity::Message, "assoc_guid_raw", 325, ColumnRule::Raw),
    col(Entity::Message, "assoc_emoji", 326, ColumnRule::Raw),
    col(
        Entity::Message,
        "assoc_range_location",
        327,
        ColumnRule::Raw,
    ),
    col(Entity::Message, "assoc_range_length", 328, ColumnRule::Raw),
    col(
        Entity::Message,
        "thread_originator_guid",
        329,
        ColumnRule::Raw,
    ),
    col(
        Entity::Message,
        "thread_originator_part",
        330,
        ColumnRule::Raw,
    ),
    col(Entity::Message, "reply_to_guid", 331, ColumnRule::Raw),
    col(Entity::Message, "balloon_bundle_id", 332, ColumnRule::Raw),
    col(
        Entity::Message,
        "expressive_send_style_id",
        333,
        ColumnRule::Raw,
    ),
    col(Entity::Message, "payload_data", 334, ColumnRule::History),
    col(Entity::Message, "part_count", 335, ColumnRule::Raw),
    col(Entity::Message, "schedule_type", 336, ColumnRule::Raw),
    col(Entity::Message, "schedule_state", 337, ColumnRule::Raw),
    col(Entity::Message, "filter_action", 338, ColumnRule::Raw),
    col(Entity::Message, "filter_sub_action", 339, ColumnRule::Raw),
    col(Entity::Message, "error", 340, ColumnRule::Raw),
    col(Entity::Message, "was_downgraded", 341, ColumnRule::Raw),
    col(Entity::Attachment, "guid", 401, ColumnRule::Key),
    col(Entity::Attachment, "original_guid", 402, ColumnRule::Raw),
    col(Entity::Attachment, "filename", 403, ColumnRule::History),
    col(Entity::Attachment, "uti", 404, ColumnRule::Raw),
    col(Entity::Attachment, "mime_type", 405, ColumnRule::Raw),
    col(
        Entity::Attachment,
        "transfer_name",
        406,
        ColumnRule::History,
    ),
    col(Entity::Attachment, "total_bytes", 407, ColumnRule::Raw),
    col(Entity::Attachment, "is_sticker", 408, ColumnRule::Raw),
    col(Entity::Attachment, "hide_attachment", 409, ColumnRule::Raw),
    col(Entity::Attachment, "is_outgoing", 410, ColumnRule::Raw),
    col(Entity::Attachment, "transfer_state", 411, ColumnRule::Raw),
    col(Entity::Attachment, "created", 412, ColumnRule::Time),
    col(Entity::Attachment, "start", 413, ColumnRule::Time),
    col(
        Entity::Attachment,
        "emoji_description",
        414,
        ColumnRule::History,
    ),
    col(Entity::MessageBody, "text_raw", 501, ColumnRule::BodyRaw),
    col(
        Entity::MessageBody,
        "attributed_raw",
        502,
        ColumnRule::BodyRaw,
    ),
    col(Entity::MessageBody, "summary_raw", 503, ColumnRule::BodyRaw),
    col(Entity::ChatMessage, "join_date", 601, ColumnRule::Time),
    col(
        Entity::MessageRecentlyDeleted,
        "chat_rowid",
        701,
        ColumnRule::Raw,
    ),
    col(
        Entity::MessageRecentlyDeleted,
        "delete",
        702,
        ColumnRule::Time,
    ),
    col(
        Entity::MessageRecentlyDeleted,
        "ck_sync_state",
        703,
        ColumnRule::Raw,
    ),
];

impl ArchiveColumn {
    /// The column with this code, if any.
    pub fn from_code(code: u16) -> Option<ArchiveColumn> {
        ARCHIVE_COLUMNS.iter().copied().find(|c| c.code == code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    #[test]
    fn codes_are_pinned() {
        let pins: [(&str, Vec<i64>); 16] = [
            (
                "ValueState",
                ValueState::ALL.iter().map(|v| v.code()).collect(),
            ),
            ("TimeUnit", TimeUnit::ALL.iter().map(|v| v.code()).collect()),
            ("Entity", Entity::ALL.iter().map(|v| v.code()).collect()),
            ("KeyKind", KeyKind::ALL.iter().map(|v| v.code()).collect()),
            (
                "SessionState",
                SessionState::ALL.iter().map(|v| v.code()).collect(),
            ),
            (
                "ValueIssueKind",
                ValueIssueKind::ALL.iter().map(|v| v.code()).collect(),
            ),
            (
                "AttributedOutcome",
                AttributedOutcome::ALL.iter().map(|v| v.code()).collect(),
            ),
            (
                "SummaryOutcome",
                SummaryOutcome::ALL.iter().map(|v| v.code()).collect(),
            ),
            (
                "FieldOutcome",
                FieldOutcome::ALL.iter().map(|v| v.code()).collect(),
            ),
            (
                "DisplaySource",
                DisplaySource::ALL.iter().map(|v| v.code()).collect(),
            ),
            (
                "AssocPresent",
                AssocPresent::ALL.iter().map(|v| v.code()).collect(),
            ),
            (
                "AssocTargetShape",
                AssocTargetShape::ALL.iter().map(|v| v.code()).collect(),
            ),
            (
                "OrderClass",
                OrderClass::ALL.iter().map(|v| v.code()).collect(),
            ),
            (
                "OrderKeySource",
                OrderKeySource::ALL.iter().map(|v| v.code()).collect(),
            ),
            (
                "VerifyDepthCode",
                VerifyDepthCode::ALL.iter().map(|v| v.code()).collect(),
            ),
            (
                "VerifyOutcome",
                VerifyOutcome::ALL.iter().map(|v| v.code()).collect(),
            ),
        ];
        let expected: [(&str, &[i64]); 16] = [
            ("ValueState", &[1, 2, 3, 4]),
            ("TimeUnit", &[0, 1, 2, 3, 4]),
            ("Entity", &[1, 2, 3, 4, 5, 6, 7]),
            ("KeyKind", &[1, 2]),
            ("SessionState", &[0, 1, 2, 3, 4, 5, 6]),
            ("ValueIssueKind", &[1, 2, 3]),
            ("AttributedOutcome", &[0, 1, 2, 3, 4, 5, 6]),
            ("SummaryOutcome", &[0, 1, 2, 3, 4]),
            ("FieldOutcome", &[0, 1, 2]),
            ("DisplaySource", &[1, 2, 3]),
            ("AssocPresent", &[0, 1, 2]),
            ("AssocTargetShape", &[1, 2, 3, 4]),
            ("OrderClass", &[0, 1]),
            ("OrderKeySource", &[1, 2]),
            ("VerifyDepthCode", &[0, 1]),
            ("VerifyOutcome", &[0, 1, 2, 3]),
        ];
        for ((name, got), (ename, want)) in pins.iter().zip(expected.iter()) {
            assert_eq!(name, ename);
            assert_eq!(got.as_slice(), *want, "{name}");
        }
        assert_eq!(SessionState::Stopped.code(), 6);
        assert_eq!(SessionState::from_code(5), Some(SessionState::Superseded));
        assert_eq!(SessionState::from_code(7), None);
        assert_eq!(ValueState::from_code(0), None);
        assert_eq!(ISSUE_KIND_BASE, 16);
        // Round trips.
        for e in Entity::ALL {
            assert_eq!(Entity::from_code(e.code()), Some(*e));
        }
    }

    /// The (entity, code, name, rule) list, pinned: renumbering any column
    /// fails here.
    #[test]
    fn column_codes_are_pinned_and_unique() {
        let mut codes = BTreeSet::new();
        let mut names = BTreeSet::new();
        for c in ARCHIVE_COLUMNS {
            assert!(codes.insert(c.code), "code {} used twice", c.code);
            assert!(names.insert((c.entity.code(), c.name)), "{} twice", c.name);
            let base = u16::try_from(c.entity.code()).expect("small") * 100;
            assert!(c.code > base && c.code < base + 100, "{}", c.name);
            assert_eq!(ArchiveColumn::from_code(c.code), Some(*c));
        }
        assert_eq!(ARCHIVE_COLUMNS.len(), 81);
        let spot = [
            (101, "address", ColumnRule::Key),
            (104, "uncanonicalized_id", ColumnRule::History),
            (205, "display_name", ColumnRule::History),
            (214, "last_read", ColumnRule::Time),
            (301, "guid", ColumnRule::Key),
            (307, "date", ColumnRule::Time),
            (318, "subject", ColumnRule::History),
            (334, "payload_data", ColumnRule::History),
            (341, "was_downgraded", ColumnRule::Raw),
            (403, "filename", ColumnRule::History),
            (414, "emoji_description", ColumnRule::History),
            (502, "attributed_raw", ColumnRule::BodyRaw),
            (601, "join_date", ColumnRule::Time),
            (703, "ck_sync_state", ColumnRule::Raw),
        ];
        for (code, name, rule) in spot {
            let c = ArchiveColumn::from_code(code).expect("present");
            assert_eq!((c.name, c.rule), (name, rule), "{code}");
        }
        assert_eq!(ArchiveColumn::from_code(100), None);
        assert_eq!(ArchiveColumn::from_code(342), None);
    }

    #[test]
    fn the_history_list_is_the_closed_list_of_d82() {
        let history: BTreeSet<(i64, &str)> = ARCHIVE_COLUMNS
            .iter()
            .filter(|c| c.rule == ColumnRule::History)
            .map(|c| (c.entity.code(), c.name))
            .collect();
        let want: BTreeSet<(i64, &str)> = [
            (3, "subject"),
            (3, "group_title"),
            (3, "payload_data"),
            (3, "destination_caller_id"),
            (2, "display_name"),
            (2, "room_name"),
            (4, "filename"),
            (4, "transfer_name"),
            (4, "emoji_description"),
            (1, "uncanonicalized_id"),
            (1, "person_centric_id"),
        ]
        .into_iter()
        .collect();
        assert_eq!(history, want);
    }

    /// Every `x`/`x_st` pair and every timestamp quad in `v1.sql` has an
    /// entry, and every entry names one.
    #[test]
    fn the_column_list_matches_v1_sql() {
        let tables: BTreeMap<&str, Entity> = [
            ("handle", Entity::Handle),
            ("chat", Entity::Chat),
            ("message", Entity::Message),
            ("attachment", Entity::Attachment),
            ("message_body", Entity::MessageBody),
            ("chat_message", Entity::ChatMessage),
            ("message_recently_deleted", Entity::MessageRecentlyDeleted),
        ]
        .into_iter()
        .collect();
        let conn = crate::conn::open_memory().expect("memory");
        conn.execute_batch(super::super::V1_SQL)
            .expect("v1.sql loads");
        let mut from_schema: BTreeMap<(i64, String), ColumnRule> = BTreeMap::new();
        let mut all_tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_schema WHERE type = 'table'")
            .expect("prepare")
            .query_map([], |r| r.get(0))
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("rows");
        all_tables.sort();
        for table in &all_tables {
            let cols: BTreeSet<String> = conn
                .prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
                .expect("prepare")
                .query_map([], |r| r.get(0))
                .expect("query")
                .collect::<Result<_, _>>()
                .expect("rows");
            for c in &cols {
                let Some(base) = c.strip_suffix("_st") else {
                    continue;
                };
                let entity = *tables
                    .get(table.as_str())
                    .unwrap_or_else(|| panic!("{table} has value columns but no entity"));
                let quad =
                    cols.contains(&format!("{base}_raw")) && cols.contains(&format!("{base}_unit"));
                assert!(
                    quad || cols.contains(base),
                    "{table}.{c} has no value column"
                );
                from_schema.insert(
                    (entity.code(), base.to_owned()),
                    if quad {
                        ColumnRule::Time
                    } else {
                        ColumnRule::Raw
                    },
                );
            }
        }
        let listed: BTreeMap<(i64, String), ColumnRule> = ARCHIVE_COLUMNS
            .iter()
            .map(|c| ((c.entity.code(), c.name.to_owned()), c.rule))
            .collect();
        assert_eq!(
            from_schema.keys().collect::<Vec<_>>(),
            listed.keys().collect::<Vec<_>>()
        );
        for (k, rule) in &from_schema {
            let got = listed.get(k).expect("listed");
            if *rule == ColumnRule::Time {
                assert_eq!(got, &ColumnRule::Time, "{k:?}");
            } else {
                assert_ne!(got, &ColumnRule::Time, "{k:?}");
            }
        }
    }
}
