//! The conversation list (M5 spec §3.5, §13.2, slice 5b1).
//!
//! - **The viewer catalogue:** every entry and scenario imports, and the
//!   list's order and each row's facts match the Python model's
//!   `viewer.conversations` (never the Rust output): the order by the
//!   newest dated row, the address count and group flag, the counts, the
//!   last dated instant, the first date of 2007 or later with the number
//!   of earlier-dated rows, "empty", and the preview.
//! - **Undated rows never pose as newest** (C2.1, C3.1), on a bulk archive
//!   with many undated rows, against an oracle built from
//!   `message.date_utc_ns` rather than `sort_key`.
//! - The name and count orders, the name filter, masking of one-time codes
//!   in previews (D115), `QueryTooBroad` (never `Damaged`), reads while an
//!   import holds the lock, stale statistics, and "reading writes nothing".
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;

use std::collections::BTreeMap;

use common::bulk::{generate, BulkSpec};
use common::import::{manifest_ref, read_only};
use common::viewer::{import_order, model_docs, Imported};
use common::*;
use oma_archive::hooks::{install, Faults};
use oma_archive::{
    ArchiveError, ArchiveReader, ChatId, ChatLabel, Conversation, ConversationSort, NameFilter,
    OptContent, PreviewBody, Recovery, TextPiece, CONVERSATIONS_PER_CALL,
};
use serde_json::Value;

/// Every row of the index, fetched in windows as the UI does.
fn all_rows(
    r: &ArchiveReader,
    sort: ConversationSort,
    f: Option<&NameFilter>,
) -> Vec<Conversation> {
    let index = r.conversation_index(sort, f).expect("index");
    assert!(!index.truncated);
    assert_eq!(index.total, index.ids.len() as u64);
    let mut out = Vec::new();
    for window in index.ids.chunks(CONVERSATIONS_PER_CALL) {
        let rows = r.conversations(window).expect("rows");
        assert_eq!(
            rows.iter().map(|c| c.id).collect::<Vec<_>>(),
            window,
            "rows come back in the order asked"
        );
        out.extend(rows);
    }
    out
}

fn shown_text(t: &oma_archive::ContentText) -> String {
    t.text.expose().clone()
}

/// The text of a preview's shown pieces, joined.
fn preview_text(c: &Conversation) -> String {
    match c.preview.as_ref().map(|p| &p.body) {
        Some(PreviewBody::Text(pieces)) => pieces
            .iter()
            .filter_map(|p| match p {
                TextPiece::Text(t) => Some(shown_text(t)),
                _ => None,
            })
            .collect(),
        _ => String::new(),
    }
}

fn opt_i64(v: &Value) -> Option<i64> {
    v.as_i64()
}

/// The list read from `i`'s archive, with chat and preview refs resolved
/// after the reader is closed.
fn listed(
    i: &Imported,
) -> (
    Vec<Conversation>,
    BTreeMap<i64, String>,
    BTreeMap<i64, String>,
) {
    let r = i.lib.open_read(&i.id).expect("reader");
    let rows = all_rows(&r, ConversationSort::Recent, None);
    drop(r);
    let c = read_only(&i.db());
    let chats = rows
        .iter()
        .map(|row| (row.id.get(), manifest_ref("chat", &c, row.id.get())))
        .collect();
    let previews = rows
        .iter()
        .filter_map(|row| row.preview.as_ref())
        .map(|p| {
            (
                p.message.get(),
                manifest_ref("message", &c, p.message.get()),
            )
        })
        .collect();
    (rows, chats, previews)
}

#[test]
fn the_list_matches_the_model_for_every_entry_and_scenario() {
    let mut previews = 0;
    let mut undated_chats = 0;
    let mut empty_chats = 0;
    let mut earlier = 0;
    for (what, m) in model_docs() {
        let orders = m["orders"].as_array().expect("orders");
        let i = import_order(&orders[0], &what);
        let (rows, chat_refs, preview_refs) = listed(&i);
        let want = m["viewer"]["conversations"]
            .as_array()
            .expect("conversations");
        let got_order: Vec<&str> = rows
            .iter()
            .map(|r| chat_refs[&r.id.get()].as_str())
            .collect();
        let want_order: Vec<&str> = want
            .iter()
            .map(|c| c["chat"].as_str().expect("chat"))
            .collect();
        assert_eq!(got_order, want_order, "{what}: order");
        for (row, w) in rows.iter().zip(want) {
            let at = format!("{what}: {}", w["chat"]);
            assert_eq!(row.addresses, w["addresses"].as_u64().expect("n"), "{at}");
            assert_eq!(row.group, w["group"] == true, "{at}");
            let stats = row.stats.expect("statistics after a completed import");
            assert!(!row.stats_stale, "{at}");
            assert_eq!(
                stats.items_recorded,
                w["itemsRecorded"].as_u64().expect("n"),
                "{at}"
            );
            assert_eq!(
                stats.undated,
                w["undatedItems"].as_u64().expect("n"),
                "{at}"
            );
            assert_eq!(
                row.undated.n,
                w["undatedItems"].as_u64().expect("n"),
                "{at}"
            );
            assert!(!row.undated.capped);
            assert_eq!(row.last_dated_utc_ns, opt_i64(&w["lastDatedUtcNs"]), "{at}");
            assert_eq!(
                row.first_dated_2007_utc_ns,
                opt_i64(&w["firstDate2007UtcNs"]),
                "{at}"
            );
            assert_eq!(
                row.earlier_dated.n,
                w["earlierDatedItems"].as_u64().expect("n"),
                "{at}"
            );
            assert_eq!(row.empty, w["empty"] == true, "{at}");
            let preview = row
                .preview
                .as_ref()
                .map(|p| preview_refs[&p.message.get()].as_str());
            assert_eq!(preview, w["preview"].as_str(), "{at}: preview");
            if let Some(p) = &row.preview {
                assert!(row.last_dated_utc_ns.is_some_and(|l| p.utc_ns <= l), "{at}");
            }
            previews += usize::from(row.preview.is_some());
            undated_chats += usize::from(row.undated.n > 0 && row.last_dated_utc_ns.is_some());
            empty_chats += usize::from(row.empty);
            earlier += row.earlier_dated.n;
        }
        // No masked string reaches a preview (D115: list previews never
        // reveal).
        for kind in ["oneTimeCodes", "earlierText"] {
            for s in m["viewer"]["masked"][kind].as_array().expect("masked") {
                let s = s.as_str().expect("text");
                for row in &rows {
                    assert!(
                        !preview_text(row).contains(s),
                        "{what}: a masked {kind} in a preview"
                    );
                }
            }
        }
    }
    // The catalogue exercised what the rules are about.
    assert!(previews > 10, "{previews}");
    assert!(undated_chats > 0, "a chat mixing dated and undated rows");
    assert!(empty_chats > 0, "an empty chat");
    assert!(earlier > 0, "a chat with pre-2007 rows");
}

/// The model's order with an independent oracle: each chat's newest
/// `message.date_utc_ns` (not `sort_key`), newest first; chats without one
/// last, by id.
#[test]
fn undated_rows_never_pose_as_newest() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let mut spec = BulkSpec::new(3_000);
    spec.undated_permille = 60;
    spec.chats = 30;
    spec.big_chat_permille = 100;
    {
        let mut a = lib.open(&id).expect("open");
        let rep = generate(&mut a, &spec);
        assert!(rep.undated > 50 && rep.reactions > 50, "{rep:?}");
    }
    let r = lib.open_read(&id).expect("reader");
    let rows = all_rows(&r, ConversationSort::Recent, None);
    drop(r);
    let c = read_only(&db_path(&tmp, &id));
    type Facts = (i64, Option<i64>, i64, i64);
    let mut oracle: Vec<Facts> = c
        .prepare(
            "SELECT ch.id, max(m.date_utc_ns), \
                    coalesce(sum(m.id IS NOT NULL AND m.date_utc_ns IS NULL), 0), count(m.id) \
             FROM chat ch LEFT JOIN chat_message cm ON cm.chat_id = ch.id \
             LEFT JOIN message m ON m.id = cm.message_id GROUP BY ch.id",
        )
        .expect("prepare")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    oracle.sort_by(|a, b| match (a.1, b.1) {
        (Some(x), Some(y)) => y.cmp(&x).then(a.0.cmp(&b.0)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.0.cmp(&b.0),
    });
    assert_eq!(
        rows.iter().map(|r| r.id.get()).collect::<Vec<_>>(),
        oracle.iter().map(|o| o.0).collect::<Vec<_>>(),
        "the order follows the newest dated row only"
    );
    let mut mixed = 0;
    for (row, (chat, newest, undated, n)) in rows.iter().zip(&oracle) {
        assert_eq!(row.last_dated_utc_ns, *newest, "chat {chat}");
        assert_eq!(
            row.undated.n,
            u64::try_from(*undated).expect("n"),
            "chat {chat}"
        );
        assert_eq!(
            row.stats.expect("stats").items_recorded,
            u64::try_from(*n).expect("n")
        );
        if *undated > 0 && newest.is_some() {
            mixed += 1;
        }
        // The preview is dated, never folded, and never newer than the
        // newest dated row.
        let Some(p) = &row.preview else {
            assert!(newest.is_none(), "chat {chat}: a dated chat has a preview");
            continue;
        };
        let (date, folded): (Option<i64>, i64) = c
            .query_row(
                "SELECT m.date_utc_ns, EXISTS(SELECT 1 FROM message t \
                     JOIN chat_message tc ON tc.message_id = t.id AND tc.chat_id = ?2 \
                     WHERE m.assoc_present = 1 AND m.assoc_kind BETWEEN 10 AND 27 \
                       AND t.key_kind = 1 AND t.guid = m.assoc_target_guid) \
                 FROM message m WHERE m.id = ?1",
                [p.message.get(), *chat],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("preview row");
        assert_eq!(date, Some(p.utc_ns), "chat {chat}");
        assert_eq!(folded, 0, "chat {chat}: a folded reaction is not a preview");
        assert!(newest.is_some_and(|n| p.utc_ns <= n));
    }
    assert!(mixed >= 5, "chats mixing dated and undated rows: {mixed}");
}

fn label_key(row: &Conversation) -> Option<String> {
    match &row.label {
        ChatLabel::Name(t) | ChatLabel::Identifier(t) => Some(shown_text(t).to_lowercase()),
        ChatLabel::Participants { first, .. } => Some(
            first
                .iter()
                .filter_map(|p| p.address.text().map(shown_text))
                .collect::<Vec<_>>()
                .join(", ")
                .to_lowercase(),
        ),
        _ => None,
    }
}

#[test]
fn name_and_count_orders_and_the_name_filter() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let mut spec = BulkSpec::new(1_500);
    spec.chats = 40;
    {
        let mut a = lib.open(&id).expect("open");
        generate(&mut a, &spec);
    }
    let r = lib.open_read(&id).expect("reader");
    // Name: by the label's text, case-insensitively, ties by id.
    let rows = all_rows(&r, ConversationSort::Name, None);
    assert_eq!(rows.len(), 40);
    let keys: Vec<(Option<String>, i64)> =
        rows.iter().map(|r| (label_key(r), r.id.get())).collect();
    for w in keys.windows(2) {
        match (&w[0].0, &w[1].0) {
            (Some(a), Some(b)) => assert!(a < b || (a == b && w[0].1 < w[1].1), "{w:?}"),
            (None, Some(_)) => panic!("an unlabelled chat before a labelled one"),
            (Some(_), None) => {}
            (None, None) => assert!(w[0].1 < w[1].1),
        }
    }
    assert!(rows.iter().any(|r| matches!(r.label, ChatLabel::Name(_))));
    assert!(rows
        .iter()
        .any(|r| matches!(r.label, ChatLabel::Participants { .. })));
    // Count: the most items first.
    let rows = all_rows(&r, ConversationSort::Count, None);
    let counts: Vec<u64> = rows
        .iter()
        .map(|r| r.stats.expect("stats").items_recorded)
        .collect();
    assert!(counts.windows(2).all(|w| w[0] >= w[1]), "{counts:?}");
    // Recent, filtered: names, identifiers and addresses, any case.
    let all = all_rows(&r, ConversationSort::Recent, None);
    drop(r);
    let c = read_only(&db_path(&tmp, &id));
    let mut texts: BTreeMap<i64, Vec<String>> = BTreeMap::new();
    for (chat, v) in c
        .prepare(
            "SELECT id, display_name FROM chat WHERE display_name IS NOT NULL \
             UNION ALL SELECT id, chat_identifier FROM chat WHERE chat_identifier IS NOT NULL \
             UNION ALL SELECT ch.chat_id, h.address FROM chat_handle ch \
                JOIN handle h ON h.id = ch.handle_id WHERE h.address IS NOT NULL",
        )
        .expect("prepare")
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)))
        .expect("query")
        .map(|r| r.expect("row"))
    {
        texts
            .entry(chat)
            .or_default()
            .push(String::from_utf8_lossy(&v).to_lowercase());
    }
    drop(c);
    let r = lib.open_read(&id).expect("reader");
    for needle in ["group-1", "SYNTHBULK3", "bulk", "no-such-chat"] {
        let f = NameFilter::new(needle).expect("filter");
        let got: Vec<i64> = r
            .conversation_index(ConversationSort::Recent, Some(&f))
            .expect("filtered")
            .ids
            .iter()
            .map(|i| i.get())
            .collect();
        let lower = needle.to_lowercase();
        let want: Vec<i64> = all
            .iter()
            .map(|row| row.id.get())
            .filter(|id| {
                texts
                    .get(id)
                    .is_some_and(|t| t.iter().any(|s| s.contains(&lower)))
            })
            .collect();
        assert_eq!(got, want, "{needle}");
    }
    assert!(
        r.conversation_index(
            ConversationSort::Recent,
            Some(&NameFilter::new("bulk").expect("f"))
        )
        .expect("all match")
        .total
            == 40
    );
}

#[test]
fn a_broad_list_is_too_broad_never_damaged() {
    let (_tmp, lib) = library();
    let id = lib.create(None).expect("create");
    {
        let mut a = lib.open(&id).expect("open");
        let mut spec = BulkSpec::new(800);
        spec.chats = 150;
        generate(&mut a, &spec);
    }
    let r = lib.open_read(&id).expect("reader");
    let ids = r
        .conversation_index(ConversationSort::Recent, None)
        .expect("index")
        .ids;
    {
        let _f = install(Faults {
            // No ticks left: the first progress check (after 1,000 VM
            // operations) stops the phase.
            view_op_budget: Some(0),
            ..Faults::default()
        });
        for sort in [
            ConversationSort::Recent,
            ConversationSort::Name,
            ConversationSort::Count,
        ] {
            let e = r.conversation_index(sort, None).expect_err("too broad");
            assert!(matches!(e, ArchiveError::QueryTooBroad), "{e:?}");
        }
        // The rows are bounded by construction (every statement is a
        // primary-key lookup or carries a LIMIT), and SQLite counts
        // operations per statement, so short statements may finish under
        // any budget; whatever happens, it is never `Damaged`.
        match r.conversations(&ids) {
            Ok(rows) => assert_eq!(rows.len(), ids.len()),
            Err(e) => assert!(matches!(e, ArchiveError::QueryTooBroad), "{e:?}"),
        }
    }
    // The reader is usable again at once.
    assert_eq!(r.conversations(&ids[..2]).expect("rows").len(), 2);
    let too_many: Vec<ChatId> = (1..=201).map(ChatId::new).collect();
    assert!(matches!(
        r.conversations(&too_many),
        Err(ArchiveError::Internal(_))
    ));
    // Unknown ids are left out.
    assert!(r
        .conversations(&[ChatId::new(999_999)])
        .expect("rows")
        .is_empty());
}

#[test]
fn reading_the_list_writes_nothing_and_works_while_an_import_holds_the_lock() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    {
        let mut a = lib.open(&id).expect("open");
        generate(&mut a, &BulkSpec::new(600));
    }
    let before = data_snapshot(&tmp, &id);
    {
        let r = lib.open_read(&id).expect("reader");
        for sort in [
            ConversationSort::Recent,
            ConversationSort::Name,
            ConversationSort::Count,
        ] {
            all_rows(&r, sort, Some(&NameFilter::new("synth").expect("f")));
        }
    }
    assert_eq!(data_snapshot(&tmp, &id), before, "the list wrote nothing");

    // An import holds the lock and its writer is open: the list still
    // reads, never `Locked`.
    let mut a = lib.open(&id).expect("open");
    a.begin_write(Recovery::Run).expect("begin_write");
    let r = lib.open_read(&id).expect("reader while locked");
    let rows = all_rows(&r, ConversationSort::Recent, None);
    assert!(!rows.is_empty());
    drop(r);
    a.close();
}

#[test]
fn statistics_are_marked_stale_while_a_newer_session_is_unfinished() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    {
        let mut a = lib.open(&id).expect("open");
        generate(&mut a, &BulkSpec::new(300));
    }
    {
        let r = lib.open_read(&id).expect("reader");
        assert!(all_rows(&r, ConversationSort::Recent, None)
            .iter()
            .all(|c| !c.stats_stale));
    }
    {
        let c = raw(&db_path(&tmp, &id));
        insert_session(&c, 0);
    }
    let r = lib.open_read(&id).expect("reader");
    let rows = all_rows(&r, ConversationSort::Recent, None);
    assert!(rows.iter().all(|c| c.stats_stale && c.stats.is_some()));
}

/// D115 in previews: a one-time-code part is withheld; its ranges apply
/// only when the formatting was decoded cleanly, otherwise the whole text
/// is withheld.
#[test]
fn previews_withhold_one_time_codes() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    {
        let mut a = lib.open(&id).expect("open");
        let mut spec = BulkSpec::new(300);
        spec.reaction_permille = 0;
        spec.undated_permille = 0;
        generate(&mut a, &spec);
    }
    let (chat, message) = {
        let r = lib.open_read(&id).expect("reader");
        let first = r
            .conversation_index(ConversationSort::Recent, None)
            .expect("index")
            .ids[0];
        let row = r.conversations(&[first]).expect("row").remove(0);
        let p = row.preview.expect("preview");
        assert!(matches!(&p.body, PreviewBody::Text(pieces) if pieces.len() == 1));
        (first, p.message)
    };
    let (body, text): (i64, String) = {
        let c = raw(&db_path(&tmp, &id));
        let (body, text): (i64, String) = c
            .query_row(
                "SELECT b.id, b.display_text FROM message m \
                 JOIN message_body b ON b.id = m.current_body_id WHERE m.id = ?1",
                [message.get()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("body");
        assert!(
            text.encode_utf16().count() > 6,
            "a body long enough to mask"
        );
        // Units 2..5 are a one-time code (flags bit 4).
        c.execute(
            "INSERT INTO message_body_part (body_id, ordinal, utf16_start, utf16_end, \
                 utf8_start, utf8_end, flags, keys_present) VALUES (?1, 0, 2, 5, 2, 5, 16, 0)",
            [body],
        )
        .expect("part");
        (body, text)
    };
    let pieces_of = || {
        let r = lib.open_read(&id).expect("reader");
        let row = r.conversations(&[chat]).expect("row").remove(0);
        match row.preview.expect("preview").body {
            PreviewBody::Text(p) => p,
            other => panic!("{other:?}"),
        }
    };
    // The bulk bodies come from the text column (display_source 2): the
    // ranges are not trusted, so the whole text is withheld.
    let pieces = pieces_of();
    assert_eq!(
        pieces,
        vec![TextPiece::Masked(oma_archive::MaskKind::OneTimeCode)]
    );
    // Decoded cleanly (display_source 1): only the code is withheld.
    {
        let c = raw(&db_path(&tmp, &id));
        c.execute(
            "UPDATE message_body SET display_source = 1 WHERE id = ?1",
            [body],
        )
        .expect("source");
    }
    let pieces = pieces_of();
    assert_eq!(pieces.len(), 3, "{pieces:?}");
    assert_eq!(
        pieces[1],
        TextPiece::Masked(oma_archive::MaskKind::OneTimeCode)
    );
    let units: Vec<u16> = text.encode_utf16().collect();
    let expect_head = String::from_utf16(&units[..2]).expect("utf16");
    let expect_tail = String::from_utf16(&units[5..]).expect("utf16");
    let TextPiece::Text(head) = &pieces[0] else {
        panic!("text")
    };
    let TextPiece::Text(tail) = &pieces[2] else {
        panic!("text")
    };
    assert_eq!(shown_text(head), expect_head);
    assert_eq!(shown_text(tail), expect_tail);
    // A run issue makes the ranges untrusted again.
    {
        let c = raw(&db_path(&tmp, &id));
        c.execute(
            "UPDATE message_body SET run_issue = 12 WHERE id = ?1",
            [body],
        )
        .expect("run issue");
    }
    assert_eq!(
        pieces_of(),
        vec![TextPiece::Masked(oma_archive::MaskKind::OneTimeCode)]
    );
}

/// Labels come from what is recorded, in §8.4's order, and a chat name
/// that is recorded but unreadable never falls through.
#[test]
fn labels_follow_the_recorded_values() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    {
        let mut a = lib.open(&id).expect("open");
        let mut spec = BulkSpec::new(200);
        spec.chats = 10;
        spec.group_every = 2;
        generate(&mut a, &spec);
    }
    {
        let c = raw(&db_path(&tmp, &id));
        // Chat 2 (a group with a name): the name becomes unreadable.
        c.execute(
            "UPDATE chat SET display_name = NULL, display_name_st = 4 WHERE id = 2",
            [],
        )
        .expect("unreadable");
        // Chat 1: no participants and no identifier.
        c.execute("DELETE FROM chat_handle WHERE chat_id = 1", [])
            .expect("handles");
        c.execute(
            "UPDATE chat SET chat_identifier = NULL, chat_identifier_st = 1 WHERE id = 1",
            [],
        )
        .expect("identifier");
        // Chat 3 (not a group): its address is stored as a BLOB.
        c.execute(
            "INSERT INTO value_issue (entity, row_id, col, kind) \
             SELECT 1, handle_id, 101, 3 FROM chat_handle WHERE chat_id = 3",
            [],
        )
        .expect("issue");
    }
    let r = lib.open_read(&id).expect("reader");
    let rows = r
        .conversations(&[
            ChatId::new(1),
            ChatId::new(2),
            ChatId::new(3),
            ChatId::new(4),
        ])
        .expect("rows");
    assert_eq!(rows[0].label, ChatLabel::Unnamed);
    assert_eq!(rows[0].addresses, 0);
    assert!(!rows[0].group);
    assert!(matches!(
        rows[1].label,
        ChatLabel::NameUnreadable(oma_archive::schema::codes::ValueState::Unreadable)
    ));
    assert!(rows[1].group && rows[1].addresses == 3);
    let ChatLabel::Participants { first, more } = &rows[2].label else {
        panic!("{:?}", rows[2].label)
    };
    assert_eq!((first.len(), *more), (1, 0));
    let OptContent::Text(addr) = &first[0].address else {
        panic!("address")
    };
    assert!(addr.stored_as_blob && !addr.truncated && !addr.lossy);
    assert!(matches!(rows[3].label, ChatLabel::Name(_)));
}
