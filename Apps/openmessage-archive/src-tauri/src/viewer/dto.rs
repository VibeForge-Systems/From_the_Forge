//! Every IPC type of the viewer (M5 spec §7.3), and their conversion from
//! `oma_archive`'s viewer types.
//!
//! These are the first DTOs that carry message content, addresses and
//! names. The rules, each enforced by a test in this module:
//!
//! - **Content crosses only as [`UntrustedText`]**, inside
//!   [`ContentTextDto`]; the frontend renders it by interpolation only
//!   (I7). The other string brand is [`DecimalString`], for raw source
//!   numbers that may exceed 2^53 (`^-?[0-9]{1,20}$`). No other `string`
//!   field exists.
//! - **Ids cross as [`SafeId`]** (1 to 2^53 − 1). A row whose id is out of
//!   range is skipped and counted in `skippedRows`; nothing is rounded.
//! - **No GUID, rank, rowid, cursor, sort key, path, storage name, digest,
//!   device fact, or read, delivery or sent state** is a field (D94); the
//!   viewer guard bans the keys and `bigint`.
//! - Instants cross as Unix milliseconds, floored; raw values as decimal
//!   strings; every derived state as a closed code the UI words from an
//!   exhaustive table.
//!
//! The archive guard (`archives::dto`) rejects every brand and every type
//! listed in [`M5_VIEWER_TYPES`], so neither guard silently covers the
//! other (D95).

use oma_archive as oa;
use oma_archive::schema::codes;
use serde::Serialize;
use ts_rs::TS;

/// 2^53 − 1: the largest integer a JavaScript number holds exactly.
pub const MAX_SAFE: i64 = 9_007_199_254_740_991;

// ------------------------------------------------------------------ brands

/// An archive row id, checked to be in `1..=2^53 − 1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, TS)]
#[serde(transparent)]
#[ts(export, type = "number & { readonly __id: unique symbol }")]
pub struct SafeId(i64);

impl SafeId {
    /// `Some` only for `1..=2^53 − 1`.
    pub fn new(id: i64) -> Option<SafeId> {
        (1..=MAX_SAFE).contains(&id).then_some(SafeId(id))
    }

    pub fn get(self) -> i64 {
        self.0
    }
}

/// Text from the archive: message bodies, addresses, chat names, file
/// names. Rendered by interpolation only, never as HTML or in an
/// attribute. `Debug` prints only its length.
#[derive(Clone, PartialEq, Eq, Serialize, TS)]
#[serde(transparent)]
#[ts(export, type = "string & { readonly __untrusted: unique symbol }")]
pub struct UntrustedText(String);

impl UntrustedText {
    /// The one constructor: the text of a [`oa::ContentText`].
    fn of(t: &oa::ContentText) -> UntrustedText {
        UntrustedText(t.text.expose().clone())
    }

    /// The UTF-8 length (for tests and size bounds).
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for UntrustedText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "UntrustedText(<{} bytes>)", self.0.len())
    }
}

/// A raw integer as recorded, as decimal digits (nanosecond values exceed
/// 2^53).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(transparent)]
#[ts(export, type = "string & { readonly __decimal: unique symbol }")]
pub struct DecimalString(String);

impl DecimalString {
    pub fn of(v: i64) -> DecimalString {
        DecimalString(v.to_string())
    }

    /// `Some` only for `^-?[0-9]{1,20}$`.
    pub fn parse(s: &str) -> Option<DecimalString> {
        let digits = s.strip_prefix('-').unwrap_or(s);
        (!digits.is_empty() && digits.len() <= 20 && digits.bytes().all(|b| b.is_ascii_digit()))
            .then(|| DecimalString(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A media token (§6.3): 32 lowercase hex characters naming one surfaced
/// attachment in this viewer session. Only `services/media.ts` turns it
/// into a URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(transparent)]
#[ts(export, type = "string & { readonly __mediaToken: unique symbol }")]
pub struct MediaToken(String);

impl MediaToken {
    pub fn of(t: &super::media::tokens::TokenBytes) -> MediaToken {
        MediaToken(t.hex())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The base of every media URL on this platform (§6.2):
/// `oma-media://localhost/` on Linux, `http://oma-media.localhost/` on
/// Windows. Rust decides it, so the frontend never guesses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(transparent)]
#[ts(export, type = "string & { readonly __mediaBase: unique symbol }")]
pub struct MediaBase(String);

impl MediaBase {
    pub fn platform() -> MediaBase {
        MediaBase(super::media::media_base().to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn dec(v: Option<i64>) -> Option<DecimalString> {
    v.map(DecimalString::of)
}

/// A count as a safe integer (saturating; counts never come near 2^53).
fn n(v: u64) -> u64 {
    v.min(MAX_SAFE.unsigned_abs())
}

fn ms(ns: i64) -> i64 {
    crate::archives::dto::ms(ns)
}

fn len_of(v: usize) -> u64 {
    n(u64::try_from(v).unwrap_or(u64::MAX))
}

/// Converts ids and counts the rows skipped because an id was out of
/// range (`skippedRows`).
#[derive(Debug, Default)]
pub struct Conv {
    pub skipped: u64,
}

impl Conv {
    /// `Some` for a safe id; counts a skip otherwise.
    pub fn id(&mut self, id: i64) -> Option<SafeId> {
        let s = SafeId::new(id);
        if s.is_none() {
            self.skipped = self.skipped.saturating_add(1);
        }
        s
    }

    /// Every id that converts; the others are counted.
    fn ids(&mut self, ids: impl IntoIterator<Item = i64>) -> Vec<SafeId> {
        ids.into_iter().filter_map(|i| self.id(i)).collect()
    }

    /// `None` stays `None`; an out-of-range id fails the caller.
    fn opt_id(&mut self, id: Option<i64>) -> Option<Option<SafeId>> {
        match id {
            None => Some(None),
            Some(i) => self.id(i).map(Some),
        }
    }
}

// ------------------------------------------------------------ closed codes

/// Mirrors a closed code of `oma_archive::schema::codes` as a camelCase
/// string enum. The match is exhaustive: a new code fails the build until
/// it is mirrored, and the UI's `Record<>` table fails the typecheck until
/// it has words.
macro_rules! code_dto {
    ($(#[$meta:meta])* $dto:ident from $src:ident { $($v:ident),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, TS)]
        #[serde(rename_all = "camelCase")]
        #[ts(export)]
        pub enum $dto { $($v),+ }

        impl From<codes::$src> for $dto {
            fn from(c: codes::$src) -> $dto {
                match c { $(codes::$src::$v => $dto::$v),+ }
            }
        }
    };
}

code_dto! {
    /// Why a source value is missing.
    ValueStateDto from ValueState { Null, Absent, Oversized, Unreadable }
}
code_dto! {
    /// How a raw timestamp was read.
    TimeUnitDto from TimeUnit { Zero, Negative, Seconds, Nanoseconds, Ambiguous }
}
code_dto! {
    /// A chat's or handle's service class, as recorded.
    ServiceClassDto from ServiceClassCode { IMessage, Sms, Rcs, IMessageLite, Empty, Other }
}
code_dto! {
    /// What kind of timeline item a row is.
    ItemKindDto from ItemKindCode {
        Unknown, Message, ParticipantAdded, OwnNumberChanged, ParticipantRemoved,
        GroupRenamed, ParticipantLeft, GroupPhotoChanged, GroupPhotoRemoved,
        BackgroundChanged, BackgroundRemoved, LegacySharedLocation, AudioKept, SharePlay,
    }
}
code_dto! {
    /// An association's kind (tapbacks and others).
    AssocKindDto from AssocKindCode {
        NoAssociation, StickerLegacy, PollVote, Unknown, TapbackLove, TapbackLike,
        TapbackDislike, TapbackLaugh, TapbackEmphasize, TapbackQuestion, TapbackEmoji,
        TapbackSticker, RemovedLove, RemovedLike, RemovedDislike, RemovedLaugh,
        RemovedEmphasize, RemovedQuestion, RemovedEmoji, RemovedSticker,
    }
}
code_dto! {
    /// Why a body has no text.
    DisplayNoneReasonDto from DisplayNoneReason { AttributedBodyUnusable, TextEmpty, NoSource }
}
code_dto! {
    /// Where a body's text came from.
    DisplaySourceDto from DisplaySource { AttributedBody, Text, TextLossy }
}
code_dto! {
    /// The edit label of a body (derived).
    EditLabelDto from EditLabelCode {
        Undeterminable, NotRecorded, SummaryUnreadable, UnsentPartsAndEditHistory,
        UnsentParts, EditHistory, DateEditedNoBody, DateEditedOnly, NoEditRecorded,
        Undetermined,
    }
}
code_dto! {
    /// Why a typedstream could not be decoded.
    TsErrorDto from TsErrorCode {
        UnsupportedHeader, Truncated, UnexpectedTag, BadReference, UnsupportedType,
        TooDeep, Budget, MaterialisationBudget, TrailingBytes, NotAttributedString,
        InvalidUtf8, RunLengthMismatch, RunOverflow, RunSplitsSurrogate, BadRunIndex,
        Unrecognised,
    }
}
code_dto! {
    /// The outcome of decoding `attributedBody`.
    AttributedOutcomeDto from AttributedOutcome {
        Absent, Null, Oversized, Unreadable, NotDecoded, Decoded, DecodeFailed,
    }
}
code_dto! {
    /// The shape of an association's target GUID.
    AssocTargetShapeDto from AssocTargetShape { Part, Bubble, Bare, Unparsed }
}
code_dto! {
    /// How an edit entry's time was recorded.
    SummaryTimeKindDto from SummaryTimeKind { Integer, Real, Date }
}
code_dto! {
    /// How an attachment's file was looked up in the backup.
    LookupDto from LookupCode {
        NotLookedUp, NotAttemptedNull, NotAttemptedTooLong, NotAttemptedUnusualName,
        NotAttemptedClass, NotAttemptedUnrecognized, NotAttemptedColumnAbsent,
        NotAttemptedUnreadable, NotAttemptedOther, Present, BlobMissing, BlobNotRegular,
        BlobUnreadable, NotAFile, EntryInvalid, NoKey, NoProtectionClass,
        ClassKeyUnavailable, KeyDoesNotUnwrap, SizeMismatch, NotProbed, ProbeOther,
        NotListedAtSourcedKey, NotFoundAtAssumedKey, Ambiguous, InvalidEntry, LookupOther,
    }
}
code_dto! {
    /// What the import did with an attachment's file.
    CopyDto from CopyCode {
        Copied, Deduplicated, Adopted, OrphanReplaced, AlreadyArchived, NotRequested,
        NotLookedUp, NotInBackup, BlobUnavailable, StoreConflict, OverLimit,
        TooLargeForVolume, ReadFailed, WriteFailed, Replaced,
    }
}
code_dto! {
    /// The class of an attachment's recorded path (never the path).
    PathClassDto from PathClassCode {
        NoClass, SmsAttachments, SmsStickerCache, MacMessages, OtherHomeRelative,
        AbsoluteVarMobile, AbsolutePrivateVarMobile, Unrecognized, Other,
    }
}
code_dto! {
    /// The last verify result of a stored file.
    BlobVerifyStateDto from BlobVerifyState {
        Ok, Missing, NotRegular, LengthMismatch, HashMismatch, Unreadable, BadName,
    }
}
code_dto! {
    /// An import session's state.
    ViewerSessionStateDto from SessionState {
        Running, Completed, Cancelled, Failed, Interrupted, Superseded, Stopped,
    }
}

// ---------------------------------------------------------------- common

/// One content string, cut to its cap, with facts about the whole value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ContentTextDto {
    pub text: UntrustedText,
    pub lossy: bool,
    pub stored_as_blob: bool,
    pub truncated: bool,
    pub full_utf16: u32,
    pub bidi_controls: bool,
    pub invisible: bool,
}

impl From<&oa::ContentText> for ContentTextDto {
    fn from(t: &oa::ContentText) -> Self {
        ContentTextDto {
            text: UntrustedText::of(t),
            lossy: t.lossy,
            stored_as_blob: t.stored_as_blob,
            truncated: t.truncated,
            full_utf16: t.full_utf16,
            bidi_controls: t.bidi_controls,
            invisible: t.invisible,
        }
    }
}

/// A content value, or why there is none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export)]
pub enum OptContentDto {
    Text { value: ContentTextDto },
    Missing { state: ValueStateDto },
}

impl From<&oa::OptContent> for OptContentDto {
    fn from(c: &oa::OptContent) -> Self {
        match c {
            oa::OptContent::Text(t) => OptContentDto::Text { value: t.into() },
            oa::OptContent::Missing(s) => OptContentDto::Missing { state: (*s).into() },
            _ => OptContentDto::Missing {
                state: ValueStateDto::Unreadable,
            },
        }
    }
}

/// Why text is withheld (D115).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum MaskKindDto {
    OneTimeCode,
}

fn mask(m: oa::MaskKind) -> MaskKindDto {
    match m {
        oa::MaskKind::OneTimeCode => MaskKindDto::OneTimeCode,
        _ => MaskKindDto::OneTimeCode,
    }
}

/// A piece of a masked text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export)]
pub enum TextPieceDto {
    Text { value: ContentTextDto },
    Masked { mask: MaskKindDto },
}

fn pieces(p: &[oa::TextPiece]) -> Vec<TextPieceDto> {
    p.iter()
        .map(|p| match p {
            oa::TextPiece::Text(t) => TextPieceDto::Text { value: t.into() },
            oa::TextPiece::Masked(m) => TextPieceDto::Masked { mask: mask(*m) },
            _ => TextPieceDto::Masked {
                mask: MaskKindDto::OneTimeCode,
            },
        })
        .collect()
}

/// A count read up to a cap ("N+").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CappedCountDto {
    pub n: u64,
    pub capped: bool,
}

impl From<oa::CappedCount> for CappedCountDto {
    fn from(c: oa::CappedCount) -> Self {
        CappedCountDto {
            n: n(c.n),
            capped: c.capped,
        }
    }
}

/// A timestamp as recorded: the instant (Unix ms, floored) when the unit
/// gives one, the raw value as digits, how it was read, or why there is
/// none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TimeDto {
    pub ms: Option<i64>,
    pub raw: Option<DecimalString>,
    pub unit: Option<TimeUnitDto>,
    pub state: Option<ValueStateDto>,
    pub before2007: bool,
}

impl From<&oa::RecordedTime> for TimeDto {
    fn from(t: &oa::RecordedTime) -> Self {
        TimeDto {
            ms: t.utc_ns.map(ms),
            raw: dec(t.raw),
            unit: t.unit.map(Into::into),
            state: t.state.map(Into::into),
            before2007: t.before_2007,
        }
    }
}

/// `is_from_me` as the viewer shows it. Read, delivery and sent state are
/// never shown (D94).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum DirectionDto {
    You,
    Received,
    NotRecorded,
}

impl From<oa::DirectionCode> for DirectionDto {
    fn from(d: oa::DirectionCode) -> Self {
        match d {
            oa::DirectionCode::You => DirectionDto::You,
            oa::DirectionCode::Received => DirectionDto::Received,
            _ => DirectionDto::NotRecorded,
        }
    }
}

/// One recorded address and its service, as recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct HandleDto {
    pub id: SafeId,
    pub address: OptContentDto,
    pub service: Option<ServiceClassDto>,
}

fn handle(p: &oa::Participant, cx: &mut Conv) -> Option<HandleDto> {
    Some(HandleDto {
        id: cx.id(p.handle.get())?,
        address: (&p.address).into(),
        service: p.service.map(Into::into),
    })
}

// ------------------------------------------------------------ the session

/// One import session, content-free, for the "in the backup of" sentences.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ViewerImportDto {
    pub session: SafeId,
    pub state: ViewerSessionStateDto,
    pub started_ms: i64,
    pub ended_ms: Option<i64>,
}

/// The archive's totals as of the last completed import.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ViewerStatsDto {
    pub chats: u64,
    pub messages: u64,
    pub attachments: u64,
}

/// `open_viewer`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ViewerSessionDto {
    /// The session's generation: every later request carries it.
    pub viewer: u32,
    /// An import into this archive is running (pages read committed data).
    pub import_running: bool,
    /// The newest import's derived values are provisional (D79).
    pub provisional: bool,
    pub imports: Vec<ViewerImportDto>,
    pub stats: Option<ViewerStatsDto>,
    /// "N messages have no searchable text"; `None` when it could not be
    /// counted within its budget.
    pub unsearchable: Option<u64>,
    pub skipped_rows: u64,
    /// The base of media URLs on this platform.
    pub media_base: MediaBase,
}

impl ViewerSessionDto {
    pub fn build(
        viewer: u32,
        import_running: bool,
        provisional: bool,
        sessions: &[oa::SessionInfo],
        stats: Option<oa::ArchiveStats>,
        unsearchable: Option<u64>,
    ) -> ViewerSessionDto {
        let mut cx = Conv::default();
        let imports = sessions
            .iter()
            .filter_map(|s| {
                Some(ViewerImportDto {
                    session: cx.id(s.id)?,
                    state: s.state.into(),
                    started_ms: ms(s.started_at_ns),
                    ended_ms: s.ended_at_ns.map(ms),
                })
            })
            .collect();
        ViewerSessionDto {
            viewer,
            import_running,
            provisional,
            imports,
            stats: stats.map(|s| ViewerStatsDto {
                chats: n(s.chats),
                messages: n(s.messages),
                attachments: n(s.attachments),
            }),
            unsearchable: unsearchable.map(n),
            skipped_rows: cx.skipped,
            media_base: MediaBase::platform(),
        }
    }
}

// ----------------------------------------------------------- conversations

/// `list_conversations`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ConversationIndexDto {
    pub ids: Vec<SafeId>,
    pub total: u64,
    pub truncated: bool,
    pub skipped_rows: u64,
}

impl From<&oa::ConversationIndex> for ConversationIndexDto {
    fn from(i: &oa::ConversationIndex) -> Self {
        let mut cx = Conv::default();
        let ids = cx.ids(i.ids.iter().map(|c| c.get()));
        ConversationIndexDto {
            ids,
            total: n(i.total),
            truncated: i.truncated,
            skipped_rows: cx.skipped,
        }
    }
}

/// A chat's label. Nothing is invented.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export)]
pub enum ChatLabelDto {
    Name { value: ContentTextDto },
    NameUnreadable { state: ValueStateDto },
    Participants { first: Vec<HandleDto>, more: u64 },
    Identifier { value: ContentTextDto },
    Unnamed,
}

/// What a preview or a reply quote shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export)]
pub enum PreviewBodyDto {
    Text {
        pieces: Vec<TextPieceDto>,
    },
    NoText {
        reason: Option<DisplayNoneReasonDto>,
    },
    NoBody,
    Event,
    Reaction {
        association: Option<AssocKindDto>,
    },
    /// List previews are turned off in the viewer settings.
    Hidden,
}

fn preview_body(b: &oa::PreviewBody) -> PreviewBodyDto {
    match b {
        oa::PreviewBody::Text(p) => PreviewBodyDto::Text { pieces: pieces(p) },
        oa::PreviewBody::NoText { reason } => PreviewBodyDto::NoText {
            reason: reason.map(Into::into),
        },
        oa::PreviewBody::NoBody => PreviewBodyDto::NoBody,
        oa::PreviewBody::Event => PreviewBodyDto::Event,
        oa::PreviewBody::Reaction(k) => PreviewBodyDto::Reaction {
            association: k.map(Into::into),
        },
        _ => PreviewBodyDto::NoBody,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PreviewDto {
    pub message: SafeId,
    pub ms: i64,
    pub item: Option<ItemKindDto>,
    pub direction: DirectionDto,
    pub body: PreviewBodyDto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ChatStatsDto {
    pub items_recorded: u64,
    pub undated: u64,
    pub first_ms: Option<i64>,
    pub last_ms: Option<i64>,
}

/// One conversation row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ConversationRowDto {
    pub id: SafeId,
    pub label: ChatLabelDto,
    pub service: Option<ServiceClassDto>,
    pub addresses: u64,
    pub group: bool,
    pub stats: Option<ChatStatsDto>,
    pub stats_stale: bool,
    pub last_dated_ms: Option<i64>,
    pub first_dated2007_ms: Option<i64>,
    pub earlier_dated: CappedCountDto,
    pub undated: CappedCountDto,
    pub empty: bool,
    pub preview: Option<PreviewDto>,
}

impl ConversationRowDto {
    /// `None` when an id is out of range (counted in `cx`). Without
    /// `previews`, the preview's text is replaced by `hidden`.
    pub fn build(c: &oa::Conversation, previews: bool, cx: &mut Conv) -> Option<Self> {
        let label = match &c.label {
            oa::ChatLabel::Name(t) => ChatLabelDto::Name { value: t.into() },
            oa::ChatLabel::NameUnreadable(s) => ChatLabelDto::NameUnreadable { state: (*s).into() },
            oa::ChatLabel::Participants { first, more } => ChatLabelDto::Participants {
                first: first.iter().filter_map(|p| handle(p, cx)).collect(),
                more: n(*more),
            },
            oa::ChatLabel::Identifier(t) => ChatLabelDto::Identifier { value: t.into() },
            _ => ChatLabelDto::Unnamed,
        };
        let preview = match &c.preview {
            None => None,
            Some(p) => Some(PreviewDto {
                message: cx.id(p.message.get())?,
                ms: ms(p.utc_ns),
                item: p.item.map(Into::into),
                direction: p.direction.into(),
                body: match (&p.body, previews) {
                    (oa::PreviewBody::Text(_), false) => PreviewBodyDto::Hidden,
                    (b, _) => preview_body(b),
                },
            }),
        };
        Some(ConversationRowDto {
            id: cx.id(c.id.get())?,
            label,
            service: c.service.map(Into::into),
            addresses: n(c.addresses),
            group: c.group,
            stats: c.stats.map(|s| ChatStatsDto {
                items_recorded: n(s.items_recorded),
                undated: n(s.undated),
                first_ms: s.first_utc_ns.map(ms),
                last_ms: s.last_utc_ns.map(ms),
            }),
            stats_stale: c.stats_stale,
            last_dated_ms: c.last_dated_utc_ns.map(ms),
            first_dated2007_ms: c.first_dated_2007_utc_ns.map(ms),
            earlier_dated: c.earlier_dated.into(),
            undated: c.undated.into(),
            empty: c.empty,
            preview,
        })
    }
}

/// `get_conversations`: the rows asked for, in order; an unknown id is left
/// out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ConversationRowsDto {
    pub rows: Vec<ConversationRowDto>,
    pub skipped_rows: u64,
}

// ---------------------------------------------------------------- messages

/// Who sent a message, as recorded. Never filled in from participants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export)]
pub enum SenderDto {
    FromMe,
    Resolved {
        handle: SafeId,
    },
    NoSender,
    ColumnAbsent,
    Unreadable,
    NotFound,
    /// The direction is not recorded, so the recorded handle may be the
    /// other party: no sender is shown.
    DirectionNotRecorded,
}

/// The backup set a direction value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RecordedByDto {
    pub lineage: SafeId,
    pub session: SafeId,
}

/// A derived label on a message; each sentence says it is derived.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export)]
pub enum MessageLabelDto {
    Undated,
    Unkeyed,
    ScheduleType { value: DecimalString },
    AppContent,
    Effect,
    NotInLatest { lineage: SafeId },
    Provisional,
}

/// "Listed in Recently Deleted in the backup of …".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RecentlyDeletedDto {
    pub session: SafeId,
    pub chat: Option<SafeId>,
    pub not_listed_later: Vec<SafeId>,
}

/// Why an association row is a timeline item, or what it folds into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export)]
pub enum AssocVisibilityDto {
    Undeterminable,
    NoKind,
    NotAReaction,
    TargetUnreadable,
    TargetNotUnique,
    TargetInAnotherChat {
        target: SafeId,
    },
    Folded {
        target: SafeId,
    },
    /// The target is itself a folded reaction: this row stays visible.
    TargetFolds {
        target: SafeId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AssociationDto {
    pub visibility: AssocVisibilityDto,
    pub kind: Option<AssocKindDto>,
    pub shape: Option<AssocTargetShapeDto>,
    pub target_part: Option<DecimalString>,
    pub emoji: Option<OptContentDto>,
}

/// A group event: the handles are listed, no roles are asserted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GroupEventDto {
    pub handles: Vec<SafeId>,
    pub title: Option<OptContentDto>,
    pub item_type: Option<DecimalString>,
    pub group_action_type: Option<DecimalString>,
}

/// What a body segment holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export)]
pub enum SegmentPieceDto {
    Text { value: ContentTextDto },
    Withheld { mask: MaskKindDto },
    InlineEmoji,
    Attachment { attachment: SafeId },
    UnmatchedPlaceholder,
}

/// One run of a body with the same formatting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SegmentDto {
    pub piece: SegmentPieceDto,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub mention: bool,
    pub transcription: bool,
    pub one_time_code: bool,
    pub url: bool,
    pub url_hidden: bool,
}

/// Whether formatting ranges were applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export)]
pub enum FormattingDto {
    Plain,
    Applied,
    NotApplied,
    Unreadable { error: Option<TsErrorDto> },
    TooManySegments,
}

/// A body without text, and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct NoTextDto {
    pub body: SafeId,
    pub reason: Option<DisplayNoneReasonDto>,
    pub attributed: Option<AttributedOutcomeDto>,
    pub attributed_error: Option<TsErrorDto>,
    pub text_column: Option<ValueStateDto>,
}

/// The current body of a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export)]
pub enum BodyViewDto {
    #[serde(rename_all = "camelCase")]
    Text {
        body: SafeId,
        segments: Vec<SegmentDto>,
        formatting: FormattingDto,
        source: Option<DisplaySourceDto>,
        full_utf16: u32,
        truncated: bool,
        bidi_controls: bool,
        invisible: bool,
        lossy: bool,
        stored_as_blob: bool,
        sources_differ: Option<bool>,
    },
    NoText {
        detail: NoTextDto,
    },
    NoBody,
}

/// Why earlier text is withheld (D115).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum WithheldReasonDto {
    Unsent,
    EarlierVersion,
    NoCurrentText,
}

fn withheld(w: oa::WithheldReason) -> WithheldReasonDto {
    match w {
        oa::WithheldReason::Unsent => WithheldReasonDto::Unsent,
        oa::WithheldReason::EarlierVersion => WithheldReasonDto::EarlierVersion,
        _ => WithheldReasonDto::NoCurrentText,
    }
}

/// Earlier body text, per D115: withheld unless the request revealed it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export)]
pub enum EarlierTextDto {
    None,
    Available,
    Withheld {
        reason: WithheldReasonDto,
    },
    /// Its one-time codes are masked unless codes were revealed too.
    Revealed {
        reason: WithheldReasonDto,
        pieces: Vec<TextPieceDto>,
        session: SafeId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct EditFactsDto {
    pub label: Option<EditLabelDto>,
    pub edited_parts: Vec<DecimalString>,
    pub unsent_parts: Vec<DecimalString>,
    pub part_count: Option<u32>,
    pub unsent_whole: bool,
    pub stored_versions: u32,
    pub earlier: EarlierTextDto,
}

/// What a reaction chip is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ReactionKindDto {
    Love,
    Like,
    Dislike,
    Laugh,
    Emphasize,
    Question,
    Emoji,
    Sticker,
}

/// Who a chip's or a reaction row's rows came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export)]
pub enum ChipSenderDto {
    Me,
    Handle { handle: SafeId },
    NotRecorded,
}

/// A chip's state, derived from the recorded rows (D93).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ChipStateDto {
    Shown,
    Hidden,
    Undeterminable,
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ReactionChipDto {
    pub kind: ReactionKindDto,
    pub emoji: Option<OptContentDto>,
    pub sender: ChipSenderDto,
    pub rows: Vec<SafeId>,
    pub state: ChipStateDto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ReactionSummaryDto {
    pub rows: CappedCountDto,
    pub chips: Vec<ReactionChipDto>,
    pub more_chips: u64,
    pub unpaired: Vec<SafeId>,
}

/// A reply's reference to the message it replies to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export)]
pub enum ReplyRefDto {
    Resolved {
        originator: SafeId,
        quote: PreviewBodyDto,
    },
    InAnotherConversation {
        originator: SafeId,
        chat: Option<SafeId>,
        quote: PreviewBodyDto,
    },
    NotUniquelyIdentified,
}

/// An attachment as a page lists it; the card comes from
/// `inspect_attachment`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AttachmentDto {
    pub id: SafeId,
    pub declared_type: OptContentDto,
    pub name: OptContentDto,
    pub sticker: Option<bool>,
    pub declared_bytes: Option<DecimalString>,
    /// A file is stored for it.
    pub stored: bool,
    pub stored_bytes: Option<u64>,
    pub placed_inline: bool,
    /// The media token of the stored file, for `services/media.ts`.
    pub token: Option<MediaToken>,
}

/// One message, assembled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MessageDto {
    pub id: SafeId,
    pub item: Option<ItemKindDto>,
    pub direction: DirectionDto,
    pub direction_recorded_by: Option<RecordedByDto>,
    pub sender: SenderDto,
    pub service: Option<ServiceClassDto>,
    pub date: TimeDto,
    pub body: BodyViewDto,
    pub edit: EditFactsDto,
    pub labels: Vec<MessageLabelDto>,
    pub recently_deleted: Vec<RecentlyDeletedDto>,
    pub association: Option<AssociationDto>,
    pub event: Option<GroupEventDto>,
    pub attachments: Vec<AttachmentDto>,
    pub more_attachments: u64,
    /// The extras (chips, reply reference and count) were read.
    pub extras: bool,
    pub reactions: Option<ReactionSummaryDto>,
    pub reply: Option<ReplyRefDto>,
    pub reply_count: Option<CappedCountDto>,
}

fn segment(s: &oa::Segment, cx: &mut Conv) -> Option<SegmentDto> {
    let piece = match &s.kind {
        oa::SegmentKind::Text(t) => SegmentPieceDto::Text { value: t.into() },
        oa::SegmentKind::Withheld(m) => SegmentPieceDto::Withheld { mask: mask(*m) },
        oa::SegmentKind::InlineEmoji => SegmentPieceDto::InlineEmoji,
        oa::SegmentKind::Attachment(a) => SegmentPieceDto::Attachment {
            attachment: cx.id(a.get())?,
        },
        _ => SegmentPieceDto::UnmatchedPlaceholder,
    };
    Some(SegmentDto {
        piece,
        bold: s.bold,
        italic: s.italic,
        underline: s.underline,
        strikethrough: s.strikethrough,
        mention: s.mention,
        transcription: s.transcription,
        one_time_code: s.one_time_code,
        url: s.url,
        url_hidden: s.url_hidden,
    })
}

fn formatting(f: oa::Formatting) -> FormattingDto {
    match f {
        oa::Formatting::Plain => FormattingDto::Plain,
        oa::Formatting::Applied => FormattingDto::Applied,
        oa::Formatting::NotApplied => FormattingDto::NotApplied,
        oa::Formatting::Unreadable(e) => FormattingDto::Unreadable {
            error: e.map(Into::into),
        },
        oa::Formatting::TooManySegments => FormattingDto::TooManySegments,
        _ => FormattingDto::Unreadable { error: None },
    }
}

fn no_text(t: &oa::NoText, cx: &mut Conv) -> Option<NoTextDto> {
    Some(NoTextDto {
        body: cx.id(t.body.get())?,
        reason: t.reason.map(Into::into),
        attributed: t.attributed.map(Into::into),
        attributed_error: t.attributed_error.map(Into::into),
        text_column: t.text_column.map(Into::into),
    })
}

fn body_view(b: &oa::BodyView, cx: &mut Conv) -> Option<BodyViewDto> {
    Some(match b {
        oa::BodyView::Text(t) => BodyViewDto::Text {
            body: cx.id(t.body.get())?,
            segments: t
                .segments
                .iter()
                .map(|s| segment(s, cx))
                .collect::<Option<Vec<_>>>()?,
            formatting: formatting(t.formatting),
            source: t.source.map(Into::into),
            full_utf16: t.full_utf16,
            truncated: t.truncated,
            bidi_controls: t.bidi_controls,
            invisible: t.invisible,
            lossy: t.lossy,
            stored_as_blob: t.stored_as_blob,
            sources_differ: t.sources_differ,
        },
        oa::BodyView::NoText(t) => BodyViewDto::NoText {
            detail: no_text(t, cx)?,
        },
        _ => BodyViewDto::NoBody,
    })
}

fn earlier(e: &oa::EarlierText, cx: &mut Conv) -> Option<EarlierTextDto> {
    Some(match e {
        oa::EarlierText::None => EarlierTextDto::None,
        oa::EarlierText::Available => EarlierTextDto::Available,
        oa::EarlierText::Withheld(w) => EarlierTextDto::Withheld {
            reason: withheld(*w),
        },
        oa::EarlierText::Revealed {
            reason,
            text,
            session,
        } => EarlierTextDto::Revealed {
            reason: withheld(*reason),
            pieces: pieces(text),
            session: cx.id(*session)?,
        },
        _ => EarlierTextDto::Withheld {
            reason: WithheldReasonDto::NoCurrentText,
        },
    })
}

fn chip_sender(s: oa::ChipSender, cx: &mut Conv) -> Option<ChipSenderDto> {
    Some(match s {
        oa::ChipSender::Me => ChipSenderDto::Me,
        oa::ChipSender::Handle(h) => ChipSenderDto::Handle {
            handle: cx.id(h.get())?,
        },
        _ => ChipSenderDto::NotRecorded,
    })
}

fn reaction_kind(k: oa::ReactionKind) -> ReactionKindDto {
    match k {
        oa::ReactionKind::Love => ReactionKindDto::Love,
        oa::ReactionKind::Like => ReactionKindDto::Like,
        oa::ReactionKind::Dislike => ReactionKindDto::Dislike,
        oa::ReactionKind::Laugh => ReactionKindDto::Laugh,
        oa::ReactionKind::Emphasize => ReactionKindDto::Emphasize,
        oa::ReactionKind::Question => ReactionKindDto::Question,
        oa::ReactionKind::Emoji => ReactionKindDto::Emoji,
        _ => ReactionKindDto::Sticker,
    }
}

fn chip_state(s: oa::ChipState) -> ChipStateDto {
    match s {
        oa::ChipState::Shown => ChipStateDto::Shown,
        oa::ChipState::Hidden => ChipStateDto::Hidden,
        oa::ChipState::Added => ChipStateDto::Added,
        oa::ChipState::Removed => ChipStateDto::Removed,
        _ => ChipStateDto::Undeterminable,
    }
}

fn reactions(r: &oa::ReactionSummary, cx: &mut Conv) -> ReactionSummaryDto {
    ReactionSummaryDto {
        rows: r.rows.into(),
        chips: r
            .chips
            .iter()
            .filter_map(|c| {
                Some(ReactionChipDto {
                    kind: reaction_kind(c.kind),
                    emoji: c.emoji.as_ref().map(Into::into),
                    sender: chip_sender(c.sender, cx)?,
                    rows: cx.ids(c.rows.iter().map(|m| m.get())),
                    state: chip_state(c.state),
                })
            })
            .collect(),
        more_chips: n(r.more_chips),
        unpaired: cx.ids(r.unpaired.iter().map(|m| m.get())),
    }
}

fn reply(r: &oa::ReplyRef, cx: &mut Conv) -> Option<ReplyRefDto> {
    Some(match r {
        oa::ReplyRef::Resolved { originator, quote } => ReplyRefDto::Resolved {
            originator: cx.id(originator.get())?,
            quote: preview_body(quote),
        },
        oa::ReplyRef::InAnotherConversation {
            originator,
            chat,
            quote,
        } => ReplyRefDto::InAnotherConversation {
            originator: cx.id(originator.get())?,
            chat: cx.opt_id(chat.map(|c| c.get()))?,
            quote: preview_body(quote),
        },
        _ => ReplyRefDto::NotUniquelyIdentified,
    })
}

fn label(l: oa::MessageLabel, cx: &mut Conv) -> Option<MessageLabelDto> {
    Some(match l {
        oa::MessageLabel::Undated => MessageLabelDto::Undated,
        oa::MessageLabel::Unkeyed => MessageLabelDto::Unkeyed,
        oa::MessageLabel::ScheduleType(v) => MessageLabelDto::ScheduleType {
            value: DecimalString::of(v),
        },
        oa::MessageLabel::AppContent => MessageLabelDto::AppContent,
        oa::MessageLabel::Effect => MessageLabelDto::Effect,
        oa::MessageLabel::NotInLatest(l) => MessageLabelDto::NotInLatest {
            lineage: cx.id(l.get())?,
        },
        oa::MessageLabel::Provisional => MessageLabelDto::Provisional,
        _ => return None,
    })
}

fn visibility(v: oa::AssocVisibility, cx: &mut Conv) -> Option<AssocVisibilityDto> {
    Some(match v {
        oa::AssocVisibility::Undeterminable => AssocVisibilityDto::Undeterminable,
        oa::AssocVisibility::NoKind => AssocVisibilityDto::NoKind,
        oa::AssocVisibility::NotAReaction => AssocVisibilityDto::NotAReaction,
        oa::AssocVisibility::TargetUnreadable => AssocVisibilityDto::TargetUnreadable,
        oa::AssocVisibility::TargetNotUnique => AssocVisibilityDto::TargetNotUnique,
        oa::AssocVisibility::TargetInAnotherChat(m) => AssocVisibilityDto::TargetInAnotherChat {
            target: cx.id(m.get())?,
        },
        oa::AssocVisibility::Folded(m) => AssocVisibilityDto::Folded {
            target: cx.id(m.get())?,
        },
        oa::AssocVisibility::TargetFolds(m) => AssocVisibilityDto::TargetFolds {
            target: cx.id(m.get())?,
        },
        _ => AssocVisibilityDto::Undeterminable,
    })
}

fn attachment_ref(a: &oa::AttachmentRef, cx: &mut Conv) -> Option<AttachmentDto> {
    Some(AttachmentDto {
        id: cx.id(a.id.get())?,
        declared_type: (&a.declared_type).into(),
        name: (&a.name).into(),
        sticker: a.sticker,
        declared_bytes: dec(a.declared_bytes),
        stored: a.blob.is_some(),
        stored_bytes: a.stored_bytes.and_then(|b| u64::try_from(b).ok()).map(n),
        placed_inline: a.placed_inline,
        token: None,
    })
}

impl MessageDto {
    /// `None` when an id the message needs is out of range (counted).
    pub fn build(m: &oa::AssembledMessage, cx: &mut Conv) -> Option<MessageDto> {
        let sender = match m.sender {
            oa::SenderReason::FromMe => SenderDto::FromMe,
            oa::SenderReason::Resolved(h) => SenderDto::Resolved {
                handle: cx.id(h.get())?,
            },
            oa::SenderReason::NoSender => SenderDto::NoSender,
            oa::SenderReason::ColumnAbsent => SenderDto::ColumnAbsent,
            oa::SenderReason::NotFound => SenderDto::NotFound,
            oa::SenderReason::DirectionNotRecorded => SenderDto::DirectionNotRecorded,
            _ => SenderDto::Unreadable,
        };
        let recorded_by = match m.direction_recorded_by {
            None => None,
            Some(r) => Some(RecordedByDto {
                lineage: cx.id(r.lineage.get())?,
                session: cx.id(r.session)?,
            }),
        };
        let association = match &m.association {
            None => None,
            Some(a) => Some(AssociationDto {
                visibility: visibility(a.visibility, cx)?,
                kind: a.kind.map(Into::into),
                shape: a.shape.map(Into::into),
                target_part: dec(a.target_part),
                emoji: a.emoji.as_ref().map(Into::into),
            }),
        };
        let event = m.event.as_ref().map(|e| GroupEventDto {
            handles: cx.ids(e.handles.iter().map(|h| h.get())),
            title: e.title.as_ref().map(Into::into),
            item_type: dec(e.item_type),
            group_action_type: dec(e.group_action_type),
        });
        let recently_deleted = m
            .recently_deleted
            .iter()
            .filter_map(|r| {
                Some(RecentlyDeletedDto {
                    session: cx.id(r.session)?,
                    chat: cx.opt_id(r.chat.map(|c| c.get()))?,
                    not_listed_later: cx.ids(r.not_listed_later.iter().copied()),
                })
            })
            .collect();
        let reply_ref = match &m.reply {
            None => None,
            Some(r) => Some(reply(r, cx)?),
        };
        Some(MessageDto {
            id: cx.id(m.id.get())?,
            item: m.item.map(Into::into),
            direction: m.direction.into(),
            direction_recorded_by: recorded_by,
            sender,
            service: m.service.map(Into::into),
            date: (&m.date).into(),
            body: body_view(&m.body, cx)?,
            edit: EditFactsDto {
                label: m.edit.label.map(Into::into),
                edited_parts: m
                    .edit
                    .edited_parts
                    .iter()
                    .map(|p| DecimalString::of(*p))
                    .collect(),
                unsent_parts: m
                    .edit
                    .unsent_parts
                    .iter()
                    .map(|p| DecimalString::of(*p))
                    .collect(),
                part_count: m.edit.part_count,
                unsent_whole: m.edit.unsent_whole,
                stored_versions: m.edit.stored_versions,
                earlier: earlier(&m.edit.earlier, cx)?,
            },
            labels: m.labels.iter().filter_map(|l| label(*l, cx)).collect(),
            recently_deleted,
            association,
            event,
            attachments: m
                .attachments
                .iter()
                .filter_map(|a| attachment_ref(a, cx))
                .collect(),
            more_attachments: n(m.more_attachments),
            extras: m.extras,
            reactions: m.reactions.as_ref().map(|r| reactions(r, cx)),
            reply: reply_ref,
            reply_count: m.reply_count.map(Into::into),
        })
    }
}

// ------------------------------------------------------------------ pages

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum PageRangeDto {
    Dated,
    Undated,
    Replies,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum FocusReasonDto {
    Anchor,
    ReactionTarget,
    NextVisible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct FocusDto {
    pub id: SafeId,
    pub reason: FocusReasonDto,
}

/// `get_thread_page` and `get_reply_thread`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ThreadPageDto {
    /// Visible items, oldest first.
    pub items: Vec<MessageDto>,
    pub handles: Vec<HandleDto>,
    pub handles_capped: bool,
    pub range: PageRangeDto,
    pub older_anchor: Option<SafeId>,
    pub newer_anchor: Option<SafeId>,
    pub has_older: bool,
    pub has_newer: bool,
    /// The chat's undated rows (dated pages only).
    pub undated_count: Option<CappedCountDto>,
    pub partial: bool,
    pub extras_incomplete: bool,
    pub anchor_moved: bool,
    pub focus: Option<FocusDto>,
    /// The page stopped before its serialised-size bound.
    pub cut_for_size: bool,
    pub skipped_rows: u64,
}

impl ThreadPageDto {
    pub fn build(p: &oa::ThreadPage) -> ThreadPageDto {
        let mut cx = Conv::default();
        let items = p
            .items
            .iter()
            .filter_map(|m| MessageDto::build(m, &mut cx))
            .collect();
        let handles = p
            .handles
            .iter()
            .filter_map(|h| handle(h, &mut cx))
            .collect();
        let range = match p.range {
            oa::PageRange::Dated => PageRangeDto::Dated,
            oa::PageRange::Undated => PageRangeDto::Undated,
            _ => PageRangeDto::Replies,
        };
        let anchor = |cx: &mut Conv, m: Option<oa::MessageId>| m.and_then(|m| cx.id(m.get()));
        let older_anchor = anchor(&mut cx, p.older_anchor);
        let newer_anchor = anchor(&mut cx, p.newer_anchor);
        let focus = p.focus.and_then(|f| {
            Some(FocusDto {
                id: cx.id(f.id.get())?,
                reason: match f.reason {
                    oa::FocusReason::Anchor => FocusReasonDto::Anchor,
                    oa::FocusReason::ReactionTarget => FocusReasonDto::ReactionTarget,
                    _ => FocusReasonDto::NextVisible,
                },
            })
        });
        ThreadPageDto {
            items,
            handles,
            handles_capped: p.handles_capped,
            range,
            older_anchor,
            newer_anchor,
            has_older: p.has_older,
            has_newer: p.has_newer,
            undated_count: p.undated.map(Into::into),
            partial: p.partial,
            extras_incomplete: p.extras_incomplete,
            anchor_moved: p.anchor_moved,
            focus,
            cut_for_size: p.cut_for_size,
            skipped_rows: cx.skipped,
        }
    }
}

// ----------------------------------------------------------------- detail

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RawValueDto {
    pub value: Option<DecimalString>,
    pub state: Option<ValueStateDto>,
}

impl From<oa::RawValue> for RawValueDto {
    fn from(v: oa::RawValue) -> Self {
        RawValueDto {
            value: dec(v.value),
            state: v.state.map(Into::into),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum HistoryColumnDto {
    Subject,
    GroupTitle,
    DestinationCallerId,
    ChatDisplayName,
    TransferName,
    EmojiDescription,
}

/// An earlier value of a column, from `value_history`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct HistoryValueDto {
    pub column: HistoryColumnDto,
    pub value: OptContentDto,
    pub first_session: SafeId,
}

fn history(h: &[oa::HistoryValue], cx: &mut Conv) -> Vec<HistoryValueDto> {
    h.iter()
        .filter_map(|v| {
            Some(HistoryValueDto {
                column: match v.column {
                    oa::HistoryColumn::Subject => HistoryColumnDto::Subject,
                    oa::HistoryColumn::GroupTitle => HistoryColumnDto::GroupTitle,
                    oa::HistoryColumn::DestinationCallerId => HistoryColumnDto::DestinationCallerId,
                    oa::HistoryColumn::ChatDisplayName => HistoryColumnDto::ChatDisplayName,
                    oa::HistoryColumn::TransferName => HistoryColumnDto::TransferName,
                    oa::HistoryColumn::EmojiDescription => HistoryColumnDto::EmojiDescription,
                    _ => return None,
                },
                value: (&v.value).into(),
                first_session: cx.id(v.first_session)?,
            })
        })
        .collect()
}

/// A stored body version's text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export)]
pub enum VersionTextDto {
    Text { pieces: Vec<TextPieceDto> },
    Withheld { reason: WithheldReasonDto },
    NoText { detail: NoTextDto },
}

/// A raw number from an edit entry.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export)]
pub enum RawNumberDto {
    Integer {
        value: DecimalString,
    },
    /// `None` for a value that is not finite.
    Real {
        value: Option<f64>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct EditTimeDto {
    pub kind: Option<SummaryTimeKindDto>,
    pub raw: Option<RawNumberDto>,
    pub ms: Option<i64>,
}

/// An edit entry's text, decoded at read time (owner Q3a), withheld unless
/// revealed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export)]
pub enum EditEntryTextDto {
    Text { pieces: Vec<TextPieceDto> },
    Withheld { reason: WithheldReasonDto },
    NoBody,
    Failed { error: TsErrorDto },
    NotMatched,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct EditEntryDto {
    pub part: DecimalString,
    pub ordinal: DecimalString,
    pub at: EditTimeDto,
    pub text: EditEntryTextDto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum EditTextsOutcomeDto {
    NotNeeded,
    Decoded,
    Unmatched,
    SummaryUnreadable,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct BodyVersionDto {
    pub body: SafeId,
    pub current: bool,
    pub first_session: SafeId,
    pub last_session: SafeId,
    pub text: VersionTextDto,
    pub text_unchanged: bool,
    pub edit_label: Option<EditLabelDto>,
    pub edits: Vec<EditEntryDto>,
    pub more_edits: u64,
    pub edit_texts: EditTextsOutcomeDto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DetailTimesDto {
    pub date: TimeDto,
    pub edited: TimeDto,
    pub retracted: TimeDto,
}

/// A conversation the message is in, with its raw flags ("meaning
/// unverified").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ChatFlagsDto {
    pub chat: SafeId,
    pub style: RawValueDto,
    pub is_filtered: RawValueDto,
    pub is_archived: RawValueDto,
    pub history: Vec<HistoryValueDto>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum IdentifierProbeDto {
    NotInArchive,
    SharedByUnkeyed,
    NotChecked,
}

fn probe(p: oa::IdentifierProbe) -> IdentifierProbeDto {
    match p {
        oa::IdentifierProbe::NotInArchive => IdentifierProbeDto::NotInArchive,
        oa::IdentifierProbe::SharedByUnkeyed => IdentifierProbeDto::SharedByUnkeyed,
        _ => IdentifierProbeDto::NotChecked,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct OlderBackupDto {
    pub held_by: SafeId,
    pub newest: SafeId,
}

/// An attachment's card (§5.9). `file` is refined by `inspect_attachment`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export)]
pub enum CardStateDto {
    #[serde(rename_all = "camelCase")]
    File {
        older_backup: Option<OlderBackupDto>,
    },
    NoObservation,
    NotInBackup,
    NotFoundAtAssumedKey,
    NotCopied,
    ClassNotLookedUp,
    LocationUnusable {
        lookup: LookupDto,
    },
    NotDecrypted,
    CouldNotBeRead,
    TooLarge,
    Unavailable {
        lookup: DecimalString,
        copy: DecimalString,
    },
}

fn card(c: &oa::CardState, cx: &mut Conv) -> Option<CardStateDto> {
    Some(match c {
        oa::CardState::File { older_backup } => CardStateDto::File {
            older_backup: match older_backup {
                None => None,
                Some(o) => Some(OlderBackupDto {
                    held_by: cx.id(o.held_by)?,
                    newest: cx.id(o.newest)?,
                }),
            },
        },
        oa::CardState::NoObservation => CardStateDto::NoObservation,
        oa::CardState::NotInBackup => CardStateDto::NotInBackup,
        oa::CardState::NotFoundAtAssumedKey => CardStateDto::NotFoundAtAssumedKey,
        oa::CardState::NotCopied => CardStateDto::NotCopied,
        oa::CardState::ClassNotLookedUp => CardStateDto::ClassNotLookedUp,
        oa::CardState::LocationUnusable(l) => CardStateDto::LocationUnusable {
            lookup: (*l).into(),
        },
        oa::CardState::NotDecrypted => CardStateDto::NotDecrypted,
        oa::CardState::CouldNotBeRead => CardStateDto::CouldNotBeRead,
        oa::CardState::TooLarge => CardStateDto::TooLarge,
        oa::CardState::Unavailable { lookup, copy } => CardStateDto::Unavailable {
            lookup: DecimalString::of(*lookup),
            copy: DecimalString::of(*copy),
        },
        _ => CardStateDto::CouldNotBeRead,
    })
}

/// One backup's observation of an attachment, as codes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ObservationDto {
    pub session: SafeId,
    pub lookup: Option<LookupDto>,
    pub lookup_raw: DecimalString,
    pub copy: Option<CopyDto>,
    pub copy_raw: DecimalString,
    pub path_class: Option<PathClassDto>,
    pub stored: bool,
    pub size_matched_manifest: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct StoredFileDto {
    pub stored_bytes: u64,
    pub held_by: SafeId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct VerifyFactsDto {
    pub state: Option<BlobVerifyStateDto>,
    pub raw: DecimalString,
    pub verified_ms: Option<i64>,
    pub may_be_stale: bool,
}

/// An attachment's facts and card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AttachmentFactsDto {
    pub id: SafeId,
    pub declared_type: OptContentDto,
    pub uti: OptContentDto,
    pub name: OptContentDto,
    pub declared_bytes: RawValueDto,
    pub sticker: RawValueDto,
    /// Raw, "meaning unverified".
    pub hide_attachment: RawValueDto,
    pub emoji_description: OptContentDto,
    pub file: Option<StoredFileDto>,
    pub observations: Vec<ObservationDto>,
    pub card: CardStateDto,
    pub verify: Option<VerifyFactsDto>,
    pub history: Vec<HistoryValueDto>,
}

impl AttachmentFactsDto {
    pub fn build(a: &oa::AttachmentFacts, cx: &mut Conv) -> Option<AttachmentFactsDto> {
        let file = match a.blob {
            None => None,
            Some(b) => Some(StoredFileDto {
                stored_bytes: n(u64::try_from(b.stored_bytes).unwrap_or(0)),
                held_by: cx.id(b.held_by)?,
            }),
        };
        Some(AttachmentFactsDto {
            id: cx.id(a.id.get())?,
            declared_type: (&a.declared_type).into(),
            uti: (&a.uti).into(),
            name: (&a.name).into(),
            declared_bytes: a.declared_bytes.into(),
            sticker: a.sticker.into(),
            hide_attachment: a.hide_attachment.into(),
            emoji_description: (&a.emoji_description).into(),
            file,
            observations: a
                .observations
                .iter()
                .filter_map(|o| {
                    Some(ObservationDto {
                        session: cx.id(o.session)?,
                        lookup: o.lookup.map(Into::into),
                        lookup_raw: DecimalString::of(o.lookup_raw),
                        copy: o.copy.map(Into::into),
                        copy_raw: DecimalString::of(o.copy_raw),
                        path_class: o.path_class.map(Into::into),
                        stored: o.blob.is_some(),
                        size_matched_manifest: o.size_matched_manifest,
                    })
                })
                .collect(),
            card: card(&a.card, cx)?,
            verify: a.verify.map(|v| VerifyFactsDto {
                state: v.state.map(Into::into),
                raw: DecimalString::of(v.raw),
                verified_ms: v.verified_at_ns.map(ms),
                may_be_stale: v.may_be_stale,
            }),
            history: history(&a.history, cx),
        })
    }
}

/// One reaction row on a target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ReactionRecordDto {
    pub id: SafeId,
    pub kind: Option<AssocKindDto>,
    pub adds: bool,
    pub emoji: Option<OptContentDto>,
    pub sender: ChipSenderDto,
    pub time: TimeDto,
    pub target_part: Option<DecimalString>,
}

/// A page of the reaction rows recorded on a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ReactionHistoryDto {
    pub target: SafeId,
    pub keyed: bool,
    pub rows: Vec<ReactionRecordDto>,
    pub total: CappedCountDto,
    /// More rows follow: ask for them with `reactionPage.after` set to the
    /// last row's id.
    pub has_more: bool,
}

impl ReactionHistoryDto {
    pub fn build(h: &oa::ReactionHistory, cx: &mut Conv) -> Option<ReactionHistoryDto> {
        Some(ReactionHistoryDto {
            target: cx.id(h.target.get())?,
            keyed: h.keyed,
            rows: h
                .rows
                .iter()
                .filter_map(|r| {
                    Some(ReactionRecordDto {
                        id: cx.id(r.id.get())?,
                        kind: r.kind.map(Into::into),
                        adds: r.adds,
                        emoji: r.emoji.as_ref().map(Into::into),
                        sender: chip_sender(r.sender, cx)?,
                        time: (&r.time).into(),
                        target_part: dec(r.target_part),
                    })
                })
                .collect(),
            total: h.total.into(),
            has_more: h.has_more,
        })
    }
}

/// `get_message_detail`.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MessageDetailDto {
    pub message: MessageDto,
    pub versions: Vec<BodyVersionDto>,
    pub more_versions: u64,
    pub times: DetailTimesDto,
    /// `destination_caller_id`: "Account (meaning unverified)".
    pub account: OptContentDto,
    pub subject: OptContentDto,
    pub history: Vec<HistoryValueDto>,
    pub chats: Vec<ChatFlagsDto>,
    pub attachments: Vec<AttachmentFactsDto>,
    pub reply_probe: Option<IdentifierProbeDto>,
    pub reaction_target_probe: Option<IdentifierProbeDto>,
    /// The page of the reaction history asked for with `reactionPage`.
    pub reactions: Option<ReactionHistoryDto>,
    /// Versions left out to keep the reply under its size bound.
    pub cut_for_size: bool,
    pub skipped_rows: u64,
}

fn edit_entry(e: &oa::EditEntryView) -> EditEntryDto {
    EditEntryDto {
        part: DecimalString::of(e.part),
        ordinal: DecimalString::of(e.ordinal),
        at: EditTimeDto {
            kind: e.at.kind.map(Into::into),
            raw: e.at.raw.map(|r| match r {
                oa::RawNumber::Integer(i) => RawNumberDto::Integer {
                    value: DecimalString::of(i),
                },
                oa::RawNumber::Real(f) => RawNumberDto::Real {
                    value: f.is_finite().then_some(f),
                },
                _ => RawNumberDto::Real { value: None },
            }),
            ms: e.at.utc_ns.map(ms),
        },
        text: match &e.text {
            oa::EditEntryText::Text(p) => EditEntryTextDto::Text { pieces: pieces(p) },
            oa::EditEntryText::Withheld(w) => EditEntryTextDto::Withheld {
                reason: withheld(*w),
            },
            oa::EditEntryText::NoBody => EditEntryTextDto::NoBody,
            oa::EditEntryText::Failed(e) => EditEntryTextDto::Failed { error: (*e).into() },
            _ => EditEntryTextDto::NotMatched,
        },
    }
}

fn version(v: &oa::BodyVersion, cx: &mut Conv) -> Option<BodyVersionDto> {
    Some(BodyVersionDto {
        body: cx.id(v.body.get())?,
        current: v.current,
        first_session: cx.id(v.first_session)?,
        last_session: cx.id(v.last_session)?,
        text: match &v.text {
            oa::VersionText::Text(p) => VersionTextDto::Text { pieces: pieces(p) },
            oa::VersionText::Withheld(w) => VersionTextDto::Withheld {
                reason: withheld(*w),
            },
            oa::VersionText::NoText(t) => VersionTextDto::NoText {
                detail: no_text(t, cx)?,
            },
            _ => VersionTextDto::Withheld {
                reason: WithheldReasonDto::NoCurrentText,
            },
        },
        text_unchanged: v.text_unchanged,
        edit_label: v.edit_label.map(Into::into),
        edits: v.edits.iter().map(edit_entry).collect(),
        more_edits: n(v.more_edits),
        edit_texts: match v.edit_texts {
            oa::EditTextsOutcome::NotNeeded => EditTextsOutcomeDto::NotNeeded,
            oa::EditTextsOutcome::Decoded => EditTextsOutcomeDto::Decoded,
            oa::EditTextsOutcome::Unmatched => EditTextsOutcomeDto::Unmatched,
            _ => EditTextsOutcomeDto::SummaryUnreadable,
        },
    })
}

impl MessageDetailDto {
    /// `None` when the message's own id is out of range.
    pub fn build(
        d: &oa::MessageDetail,
        reactions: Option<&oa::ReactionHistory>,
    ) -> Option<MessageDetailDto> {
        let mut cx = Conv::default();
        let message = MessageDto::build(&d.message, &mut cx)?;
        let versions = d
            .versions
            .iter()
            .filter_map(|v| version(v, &mut cx))
            .collect();
        let chats = d
            .chats
            .iter()
            .filter_map(|c| {
                let chat = cx.id(c.chat.get())?;
                Some(ChatFlagsDto {
                    chat,
                    style: c.style.into(),
                    is_filtered: c.is_filtered.into(),
                    is_archived: c.is_archived.into(),
                    history: history(&c.history, &mut cx),
                })
            })
            .collect();
        let attachments = d
            .attachments
            .iter()
            .filter_map(|a| AttachmentFactsDto::build(a, &mut cx))
            .collect();
        let reactions = reactions.and_then(|h| ReactionHistoryDto::build(h, &mut cx));
        let history = history(&d.history, &mut cx);
        Some(MessageDetailDto {
            message,
            versions,
            more_versions: n(d.more_versions),
            times: DetailTimesDto {
                date: (&d.times.date).into(),
                edited: (&d.times.edited).into(),
                retracted: (&d.times.retracted).into(),
            },
            account: (&d.account).into(),
            subject: (&d.subject).into(),
            history,
            chats,
            attachments,
            reply_probe: d.reply_probe.map(probe),
            reaction_target_probe: d.reaction_target_probe.map(probe),
            reactions,
            cut_for_size: false,
            skipped_rows: cx.skipped,
        })
    }
}

/// A chunk of a body's text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export)]
pub enum ChunkTextDto {
    Text { pieces: Vec<TextPieceDto> },
    Withheld { reason: WithheldReasonDto },
    NoText,
}

/// `get_message_text`: offsets are UTF-16 units, clamped in Rust.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TextChunkDto {
    pub body: SafeId,
    pub from_utf16: u32,
    pub end_utf16: u32,
    pub full_utf16: u32,
    pub text: ChunkTextDto,
}

impl TextChunkDto {
    pub fn build(c: &oa::TextChunk) -> Option<TextChunkDto> {
        Some(TextChunkDto {
            body: SafeId::new(c.body.get())?,
            from_utf16: c.from_utf16,
            end_utf16: c.end_utf16,
            full_utf16: c.full_utf16,
            text: match &c.text {
                oa::ChunkText::Text(p) => ChunkTextDto::Text { pieces: pieces(p) },
                oa::ChunkText::Withheld(w) => ChunkTextDto::Withheld {
                    reason: withheld(*w),
                },
                _ => ChunkTextDto::NoText,
            },
        })
    }
}

/// `get_day_counts`: visible items per interval (folded reactions and
/// undated rows are never counted).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DayCountsDto {
    pub counts: Vec<u64>,
}

/// `seek_date`: a visible item, or none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SeekResultDto {
    pub message_id: Option<SafeId>,
}

// ----------------------------------------------------------------- search

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum SearchRanDto {
    Words,
    Exact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum WordsPlanDto {
    Global,
    Scoped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ScanStopDto {
    ExaminedLimit,
    MatchCap,
    TooBroad,
}

/// What an exact-text scan covered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ScanCoverageDto {
    pub examined: u64,
    pub complete: bool,
    pub stop: Option<ScanStopDto>,
}

/// Progress of an exact-text scan, pushed on the search's channel. It
/// names the running search, so `cancel_search` can stop a scan before it
/// returns its `SearchStartedDto` (slice 5i's Cancel).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ScanProgressDto {
    pub search_id: u32,
    pub examined: u64,
    pub matched: u64,
}

impl ScanProgressDto {
    pub fn new(search_id: u32, p: oa::ScanProgress) -> Self {
        ScanProgressDto {
            search_id,
            examined: n(p.examined),
            matched: n(p.matched),
        }
    }
}

/// `search_messages`: the result set is kept in the session under
/// `searchId`; its hits are read with `get_search_results`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SearchStartedDto {
    pub search_id: u32,
    pub total: u64,
    /// The first `dated` results are dated; the rest are undated.
    pub dated: u64,
    pub capped: bool,
    /// When capped: whether the kept results are the newest.
    pub newest_kept: bool,
    pub mode: SearchRanDto,
    pub plan: Option<WordsPlanDto>,
    pub coverage: Option<ScanCoverageDto>,
}

impl SearchStartedDto {
    pub fn build(search_id: u32, s: &oa::SearchResultSet) -> SearchStartedDto {
        SearchStartedDto {
            search_id,
            total: len_of(s.entries.len()),
            dated: len_of(s.dated),
            capped: s.capped,
            newest_kept: s.newest_kept,
            mode: match s.ran {
                oa::SearchRan::Words => SearchRanDto::Words,
                _ => SearchRanDto::Exact,
            },
            plan: s.plan.map(|p| match p {
                oa::WordsPlan::Global => WordsPlanDto::Global,
                _ => WordsPlanDto::Scoped,
            }),
            coverage: s.coverage.map(|c| ScanCoverageDto {
                examined: n(c.examined),
                complete: c.complete,
                stop: c.stop.map(|s| match s {
                    oa::ScanStop::ExaminedLimit => ScanStopDto::ExaminedLimit,
                    oa::ScanStop::MatchCap => ScanStopDto::MatchCap,
                    _ => ScanStopDto::TooBroad,
                }),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum HitVersionDto {
    Current,
    Earlier,
    Unsent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum NoHighlightDto {
    MarkerInText,
    Mismatch,
    Budget,
    NotFound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export)]
pub enum HighlightDto {
    Shown,
    NotShown { why: NoHighlightDto },
}

/// A piece of a hit's snippet; `hit` marks a match (wrapped in `<mark>`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export)]
pub enum HitPieceDto {
    Text { value: ContentTextDto, hit: bool },
    Masked { mask: MaskKindDto },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct HitTextDto {
    pub pieces: Vec<HitPieceDto>,
    pub cut_start: bool,
    pub cut_end: bool,
    pub highlight: HighlightDto,
    pub matched_hidden: bool,
    pub full_utf16: u32,
}

/// One search result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SearchHitDto {
    pub message: SafeId,
    pub body: SafeId,
    pub chat: Option<SafeId>,
    pub other_chats: CappedCountDto,
    pub ms: Option<i64>,
    pub direction: DirectionDto,
    pub item: Option<ItemKindDto>,
    pub association: Option<AssocKindDto>,
    pub version: HitVersionDto,
    pub text: Option<HitTextDto>,
}

impl SearchHitDto {
    pub fn build(h: &oa::SearchHit, cx: &mut Conv) -> Option<SearchHitDto> {
        Some(SearchHitDto {
            message: cx.id(h.message.get())?,
            body: cx.id(h.body.get())?,
            chat: cx.opt_id(h.chat.map(|c| c.get()))?,
            other_chats: h.other_chats.into(),
            ms: h.utc_ns.map(ms),
            direction: h.direction.into(),
            item: h.item.map(Into::into),
            association: h.association.map(Into::into),
            version: match h.version {
                oa::HitVersion::Current => HitVersionDto::Current,
                oa::HitVersion::Earlier => HitVersionDto::Earlier,
                _ => HitVersionDto::Unsent,
            },
            text: h.text.as_ref().map(|t| HitTextDto {
                pieces: t
                    .pieces
                    .iter()
                    .map(|p| match p {
                        oa::HitPiece::Text { text, hit } => HitPieceDto::Text {
                            value: text.into(),
                            hit: *hit,
                        },
                        oa::HitPiece::Masked(m) => HitPieceDto::Masked { mask: mask(*m) },
                        _ => HitPieceDto::Masked {
                            mask: MaskKindDto::OneTimeCode,
                        },
                    })
                    .collect(),
                cut_start: t.cut_start,
                cut_end: t.cut_end,
                highlight: match t.highlight {
                    oa::Highlight::Shown => HighlightDto::Shown,
                    oa::Highlight::NotShown(w) => HighlightDto::NotShown {
                        why: match w {
                            oa::NoHighlight::MarkerInText => NoHighlightDto::MarkerInText,
                            oa::NoHighlight::Mismatch => NoHighlightDto::Mismatch,
                            oa::NoHighlight::Budget => NoHighlightDto::Budget,
                            _ => NoHighlightDto::NotFound,
                        },
                    },
                    _ => HighlightDto::NotShown {
                        why: NoHighlightDto::NotFound,
                    },
                },
                matched_hidden: t.matched_hidden,
                full_utf16: t.full_utf16,
            }),
        })
    }
}

/// `get_search_results`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SearchHitsDto {
    pub hits: Vec<SearchHitDto>,
    pub skipped_rows: u64,
}

// ------------------------------------------------------------ attachments

/// What the bytes are (§5.2), decided from the first 4 KiB, never from the
/// declared type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum SniffedKindDto {
    Gif,
    Webp,
    Png,
    Jpeg,
    Bmp,
    Ico,
    QuickTime,
    M4a,
    M4v,
    Heif,
    Mp4,
    IsoBmffOther,
    Webm,
    Wav,
    Aiff,
    Ogg,
    Mp3,
    Caf,
    Pdf,
    Xml,
    Html,
    Vcard,
    Text,
    Unknown,
}

fn sniffed_kind(k: oa::SniffedKind) -> SniffedKindDto {
    use oa::SniffedKind as K;
    use SniffedKindDto as D;
    match k {
        K::Gif => D::Gif,
        K::Webp => D::Webp,
        K::Png => D::Png,
        K::Jpeg => D::Jpeg,
        K::Bmp => D::Bmp,
        K::Ico => D::Ico,
        K::QuickTime => D::QuickTime,
        K::M4a => D::M4a,
        K::M4v => D::M4v,
        K::Heif => D::Heif,
        K::Mp4 => D::Mp4,
        K::IsoBmffOther => D::IsoBmffOther,
        K::Webm => D::Webm,
        K::Wav => D::Wav,
        K::Aiff => D::Aiff,
        K::Ogg => D::Ogg,
        K::Mp3 => D::Mp3,
        K::Caf => D::Caf,
        K::Pdf => D::Pdf,
        K::Xml => D::Xml,
        K::Html => D::Html,
        K::Vcard => D::Vcard,
        K::Text => D::Text,
        _ => D::Unknown,
    }
}

/// What the viewer offers for a kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export)]
pub enum MediaClassDto {
    Image,
    #[serde(rename_all = "camelCase")]
    Playable {
        structure_checked: bool,
    },
    File,
    Unrecognised,
}

fn media_class(c: oa::MediaClass) -> MediaClassDto {
    match c {
        oa::MediaClass::Image => MediaClassDto::Image,
        oa::MediaClass::Playable { structure_checked } => {
            MediaClassDto::Playable { structure_checked }
        }
        oa::MediaClass::File => MediaClassDto::File,
        _ => MediaClassDto::Unrecognised,
    }
}

/// Why a stored file cannot be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum FileProblemDto {
    NotFound,
    BadName,
    Missing,
    NotRegular,
    LengthMismatch,
    Io,
    DigestMismatch,
    Changed,
    NotVerified,
    OverLimit,
}

pub fn file_problem(p: oa::BlobProblem) -> FileProblemDto {
    use oa::BlobProblem as P;
    use FileProblemDto as D;
    match p {
        P::NotFound => D::NotFound,
        P::BadName => D::BadName,
        P::Missing => D::Missing,
        P::NotRegular => D::NotRegular,
        P::LengthMismatch => D::LengthMismatch,
        P::DigestMismatch => D::DigestMismatch,
        P::Changed => D::Changed,
        P::NotVerified => D::NotVerified,
        P::OverLimit => D::OverLimit,
        _ => D::Io,
    }
}

/// What an attachment's stored bytes are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export)]
pub enum ContentKindDto {
    /// No file is stored for it (the card says why).
    NoFile,
    #[serde(rename_all = "camelCase")]
    Sniffed {
        sniffed: SniffedKindDto,
        class: MediaClassDto,
        stored_bytes: u64,
    },
    Unavailable {
        problem: FileProblemDto,
    },
}

impl ContentKindDto {
    pub fn sniffed(s: &oa::Sniffed) -> ContentKindDto {
        ContentKindDto::Sniffed {
            sniffed: sniffed_kind(s.kind),
            class: media_class(s.class),
            stored_bytes: n(s.stored_bytes),
        }
    }
}

/// Why the structural check refused a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ContainerRefusalDto {
    TooDeep,
    TooManyBoxes,
    BoxPastEnd,
    SizeOverflow,
    SizeZeroNested,
    BoxTooSmall,
    ReferenceMovie,
    ExternalDataReference,
    NoMoov,
    ChunkPastEnd,
    DataBeforeFmt,
    NoFmt,
    TimedOut,
    NotPlayable,
}

fn refusal(r: oa::ContainerRefusal) -> ContainerRefusalDto {
    use oa::ContainerRefusal as R;
    use ContainerRefusalDto as D;
    match r {
        R::TooDeep => D::TooDeep,
        R::TooManyBoxes => D::TooManyBoxes,
        R::BoxPastEnd => D::BoxPastEnd,
        R::SizeOverflow => D::SizeOverflow,
        R::SizeZeroNested => D::SizeZeroNested,
        R::BoxTooSmall => D::BoxTooSmall,
        R::ReferenceMovie => D::ReferenceMovie,
        R::ExternalDataReference => D::ExternalDataReference,
        R::NoMoov => D::NoMoov,
        R::ChunkPastEnd => D::ChunkPastEnd,
        R::DataBeforeFmt => D::DataBeforeFmt,
        R::NoFmt => D::NoFmt,
        R::TimedOut => D::TimedOut,
        _ => D::NotPlayable,
    }
}

/// The outcome of `inspect_attachment { forPlayback: true }`: the full
/// hash (an integrity check only, never a safety check) and the structural
/// check, in that order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export)]
pub enum PlaybackDto {
    /// Both checks passed; `structureChecked` is false for the formats
    /// whose check is magic only (MP3, Ogg, WebM).
    #[serde(rename_all = "camelCase")]
    Allowed { structure_checked: bool },
    /// "This file can't be played in the app: its structure failed a
    /// check."
    Refused { reason: ContainerRefusalDto },
    /// The content is not audio or video.
    NotPlayable,
    /// Playback is turned off in the viewer settings.
    Off,
    /// The file failed the integrity check or could not be read.
    Unavailable { problem: FileProblemDto },
}

impl PlaybackDto {
    pub fn verdict(v: oa::ContainerVerdict) -> PlaybackDto {
        match v {
            oa::ContainerVerdict::Ok => PlaybackDto::Allowed {
                structure_checked: true,
            },
            oa::ContainerVerdict::NotChecked => PlaybackDto::Allowed {
                structure_checked: false,
            },
            oa::ContainerVerdict::Refused(r) => PlaybackDto::Refused { reason: refusal(r) },
            _ => PlaybackDto::NotPlayable,
        }
    }
}

/// `inspect_attachment`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AttachmentInspectionDto {
    pub facts: AttachmentFactsDto,
    pub content: ContentKindDto,
    /// Only when asked `forPlayback`.
    pub playback: Option<PlaybackDto>,
    /// The media token of the stored file; renditions and streams are
    /// served for it only after this inspection.
    pub token: Option<MediaToken>,
    /// The original's size, once a rendition was made ("original W×H");
    /// `None` until then.
    pub original_size: Option<ImageSizeDto>,
    /// Image previews are off for this session (the decode helper
    /// crashed too often).
    pub previews_off: bool,
}

/// An image's size in pixels, as the decode helper reported it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ImageSizeDto {
    pub width: u32,
    pub height: u32,
}

/// The viewer's output types, for the guard (and the archive guard's
/// rejection list). Kept in step with the roots by a test.
pub const M5_VIEWER_TYPES: &[&str] = &[
    "AssocKindDto",
    "AssocTargetShapeDto",
    "AssocVisibilityDto",
    "AssociationDto",
    "AttachmentDto",
    "AttachmentFactsDto",
    "AttachmentInspectionDto",
    "AttributedOutcomeDto",
    "BlobVerifyStateDto",
    "BodyVersionDto",
    "BodyViewDto",
    "CappedCountDto",
    "CardStateDto",
    "ChatFlagsDto",
    "ChatLabelDto",
    "ChatStatsDto",
    "ChipSenderDto",
    "ChipStateDto",
    "ChunkTextDto",
    "ContainerRefusalDto",
    "ContentKindDto",
    "ContentTextDto",
    "ConversationIndexDto",
    "ConversationRowDto",
    "ConversationRowsDto",
    "CopyDto",
    "DayCountsDto",
    "DecimalString",
    "DetailTimesDto",
    "DirectionDto",
    "DisplayNoneReasonDto",
    "DisplaySourceDto",
    "EarlierTextDto",
    "EditEntryDto",
    "EditEntryTextDto",
    "EditFactsDto",
    "EditLabelDto",
    "EditTextsOutcomeDto",
    "EditTimeDto",
    "FileProblemDto",
    "FocusDto",
    "FocusReasonDto",
    "FormattingDto",
    "GroupEventDto",
    "HandleDto",
    "HighlightDto",
    "HistoryColumnDto",
    "HistoryValueDto",
    "HitPieceDto",
    "HitTextDto",
    "HitVersionDto",
    "IdentifierProbeDto",
    "ImageSizeDto",
    "ItemKindDto",
    "LookupDto",
    "MaskKindDto",
    "MediaBase",
    "MediaClassDto",
    "MediaToken",
    "MessageDetailDto",
    "MessageDto",
    "MessageLabelDto",
    "NoHighlightDto",
    "NoTextDto",
    "ObservationDto",
    "OlderBackupDto",
    "OptContentDto",
    "PageRangeDto",
    "PathClassDto",
    "PlaybackDto",
    "PreviewBodyDto",
    "PreviewDto",
    "RawNumberDto",
    "RawValueDto",
    "ReactionChipDto",
    "ReactionHistoryDto",
    "ReactionKindDto",
    "ReactionRecordDto",
    "ReactionSummaryDto",
    "RecentlyDeletedDto",
    "RecordedByDto",
    "ReplyRefDto",
    "SafeId",
    "ScanCoverageDto",
    "ScanProgressDto",
    "ScanStopDto",
    "SearchHitDto",
    "SearchHitsDto",
    "SearchRanDto",
    "SearchStartedDto",
    "SeekResultDto",
    "SegmentDto",
    "SegmentPieceDto",
    "SenderDto",
    "ServiceClassDto",
    "SniffedKindDto",
    "StoredFileDto",
    "SummaryTimeKindDto",
    "TextChunkDto",
    "TextPieceDto",
    "ThreadPageDto",
    "TimeDto",
    "TimeUnitDto",
    "TsErrorDto",
    "UntrustedText",
    "ValueStateDto",
    "VerifyFactsDto",
    "VersionTextDto",
    "ViewerImportDto",
    "ViewerSessionDto",
    "ViewerSessionStateDto",
    "ViewerStatsDto",
    "WithheldReasonDto",
    "WordsPlanDto",
];

/// The brands: a field naming one carries content or a viewer value.
pub const BRANDS: &[&str] = &[
    "UntrustedText",
    "DecimalString",
    "SafeId",
    "MediaToken",
    "MediaBase",
];
