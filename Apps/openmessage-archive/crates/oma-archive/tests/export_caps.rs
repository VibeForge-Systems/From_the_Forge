//! Slice 6a of M6 (spec §3.1, §3.3, §3.9, §14.1): the assembly layer's
//! caps and codes.
//!
//! - **`Caps::PAGE` is byte-identical to M5.** Every viewer read the caps
//!   reach (conversation rows, thread pages of both ranges, reply
//!   threads, details, reaction histories), with no reveal and with both,
//!   over every entry and scenario of the viewer catalogue, is dumped as
//!   `Debug` (which prints a content value's length only) plus the texts,
//!   and its SHA-256 compared with the dump the M5 code produced.
//! - **`Caps::EXPORT` is uncapped**: values of 300+ UTF-16 units (unit
//!   names, types, addresses, titles, the account, the subject, versions),
//!   bodies over 8,192 units and emoji of 65+ bytes come back whole, and a
//!   lowered cap is reported by the check that runs in every build.
//! - **Codes and states survive under `EXPORT`**: unknown item kinds,
//!   `is_from_me` values, `_st` states, units, services and edit labels
//!   keep their numbers, and presence booleans keep their state.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;

use std::fmt::Write as _;

use common::bulk::{generate, BulkSpec};
use common::import::read_only;
use common::viewer::{import_order, model_docs, Imported};
use common::*;
use oma_archive::hooks::{export_detail, export_handle, export_item, LowerCaps};
use oma_archive::schema::codes::{AssocKindCode, ValueState};
use oma_archive::{
    Anchor, ArchiveError, ArchiveReader, AssembledMessage, BodyView, CapMiss, ConversationSort,
    Direction, DirectionCode, EarlierText, EditEntryText, ExportItem, HandleId, HistoryColumn,
    MessageDetail, MessageId, MessageLabel, OptContent, PreviewBody, RawDirection, ReplyRef,
    Reveal, SegmentKind, TextPiece, VersionText,
};
use sha2::{Digest, Sha256};

const REVEAL_ALL: Reveal = Reveal {
    earlier_text: true,
    one_time_code: true,
};

// ---------------------------------------------------------------------------
// Caps::PAGE is byte-identical to M5 (M6 spec §3.3, §14.1)
// ---------------------------------------------------------------------------

/// The text of every content value a dump can reach by its public fields:
/// bodies, earlier text, quotes, versions and edit entries. Lengths and
/// every other fact come from `Debug`, which prints a content value's
/// length only.
fn texts_of(m: &AssembledMessage, out: &mut String) {
    if let BodyView::Text(b) = &m.body {
        for s in &b.segments {
            if let SegmentKind::Text(t) = &s.kind {
                out.push_str(t.text.expose());
                out.push('\u{1}');
            }
        }
    }
    if let EarlierText::Revealed { text, .. } = &m.edit.earlier {
        pieces(text, out);
    }
    if let Some(ReplyRef::Resolved { quote, .. } | ReplyRef::InAnotherConversation { quote, .. }) =
        &m.reply
    {
        if let PreviewBody::Text(p) = quote {
            pieces(p, out);
        }
    }
    for a in &m.attachments {
        opt(&a.name, out);
        opt(&a.declared_type, out);
    }
    if let Some(e) = m.event.as_ref().and_then(|e| e.title.as_ref()) {
        opt(e, out);
    }
    if let Some(e) = m.association.as_ref().and_then(|a| a.emoji.as_ref()) {
        opt(e, out);
    }
    out.push('\u{0}');
}

fn pieces(p: &[TextPiece], out: &mut String) {
    for piece in p {
        if let TextPiece::Text(t) = piece {
            out.push_str(t.text.expose());
        }
        out.push('\u{1}');
    }
}

fn opt(o: &OptContent, out: &mut String) {
    if let OptContent::Text(t) = o {
        out.push_str(t.text.expose());
    }
    out.push('\u{1}');
}

fn detail_texts(d: &MessageDetail, out: &mut String) {
    texts_of(&d.message, out);
    opt(&d.account, out);
    opt(&d.subject, out);
    for v in &d.versions {
        if let VersionText::Text(p) = &v.text {
            pieces(p, out);
        }
        for e in &v.edits {
            if let EditEntryText::Text(p) = &e.text {
                pieces(p, out);
            }
        }
    }
    for h in &d.history {
        opt(&h.value, out);
    }
    for a in &d.attachments {
        for o in [&a.declared_type, &a.uti, &a.name, &a.emoji_description] {
            opt(o, out);
        }
        for h in &a.history {
            opt(&h.value, out);
        }
    }
    out.push('\u{0}');
}

/// Every viewer read the caps reach, in a fixed order: the conversation
/// rows, every thread page of both ranges, every reply thread, detail and
/// reaction history, as `Debug` plus the texts.
fn page_dump(r: &ArchiveReader, db: &std::path::Path, reveal: Reveal) -> String {
    let mut out = String::new();
    let index = r
        .conversation_index(ConversationSort::Recent, None)
        .expect("index");
    for row in r.conversations(&index.ids).expect("rows") {
        writeln!(out, "{row:?}").expect("write");
    }
    for chat in &index.ids {
        for (anchor, dir) in [
            (Anchor::Newest, Direction::Older),
            (Anchor::Undated, Direction::Newer),
        ] {
            let mut page = r
                .thread_page(*chat, anchor, dir, 100, None, reveal)
                .expect("page");
            loop {
                writeln!(out, "{page:?}").expect("write");
                for m in &page.items {
                    texts_of(m, &mut out);
                }
                for h in &page.handles {
                    opt(&h.address, &mut out);
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
        .prepare("SELECT id FROM message ORDER BY id")
        .expect("prepare")
        .query_map([], |row| row.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("ids");
    for id in all {
        let id = MessageId::new(id);
        let d = r.message_detail(id, reveal).expect("detail");
        writeln!(out, "{d:?}").expect("write");
        detail_texts(&d, &mut out);
        match r.reply_thread(id, reveal) {
            Ok(t) => {
                writeln!(out, "{t:?}").expect("write");
                for m in &t.items {
                    texts_of(m, &mut out);
                }
            }
            Err(e) => writeln!(out, "reply thread: {e:?}").expect("write"),
        }
        let h = r.reaction_history(id, None).expect("history");
        writeln!(out, "{h:?}").expect("write");
        for row in &h.rows {
            if let Some(e) = &row.emoji {
                opt(e, &mut out);
            }
        }
    }
    out
}

/// SHA-256 of the dump of every entry and scenario (its first order),
/// with no reveal and with both, taken with the M5 code (`main` at
/// `9883217`, before slice 6a) and pinned: `Caps::PAGE` must stay
/// byte-identical.
const PAGE_DUMPS: &[(&str, &str)] = &[
    (
        "view-hostile-text-plain",
        "aff4480a1d03079e8706c164aa5c9a4503c4c5c4277cf9717790c4a48e78445a",
    ),
    (
        "view-lin-a1-plain",
        "22a1a9cf4484b22530bf17140364289150f04783ea5519834542ed63d6cc9e03",
    ),
    (
        "view-lin-a2-plain",
        "7e31c7154f034bfb9fd9fb9ee7b6fcbd3a0bfdf47e587919981cf1034bbad506",
    ),
    (
        "view-lin-a3-incomplete-plain",
        "349c5231895ee6b621fbcee2c86168af8932a098628c51cae073469e0a5e2d02",
    ),
    (
        "view-lin-b-plain",
        "80e2eaf9efe43b8cb16a7e33bb18010abd96d9268f5f99644bec08b303ce93f7",
    ),
    (
        "view-lin-rd-plain",
        "349c5231895ee6b621fbcee2c86168af8932a098628c51cae073469e0a5e2d02",
    ),
    (
        "view-main-plain",
        "bceb09e883d3ec47d074b683840fb841c0f54172c7bc8750d254a6c2c293db75",
    ),
    (
        "view-media-plain",
        "1544a97d66dc9bc8815dbb710adea4a3d4109b78dbd918de3e940faa035b3e8c",
    ),
    (
        "view-media2-plain",
        "a77d2074f033d46168774d7875eab8f70f949f96683a3ab943b05725957898df",
    ),
    (
        "view-oldcols-plain",
        "66aaa93f24e4fb273130fe38429b189098fe0912ec59b4293e55b06c684bd3fc",
    ),
    (
        "lineages",
        "3808904c3733201df3b896a309f7761c6bf23991a031c9ca02a208805e192a6d",
    ),
    (
        "media-copy-off",
        "706f33bb8e376d16cc02e134eff9fb1d9330ab28f061d23d36d0bb7e94728d28",
    ),
    (
        "media-older-blob",
        "2aaa2432a1848a67a9ee3efb446f44e4703ca492c3edbec48060da148b82903b",
    ),
    (
        "rd-first",
        "d5688ba201b547d114377d13fc6253b459fde329ff44de1562b2f7012f534144",
    ),
];

#[test]
fn page_caps_are_byte_identical_to_m5_over_the_viewer_catalogue() {
    let mut got: Vec<(String, String)> = Vec::new();
    for (what, model) in model_docs() {
        let i: Imported = import_order(&model["orders"][0], &what);
        let r = i.lib.open_read(&i.id).expect("reader");
        let mut h = Sha256::new();
        for reveal in [Reveal::default(), REVEAL_ALL] {
            h.update(page_dump(&r, &i.db(), reveal).as_bytes());
        }
        got.push((what, hex(&h.finalize())));
    }
    let want: Vec<(String, String)> = PAGE_DUMPS
        .iter()
        .map(|(w, d)| ((*w).to_owned(), (*d).to_owned()))
        .collect();
    let printed: String = got
        .iter()
        .map(|(w, d)| format!("    (\"{w}\", \"{d}\"),\n"))
        .collect();
    assert_eq!(got, want, "PAGE dumps changed; now:\n{printed}");
}

// ---------------------------------------------------------------------------
// Caps::EXPORT and the codes (M6 spec §3.3, §3.9, §14.1)
// ---------------------------------------------------------------------------

/// Distinctive synthetic values, each over its viewer cap.
fn long(c: char, n: usize) -> String {
    std::iter::repeat_n(c, n).collect()
}

/// 17 four-byte emoji: 68 bytes, over the viewer's 64.
fn long_emoji() -> String {
    long('\u{1F600}', 17)
}

/// The rows the tests below look at, by archive id.
struct Hostile {
    _tmp: tempfile::TempDir,
    reader: ArchiveReader,
    /// A received message with an attachment: long body, versions,
    /// subject, account, subject history, attachment values.
    plain: i64,
    /// Its sender.
    handle: i64,
    /// A group rename with a long title.
    rename: i64,
    /// An emoji reaction (68-byte emoji) and its target.
    reaction: i64,
    target: i64,
    /// Unknown item kind, `is_from_me`, `_st`, service, labels and codes.
    unknown: i64,
    /// An unknown time unit, `is_from_me` absent.
    odd_unit: i64,
    /// `is_from_me` with an unknown state code.
    odd_state: i64,
}

fn one(c: &rusqlite::Connection, sql: &str) -> i64 {
    c.query_row(sql, [], |r| r.get(0)).expect(sql)
}

/// A small bulk archive (every column the generator leaves unset is "NULL
/// in the source"), then rows edited the way no import of this version
/// writes them: values over every viewer cap and codes this build does
/// not know. All values are synthetic.
fn hostile() -> Hostile {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    {
        let mut a = lib.open(&id).expect("open");
        let mut spec = BulkSpec::new(60);
        spec.chats = 4;
        spec.reaction_permille = 300;
        spec.attachment_permille = 1000;
        spec.undated_permille = 0;
        generate(&mut a, &spec);
    }
    let c = raw(&db_path(&tmp, &id));
    let plain = one(
        &c,
        "SELECT ma.message_id FROM message_attachment ma JOIN message m ON m.id = ma.message_id \
         WHERE m.handle_ref IS NOT NULL AND m.assoc_present = 0 ORDER BY ma.message_id LIMIT 1",
    );
    let handle = one(
        &c,
        &format!("SELECT handle_ref FROM message WHERE id = {plain}"),
    );
    let reaction = one(
        &c,
        "SELECT id FROM message WHERE assoc_present = 1 ORDER BY id LIMIT 1",
    );
    let target = one(
        &c,
        &format!(
            "SELECT t.id FROM message t JOIN message r ON r.assoc_target_guid = t.guid \
             WHERE r.id = {reaction}"
        ),
    );
    let others: Vec<i64> = c
        .prepare(&format!(
            "SELECT id FROM message WHERE assoc_present = 0 AND id NOT IN ({plain}, {target}) \
             ORDER BY id LIMIT 4"
        ))
        .expect("prepare")
        .query_map([], |r| r.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("ids");
    let [rename, unknown, odd_unit, odd_state] = others[..] else {
        panic!("four other messages");
    };
    let body = long('b', 9000);
    let older = long('o', 9000);
    let sql = format!(
        "UPDATE message_body SET display_text = ?1, text_raw = CAST(?1 AS BLOB) \
           WHERE id = (SELECT current_body_id FROM message WHERE id = {plain});
         CREATE TEMP TABLE v AS SELECT * FROM message_body \
           WHERE id = (SELECT current_body_id FROM message WHERE id = {plain});
         UPDATE v SET id = (SELECT max(id) + 1 FROM message_body), \
           raw_hash = zeroblob(32), display_text = ?2, text_raw = CAST(?2 AS BLOB);
         INSERT INTO message_body SELECT * FROM v;
         UPDATE message SET subject = CAST(?3 AS BLOB), subject_st = NULL, destination_caller_id = CAST(?4 AS BLOB), \
           destination_caller_id_st = NULL WHERE id = {plain};
         INSERT INTO value_history (entity, row_id, col, value_hash, value, first_rank, \
           first_session_id) SELECT 3, id, 318, zeroblob(32), ?5, first_rank, 1 FROM message \
           WHERE id = {plain};
         UPDATE attachment SET transfer_name = CAST(?6 AS BLOB), transfer_name_st = NULL, mime_type = CAST(?7 AS BLOB), \
           mime_type_st = NULL, uti = CAST(?8 AS BLOB), uti_st = NULL, emoji_description = CAST(?9 AS BLOB), \
           emoji_description_st = NULL, total_bytes = NULL, total_bytes_st = 9, \
           is_sticker = 5, is_sticker_st = NULL \
           WHERE id IN (SELECT attachment_id FROM message_attachment WHERE message_id = {plain});
         UPDATE handle SET address = CAST(?10 AS BLOB) WHERE id = {handle};
         UPDATE message SET item_kind = 5, item_type = 1, item_type_st = NULL, \
           group_title = CAST(?11 AS BLOB), group_title_st = NULL WHERE id = {rename};
         UPDATE message SET assoc_kind = 16, assoc_emoji = CAST(?12 AS BLOB), assoc_emoji_st = NULL \
           WHERE id = {reaction};
         UPDATE message SET item_kind = 99, is_from_me = 7, is_from_me_st = NULL, \
           date_raw = NULL, date_st = 9, date_unit = NULL, date_utc_ns = NULL, \
           service_class = 77, schedule_type = 5, schedule_type_st = NULL, \
           balloon_bundle_id = NULL, balloon_bundle_id_st = 2, \
           expressive_send_style_id = x'', expressive_send_style_id_st = NULL, \
           item_type = 42, item_type_st = NULL WHERE id = {unknown};
         UPDATE message_body SET edit_label = 55 \
           WHERE id = (SELECT current_body_id FROM message WHERE id = {unknown});
         UPDATE message SET is_from_me = NULL, is_from_me_st = 2, date_raw = 123, \
           date_st = NULL, date_unit = 9, date_utc_ns = NULL WHERE id = {odd_unit};
         UPDATE message SET is_from_me = NULL, is_from_me_st = 9 WHERE id = {odd_state};"
    );
    // `execute_batch` takes no parameters: bind them per statement.
    let params: Vec<String> = vec![
        body,
        older,
        long('s', 600),
        long('a', 300),
        long('h', 600),
        long('n', 300),
        long('t', 300),
        long('u', 300),
        long('g', 600),
        long('d', 300),
        long('r', 300),
        long_emoji(),
    ];
    for stmt in sql.split(";\n") {
        let stmt = stmt.trim();
        if stmt.is_empty() {
            continue;
        }
        let mut st = c.prepare(stmt).expect(stmt);
        // Numbered parameters: `?N` is index N.
        for k in 1..=st.parameter_count() {
            if st.parameter_name(k).is_some() {
                st.raw_bind_parameter(k, params[k - 1].as_str())
                    .expect("bind");
            }
        }
        st.raw_execute().expect(stmt);
    }
    drop(c);
    let reader = lib.open_read(&id).expect("reader");
    Hostile {
        _tmp: tmp,
        reader,
        plain,
        handle,
        rename,
        reaction,
        target,
        unknown,
        odd_unit,
        odd_state,
    }
}

fn item(h: &Hostile, id: i64) -> ExportItem {
    export_item(
        &h.reader,
        None,
        MessageId::new(id),
        Reveal::default(),
        LowerCaps::default(),
    )
    .expect("export item")
}

fn whole(o: &OptContent, units: u32) {
    let t = o.text().expect("a value");
    assert!(!t.truncated, "cut at {}", t.text.expose().len());
    assert_eq!(t.full_utf16, units);
    assert_eq!(t.text.expose().encode_utf16().count(), units as usize);
}

fn cut(o: &OptContent) -> bool {
    o.text().is_some_and(|t| t.truncated)
}

fn body_units(m: &AssembledMessage) -> (usize, bool) {
    let BodyView::Text(b) = &m.body else {
        panic!("a body with text");
    };
    let n = b
        .segments
        .iter()
        .map(|s| match &s.kind {
            SegmentKind::Text(t) => t.text.expose().encode_utf16().count(),
            _ => 0,
        })
        .sum();
    (n, b.truncated)
}

#[test]
fn export_caps_return_untruncated_values_where_the_viewer_cuts() {
    let h = hostile();
    // The viewer still cuts (Caps::PAGE), and its items say so.
    let d = h
        .reader
        .message_detail(MessageId::new(h.plain), Reveal::default())
        .expect("detail");
    assert_eq!(body_units(&d.message), (8192, true));
    assert_eq!(d.message.cap_miss(), Some(CapMiss::BodyTruncated));
    assert!(cut(&d.subject) && cut(&d.account));
    let a = d.attachments.first().expect("attachment");
    assert!(cut(&a.name) && cut(&a.declared_type) && cut(&a.uti));
    assert!(cut(&a.emoji_description));
    assert!(d.history.iter().all(|v| cut(&v.value)));
    assert!(d
        .versions
        .iter()
        .all(|v| matches!(&v.text, VersionText::Text(p) if p.iter().any(|p| matches!(p, TextPiece::Text(t) if t.truncated)))));

    // Under EXPORT every value is whole.
    let x = item(&h, h.plain);
    assert_eq!(x.message.cap_miss(), None);
    assert_eq!(body_units(&x.message), (9000, false));
    let att = x.message.attachments.first().expect("attachment");
    whole(&att.name, 300);
    whole(&att.declared_type, 300);
    let d = export_detail(
        &h.reader,
        MessageId::new(h.plain),
        Reveal::default(),
        LowerCaps::default(),
    )
    .expect("detail");
    whole(&d.subject, 600);
    whole(&d.account, 300);
    let a = d.attachments.first().expect("attachment");
    whole(&a.name, 300);
    whole(&a.declared_type, 300);
    whole(&a.uti, 300);
    whole(&a.emoji_description, 600);
    let subject_history: Vec<&oma_archive::HistoryValue> = d
        .history
        .iter()
        .filter(|v| v.column == HistoryColumn::Subject)
        .collect();
    assert_eq!(subject_history.len(), 1);
    whole(&subject_history[0].value, 600);
    assert_eq!(d.versions.len(), 2);
    for v in &d.versions {
        let VersionText::Text(p) = &v.text else {
            panic!("version text");
        };
        let [TextPiece::Text(t)] = p.as_slice() else {
            panic!("one piece");
        };
        assert!(!t.truncated);
        assert_eq!(t.text.expose().len(), 9000);
    }

    // The sender's address.
    let p =
        export_handle(&h.reader, HandleId::new(h.handle), LowerCaps::default()).expect("handle");
    whole(&p.address, 300);
    let lowered = export_handle(
        &h.reader,
        HandleId::new(h.handle),
        LowerCaps {
            identifier_utf16: Some(256),
            ..LowerCaps::default()
        },
    )
    .expect("handle");
    assert!(cut(&lowered.address));

    // A group rename's title.
    let e = item(&h, h.rename);
    let title = e
        .message
        .event
        .as_ref()
        .and_then(|e| e.title.as_ref())
        .expect("title");
    whole(title, 300);
    let page = h
        .reader
        .message_detail(MessageId::new(h.rename), Reveal::default())
        .expect("detail");
    assert_eq!(page.message.cap_miss(), Some(CapMiss::EventTitleTruncated));

    // Emoji of 65+ bytes: on the reaction row and on its target's chip.
    let emoji = long_emoji();
    let r = item(&h, h.reaction);
    let got = r
        .message
        .association
        .as_ref()
        .and_then(|a| a.emoji.as_ref())
        .and_then(OptContent::text)
        .expect("emoji");
    assert!(!got.truncated);
    assert_eq!(got.text.expose(), &emoji);
    let t = item(&h, h.target);
    let chips = &t.message.reactions.as_ref().expect("reactions").chips;
    assert!(chips.iter().any(|c| c
        .emoji
        .as_ref()
        .and_then(OptContent::text)
        .is_some_and(|e| !e.truncated && e.text.expose() == &emoji)));
    let page = h
        .reader
        .message_detail(MessageId::new(h.reaction), Reveal::default())
        .expect("detail");
    assert_eq!(page.message.cap_miss(), Some(CapMiss::EmojiTruncated));
}

#[test]
fn a_missed_cap_is_refused_in_every_build() {
    let h = hostile();
    let refused = |id: i64, lower: LowerCaps| match export_item(
        &h.reader,
        None,
        MessageId::new(id),
        Reveal::default(),
        lower,
    ) {
        Err(ArchiveError::Internal(why)) => why,
        other => panic!("expected a refusal, got {other:?}"),
    };
    let truncated = "value truncated under export caps";
    for lower in [
        LowerCaps {
            body_utf16: Some(8192),
            ..LowerCaps::default()
        },
        LowerCaps {
            identifier_utf16: Some(256),
            ..LowerCaps::default()
        },
        LowerCaps {
            attachments: Some(0),
            ..LowerCaps::default()
        },
    ] {
        assert_eq!(refused(h.plain, lower), truncated);
    }
    assert_eq!(
        refused(
            h.reaction,
            LowerCaps {
                emoji_bytes: Some(64),
                ..LowerCaps::default()
            }
        ),
        truncated
    );
    assert_eq!(
        refused(
            h.target,
            LowerCaps {
                reactions: Some(0),
                ..LowerCaps::default()
            }
        ),
        truncated
    );
    // Lowered caps that cut nothing are not refused.
    let fine = export_item(
        &h.reader,
        None,
        MessageId::new(h.plain),
        Reveal::default(),
        LowerCaps {
            body_utf16: Some(9000),
            identifier_utf16: Some(300),
            ..LowerCaps::default()
        },
    )
    .expect("whole at the exact length");
    assert_eq!(fine.message.cap_miss(), None);
    // The check reads every marker the viewer's items carry.
    let page = h
        .reader
        .message_detail(MessageId::new(h.plain), Reveal::default())
        .expect("detail");
    assert!(page.message.cap_miss().is_some());
}

#[test]
fn unknown_codes_and_states_survive_under_export() {
    let h = hostile();
    let x = item(&h, h.unknown);
    let c = &x.codes;
    assert_eq!(c.item, Err(99));
    assert_eq!(c.direction, RawDirection::UnknownValue { raw: 7 });
    assert_eq!(
        (c.date.raw, c.date.state, c.date.unit),
        (None, Some(Err(9)), None)
    );
    assert_eq!(c.service, Some(Err(77)));
    assert_eq!(
        (c.schedule_type.raw, c.schedule_type.state),
        (Some(5), None)
    );
    assert_eq!((c.item_type.raw, c.item_type.state), (Some(42), None));
    assert_eq!(
        (c.app_content.value, c.app_content.state),
        (None, Some(Ok(ValueState::Absent)))
    );
    assert_eq!((c.effect.value, c.effect.state), (Some(false), None));
    let b = c.body.expect("body codes");
    // `display_source` and `attributed_outcome` are closed by v1's
    // `CHECK`s; the edit label is not.
    assert_eq!(b.edit_label, Err(55));
    assert!(matches!(b.display_source, Some(Ok(_))));
    // The view types collapse the same row, as in M5.
    let m = &x.message;
    assert_eq!(m.item, None);
    assert_eq!(m.direction, DirectionCode::NotRecorded);
    assert_eq!(m.date.state, Some(ValueState::Unreadable));
    assert_eq!(m.service, None);
    assert_eq!(m.edit.label, None);
    assert!(!m.labels.contains(&MessageLabel::AppContent));
    assert!(m.labels.contains(&MessageLabel::ScheduleType(5)));

    // An unknown unit keeps its number; a direction with a state is
    // "not recorded" with that state, never an unknown value.
    let u = item(&h, h.odd_unit);
    assert_eq!(
        (u.codes.date.raw, u.codes.date.unit, u.codes.date.state),
        (Some(123), Some(Err(9)), None)
    );
    assert_eq!(u.message.date.unit, None);
    assert_eq!(
        u.codes.direction,
        RawDirection::NotRecorded {
            state: Some(Ok(ValueState::Absent))
        }
    );
    let s = item(&h, h.odd_state);
    assert_eq!(
        s.codes.direction,
        RawDirection::NotRecorded {
            state: Some(Err(9))
        }
    );
    assert_ne!(s.codes.direction, x.codes.direction);

    // Attachments: the declared size's unknown state, the raw sticker
    // value, the stored size, in the message's attachment order.
    let p = item(&h, h.plain);
    assert_eq!(p.codes.attachments.len(), p.message.attachments.len());
    let a = p.codes.attachments.first().expect("attachment codes");
    assert_eq!(
        (a.declared_bytes.raw, a.declared_bytes.state),
        (None, Some(Err(9)))
    );
    assert_eq!((a.sticker.raw, a.sticker.state), (Some(5), None));
    assert_eq!(a.stored_bytes, p.message.attachments[0].stored_bytes);
    assert_eq!(p.codes.direction, RawDirection::Received);

    // A reaction keeps its association codes.
    let r = item(&h, h.reaction);
    let assoc = r.codes.association.expect("association codes");
    assert_eq!(
        (assoc.present, assoc.kind),
        (1, Some(Ok(AssocKindCode::TapbackEmoji)))
    );
}
