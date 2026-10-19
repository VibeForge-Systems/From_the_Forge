//! The viewer's request types (M5 spec §7.2). Every one refuses unknown
//! fields; every id is checked in Rust to be an integer in
//! `1..=2^53 − 1`, so a fraction, a negative number or a value past 2^53
//! is `invalidInput` and never rounded; every instant is a UTC
//! millisecond count in the range a sort key encodes.
//!
//! The two commands with a free-text input (`list_conversations` and
//! `search_messages`) take the raw request and parse it with
//! [`parse_raw`]: any failure is `invalidInput` with a fixed sentence, so
//! a deserialisation error can never echo the text back.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use ts_rs::TS;

use super::dto::{SafeId, MAX_SAFE};
use super::messages;
use crate::error::IpcResult;

/// An id from the webview: any JSON number, checked by [`InId::get`].
/// Fields of this type are declared `SafeId` in TypeScript (`#[ts(as)]`),
/// so the frontend can only send ids it was given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InId(Option<i64>);

impl InId {
    pub fn new(v: i64) -> InId {
        InId((1..=MAX_SAFE).contains(&v).then_some(v))
    }

    /// The id, or `invalidInput`.
    pub fn get(self) -> IpcResult<i64> {
        self.0.ok_or_else(messages::invalid_id)
    }

    pub fn safe(self) -> IpcResult<SafeId> {
        self.get()
            .and_then(|v| SafeId::new(v).ok_or_else(messages::invalid_id))
    }
}

impl From<SafeId> for InId {
    fn from(s: SafeId) -> InId {
        InId(Some(s.get()))
    }
}

impl<'de> Deserialize<'de> for InId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let n = serde_json::Number::deserialize(d)?;
        let v = n.as_i64().or_else(|| {
            // Only an integral float names an integer; 1.5 does not.
            n.as_f64()
                .filter(|f| f.fract() == 0.0 && f.abs() <= MAX_SAFE as f64)
                .map(|f| f as i64)
        });
        Ok(match v {
            Some(v) => InId::new(v),
            None => InId(None),
        })
    }
}

/// Unix milliseconds from the webview, checked against the range a sort
/// key encodes (and 2^53).
/// Fields of this type are declared `number` in TypeScript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InMs(Option<i64>);

/// The widest instant: ±(2^53 − 1) ms is far past the ±292 years Unix
/// nanoseconds hold, so the nanosecond product is checked too.
impl InMs {
    pub fn new(v: i64) -> InMs {
        InMs(
            (-MAX_SAFE..=MAX_SAFE)
                .contains(&v)
                .then_some(v)
                .filter(|ms| ms.checked_mul(1_000_000).is_some()),
        )
    }

    /// The instant in Unix nanoseconds, or `invalidInput`.
    pub fn ns(self) -> IpcResult<i64> {
        self.0
            .and_then(|ms| ms.checked_mul(1_000_000))
            .ok_or_else(messages::invalid_input)
    }
}

impl<'de> Deserialize<'de> for InMs {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let n = serde_json::Number::deserialize(d)?;
        let v = n.as_i64().or_else(|| {
            n.as_f64()
                .filter(|f| f.fract() == 0.0 && f.abs() <= MAX_SAFE as f64)
                .map(|f| f as i64)
        });
        Ok(match v {
            Some(v) => InMs::new(v),
            None => InMs(None),
        })
    }
}

/// Parse the `request` member of a raw invoke body. Any failure, a missing
/// member included, is `invalidInput` with a fixed sentence that echoes
/// nothing.
pub fn parse_raw<T: DeserializeOwned>(body: Option<&serde_json::Value>) -> IpcResult<T> {
    body.and_then(|b| b.get("request"))
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .ok_or_else(messages::invalid_input)
}

/// `open_viewer`.
#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct OpenViewerRequest {
    pub archive_id: String,
}

/// `close_viewer`.
#[derive(Debug, Clone, Copy, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ViewerRef {
    pub viewer: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ConversationSortInput {
    Recent,
    Name,
    Count,
}

/// `list_conversations`. Parsed from the raw request (the filter is free
/// text).
#[derive(Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ListConversationsRequest {
    pub viewer: u32,
    pub sort: ConversationSortInput,
    #[serde(default)]
    #[ts(optional)]
    pub filter: Option<String>,
}

impl std::fmt::Debug for ListConversationsRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ListConversationsRequest")
            .field("sort", &self.sort)
            .field("filter", &self.filter.as_ref().map(String::len))
            .finish()
    }
}

/// `get_conversations`: at most 200 ids.
#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct GetConversationsRequest {
    pub viewer: u32,
    #[ts(as = "Vec<SafeId>")]
    pub ids: Vec<InId>,
}

/// What the user pressed "Show code" or "Show earlier text" for (D115).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct RevealInput {
    #[serde(default)]
    #[ts(optional)]
    pub earlier_text: Option<bool>,
    #[serde(default)]
    #[ts(optional)]
    pub one_time_code: Option<bool>,
}

impl RevealInput {
    pub fn reveal(r: Option<RevealInput>) -> oma_archive::Reveal {
        let r = r.unwrap_or_default();
        oma_archive::Reveal {
            earlier_text: r.earlier_text == Some(true),
            one_time_code: r.one_time_code == Some(true),
        }
    }
}

/// Where a page starts.
#[derive(Debug, Clone, Copy, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub enum AnchorInput {
    /// The newest dated items.
    Newest,
    /// The oldest dated items.
    Oldest,
    /// The undated section, in source order.
    Undated,
    /// A message of the conversation.
    Message {
        #[ts(as = "SafeId")]
        message: InId,
        #[serde(default)]
        #[ts(optional)]
        include: Option<bool>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum DirectionInput {
    Older,
    Newer,
    Around,
}

/// `get_thread_page`: `limit` is 1 to 100.
#[derive(Debug, Clone, Copy, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct GetThreadPageRequest {
    pub viewer: u32,
    #[ts(as = "SafeId")]
    pub chat_id: InId,
    pub anchor: AnchorInput,
    pub direction: DirectionInput,
    pub limit: u16,
    #[serde(default)]
    #[ts(optional)]
    pub reveal: Option<RevealInput>,
}

/// `get_reply_thread`.
#[derive(Debug, Clone, Copy, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct GetReplyThreadRequest {
    pub viewer: u32,
    #[ts(as = "SafeId")]
    pub message_id: InId,
    #[serde(default)]
    #[ts(optional)]
    pub reveal: Option<RevealInput>,
}

/// `get_message_detail`: `reactionPage` adds that page of the reaction
/// history.
#[derive(Debug, Clone, Copy, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct GetMessageDetailRequest {
    pub viewer: u32,
    #[ts(as = "SafeId")]
    pub message_id: InId,
    #[serde(default)]
    #[ts(optional)]
    pub reveal: Option<RevealInput>,
    #[serde(default)]
    #[ts(optional)]
    pub reaction_page: Option<u32>,
}

/// `get_message_text`: `fromUtf16` is clamped to the body in Rust.
#[derive(Debug, Clone, Copy, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct GetMessageTextRequest {
    pub viewer: u32,
    #[ts(as = "SafeId")]
    pub body_id: InId,
    pub from_utf16: u32,
    #[serde(default)]
    #[ts(optional)]
    pub reveal: Option<RevealInput>,
}

/// `get_day_counts`: 2 to 43 strictly increasing UTC instants (the UI
/// computes local day boundaries; Rust is time-zone-free, D104).
#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct GetDayCountsRequest {
    pub viewer: u32,
    #[ts(as = "SafeId")]
    pub chat_id: InId,
    #[ts(as = "Vec<f64>")]
    pub bounds_utc_ms: Vec<InMs>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum SeekDirInput {
    OnOrAfter,
    Before,
}

/// `seek_date`.
#[derive(Debug, Clone, Copy, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SeekDateRequest {
    pub viewer: u32,
    #[ts(as = "SafeId")]
    pub chat_id: InId,
    #[ts(as = "f64")]
    pub utc_ms: InMs,
    pub dir: SeekDirInput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum SearchModeInput {
    Auto,
    Exact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum SenderFilterInput {
    Anyone,
    Me,
    Others,
}

/// A search's filters.
#[derive(Debug, Clone, Copy, Default, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SearchFiltersInput {
    #[serde(default)]
    #[ts(as = "Option<SafeId>", optional)]
    pub chat_id: Option<InId>,
    #[serde(default)]
    #[ts(as = "Option<f64>", optional)]
    pub from_utc_ms: Option<InMs>,
    #[serde(default)]
    #[ts(as = "Option<f64>", optional)]
    pub until_utc_ms: Option<InMs>,
    #[serde(default)]
    #[ts(optional)]
    pub has_attachment: Option<bool>,
    #[serde(default)]
    #[ts(optional)]
    pub sender: Option<SenderFilterInput>,
}

/// `search_messages`. Parsed from the raw request (the query is free
/// text). `includeEarlier` is the explicit reveal for search (D115).
#[derive(Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SearchMessagesRequest {
    pub viewer: u32,
    pub query: String,
    pub mode: SearchModeInput,
    #[serde(default)]
    pub filters: SearchFiltersInput,
    #[serde(default)]
    pub include_earlier: bool,
    #[serde(default)]
    pub include_events: bool,
}

impl std::fmt::Debug for SearchMessagesRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchMessagesRequest")
            .field("query", &self.query.len())
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

/// `get_search_results`: `limit` is 1 to 50.
#[derive(Debug, Clone, Copy, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct GetSearchResultsRequest {
    pub viewer: u32,
    pub search_id: u32,
    pub offset: u32,
    pub limit: u32,
}

/// `cancel_search`.
#[derive(Debug, Clone, Copy, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct CancelSearchRequest {
    pub viewer: u32,
    pub search_id: u32,
}

/// `inspect_attachment`: only for attachments a page or detail of this
/// session surfaced.
#[derive(Debug, Clone, Copy, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct InspectAttachmentRequest {
    pub viewer: u32,
    #[ts(as = "SafeId")]
    pub attachment_id: InId,
    #[serde(default)]
    pub for_playback: bool,
}

/// The viewer's input types, for the guard.
pub const M5_VIEWER_INPUTS: &[&str] = &[
    "AnchorInput",
    "CancelSearchRequest",
    "ConversationSortInput",
    "DirectionInput",
    "GetConversationsRequest",
    "GetDayCountsRequest",
    "GetMessageDetailRequest",
    "GetMessageTextRequest",
    "GetReplyThreadRequest",
    "GetSearchResultsRequest",
    "GetThreadPageRequest",
    "InspectAttachmentRequest",
    "ListConversationsRequest",
    "OpenViewerRequest",
    "RevealInput",
    "SearchFiltersInput",
    "SearchMessagesRequest",
    "SearchModeInput",
    "SeekDateRequest",
    "SeekDirInput",
    "SenderFilterInput",
    "ViewerRef",
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;
    use serde_json::json;

    #[test]
    fn ids_are_integers_in_the_safe_range() {
        let id = |v: serde_json::Value| serde_json::from_value::<InId>(v).expect("number");
        assert_eq!(id(json!(1)).get().expect("ok"), 1);
        assert_eq!(id(json!(MAX_SAFE)).get().expect("ok"), MAX_SAFE);
        assert_eq!(id(json!(7.0)).get().expect("integral float"), 7);
        for bad in [
            json!(0),
            json!(-1),
            json!(MAX_SAFE + 1),
            json!(9_007_199_254_740_992.0),
            json!(1.5),
            json!(-0.5),
            json!(1e300),
            json!(u64::MAX),
        ] {
            let e = id(bad.clone()).get().expect_err("refused");
            assert_eq!(e.code, ErrorCode::InvalidInput, "{bad}");
            assert!(!e.message.contains(&bad.to_string()));
        }
        assert!(serde_json::from_value::<InId>(json!("1")).is_err());
    }

    #[test]
    fn instants_are_checked() {
        let ms = |v: serde_json::Value| serde_json::from_value::<InMs>(v).expect("number");
        assert_eq!(ms(json!(1)).ns().expect("ok"), 1_000_000);
        assert_eq!(ms(json!(-1)).ns().expect("ok"), -1_000_000);
        for bad in [json!(MAX_SAFE), json!(1.5), json!(10_000_000_000_000_i64)] {
            assert_eq!(
                ms(bad.clone()).ns().expect_err("refused").code,
                ErrorCode::InvalidInput,
                "{bad}"
            );
        }
    }

    #[test]
    fn raw_requests_never_echo_their_text() {
        let secret = "SYNTH-QUERY-SENTINEL";
        let bodies = [
            None,
            Some(json!({})),
            Some(json!({ "request": { "viewer": 1, "query": secret } })),
            Some(json!({ "request": { "viewer": 1, "query": secret, "mode": "auto", "x": 1 } })),
            Some(json!({ "request": { "viewer": 1, "query": 5, "mode": "auto" } })),
            Some(json!({ "request": { "viewer": -1, "query": secret, "mode": "auto" } })),
            Some(json!({ "request": { "viewer": 1, "query": secret, "mode": secret } })),
        ];
        for b in &bodies {
            let e = parse_raw::<SearchMessagesRequest>(b.as_ref()).expect_err("refused");
            assert_eq!(e.code, ErrorCode::InvalidInput);
            assert_eq!(e.message, messages::INVALID_VIEWER_INPUT);
        }
        let ok: SearchMessagesRequest = parse_raw(Some(&json!({
            "request": { "viewer": 3, "query": secret, "mode": "exact",
                         "filters": { "chatId": 2, "sender": "me" }, "includeEarlier": true }
        })))
        .expect("valid");
        assert_eq!(ok.viewer, 3);
        assert_eq!(ok.mode, SearchModeInput::Exact);
        assert!(ok.include_earlier && !ok.include_events);
        assert!(!format!("{ok:?}").contains(secret));
        let e = parse_raw::<ListConversationsRequest>(Some(&json!({
            "request": { "viewer": 1, "sort": "recent", "filter": 7 }
        })))
        .expect_err("refused");
        assert_eq!(e.message, messages::INVALID_VIEWER_INPUT);
        let ok: ListConversationsRequest = parse_raw(Some(&json!({
            "request": { "viewer": 1, "sort": "name", "filter": secret }
        })))
        .expect("valid");
        assert!(!format!("{ok:?}").contains(secret));
    }

    #[test]
    fn typed_requests_refuse_unknown_fields() {
        assert!(serde_json::from_value::<GetThreadPageRequest>(json!({
            "viewer": 1, "chatId": 1, "anchor": { "kind": "newest" },
            "direction": "older", "limit": 10, "sortKey": "00"
        }))
        .is_err());
        assert!(serde_json::from_value::<RevealInput>(json!({ "everything": true })).is_err());
        assert!(serde_json::from_value::<AnchorInput>(json!({
            "kind": "message", "message": 2, "cursor": 1
        }))
        .is_err());
        let ok: GetThreadPageRequest = serde_json::from_value(json!({
            "viewer": 1, "chatId": 4, "anchor": { "kind": "message", "message": 9, "include": true },
            "direction": "around", "limit": 50, "reveal": { "oneTimeCode": true }
        }))
        .expect("valid");
        assert_eq!(ok.chat_id.get().expect("id"), 4);
        let r = RevealInput::reveal(ok.reveal);
        assert!(r.one_time_code && !r.earlier_text);
        assert!(serde_json::from_value::<InspectAttachmentRequest>(json!({
            "viewer": 1, "attachmentId": 1, "path": "/etc/passwd"
        }))
        .is_err());
    }
}
