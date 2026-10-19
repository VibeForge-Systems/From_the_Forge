//! Thread pages and the reply thread (M5 spec §3.5, §3.6, §13.2, §13.3,
//! slice 5b2).
//!
//! - **The viewer catalogue:** for every entry and scenario, every chat is
//!   paged (newest to oldest, oldest to newest, and the undated section)
//!   and the items, their order and each item's facts match the Python
//!   model's `viewer.threads` and `viewer.messages` (never the Rust
//!   output): folding, every visible association class, direction and
//!   sender, labels and provenance, edits, unsends and withheld earlier
//!   text, the body facts, formatting, URL runs, inline placement, chips,
//!   replies and reply counts. No masked string reaches a page unless the
//!   request reveals it, and `Debug` prints no content.
//! - **Keyset paging on a bulk archive** against an oracle built in SQL:
//!   every visible row exactly once, in order, from either end, and
//!   `Around` pages from any anchor.
//! - **Concurrency (§13.3):** an anchor that a higher-ranked import moves
//!   older is paged from its remembered key, so every row between its old
//!   and new positions is returned exactly once and `anchor_moved` is set;
//!   reads during an import's write transaction see the committed snapshot
//!   and never `Locked`; a reader never holds back `wal_checkpoint
//!   (TRUNCATE)` once its call has returned.
//! - Budgets: `QueryTooBroad` (never `Damaged`) and `extras_incomplete`.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;

use std::collections::{BTreeMap, BTreeSet, HashMap};

use common::bulk::{generate, BulkSpec};
use common::import::read_only;
use common::viewer::{import_order, model_docs, names, Imported, Names};
use common::*;
use oma_archive::hooks::{install, Faults};
use oma_archive::schema::codes::{
    AssocKindCode, AssocTargetShape, DisplayNoneReason, DisplaySource, EditLabelCode, ItemKindCode,
    ServiceClassCode,
};
use oma_archive::{
    Anchor, ArchiveError, ArchiveReader, AssembledMessage, AssocVisibility, BodyView, ChatId,
    ChipSender, ChipState, ConversationSort, Direction, DirectionCode, EarlierText, FocusReason,
    Formatting, MessageId, MessageLabel, PageRange, PreviewBody, ReactionKind, Recovery, ReplyRef,
    Reveal, SegmentKind, SenderReason, SortKeyHint, TextPiece, ThreadPage, WithheldReason,
    REPLY_THREAD_LIMIT,
};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Paging helpers
// ---------------------------------------------------------------------------

fn hint(page: &ThreadPage, id: MessageId) -> Option<SortKeyHint> {
    page.served_keys()
        .iter()
        .find(|(m, _)| *m == id)
        .map(|(_, h)| h.clone())
}

/// Every item of one range, oldest first, paged as the UI does: from
/// `start` in `dir` (`Older` or `Newer`) with `want` items per page, each
/// next page anchored on the previous page's far anchor and its remembered
/// key.
fn page_all(
    r: &ArchiveReader,
    chat: ChatId,
    start: Anchor,
    dir: Direction,
    want: u16,
    reveal: Reveal,
) -> (Vec<AssembledMessage>, Vec<ThreadPage>) {
    let mut pages = Vec::new();
    let mut page = r
        .thread_page(chat, start, dir, want, None, reveal)
        .expect("first page");
    loop {
        assert!(!page.partial && !page.extras_incomplete && !page.anchor_moved);
        assert!(page.items.len() <= usize::from(want));
        let (more, next) = match dir {
            Direction::Older => (page.has_older, page.older_anchor),
            _ => (page.has_newer, page.newer_anchor),
        };
        pages.push(page);
        if !more {
            break;
        }
        let next = next.expect("a far anchor when there is more");
        let h = hint(pages.last().expect("page"), next);
        assert!(h.is_some(), "the far anchor's key was served");
        page = r
            .thread_page(
                chat,
                Anchor::Message {
                    id: next,
                    include: false,
                },
                dir,
                want,
                h.as_ref(),
                reveal,
            )
            .expect("next page");
        assert!(pages.len() < 10_000, "paging terminates");
    }
    let mut items: Vec<AssembledMessage> = Vec::new();
    let ordered: Vec<&ThreadPage> = match dir {
        Direction::Older => pages.iter().rev().collect(),
        _ => pages.iter().collect(),
    };
    for p in ordered {
        items.extend(p.items.iter().cloned());
    }
    (items, pages)
}

fn ids(items: &[AssembledMessage]) -> Vec<i64> {
    items.iter().map(|m| m.id.get()).collect()
}

// ---------------------------------------------------------------------------
// Names (the model's refs)
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// The model comparison
// ---------------------------------------------------------------------------

fn item_name(k: Option<ItemKindCode>) -> &'static str {
    match k {
        Some(ItemKindCode::Message) => "message",
        Some(ItemKindCode::ParticipantAdded) => "participantAdded",
        Some(ItemKindCode::OwnNumberChanged) => "ownNumberChanged",
        Some(ItemKindCode::ParticipantRemoved) => "participantRemoved",
        Some(ItemKindCode::GroupRenamed) => "groupRenamed",
        Some(ItemKindCode::ParticipantLeft) => "participantLeft",
        Some(ItemKindCode::GroupPhotoChanged) => "groupPhotoChanged",
        Some(ItemKindCode::GroupPhotoRemoved) => "groupPhotoRemoved",
        Some(ItemKindCode::BackgroundChanged) => "backgroundChanged",
        Some(ItemKindCode::BackgroundRemoved) => "backgroundRemoved",
        Some(ItemKindCode::LegacySharedLocation) => "legacySharedLocation",
        Some(ItemKindCode::AudioKept) => "audioKept",
        Some(ItemKindCode::SharePlay) => "sharePlay",
        Some(ItemKindCode::Unknown) => "unknown",
        _ => "?",
    }
}

fn service_name(s: Option<ServiceClassCode>) -> &'static str {
    match s {
        Some(ServiceClassCode::IMessage) => "imessage",
        Some(ServiceClassCode::Sms) => "sms",
        Some(ServiceClassCode::Rcs) => "rcs",
        Some(ServiceClassCode::IMessageLite) => "imessageLite",
        Some(ServiceClassCode::Empty) => "empty",
        Some(ServiceClassCode::Other) => "other",
        _ => "?",
    }
}

fn edit_name(l: Option<EditLabelCode>) -> &'static str {
    match l {
        Some(EditLabelCode::NotRecorded) => "notRecorded",
        Some(EditLabelCode::SummaryUnreadable) => "summaryUnreadable",
        Some(EditLabelCode::UnsentPartsAndEditHistory) => "unsentPartsAndEditHistory",
        Some(EditLabelCode::UnsentParts) => "unsentParts",
        Some(EditLabelCode::EditHistory) => "editHistory",
        Some(EditLabelCode::DateEditedNoBody) => "dateEditedNoBody",
        Some(EditLabelCode::DateEditedOnly) => "dateEditedOnly",
        Some(EditLabelCode::NoEditRecorded) => "noEditRecorded",
        Some(EditLabelCode::Undetermined) => "undetermined",
        _ => "?",
    }
}

fn kind_name(k: ReactionKind) -> &'static str {
    match k {
        ReactionKind::Love => "love",
        ReactionKind::Like => "like",
        ReactionKind::Dislike => "dislike",
        ReactionKind::Laugh => "laugh",
        ReactionKind::Emphasize => "emphasize",
        ReactionKind::Question => "question",
        ReactionKind::Emoji => "emoji",
        ReactionKind::Sticker => "sticker",
        _ => "?",
    }
}

fn shown(t: &oma_archive::ContentText) -> String {
    t.text.expose().clone()
}

/// The shown text of a page: segments, revealed earlier text and quotes.
fn page_text(p: &ThreadPage) -> String {
    let mut out = String::new();
    for m in &p.items {
        if let BodyView::Text(b) = &m.body {
            for s in &b.segments {
                if let SegmentKind::Text(t) = &s.kind {
                    out.push_str(&shown(t));
                }
            }
        }
        if let EarlierText::Revealed { text, .. } = &m.edit.earlier {
            for piece in text {
                if let TextPiece::Text(t) = piece {
                    out.push_str(&shown(t));
                }
            }
        }
        if let Some(
            ReplyRef::Resolved { quote, .. } | ReplyRef::InAnotherConversation { quote, .. },
        ) = &m.reply
        {
            if let PreviewBody::Text(pieces) = quote {
                for piece in pieces {
                    if let TextPiece::Text(t) = piece {
                        out.push_str(&shown(t));
                    }
                }
            }
        }
        out.push('\u{0}');
    }
    out
}

fn label_names(m: &AssembledMessage, n: &Names) -> Vec<String> {
    let mut v: Vec<String> = m
        .labels
        .iter()
        .map(|l| match l {
            MessageLabel::Undated => "undated".to_owned(),
            MessageLabel::Before2007 => "before2007".to_owned(),
            MessageLabel::Unkeyed => "unkeyed".to_owned(),
            MessageLabel::ScheduleType(t) => format!("scheduleType:{t}"),
            MessageLabel::AppContent => "appContent".to_owned(),
            MessageLabel::Effect => "effect".to_owned(),
            MessageLabel::NotInLatest(l) => format!("notInLatest:{}", n.lineages[&l.get()]),
            MessageLabel::Provisional => "provisional".to_owned(),
            _ => "?".to_owned(),
        })
        .collect();
    v.sort();
    v
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .map(|x| x.as_str().expect("str").to_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn ints(v: &Value) -> Vec<i64> {
    v.as_array()
        .map(|a| a.iter().map(|x| x.as_i64().expect("int")).collect())
        .unwrap_or_default()
}

/// Counters of what the catalogue exercised.
#[derive(Default)]
struct Seen {
    classes: BTreeSet<String>,
    chips: BTreeSet<String>,
    replies: BTreeSet<String>,
    earlier: BTreeSet<String>,
    over200: usize,
    unpaired: usize,
    inline: usize,
    recently_deleted: usize,
    recorded_by: usize,
    labels: BTreeSet<String>,
    senders: BTreeSet<String>,
    folded: usize,
}

#[allow(clippy::too_many_lines)]
fn compare(at: &str, m: &AssembledMessage, w: &Value, n: &Names, seen: &mut Seen) {
    assert!(m.extras, "{at}: extras loaded");
    assert_eq!(item_name(m.item), w["item"].as_str().expect("item"), "{at}");
    let dir = match m.direction {
        DirectionCode::You => "you",
        DirectionCode::Received => "received",
        _ => "notRecorded",
    };
    assert_eq!(dir, w["direction"], "{at}: direction");
    match (&m.direction_recorded_by, w.get("directionAsRecordedBy")) {
        (None, None) => {}
        (Some(by), Some(want)) => {
            assert_eq!(n.lineages[&by.lineage.get()], want["lineage"], "{at}");
            assert_eq!(n.entries[&by.session], want["entry"], "{at}");
            seen.recorded_by += 1;
        }
        (got, want) => panic!("{at}: direction recorded by {got:?} vs {want:?}"),
    }
    let (reason, handle) = match m.sender {
        SenderReason::FromMe => ("fromMe", None),
        SenderReason::Resolved(h) => ("resolved", Some(n.handles[&h.get()].clone())),
        SenderReason::NoSender => ("noSender", None),
        SenderReason::ColumnAbsent => ("columnAbsent", None),
        SenderReason::Unreadable => ("unreadable", None),
        SenderReason::NotFound => ("notFound", None),
        SenderReason::DirectionNotRecorded => ("directionNotRecorded", None),
        _ => ("?", None),
    };
    seen.senders.insert(reason.to_owned());
    assert_eq!(reason, w["sender"]["reason"], "{at}: sender");
    assert_eq!(handle.as_deref(), w["sender"]["handle"].as_str(), "{at}");
    assert_eq!(service_name(m.service), w["service"], "{at}: service");
    assert_eq!(m.date.utc_ns, w["utcNs"].as_i64(), "{at}: date");
    let mut want_labels = strings(&w["labels"]);
    want_labels.sort();
    let labels = label_names(m, n);
    for l in &labels {
        seen.labels
            .insert(l.split(':').next().unwrap_or("").to_owned());
    }
    assert_eq!(labels, want_labels, "{at}: labels");

    // Recently Deleted.
    let rd: Vec<(String, Option<String>, Vec<String>)> = m
        .recently_deleted
        .iter()
        .map(|r| {
            let mut later: Vec<String> = r
                .not_listed_later
                .iter()
                .map(|s| n.entries[s].clone())
                .collect();
            later.sort();
            (
                n.entries[&r.session].clone(),
                r.chat.map(|c| n.chats[&c.get()].clone()),
                later,
            )
        })
        .collect();
    let want_rd: Vec<(String, Option<String>, Vec<String>)> = w["recentlyDeleted"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|r| {
                    (
                        r["entry"].as_str().expect("entry").to_owned(),
                        r["chat"].as_str().map(str::to_owned),
                        strings(&r["notListedLater"]),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(rd, want_rd, "{at}: recently deleted");
    seen.recently_deleted += rd.len();

    // The body.
    let wb = &w["body"];
    match &m.body {
        BodyView::Text(b) => {
            let src = match b.source {
                Some(DisplaySource::AttributedBody) => "attributedBody",
                Some(DisplaySource::Text) => "text",
                Some(DisplaySource::TextLossy) => "textLossy",
                _ => "?",
            };
            assert_eq!(src, wb["source"], "{at}: source");
            assert_eq!(i64::from(b.full_utf16), wb["utf16"], "{at}: utf16");
            assert_eq!(b.bidi_controls, wb["bidiControls"] == true, "{at}");
            assert_eq!(b.invisible, wb["invisible"] == true, "{at}");
            assert_eq!(b.lossy, src == "textLossy", "{at}");
            if let Some(d) = wb.get("sourcesDiffer") {
                assert_eq!(b.sources_differ, d.as_bool(), "{at}: sources differ");
            }
            // URL runs, merged from adjacent URL segments.
            if !b.truncated {
                let mut runs: Vec<(String, bool)> = Vec::new();
                let mut open = false;
                for s in &b.segments {
                    match (&s.kind, s.url) {
                        (SegmentKind::Text(t), true) => {
                            if open {
                                if let Some(last) = runs.last_mut() {
                                    last.0.push_str(&shown(t));
                                    last.1 |= s.url_hidden;
                                }
                            } else {
                                runs.push((shown(t), s.url_hidden));
                            }
                            open = true;
                        }
                        _ => open = false,
                    }
                }
                let want_runs: Vec<(String, bool)> = wb["urls"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|u| {
                                (
                                    u["text"].as_str().expect("t").to_owned(),
                                    u["hidden"] == true,
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                assert_eq!(runs, want_runs, "{at}: URL runs");
            }
            // Formatting.
            let f = match b.formatting {
                Formatting::Applied | Formatting::TooManySegments => Some("applied"),
                Formatting::NotApplied => Some("notApplied"),
                Formatting::Unreadable(_) => Some("unreadable"),
                _ => None,
            };
            assert_eq!(f, w["formatting"].as_str(), "{at}: formatting");
            // Placeholders, codes, transcription and mentions.
            if b.formatting == Formatting::Applied {
                let inline: Vec<(String, Option<String>)> = b
                    .segments
                    .iter()
                    .filter_map(|s| match &s.kind {
                        SegmentKind::InlineEmoji => Some(("inlineEmoji".to_owned(), None)),
                        SegmentKind::UnmatchedPlaceholder => Some(("unmatched".to_owned(), None)),
                        SegmentKind::Attachment(a) => Some((
                            "attachment".to_owned(),
                            Some(n.attachments[&a.get()].clone()),
                        )),
                        _ => None,
                    })
                    .collect();
                let want_inline: Vec<(String, Option<String>)> = w["inline"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|i| {
                                (
                                    i["kind"].as_str().expect("k").to_owned(),
                                    i["attachment"].as_str().map(str::to_owned),
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                assert_eq!(inline, want_inline, "{at}: inline");
                seen.inline += inline.len();
                if w.get("listedBelow").is_some() {
                    let mut below: Vec<String> = m
                        .attachments
                        .iter()
                        .filter(|a| !a.placed_inline)
                        .map(|a| n.attachments[&a.id.get()].clone())
                        .collect();
                    below.sort();
                    assert_eq!(below, strings(&w["listedBelow"]), "{at}: listed below");
                }
                let withheld = b
                    .segments
                    .iter()
                    .filter(|s| matches!(s.kind, SegmentKind::Withheld(_)))
                    .count();
                assert_eq!(
                    withheld,
                    strings(&w["oneTimeCodes"]).len(),
                    "{at}: one-time codes"
                );
                let transcription = b.segments.iter().any(|s| s.transcription);
                assert_eq!(transcription, w["audioTranscription"] == true, "{at}");
                let mentions = b.segments.iter().filter(|s| s.mention).count();
                assert_eq!(
                    mentions as i64,
                    w["mentions"].as_i64().unwrap_or(0),
                    "{at}: mentions"
                );
            }
        }
        BodyView::NoText(t) => {
            let r = match t.reason {
                Some(DisplayNoneReason::AttributedBodyUnusable) => "attributedBodyUnusable",
                Some(DisplayNoneReason::TextEmpty) => "textEmpty",
                Some(DisplayNoneReason::NoSource) => "noSource",
                _ => "?",
            };
            assert_eq!(r, wb["noneReason"], "{at}: none reason");
            if let Some(e) = wb["attributedError"].as_str() {
                assert_eq!(format!("{:?}", t.attributed_error.expect("error")), e);
            }
        }
        BodyView::NoBody => panic!("{at}: every catalogue message has a body"),
        _ => panic!("{at}: unknown body view"),
    }

    // Edits, unsends and earlier text.
    let we = &w["edit"];
    assert_eq!(edit_name(m.edit.label), we["label"], "{at}: edit label");
    assert_eq!(
        i64::from(m.edit.stored_versions),
        we["storedVersions"],
        "{at}"
    );
    assert_eq!(
        m.edit.edited_parts,
        ints(&we["editedParts"]),
        "{at}: edited"
    );
    assert_eq!(
        m.edit.unsent_parts,
        ints(&we["unsentParts"]),
        "{at}: unsent"
    );
    if let Some(of) = we.get("unsentOf") {
        assert_eq!(
            m.edit.part_count.map(i64::from),
            of.as_i64(),
            "{at}: unsent of"
        );
        assert_eq!(m.edit.unsent_whole, we["unsentWhole"] == true, "{at}");
    }
    let (state, reason) = match &m.edit.earlier {
        EarlierText::None => (None, None),
        EarlierText::Available => (Some("shown"), None),
        EarlierText::Withheld(r) | EarlierText::Revealed { reason: r, .. } => (
            Some("withheld"),
            Some(match r {
                WithheldReason::Unsent => "unsent",
                WithheldReason::EarlierVersion => "earlierVersion",
                WithheldReason::NoCurrentText => "noCurrentText",
                _ => "?",
            }),
        ),
        _ => (Some("?"), None),
    };
    assert_eq!(state, we["earlierText"].as_str(), "{at}: earlier text");
    assert_eq!(reason, we["earlierTextReason"].as_str(), "{at}: reason");
    if let Some(s) = state {
        seen.earlier.insert(format!("{s}:{}", reason.unwrap_or("")));
    }

    // The association class of a visible row.
    let wa = &w["association"];
    match (&m.association, wa.is_object()) {
        (None, false) => {}
        (Some(a), true) => {
            let class = match a.visibility {
                AssocVisibility::Undeterminable => "visible:undeterminable".to_owned(),
                AssocVisibility::NoKind => "visible:noKind".to_owned(),
                AssocVisibility::NotAReaction => match a.kind {
                    Some(AssocKindCode::StickerLegacy) => "visible:stickerlegacy".to_owned(),
                    Some(AssocKindCode::PollVote) => "visible:pollvote".to_owned(),
                    Some(AssocKindCode::Unknown) => "visible:unknown".to_owned(),
                    k => format!("visible:{k:?}"),
                },
                AssocVisibility::TargetUnreadable => "visible:targetUnreadable".to_owned(),
                AssocVisibility::TargetNotUnique => "visible:targetNotUnique".to_owned(),
                AssocVisibility::TargetInAnotherChat(t) => {
                    assert_eq!(n.messages[&t.get()], wa["target"], "{at}: target");
                    "visible:targetInAnotherChat".to_owned()
                }
                AssocVisibility::Folded(_) => panic!("{at}: a folded row on a page"),
                AssocVisibility::TargetFolds(t) => {
                    assert_eq!(n.messages[&t.get()], wa["target"], "{at}: target");
                    "visible:targetFolds".to_owned()
                }
                _ => "?".to_owned(),
            };
            assert_eq!(class, wa["timeline"], "{at}: association");
            seen.classes.insert(class);
            assert_eq!(
                a.shape == Some(AssocTargetShape::Bare),
                wa["note"] == "bareTarget",
                "{at}"
            );
        }
        (got, _) => panic!("{at}: association {got:?} vs {wa}"),
    }

    // Reaction chips (compared as a set: the model sorts differently).
    let wr = &w["reactions"];
    match (&m.reactions, wr.is_object()) {
        (None, false) => {}
        (Some(r), true) => {
            assert_eq!(r.rows.capped, wr["over200"] == true, "{at}: over 200");
            if r.rows.capped {
                seen.over200 += 1;
                assert!(r.chips.is_empty());
            } else {
                assert_eq!(r.rows.n as i64, wr["rows"], "{at}: rows");
            }
            let chips: BTreeSet<String> = r
                .chips
                .iter()
                .map(|c| {
                    let kind = match (c.kind, &c.emoji) {
                        (ReactionKind::Emoji, Some(oma_archive::OptContent::Text(t))) => {
                            format!("emoji:{}", shown(t))
                        }
                        (k, _) => kind_name(k).to_owned(),
                    };
                    let sender = match c.sender {
                        ChipSender::Me => "me".to_owned(),
                        ChipSender::Handle(h) => n.handles[&h.get()].clone(),
                        _ => "null".to_owned(),
                    };
                    let state = match c.state {
                        ChipState::Shown => "shown",
                        ChipState::Hidden => "hidden",
                        ChipState::Undeterminable => "undeterminable",
                        ChipState::Added => "added",
                        ChipState::Removed => "removed",
                        _ => "?",
                    };
                    seen.chips.insert(state.to_owned());
                    let mut rows: Vec<String> = c
                        .rows
                        .iter()
                        .map(|id| n.messages[&id.get()].clone())
                        .collect();
                    rows.sort();
                    format!("{kind}|{sender}|{}|{state}", rows.join(","))
                })
                .collect();
            let want: BTreeSet<String> = wr["chips"]
                .as_array()
                .expect("chips")
                .iter()
                .map(|c| {
                    format!(
                        "{}|{}|{}|{}",
                        c["kind"].as_str().expect("kind"),
                        c["sender"].as_str().unwrap_or("null"),
                        strings(&c["rows"]).join(","),
                        c["state"].as_str().expect("state")
                    )
                })
                .collect();
            assert_eq!(chips, want, "{at}: chips");
            let unpaired: Vec<String> = r
                .unpaired
                .iter()
                .map(|id| n.messages[&id.get()].clone())
                .collect();
            assert_eq!(unpaired, strings(&wr["unpaired"]), "{at}: unpaired");
            seen.unpaired += unpaired.len();
        }
        (got, _) => panic!("{at}: reactions {got:?} vs {wr}"),
    }

    // Replies.
    let wp = &w["reply"];
    match (&m.reply, wp.is_object()) {
        (None, false) => {}
        (Some(r), true) => {
            let (state, originator) = match r {
                ReplyRef::Resolved { originator, quote } => {
                    assert!(!matches!(quote, PreviewBody::NoBody), "{at}: quote");
                    ("resolved", Some(n.messages[&originator.get()].clone()))
                }
                ReplyRef::InAnotherConversation {
                    originator, chat, ..
                } => {
                    assert!(chat.is_some(), "{at}: the originator's chat");
                    (
                        "inAnotherConversation",
                        Some(n.messages[&originator.get()].clone()),
                    )
                }
                ReplyRef::NotUniquelyIdentified => ("notUniquelyIdentified", None),
                _ => ("?", None),
            };
            assert_eq!(state, wp["state"], "{at}: reply");
            assert_eq!(originator.as_deref(), wp["originator"].as_str(), "{at}");
            seen.replies.insert(state.to_owned());
        }
        (got, _) => panic!("{at}: reply {got:?} vs {wp}"),
    }
    match m.reply_count {
        None => assert!(w.get("replyCount").is_none(), "{at}: reply count"),
        Some(c) => {
            assert_eq!(c.n as i64, w["replyCount"], "{at}: reply count");
            assert_eq!(c.capped, w["replyCountCapped"] == true, "{at}");
        }
    }
}

#[test]
fn pages_match_the_model_for_every_entry_and_scenario() {
    let mut seen = Seen::default();
    for (what, model) in model_docs() {
        let order = &model["orders"][0];
        let i = import_order(order, &what);
        let n = names(&i, order);
        let before = data_snapshot(&i.tmp, &i.id);
        let r = i.lib.open_read(&i.id).expect("reader");
        let threads = model["viewer"]["threads"].as_object().expect("threads");
        let messages = &model["viewer"]["messages"];
        let masked: Vec<String> = ["oneTimeCodes", "earlierText"]
            .iter()
            .flat_map(|k| strings(&model["viewer"]["masked"][*k]))
            .collect();
        let index = r
            .conversation_index(ConversationSort::Recent, None)
            .expect("index");
        assert_eq!(index.ids.len(), threads.len(), "{what}: chats");
        for chat in &index.ids {
            let chat_name = &n.chats[&chat.get()];
            let want = &threads[chat_name.as_str()];
            let at = format!("{what}: {chat_name}");
            let (older, older_pages) = page_all(
                &r,
                *chat,
                Anchor::Newest,
                Direction::Older,
                7,
                Reveal::default(),
            );
            let (newer, _) = page_all(
                &r,
                *chat,
                Anchor::Oldest,
                Direction::Newer,
                5,
                Reveal::default(),
            );
            assert_eq!(ids(&older), ids(&newer), "{at}: both ends agree");
            let (undated, undated_pages) = page_all(
                &r,
                *chat,
                Anchor::Undated,
                Direction::Newer,
                3,
                Reveal::default(),
            );
            let named = |items: &[AssembledMessage]| -> Vec<String> {
                items
                    .iter()
                    .map(|m| n.messages[&m.id.get()].clone())
                    .collect()
            };
            assert_eq!(named(&older), strings(&want["dated"]), "{at}: dated");
            assert_eq!(named(&undated), strings(&want["undated"]), "{at}: undated");
            // The undated rows recorded (reactions included), as the
            // conversation list counts them.
            let undated_rows = model["viewer"]["conversations"]
                .as_array()
                .expect("conversations")
                .iter()
                .find(|c| c["chat"] == chat_name.as_str())
                .and_then(|c| c["undatedItems"].as_u64());
            for p in &older_pages {
                assert_eq!(p.range, PageRange::Dated);
                assert_eq!(
                    p.undated.map(|u| u.n),
                    undated_rows,
                    "{at}: the dated pages offer the undated section"
                );
            }
            assert!(!older_pages[0].has_newer, "{at}: the dated end");
            for p in &undated_pages {
                assert_eq!(p.range, PageRange::Undated);
                assert!(p.undated.is_none());
            }
            assert!(!undated_pages[0].has_older, "{at}: the undated start");
            // Every folded reaction is a row of its target's chips.
            for f in want["folded"].as_array().expect("folded") {
                let target = f["target"].as_str().expect("target");
                let reaction = f["reaction"].as_str().expect("reaction");
                let t = older
                    .iter()
                    .chain(&undated)
                    .find(|m| n.messages[&m.id.get()] == target)
                    .unwrap_or_else(|| panic!("{at}: target {target} on a page"));
                let rs = t.reactions.as_ref().expect("reactions");
                let listed = rs.rows.capped
                    || rs
                        .unpaired
                        .iter()
                        .any(|id| n.messages[&id.get()] == reaction)
                    || rs
                        .chips
                        .iter()
                        .any(|c| c.rows.iter().any(|id| n.messages[&id.get()] == reaction));
                assert!(listed, "{at}: folded {reaction} listed on {target}");
                seen.folded += 1;
            }
            for m in older.iter().chain(&undated) {
                let name = &n.messages[&m.id.get()];
                compare(
                    &format!("{at}: {name}"),
                    m,
                    &messages[name.as_str()],
                    &n,
                    &mut seen,
                );
            }
            // Nothing masked reaches a page, and Debug prints no content.
            for p in older_pages.iter().chain(&undated_pages) {
                let text = page_text(p);
                let debug = format!("{p:?}");
                for s in &masked {
                    assert!(!text.contains(s.as_str()), "{at}: a masked string shown");
                    assert!(
                        !debug.contains(s.as_str()),
                        "{at}: a masked string in Debug"
                    );
                }
                for m in &p.items {
                    if let BodyView::Text(b) = &m.body {
                        for s in &b.segments {
                            if let SegmentKind::Text(t) = &s.kind {
                                let t = shown(t);
                                if t.chars().count() > 3 {
                                    assert!(!debug.contains(&t), "{at}: body text in Debug");
                                }
                            }
                        }
                    }
                }
            }
            // Revealed on request: the codes and the earlier text.
            let reveal = Reveal {
                earlier_text: true,
                one_time_code: true,
            };
            let (revealed, _) = page_all(&r, *chat, Anchor::Newest, Direction::Older, 50, reveal);
            assert_eq!(ids(&revealed), ids(&older));
            for (m, plain) in revealed.iter().zip(&older) {
                let name = &n.messages[&m.id.get()];
                let w = &messages[name.as_str()];
                if let EarlierText::Withheld(reason) = &plain.edit.earlier {
                    let EarlierText::Revealed {
                        reason: r2, text, ..
                    } = &m.edit.earlier
                    else {
                        panic!("{at}: {name}: revealed on request");
                    };
                    assert_eq!(reason, r2);
                    let [TextPiece::Text(text)] = text.as_slice() else {
                        panic!("{at}: {name}: codes revealed, so one text piece");
                    };
                    assert!(masked.contains(&shown(text)), "{at}: {name}: earlier text");
                }
                for code in strings(&w["oneTimeCodes"]) {
                    let p = ThreadPage::clone(
                        &r.thread_page(
                            *chat,
                            Anchor::Message {
                                id: m.id,
                                include: true,
                            },
                            Direction::Older,
                            1,
                            None,
                            reveal,
                        )
                        .expect("page"),
                    );
                    assert!(page_text(&p).contains(&code), "{at}: {name}: code revealed");
                }
            }
        }
        drop(r);
        assert_eq!(data_snapshot(&i.tmp, &i.id), before, "{what}: reads wrote");
    }
    // The catalogue exercised what the rules are about.
    for c in [
        "visible:stickerlegacy",
        "visible:pollvote",
        "visible:unknown",
        "visible:undeterminable",
        "visible:noKind",
        "visible:targetInAnotherChat",
        "visible:targetNotUnique",
        "visible:targetUnreadable",
    ] {
        assert!(seen.classes.contains(c), "no {c} row: {:?}", seen.classes);
    }
    for s in ["shown", "hidden", "undeterminable", "added"] {
        assert!(seen.chips.contains(s), "no {s} chip");
    }
    for s in ["resolved", "notUniquelyIdentified", "inAnotherConversation"] {
        assert!(seen.replies.contains(s), "no {s} reply");
    }
    for s in ["withheld:unsent", "withheld:earlierVersion", "shown:"] {
        assert!(
            seen.earlier.contains(s),
            "no {s} earlier text: {:?}",
            seen.earlier
        );
    }
    for l in [
        "undated",
        "before2007",
        "unkeyed",
        "scheduleType",
        "appContent",
        "effect",
        "notInLatest",
        "provisional",
    ] {
        assert!(seen.labels.contains(l), "no {l} label");
    }
    for s in [
        "fromMe",
        "resolved",
        "noSender",
        "columnAbsent",
        "unreadable",
        "notFound",
        "directionNotRecorded",
    ] {
        assert!(seen.senders.contains(s), "no {s} sender");
    }
    assert!(seen.over200 > 0 && seen.unpaired > 0 && seen.inline > 0);
    assert!(seen.recently_deleted > 0 && seen.recorded_by > 0 && seen.folded > 200);
}

/// A folded row is not a timeline item (§3.5 step 3): a reaction to a
/// reaction that folds, and a cycle of reactions, stay visible and are
/// counted, so no recorded row disappears.
#[test]
fn a_reaction_to_a_folded_reaction_stays_visible() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    {
        let mut a = lib.open(&id).expect("open");
        let mut spec = BulkSpec::new(40);
        spec.chats = 1;
        spec.reaction_permille = 0;
        spec.undated_permille = 0;
        generate(&mut a, &spec);
    }
    let rows: Vec<(i64, i64)> = {
        let c = raw(&db_path(&tmp, &id));
        let rows: Vec<(i64, i64)> = c
            .prepare(
                "SELECT cm.chat_id, cm.message_id FROM chat_message cm \
                 JOIN message m ON m.id = cm.message_id \
                 WHERE m.key_kind = 1 AND m.assoc_present = 0 AND cm.sort_key < X'01' \
                 ORDER BY cm.sort_key, cm.message_id LIMIT 5",
            )
            .expect("prepare")
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("rows");
        assert_eq!(rows.len(), 5);
        assert!(rows.iter().all(|r| r.0 == rows[0].0), "one chat");
        let react = |from: i64, to: i64| {
            c.execute(
                "UPDATE message SET assoc_present = 1, assoc_kind = 10, assoc_target_shape = 2, \
                   assoc_target_guid = (SELECT guid FROM message WHERE id = ?2) WHERE id = ?1",
                [from, to],
            )
            .expect("react");
        };
        // R1 -> M folds; R2 -> R1 stays visible (R1 is not an item).
        react(rows[1].1, rows[0].1);
        react(rows[2].1, rows[1].1);
        // A cycle: R3 -> R4 -> R3; neither is hidden.
        react(rows[3].1, rows[4].1);
        react(rows[4].1, rows[3].1);
        rows
    };
    let chat = ChatId::new(rows[0].0);
    let r = lib.open_read(&id).expect("reader");
    let (items, _) = page_all(
        &r,
        chat,
        Anchor::Oldest,
        Direction::Newer,
        50,
        Reveal::default(),
    );
    let shown: BTreeSet<i64> = items.iter().map(|m| m.id.get()).collect();
    assert!(shown.contains(&rows[0].1), "M");
    assert!(!shown.contains(&rows[1].1), "R1 folds into M");
    for (k, row) in rows.iter().enumerate().skip(2) {
        assert!(shown.contains(&row.1), "row {k} stays visible");
    }
    let r2 = items.iter().find(|m| m.id.get() == rows[2].1).expect("R2");
    assert_eq!(
        r2.association.as_ref().map(|a| a.visibility),
        Some(AssocVisibility::TargetFolds(MessageId::new(rows[1].1)))
    );
    // Every row of the chat but R1 is counted on its day.
    let c = read_only(&db_path(&tmp, &id));
    let total: i64 = c
        .query_row(
            "SELECT count(*) FROM chat_message WHERE chat_id = ?1 AND sort_key < X'01'",
            [chat.get()],
            |row| row.get(0),
        )
        .expect("count");
    let counts = r.day_counts(chat, &[i64::MIN, i64::MAX]).expect("days");
    assert_eq!(counts.counts, vec![u64::try_from(total - 1).expect("n")]);
    assert_eq!(items.len(), usize::try_from(total - 1).expect("n"));
}

// ---------------------------------------------------------------------------
// Anchors, focus, the reply thread
// ---------------------------------------------------------------------------

/// The main catalogue entry, imported, with its names.
fn main_entry() -> (Imported, Names) {
    let (what, model) = model_docs()
        .into_iter()
        .find(|(w, _)| w == "view-main-plain")
        .expect("entry");
    let i = import_order(&model["orders"][0], &what);
    let n = names(&i, &model["orders"][0]);
    (i, n)
}

fn by_name(map: &HashMap<i64, String>, name: &str) -> i64 {
    *map.iter()
        .find(|(_, v)| v.as_str() == name)
        .unwrap_or_else(|| panic!("{name}"))
        .0
}

#[test]
fn anchors_focus_and_the_reply_thread() {
    let (i, n) = main_entry();
    let r = i.lib.open_read(&i.id).expect("reader");
    let msg = |name: &str| MessageId::new(by_name(&n.messages, name));
    let chat1 = ChatId::new(by_name(
        &n.chats,
        "guid:iMessage;-;synthetic-viewer-1@example.invalid",
    ));
    let none = Reveal::default();

    // Around a visible message: it is the focus, with neighbours on both
    // sides.
    let target = msg("guid:SYNTH-VIEW-MSG-0001");
    let p = r
        .thread_page(
            chat1,
            Anchor::Message {
                id: target,
                include: true,
            },
            Direction::Around,
            9,
            None,
            none,
        )
        .expect("around");
    let f = p.focus.expect("focus");
    assert_eq!((f.id, f.reason), (target, FocusReason::Anchor));
    let at = p.items.iter().position(|m| m.id == target).expect("anchor");
    assert!(at > 0 && at + 1 < p.items.len(), "neighbours on both sides");
    assert!(p.items.len() <= 9);
    assert!(hint(&p, target).is_some(), "the focus key is served");

    // Around a folded reaction: the page centres on its target.
    let love = msg("guid:SYNTH-VIEW-MSG-0003");
    let p = r
        .thread_page(
            chat1,
            Anchor::Message {
                id: love,
                include: true,
            },
            Direction::Around,
            5,
            None,
            none,
        )
        .expect("around a reaction");
    let f = p.focus.expect("focus");
    assert_eq!((f.id, f.reason), (target, FocusReason::ReactionTarget));
    assert!(
        p.items.iter().all(|m| m.id != love),
        "a folded row is no item"
    );

    // `include` puts the anchor on an Older page; without it, it is not.
    let older = |include: bool| {
        r.thread_page(
            chat1,
            Anchor::Message {
                id: target,
                include,
            },
            Direction::Older,
            3,
            None,
            none,
        )
        .expect("older")
    };
    assert_eq!(older(true).items.last().map(|m| m.id), Some(target));
    assert!(older(false).items.iter().all(|m| m.id != target));
    assert!(older(false).has_newer, "the anchor itself is newer");

    // An anchor from another chat, or no message at all.
    let other = ChatId::new(by_name(&n.chats, "guid:iMessage;+;chatSYNTHVIEW2"));
    for (chat, id) in [(other, target), (chat1, MessageId::new(9_999_999))] {
        let e = r
            .thread_page(
                chat,
                Anchor::Message { id, include: true },
                Direction::Older,
                10,
                None,
                none,
            )
            .expect_err("not in this chat");
        assert!(matches!(e, ArchiveError::AnchorNotFound), "{e:?}");
    }
    // Page sizes are clamped to 1..=100.
    let p = r
        .thread_page(chat1, Anchor::Newest, Direction::Older, 0, None, none)
        .expect("one");
    assert_eq!(p.items.len(), 1);
    let p = r
        .thread_page(chat1, Anchor::Newest, Direction::Older, 500, None, none)
        .expect("many");
    assert!(p.items.len() <= 100);

    // The reply thread: the originator, then its reply.
    let t = r.reply_thread(target, none).expect("reply thread");
    assert_eq!(t.range, PageRange::Replies);
    let names: Vec<&str> = t
        .items
        .iter()
        .map(|m| n.messages[&m.id.get()].as_str())
        .collect();
    assert_eq!(
        names,
        ["guid:SYNTH-VIEW-MSG-0001", "guid:SYNTH-VIEW-MSG-0024"]
    );
    assert!(!t.has_newer);
    assert!(matches!(
        t.items[1].reply,
        Some(ReplyRef::Resolved { originator, .. }) if originator == target
    ));
    // An unkeyed message has no replies; a missing one is not found.
    let unkeyed = MessageId::new(by_name(&n.messages, "unkeyed:message:1:0"));
    assert_eq!(
        r.reply_thread(unkeyed, none).expect("unkeyed").items.len(),
        1
    );
    assert!(matches!(
        r.reply_thread(MessageId::new(9_999_999), none),
        Err(ArchiveError::AnchorNotFound)
    ));
}

/// The reply thread keeps the first replies in timeline order, not in id
/// (import) order, and `has_newer` is true only of later replies.
#[test]
fn the_reply_thread_keeps_the_earliest_replies_by_date() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    {
        let mut a = lib.open(&id).expect("open");
        let mut spec = BulkSpec::new(560);
        spec.chats = 1;
        spec.reaction_permille = 0;
        spec.reply_permille = 0;
        spec.undated_permille = 0;
        generate(&mut a, &spec);
    }
    let db = db_path(&tmp, &id);
    let (originator, want): (i64, Vec<i64>) = {
        let c = raw(&db);
        let originator: i64 = c
            .query_row(
                "SELECT min(id) FROM message WHERE key_kind = 1 AND assoc_present = 0 \
                   AND item_kind = 1",
                [],
                |r| r.get(0),
            )
            .expect("originator");
        c.execute(
            "UPDATE message SET thread_originator_guid = \
               (SELECT guid FROM message WHERE id = ?1), thread_originator_guid_st = NULL \
             WHERE id <> ?1 AND assoc_present = 0 AND item_kind = 1",
            [originator],
        )
        .expect("replies");
        let mut replies: Vec<i64> = c
            .prepare(
                "SELECT id FROM message WHERE id <> ?1 AND assoc_present = 0 \
                   AND item_kind = 1 ORDER BY id",
            )
            .expect("prepare")
            .query_map([originator], |r| r.get(0))
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("ids");
        assert!(replies.len() > REPLY_THREAD_LIMIT + 30, "{}", replies.len());
        // The 30 newest imports are dated before every other reply.
        let late: Vec<i64> = replies.split_off(replies.len() - 30);
        for (k, m) in late.iter().enumerate() {
            let ns = 1_000 + i64::try_from(k).expect("k");
            let mut key = vec![0u8];
            key.extend_from_slice(
                &(u64::from_be_bytes(ns.to_be_bytes()) ^ (1 << 63)).to_be_bytes(),
            );
            c.execute(
                "UPDATE message SET sort_key = ?2 WHERE id = ?1",
                rusqlite::params![m, key],
            )
            .expect("key");
        }
        let mut want = late;
        want.extend(replies);
        want.truncate(REPLY_THREAD_LIMIT);
        (originator, want)
    };
    let r = lib.open_read(&id).expect("reader");
    let t = r
        .reply_thread(MessageId::new(originator), Reveal::default())
        .expect("reply thread");
    assert!(t.has_newer, "more replies, all later than these");
    let got: Vec<i64> = t.items.iter().skip(1).map(|m| m.id.get()).collect();
    assert!(!got.is_empty());
    assert_eq!(got, want[..got.len()].to_vec(), "earliest replies first");
}

/// A `Scan` that used up the call's deadline (no op budget involved) still
/// hands its items to an `Assembly` phase that has time of its own (§3.2),
/// for a page and for the reply thread.
#[test]
fn a_scan_that_used_up_the_deadline_still_assembles_its_items() {
    let (i, n) = main_entry();
    let r = i.lib.open_read(&i.id).expect("reader");
    let chat = ChatId::new(*n.chats.keys().next().expect("a chat"));
    let normal = r
        .thread_page(
            chat,
            Anchor::Newest,
            Direction::Older,
            5,
            None,
            Reveal::default(),
        )
        .expect("page");
    let _f = install(Faults {
        page_window_ms: Some(50),
        pause_after_scan_ms: Some(120),
        ..Faults::default()
    });
    let late = r
        .thread_page(
            chat,
            Anchor::Newest,
            Direction::Older,
            5,
            None,
            Reveal::default(),
        )
        .expect("assembled after a late scan");
    assert_eq!(
        late.items.iter().map(|m| m.id).collect::<Vec<_>>(),
        normal.items.iter().map(|m| m.id).collect::<Vec<_>>()
    );
    let first = normal.items[0].id;
    let t = r
        .reply_thread(first, Reveal::default())
        .expect("reply thread after a late scan");
    assert_eq!(t.items[0].id, first);
}

#[test]
fn a_broad_page_is_too_broad_and_a_tripped_assembly_keeps_its_items() {
    let (i, n) = main_entry();
    let r = i.lib.open_read(&i.id).expect("reader");
    // The target of 201 reactions: its chip read is the one long
    // statement of the page.
    let target = MessageId::new(by_name(&n.messages, "guid:SYNTH-VIEW-MSG-0090"));
    let chat = ChatId::new(
        *n.chats
            .iter()
            .find(|(id, _)| {
                r.thread_page(
                    ChatId::new(**id),
                    Anchor::Message {
                        id: target,
                        include: true,
                    },
                    Direction::Older,
                    1,
                    None,
                    Reveal::default(),
                )
                .is_ok()
            })
            .expect("its chat")
            .0,
    );
    let full = r
        .thread_page(
            chat,
            Anchor::Message {
                id: target,
                include: true,
            },
            Direction::Older,
            1,
            None,
            Reveal::default(),
        )
        .expect("page");
    assert!(full.items[0].reactions.as_ref().expect("rows").rows.capped);
    {
        let _f = install(Faults {
            view_op_budget: Some(0),
            ..Faults::default()
        });
        let p = r
            .thread_page(
                chat,
                Anchor::Message {
                    id: target,
                    include: true,
                },
                Direction::Older,
                1,
                None,
                Reveal::default(),
            )
            .expect("the items it has");
        assert!(p.extras_incomplete);
        assert_eq!(p.items.len(), 1);
        assert!(!p.items[0].extras && p.items[0].reactions.is_none());
    }
    // A scan whose first statement trips has no item: too broad.
    let (_tmp, lib) = library();
    let id = lib.create(None).expect("create");
    {
        let mut a = lib.open(&id).expect("open");
        let mut spec = BulkSpec::new(3_000);
        spec.chats = 2;
        spec.big_chat_permille = 900;
        generate(&mut a, &spec);
    }
    let r = lib.open_read(&id).expect("reader");
    let big = r
        .conversation_index(ConversationSort::Count, None)
        .expect("index")
        .ids[0];
    {
        let _f = install(Faults {
            view_op_budget: Some(0),
            ..Faults::default()
        });
        match r.thread_page(
            big,
            Anchor::Newest,
            Direction::Older,
            100,
            None,
            Reveal::default(),
        ) {
            Err(e) => assert!(matches!(e, ArchiveError::QueryTooBroad), "{e:?}"),
            Ok(p) => assert!(p.partial || p.extras_incomplete, "a trip shows"),
        }
    }
    // The reader is usable again at once.
    assert!(!r
        .thread_page(
            big,
            Anchor::Newest,
            Direction::Older,
            100,
            None,
            Reveal::default()
        )
        .expect("page")
        .items
        .is_empty());
}

// ---------------------------------------------------------------------------
// Bulk: keyset paging against an oracle
// ---------------------------------------------------------------------------

/// The visible rows of `chat` in one range, oldest first, from SQL alone:
/// timeline order, without reactions whose keyed target is in the chat.
fn oracle(c: &rusqlite::Connection, chat: i64, dated: bool) -> Vec<i64> {
    let bound = if dated {
        "cm.sort_key < X'01'"
    } else {
        "cm.sort_key >= X'01'"
    };
    c.prepare(&format!(
        "SELECT cm.message_id FROM chat_message cm JOIN message m ON m.id = cm.message_id \
         WHERE cm.chat_id = ?1 AND {bound} AND NOT (m.assoc_present = 1 \
           AND (coalesce(m.assoc_kind, 0) BETWEEN 10 AND 17 \
             OR coalesce(m.assoc_kind, 0) BETWEEN 20 AND 27) \
           AND EXISTS (SELECT 1 FROM message t JOIN chat_message tc \
                       ON tc.message_id = t.id AND tc.chat_id = cm.chat_id \
                       WHERE t.key_kind = 1 AND t.guid = m.assoc_target_guid)) \
         ORDER BY cm.sort_key, cm.message_id"
    ))
    .expect("prepare")
    .query_map([chat], |r| r.get(0))
    .expect("query")
    .collect::<Result<_, _>>()
    .expect("rows")
}

fn bulk(
    messages: usize,
) -> (
    tempfile::TempDir,
    oma_archive::Library,
    oma_archive::ArchiveId,
) {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    {
        let mut a = lib.open(&id).expect("open");
        let mut spec = BulkSpec::new(messages);
        spec.chats = 4;
        spec.big_chat_permille = 700;
        spec.undated_permille = 30;
        spec.reaction_permille = 120;
        let rep = generate(&mut a, &spec);
        assert!(rep.reactions > 100 && rep.undated > 10, "{rep:?}");
    }
    (tmp, lib, id)
}

#[test]
fn keyset_pages_return_every_visible_row_once_in_order() {
    let (tmp, lib, id) = bulk(4_000);
    let r = lib.open_read(&id).expect("reader");
    let c = read_only(&db_path(&tmp, &id));
    let chats = r
        .conversation_index(ConversationSort::Count, None)
        .expect("index")
        .ids;
    let mut folded_somewhere = false;
    for chat in &chats {
        let dated = oracle(&c, chat.get(), true);
        let undated = oracle(&c, chat.get(), false);
        let all: i64 = c
            .query_row(
                "SELECT count(*) FROM chat_message WHERE chat_id = ?1",
                [chat.get()],
                |r| r.get(0),
            )
            .expect("count");
        let all = usize::try_from(all).expect("count");
        folded_somewhere |= dated.len() + undated.len() < all;
        let (older, _) = page_all(
            &r,
            *chat,
            Anchor::Newest,
            Direction::Older,
            50,
            Reveal::default(),
        );
        let (newer, _) = page_all(
            &r,
            *chat,
            Anchor::Oldest,
            Direction::Newer,
            37,
            Reveal::default(),
        );
        let (und, _) = page_all(
            &r,
            *chat,
            Anchor::Undated,
            Direction::Newer,
            20,
            Reveal::default(),
        );
        assert_eq!(ids(&older), dated, "chat {chat:?}: newest to oldest");
        assert_eq!(ids(&newer), dated, "chat {chat:?}: oldest to newest");
        assert_eq!(ids(&und), undated, "chat {chat:?}: undated");
        // Around any visible row: it is the focus, and its neighbours are
        // the oracle's.
        for (k, &anchor) in dated.iter().enumerate().step_by(97) {
            let p = r
                .thread_page(
                    *chat,
                    Anchor::Message {
                        id: MessageId::new(anchor),
                        include: true,
                    },
                    Direction::Around,
                    11,
                    None,
                    Reveal::default(),
                )
                .expect("around");
            let at = p
                .items
                .iter()
                .position(|m| m.id.get() == anchor)
                .expect("the anchor");
            let start = k - at;
            assert_eq!(
                ids(&p.items),
                dated[start..start + p.items.len()],
                "chat {chat:?}: around {anchor}"
            );
            assert_eq!(p.has_older, start > 0);
            assert_eq!(p.has_newer, start + p.items.len() < dated.len());
        }
    }
    assert!(folded_somewhere, "the bulk archive folds reactions");
}

// ---------------------------------------------------------------------------
// Concurrency with an import (§13.3)
// ---------------------------------------------------------------------------

#[test]
fn a_moved_anchor_is_paged_from_its_remembered_key() {
    let (tmp, lib, id) = bulk(3_000);
    let r = lib.open_read(&id).expect("reader");
    let c = read_only(&db_path(&tmp, &id));
    let chat = r
        .conversation_index(ConversationSort::Count, None)
        .expect("index")
        .ids[0];
    let before = oracle(&c, chat.get(), true);
    assert!(before.len() > 300);
    let first = r
        .thread_page(
            chat,
            Anchor::Newest,
            Direction::Older,
            20,
            None,
            Reveal::default(),
        )
        .expect("newest");
    let a = first.older_anchor.expect("anchor");
    let kept_hint = hint(&first, a).expect("served");
    // A higher-ranked import changes the anchor's date: it moves 60 rows
    // older (the `message_sort_au` trigger moves its timeline rows).
    let pos = before.iter().position(|&m| m == a.get()).expect("anchor");
    let landing = before[pos - 60];
    {
        let w = raw(&db_path(&tmp, &id));
        let (key, ns): (Vec<u8>, i64) = w
            .query_row(
                "SELECT sort_key, date_utc_ns FROM message WHERE id = ?1",
                [landing],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("landing");
        let new_key = oma_archive::hooks::sort_key(Some(ns - 1), &[0u8; 41], 0);
        assert!(new_key < key);
        w.execute(
            "UPDATE message SET sort_key = ?1, date_utc_ns = ?2 WHERE id = ?3",
            rusqlite::params![new_key, ns - 1, a.get()],
        )
        .expect("move");
    }
    let after = oracle(&c, chat.get(), true);
    // Paging older from the remembered key: no row is skipped.
    let mut seen: Vec<i64> = ids(&first.items);
    let mut anchor = a;
    let mut h = Some(kept_hint.clone());
    let mut moved_reported = false;
    loop {
        let p = r
            .thread_page(
                chat,
                Anchor::Message {
                    id: anchor,
                    include: false,
                },
                Direction::Older,
                25,
                h.as_ref(),
                Reveal::default(),
            )
            .expect("older");
        moved_reported |= p.anchor_moved;
        let mut page_ids = ids(&p.items);
        let unique: BTreeSet<i64> = page_ids.iter().copied().collect();
        assert_eq!(unique.len(), page_ids.len(), "no duplicates within a page");
        page_ids.reverse();
        seen.extend(page_ids);
        if !p.has_older {
            break;
        }
        anchor = p.older_anchor.expect("anchor");
        h = hint(&p, anchor);
    }
    assert!(moved_reported, "anchor_moved is set");
    let mut counts: BTreeMap<i64, usize> = BTreeMap::new();
    for m in &seen {
        *counts.entry(*m).or_default() += 1;
    }
    for m in &after {
        let n = counts.get(m).copied().unwrap_or(0);
        if *m == a.get() {
            // On the first page at its old place, and again at its new one.
            assert_eq!(n, 2, "the moved anchor, once per position");
        } else {
            assert_eq!(n, 1, "row {m} exactly once");
        }
    }
    assert_eq!(counts.len(), after.len());
    // Without the remembered key, paging starts at the anchor's new place
    // and the rows between its two positions are not on the next page.
    let p = r
        .thread_page(
            chat,
            Anchor::Message {
                id: a,
                include: false,
            },
            Direction::Older,
            25,
            None,
            Reveal::default(),
        )
        .expect("current key");
    assert!(!p.anchor_moved);
    let new_pos = after.iter().position(|&m| m == a.get()).expect("moved");
    assert_eq!(p.items.last().map(|m| m.id.get()), Some(after[new_pos - 1]));
    assert!(p
        .items
        .iter()
        .all(|m| !before[pos - 59..pos].contains(&m.id.get())));
}

#[test]
fn reads_during_a_write_see_the_committed_snapshot_and_never_lock() {
    let (tmp, lib, id) = bulk(1_500);
    let r = lib.open_read(&id).expect("reader");
    let chat = r
        .conversation_index(ConversationSort::Count, None)
        .expect("index")
        .ids[0];
    let newest = |r: &ArchiveReader| {
        ids(&r
            .thread_page(
                chat,
                Anchor::Newest,
                Direction::Older,
                10,
                None,
                Reveal::default(),
            )
            .expect("page never Locked")
            .items)
    };
    let committed = newest(&r);
    // An import holds the lock, its writer is open and a batch is in
    // flight: it moves the oldest dated row to the newest position.
    let mut archive = lib.open(&id).expect("open");
    archive.begin_write(Recovery::Run).expect("begin_write");
    let w = raw(&db_path(&tmp, &id));
    let c = read_only(&db_path(&tmp, &id));
    let dated = oracle(&c, chat.get(), true);
    let oldest = dated[0];
    w.execute_batch("BEGIN IMMEDIATE").expect("begin");
    let key = oma_archive::hooks::sort_key(Some(i64::MAX - 1), &[0u8; 41], 0);
    w.execute(
        "UPDATE message SET sort_key = ?1 WHERE id = ?2",
        rusqlite::params![key, oldest],
    )
    .expect("move");
    assert_eq!(newest(&r), committed, "uncommitted rows are not seen");
    let r2 = lib.open_read(&id).expect("a reader while locked");
    assert_eq!(newest(&r2), committed);
    w.execute_batch("COMMIT").expect("commit");
    let now = newest(&r);
    assert_eq!(now.last(), Some(&oldest), "the committed batch is seen");
    assert_eq!(
        now.iter().filter(|&&m| m == oldest).count(),
        1,
        "no duplicates within a page"
    );
    drop(r2);
    drop(w);
    archive.close();
}

#[test]
fn a_reader_never_holds_back_the_truncating_checkpoint() {
    let (tmp, lib, id) = bulk(1_000);
    let r = lib.open_read(&id).expect("reader");
    let chat = r
        .conversation_index(ConversationSort::Count, None)
        .expect("index")
        .ids[0];
    let first = r
        .thread_page(
            chat,
            Anchor::Newest,
            Direction::Older,
            30,
            None,
            Reveal::default(),
        )
        .expect("page");
    // A write puts frames in the WAL while the reader stays open.
    let w = raw(&db_path(&tmp, &id));
    w.execute(
        "UPDATE message SET sort_key = sort_key WHERE id = ?1",
        [first.items[0].id.get()],
    )
    .expect("write");
    w.execute(
        "UPDATE chat_stats SET message_count = message_count WHERE chat_id = ?1",
        [chat.get()],
    )
    .expect("write");
    let reply = r.reply_thread(first.items[0].id, Reveal::default());
    assert!(reply.is_ok());
    let (busy, _, _): (i64, i64, i64) = w
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .expect("checkpoint");
    assert_eq!(busy, 0, "no read transaction outlives a call");
    let wal = archive_dir(&tmp, &id).join("archive.sqlite-wal");
    assert_eq!(std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0), 0);
    // And the reader still reads.
    r.thread_page(
        chat,
        Anchor::Newest,
        Direction::Older,
        30,
        None,
        Reveal::default(),
    )
    .expect("page after the checkpoint");
}
