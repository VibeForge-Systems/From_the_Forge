//! Message detail, `message_text`, the reaction history, attachment facts
//! and cards, date seek and day counts (M5 spec §3.5–§3.9, §5.9, §13.2,
//! slice 5b3).
//!
//! - **The viewer catalogue:** for every entry and scenario, every message
//!   of the Python model is read with `message_detail`, and its stored
//!   versions (grouped by text), its `ec` texts decoded at read time (owner
//!   Q3a), whether earlier text is withheld and why, its value history and
//!   the reply's unkeyed identifier probe match the model's `viewer` facts
//!   and the M4 manifest (`bodies`, `history`), never the Rust output.
//!   Every attachment's observations, sizes, flags and card (§5.9) match
//!   the model's.
//! - **Masking and retraction (D115, owner Q3b) across every command:** no
//!   one-time code and no earlier text of the model's `masked` lists
//!   appears in any conversation preview, thread page, reply thread,
//!   detail, `message_text` chunk or reaction history unless the request
//!   reveals it; revealed, every one does. `Debug` prints no content.
//! - **Dates:** `seek_date` and `day_counts` match the model's dated items
//!   (folded reactions skipped, undated rows never returned or counted),
//!   and an oracle on a bulk archive.
//! - **Cards an import cannot produce** are built by editing a copy: no
//!   observation, a stale verify report after `Replaced`, keys that do not
//!   unwrap, and a file too large to copy.
//! - Budgets give `QueryTooBroad` (never `Damaged`); reads write nothing.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;

use std::collections::{BTreeMap, BTreeSet, HashMap};

use common::bulk::{generate, BulkSpec};
use common::import::read_only;
use common::viewer::{import_order, model_docs, names, Imported, Names};
use common::*;
use oma_archive::hooks::{install, Faults};
use oma_archive::schema::codes::{BlobVerifyState, LookupCode};
use oma_archive::{
    Anchor, ArchiveError, ArchiveReader, AssembledMessage, AttachmentFacts, AttachmentId, BodyId,
    BodyView, CardState, ChatId, ChunkText, ConversationSort, Direction, EarlierText,
    EditEntryText, HistoryColumn, IdentifierProbe, MessageDetail, MessageId, OptContent,
    PreviewBody, ReplyRef, Reveal, SeekDir, SegmentKind, TextPiece, VersionText, WithheldReason,
    DAY_BOUNDS, REACTION_HISTORY_PAGE, TEXT_CHUNK_UTF16,
};
use serde_json::Value;

const REVEAL_ALL: Reveal = Reveal {
    earlier_text: true,
    one_time_code: true,
};

// ---------------------------------------------------------------------------
// Text collectors (what a DTO would carry)
// ---------------------------------------------------------------------------

fn pieces_text(p: &[TextPiece], out: &mut String) {
    for piece in p {
        if let TextPiece::Text(t) = piece {
            out.push_str(t.text.expose());
        }
    }
}

fn opt_text(o: &OptContent, out: &mut String) {
    if let OptContent::Text(t) = o {
        out.push_str(t.text.expose());
        out.push('\u{0}');
    }
}

fn preview_text(p: &PreviewBody, out: &mut String) {
    if let PreviewBody::Text(pieces) = p {
        pieces_text(pieces, out);
    }
}

fn message_text_of(m: &AssembledMessage, out: &mut String) {
    if let BodyView::Text(b) = &m.body {
        for s in &b.segments {
            if let SegmentKind::Text(t) = &s.kind {
                out.push_str(t.text.expose());
            }
        }
    }
    if let EarlierText::Revealed { text, .. } = &m.edit.earlier {
        pieces_text(text, out);
    }
    if let Some(ReplyRef::Resolved { quote, .. } | ReplyRef::InAnotherConversation { quote, .. }) =
        &m.reply
    {
        preview_text(quote, out);
    }
    out.push('\u{0}');
}

fn version_text(v: &VersionText) -> Option<String> {
    match v {
        VersionText::Text(p) => {
            let mut s = String::new();
            pieces_text(p, &mut s);
            Some(s)
        }
        _ => None,
    }
}

fn detail_text(d: &MessageDetail) -> String {
    let mut out = String::new();
    message_text_of(&d.message, &mut out);
    for v in &d.versions {
        if let VersionText::Text(p) = &v.text {
            pieces_text(p, &mut out);
            out.push('\u{0}');
        }
        for e in &v.edits {
            if let EditEntryText::Text(p) = &e.text {
                pieces_text(p, &mut out);
                out.push('\u{0}');
            }
        }
    }
    opt_text(&d.subject, &mut out);
    opt_text(&d.account, &mut out);
    for h in d
        .history
        .iter()
        .chain(d.chats.iter().flat_map(|c| c.history.iter()))
    {
        opt_text(&h.value, &mut out);
    }
    for a in &d.attachments {
        opt_text(&a.name, &mut out);
        opt_text(&a.emoji_description, &mut out);
    }
    out
}

fn chunk_text(r: &ArchiveReader, body: BodyId, reveal: Reveal) -> (String, bool) {
    let mut out = String::new();
    let mut from = 0u32;
    loop {
        let c = r.message_text(body, from, reveal).expect("chunk");
        assert_eq!(c.from_utf16, from, "chunks continue where they ended");
        match &c.text {
            ChunkText::Text(p) => pieces_text(p, &mut out),
            ChunkText::Withheld(_) => return (out, true),
            _ => return (out, false),
        }
        assert!(c.end_utf16 - c.from_utf16 <= TEXT_CHUNK_UTF16);
        if c.end_utf16 >= c.full_utf16 {
            return (out, false);
        }
        from = c.end_utf16;
    }
}

/// The manifest's long-text rule: a text over 4,096 bytes is its digest
/// and length.
fn long_form(t: String) -> String {
    use sha2::{Digest, Sha256};
    if t.len() > 4096 {
        format!("sha256:{}:{}", hex(&Sha256::digest(t.as_bytes())), t.len())
    } else {
        t
    }
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn reverse(map: &HashMap<i64, String>) -> HashMap<String, i64> {
    map.iter().map(|(k, v)| (v.clone(), *k)).collect()
}

// ---------------------------------------------------------------------------
// Attachment names
// ---------------------------------------------------------------------------

fn observation_name(l: Option<LookupCode>) -> String {
    use LookupCode as L;
    match l {
        Some(L::NotLookedUp) => "copyOff",
        Some(L::NotAttemptedNull) => "notAttempted:null",
        Some(L::NotAttemptedTooLong) => "notAttempted:tooLong",
        Some(L::NotAttemptedUnusualName) => "notAttempted:unusualName",
        Some(L::NotAttemptedClass) => "notAttempted:class",
        Some(L::NotAttemptedUnrecognized) => "notAttempted:unrecognized",
        Some(L::NotAttemptedColumnAbsent) => "notAttempted:columnAbsent",
        Some(L::NotAttemptedUnreadable) => "notAttempted:unreadable",
        Some(L::Present | L::SizeMismatch) => "present",
        Some(L::BlobMissing) => "blob:missing",
        Some(L::NotListedAtSourcedKey) => "notListedAtSourcedKey",
        Some(L::NotFoundAtAssumedKey) => "notFoundAtAssumedKey",
        other => return format!("?{other:?}"),
    }
    .to_owned()
}

/// The model's card name, with every kind of a present file as `file`
/// (which kind is the sniff's, slice 5d).
fn card_name(c: &CardState) -> &'static str {
    match c {
        CardState::File { .. } => "file",
        CardState::NoObservation => "noObservation",
        CardState::NotInBackup => "notInBackup",
        CardState::NotFoundAtAssumedKey => "notFoundAtAssumedKey",
        CardState::NotCopied => "notCopied",
        CardState::ClassNotLookedUp => "classNotLookedUp",
        CardState::LocationUnusable(_) => "locationUnusable",
        CardState::NotDecrypted => "notDecrypted",
        CardState::CouldNotBeRead => "couldNotBeRead",
        CardState::TooLarge => "tooLarge",
        _ => "unavailable",
    }
}

fn model_card(w: &Value) -> String {
    let c = w["card"].as_str().expect("card");
    match c {
        "image" | "play" | "unrecognised" => "file".to_owned(),
        _ if c.starts_with("file:") => "file".to_owned(),
        _ => c.to_owned(),
    }
}

fn compare_attachment(at: &str, a: &AttachmentFacts, w: &Value) {
    assert_eq!(card_name(&a.card), model_card(w), "{at}: card");
    let obs: Vec<String> = a
        .observations
        .iter()
        .map(|o| observation_name(o.lookup))
        .collect();
    assert_eq!(obs, strings(&w["observations"]), "{at}: observations");
    assert_eq!(a.declared_bytes.value, w["declaredBytes"].as_i64(), "{at}");
    assert_eq!(
        a.blob.map(|b| b.stored_bytes),
        w["storedBytes"].as_i64(),
        "{at}: stored bytes"
    );
    assert_eq!(a.sticker.value == Some(1), w["isSticker"] == true, "{at}");
    assert_eq!(
        a.hide_attachment.value == Some(1),
        w["hideAttachment"] == true,
        "{at}: hide_attachment raw"
    );
    if let OptContent::Text(t) = &a.name {
        assert_eq!(t.bidi_controls, w["nameBidiControls"] == true, "{at}");
        assert_eq!(t.invisible, w["nameInvisible"] == true, "{at}");
    }
    if let Some(older) = w["fromOlderBackup"].as_bool() {
        let CardState::File { older_backup } = a.card else {
            panic!("{at}: a file");
        };
        assert_eq!(older_backup.is_some(), older, "{at}: from an older backup");
        if let Some(o) = older_backup {
            assert_ne!(o.held_by, o.newest, "{at}");
            assert_eq!(a.blob.map(|b| b.held_by), Some(o.held_by), "{at}");
        }
    }
    assert!(a.verify.is_none(), "{at}: never verified");
}

// ---------------------------------------------------------------------------
// The catalogue
// ---------------------------------------------------------------------------

/// The model's version groups: consecutive versions with the same text.
fn groups(d: &MessageDetail) -> Vec<(bool, u64)> {
    let mut out: Vec<(Option<String>, u64)> = Vec::new();
    for v in &d.versions {
        let t = version_text(&v.text);
        match out.last_mut() {
            Some((last, n)) if *last == t => *n += 1,
            _ => out.push((t, 1)),
        }
    }
    out.into_iter().map(|(t, n)| (t.is_some(), n)).collect()
}

fn reason_name(r: WithheldReason) -> &'static str {
    match r {
        WithheldReason::Unsent => "unsent",
        WithheldReason::EarlierVersion => "earlierVersion",
        WithheldReason::NoCurrentText => "noCurrentText",
        _ => "?",
    }
}

fn history_texts(h: &[oma_archive::HistoryValue], col: HistoryColumn) -> BTreeSet<String> {
    h.iter()
        .filter(|v| v.column == col)
        .map(|v| match &v.value {
            OptContent::Text(t) => t.text.expose().clone(),
            OptContent::Missing(s) => format!("<{s:?}>"),
            _ => "?".to_owned(),
        })
        .collect()
}

fn manifest_history(row: &Value, col: &str) -> BTreeSet<String> {
    row["history"][col]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|v| match v["text"].as_str() {
                    Some(t) => t.to_owned(),
                    None => "<Null>".to_owned(),
                })
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Default)]
struct Seen {
    ec_texts: usize,
    withheld: BTreeSet<String>,
    shown_earlier: usize,
    groups_unchanged: usize,
    history: usize,
    probes: BTreeSet<String>,
    cards: BTreeSet<String>,
    older: usize,
}

#[allow(clippy::too_many_lines)]
fn compare_detail(
    at: &str,
    r: &ArchiveReader,
    id: i64,
    w: &Value,
    manifest: Option<&Value>,
    n: &Names,
    seen: &mut Seen,
) {
    let d = r
        .message_detail(MessageId::new(id), Reveal::default())
        .expect("detail");
    let all = r
        .message_detail(MessageId::new(id), REVEAL_ALL)
        .expect("revealed detail");
    assert_eq!(d.message.id.get(), id);
    let e = &w["edit"];
    let stored = u64::try_from(d.versions.len()).expect("len") + d.more_versions;
    assert_eq!(Some(stored), e["storedVersions"].as_u64(), "{at}: versions");
    assert_eq!(
        d.versions.iter().filter(|v| v.current).count(),
        usize::from(!matches!(d.message.body, BodyView::NoBody)),
        "{at}: one current version"
    );
    if let Some(want) = e["versionGroups"].as_array() {
        let want: Vec<(bool, u64)> = want
            .iter()
            .map(|g| {
                (
                    g["hasText"] == true,
                    g["versions"].as_u64().expect("versions"),
                )
            })
            .collect();
        assert_eq!(groups(&all), want, "{at}: version groups");
        for (i, v) in all.versions.iter().enumerate().skip(1) {
            let same = version_text(&v.text).is_some()
                && version_text(&v.text) == version_text(&all.versions[i - 1].text);
            assert_eq!(v.text_unchanged, same, "{at}: text unchanged");
            seen.groups_unchanged += usize::from(same);
        }
    }
    // The bodies of the M4 manifest, as captured versions.
    if let Some(row) = manifest {
        let mut want: Vec<(bool, Option<String>)> = row["bodies"]
            .as_array()
            .expect("bodies")
            .iter()
            .map(|b| (b["current"] == true, b["text"].as_str().map(str::to_owned)))
            .collect();
        let mut got: Vec<(bool, Option<String>)> = all
            .versions
            .iter()
            .map(|v| {
                let t =
                    version_text(&v.text).map(|_| long_form(chunk_text(r, v.body, REVEAL_ALL).0));
                (v.current, t)
            })
            .collect();
        want.sort();
        got.sort();
        assert_eq!(got, want, "{at}: captured versions");
        for (col, hc) in [
            ("subject", HistoryColumn::Subject),
            ("group_title", HistoryColumn::GroupTitle),
            ("destination_caller_id", HistoryColumn::DestinationCallerId),
        ] {
            let want = manifest_history(row, col);
            seen.history += want.len();
            assert_eq!(history_texts(&all.history, hc), want, "{at}: history {col}");
        }
    }
    // Edit texts decoded at read time (Q3a), on the current version.
    if let Some(want) = e["ecTexts"].as_array() {
        let current = all.versions.iter().find(|v| v.current).expect("current");
        let got: Vec<(i64, i64, Option<String>)> = current
            .edits
            .iter()
            .map(|x| {
                let t = match &x.text {
                    EditEntryText::Text(p) => {
                        let mut s = String::new();
                        pieces_text(p, &mut s);
                        Some(s)
                    }
                    EditEntryText::NoBody => None,
                    other => Some(format!("<{other:?}>")),
                };
                (x.part, x.ordinal, t)
            })
            .collect();
        let want: Vec<(i64, i64, Option<String>)> = want
            .iter()
            .map(|x| {
                (
                    x["part"].as_i64().expect("part"),
                    x["ordinal"].as_i64().expect("ordinal"),
                    x["text"].as_str().map(str::to_owned),
                )
            })
            .collect();
        assert_eq!(got, want, "{at}: ec texts");
        seen.ec_texts += want.len();
    }
    // Earlier text: withheld (and why) or shown, in the versions.
    let current = d.versions.iter().find(|v| v.current).map(|v| v.body);
    let earlier: Vec<&VersionText> = d
        .versions
        .iter()
        .filter(|v| Some(v.body) != current)
        .map(|v| &v.text)
        .collect();
    match e["earlierText"].as_str() {
        Some("withheld") => {
            let reason = e["earlierTextReason"].as_str().expect("reason");
            assert!(
                earlier
                    .iter()
                    .any(|t| matches!(t, VersionText::Withheld(r) if reason_name(*r) == reason)),
                "{at}: withheld ({reason})"
            );
            assert!(
                earlier.iter().all(|t| !matches!(t, VersionText::Text(_))),
                "{at}: no earlier text shown"
            );
            seen.withheld.insert(reason.to_owned());
        }
        Some("shown") => {
            assert!(
                earlier.iter().any(|t| matches!(t, VersionText::Text(_))),
                "{at}: earlier text shown"
            );
            seen.shown_earlier += 1;
        }
        _ => {}
    }
    // The reply's unkeyed identifier probe.
    match w["reply"]["state"].as_str() {
        Some("notUniquelyIdentified") => {
            let want = if w["reply"]["unkeyedShareIdentifier"] == true {
                IdentifierProbe::SharedByUnkeyed
            } else {
                IdentifierProbe::NotInArchive
            };
            assert_eq!(d.reply_probe, Some(want), "{at}: probe");
            seen.probes.insert(format!("{want:?}"));
        }
        _ => assert_eq!(d.reply_probe, None, "{at}: no probe"),
    }
    if let Some(p) = d.reaction_target_probe {
        assert_ne!(p, IdentifierProbe::NotChecked, "{at}");
        seen.probes.insert(format!("target {p:?}"));
    }
    // The detail's attachments are the message's, with their facts.
    for a in &d.attachments {
        assert_eq!(&r.attachment(a.id).expect("attachment"), a, "{at}");
        let _ = &n.attachments[&a.id.get()];
    }
    // Debug prints no content.
    let shown = format!("{d:?} {all:?}");
    assert!(!shown.contains("SYNTH"), "{at}: Debug carries content");
}

#[test]
fn details_and_cards_match_the_model_for_every_entry_and_scenario() {
    let mut seen = Seen::default();
    for (what, model) in model_docs() {
        let order = &model["orders"][0];
        let i = import_order(order, &what);
        let n = names(&i, order);
        let before = data_snapshot(&i.tmp, &i.id);
        let r = i.lib.open_read(&i.id).expect("reader");
        let ids = reverse(&n.messages);
        let manifest: HashMap<String, &Value> = model["content"]["messages"]
            .as_array()
            .expect("messages")
            .iter()
            .map(|m| (m["ref"].as_str().expect("ref").to_owned(), m))
            .collect();
        for (name, w) in model["viewer"]["messages"].as_object().expect("messages") {
            let at = format!("{what}: {name}");
            let id = ids[name.as_str()];
            compare_detail(
                &at,
                &r,
                id,
                w,
                manifest.get(name.as_str()).copied(),
                &n,
                &mut seen,
            );
        }
        // Chat name history (the conversation info), from any message's
        // detail.
        for chat in model["content"]["chats"].as_array().expect("chats") {
            let want = manifest_history(chat, "display_name");
            if want.is_empty() {
                continue;
            }
            let chat_id = reverse(&n.chats)[chat["ref"].as_str().expect("ref")];
            let c = read_only(&i.db());
            let m: i64 = c
                .query_row(
                    "SELECT message_id FROM chat_message WHERE chat_id = ?1 LIMIT 1",
                    [chat_id],
                    |row| row.get(0),
                )
                .expect("a message");
            let d = r
                .message_detail(MessageId::new(m), Reveal::default())
                .expect("detail");
            let flags = d
                .chats
                .iter()
                .find(|f| f.chat.get() == chat_id)
                .expect("the chat");
            assert_eq!(
                history_texts(&flags.history, HistoryColumn::ChatDisplayName),
                want,
                "{what}: chat name history"
            );
            assert!(flags.style.value.is_some() || flags.style.state.is_some());
            seen.history += want.len();
        }
        // Every attachment's facts and card.
        let atts = reverse(&n.attachments);
        for (name, w) in model["viewer"]["attachments"]
            .as_object()
            .expect("attachments")
        {
            let at = format!("{what}: {name}");
            let a = r
                .attachment(AttachmentId::new(atts[name.as_str()]))
                .expect("attachment");
            compare_attachment(&at, &a, w);
            seen.cards.insert(card_name(&a.card).to_owned());
            if matches!(
                a.card,
                CardState::File {
                    older_backup: Some(_)
                }
            ) {
                seen.older += 1;
            }
        }
        drop(r);
        assert_eq!(
            data_snapshot(&i.tmp, &i.id),
            before,
            "{what}: reading wrote nothing"
        );
    }
    assert!(seen.ec_texts >= 2, "ec texts decoded");
    assert_eq!(
        seen.withheld,
        ["earlierVersion", "unsent"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        "withheld for an unsend and for code 6"
    );
    assert!(seen.shown_earlier >= 1 && seen.groups_unchanged >= 1);
    assert!(seen.history >= 3, "value history");
    assert!(
        seen.probes.contains("SharedByUnkeyed") && seen.probes.contains("NotInArchive"),
        "{:?}",
        seen.probes
    );
    for card in [
        "file",
        "notCopied",
        "notInBackup",
        "notFoundAtAssumedKey",
        "classNotLookedUp",
        "locationUnusable",
        "couldNotBeRead",
    ] {
        assert!(seen.cards.contains(card), "{card}: {:?}", seen.cards);
    }
    assert!(seen.older >= 1, "a file from an older backup");
}

// ---------------------------------------------------------------------------
// Masking across every command (D115)
// ---------------------------------------------------------------------------

/// Every text the viewer commands return for one archive, with `reveal`.
fn everything(r: &ArchiveReader, db: &std::path::Path, reveal: Reveal) -> String {
    let mut out = String::new();
    let index = r
        .conversation_index(ConversationSort::Recent, None)
        .expect("index");
    for row in r.conversations(&index.ids).expect("rows") {
        if let Some(p) = &row.preview {
            preview_text(&p.body, &mut out);
        }
    }
    let mut messages: BTreeSet<i64> = BTreeSet::new();
    for chat in &index.ids {
        for (anchor, dir) in [
            (Anchor::Newest, Direction::Older),
            (Anchor::Undated, Direction::Newer),
        ] {
            let mut page = r
                .thread_page(*chat, anchor, dir, 100, None, reveal)
                .expect("page");
            loop {
                for m in &page.items {
                    message_text_of(m, &mut out);
                    messages.insert(m.id.get());
                }
                let next = match dir {
                    Direction::Older => page.older_anchor.filter(|_| page.has_older),
                    _ => page.newer_anchor.filter(|_| page.has_newer),
                };
                let Some(next) = next else {
                    break;
                };
                page = r
                    .thread_page(
                        *chat,
                        Anchor::Message {
                            id: next,
                            include: false,
                        },
                        dir,
                        100,
                        None,
                        reveal,
                    )
                    .expect("page");
            }
        }
    }
    let c = read_only(db);
    let all: Vec<i64> = c
        .prepare("SELECT id FROM message")
        .expect("prepare")
        .query_map([], |row| row.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("ids");
    for id in all {
        let d = r
            .message_detail(MessageId::new(id), reveal)
            .expect("detail");
        out.push_str(&detail_text(&d));
        if messages.contains(&id) {
            let t = r.reply_thread(MessageId::new(id), reveal).expect("replies");
            for m in &t.items {
                message_text_of(m, &mut out);
            }
        }
        let h = r
            .reaction_history(MessageId::new(id), None)
            .expect("history");
        for row in &h.rows {
            if let Some(e) = &row.emoji {
                opt_text(e, &mut out);
            }
        }
    }
    let bodies: Vec<i64> = c
        .prepare("SELECT id FROM message_body")
        .expect("prepare")
        .query_map([], |row| row.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("ids");
    for b in bodies {
        out.push_str(&chunk_text(r, BodyId::new(b), reveal).0);
        out.push('\u{0}');
    }
    out
}

#[test]
fn no_masked_text_reaches_any_command_unless_revealed() {
    let mut checked = 0;
    for what in ["view-main-plain", "lineages"] {
        let (_, model) = model_docs()
            .into_iter()
            .find(|(w, _)| w == what)
            .expect("doc");
        let order = &model["orders"][0];
        let i: Imported = import_order(order, what);
        let r = i.lib.open_read(&i.id).expect("reader");
        let masked: Vec<String> = ["oneTimeCodes", "earlierText"]
            .iter()
            .flat_map(|k| strings(&model["viewer"]["masked"][*k]))
            .collect();
        assert!(!masked.is_empty(), "{what}: something to mask");
        let hidden = everything(&r, &i.db(), Reveal::default());
        for m in &masked {
            assert!(!hidden.contains(m.as_str()), "{what}: {m} leaked");
        }
        // Revealing only codes still withholds earlier text, and the other
        // way round.
        let codes_only = everything(
            &r,
            &i.db(),
            Reveal {
                earlier_text: false,
                one_time_code: true,
            },
        );
        for m in strings(&model["viewer"]["masked"]["earlierText"]) {
            assert!(!codes_only.contains(m.as_str()), "{what}: {m} leaked");
        }
        let shown = everything(&r, &i.db(), REVEAL_ALL);
        for m in &masked {
            assert!(shown.contains(m.as_str()), "{what}: {m} revealed");
            checked += 1;
        }
    }
    assert!(checked >= 4);
}

// ---------------------------------------------------------------------------
// message_text and the reaction history
// ---------------------------------------------------------------------------

#[test]
fn text_chunks_rebuild_every_body_and_clamp_their_offsets() {
    let (_, model) = model_docs()
        .into_iter()
        .find(|(w, _)| w == "view-hostile-text-plain")
        .expect("doc");
    let order = &model["orders"][0];
    let i = import_order(order, "hostile");
    let r = i.lib.open_read(&i.id).expect("reader");
    let c = read_only(&i.db());
    let bodies: Vec<(i64, Option<String>)> = c
        .prepare("SELECT id, display_text FROM message_body")
        .expect("prepare")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("bodies");
    let mut multi = 0;
    for (b, text) in bodies {
        let (got, withheld) = chunk_text(&r, BodyId::new(b), REVEAL_ALL);
        assert!(!withheld);
        assert_eq!(got, text.clone().unwrap_or_default());
        let Some(text) = text else { continue };
        let full = u32::try_from(text.encode_utf16().count()).expect("u32");
        multi += usize::from(full > TEXT_CHUNK_UTF16);
        // Past the end: clamped, empty.
        let end = r
            .message_text(BodyId::new(b), full + 100, REVEAL_ALL)
            .expect("past the end");
        assert_eq!((end.from_utf16, end.end_utf16), (full, full));
        // From inside a surrogate pair: moved back to its start.
        let mut units = 0u32;
        for ch in text.chars() {
            if ch.len_utf16() == 2 {
                let mid = r
                    .message_text(BodyId::new(b), units + 1, REVEAL_ALL)
                    .expect("mid pair");
                assert_eq!(mid.from_utf16, units, "moved off the low surrogate");
                break;
            }
            units += u32::try_from(ch.len_utf16()).expect("small");
        }
    }
    assert!(multi >= 1, "a body longer than one chunk");
    assert!(matches!(
        r.message_text(BodyId::new(999_999), 0, REVEAL_ALL),
        Err(ArchiveError::AnchorNotFound)
    ));
}

/// A body whose code ranges are not trusted is withheld whole in every
/// chunk, even one that starts past the stored range (§3.6, D115): the
/// range does not say where the code really is.
#[test]
fn chunks_past_an_untrusted_code_range_stay_masked() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    {
        let mut a = lib.open(&id).expect("open");
        let mut spec = BulkSpec::new(50);
        spec.reaction_permille = 0;
        spec.undated_permille = 0;
        generate(&mut a, &spec);
    }
    let (body, text): (i64, String) = {
        let c = raw(&db_path(&tmp, &id));
        let (body, text): (i64, String) = c
            .query_row(
                "SELECT id, display_text FROM message_body \
                 WHERE display_text IS NOT NULL AND length(display_text) > 12 LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("body");
        // Units 0..6 are a one-time code (flags bit 4); the body came from
        // the text column (display_source 2), so the range is untrusted.
        c.execute(
            "INSERT INTO message_body_part (body_id, ordinal, utf16_start, utf16_end, \
                 utf8_start, utf8_end, flags, keys_present) VALUES (?1, 0, 0, 6, 0, 6, 16, 0)",
            [body],
        )
        .expect("part");
        c.execute(
            "UPDATE message_body SET display_source = 2, run_issue = NULL WHERE id = ?1",
            [body],
        )
        .expect("source");
        (body, text)
    };
    let tail: String =
        String::from_utf16(&text.encode_utf16().skip(6).collect::<Vec<u16>>()).expect("utf16");
    let r = lib.open_read(&id).expect("reader");
    let whole = vec![TextPiece::Masked(oma_archive::MaskKind::OneTimeCode)];
    for from in [0, 6, 8] {
        let c = r
            .message_text(BodyId::new(body), from, Reveal::default())
            .expect("chunk");
        assert_eq!(c.text, ChunkText::Text(whole.clone()), "from {from}");
    }
    // Revealed, the text is shown.
    let (shown, _) = chunk_text(&r, BodyId::new(body), REVEAL_ALL);
    assert_eq!(shown, text);
    drop(r);
    // Trusted (decoded cleanly): a chunk past the code shows its text.
    {
        let c = raw(&db_path(&tmp, &id));
        c.execute(
            "UPDATE message_body SET display_source = 1 WHERE id = ?1",
            [body],
        )
        .expect("source");
    }
    let r = lib.open_read(&id).expect("reader");
    let c = r
        .message_text(BodyId::new(body), 6, Reveal::default())
        .expect("chunk");
    let mut got = String::new();
    let ChunkText::Text(p) = &c.text else {
        panic!("text")
    };
    pieces_text(p, &mut got);
    assert_eq!(got, tail);
    let c = r
        .message_text(BodyId::new(body), 0, Reveal::default())
        .expect("chunk");
    let ChunkText::Text(p) = &c.text else {
        panic!("text")
    };
    assert_eq!(
        p.first(),
        Some(&TextPiece::Masked(oma_archive::MaskKind::OneTimeCode))
    );
}

/// "Show earlier text" reveals the earlier text only: its one-time codes
/// stay masked until "Show code" (D115), whole when the ranges cannot be
/// trusted.
#[test]
fn revealed_earlier_text_keeps_its_codes_masked() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    {
        let mut a = lib.open(&id).expect("open");
        let mut spec = BulkSpec::new(50);
        spec.reaction_permille = 0;
        spec.undated_permille = 0;
        generate(&mut a, &spec);
    }
    let (message, earlier, text): (i64, i64, String) = {
        let c = raw(&db_path(&tmp, &id));
        let (message, earlier, text): (i64, i64, String) = c
            .query_row(
                "SELECT m.id, b.id, b.display_text FROM message m \
                 JOIN message_body b ON b.id = m.current_body_id \
                 WHERE b.display_text IS NOT NULL AND length(b.display_text) > 12 LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("body");
        // A newer body without text becomes current, so the old text is
        // earlier text withheld as "no current text".
        c.execute(
            "INSERT INTO message_body (message_id, raw_hash, text_raw, text_raw_st, \
                 attributed_raw, attributed_raw_st, summary_raw, summary_raw_st, \
                 attributed_outcome, attributed_error, run_issue, unknown_attr_keys, \
                 unknown_attr_classes, display_text, display_source, display_none_reason, \
                 sources_differ, summary_outcome, otr, ec_state, rp_state, has_tmp, \
                 summary_unknown_keys, edit_label, edit_label_a, edit_label_b, \
                 decoder_version, first_session_id, first_rank, last_session_id, last_rank) \
             SELECT message_id, randomblob(32), NULL, 1, NULL, 1, NULL, 1, \
                 1, NULL, NULL, NULL, NULL, NULL, NULL, 1, NULL, 1, NULL, NULL, NULL, NULL, \
                 NULL, edit_label, edit_label_a, edit_label_b, decoder_version, \
                 first_session_id, first_rank, last_session_id, last_rank \
             FROM message_body WHERE id = ?1",
            [earlier],
        )
        .expect("newer body");
        let newer = c.last_insert_rowid();
        c.execute(
            "UPDATE message SET current_body_id = ?2 WHERE id = ?1",
            [message, newer],
        )
        .expect("current");
        // Units 0..6 of the earlier text are a one-time code; it came from
        // the text column, so the range is not trusted.
        c.execute(
            "INSERT INTO message_body_part (body_id, ordinal, utf16_start, utf16_end, \
                 utf8_start, utf8_end, flags, keys_present) VALUES (?1, 0, 0, 6, 0, 6, 16, 0)",
            [earlier],
        )
        .expect("part");
        (message, earlier, text)
    };
    let earlier_only = Reveal {
        earlier_text: true,
        one_time_code: false,
    };
    let revealed = |reveal: Reveal| -> Vec<TextPiece> {
        let r = lib.open_read(&id).expect("reader");
        let d = r
            .message_detail(MessageId::new(message), reveal)
            .expect("detail");
        match d.message.edit.earlier {
            EarlierText::Revealed { text, .. } => text,
            other => panic!("{other:?}"),
        }
    };
    let whole = vec![TextPiece::Masked(oma_archive::MaskKind::OneTimeCode)];
    assert_eq!(revealed(earlier_only), whole);
    let mut all = String::new();
    pieces_text(&revealed(REVEAL_ALL), &mut all);
    assert_eq!(all, text);
    // Trusted ranges: only the code is masked.
    {
        let c = raw(&db_path(&tmp, &id));
        c.execute(
            "UPDATE message_body SET display_source = 1, run_issue = NULL WHERE id = ?1",
            [earlier],
        )
        .expect("source");
    }
    let pieces = revealed(earlier_only);
    assert_eq!(
        pieces.first(),
        Some(&TextPiece::Masked(oma_archive::MaskKind::OneTimeCode))
    );
    let mut shown = String::new();
    pieces_text(&pieces, &mut shown);
    let tail: String =
        String::from_utf16(&text.encode_utf16().skip(6).collect::<Vec<u16>>()).expect("utf16");
    assert_eq!(shown, tail);
}

/// A whole-bubble unsend and a part count come only from recorded parts
/// (decoded runs or the summary's `otr` count, §3.6), never from a guess.
#[test]
fn an_unsend_with_unknown_parts_is_not_called_whole() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    {
        let mut a = lib.open(&id).expect("open");
        let mut spec = BulkSpec::new(50);
        spec.reaction_permille = 0;
        spec.undated_permille = 0;
        generate(&mut a, &spec);
    }
    let (message, body): (i64, i64) = {
        let c = raw(&db_path(&tmp, &id));
        let pair = c
            .query_row(
                "SELECT m.id, b.id FROM message m JOIN message_body b ON b.id = m.current_body_id \
                 WHERE b.display_text IS NOT NULL LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("body");
        c.execute(
            "INSERT INTO message_unsent_part (body_id, ordinal, part) VALUES (?1, 0, 0)",
            [pair.1],
        )
        .expect("unsent");
        pair
    };
    let edit = || {
        let r = lib.open_read(&id).expect("reader");
        let d = r
            .message_detail(MessageId::new(message), Reveal::default())
            .expect("detail");
        (d.message.edit.part_count, d.message.edit.unsent_whole)
    };
    let set = |sql: &str| {
        raw(&db_path(&tmp, &id))
            .execute(sql, [body])
            .expect("update");
    };
    // Text from the text column, no runs and no `otr`: parts unknown.
    assert_eq!(edit(), (None, false));
    // No text at all (an attributedBody that could not be decoded).
    set(
        "UPDATE message_body SET display_text = NULL, display_source = NULL, \
         display_none_reason = 1 WHERE id = ?1",
    );
    assert_eq!(edit(), (None, false));
    // `otr` records two original parts: one of two is unsent.
    set("UPDATE message_body SET otr = 2 WHERE id = ?1");
    assert_eq!(edit(), (Some(2), false));
    // One original part, listed: the whole bubble.
    set("UPDATE message_body SET otr = 1 WHERE id = ?1");
    assert_eq!(edit(), (Some(1), true));
}

#[test]
fn reaction_history_pages_every_row_in_date_order() {
    let (_, model) = model_docs()
        .into_iter()
        .find(|(w, _)| w == "view-main-plain")
        .expect("doc");
    let order = &model["orders"][0];
    let i = import_order(order, "main");
    let r = i.lib.open_read(&i.id).expect("reader");
    let c = read_only(&i.db());
    let targets: Vec<(i64, i64)> = c
        .prepare(
            "SELECT t.id, count(*) FROM message m JOIN message t \
               ON t.key_kind = 1 AND t.guid = m.assoc_target_guid \
             WHERE m.assoc_present = 1 \
               AND (m.assoc_kind BETWEEN 10 AND 17 OR m.assoc_kind BETWEEN 20 AND 27) \
             GROUP BY t.id",
        )
        .expect("prepare")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("targets");
    assert!(targets.iter().any(|(_, n)| *n > 200), "a target over 200");
    for &(t, n) in &targets {
        let oracle: Vec<i64> = c
            .prepare(
                "SELECT m.id FROM message m JOIN message t \
                   ON t.key_kind = 1 AND t.guid = m.assoc_target_guid \
                 WHERE t.id = ?1 AND m.assoc_present = 1 \
                   AND (m.assoc_kind BETWEEN 10 AND 17 OR m.assoc_kind BETWEEN 20 AND 27) \
                 ORDER BY m.sort_key, m.id",
            )
            .expect("prepare")
            .query_map([t], |row| row.get(0))
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("rows");
        let mut got = Vec::new();
        let mut after = None;
        loop {
            let h = r
                .reaction_history(MessageId::new(t), after.as_ref())
                .expect("history");
            assert!(h.keyed && h.rows.len() <= REACTION_HISTORY_PAGE);
            assert_eq!(h.total.n, u64::try_from(n).expect("n"));
            got.extend(h.rows.iter().map(|x| x.id.get()));
            assert_eq!(h.has_more, h.next.is_some());
            if !h.has_more {
                break;
            }
            after = h.next;
        }
        assert_eq!(got, oracle, "every row once, by date then id");
    }
    // A reaction added before the first page between two calls (an
    // import) is neither served twice nor makes a row repeat: the next
    // page starts strictly after the last row served.
    let (big, _) = *targets
        .iter()
        .find(|(_, n)| *n > i64::try_from(REACTION_HISTORY_PAGE).expect("page"))
        .expect("a target over a page");
    let first = r
        .reaction_history(MessageId::new(big), None)
        .expect("page 0");
    let served: BTreeSet<i64> = first.rows.iter().map(|x| x.id.get()).collect();
    {
        let w = raw(&i.db());
        let added: i64 = w
            .query_row(
                "SELECT id FROM message WHERE assoc_present = 0 AND key_kind = 1 \
                   AND id <> ?1 ORDER BY id LIMIT 1",
                [big],
                |row| row.get(0),
            )
            .expect("a plain message");
        w.execute(
            "UPDATE message SET assoc_present = 1, assoc_kind = 10, assoc_target_shape = 2, \
               assoc_target_guid = (SELECT guid FROM message WHERE id = ?2), \
               sort_key = X'000000000000000001' WHERE id = ?1",
            [added, big],
        )
        .expect("an earlier reaction");
    }
    let second = r
        .reaction_history(MessageId::new(big), first.next.as_ref())
        .expect("page 1");
    assert!(!second.rows.is_empty());
    for x in &second.rows {
        assert!(!served.contains(&x.id.get()), "a row served twice");
    }
    // An unkeyed message has no resolvable reactions; an unknown id is not
    // found.
    let unkeyed: i64 = c
        .query_row(
            "SELECT id FROM message WHERE key_kind = 2 LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("an unkeyed message");
    let h = r
        .reaction_history(MessageId::new(unkeyed), None)
        .expect("history");
    assert!(!h.keyed && h.rows.is_empty());
    assert!(matches!(
        r.reaction_history(MessageId::new(999_999), None),
        Err(ArchiveError::AnchorNotFound)
    ));
}

// ---------------------------------------------------------------------------
// Dates
// ---------------------------------------------------------------------------

/// The expected seek: the first item at or after `t`, or the last before.
fn expect_seek(items: &[(i64, i64)], t: i64, dir: SeekDir) -> Option<i64> {
    match dir {
        SeekDir::OnOrAfter => items.iter().find(|(ns, _)| *ns >= t).map(|x| x.1),
        _ => items.iter().rev().find(|(ns, _)| *ns < t).map(|x| x.1),
    }
}

fn check_dates(at: &str, r: &ArchiveReader, chat: ChatId, items: &[(i64, i64)]) {
    let mut instants: BTreeSet<i64> = BTreeSet::new();
    for (ns, _) in items {
        instants.extend([ns - 1, *ns, ns + 1]);
    }
    if let (Some(first), Some(last)) = (items.first(), items.last()) {
        instants.extend([first.0 - 86_400_000_000_000, last.0 + 86_400_000_000_000]);
    }
    instants.extend([0, i64::MIN + 1, i64::MAX - 1]);
    for t in &instants {
        for dir in [SeekDir::OnOrAfter, SeekDir::Before] {
            assert_eq!(
                r.seek_date(chat, *t, dir)
                    .expect("seek")
                    .map(MessageId::get),
                expect_seek(items, *t, dir),
                "{at}: seek {t} {dir:?}"
            );
        }
    }
    // Day counts over a grid that covers every item, and over the items'
    // own instants.
    let Some((lo, hi)) = items.first().zip(items.last()).map(|(a, b)| (a.0, b.0)) else {
        let none = r
            .day_counts(chat, &[i64::MIN, 0, i64::MAX])
            .expect("counts");
        assert_eq!(none.counts, [0, 0], "{at}: no dated items");
        return;
    };
    // Each item alone.
    for (ns, id) in items {
        let n = items.iter().filter(|(t, _)| t == ns).count();
        let one = r.day_counts(chat, &[*ns, ns + 1]).expect("one");
        assert_eq!(one.counts, [n as u64], "{at}: the day of {id}");
    }
    let span = (hi - lo) / 40 + 1;
    let grid: Vec<i64> = (0..=41).map(|k| lo - span + k * span).collect();
    let mut grids = vec![grid];
    let own: Vec<i64> = instants
        .iter()
        .copied()
        .take(DAY_BOUNDS)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if own.len() >= 2 {
        grids.push(own);
    }
    for bounds in grids {
        let got = r.day_counts(chat, &bounds).expect("counts");
        let want: Vec<u64> = bounds
            .windows(2)
            .map(|w| {
                u64::try_from(
                    items
                        .iter()
                        .filter(|(ns, _)| *ns >= w[0] && *ns < w[1])
                        .count(),
                )
                .expect("n")
            })
            .collect();
        assert_eq!(got.counts, want, "{at}: day counts");
    }
}

#[test]
fn seek_and_day_counts_match_the_model() {
    let mut chats = 0;
    for what in ["view-main-plain", "lineages", "view-hostile-text-plain"] {
        let (_, model) = model_docs()
            .into_iter()
            .find(|(w, _)| w == what)
            .expect("doc");
        let order = &model["orders"][0];
        let i = import_order(order, what);
        let n = names(&i, order);
        let r = i.lib.open_read(&i.id).expect("reader");
        let chat_ids = reverse(&n.chats);
        let messages = &model["viewer"]["messages"];
        for (chat, t) in model["viewer"]["threads"].as_object().expect("threads") {
            let ids = reverse(&n.messages);
            let items: Vec<(i64, i64)> = strings(&t["dated"])
                .iter()
                .map(|m| {
                    (
                        messages[m.as_str()]["utcNs"].as_i64().expect("dated"),
                        ids[m.as_str()],
                    )
                })
                .collect();
            check_dates(
                &format!("{what}: {chat}"),
                &r,
                ChatId::new(chat_ids[chat.as_str()]),
                &items,
            );
            chats += 1;
        }
    }
    assert!(chats >= 8);
}

#[test]
fn seek_and_day_counts_match_an_oracle_on_a_bulk_archive() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    {
        let mut a = lib.open(&id).expect("open");
        let mut spec = BulkSpec::new(3000);
        spec.chats = 3;
        spec.big_chat_permille = 700;
        spec.undated_permille = 30;
        spec.reaction_permille = 150;
        let rep = generate(&mut a, &spec);
        assert!(rep.reactions > 100 && rep.undated > 10, "{rep:?}");
    }
    let c = read_only(&db_path(&tmp, &id));
    let r = lib.open_read(&id).expect("reader");
    let chats: Vec<i64> = c
        .prepare("SELECT id FROM chat")
        .expect("prepare")
        .query_map([], |row| row.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("chats");
    for chat in chats {
        // Visible dated items, independently: each row's own fields and a
        // plain lookup of its target.
        type Row = (i64, Vec<u8>, i64, Option<i64>, Option<Vec<u8>>);
        let rows: Vec<Row> = c
            .prepare(
                "SELECT cm.message_id, cm.sort_key, m.assoc_present, m.assoc_kind, \
                   m.assoc_target_guid FROM chat_message cm JOIN message m \
                   ON m.id = cm.message_id WHERE cm.chat_id = ?1 \
                 ORDER BY cm.sort_key, cm.message_id",
            )
            .expect("prepare")
            .query_map([chat], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("rows");
        let in_chat: BTreeSet<i64> = rows.iter().map(|r| r.0).collect();
        let mut by_guid: BTreeMap<Vec<u8>, i64> = BTreeMap::new();
        for (id, guid) in c
            .prepare("SELECT id, guid FROM message WHERE key_kind = 1")
            .expect("prepare")
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
            })
            .expect("query")
            .map(|x| x.expect("row"))
        {
            by_guid.insert(guid, id);
        }
        let items: Vec<(i64, i64)> = rows
            .iter()
            .filter(|(_, key, ..)| key.first() == Some(&0))
            .filter(|(_, _, present, kind, target)| {
                let reaction = *present == 1 && matches!(kind, Some(10..=17 | 20..=27));
                let folds = reaction
                    && target
                        .as_ref()
                        .and_then(|g| by_guid.get(g))
                        .is_some_and(|t| in_chat.contains(t));
                !folds
            })
            .map(|(id, key, ..)| {
                let b: [u8; 8] = key[1..9].try_into().expect("dated key");
                let ns = i64::from_be_bytes((u64::from_be_bytes(b) ^ (1u64 << 63)).to_be_bytes());
                (ns, *id)
            })
            .collect();
        assert!(items.len() > 10);
        // A sample of instants keeps the test fast.
        let sample: Vec<(i64, i64)> = items
            .iter()
            .step_by((items.len() / 25).max(1))
            .copied()
            .collect();
        for (ns, _) in &sample {
            for t in [ns - 1, *ns, ns + 1] {
                for dir in [SeekDir::OnOrAfter, SeekDir::Before] {
                    assert_eq!(
                        r.seek_date(ChatId::new(chat), t, dir)
                            .expect("seek")
                            .map(MessageId::get),
                        expect_seek(&items, t, dir),
                        "chat {chat}: seek {t} {dir:?}"
                    );
                }
            }
        }
        let (lo, hi) = (items[0].0, items[items.len() - 1].0);
        let span = (hi - lo) / 41 + 1;
        let bounds: Vec<i64> = (0..43).map(|k| lo + (k - 1) * span).collect();
        let got = r.day_counts(ChatId::new(chat), &bounds).expect("counts");
        let want: Vec<u64> = bounds
            .windows(2)
            .map(|w| {
                u64::try_from(
                    items
                        .iter()
                        .filter(|(ns, _)| *ns >= w[0] && *ns < w[1])
                        .count(),
                )
                .expect("n")
            })
            .collect();
        assert_eq!(got.counts, want, "chat {chat}: day counts");
    }
}

// ---------------------------------------------------------------------------
// Constructed cards, budgets and errors
// ---------------------------------------------------------------------------

#[test]
fn cards_an_import_cannot_produce_are_decided_from_the_codes() {
    let (_, model) = model_docs()
        .into_iter()
        .find(|(w, _)| w == "view-media-plain")
        .expect("doc");
    let order = &model["orders"][0];
    let i = import_order(order, "media");
    let c = raw(&i.db());
    let with_blob: Vec<(i64, i64)> = c
        .prepare("SELECT id, current_blob_id FROM attachment WHERE current_blob_id IS NOT NULL")
        .expect("prepare")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    let without: Vec<i64> = c
        .prepare("SELECT id FROM attachment WHERE current_blob_id IS NULL")
        .expect("prepare")
        .query_map([], |row| row.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    assert!(with_blob.len() >= 2 && without.len() >= 4);
    let session: i64 = c
        .query_row("SELECT max(id) FROM import_session", [], |row| row.get(0))
        .expect("session");
    // No observation at all.
    c.execute(
        "DELETE FROM attachment_observation WHERE attachment_id = ?1",
        [without[0]],
    )
    .expect("delete");
    // Keys that do not unwrap; a file over the copy limit; unknown codes.
    let set = |a: i64, lookup: i64, copy: i64| {
        c.execute(
            "UPDATE attachment_observation SET lookup = ?2, copy = ?3 WHERE attachment_id = ?1",
            rusqlite::params![a, lookup, copy],
        )
        .expect("update");
    };
    set(without[1], LookupCode::KeyDoesNotUnwrap.code(), 9);
    set(without[2], LookupCode::Present.code(), 11);
    set(without[3], 99, 98);
    // A full verify reported the first blob missing at t = 1000; a later
    // import replaced the file, so the report may be out of date. The
    // second blob's report is not followed by a replacement.
    let (stale_att, stale_blob) = with_blob[0];
    let (kept_att, kept_blob) = with_blob[1];
    for b in [stale_blob, kept_blob] {
        c.execute(
            "UPDATE blob SET verify_state = ?2, verified_at_ns = 1000 WHERE id = ?1",
            rusqlite::params![b, BlobVerifyState::Missing.code()],
        )
        .expect("verify state");
    }
    c.execute(
        "UPDATE attachment_observation SET copy = 15 WHERE attachment_id = ?1",
        [stale_att],
    )
    .expect("replaced");
    drop(c);
    let r = i.lib.open_read(&i.id).expect("reader");
    let card = |a: i64| r.attachment(AttachmentId::new(a)).expect("facts");
    assert_eq!(card(without[0]).card, CardState::NoObservation);
    assert!(card(without[0]).observations.is_empty());
    assert_eq!(card(without[1]).card, CardState::NotDecrypted);
    assert_eq!(card(without[2]).card, CardState::TooLarge);
    assert!(matches!(
        card(without[3]).card,
        CardState::Unavailable {
            lookup: 99,
            copy: 98
        }
    ));
    let stale = card(stale_att).verify.expect("a report");
    assert_eq!(stale.state, Some(BlobVerifyState::Missing));
    assert_eq!(stale.verified_at_ns, Some(1000));
    assert!(stale.may_be_stale, "replaced after the verify");
    let kept = card(kept_att).verify.expect("a report");
    assert!(!kept.may_be_stale);
    assert!(matches!(card(kept_att).card, CardState::File { .. }));
    assert!(session > 0);
}

#[test]
fn budgets_give_query_too_broad_and_bad_inputs_are_refused() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    {
        let mut a = lib.open(&id).expect("open");
        let mut spec = BulkSpec::new(600);
        spec.chats = 2;
        generate(&mut a, &spec);
    }
    let r = lib.open_read(&id).expect("reader");
    let c = read_only(&db_path(&tmp, &id));
    let (chat, message): (i64, i64) = c
        .query_row(
            "SELECT chat_id, message_id FROM chat_message WHERE sort_key < X'01' LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("row");
    let body: i64 = c
        .query_row(
            "SELECT current_body_id FROM message WHERE id = ?1",
            [message],
            |row| row.get(0),
        )
        .expect("body");
    let chat = ChatId::new(chat);
    let bounds: Vec<i64> = (0..43).map(|k| k * 86_400_000_000_000).collect();
    {
        let _f = install(Faults {
            view_op_budget: Some(1000),
            ..Faults::default()
        });
        // One statement over the whole chat goes over the budget.
        assert!(matches!(
            r.day_counts(chat, &[i64::MIN, i64::MAX]),
            Err(ArchiveError::QueryTooBroad)
        ));
        // Reads made of short statements may finish within one tick
        // (SQLite counts operations per statement); a trip is never
        // `Damaged`.
        for e in [
            r.message_detail(MessageId::new(message), Reveal::default())
                .err(),
            r.day_counts(chat, &bounds).err(),
            r.seek_date(chat, 0, SeekDir::OnOrAfter).err(),
            r.message_text(BodyId::new(body), 0, Reveal::default())
                .err(),
        ]
        .into_iter()
        .flatten()
        {
            assert!(matches!(e, ArchiveError::QueryTooBroad), "{e:?}");
        }
    }
    assert!(r.day_counts(chat, &[i64::MIN, i64::MAX]).is_ok());
    for bad in [
        &[][..],
        &[1][..],
        &[2, 1][..],
        &[1, 1][..],
        &(0..44).collect::<Vec<i64>>()[..],
    ] {
        assert!(matches!(
            r.day_counts(chat, bad),
            Err(ArchiveError::Internal(_))
        ));
    }
    assert!(matches!(
        r.message_detail(MessageId::new(999_999), Reveal::default()),
        Err(ArchiveError::AnchorNotFound)
    ));
    assert!(matches!(
        r.attachment(AttachmentId::new(999_999)),
        Err(ArchiveError::AnchorNotFound)
    ));
    assert_eq!(
        r.seek_date(ChatId::new(999_999), 0, SeekDir::OnOrAfter)
            .expect("an unknown chat has nothing"),
        None
    );
}
