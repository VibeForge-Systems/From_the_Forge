//! The assembly layer without caps, for exports (M6 spec §3.1, §3.3,
//! slice 6a).
//!
//! - [`Caps::EXPORT`] lifts every cap the assembly layer and the details
//!   apply. A value assembled under it that still carries a `truncated`,
//!   `capped` or `more` marker is a missed cap, and an item whose extras
//!   were not loaded must never be exported: [`AssembledMessage::cap_miss`]
//!   finds both, and it runs in **every** build (never a
//!   `debug_assert`), so a release export refuses rather than ship a cut
//!   value.
//! - [`ExportItem`] is one message as an export assembles it: base and
//!   extras under `Caps::EXPORT`, with the raw codes and states the view
//!   types collapse ([`RawCodes`], §3.9).
//! - The public export reads (`with_export_snapshot`, the cursors, the
//!   uncapped rows) are in `crate::export` (slice 6b1), which assembles
//!   through [`export_item`] and [`checked`]. The fault-injection hooks
//!   below run one item with caps a test lowers, in its own read
//!   transaction under `ReadConn::cancellable`.
//!
//! The reply quote is the one capped value an export item keeps: it is the
//! viewer's two-line excerpt of the originator ([`super::QUOTE_UTF16`]),
//! and an export references the originator instead of quoting it. The
//! check ignores it for that reason.

use super::assemble::{self, AssembledMessage, BodyView, Caps, Ctx, Formatting, Reveal};
use super::raw::RawCodes;
use super::text::{OptContent, TextPiece};
use super::{ChatId, EarlierText, HandleId, MessageId, Participant, SegmentKind};
use crate::error::{ArchiveError, Stage};
use crate::progress::CancelToken;
use crate::reader::ArchiveReader;
use crate::stmt::StmtCache;

/// Where a cap left its marker in an assembled message, or why its extras
/// are missing (M6 spec §3.3). `oma-export` maps
/// [`CapMiss::ExtrasNotLoaded`] to `Limit(ExtrasNotLoaded)` and every other
/// kind to `Limit(TruncatedUnderExport)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CapMiss {
    /// [`AssembledMessage::extras`] is `false`.
    ExtrasNotLoaded,
    /// The current body, or one of its text segments, was cut.
    BodyTruncated,
    /// The body fell back to plain text over the segment cap.
    TooManySegments,
    /// Revealed earlier text was cut.
    EarlierTextTruncated,
    /// An attachment's declared type or name was cut.
    AttachmentValueTruncated,
    /// Attachments were left out ("N more").
    MoreAttachments,
    /// A group event's title was cut.
    EventTitleTruncated,
    /// The association's emoji, or a chip's, was cut.
    EmojiTruncated,
    /// The reaction rows were capped (no chips derived).
    ReactionRowsCapped,
    /// Chips were left out ("N more").
    MoreChips,
    /// The reply count was capped ("N+ replies").
    ReplyCountCapped,
}

fn opt_cut(o: &OptContent) -> bool {
    o.text().is_some_and(|t| t.truncated)
}

impl AssembledMessage {
    /// The first cap marker in this message, or `ExtrasNotLoaded`; `None`
    /// when every value is whole and the extras are loaded. Runs in every
    /// build. The reply quote, an excerpt by design, is not checked.
    pub fn cap_miss(&self) -> Option<CapMiss> {
        if !self.extras {
            return Some(CapMiss::ExtrasNotLoaded);
        }
        if let BodyView::Text(b) = &self.body {
            if b.formatting == Formatting::TooManySegments {
                return Some(CapMiss::TooManySegments);
            }
            let cut = b.truncated
                || b.segments.iter().any(|s| match &s.kind {
                    SegmentKind::Text(t) => t.truncated,
                    _ => false,
                });
            if cut {
                return Some(CapMiss::BodyTruncated);
            }
        }
        if let EarlierText::Revealed { text, .. } = &self.edit.earlier {
            if text.iter().any(|p| match p {
                TextPiece::Text(t) => t.truncated,
                TextPiece::Masked(_) => false,
            }) {
                return Some(CapMiss::EarlierTextTruncated);
            }
        }
        if self
            .attachments
            .iter()
            .any(|a| opt_cut(&a.declared_type) || opt_cut(&a.name))
        {
            return Some(CapMiss::AttachmentValueTruncated);
        }
        if self.more_attachments > 0 {
            return Some(CapMiss::MoreAttachments);
        }
        if self
            .event
            .as_ref()
            .and_then(|e| e.title.as_ref())
            .is_some_and(opt_cut)
        {
            return Some(CapMiss::EventTitleTruncated);
        }
        if self
            .association
            .as_ref()
            .and_then(|a| a.emoji.as_ref())
            .is_some_and(opt_cut)
        {
            return Some(CapMiss::EmojiTruncated);
        }
        if let Some(r) = &self.reactions {
            if r.rows.capped {
                return Some(CapMiss::ReactionRowsCapped);
            }
            if r.more_chips > 0 {
                return Some(CapMiss::MoreChips);
            }
            if r.chips
                .iter()
                .any(|c| c.emoji.as_ref().is_some_and(opt_cut))
            {
                return Some(CapMiss::EmojiTruncated);
            }
        }
        if self.reply_count.is_some_and(|n| n.capped) {
            return Some(CapMiss::ReplyCountCapped);
        }
        None
    }
}

/// One message as an export assembles it (M6 spec §3.2, §3.9): the
/// assembled message under `Caps::EXPORT`, its extras loaded, and the raw
/// codes and states the view types collapse.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ExportItem {
    pub message: AssembledMessage,
    pub codes: RawCodes,
}

/// Assemble message `id` (base, then extras) under `caps`, with its raw
/// codes. `None` when the message does not exist. Unchecked: see
/// [`checked`].
pub(crate) fn export_item(
    c: &StmtCache<'_>,
    ctx: &Ctx,
    chat: Option<i64>,
    id: i64,
    caps: &Caps,
    reveal: Reveal,
) -> rusqlite::Result<Option<ExportItem>> {
    let Some((mut message, extras, codes)) = assemble::base_coded(c, ctx, chat, id, caps, reveal)?
    else {
        return Ok(None);
    };
    assemble::extras(c, chat, &mut message, &extras, caps)?;
    Ok(Some(ExportItem { message, codes }))
}

/// `item`, unless a cap left its marker or its extras are missing: then a
/// refusal that names the kind only (never a value).
pub(crate) fn checked(item: ExportItem) -> Result<ExportItem, ArchiveError> {
    match item.message.cap_miss() {
        None => Ok(item),
        Some(CapMiss::ExtrasNotLoaded) => Err(ArchiveError::Internal("export extras not loaded")),
        Some(_) => Err(ArchiveError::Internal("value truncated under export caps")),
    }
}

impl ArchiveReader {
    /// One export item in one read transaction under
    /// `ReadConn::cancellable` (no op budget, stopped by `cancel` only),
    /// checked, under caps a test may lower. Only the fault-injection
    /// hooks call it; exports read through `with_export_snapshot`.
    #[cfg_attr(not(feature = "fault-injection"), allow(dead_code))]
    pub(crate) fn export_item(
        &self,
        chat: Option<ChatId>,
        id: MessageId,
        reveal: Reveal,
        caps: Caps,
        cancel: &CancelToken,
    ) -> Result<ExportItem, ArchiveError> {
        self.require_current()?;
        let conn = self.conn();
        let item = conn.snapshot(|| {
            conn.cancellable(cancel, Stage::View, |c| {
                let ctx = Ctx::load(c)?;
                export_item(
                    &StmtCache::new(c),
                    &ctx,
                    chat.map(ChatId::get),
                    id.get(),
                    &caps,
                    reveal,
                )
            })
        })?;
        checked(item.ok_or(ArchiveError::AnchorNotFound)?)
    }

    /// One handle with its address under `caps`, in one read transaction
    /// under `ReadConn::cancellable`, under caps a test may lower.
    /// Test-only.
    #[cfg_attr(not(feature = "fault-injection"), allow(dead_code))]
    pub(crate) fn export_handle(
        &self,
        id: HandleId,
        caps: Caps,
        cancel: &CancelToken,
    ) -> Result<Participant, ArchiveError> {
        self.require_current()?;
        let conn = self.conn();
        conn.snapshot(|| {
            conn.cancellable(cancel, Stage::View, |c| {
                assemble::handle(&StmtCache::new(c), id.get(), &caps)
            })
        })?
        .ok_or(ArchiveError::AnchorNotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::assemble::{
        AttachmentRef, EditFacts, RecordedTime, SenderReason, EMOJI_BYTES,
    };
    use crate::view::text::ContentText;
    use crate::view::{
        AttachmentId, BodyId, BodyText, CappedCount, DirectionCode, BODY_PAGE_UTF16,
        IDENTIFIER_UTF16, PREVIEW_UTF16,
    };

    fn message(text: &str, cap: u32) -> AssembledMessage {
        AssembledMessage {
            id: MessageId::new(1),
            item: None,
            direction: DirectionCode::Received,
            direction_recorded_by: None,
            sender: SenderReason::NoSender,
            service: None,
            date: RecordedTime {
                utc_ns: None,
                raw: None,
                unit: None,
                state: None,
                before_2007: false,
            },
            body: BodyView::Text(BodyText {
                body: BodyId::new(1),
                segments: Vec::new(),
                formatting: Formatting::Plain,
                source: None,
                full_utf16: 0,
                truncated: ContentText::from_str(text, cap).truncated,
                bidi_controls: false,
                invisible: false,
                lossy: false,
                stored_as_blob: false,
                sources_differ: None,
            }),
            edit: EditFacts {
                label: None,
                edited_parts: Vec::new(),
                unsent_parts: Vec::new(),
                part_count: None,
                unsent_whole: false,
                stored_versions: 1,
                earlier: EarlierText::None,
            },
            labels: Vec::new(),
            recently_deleted: Vec::new(),
            association: None,
            event: None,
            attachments: Vec::new(),
            more_attachments: 0,
            extras: true,
            reactions: None,
            reply: None,
            reply_count: None,
        }
    }

    #[test]
    fn page_keeps_the_m5_values_and_export_lifts_every_cap() {
        let p = Caps::PAGE;
        assert_eq!(
            (
                p.body_utf16,
                p.segments,
                p.reactions,
                p.chips,
                p.attachments
            ),
            (BODY_PAGE_UTF16, 2000, 200, 50, 100)
        );
        assert_eq!(p.reply_count, 1000);
        assert_eq!(
            (
                p.identifier_utf16,
                p.version_utf16,
                p.subject_utf16,
                p.emoji_bytes
            ),
            (
                IDENTIFIER_UTF16,
                BODY_PAGE_UTF16,
                PREVIEW_UTF16,
                EMOJI_BYTES
            )
        );
        let e = Caps::EXPORT;
        assert_eq!(
            (
                e.body_utf16,
                e.identifier_utf16,
                e.version_utf16,
                e.subject_utf16
            ),
            (u32::MAX, u32::MAX, u32::MAX, u32::MAX)
        );
        assert_eq!(
            (e.segments, e.chips, e.attachments, e.emoji_bytes),
            (usize::MAX, usize::MAX, usize::MAX, usize::MAX)
        );
        assert_eq!((e.reactions, e.reply_count), (u64::MAX, u64::MAX));
        // Long values come back whole under EXPORT.
        let emoji = "\u{1F600}".repeat(17);
        let OptContent::Text(t) =
            assemble::emoji_content(Some(emoji.as_bytes()), None, e.emoji_bytes)
        else {
            panic!("text");
        };
        assert!(!t.truncated);
        assert_eq!(t.text.expose().len(), 68);
        assert!(!ContentText::from_str(&"x".repeat(100_000), e.body_utf16).truncated);
    }

    #[test]
    fn every_marker_is_a_miss() {
        assert_eq!(message("ok", u32::MAX).cap_miss(), None);
        assert_eq!(
            message(&"x".repeat(9000), BODY_PAGE_UTF16).cap_miss(),
            Some(CapMiss::BodyTruncated)
        );
        let mut m = message("ok", u32::MAX);
        m.extras = false;
        assert_eq!(m.cap_miss(), Some(CapMiss::ExtrasNotLoaded));
        let mut m = message("ok", u32::MAX);
        m.more_attachments = 1;
        assert_eq!(m.cap_miss(), Some(CapMiss::MoreAttachments));
        let mut m = message("ok", u32::MAX);
        m.attachments.push(AttachmentRef {
            id: AttachmentId::new(1),
            declared_type: OptContent::Text(ContentText::from_str("t", u32::MAX)),
            name: OptContent::Text(ContentText::from_str(&"n".repeat(300), IDENTIFIER_UTF16)),
            sticker: None,
            declared_bytes: None,
            blob: None,
            stored_bytes: None,
            placed_inline: false,
        });
        assert_eq!(m.cap_miss(), Some(CapMiss::AttachmentValueTruncated));
        let mut m = message("ok", u32::MAX);
        m.reply_count = Some(CappedCount::of(1001, 1000));
        assert_eq!(m.cap_miss(), Some(CapMiss::ReplyCountCapped));
        let mut m = message("ok", u32::MAX);
        if let BodyView::Text(b) = &mut m.body {
            b.formatting = Formatting::TooManySegments;
        }
        assert_eq!(m.cap_miss(), Some(CapMiss::TooManySegments));
        // `checked` refuses with a kind, never a value.
        let item = ExportItem {
            message: message(&"x".repeat(9000), 10),
            codes: RawCodes {
                item: Err(0),
                direction: crate::view::RawDirection::Received,
                service: None,
                date: crate::view::RawTime::of(None, Some(1), None, None),
                item_type: crate::view::RawInt::of(None, Some(1)),
                group_action_type: crate::view::RawInt::of(None, Some(1)),
                schedule_type: crate::view::RawInt::of(None, Some(1)),
                app_content: crate::view::StateBool::of(None, Some(1)),
                effect: crate::view::StateBool::of(None, Some(1)),
                association: None,
                body: None,
                attachments: Vec::new(),
            },
        };
        assert!(matches!(
            checked(item),
            Err(ArchiveError::Internal("value truncated under export caps"))
        ));
    }
}
