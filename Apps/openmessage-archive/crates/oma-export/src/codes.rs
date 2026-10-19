//! The code registry (M6 spec §4.2, §5.1 "Codes", D142).
//!
//! Every enum an export record carries travels as a [`Code`]: a token and
//! a number. The numbers are **explicit and never reused**:
//!
//! - **Archive codes** ([`CodeSource::Archive`]) are the numbers
//!   `archive.sqlite` stores (`oma_archive::schema::codes`), so a record's
//!   number is the stored number. A number this build does not know is
//!   kept as is, with the token [`UNRECOGNISED_TOKEN`] (spec §3.9: "unknown
//!   keeps the number").
//! - **Export codes** ([`CodeSource::Export`]) are enums that exist only in
//!   Rust (`SenderReason`, `ChipState`, `CardState`, …) or only in the
//!   export (segment kinds, styles, attachment outcomes, exclusion
//!   reasons). Their numbers are written beside each variant below; the
//!   order of variants never defines a number.
//!
//! [`REGISTRIES`] lists every table. `tests/codes.rs` pins the whole
//! registry against `tests/codes.pin`, so a renumbered, reused or
//! removed code fails a test; a new code is added to the pin
//! deliberately. Tokens are the viewer's DTO tokens where the viewer has
//! the same enum, so `export-words.json` can be checked against
//! `viewerText.ts`.

use oma_archive::schema::codes::{
    AssocKindCode, AssocTargetShape, AttributedOutcome, BlobVerifyState, CopyCode,
    DisplayNoneReason, DisplaySource, EditLabelCode, ItemKindCode, LookupCode, PathClassCode,
    ServiceClassCode, SummaryTimeKind, TimeUnit, TsErrorCode, ValueState,
};
use oma_archive::Coded;
use serde::Serialize;

/// The token of a number this build does not know.
pub const UNRECOGNISED_TOKEN: &str = "unrecognisedCode";

/// A code as a record carries it: `{"token": …, "code": …}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct Code {
    pub token: &'static str,
    pub code: i64,
}

impl Code {
    /// A known value's code.
    pub fn of<T: ExportCode>(v: T) -> Code {
        Code {
            token: v.token(),
            code: v.export_code(),
        }
    }

    /// A stored value: its code, or the number this build does not know.
    pub fn coded<T: ExportCode>(v: Coded<T>) -> Code {
        match v {
            Ok(v) => Code::of(v),
            Err(n) => Code {
                token: UNRECOGNISED_TOKEN,
                code: n,
            },
        }
    }

    /// `true` for a number this build does not know.
    pub fn is_unrecognised(&self) -> bool {
        self.token == UNRECOGNISED_TOKEN
    }
}

/// Where a table's numbers come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CodeSource {
    /// The numbers `archive.sqlite` stores.
    Archive,
    /// Numbers assigned here, for the export.
    Export,
}

/// One entry of a table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Entry {
    pub token: &'static str,
    pub code: i64,
}

/// One code table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Registry {
    pub name: &'static str,
    pub source: CodeSource,
    pub entries: &'static [Entry],
}

impl Registry {
    /// The entry with `code`, if any.
    pub fn entry(&self, code: i64) -> Option<Entry> {
        self.entries.iter().copied().find(|e| e.code == code)
    }
}

/// An enum with a registry table.
pub trait ExportCode: Copy {
    fn registry() -> &'static Registry;
    fn export_code(self) -> i64;
    fn token(self) -> &'static str;
}

/// A registry table over an `oma-archive` stored-code enum: the numbers
/// are the stored ones, the tokens are written here. The match is
/// exhaustive, so a new stored code fails to compile until it has a
/// token.
macro_rules! archive_registry {
    ($static:ident, $name:literal, $ty:ident { $($v:ident => $tok:literal),+ $(,)? }) => {
        pub static $static: Registry = Registry {
            name: $name,
            source: CodeSource::Archive,
            entries: &[$(Entry { token: $tok, code: $ty::$v.code() }),+],
        };

        impl ExportCode for $ty {
            fn registry() -> &'static Registry {
                &$static
            }
            fn export_code(self) -> i64 {
                self.code()
            }
            fn token(self) -> &'static str {
                match self {
                    $($ty::$v => $tok),+
                }
            }
        }
    };
}

/// An export-only code enum with explicit, never-reused numbers.
macro_rules! export_registry {
    (
        $(#[$meta:meta])*
        $ty:ident, $static:ident, $name:literal {
            $($(#[$vmeta:meta])* $v:ident = $code:literal => $tok:literal),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum $ty {
            $($(#[$vmeta])* $v),+
        }

        pub static $static: Registry = Registry {
            name: $name,
            source: CodeSource::Export,
            entries: &[$(Entry { token: $tok, code: $code }),+],
        };

        impl $ty {
            /// Every value, in code order.
            pub const ALL: &'static [$ty] = &[$($ty::$v),+];
        }

        impl ExportCode for $ty {
            fn registry() -> &'static Registry {
                &$static
            }
            fn export_code(self) -> i64 {
                match self {
                    $($ty::$v => $code),+
                }
            }
            fn token(self) -> &'static str {
                match self {
                    $($ty::$v => $tok),+
                }
            }
        }
    };
}

// ---------------------------------------------------------------------------
// Archive codes (stored numbers)
// ---------------------------------------------------------------------------

archive_registry!(VALUE_STATE, "valueState", ValueState {
    Null => "null",
    Absent => "absent",
    Oversized => "oversized",
    Unreadable => "unreadable",
});

archive_registry!(TIME_UNIT, "timeUnit", TimeUnit {
    Zero => "zero",
    Negative => "negative",
    Seconds => "seconds",
    Nanoseconds => "nanoseconds",
    Ambiguous => "ambiguous",
});

archive_registry!(ITEM_KIND, "itemKind", ItemKindCode {
    Unknown => "unknown",
    Message => "message",
    ParticipantAdded => "participantAdded",
    OwnNumberChanged => "ownNumberChanged",
    ParticipantRemoved => "participantRemoved",
    GroupRenamed => "groupRenamed",
    ParticipantLeft => "participantLeft",
    GroupPhotoChanged => "groupPhotoChanged",
    GroupPhotoRemoved => "groupPhotoRemoved",
    BackgroundChanged => "backgroundChanged",
    BackgroundRemoved => "backgroundRemoved",
    LegacySharedLocation => "legacySharedLocation",
    AudioKept => "audioKept",
    SharePlay => "sharePlay",
});

archive_registry!(SERVICE_CLASS, "serviceClass", ServiceClassCode {
    IMessage => "iMessage",
    Sms => "sms",
    Rcs => "rcs",
    IMessageLite => "iMessageLite",
    Empty => "empty",
    Other => "other",
});

archive_registry!(ASSOC_KIND, "assocKind", AssocKindCode {
    NoAssociation => "noAssociation",
    StickerLegacy => "stickerLegacy",
    PollVote => "pollVote",
    Unknown => "unknown",
    TapbackLove => "tapbackLove",
    TapbackLike => "tapbackLike",
    TapbackDislike => "tapbackDislike",
    TapbackLaugh => "tapbackLaugh",
    TapbackEmphasize => "tapbackEmphasize",
    TapbackQuestion => "tapbackQuestion",
    TapbackEmoji => "tapbackEmoji",
    TapbackSticker => "tapbackSticker",
    RemovedLove => "removedLove",
    RemovedLike => "removedLike",
    RemovedDislike => "removedDislike",
    RemovedLaugh => "removedLaugh",
    RemovedEmphasize => "removedEmphasize",
    RemovedQuestion => "removedQuestion",
    RemovedEmoji => "removedEmoji",
    RemovedSticker => "removedSticker",
});

archive_registry!(ASSOC_SHAPE, "assocTargetShape", AssocTargetShape {
    Part => "part",
    Bubble => "bubble",
    Bare => "bare",
    Unparsed => "unparsed",
});

archive_registry!(DISPLAY_SOURCE, "displaySource", DisplaySource {
    AttributedBody => "attributedBody",
    Text => "text",
    TextLossy => "textLossy",
});

archive_registry!(DISPLAY_NONE_REASON, "displayNoneReason", DisplayNoneReason {
    AttributedBodyUnusable => "attributedBodyUnusable",
    TextEmpty => "textEmpty",
    NoSource => "noSource",
});

archive_registry!(ATTRIBUTED_OUTCOME, "attributedOutcome", AttributedOutcome {
    Absent => "absent",
    Null => "null",
    Oversized => "oversized",
    Unreadable => "unreadable",
    NotDecoded => "notDecoded",
    Decoded => "decoded",
    DecodeFailed => "decodeFailed",
});

archive_registry!(TS_ERROR, "tsError", TsErrorCode {
    UnsupportedHeader => "unsupportedHeader",
    Truncated => "truncated",
    UnexpectedTag => "unexpectedTag",
    BadReference => "badReference",
    UnsupportedType => "unsupportedType",
    TooDeep => "tooDeep",
    Budget => "budget",
    MaterialisationBudget => "materialisationBudget",
    TrailingBytes => "trailingBytes",
    NotAttributedString => "notAttributedString",
    InvalidUtf8 => "invalidUtf8",
    RunLengthMismatch => "runLengthMismatch",
    RunOverflow => "runOverflow",
    RunSplitsSurrogate => "runSplitsSurrogate",
    BadRunIndex => "badRunIndex",
    Unrecognised => "unrecognised",
});

archive_registry!(EDIT_LABEL, "editLabel", EditLabelCode {
    Undeterminable => "undeterminable",
    NotRecorded => "notRecorded",
    SummaryUnreadable => "summaryUnreadable",
    UnsentPartsAndEditHistory => "unsentPartsAndEditHistory",
    UnsentParts => "unsentParts",
    EditHistory => "editHistory",
    DateEditedNoBody => "dateEditedNoBody",
    DateEditedOnly => "dateEditedOnly",
    NoEditRecorded => "noEditRecorded",
    Undetermined => "undetermined",
});

archive_registry!(SUMMARY_TIME_KIND, "summaryTimeKind", SummaryTimeKind {
    Integer => "integer",
    Real => "real",
    Date => "date",
});

archive_registry!(PATH_CLASS, "pathClass", PathClassCode {
    NoClass => "noClass",
    SmsAttachments => "smsAttachments",
    SmsStickerCache => "smsStickerCache",
    MacMessages => "macMessages",
    OtherHomeRelative => "otherHomeRelative",
    AbsoluteVarMobile => "absoluteVarMobile",
    AbsolutePrivateVarMobile => "absolutePrivateVarMobile",
    Unrecognized => "unrecognized",
    Other => "other",
});

archive_registry!(LOOKUP, "lookup", LookupCode {
    NotLookedUp => "notLookedUp",
    NotAttemptedNull => "notAttemptedNull",
    NotAttemptedTooLong => "notAttemptedTooLong",
    NotAttemptedUnusualName => "notAttemptedUnusualName",
    NotAttemptedClass => "notAttemptedClass",
    NotAttemptedUnrecognized => "notAttemptedUnrecognized",
    NotAttemptedColumnAbsent => "notAttemptedColumnAbsent",
    NotAttemptedUnreadable => "notAttemptedUnreadable",
    NotAttemptedOther => "notAttemptedOther",
    Present => "present",
    BlobMissing => "blobMissing",
    BlobNotRegular => "blobNotRegular",
    BlobUnreadable => "blobUnreadable",
    NotAFile => "notAFile",
    EntryInvalid => "entryInvalid",
    NoKey => "noKey",
    NoProtectionClass => "noProtectionClass",
    ClassKeyUnavailable => "classKeyUnavailable",
    KeyDoesNotUnwrap => "keyDoesNotUnwrap",
    SizeMismatch => "sizeMismatch",
    NotProbed => "notProbed",
    ProbeOther => "probeOther",
    NotListedAtSourcedKey => "notListedAtSourcedKey",
    NotFoundAtAssumedKey => "notFoundAtAssumedKey",
    Ambiguous => "ambiguous",
    InvalidEntry => "invalidEntry",
    LookupOther => "lookupOther",
});

archive_registry!(COPY, "copy", CopyCode {
    Copied => "copied",
    Deduplicated => "deduplicated",
    Adopted => "adopted",
    OrphanReplaced => "orphanReplaced",
    AlreadyArchived => "alreadyArchived",
    NotRequested => "notRequested",
    NotLookedUp => "notLookedUp",
    NotInBackup => "notInBackup",
    BlobUnavailable => "blobUnavailable",
    StoreConflict => "storeConflict",
    OverLimit => "overLimit",
    TooLargeForVolume => "tooLargeForVolume",
    ReadFailed => "readFailed",
    WriteFailed => "writeFailed",
    Replaced => "replaced",
});

archive_registry!(BLOB_VERIFY, "blobVerifyState", BlobVerifyState {
    Ok => "ok",
    Missing => "missing",
    NotRegular => "notRegular",
    LengthMismatch => "lengthMismatch",
    HashMismatch => "hashMismatch",
    Unreadable => "unreadable",
    BadName => "badName",
});

// ---------------------------------------------------------------------------
// Export codes (numbers assigned here; never renumbered or reused)
// ---------------------------------------------------------------------------

export_registry! {
    /// `message.is_from_me` with four tokens (spec §3.9).
    Direction, DIRECTION, "direction" {
        FromMe = 1 => "fromMe",
        Received = 2 => "received",
        NotRecorded = 3 => "notRecorded",
        UnknownValue = 4 => "unknownValue",
    }
}

export_registry! {
    /// Who sent a message, as recorded (never inferred from a chat).
    SenderReasonCode, SENDER_REASON, "senderReason" {
        FromMe = 1 => "fromMe",
        Resolved = 2 => "resolved",
        NoSender = 3 => "noSender",
        ColumnAbsent = 4 => "columnAbsent",
        Unreadable = 5 => "unreadable",
        NotFound = 6 => "notFound",
        DirectionNotRecorded = 7 => "directionNotRecorded",
    }
}

export_registry! {
    /// Why an association row is an item of its own, or where it folds.
    AssocVisibilityCode, ASSOC_VISIBILITY, "assocVisibility" {
        Undeterminable = 1 => "undeterminable",
        NoKind = 2 => "noKind",
        NotAReaction = 3 => "notAReaction",
        TargetUnreadable = 4 => "targetUnreadable",
        TargetNotUnique = 5 => "targetNotUnique",
        TargetInAnotherChat = 6 => "targetInAnotherChat",
        Folded = 7 => "folded",
        TargetFolds = 8 => "targetFolds",
    }
}

export_registry! {
    /// A reaction chip's derived state (D93).
    ChipStateCode, CHIP_STATE, "chipState" {
        Shown = 1 => "shown",
        Hidden = 2 => "hidden",
        Undeterminable = 3 => "undeterminable",
        Added = 4 => "added",
        Removed = 5 => "removed",
    }
}

export_registry! {
    /// Who a chip's rows came from.
    ChipSenderCode, CHIP_SENDER, "chipSender" {
        Me = 1 => "me",
        Handle = 2 => "handle",
        NotRecorded = 3 => "notRecorded",
    }
}

export_registry! {
    /// What a reaction chip is for.
    ReactionKindCode, REACTION_KIND, "reactionKind" {
        Love = 1 => "love",
        Like = 2 => "like",
        Dislike = 3 => "dislike",
        Laugh = 4 => "laugh",
        Emphasize = 5 => "emphasize",
        Question = 6 => "question",
        Emoji = 7 => "emoji",
        Sticker = 8 => "sticker",
    }
}

export_registry! {
    /// Why earlier content is withheld (D115).
    WithheldReasonCode, WITHHELD_REASON, "withheldReason" {
        Unsent = 1 => "unsent",
        EarlierVersion = 2 => "earlierVersion",
        NoCurrentText = 3 => "noCurrentText",
    }
}

export_registry! {
    /// An attachment's card (M5 §5.9).
    CardStateCode, CARD_STATE, "cardState" {
        File = 1 => "file",
        NoObservation = 2 => "noObservation",
        NotInBackup = 3 => "notInBackup",
        NotFoundAtAssumedKey = 4 => "notFoundAtAssumedKey",
        NotCopied = 5 => "notCopied",
        ClassNotLookedUp = 6 => "classNotLookedUp",
        LocationUnusable = 7 => "locationUnusable",
        NotDecrypted = 8 => "notDecrypted",
        CouldNotBeRead = 9 => "couldNotBeRead",
        TooLarge = 10 => "tooLarge",
        Unavailable = 11 => "unavailable",
    }
}

export_registry! {
    /// Why a piece of text is withheld (D115).
    MaskKindCode, MASK_KIND, "maskKind" {
        OneTimeCode = 1 => "oneTimeCode",
    }
}

export_registry! {
    /// A formatting style of a body segment.
    Style, STYLE, "style" {
        Bold = 1 => "bold",
        Italic = 2 => "italic",
        Underline = 3 => "underline",
        Strikethrough = 4 => "strikethrough",
    }
}

export_registry! {
    /// What a body segment is (spec §4.2).
    SegmentKindCode, SEGMENT_KIND, "segmentKind" {
        Text = 1 => "text",
        Withheld = 2 => "withheld",
        InlineEmoji = 3 => "inlineEmoji",
        Attachment = 4 => "attachment",
        UnmatchedPlaceholder = 5 => "unmatchedPlaceholder",
    }
}

export_registry! {
    /// Why a message has no body text.
    NoTextReason, NO_TEXT_REASON, "noTextReason" {
        NoBodyRow = 1 => "noBodyRow",
        AttributedBodyUnusable = 2 => "attributedBodyUnusable",
        TextEmpty = 3 => "textEmpty",
        NoSource = 4 => "noSource",
        ReasonNotRecorded = 5 => "reasonNotRecorded",
        ReasonUnrecognised = 6 => "reasonUnrecognised",
    }
}

export_registry! {
    /// Whether formatting ranges were applied to a body.
    FormattingCode, FORMATTING, "formatting" {
        Plain = 1 => "plain",
        Applied = 2 => "applied",
        NotApplied = 3 => "notApplied",
        Unreadable = 4 => "unreadable",
    }
}

export_registry! {
    /// The value-history columns an export writes (the M5 closed set).
    HistoryColumnCode, HISTORY_COLUMN, "historyColumn" {
        Subject = 1 => "subject",
        GroupTitle = 2 => "groupTitle",
        DestinationCallerId = 3 => "destinationCallerId",
        ChatDisplayName = 4 => "chatDisplayName",
        TransferName = 5 => "transferName",
        EmojiDescription = 6 => "emojiDescription",
    }
}

export_registry! {
    /// Why a version's edit texts were or were not decoded.
    EditTextsOutcomeCode, EDIT_TEXTS_OUTCOME, "editTextsOutcome" {
        NotNeeded = 1 => "notNeeded",
        Decoded = 2 => "decoded",
        Unmatched = 3 => "unmatched",
        SummaryUnreadable = 4 => "summaryUnreadable",
    }
}

export_registry! {
    /// The text of one phone edit entry.
    EditEntryTextCode, EDIT_ENTRY_TEXT, "editEntryText" {
        Text = 1 => "text",
        Withheld = 2 => "withheld",
        NoBody = 3 => "noBody",
        Failed = 4 => "failed",
        NotMatched = 5 => "notMatched",
    }
}

export_registry! {
    /// How a reply's originator was resolved.
    ReplyResolution, REPLY_RESOLUTION, "replyResolution" {
        Resolved = 1 => "resolved",
        InAnotherConversation = 2 => "inAnotherConversation",
        NotUniquelyIdentified = 3 => "notUniquelyIdentified",
    }
}

export_registry! {
    /// A derived label on a message.
    MessageLabelCode, MESSAGE_LABEL, "messageLabel" {
        Undated = 1 => "undated",
        Before2007 = 2 => "before2007",
        Unkeyed = 3 => "unkeyed",
        ScheduleType = 4 => "scheduleType",
        AppContent = 5 => "appContent",
        Effect = 6 => "effect",
        NotInLatest = 7 => "notInLatest",
        Provisional = 8 => "provisional",
    }
}

export_registry! {
    /// Whether an export may carry an attachment's file (spec §3.8).
    AttachmentAccessCode, ATTACHMENT_ACCESS, "attachmentAccess" {
        Included = 1 => "included",
        Withheld = 2 => "withheld",
        Revealed = 3 => "revealed",
    }
}

export_registry! {
    /// What the export did with an attachment's file (spec §4.2).
    AttachmentOutcome, ATTACHMENT_OUTCOME, "attachmentOutcome" {
        Copied = 1 => "copied",
        Deduplicated = 2 => "deduplicated",
        NotRequested = 3 => "notRequested",
        WithheldUnsent = 4 => "withheldUnsent",
        ChecksumMismatch = 5 => "checksumMismatch",
        TooLargeForVolume = 6 => "tooLargeForVolume",
        WriteFailed = 7 => "writeFailed",
        BlockedExtension = 8 => "blockedExtension",
        NoStoredFile = 9 => "noStoredFile",
    }
}

export_registry! {
    /// Which stored field an alternative text is.
    AltTextSourceCode, ALT_TEXT_SOURCE, "altTextSource" {
        TextColumn = 1 => "textColumn",
        DecodedAttributedBody = 2 => "decodedAttributedBody",
    }
}

export_registry! {
    /// Why an alternative text is withheld.
    AltTextWithheldCode, ALT_TEXT_WITHHELD, "altTextWithheld" {
        EarlierContent = 1 => "earlierContent",
        OneTimeCode = 2 => "oneTimeCode",
    }
}

export_registry! {
    /// Why an archive column is not exported (the ledger, spec §4.8).
    ExclusionReason, EXCLUSION_REASON, "exclusionReason" {
        D94 = 1 => "d94",
        D115Raw = 2 => "d115Raw",
        RawSourceBytes = 3 => "rawSourceBytes",
        SourceIdentifierOptIn = 4 => "sourceIdentifierOptIn",
        SessionLocalRowid = 5 => "sessionLocalRowid",
        StorageInternal = 6 => "storageInternal",
    }
}

/// Every registry table, in a fixed order (pinned by `tests/codes.rs`).
pub static REGISTRIES: &[&Registry] = &[
    &VALUE_STATE,
    &TIME_UNIT,
    &ITEM_KIND,
    &SERVICE_CLASS,
    &ASSOC_KIND,
    &ASSOC_SHAPE,
    &DISPLAY_SOURCE,
    &DISPLAY_NONE_REASON,
    &ATTRIBUTED_OUTCOME,
    &TS_ERROR,
    &EDIT_LABEL,
    &SUMMARY_TIME_KIND,
    &PATH_CLASS,
    &LOOKUP,
    &COPY,
    &BLOB_VERIFY,
    &DIRECTION,
    &SENDER_REASON,
    &ASSOC_VISIBILITY,
    &CHIP_STATE,
    &CHIP_SENDER,
    &REACTION_KIND,
    &WITHHELD_REASON,
    &CARD_STATE,
    &MASK_KIND,
    &STYLE,
    &SEGMENT_KIND,
    &NO_TEXT_REASON,
    &FORMATTING,
    &HISTORY_COLUMN,
    &EDIT_TEXTS_OUTCOME,
    &EDIT_ENTRY_TEXT,
    &REPLY_RESOLUTION,
    &MESSAGE_LABEL,
    &ATTACHMENT_ACCESS,
    &ATTACHMENT_OUTCOME,
    &ALT_TEXT_SOURCE,
    &ALT_TEXT_WITHHELD,
    &EXCLUSION_REASON,
];

/// The registry as text, one `name<TAB>source<TAB>code<TAB>token` line per
/// entry: the form `tests/codes.pin` holds.
pub fn registry_text() -> String {
    let mut out = String::new();
    for r in REGISTRIES {
        let source = match r.source {
            CodeSource::Archive => "archive",
            CodeSource::Export => "export",
        };
        for e in r.entries {
            out.push_str(&format!("{}\t{source}\t{}\t{}\n", r.name, e.code, e.token));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn every_table_has_unique_codes_and_tokens() {
        let mut names = BTreeSet::new();
        for r in REGISTRIES {
            assert!(names.insert(r.name), "{} twice", r.name);
            let codes: BTreeSet<i64> = r.entries.iter().map(|e| e.code).collect();
            let tokens: BTreeSet<&str> = r.entries.iter().map(|e| e.token).collect();
            assert_eq!(codes.len(), r.entries.len(), "{}: a code twice", r.name);
            assert_eq!(tokens.len(), r.entries.len(), "{}: a token twice", r.name);
            assert!(!tokens.contains(UNRECOGNISED_TOKEN), "{}", r.name);
            for t in tokens {
                assert!(
                    t.chars().all(|c| c.is_ascii_alphanumeric()),
                    "{}: token {t}",
                    r.name
                );
            }
        }
    }

    #[test]
    fn archive_tables_follow_the_stored_codes() {
        fn check<T: ExportCode>(all: &[T]) {
            let r = T::registry();
            assert_eq!(r.entries.len(), all.len(), "{}", r.name);
            for v in all {
                assert_eq!(
                    r.entry(v.export_code()).map(|e| e.token),
                    Some(v.token()),
                    "{}",
                    r.name
                );
            }
        }
        check(ValueState::ALL);
        check(TimeUnit::ALL);
        check(ItemKindCode::ALL);
        check(ServiceClassCode::ALL);
        check(AssocKindCode::ALL);
        check(AssocTargetShape::ALL);
        check(DisplaySource::ALL);
        check(DisplayNoneReason::ALL);
        check(AttributedOutcome::ALL);
        check(TsErrorCode::ALL);
        check(EditLabelCode::ALL);
        check(SummaryTimeKind::ALL);
        check(PathClassCode::ALL);
        check(LookupCode::ALL);
        check(CopyCode::ALL);
        check(BlobVerifyState::ALL);
        check(Direction::ALL);
        check(ExclusionReason::ALL);
    }

    #[test]
    fn every_code_renders_a_token_and_a_number_and_unknown_keeps_it() {
        assert_eq!(
            Code::coded(Ok(ItemKindCode::Message)),
            Code {
                token: "message",
                code: 1
            }
        );
        let u = Code::coded::<ItemKindCode>(Err(99));
        assert_eq!((u.token, u.code), (UNRECOGNISED_TOKEN, 99));
        assert!(u.is_unrecognised());
        let v = serde_json::to_value(Code::of(SenderReasonCode::NotFound)).expect("json");
        assert_eq!(v, serde_json::json!({"token": "notFound", "code": 6}));
    }
}
