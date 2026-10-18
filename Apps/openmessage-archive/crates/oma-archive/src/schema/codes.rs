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

closed_code! {
    /// `blob.verify_state`, set by the full verifier (spec §11). NULL means
    /// never verified.
    BlobVerifyState {
        /// The file re-hashed to its `sha256`.
        Ok = 0,
        /// No file at the storage name.
        Missing = 1,
        /// A link, a directory or another non-regular file, or a `<hh>`
        /// directory that is not a private real directory.
        NotRegular = 2,
        /// A regular file whose length differs from `byte_len`.
        LengthMismatch = 3,
        /// A regular file of the right length whose content hashes to
        /// another digest.
        HashMismatch = 4,
        /// Reading the file failed.
        Unreadable = 5,
        /// `storage_name` does not parse, or does not spell `sha256`.
        BadName = 6,
    }
}

closed_code! {
    /// `import_cursor.pass` and `import_issue.pass`: the import passes of
    /// spec §8.4 that write a cursor (P0 and P5b read only), and P9, the
    /// finalize step, which writes no cursor but can record an issue.
    Pass {
        Handles = 1,
        Chats = 2,
        Messages = 3,
        ChatHandles = 4,
        ChatMessages = 5,
        Attachments = 6,
        MessageAttachments = 7,
        RecentlyDeleted = 8,
        Finalize = 9,
    }
}

closed_code! {
    /// `import_session.wal_outcome`: what happened to sms.db's `-wal`.
    WalOutcomeCode {
        /// The backup lists no `-wal`.
        NotListed = 0,
        /// The `-wal` was staged; `wal_len` holds its length.
        Staged = 1,
        /// Listed but not provided (best effort): not a full observation.
        Skipped = 2,
    }
}

closed_code! {
    /// `import_session.source_text_encoding`: sms.db's `PRAGMA encoding`.
    TextEncodingCode {
        Utf8 = 1,
        Utf16le = 2,
        Utf16be = 3,
    }
}

closed_code! {
    /// `handle.service_class`, `chat.service_class` and
    /// `message.service_class`: the class of the raw service text.
    ServiceClassCode {
        IMessage = 1,
        Sms = 2,
        Rcs = 3,
        IMessageLite = 4,
        Empty = 5,
        Other = 6,
    }
}

closed_code! {
    /// `value_issue.kind` from [`ISSUE_KIND_BASE`] up: why a value is
    /// unreadable (the `oma-messages` `IssueKind`).
    UnreadableKind {
        TypeMismatch = 16,
        BoolOutOfRange = 17,
        PlistInvalid = 18,
        PlistOverBudget = 19,
        PlistNotDictionary = 20,
        /// A reason this build does not know (a newer reader). Never
        /// produced by a known reason; a test checks that.
        Unrecognised = 31,
    }
}

closed_code! {
    /// `import_issue.kind`: what an import counted per pass, including the
    /// reasons a session stopped.
    IssueCode {
        /// An observation in state 2, 3 or 4 that did not replace the
        /// stored value (spec §8.7).
        ObservationNotApplied = 1,
        /// The session stopped: not enough space (state 6).
        InsufficientSpace = 2,
        /// The session stopped: the source changed (state 6).
        SourceChanged = 3,
        /// The session stopped: a file was too large for the volume
        /// (state 6).
        VolumeFileLimit = 4,
        /// The session failed on an archive constraint: a bug (state 3).
        ConstraintFailed = 5,
        /// The session failed on an archive I/O or other SQLite error
        /// (state 3).
        ArchiveError = 6,
        /// The session failed reading the source (state 3).
        SourceReadFailed = 7,
    }
}

closed_code! {
    /// `message.item_kind`: the kind `oma-messages` derives from
    /// `item_type` and `group_action_type` (M3 §11.4). `Unknown` keeps the
    /// raw pair in the row's own columns.
    ItemKindCode {
        Unknown = 0,
        Message = 1,
        ParticipantAdded = 2,
        OwnNumberChanged = 3,
        ParticipantRemoved = 4,
        GroupRenamed = 5,
        ParticipantLeft = 6,
        GroupPhotoChanged = 7,
        GroupPhotoRemoved = 8,
        BackgroundChanged = 9,
        BackgroundRemoved = 10,
        LegacySharedLocation = 11,
        AudioKept = 12,
        SharePlay = 13,
    }
}

closed_code! {
    /// `message.assoc_kind`: the mapped `associated_message_type` (M3
    /// §11.1). The raw integer is in `assoc_type`.
    AssocKindCode {
        NoAssociation = 0,
        StickerLegacy = 1,
        PollVote = 2,
        /// A raw value with no documented meaning.
        Unknown = 3,
        TapbackLove = 10,
        TapbackLike = 11,
        TapbackDislike = 12,
        TapbackLaugh = 13,
        TapbackEmphasize = 14,
        TapbackQuestion = 15,
        TapbackEmoji = 16,
        TapbackSticker = 17,
        RemovedLove = 20,
        RemovedLike = 21,
        RemovedDislike = 22,
        RemovedLaugh = 23,
        RemovedEmphasize = 24,
        RemovedQuestion = 25,
        RemovedEmoji = 26,
        RemovedSticker = 27,
    }
}

closed_code! {
    /// `message_body.display_none_reason`: why a body version has no
    /// `display_text` (D58).
    DisplayNoneReason {
        /// `attributedBody` holds a value (or one over its cap, or an
        /// unreadable one) that gave no text, and `text` gave none either.
        AttributedBodyUnusable = 1,
        /// `text` is the empty string and there is no `attributedBody`.
        TextEmpty = 2,
        /// Neither source holds a usable value: both absent or NULL, or
        /// `text` over its cap or unreadable with no `attributedBody`.
        NoSource = 3,
    }
}

closed_code! {
    /// `message_body.edit_label`: the `oma-messages` `EditLabel` (M3
    /// §11.3). `edit_label_a` and `edit_label_b` hold its numbers.
    EditLabelCode {
        /// A label this build does not know (a newer reader).
        Undeterminable = 0,
        NotRecorded = 1,
        SummaryUnreadable = 2,
        /// `a` = unsent parts, `b` = edited parts.
        UnsentPartsAndEditHistory = 3,
        /// `a` = unsent parts.
        UnsentParts = 4,
        /// `a` = edited parts, `b` = the most entries of one part.
        EditHistory = 5,
        DateEditedNoBody = 6,
        DateEditedOnly = 7,
        NoEditRecorded = 8,
        Undetermined = 9,
    }
}

closed_code! {
    /// `message_body.attributed_error` and `run_issue`: the typedstream
    /// decoder's `TsErrorKind` (fatal kinds 1–11, run issues 12–15).
    TsErrorCode {
        UnsupportedHeader = 1,
        Truncated = 2,
        UnexpectedTag = 3,
        BadReference = 4,
        UnsupportedType = 5,
        TooDeep = 6,
        Budget = 7,
        MaterialisationBudget = 8,
        TrailingBytes = 9,
        NotAttributedString = 10,
        InvalidUtf8 = 11,
        RunLengthMismatch = 12,
        RunOverflow = 13,
        RunSplitsSurrogate = 14,
        BadRunIndex = 15,
        /// A kind this build does not know.
        Unrecognised = 31,
    }
}

closed_code! {
    /// `message_edit_entry.at_kind`: how an edit entry's `d` was stored.
    SummaryTimeKind {
        Integer = 1,
        Real = 2,
        /// A plist date (Cocoa seconds by definition); `at_raw` is NULL.
        Date = 3,
    }
}

closed_code! {
    /// `attachment_observation.path_class`: the `oma-messages` `PathClass`
    /// of the observed `filename` (M3 §12, D50).
    PathClassCode {
        /// `filename` is NULL, absent, over its cap or unreadable: no class.
        NoClass = 0,
        SmsAttachments = 1,
        SmsStickerCache = 2,
        MacMessages = 3,
        OtherHomeRelative = 4,
        AbsoluteVarMobile = 5,
        AbsolutePrivateVarMobile = 6,
        Unrecognized = 7,
        /// A class this build does not know (a newer reader).
        Other = 15,
    }
}

closed_code! {
    /// `attachment_observation.lookup`: what the Manifest lookup and the
    /// blob probe found (the `oma-messages` `LookupOutcome` and the
    /// `oma-backup` `BlobProbe`; spec §9.1).
    LookupCode {
        /// No lookup was made: `copy_attachments` is off.
        NotLookedUp = 0,
        /// Not attempted: `filename` is NULL.
        NotAttemptedNull = 1,
        /// Not attempted: `filename` is over its cap.
        NotAttemptedTooLong = 2,
        /// Not attempted: the candidate path is unusual.
        NotAttemptedUnusualName = 3,
        /// Not attempted: the class is not looked up (D50).
        NotAttemptedClass = 4,
        /// Not attempted: no recognised prefix.
        NotAttemptedUnrecognized = 5,
        /// Not attempted: the `filename` column is absent.
        NotAttemptedColumnAbsent = 6,
        /// Not attempted: `filename` is unreadable.
        NotAttemptedUnreadable = 7,
        /// Not attempted, for a reason this build does not know.
        NotAttemptedOther = 8,
        /// Listed; the blob is a regular file whose length matches.
        Present = 20,
        /// Listed; the blob is not in the backup.
        BlobMissing = 21,
        /// Listed; the blob (or its directory) is not a regular file.
        BlobNotRegular = 22,
        /// Listed; the blob's metadata could not be read.
        BlobUnreadable = 23,
        /// Listed, but as a directory, link or other non-file.
        NotAFile = 24,
        /// Listed; the entry's fileID does not match its domain and path.
        EntryInvalid = 25,
        /// Encrypted: a file with content has no key.
        NoKey = 26,
        /// Encrypted: the file has no protection class.
        NoProtectionClass = 27,
        /// Encrypted: the class key is unavailable.
        ClassKeyUnavailable = 28,
        /// Encrypted: the file key did not unwrap.
        KeyDoesNotUnwrap = 29,
        /// Listed; the blob's length differs from the Manifest `Size`. Its
        /// bytes are still copied (spec §9.2).
        SizeMismatch = 30,
        /// Listed, not probed.
        NotProbed = 31,
        /// Listed; a probe result this build does not know.
        ProbeOther = 32,
        /// `SmsAttachments`: not listed at the sourced key.
        NotListedAtSourcedKey = 40,
        /// `SmsStickerCache`: not listed at the assumed key (absence not
        /// claimed).
        NotFoundAtAssumedKey = 41,
        /// More than one Manifest row has the fileID.
        Ambiguous = 42,
        /// The Manifest row does not match its lookup.
        InvalidEntry = 43,
        /// A lookup outcome this build does not know.
        LookupOther = 44,
    }
}

closed_code! {
    /// `attachment_observation.copy`: what P6 did with the file (spec §9.1).
    CopyCode {
        /// Copied and placed as a new store file.
        Copied = 1,
        /// Copied; the store already held the file (referenced, or placed
        /// earlier in the batch), so the copy was dropped.
        Deduplicated = 2,
        /// Copied; an unreferenced store file with the same name re-hashed
        /// equal and was adopted.
        Adopted = 3,
        /// Copied; an unreferenced store file with the same name did not
        /// match and was replaced.
        OrphanReplaced = 4,
        /// Not read: the current blob's file is present with the length the
        /// backup's file has (spec §9.2: presence and length, not bytes).
        AlreadyArchived = 5,
        /// Not read: `copy_attachments` is off.
        NotRequested = 6,
        /// Not read: no lookup was attempted.
        NotLookedUp = 7,
        /// Not read: the file is not listed in this backup.
        NotInBackup = 8,
        /// Not read: the file is listed but its blob or key is unusable.
        BlobUnavailable = 9,
        /// Copied, but the store file in the way is not usable (wrong
        /// length, not regular, or a directory there is not private and
        /// real). It is left untouched; the verifier reports it.
        StoreConflict = 10,
        /// Not read: over `Limits::attachment_bytes`.
        OverLimit = 11,
        /// The copy failed: the file is too large for the archive volume.
        TooLargeForVolume = 12,
        /// The copy failed reading or decrypting the backup's file.
        ReadFailed = 13,
        /// The copy failed writing or placing the store file.
        WriteFailed = 14,
        /// Copied and placed again: the current blob's store file was
        /// missing or unusable on disk.
        Replaced = 15,
    }
}

/// The bit of each `oma-messages` `AttrFlag` in `message_body_part.flags`:
/// its position in this list. Pinned by name; never reordered.
pub const ATTR_FLAG_BITS: [&str; 7] = [
    "bold",
    "italic",
    "underline",
    "strikethrough",
    "one_time_code",
    "inline_emoji",
    "audio_transcription",
];

/// The bit of each known attribute key in
/// `message_body_part.keys_present`: its position in this list. Pinned by
/// name; never reordered.
pub const ATTR_KEY_BITS: [&str; 19] = [
    "__kIMMessagePartAttributeName",
    "__kIMFileTransferGUIDAttributeName",
    "__kIMFilenameAttributeName",
    "__kIMInlineMediaWidthAttributeName",
    "__kIMInlineMediaHeightAttributeName",
    "IMAudioTranscription",
    "__kIMMentionConfirmedMention",
    "__kIMLinkAttributeName",
    "__kIMEmojiImageAttributeName",
    "__kIMTextEffectAttributeName",
    "__kIMTextBoldAttributeName",
    "__kIMTextItalicAttributeName",
    "__kIMTextUnderlineAttributeName",
    "__kIMTextStrikethroughAttributeName",
    "__kIMOneTimeCodeAttributeName",
    "__kIMCalendarEventAttributeName",
    "__kIMDataDetectedAttributeName",
    "__kIMMoneyAttributeName",
    "__kIMAddressAttributeName",
];

closed_code! {
    /// Bits of `import_session.qualifiers`: the closed qualifier kinds a
    /// session carries. Each value is a bit position.
    QualifierBit {
        /// A plist backup date was outside 2007-01-01 .. import start + 48 h
        /// and was ignored for the rank (spec §8.7).
        BackupDateOutOfRange = 0,
        /// A listed `-wal` could not be staged.
        WalSkipped = 1,
        /// No `-wal` is listed but the header says WAL mode.
        WalStateUnknown = 2,
        /// The staged `-wal` has frames that failed validation.
        WalFramesInvalid = 3,
        /// sms.db is not UTF-8; SQLite converted its text.
        NonUtf8Encoding = 4,
        /// The staged size differs from Manifest's `Size`.
        SizeMismatch = 5,
        /// The iOS version is not one the app was tested with (D79).
        IosVersionUntested = 6,
        /// Ordinary tables the reader does not know.
        UnknownTables = 7,
        /// Columns of known tables the reader does not know.
        UnknownColumns = 8,
        /// Known columns whose name differs only in case.
        WrongCaseColumns = 9,
    }
}

impl QualifierBit {
    /// The bit in `import_session.qualifiers`.
    pub const fn mask(self) -> i64 {
        1i64 << self.code()
    }

    /// The qualifiers that make a session not a full observation (spec
    /// §8.7): the WAL-related ones.
    pub const WAL_RELATED: [QualifierBit; 3] = [
        QualifierBit::WalSkipped,
        QualifierBit::WalStateUnknown,
        QualifierBit::WalFramesInvalid,
    ];
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

    /// The codes added with the import core (slice 4f).
    #[test]
    fn import_codes_are_pinned() {
        let got: [(&str, Vec<i64>); 7] = [
            ("Pass", Pass::ALL.iter().map(|v| v.code()).collect()),
            (
                "WalOutcomeCode",
                WalOutcomeCode::ALL.iter().map(|v| v.code()).collect(),
            ),
            (
                "TextEncodingCode",
                TextEncodingCode::ALL.iter().map(|v| v.code()).collect(),
            ),
            (
                "ServiceClassCode",
                ServiceClassCode::ALL.iter().map(|v| v.code()).collect(),
            ),
            (
                "UnreadableKind",
                UnreadableKind::ALL.iter().map(|v| v.code()).collect(),
            ),
            (
                "IssueCode",
                IssueCode::ALL.iter().map(|v| v.code()).collect(),
            ),
            (
                "QualifierBit",
                QualifierBit::ALL.iter().map(|v| v.code()).collect(),
            ),
        ];
        let want: [(&str, &[i64]); 7] = [
            ("Pass", &[1, 2, 3, 4, 5, 6, 7, 8, 9]),
            ("WalOutcomeCode", &[0, 1, 2]),
            ("TextEncodingCode", &[1, 2, 3]),
            ("ServiceClassCode", &[1, 2, 3, 4, 5, 6]),
            ("UnreadableKind", &[16, 17, 18, 19, 20, 31]),
            ("IssueCode", &[1, 2, 3, 4, 5, 6, 7]),
            ("QualifierBit", &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]),
        ];
        for ((name, got), (wname, want)) in got.iter().zip(want.iter()) {
            assert_eq!(name, wname);
            assert_eq!(got.as_slice(), *want, "{name}");
        }
        assert_eq!(UnreadableKind::TypeMismatch.code(), ISSUE_KIND_BASE);
        assert_eq!(QualifierBit::WalFramesInvalid.mask(), 8);
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

    /// The codes added with the message pass (slice 4g).
    #[test]
    fn message_codes_are_pinned() {
        let got: [(&str, Vec<i64>); 6] = [
            (
                "ItemKindCode",
                ItemKindCode::ALL.iter().map(|v| v.code()).collect(),
            ),
            (
                "AssocKindCode",
                AssocKindCode::ALL.iter().map(|v| v.code()).collect(),
            ),
            (
                "DisplayNoneReason",
                DisplayNoneReason::ALL.iter().map(|v| v.code()).collect(),
            ),
            (
                "EditLabelCode",
                EditLabelCode::ALL.iter().map(|v| v.code()).collect(),
            ),
            (
                "TsErrorCode",
                TsErrorCode::ALL.iter().map(|v| v.code()).collect(),
            ),
            (
                "SummaryTimeKind",
                SummaryTimeKind::ALL.iter().map(|v| v.code()).collect(),
            ),
        ];
        let want: [(&str, &[i64]); 6] = [
            (
                "ItemKindCode",
                &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13],
            ),
            (
                "AssocKindCode",
                &[
                    0, 1, 2, 3, 10, 11, 12, 13, 14, 15, 16, 17, 20, 21, 22, 23, 24, 25, 26, 27,
                ],
            ),
            ("DisplayNoneReason", &[1, 2, 3]),
            ("EditLabelCode", &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]),
            (
                "TsErrorCode",
                &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 31],
            ),
            ("SummaryTimeKind", &[1, 2, 3]),
        ];
        for ((name, got), (wname, want)) in got.iter().zip(want.iter()) {
            assert_eq!(name, wname);
            assert_eq!(got.as_slice(), *want, "{name}");
        }
        // The bit tables follow the reader's own order today; the archive
        // pins its own copy by name.
        let flags: Vec<&str> = oma_messages::AttrFlag::ALL
            .iter()
            .map(|f| f.name())
            .collect();
        assert_eq!(flags, ATTR_FLAG_BITS);
        let keys: Vec<&str> = oma_messages::AttrKey::ALL
            .iter()
            .map(|k| k.name())
            .collect();
        assert_eq!(keys, ATTR_KEY_BITS);
        // Every decoder error kind has its own code.
        let names: Vec<&str> = oma_messages::TsErrorKind::ALL
            .iter()
            .map(|k| k.name())
            .collect();
        assert_eq!(names.len(), 15);
    }

    /// The codes added with the attachment store (slice 4i).
    #[test]
    fn store_codes_are_pinned() {
        let got: [(&str, Vec<i64>); 3] = [
            (
                "PathClassCode",
                PathClassCode::ALL.iter().map(|v| v.code()).collect(),
            ),
            (
                "LookupCode",
                LookupCode::ALL.iter().map(|v| v.code()).collect(),
            ),
            ("CopyCode", CopyCode::ALL.iter().map(|v| v.code()).collect()),
        ];
        let want: [(&str, &[i64]); 3] = [
            ("PathClassCode", &[0, 1, 2, 3, 4, 5, 6, 7, 15]),
            (
                "LookupCode",
                &[
                    0, 1, 2, 3, 4, 5, 6, 7, 8, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32,
                    40, 41, 42, 43, 44,
                ],
            ),
            (
                "CopyCode",
                &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
            ),
        ];
        for ((name, got), (wname, want)) in got.iter().zip(want.iter()) {
            assert_eq!(name, wname);
            assert_eq!(got.as_slice(), *want, "{name}");
        }
        // Every reader path class has its own code.
        assert_eq!(oma_messages::PathClass::ALL.len(), 7);
    }

    /// The codes added with the verifier (slice 4k).
    #[test]
    fn verify_codes_are_pinned() {
        let got: Vec<i64> = BlobVerifyState::ALL.iter().map(|v| v.code()).collect();
        assert_eq!(got, [0, 1, 2, 3, 4, 5, 6]);
        assert_eq!(BlobVerifyState::from_code(7), None);
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
