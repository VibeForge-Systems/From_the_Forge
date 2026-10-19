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
    SegmentKindCode, SenderReasonCode, SniffedKindCode, Style, WithheldReasonCode,
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

words! {
    /// What a copied file's bytes are (the viewer's sentences).
    sniffed_kind, "sniffedKind", SniffedKindCode, viewer = "SNIFFED_KIND_TEXT" {
        Gif => "GIF image",
        Webp => "WebP image",
        Png => "PNG image",
        Jpeg => "JPEG image",
        Bmp => "BMP image",
        Ico => "Icon image",
        QuickTime => "QuickTime video",
        M4a => "MPEG-4 audio",
        M4v => "MPEG-4 video",
        Heif => "HEIC image" @ "",
        Mp4 => "MPEG-4 video",
        IsoBmffOther => "MPEG-4 family file",
        Webm => "WebM video",
        Wav => "WAV audio",
        Aiff => "AIFF audio",
        Ogg => "Ogg audio",
        Mp3 => "MP3 audio",
        Caf => "Core Audio file",
        Pdf => "PDF document",
        Xml => "XML document",
        Html => "HTML document" @ "",
        Vcard => "Contact card (vCard)",
        Text => "Text file",
        Unknown => "Unrecognised content",
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
        sniffed_kind::table(),
    ]
}

/// The fixed text of a bundle's `README.txt` (spec §7.2). Paragraphs, in
/// order; [`readme`] picks the ones an export's options need. No sentence
/// names a path, a conversation, a search or a device, and none says
/// "you".
pub mod readme {
    use crate::ids::IdentifierMode;
    use crate::plan::Format;

    pub const TITLE: &str = "OpenMessage Archive export";

    pub const WHAT: &str = "This folder is an export from an OpenMessage Archive archive: copies of messages, and of attachment files when they were included, from iPhone backups. The files are plain text and ordinary files. Anyone who can open this folder can read them; they are not encrypted.";

    pub const VERIFY: &str = "To check that no file changed since the export: SHA256SUMS lists a SHA-256 checksum for every file, manifest.json included. On Linux, run 'sha256sum -c SHA256SUMS' in this folder. On Windows, compare each file's 'Get-FileHash <file>' output with its line. manifest.json lists every file except SHA256SUMS with its size and checksum.";

    pub const MANIFEST: &str = "manifest.json records what was exported: the scope kind, the formats, the options, the time zone table the times were written in, the counts and the kinds of data that are never exported. It never records a search's words, device facts, the archive's name or id, or any path.";

    pub const MARKERS: &str = "Values marked 'derived' were worked out by the archive from what the phone recorded, not recorded by the phone itself. Text in brackets such as [withheld] or [masked] is a placeholder written by the export, not message text. Characters that do not show when printed (zero-width, direction and other invisible characters) are written as a code such as \u{27E8}U+200B\u{27E9} when markers are on. The character \u{27E8} itself is always escaped inside message text, so a placeholder can never be forged by a message.";

    pub const MASKING: &str = "One-time codes are masked only when the phone marked the message as holding one; other codes in message text are not detected. Text captured before a message was unsent or edited, and the attachments of unsent parts, are withheld unless the export was made with that option, after confirming the count.";

    pub const ORDER: &str = "Items are in the order of their recorded time. When two items have the same recorded time, their order is the order the archive imported them in, and they are flagged as a tie: that order is not something the phone recorded. Items without a usable recorded time come after the dated ones.";

    pub const ORDINALS: &str = "Conversations, messages, people's addresses and attachment files are numbered within this export only (1, 2, 3 ...). The numbers do not match the archive's own row numbers and say nothing about the archive's size; two exports cannot be matched on them.";

    pub const ARCHIVE_IDS: &str = "This export also carries the archive's row ids (as decimal strings) and the identifiers the phone recorded. Row ids have gaps that reveal how many rows the archive holds outside this export, and the phone's identifiers can match these records to other copies of the same messages.";

    pub const CSV: &str = "CSV files (csv/) are UTF-8, comma-separated, with CRLF record ends and every field in quotes (RFC 4180, whose text is ASCII: UTF-8 is an extension). Read columns by their header name: the column contract is 'tabularColumnsVersion' in manifest.json, and columns are only ever added. Every file is present, with its header row only when an option left its rows out. In the 'for spreadsheets' profile each file starts with a byte order mark, control characters are written as codes such as \u{27E8}U+001B\u{27E9}, and a cell that a spreadsheet could run as a formula starts with an apostrophe; the row's 'neutralised_columns' names the columns where that was done. In the 'for data tools' profile values are raw, except that \u{27E8} is escaped. A spreadsheet may round long numbers such as raw times or cut very long cells: the JSON files keep exact values.";

    pub const XLSX: &str = "In XLSX workbooks, a value longer than a cell can hold continues on the next rows, marked as continuation rows.";

    pub const PDF: &str = "PDF files show the text with the bundled fonts; a character no font can draw shows as a box and is counted in manifest.json, while the text layer keeps the true character.";

    pub const JSON: &str = "The JSON files (json/) keep values as recorded, with codes instead of sentences, and follow the schema in schema/export-v1.schema.json, named by 'schemaVersion'. Each conversation is a JSON Lines file, one record per line; conversations.json and handles.json list the conversations and addresses. Numbers that can be too large for some readers, such as raw times, are written as strings. Line and paragraph separators and other characters that some tools treat as line ends are written as \\u escapes. 'Raw' there means as stored in the archive, which is not always the bytes the phone stored: bytes that were not valid text show as U+FFFD and are flagged, and text is not normalised. The files are JSON (RFC 8259), not I-JSON.";

    pub const HTML: &str = "The HTML pages (html/) open in a web browser from this folder, without a network and without scripts: each page forbids loading anything but the images embedded in it. html/index.html lists the conversations; a long conversation is split into parts (c0001.html, c0001-2.html ...). Images shown there are previews the app re-encoded, never the original files: original files, when they were copied, are named by their path in this folder but not linked. Web addresses in messages are text, not links, and a reply links only to a message earlier on the same page.";

    pub const CONTRACT: &str = "Columns and keys are only ever added in a minor version; a key is never removed or renamed within a major version. The versions are in manifest.json.";

    /// The paragraphs of an export's `README.txt`, in order.
    pub fn paragraphs(formats: &[Format], identifiers: IdentifierMode) -> Vec<&'static str> {
        let mut out = vec![WHAT, VERIFY, MANIFEST, MARKERS, MASKING, ORDER];
        out.push(match identifiers {
            IdentifierMode::Ordinals => ORDINALS,
            IdentifierMode::ArchiveAndSource => ARCHIVE_IDS,
        });
        for f in formats {
            match f {
                Format::Csv => out.push(CSV),
                Format::Xlsx => out.push(XLSX),
                Format::Pdf => out.push(PDF),
                Format::Json => out.push(JSON),
                Format::Html => out.push(HTML),
                Format::Txt => {}
            }
        }
        out.push(CONTRACT);
        out
    }
}

/// The fixed text of the TXT format (spec §5.4): its first line, and the
/// legend at the end of every file, which explains each bracketed tag and
/// marker once. No sentence names a path, a conversation, a search or a
/// device, and none says "you".
pub mod txt_legend {
    pub const FIRST_LINE: &str =
        "OpenMessage Archive export \u{2014} plaintext copy. See the end of this file for details.";

    pub const LINES: &[&str] = &[
        "Each item starts at the beginning of a line with its recorded time in brackets and its number in this export, [item N], then the sender's address when one is recorded, then tags in brackets. Everything that belongs to an item is indented: labels by four spaces, recorded text by six. A line of message text can therefore never start a new item.",
        "Times are shown in the zone named at the top, with the offset in force on every line. [Undated] marks an item the archive sorts as undated; undated items come last and follow the order of the earliest import that recorded them, which is not a recorded fact. [No usable time] marks an item sorted among the dated ones whose own recorded time cannot be shown; a tag says why.",
        "[same recorded time; order not known]: the item has the same recorded time as its neighbour, so their order is the order the archive imported them in.",
        "Values and tags marked 'derived' were worked out by the archive from what the phone recorded, not recorded by the phone itself.",
        "Text in angle brackets \u{27E8}\u{2026}\u{27E9} is written by the export, never by a message: \u{27E8}U+200B\u{27E9} stands for a character that does not show when printed or that controls the terminal or the text direction, \u{27E8}U+005B\u{27E9} and \u{27E8}U+005D\u{27E9} for square brackets in an address or a name, \u{27E8}+N combining marks\u{27E9} for marks left out of a very long run of accents, and \u{27E8}one-time code hidden\u{27E9} or \u{27E8}Attachment N\u{27E9} for what stood at that place in the text. A literal \u{27E8} in a message is itself written as \u{27E8}U+27E8\u{27E9}.",
        "Line breaks inside a message become new indented lines. In addresses, names and other fields on a tag line, every line break and control is written as a code instead.",
    ];
}

/// The fixed text of the HTML format (spec §5.5): the page furniture,
/// the preview cards and the legend at the end of every page. No sentence
/// names a path, a conversation, a search or a device, and none says
/// "you". Window titles stay generic (D112): content is only in the body.
pub mod html_text {
    pub const TITLE: &str = "Messages export";
    pub const NOTICE: &str = "OpenMessage Archive export \u{2014} plaintext copy: anyone who can open this file can read it. What the marks mean and how the export was made are at the end of this page.";
    pub const SKIP_MAIN: &str = "Skip to messages";
    pub const SKIP_LIST: &str = "Skip to the list";
    pub const SKIP_DAYS: &str = "Jump to days";
    pub const DAYS: &str = "Days";
    pub const UNDATED: &str = "Undated";
    pub const NO_TIME: &str = "No usable time";
    pub const NO_TIME_SECTION: &str = "Items without a usable recorded time";
    pub const EMPTY: &str = "No items of this conversation are in this export.";
    pub const VERSIONS: &str =
        "Stored versions and edit entries recorded on the phone (stored order)";
    pub const PREVIEW: &str = "preview, re-encoded";
    pub const LARGER: &str = "Larger preview";
    pub const UNAVAILABLE: &str = "Preview unavailable";
    pub const SIZE_LIMIT: &str = "Preview not embedded (size limit)";
    pub const PARTS: &str = "Parts of this conversation:";
    pub const NEXT: &str = "Next part";
    pub const INDEX_LINK: &str = "All conversations in this export";
    pub const INDEX_HEADING: &str = "Conversations in this export";
    pub const ABOUT: &str = "About this export";
    pub const ABOUT_BUNDLE: &str = "How this export was made (its scope, options, time zone table, counts and the kinds of data never exported) is recorded in manifest.json and explained in README.txt, in the export's folder, and listed at the end of index.html.";

    pub const LEGEND: &[&str] = &[
        "Each item is headed by the sender's address when one is recorded, or by what is recorded about the sender, and by its recorded time in the zone named at the top, with the offset in force. Items the archive sorts as undated come last and follow the order of the earliest import that recorded them, which is not a recorded fact. Tags in brackets follow the heading.",
        "[same recorded time; order not known]: the item has the same recorded time as its neighbour, so their order is the order the archive imported them in.",
        "Values and tags marked 'derived' were worked out by the archive from what the phone recorded, not recorded by the phone itself.",
        "A framed code such as U+200B stands for a character that does not show when printed or that changes the text direction or controls a terminal. A framed phrase such as 'one-time code hidden' or 'Attachment 3' is written by the export and stands for what was at that place in the text. Neither is ever message text.",
        "Images are previews re-encoded by the app, never the original files. Original files, when they were copied, are named by their path in the export's folder but not linked. Web addresses in messages are text, not links.",
        "A reply links to the message it answers only when that message is earlier on the same page; otherwise its item number is named.",
    ];
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
        let all = readme::paragraphs(
            &crate::plan::Format::ALL,
            crate::ids::IdentifierMode::ArchiveAndSource,
        );
        for p in all
            .iter()
            .chain(readme::paragraphs(&[], crate::ids::IdentifierMode::Ordinals).iter())
            .chain(txt_legend::LINES)
            .chain([&txt_legend::FIRST_LINE])
            .chain(html_text::LEGEND)
            .chain([
                &html_text::NOTICE,
                &html_text::ABOUT_BUNDLE,
                &html_text::EMPTY,
                &html_text::VERSIONS,
            ])
        {
            assert!(!says_you(p), "{p}");
        }
    }
}
