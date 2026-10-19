//! Words for the human-readable formats (M6 spec §4.5, D129).
//!
//! - Every closed code the record carries has a sentence here, in an
//!   exhaustive `match` per table: a new code fails to compile until it
//!   has words, as the TypeScript `Record<…>` tables do for the viewer.
//! - **Third-party wording.** An export is read by someone other than the
//!   phone's owner, so no sentence says "You": the owner's messages are
//!   "Sent from the backed-up phone's account".
//! - **Parity.** [`words_json`] is written to
//!   `src/types/generated/export-words.json` by `tests/words.rs` (the
//!   generated-files drift check keeps it committed), and the Vitest
//!   `tests/frontend/export-words.spec.ts` compares every entry that names
//!   a `viewer` key with that string in `viewerText.ts`. Export-only
//!   sentences carry `"viewer": null` and are not compared.
//! - **JSON stays language-neutral:** records carry codes; these
//!   sentences appear only in the human-readable formats, `README.txt`
//!   and the schema's descriptions.

use oma_archive::schema::codes::{
    AssocKindCode, AssocTargetShape, AttributedOutcome, BlobVerifyState, CopyCode,
    DisplayNoneReason, DisplaySource, EditLabelCode, ItemKindCode, LookupCode, PathClassCode,
    ServiceClassCode, SummaryTimeKind, TimeUnit, TsErrorCode, ValueState,
};
use serde::Serialize;

use crate::codes::{
    AltTextSourceCode, AltTextWithheldCode, AssocVisibilityCode, AttachmentAccessCode,
    AttachmentOutcome, CardStateCode, ChipSenderCode, ChipStateCode, Direction, EditEntryTextCode,
    EditTextsOutcomeCode, ExclusionReason, ExportCode, FormattingCode, HistoryColumnCode,
    MaskKindCode, MessageLabelCode, NoTextReason, ReactionKindCode, ReplyResolution,
    SegmentKindCode, SenderReasonCode, Style, WithheldReasonCode,
};

/// One entry of a words table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WordsEntry {
    pub token: &'static str,
    pub code: i64,
    pub sentence: &'static str,
    /// `TABLE.key` in `viewerText.ts` holding the same sentence, or
    /// `null` for an export-only sentence.
    pub viewer: Option<String>,
}

/// One words table over a registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WordsTable {
    pub name: &'static str,
    pub registry: &'static str,
    pub entries: Vec<WordsEntry>,
}

/// A fixed phrase of the export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Phrase {
    pub key: &'static str,
    pub sentence: &'static str,
    pub viewer: Option<&'static str>,
}

/// Declares one words table as a module with `sentence(v)` and `table()`.
/// `Variant => "…"` compares with `<viewer table>.<token>`; `@ "X.y"`
/// names another viewer key; `@ ""` marks an export-only sentence.
macro_rules! words {
    (
        $(#[$meta:meta])*
        $module:ident, $name:literal, $ty:ident, viewer = $viewer:literal {
            $($v:ident => $s:literal $(@ $path:literal)?),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        pub mod $module {
            #[allow(unused_imports)]
            use super::*;

            /// The sentence for `v`.
            pub fn sentence(v: $ty) -> &'static str {
                match v {
                    $($ty::$v => $s),+
                }
            }

            pub(crate) fn table() -> WordsTable {
                let entries = vec![$(entry($ty::$v, $s, $viewer, &[$($path)?])),+];
                WordsTable {
                    name: $name,
                    registry: <$ty as ExportCode>::registry().name,
                    entries,
                }
            }
        }
    };
}

fn entry<T: ExportCode>(
    v: T,
    sentence: &'static str,
    viewer: &'static str,
    path: &[&'static str],
) -> WordsEntry {
    let viewer = match path.first().copied() {
        Some("") => None,
        Some(p) => Some(p.to_owned()),
        None if viewer.is_empty() => None,
        None => Some(format!("{viewer}.{}", v.token())),
    };
    WordsEntry {
        token: v.token(),
        code: v.export_code(),
        sentence,
        viewer,
    }
}

words! {
    /// Why a content value is missing.
    value_state, "valueState", ValueState, viewer = "VALUE_STATE_TEXT" {
        Null => "Not recorded (NULL)",
        Absent => "Column not in this backup",
        Oversized => "Too large to store",
        Unreadable => "Could not be read",
    }
}

words! {
    /// Why a date is missing.
    time_state, "timeState", ValueState, viewer = "TIME_STATE_TEXT" {
        Null => "No date recorded (NULL)",
        Absent => "Date column not in this backup",
        Oversized => "Date too large to store",
        Unreadable => "Date could not be read",
    }
}

words! {
    /// A date's derived unit. `{raw}` is replaced with the raw value.
    time_unit, "timeUnit", TimeUnit, viewer = "TIME_UNIT_TEXT" {
        Zero => "Date recorded as 0",
        Negative => "Negative date (raw {raw})",
        Seconds => "Recorded in seconds",
        Nanoseconds => "Recorded in nanoseconds",
        Ambiguous => "Date could not be interpreted (raw {raw})",
    }
}

words! {
    summary_time_kind, "summaryTimeKind", SummaryTimeKind, viewer = "SUMMARY_TIME_KIND_TEXT" {
        Integer => "time as recorded (whole number; unit assumed)",
        Real => "time as recorded (decimal number; unit assumed)",
        Date => "time as recorded (date value; unit assumed)",
    }
}

words! {
    item_kind, "itemKind", ItemKindCode, viewer = "ITEM_KIND_TEXT" {
        Unknown => "Unrecognised item",
        Message => "Message",
        ParticipantAdded => "An address was added to the conversation",
        OwnNumberChanged => "The phone’s own number changed in the conversation",
        ParticipantRemoved => "An address was removed from the conversation",
        GroupRenamed => "The conversation name was changed",
        ParticipantLeft => "An address left the conversation",
        GroupPhotoChanged => "The conversation photo was changed",
        GroupPhotoRemoved => "The conversation photo was removed",
        BackgroundChanged => "The conversation background was changed",
        BackgroundRemoved => "The conversation background was removed",
        LegacySharedLocation => "Location sharing (older format, not decoded)",
        AudioKept => "An audio message was kept",
        SharePlay => "SharePlay activity (not decoded)",
    }
}

words! {
    /// Direction, worded for a third-party reader.
    direction, "direction", Direction, viewer = "" {
        FromMe => "Sent from the backed-up phone's account" @ "DIRECTION_DETAIL_TEXT.you",
        Received => "Received" @ "DIRECTION_TEXT.received",
        NotRecorded => "Direction not recorded" @ "DIRECTION_TEXT.notRecorded",
        UnknownValue => "Direction recorded with a value this app does not know (raw {raw})",
    }
}

words! {
    sender_reason, "senderReason", SenderReasonCode, viewer = "SENDER_TEXT" {
        FromMe => "The backed-up phone's account" @ "",
        Resolved => "Sender",
        NoSender => "No sender recorded",
        ColumnAbsent => "Sender column not in this backup",
        Unreadable => "Sender could not be read",
        NotFound => "Sender recorded but not found in this archive",
        DirectionNotRecorded => "Sender not shown: direction not recorded",
    }
}

words! {
    service, "serviceClass", ServiceClassCode, viewer = "SERVICE_TEXT" {
        IMessage => "iMessage",
        Sms => "SMS",
        Rcs => "RCS",
        IMessageLite => "iMessageLite",
        Empty => "No service recorded",
        Other => "Other service",
    }
}

words! {
    /// `{value}` is replaced with the recorded `schedule_type`.
    message_label, "messageLabel", MessageLabelCode, viewer = "MESSAGE_LABEL_TEXT" {
        Undated => "Undated (derived): no usable date is recorded",
        Before2007 => "Before 2007, outside the range this app expects; it may not be a real date"
            @ "BEFORE_2007_TEXT",
        Unkeyed => "No unique identifier in its source; it may appear more than once",
        ScheduleType => "schedule_type {value} recorded. For scheduled messages, the time shown is the planned send time",
        AppContent => "App or link content (not decoded)",
        Effect => "Sent with an effect",
        NotInLatest => "Not seen by the newest complete import of this backup set (derived)",
        Provisional => "Provisional (untested iOS version)",
    }
}

words! {
    display_none_reason, "displayNoneReason", DisplayNoneReason, viewer = "DISPLAY_NONE_TEXT" {
        AttributedBodyUnusable => "The formatted text could not be used",
        TextEmpty => "The message text is empty",
        NoSource => "No text is recorded for this message",
    }
}

words! {
    no_text_reason, "noTextReason", NoTextReason, viewer = "" {
        NoBodyRow => "No text recorded" @ "PREVIEW_BODY_TEXT.noBody",
        AttributedBodyUnusable => "The formatted text could not be used"
            @ "DISPLAY_NONE_TEXT.attributedBodyUnusable",
        TextEmpty => "The message text is empty" @ "DISPLAY_NONE_TEXT.textEmpty",
        NoSource => "No text is recorded for this message" @ "DISPLAY_NONE_TEXT.noSource",
        ReasonNotRecorded => "No text; the reason is not recorded",
        ReasonUnrecognised => "No text; the reason is recorded with a code this app does not know",
    }
}

words! {
    attributed_outcome, "attributedOutcome", AttributedOutcome, viewer = "ATTRIBUTED_OUTCOME_TEXT" {
        Absent => "The formatted text column is not in this backup",
        Null => "No formatted text recorded",
        Oversized => "The formatted text was larger than the import limit and was not stored",
        Unreadable => "The formatted text could not be read",
        NotDecoded => "The formatted text was not decoded",
        Decoded => "The formatted text was decoded",
        DecodeFailed => "The formatted text could not be decoded",
    }
}

words! {
    display_source, "displaySource", DisplaySource, viewer = "DISPLAY_SOURCE_TEXT" {
        AttributedBody => "From the formatted text",
        Text => "From the plain-text column",
        TextLossy => "From the plain-text column; some of its bytes were not valid text",
    }
}

words! {
    formatting, "formatting", FormattingCode, viewer = "FORMATTING_TEXT" {
        Plain => "No formatting recorded",
        Applied => "Formatting applied",
        NotApplied => "Formatting not applied",
        Unreadable => "Formatting could not be read",
    }
}

words! {
    ts_error, "tsError", TsErrorCode, viewer = "TS_ERROR_TEXT" {
        UnsupportedHeader => "The formatted text uses a header this app does not read",
        Truncated => "The formatted text ends early",
        UnexpectedTag => "The formatted text contains an unexpected value",
        BadReference => "The formatted text refers to a value that is not there",
        UnsupportedType => "The formatted text uses a type this app does not read",
        TooDeep => "The formatted text is nested too deeply",
        Budget => "The formatted text is too large to decode",
        MaterialisationBudget => "The formatted text expands too much to decode",
        TrailingBytes => "The formatted text has unexpected data after its end",
        NotAttributedString => "The formatted text is not in the expected form",
        InvalidUtf8 => "The formatted text contains invalid characters",
        RunLengthMismatch => "The formatting ranges do not match the text",
        RunOverflow => "The formatting ranges run past the text",
        RunSplitsSurrogate => "A formatting range splits a character",
        BadRunIndex => "A formatting range refers to a missing entry",
        Unrecognised => "The formatted text could not be decoded",
    }
}

words! {
    mask_kind, "maskKind", MaskKindCode, viewer = "MASK_TEXT" {
        OneTimeCode => "One-time code hidden",
    }
}

words! {
    segment_kind, "segmentKind", SegmentKindCode, viewer = "SEGMENT_TEXT" {
        Text => "Text" @ "",
        Withheld => "One-time code hidden" @ "MASK_TEXT.oneTimeCode",
        InlineEmoji => "Inline emoji image (not shown)",
        Attachment => "Attachment" @ "",
        UnmatchedPlaceholder => "Attachment placeholder not matched to a file",
    }
}

words! {
    style, "style", Style, viewer = "" {
        Bold => "bold",
        Italic => "italic",
        Underline => "underline",
        Strikethrough => "strikethrough",
    }
}

words! {
    /// Codes 0, 2, 9 never read "not edited"; codes 6, 7 never read
    /// "unsent".
    edit_label, "editLabel", EditLabelCode, viewer = "EDIT_LABEL_TEXT" {
        Undeterminable => "Edit state could not be determined",
        NotRecorded => "No edit information recorded",
        SummaryUnreadable => "Edit state could not be determined",
        UnsentPartsAndEditHistory => "Edited, and an unsend is recorded (derived)",
        UnsentParts => "An unsend is recorded (derived); see the parts listed",
        EditHistory => "Edited (derived)",
        DateEditedNoBody => "An edit date is recorded; no edit history in this version",
        DateEditedOnly => "An edit date is recorded; no edit history in this version",
        NoEditRecorded => "No edit recorded",
        Undetermined => "Edit state could not be determined",
    }
}

words! {
    edit_texts_outcome, "editTextsOutcome", EditTextsOutcomeCode, viewer = "EDIT_TEXTS_OUTCOME_TEXT" {
        NotNeeded => "No recorded edit text",
        Decoded => "Edit text decoded from the stored record",
        Unmatched => "Edit text could not be matched",
        SummaryUnreadable => "The edit record could not be read",
    }
}

words! {
    edit_entry_text, "editEntryText", EditEntryTextCode, viewer = "EDIT_ENTRY_TEXT" {
        Text => "Text recorded for this version",
        Withheld => "Earlier text withheld",
        NoBody => "No text recorded for this version",
        Failed => "Edit text could not be decoded",
        NotMatched => "Edit text could not be matched",
    }
}

words! {
    /// Why earlier content is not in the export (it is never shown on a
    /// click here: the export's opt-in decides).
    withheld_reason, "withheldReason", WithheldReasonCode, viewer = "" {
        Unsent => "Earlier text was kept from an import before it was unsent; not included in this export",
        EarlierVersion => "Earlier text was kept from an import; not included in this export",
        NoCurrentText => "Earlier text was kept from an import; not included in this export",
    }
}

words! {
    /// The sentence beside revealed earlier content.
    revealed_reason, "revealedReason", WithheldReasonCode, viewer = "REVEALED_TEXT" {
        Unsent => "This text is from an import taken before it was unsent; the phone no longer shows it",
        EarlierVersion => "This text is from an earlier version kept by an import",
        NoCurrentText => "This text is from an earlier version kept by an import",
    }
}

words! {
    assoc_kind, "assocKind", AssocKindCode, viewer = "ASSOC_KIND_TEXT" {
        NoAssociation => "Not a reaction",
        StickerLegacy => "Sticker placed on a message",
        PollVote => "Poll vote (poll contents not decoded)",
        Unknown => "Unrecognised association",
        TapbackLove => "Loved",
        TapbackLike => "Liked",
        TapbackDislike => "Disliked",
        TapbackLaugh => "Laughed at",
        TapbackEmphasize => "Emphasised",
        TapbackQuestion => "Questioned",
        TapbackEmoji => "Reacted with an emoji",
        TapbackSticker => "Sticker reaction",
        RemovedLove => "Removed a heart",
        RemovedLike => "Removed a like",
        RemovedDislike => "Removed a dislike",
        RemovedLaugh => "Removed a laugh",
        RemovedEmphasize => "Removed an emphasis",
        RemovedQuestion => "Removed a question",
        RemovedEmoji => "Removed an emoji reaction",
        RemovedSticker => "Removed a sticker reaction",
    }
}

words! {
    assoc_shape, "assocTargetShape", AssocTargetShape, viewer = "ASSOC_SHAPE_TEXT" {
        Part => "Target recorded with a part number",
        Bubble => "Target recorded as a whole bubble",
        Bare => "Target recorded in a form this app has not verified",
        Unparsed => "Target could not be parsed",
    }
}

words! {
    assoc_visibility, "assocVisibility", AssocVisibilityCode, viewer = "ASSOC_VISIBILITY_TEXT" {
        Undeterminable => "Whether this folds into its target could not be determined",
        NoKind => "Reaction kind not recorded",
        NotAReaction => "Not a reaction",
        TargetUnreadable => "Reaction target could not be read",
        TargetNotUnique => "Reaction target is not uniquely identified",
        TargetInAnotherChat => "Reaction to a message in another conversation",
        Folded => "Shown on the message it reacts to",
        TargetFolds => "Reaction to another reaction, which is not shown as an item of its own",
    }
}

words! {
    reaction_kind, "reactionKind", ReactionKindCode, viewer = "REACTION_KIND_TEXT" {
        Love => "Loved",
        Like => "Liked",
        Dislike => "Disliked",
        Laugh => "Laughed",
        Emphasize => "Emphasised",
        Question => "Questioned",
        Emoji => "Emoji reaction",
        Sticker => "Sticker reaction",
    }
}

words! {
    chip_state, "chipState", ChipStateCode, viewer = "CHIP_STATE_TEXT" {
        Shown => "recorded (derived)",
        Hidden => "removed later (derived)",
        Undeterminable => "cannot be determined",
        Added => "recorded add",
        Removed => "recorded removal",
    }
}

words! {
    chip_sender, "chipSender", ChipSenderCode, viewer = "CHIP_SENDER_TEXT" {
        Me => "the backed-up phone's account" @ "",
        Handle => "an address",
        NotRecorded => "sender not recorded",
    }
}

words! {
    reply_resolution, "replyResolution", ReplyResolution, viewer = "REPLY_TEXT" {
        Resolved => "Reply to",
        InAnotherConversation => "Reply to a message in another conversation",
        NotUniquelyIdentified => "No uniquely identified message with this identifier is in the archive",
    }
}

words! {
    history_column, "historyColumn", HistoryColumnCode, viewer = "HISTORY_COLUMN_TEXT" {
        Subject => "subject",
        GroupTitle => "group_title",
        DestinationCallerId => "destination_caller_id (Account, meaning unverified)",
        ChatDisplayName => "display_name",
        TransferName => "transfer_name",
        EmojiDescription => "emoji_description",
    }
}

words! {
    /// `{lookup}` and `{copy}` are replaced with the raw codes.
    card_state, "cardState", CardStateCode, viewer = "CARD_TEXT" {
        File => "File",
        NoObservation => "No copy of this file was attempted by any import",
        NotInBackup => "This file was not in the backup",
        NotFoundAtAssumedKey => "Not found where this app looked; it may be stored elsewhere in the backup",
        NotCopied => "Not copied: attachment copy was off for that import",
        ClassNotLookedUp => "This kind of file location is not looked up by this version",
        LocationUnusable => "The file's recorded location could not be used",
        NotDecrypted => "Listed in the backup but could not be decrypted",
        CouldNotBeRead => "Listed in the backup but could not be read",
        TooLarge => "Too large to copy",
        Unavailable => "Not available (lookup code {lookup}, copy code {copy})",
    }
}

words! {
    blob_verify, "blobVerifyState", BlobVerifyState, viewer = "BLOB_VERIFY_TEXT" {
        Ok => "The full verify found no problem",
        Missing => "The full verify reported: file missing",
        NotRegular => "The full verify reported: not a regular file",
        LengthMismatch => "The full verify reported: wrong size",
        HashMismatch => "The full verify reported: checksum mismatch",
        Unreadable => "The full verify reported: could not be read",
        BadName => "The full verify reported: unexpected name",
    }
}

words! {
    lookup, "lookup", LookupCode, viewer = "LOOKUP_TEXT" {
        NotLookedUp => "Not looked up",
        NotAttemptedNull => "Not looked up: no location recorded",
        NotAttemptedTooLong => "Not looked up: the recorded location is too long",
        NotAttemptedUnusualName => "Not looked up: the recorded location has an unusual name",
        NotAttemptedClass => "Not looked up: this kind of location is not looked up by this version",
        NotAttemptedUnrecognized => "Not looked up: the recorded location is not recognised",
        NotAttemptedColumnAbsent => "Not looked up: the location column is not in this backup",
        NotAttemptedUnreadable => "Not looked up: the recorded location could not be read",
        NotAttemptedOther => "Not looked up (other reason)",
        Present => "Found in the backup",
        BlobMissing => "Listed, but the file is missing from the backup",
        BlobNotRegular => "Listed, but not a regular file in the backup",
        BlobUnreadable => "Listed, but could not be read",
        NotAFile => "Listed, but not a file",
        EntryInvalid => "Listed, but the entry is invalid",
        NoKey => "Listed, but no decryption key is recorded",
        NoProtectionClass => "Listed, but no protection class is recorded",
        ClassKeyUnavailable => "Listed, but the protection class key is unavailable",
        KeyDoesNotUnwrap => "Listed, but the key could not be unwrapped",
        SizeMismatch => "Listed, but the size does not match",
        NotProbed => "Not probed",
        ProbeOther => "Probe failed (other reason)",
        NotListedAtSourcedKey => "Not in the backup",
        NotFoundAtAssumedKey => "Not found where this app looked",
        Ambiguous => "More than one matching entry",
        InvalidEntry => "Invalid entry",
        LookupOther => "Lookup failed (other reason)",
    }
}

words! {
    copy, "copy", CopyCode, viewer = "COPY_TEXT" {
        Copied => "Copied",
        Deduplicated => "Same file already stored",
        Adopted => "An existing stored file was used",
        OrphanReplaced => "An unused stored file was replaced",
        AlreadyArchived => "Already in the archive",
        NotRequested => "Not copied: attachment copy was off",
        NotLookedUp => "Not looked up",
        NotInBackup => "Not in the backup",
        BlobUnavailable => "Could not be read from the backup",
        StoreConflict => "Could not be stored (conflict)",
        OverLimit => "Too large to copy",
        TooLargeForVolume => "Too large for the archive volume",
        ReadFailed => "Reading failed",
        WriteFailed => "Writing failed",
        Replaced => "Replaced by a later import",
    }
}

words! {
    path_class, "pathClass", PathClassCode, viewer = "PATH_CLASS_TEXT" {
        NoClass => "No location class",
        SmsAttachments => "Messages attachments folder",
        SmsStickerCache => "Sticker cache",
        MacMessages => "Mac Messages folder",
        OtherHomeRelative => "Another folder in the home directory",
        AbsoluteVarMobile => "An absolute phone path",
        AbsolutePrivateVarMobile => "An absolute private phone path",
        Unrecognized => "Unrecognised location",
        Other => "Other location",
    }
}

words! {
    attachment_access, "attachmentAccess", AttachmentAccessCode, viewer = "" {
        Included => "Included",
        Withheld => "Withheld: an attachment of a part that was unsent",
        Revealed => "Included because this export includes content captured before an unsend",
    }
}

words! {
    attachment_outcome, "attachmentOutcome", AttachmentOutcome, viewer = "" {
        Copied => "File copied into this export",
        Deduplicated => "Same file as another attachment in this export",
        NotRequested => "File not included: this export carries names only",
        WithheldUnsent => "File not included: an attachment of a part that was unsent",
        ChecksumMismatch => "File not included: the stored file no longer matches its checksum",
        TooLargeForVolume => "File not included: too large for the destination's file system",
        WriteFailed => "File not included: it could not be written",
        BlockedExtension => "File copied with \".blocked\" added to its name",
        NoStoredFile => "No file is stored for this attachment",
    }
}

words! {
    alt_text_source, "altTextSource", AltTextSourceCode, viewer = "" {
        TextColumn => "The plain-text column holds different text",
        DecodedAttributedBody => "The formatted text holds different text",
    }
}

words! {
    alt_text_withheld, "altTextWithheld", AltTextWithheldCode, viewer = "" {
        EarlierContent => "The other stored text is not included: it may hold text from before an unsend or edit",
        OneTimeCode => "The other stored text is not included: it may hold a one-time code",
    }
}

words! {
    exclusion_reason, "exclusionReason", ExclusionReason, viewer = "" {
        D94 => "Read, delivery and sent state are never exported",
        D115Raw => "Raw stored text that can hold one-time codes or text from before an unsend or edit is not exported",
        RawSourceBytes => "Raw source bytes are not exported",
        SourceIdentifierOptIn => "Source identifiers and archive ids are exported only when chosen",
        SessionLocalRowid => "Row numbers that only mean something inside one backup are not exported",
        StorageInternal => "Storage names, paths and digests of files not exported are not exported",
    }
}

/// Fixed phrases of the human-readable formats.
pub const PHRASES: &[Phrase] = &[
    Phrase {
        key: "notInExport",
        sentence: "not in this export",
        viewer: None,
    },
    Phrase {
        key: "derived",
        sentence: "derived",
        viewer: None,
    },
    Phrase {
        key: "chipsOutsideScope",
        sentence: "derived; includes reactions outside this export",
        viewer: None,
    },
    Phrase {
        key: "outsideZoneWindow",
        sentence: "[UTC: outside the zone table's range]",
        viewer: None,
    },
    Phrase {
        key: "plannedSendTime",
        sentence: "[planned send time]",
        viewer: None,
    },
    Phrase {
        key: "orderTie",
        sentence: "[same recorded time; order not known]",
        viewer: None,
    },
    Phrase {
        key: "phoneEditTime",
        sentence: "time as recorded (unit assumed)",
        viewer: None,
    },
    Phrase {
        key: "transcription",
        sentence: "Transcription recorded on the phone",
        viewer: Some("SEGMENT_TEXT.transcription"),
    },
    Phrase {
        key: "oneTimeCodeMarker",
        sentence: "one-time code hidden",
        viewer: None,
    },
    Phrase {
        key: "earlierTextMarker",
        sentence: "earlier text not included",
        viewer: None,
    },
    Phrase {
        key: "combiningMarks",
        sentence: "+{n} combining marks",
        viewer: None,
    },
    Phrase {
        key: "replyElsewhere",
        sentence: "item {n}, elsewhere in this conversation",
        viewer: None,
    },
    Phrase {
        key: "directionRecordedBy",
        sentence: "As recorded by backup set {lineage}; another phone’s backup may disagree",
        viewer: Some("BUBBLE_TEXT.directionRecordedBy"),
    },
    Phrase {
        key: "recentlyDeleted",
        sentence: "Listed in Recently Deleted in a backup this archive imported",
        viewer: Some("BUBBLE_TEXT.recentlyDeletedUnknown"),
    },
    Phrase {
        key: "account",
        sentence: "destination_caller_id (Account, meaning unverified)",
        viewer: Some("DETAILS_TEXT.account"),
    },
    Phrase {
        key: "sourcesDiffer",
        sentence: "The two stored text fields differ; which one the phone showed is not known",
        viewer: Some("SOURCES_DIFFER_TEXT"),
    },
    Phrase {
        key: "reactionRule",
        sentence: "Chips are derived from the recorded rows: the latest dated row per sender and kind wins, and a removal hides the chip. Undated or tied rows make the state \"cannot be determined\".",
        viewer: Some("REACTION_RULE_TEXT"),
    },
    Phrase {
        key: "undatedOrder",
        sentence: "Undated items follow the order of the earliest import that recorded them; that order is not a recorded fact.",
        viewer: None,
    },
];

/// Every words table, in a fixed order.
pub fn tables() -> Vec<WordsTable> {
    vec![
        value_state::table(),
        time_state::table(),
        time_unit::table(),
        summary_time_kind::table(),
        item_kind::table(),
        direction::table(),
        sender_reason::table(),
        service::table(),
        message_label::table(),
        display_none_reason::table(),
        no_text_reason::table(),
        attributed_outcome::table(),
        display_source::table(),
        formatting::table(),
        ts_error::table(),
        mask_kind::table(),
        segment_kind::table(),
        style::table(),
        edit_label::table(),
        edit_texts_outcome::table(),
        edit_entry_text::table(),
        withheld_reason::table(),
        revealed_reason::table(),
        assoc_kind::table(),
        assoc_shape::table(),
        assoc_visibility::table(),
        reaction_kind::table(),
        chip_state::table(),
        chip_sender::table(),
        reply_resolution::table(),
        history_column::table(),
        card_state::table(),
        blob_verify::table(),
        lookup::table(),
        copy::table(),
        path_class::table(),
        attachment_access::table(),
        attachment_outcome::table(),
        alt_text_source::table(),
        alt_text_withheld::table(),
        exclusion_reason::table(),
    ]
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WordsDoc {
    format: &'static str,
    format_version: u32,
    rule: &'static str,
    tables: Vec<WordsTable>,
    phrases: &'static [Phrase],
}

/// `export-words.json`: every table and phrase, pretty-printed with LF
/// line endings and a final newline.
pub fn words_json() -> String {
    let doc = WordsDoc {
        format: "oma-export-words",
        format_version: 1,
        rule: "Generated by crates/oma-export (tests/words.rs); do not edit. Each entry's sentence is what the human-readable export formats write for that code. An entry with a viewer key must equal that string in src/services/viewerText.ts (tests/frontend/export-words.spec.ts); viewer null marks an export-only sentence.",
        tables: tables(),
        phrases: PHRASES,
    };
    let mut s = serde_json::to_string_pretty(&doc).unwrap_or_default();
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codes::REGISTRIES;

    #[test]
    fn every_registry_code_has_words() {
        let tables = tables();
        for r in REGISTRIES {
            let t = tables
                .iter()
                .find(|t| t.registry == r.name)
                .unwrap_or_else(|| panic!("{} has no words", r.name));
            let mut codes: Vec<i64> = t.entries.iter().map(|e| e.code).collect();
            codes.sort_unstable();
            let mut want: Vec<i64> = r.entries.iter().map(|e| e.code).collect();
            want.sort_unstable();
            assert_eq!(codes, want, "{}", r.name);
        }
    }

    #[test]
    fn third_party_wording_never_says_you() {
        let says_you = |s: &str| {
            s.split(|c: char| !c.is_alphanumeric())
                .any(|w| w.eq_ignore_ascii_case("you") || w.eq_ignore_ascii_case("your"))
        };
        for t in tables() {
            for e in &t.entries {
                assert!(!says_you(e.sentence), "{}.{}", t.name, e.token);
            }
        }
        for p in PHRASES {
            assert!(!says_you(p.sentence), "{}", p.key);
        }
        assert_eq!(
            direction::sentence(Direction::FromMe),
            "Sent from the backed-up phone's account"
        );
    }
}
