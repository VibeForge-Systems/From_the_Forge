//! Slice 6b1 of M6 (spec §3.2, §3.4, §3.5, §14.1): the export reads.
//!
//! - **Lock and snapshot.** `hold_for_export` shuts out every writer
//!   (`Library::open` gets `Locked`, the listing says in use) while viewer
//!   readers keep reading; `with_export_snapshot` is one read transaction
//!   (a write made behind the lock's back is not seen inside it), its mode
//!   is fixed (nested reads on the same reader refuse), and the
//!   generation moves only when an import writes.
//! - **Cursors.** Every timeline row is returned once, as an item or as a
//!   folded reaction with its target; the items are exactly the viewer's;
//!   dated rows come before undated ones; dated ranges are half-open and
//!   never reach an undated row; messages in no conversation are found,
//!   within the same bounds; a message in two conversations is in both
//!   timelines; batches are cut by count and by measured bytes; statements
//!   are reset before a cursor returns; a cancel stops `assemble` while a
//!   cursor is live.
//! - **Uncapped rows and ledger columns.** Over 10,000 reaction rows on one
//!   target, over 50 versions and over 200 edit entries come back whole;
//!   the detail, attachment, chat and handle columns M5 leaves out are
//!   read raw; a subject history value of a message with an unsent part is
//!   withheld unless earlier content is revealed.
//! - **Parity with the viewer** over every entry and scenario of the viewer
//!   catalogue: the assembled item, the versions, the subject, the
//!   account, the history values, the reaction rows and the conversations
//!   equal what the details read under `Caps::EXPORT`.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;

use std::collections::BTreeSet;
use std::time::Duration;

use common::bulk::{generate, BulkSpec};
use common::viewer::{import_order, model_docs, Imported};
use common::*;
use oma_archive::hooks::{export_detail, export_item, statements_stepping, LowerCaps};
use oma_archive::schema::codes::{TimeUnit, ValueState};
use oma_archive::{
    Anchor, ArchiveEntry, ArchiveError, ArchiveId, ArchiveReader, BatchLimits, CancelToken, ChatId,
    ConversationSort, Direction, ExportDepth, ExportMessage, ExportSnapshot, HistoryColumn,
    HistoryState, Library, MessageId, OptContent, Reveal, TimeRange, TimelineCursor,
    ValueHistoryEntry, WithheldReason,
};

const REVEAL_ALL: Reveal = Reveal {
    earlier_text: true,
    one_time_code: true,
};

type Snap<'a, 's> = &'a ExportSnapshot<'s>;

/// A bulk archive of `n` messages (synthetic), with `undated` per mille
/// undated rows.
fn bulk(n: usize, undated: u64) -> (tempfile::TempDir, Library, ArchiveId) {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    {
        let mut a = lib.open(&id).expect("open");
        let mut spec = BulkSpec::new(n);
        spec.chats = 5;
        spec.reaction_permille = 150;
        spec.attachment_permille = 100;
        spec.undated_permille = undated;
        generate(&mut a, &spec);
    }
    (tmp, lib, id)
}

fn ints(c: &rusqlite::Connection, sql: &str, p: &[&dyn rusqlite::ToSql]) -> Vec<i64> {
    c.prepare(sql)
        .expect(sql)
        .query_map(p, |r| r.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows")
}

fn one(c: &rusqlite::Connection, sql: &str, p: &[&dyn rusqlite::ToSql]) -> i64 {
    c.query_row(sql, p, |r| r.get(0)).expect(sql)
}

/// Every batch of `cur`: the items and the folded `(row, target)` pairs,
/// each in order, and the batches' item counts.
type Drained = (Vec<i64>, Vec<(i64, i64)>, Vec<usize>);

fn drain(cur: &mut TimelineCursor<'_, '_>) -> Result<Drained, ArchiveError> {
    let (mut items, mut folded, mut sizes) = (Vec::new(), Vec::new(), Vec::new());
    while let Some(b) = cur.next_batch()? {
        sizes.push(b.items.len());
        items.extend(b.items.iter().map(|m| m.get()));
        folded.extend(b.folded.iter().map(|(r, t)| (r.get(), t.get())));
    }
    Ok((items, folded, sizes))
}

fn snapshot<T>(
    r: &ArchiveReader,
    lib: &Library,
    id: &ArchiveId,
    f: impl FnOnce(Snap<'_, '_>) -> Result<T, ArchiveError>,
) -> T {
    let hold = lib.hold_for_export(id).expect("hold");
    r.with_export_snapshot(&hold, &CancelToken::new(), f)
        .expect("snapshot")
}

/// The viewer's timeline items of `chat`: every page of both ranges.
fn viewer_items(v: &ArchiveReader, chat: ChatId) -> BTreeSet<i64> {
    let mut out = BTreeSet::new();
    for (anchor, dir) in [
        (Anchor::Newest, Direction::Older),
        (Anchor::Undated, Direction::Newer),
    ] {
        let mut page = v
            .thread_page(chat, anchor, dir, 100, None, Reveal::default())
            .expect("page");
        loop {
            out.extend(page.items.iter().map(|m| m.id.get()));
            let next = match dir {
                Direction::Older => page.older_anchor.filter(|_| page.has_older),
                _ => page.newer_anchor.filter(|_| page.has_newer),
            };
            let Some(next) = next else { break };
            page = v
                .thread_page(
                    chat,
                    Anchor::Message {
                        id: next,
                        include: false,
                    },
                    dir,
                    100,
                    None,
                    Reveal::default(),
                )
                .expect("page");
        }
    }
    out
}

/// The target a folded reaction row names, through its target GUID.
fn target_of(c: &rusqlite::Connection, row: i64) -> i64 {
    one(
        c,
        "SELECT t.id FROM message r JOIN message t \
           ON t.guid = r.assoc_target_guid AND t.key_kind = 1 WHERE r.id = ?1",
        &[&row],
    )
}

/// Every row of every conversation is returned once, in timeline order,
/// as an item or as a folded reaction of a target in the same
/// conversation; the items are exactly the viewer's.
/// Returns how many rows folded.
fn check_timelines(
    r: &ArchiveReader,
    lib: &Library,
    id: &ArchiveId,
    db: &std::path::Path,
) -> usize {
    let c = read_only_conn(db);
    let viewer = lib.open_read(id).expect("viewer");
    let limits = [BatchLimits::DEFAULT, BatchLimits::new(3, u64::MAX)];
    let mut folds = 0;
    snapshot(r, lib, id, |s| {
        for chat in s.chat_ids()? {
            let all = ints(
                &c,
                "SELECT message_id FROM chat_message WHERE chat_id = ?1 \
                 ORDER BY sort_key, message_id",
                &[&chat.get()],
            );
            for l in limits {
                let (items, folded, _) =
                    drain(&mut s.timeline(chat, TimeRange::All).with_limits(l))?;
                let folded_rows: BTreeSet<i64> = folded.iter().map(|f| f.0).collect();
                let expected: Vec<i64> = all
                    .iter()
                    .copied()
                    .filter(|m| !folded_rows.contains(m))
                    .collect();
                assert_eq!(items, expected, "items in timeline order");
                assert_eq!(items.len() + folded.len(), all.len(), "every row once");
                for (row, target) in &folded {
                    assert_eq!(target_of(&c, *row), *target);
                    assert!(all.contains(target), "the target is in the conversation");
                }
                let set: BTreeSet<i64> = items.iter().copied().collect();
                assert_eq!(set, viewer_items(&viewer, chat), "the viewer's items");
                folds += folded.len();
            }
        }
        Ok(())
    });
    folds
}

fn read_only_conn(db: &std::path::Path) -> rusqlite::Connection {
    common::import::read_only(db)
}

// ---------------------------------------------------------------------------
// Lock and snapshot
// ---------------------------------------------------------------------------

#[test]
fn the_hold_shuts_out_writers_while_the_viewer_reads_alongside() {
    let (_tmp, lib, id) = bulk(300, 20);
    let other = lib.create(None).expect("second archive");
    let hold = lib.hold_for_export(&id).expect("hold");
    // As an import or a verify (in any instance) would open it.
    assert!(matches!(lib.open(&id), Err(ArchiveError::Locked)));
    assert!(matches!(
        lib.hold_for_export(&id),
        Err(ArchiveError::Locked)
    ));
    let listed = lib.list().expect("list");
    assert!(listed
        .iter()
        .any(|e| matches!(e, ArchiveEntry::InUse { id: x } if *x == id)));
    let r = lib.open_read(&id).expect("reader");
    let viewer = lib.open_read(&id).expect("viewer");
    let token = CancelToken::new();
    let n = r
        .with_export_snapshot(&hold, &token, |s| -> Result<usize, ArchiveError> {
            let chats = s.chat_ids()?;
            // A viewer reads beside the export.
            let index = viewer.conversation_index(ConversationSort::Recent, None)?;
            assert_eq!(index.ids.len(), chats.len());
            let page = viewer.thread_page(
                chats[0],
                Anchor::Newest,
                Direction::Older,
                50,
                None,
                Reveal::default(),
            )?;
            let first = page.items.first().expect("an item").id;
            // The snapshot's own reader refuses every other mode.
            assert!(matches!(r.info(), Err(ArchiveError::Internal(_))));
            assert!(r.message_detail(first, Reveal::default()).is_err());
            assert!(matches!(
                r.with_export_snapshot(&hold, &token, |_| Ok::<_, ArchiveError>(())),
                Err(ArchiveError::Internal(_))
            ));
            // The snapshot itself is unharmed.
            assert_eq!(s.chat_ids()?, chats);
            Ok(chats.len())
        })
        .expect("snapshot");
    assert_eq!(n, 5);
    // The mode ends with the call.
    r.info().expect("info after the snapshot");
    // A hold of another archive is refused.
    let wrong = lib.hold_for_export(&other).expect("other hold");
    assert!(matches!(
        r.with_export_snapshot(&wrong, &token, |_| Ok::<_, ArchiveError>(())),
        Err(ArchiveError::Internal(_))
    ));
    drop(hold);
    drop(lib.open(&id).expect("open after the hold"));
}

#[test]
fn one_snapshot_sees_one_state_and_the_generation_moves_with_an_import() {
    let (tmp, lib, id) = bulk(300, 20);
    let r = lib.open_read(&id).expect("reader");
    let g0 = r.archive_generation().expect("generation");
    assert_eq!(g0, r.archive_generation().expect("again"));
    let c = raw(&db_path(&tmp, &id));
    let chat = snapshot(&r, &lib, &id, |s| {
        assert_eq!(s.generation()?, g0);
        let chat = s.chat_ids()?[0];
        let mut cur = s.timeline(chat, TimeRange::All);
        let before = drain(&mut cur)?;
        assert!(!before.0.is_empty());
        // Writes made behind the lock's back (no import can) are not seen
        // inside the one read transaction.
        c.execute("DELETE FROM chat_message WHERE chat_id = ?1", [chat.get()])
            .expect("delete");
        insert_session(&c, 1);
        cur.rewind();
        assert_eq!(drain(&mut cur)?, before);
        assert_eq!(s.generation()?, g0);
        Ok(chat)
    });
    assert_ne!(r.archive_generation().expect("after"), g0);
    snapshot(&r, &lib, &id, |s| {
        assert!(s.timeline(chat, TimeRange::All).next_batch()?.is_none());
        Ok(())
    });
}

// ---------------------------------------------------------------------------
// Cursors
// ---------------------------------------------------------------------------

#[test]
fn timelines_return_every_row_once_dated_before_undated() {
    let (tmp, lib, id) = bulk(1500, 40);
    let r = lib.open_read(&id).expect("reader");
    assert!(
        check_timelines(&r, &lib, &id, &db_path(&tmp, &id)) > 0,
        "reactions folded"
    );
}

#[test]
fn dated_ranges_are_half_open_and_never_reach_undated_rows() {
    let (tmp, lib, id) = bulk(1500, 40);
    let c = read_only_conn(&db_path(&tmp, &id));
    let r = lib.open_read(&id).expect("reader");
    let chat = one(
        &c,
        "SELECT chat_id FROM chat_message GROUP BY chat_id ORDER BY count(*) DESC LIMIT 1",
        &[],
    );
    let undated = ints(
        &c,
        "SELECT message_id FROM chat_message WHERE chat_id = ?1 AND sort_key >= X'01'",
        &[&chat],
    );
    assert!(!undated.is_empty(), "the generator wrote undated rows");
    let dated: Vec<(i64, i64)> = c
        .prepare(
            "SELECT cm.message_id, m.date_utc_ns FROM chat_message cm \
             JOIN message m ON m.id = cm.message_id \
             WHERE cm.chat_id = ?1 AND cm.sort_key < X'01' ORDER BY cm.sort_key, cm.message_id",
        )
        .expect("prepare")
        .query_map([chat], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    let from = dated[dated.len() / 4].1;
    let until = dated[dated.len() / 2].1;
    assert!(from < until);
    let want: Vec<i64> = dated
        .iter()
        .filter(|(_, t)| *t >= from && *t < until)
        .map(|(m, _)| *m)
        .collect();
    assert!(
        want.iter().any(|m| *m == dated[dated.len() / 4].0),
        "from is included"
    );
    assert!(
        !want.contains(&dated[dated.len() / 2].0),
        "until is excluded"
    );
    snapshot(&r, &lib, &id, |s| {
        let chat = ChatId::new(chat);
        let range = TimeRange::Dated {
            from_ns: from,
            until_ns: until,
        };
        let (items, folded, _) = drain(
            &mut s
                .timeline(chat, range)
                .with_limits(BatchLimits::new(7, u64::MAX)),
        )?;
        let mut got: Vec<i64> = items
            .iter()
            .copied()
            .chain(folded.iter().map(|f| f.0))
            .collect();
        got.sort_unstable();
        let mut sorted = want.clone();
        sorted.sort_unstable();
        assert_eq!(got, sorted);
        assert!(items.iter().all(|m| !undated.contains(m)));
        // The whole dated range: every dated row, no undated one.
        let all_dated = TimeRange::Dated {
            from_ns: i64::MIN,
            until_ns: i64::MAX,
        };
        let (items, folded, _) = drain(&mut s.timeline(chat, all_dated))?;
        assert_eq!(items.len() + folded.len(), dated.len());
        // `All`: the undated rows come last.
        let (items, _, _) = drain(&mut s.timeline(chat, TimeRange::All))?;
        let first_undated = items
            .iter()
            .position(|m| undated.contains(m))
            .expect("an undated item");
        assert!(items[first_undated..].iter().all(|m| undated.contains(m)));
        // An empty range reads nothing.
        let empty = TimeRange::Dated {
            from_ns: from,
            until_ns: from,
        };
        assert!(s.timeline(chat, empty).next_batch()?.is_none());
        Ok(())
    });
}

#[test]
fn messages_in_no_conversation_are_found_within_the_bounds() {
    let (tmp, lib, id) = bulk(800, 40);
    let c = raw(&db_path(&tmp, &id));
    let dated = ints(
        &c,
        "SELECT id FROM message WHERE assoc_present = 0 AND sort_key < X'01' \
         ORDER BY id LIMIT 3",
        &[],
    );
    let undated = ints(
        &c,
        "SELECT id FROM message WHERE sort_key >= X'01' ORDER BY id LIMIT 2",
        &[],
    );
    assert_eq!((dated.len(), undated.len()), (3, 2));
    for m in dated.iter().chain(&undated) {
        c.execute("DELETE FROM chat_message WHERE message_id = ?1", [m])
            .expect("delete");
    }
    let order = ints(
        &c,
        "SELECT id FROM message m WHERE NOT EXISTS \
           (SELECT 1 FROM chat_message WHERE message_id = m.id) ORDER BY sort_key, id",
        &[],
    );
    let mut expected: Vec<i64> = dated.iter().chain(&undated).copied().collect();
    expected.sort_unstable();
    let mut sorted = order.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, expected);
    let t = |m: i64| one(&c, "SELECT date_utc_ns FROM message WHERE id = ?1", &[&m]);
    let mut times: Vec<i64> = dated.iter().map(|m| t(*m)).collect();
    times.sort_unstable();
    let r = lib.open_read(&id).expect("reader");
    snapshot(&r, &lib, &id, |s| {
        let (items, folded, _) = drain(
            &mut s
                .unassigned(TimeRange::All)
                .with_limits(BatchLimits::new(2, u64::MAX)),
        )?;
        assert!(folded.is_empty(), "nothing folds outside a conversation");
        assert_eq!(items, order, "dated first, then undated, in key order");
        let range = TimeRange::Dated {
            from_ns: times[0],
            until_ns: times[2],
        };
        let (items, _, _) = drain(&mut s.unassigned(range))?;
        let want: Vec<i64> = order
            .iter()
            .copied()
            .filter(|m| dated.contains(m) && t(*m) >= times[0] && t(*m) < times[2])
            .collect();
        assert_eq!(items, want);
        assert!(items.iter().all(|m| !undated.contains(m)));
        Ok(())
    });
}

#[test]
fn a_message_in_two_conversations_is_in_both_timelines_and_lists_both() {
    let (tmp, lib, id) = bulk(400, 0);
    let c = raw(&db_path(&tmp, &id));
    let m = one(
        &c,
        "SELECT message_id FROM chat_message cm JOIN message m ON m.id = cm.message_id \
         WHERE m.assoc_present = 0 ORDER BY message_id LIMIT 1",
        &[],
    );
    let home = one(
        &c,
        "SELECT chat_id FROM chat_message WHERE message_id = ?1",
        &[&m],
    );
    let other = one(&c, "SELECT min(id) FROM chat WHERE id <> ?1", &[&home]);
    let cols = columns(&c, "chat_message");
    let select: Vec<&str> = cols
        .iter()
        .map(|col| match col.as_str() {
            "chat_id" => "?1",
            "join_date_raw" => "12345",
            "join_date_st" => "NULL",
            "join_date_unit" => "9",
            "join_date_utc_ns" => "NULL",
            other => other,
        })
        .collect();
    c.execute(
        &format!(
            "INSERT INTO chat_message ({}) SELECT {} FROM chat_message WHERE message_id = ?2",
            cols.join(", "),
            select.join(", ")
        ),
        [other, m],
    )
    .expect("second membership");
    let r = lib.open_read(&id).expect("reader");
    assert!(check_timelines(&r, &lib, &id, &db_path(&tmp, &id)) > 0);
    snapshot(&r, &lib, &id, |s| {
        for chat in [home, other] {
            let (items, _, _) = drain(&mut s.timeline(ChatId::new(chat), TimeRange::All))?;
            assert!(items.contains(&m), "in conversation {chat}");
        }
        let x = s.assemble(&[MessageId::new(m)], Reveal::default(), ExportDepth::Items)?;
        let chats: Vec<i64> = x[0].chats.iter().map(|c| c.chat.get()).collect();
        let mut want = vec![home, other];
        want.sort_unstable();
        assert_eq!(chats, want);
        let joined = x[0]
            .chats
            .iter()
            .find(|c| c.chat.get() == other)
            .expect("other");
        assert_eq!(joined.join_date.raw, Some(12345));
        assert_eq!(joined.join_date.unit, Some(Err(9)));
        Ok(())
    });
}

#[test]
fn batches_are_cut_by_count_and_by_measured_bytes() {
    let (tmp, lib, id) = bulk(600, 0);
    let c = raw(&db_path(&tmp, &id));
    let chat = one(
        &c,
        "SELECT chat_id FROM chat_message GROUP BY chat_id ORDER BY count(*) DESC LIMIT 1",
        &[],
    );
    // A message of 3 MiB in the middle of the conversation.
    let big = ints(
        &c,
        "SELECT cm.message_id FROM chat_message cm JOIN message m ON m.id = cm.message_id \
         WHERE cm.chat_id = ?1 AND m.assoc_present = 0 AND m.current_body_id IS NOT NULL \
         ORDER BY cm.sort_key, cm.message_id",
        &[&chat],
    );
    let big = big[big.len() / 2];
    let text = "x".repeat(3 << 20);
    c.execute(
        "UPDATE message_body SET display_text = ?1, display_source = 1, \
           display_none_reason = NULL \
         WHERE id = (SELECT current_body_id FROM message WHERE id = ?2)",
        rusqlite::params![text, big],
    )
    .expect("big body");
    let r = lib.open_read(&id).expect("reader");
    snapshot(&r, &lib, &id, |s| {
        let chat = ChatId::new(chat);
        let sizes = s.measure(&[MessageId::new(big)], ExportDepth::Items)?;
        assert!(sizes[0] >= 3 << 20, "{}", sizes[0]);
        let deep = s.measure(&[MessageId::new(big)], ExportDepth::WithHistory)?;
        assert!(deep[0] >= sizes[0]);
        let (whole, _, _) = drain(&mut s.timeline(chat, TimeRange::All))?;
        let limits = BatchLimits::new(40, 1 << 20);
        let mut cur = s.timeline(chat, TimeRange::All).with_limits(limits);
        let mut items = Vec::new();
        let mut alone = false;
        while let Some(b) = cur.next_batch()? {
            assert!(b.items.len() <= 40);
            if b.items.iter().any(|m| m.get() == big) {
                assert_eq!(b.items.len(), 1, "a message over the budget goes alone");
                assert!(b.measured_bytes >= 3 << 20);
                alone = true;
            } else {
                assert!(b.measured_bytes <= 1 << 20);
            }
            items.extend(b.items.iter().map(|m| m.get()));
        }
        assert!(alone);
        assert_eq!(items, whole, "cutting changes no row");
        // The id-list cursor cuts the same way.
        let ids: Vec<MessageId> = whole.iter().map(|m| MessageId::new(*m)).collect();
        let mut cur = s.by_ids(&ids).with_limits(limits);
        let mut again = Vec::new();
        while let Some(b) = cur.next_batch()? {
            if b.items.iter().any(|m| m.get() == big) {
                assert_eq!(b.items.len(), 1);
            }
            again.extend(b.items.iter().map(|m| m.get()));
        }
        assert_eq!(again, whole);
        cur.rewind();
        assert!(cur.next_batch()?.is_some());
        Ok(())
    });
}

#[test]
fn statements_are_reset_before_a_cursor_returns_and_a_cancel_stops_assemble() {
    let (_tmp, lib, id) = bulk(1500, 20);
    let r = lib.open_read(&id).expect("reader");
    let hold = lib.hold_for_export(&id).expect("hold");
    let token = CancelToken::new();
    let out = r.with_export_snapshot(&hold, &token, |s| -> Result<(), ArchiveError> {
        let chat = s.chat_ids()?[0];
        let mut cur = s
            .timeline(chat, TimeRange::All)
            .with_limits(BatchLimits::new(5, u64::MAX));
        let first = cur.next_batch()?.expect("a batch");
        assert!(!statements_stepping(s), "the batch statement was reset");
        let msgs = s.assemble(&first.items, Reveal::default(), ExportDepth::WithHistory)?;
        assert!(!statements_stepping(s));
        let mut h = s.history(&msgs[0], Reveal::default())?;
        while h.next_version()?.is_some() {}
        assert!(!statements_stepping(s));
        // The cursor goes on after the nested reads.
        let second = cur.next_batch()?.expect("another batch");
        assert!(first.items.iter().all(|m| !second.items.contains(m)));
        // A cancel stops the next read, with the cursor still live.
        token.cancel();
        assert!(matches!(
            s.assemble(&second.items, Reveal::default(), ExportDepth::Items),
            Err(ArchiveError::Cancelled)
        ));
        assert!(matches!(cur.next_batch(), Err(ArchiveError::Cancelled)));
        Err(ArchiveError::Cancelled)
    });
    assert!(matches!(out, Err(ArchiveError::Cancelled)));
    r.info().expect("the transaction ended");

    // A cancel from another thread lands inside `assemble`.
    let token = CancelToken::new();
    let later = token.clone();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(30));
        later.cancel();
    });
    let out = r.with_export_snapshot(&hold, &token, |s| -> Result<(), ArchiveError> {
        let chat = s.chat_ids()?[0];
        let mut cur = s.timeline(chat, TimeRange::All);
        let live = cur.next_batch()?.expect("a batch");
        let ids: Vec<MessageId> = s
            .chat_ids()?
            .into_iter()
            .flat_map(|c| {
                let mut cur = s.timeline(c, TimeRange::All);
                let mut v = Vec::new();
                while let Ok(Some(b)) = cur.next_batch() {
                    v.extend(b.items);
                }
                v
            })
            .collect();
        for _ in 0..200 {
            s.assemble(&ids, Reveal::default(), ExportDepth::WithHistory)?;
        }
        drop(live);
        Ok(())
    });
    canceller.join().expect("join");
    assert!(matches!(out, Err(ArchiveError::Cancelled)), "{out:?}");
    r.info().expect("the transaction ended");
}

// ---------------------------------------------------------------------------
// Uncapped rows and ledger columns
// ---------------------------------------------------------------------------

/// The columns of `table`, in order.
fn columns(c: &rusqlite::Connection, table: &str) -> Vec<String> {
    c.prepare(&format!("PRAGMA table_info({table})"))
        .expect("pragma")
        .query_map([], |r| r.get::<_, String>(1))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("columns")
}

/// `n` copies of the `table` row `WHERE id = src`, with `overrides`
/// (expressions over the counter `i`, 1..=n).
fn copy_rows(c: &rusqlite::Connection, table: &str, src: i64, n: u32, overrides: &[(&str, &str)]) {
    let cols = columns(c, table);
    let select: Vec<String> = cols
        .iter()
        .map(|col| {
            overrides
                .iter()
                .find(|(k, _)| k == col)
                .map_or_else(|| col.clone(), |(_, v)| (*v).to_owned())
        })
        .collect();
    let sql = format!(
        "WITH RECURSIVE k(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM k WHERE i < {n}) \
         INSERT INTO {table} ({}) SELECT {} FROM {table}, k WHERE id = {src}",
        cols.join(", "),
        select.join(", ")
    );
    c.execute_batch(&sql).expect("copy rows");
}

#[test]
fn reactions_versions_and_edit_entries_are_uncapped() {
    let (tmp, lib, id) = bulk(200, 0);
    let c = raw(&db_path(&tmp, &id));
    let reaction = one(
        &c,
        "SELECT id FROM message WHERE assoc_present = 1 AND assoc_kind BETWEEN 10 AND 17 \
         ORDER BY id LIMIT 1",
        &[],
    );
    let target = target_of(&c, reaction);
    let max = one(&c, "SELECT max(id) FROM message", &[]);
    copy_rows(
        &c,
        "message",
        reaction,
        10_050,
        &[("id", &format!("{max} + i")), ("guid", "randomblob(16)")],
    );
    let rows_on_target = one(
        &c,
        "SELECT count(*) FROM message WHERE assoc_present = 1 AND assoc_target_guid = \
           (SELECT guid FROM message WHERE id = ?1)",
        &[&target],
    );
    assert!(rows_on_target > 10_050);
    let body = one(
        &c,
        "SELECT current_body_id FROM message WHERE id = ?1",
        &[&target],
    );
    let max_body = one(&c, "SELECT max(id) FROM message_body", &[]);
    copy_rows(
        &c,
        "message_body",
        body,
        60,
        &[
            ("id", &format!("{max_body} + i")),
            ("raw_hash", "randomblob(32)"),
            ("display_text", "'version ' || i"),
            ("display_source", "1"),
            ("display_none_reason", "NULL"),
        ],
    );
    c.execute(
        "WITH RECURSIVE k(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM k WHERE i < 250) \
         INSERT INTO message_edit_entry (body_id, part, ordinal, at_kind, at_raw, at_utc_ns, \
           body_present) SELECT ?1, 0, i, NULL, i, NULL, 0 FROM k",
        [max_body + 1],
    )
    .expect("edit entries");
    let r = lib.open_read(&id).expect("reader");
    // The viewer caps all three.
    let h = r
        .reaction_history(MessageId::new(target), None)
        .expect("history");
    assert!(h.total.capped);
    let d = r
        .message_detail(MessageId::new(target), Reveal::default())
        .expect("detail");
    assert_eq!((d.versions.len(), d.more_versions), (50, 11));
    snapshot(&r, &lib, &id, |s| {
        let x = s.assemble(
            &[MessageId::new(target)],
            Reveal::default(),
            ExportDepth::WithHistory,
        )?;
        let x = &x[0];
        assert_eq!(x.reaction_rows.len() as i64, rows_on_target);
        assert!(x.reaction_rows.windows(2).all(|w| w[0].id != w[1].id));
        assert_eq!(
            x.history,
            HistoryState::Available {
                captured_versions: 61,
                phone_entries: 250
            }
        );
        let mut h = s.history(x, Reveal::default())?;
        assert_eq!(h.len(), 61);
        let mut seen = 0;
        let mut entries = 0;
        while let Some(v) = h.next_version()? {
            seen += 1;
            assert_eq!(v.more_edits, 0);
            entries += v.edits.len();
        }
        assert_eq!((seen, entries), (61, 250));
        let shallow = s.assemble(
            &[MessageId::new(target)],
            Reveal::default(),
            ExportDepth::Items,
        )?;
        assert_eq!(
            shallow[0].history,
            HistoryState::NotIncluded {
                captured_versions: 61,
                phone_entries: 250
            }
        );
        Ok(())
    });
}

fn long(c: char, n: usize) -> String {
    std::iter::repeat_n(c, n).collect()
}

fn whole(o: &OptContent, want: &str) {
    let t = o.text().expect("a value");
    assert!(!t.truncated);
    assert_eq!(t.text.expose(), want);
}

#[test]
fn the_ledger_columns_are_read_raw_and_uncapped() {
    let (tmp, lib, id) = bulk(200, 0);
    let c = raw(&db_path(&tmp, &id));
    let (m, att): (i64, i64) = c
        .query_row(
            "SELECT ma.message_id, ma.attachment_id FROM message_attachment ma \
             JOIN message m ON m.id = ma.message_id WHERE m.handle_ref IS NOT NULL \
             ORDER BY ma.message_id LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("a message with an attachment");
    let handle = one(&c, "SELECT handle_ref FROM message WHERE id = ?1", &[&m]);
    let chat = one(
        &c,
        "SELECT chat_id FROM chat_message WHERE message_id = ?1",
        &[&m],
    );
    let (subject, account, name, address) = (
        long('s', 600),
        long('a', 300),
        long('n', 300),
        long('d', 300),
    );
    c.execute(
        "UPDATE message SET subject = CAST(?1 AS BLOB), subject_st = NULL, \
           destination_caller_id = CAST(?2 AS BLOB), destination_caller_id_st = NULL, \
           date_edited_raw = 123, date_edited_st = NULL, date_edited_unit = 9, \
           date_edited_utc_ns = NULL, date_retracted_raw = NULL, date_retracted_st = 2, \
           date_retracted_unit = NULL, date_retracted_utc_ns = NULL WHERE id = ?3",
        rusqlite::params![subject, account, m],
    )
    .expect("message");
    c.execute(
        "INSERT INTO message_recently_deleted (session_id, src_rowid, message_id, chat_ref, \
           chat_rowid, chat_rowid_st, delete_raw, delete_st, delete_unit, delete_utc_ns, \
           ck_sync_state, ck_sync_state_st) \
         VALUES (1, 5, ?1, ?2, NULL, 1, 777, NULL, 9, NULL, 3, NULL)",
        [m, chat],
    )
    .expect("recently deleted");
    c.execute(
        "UPDATE attachment SET is_outgoing = 1, is_outgoing_st = NULL, transfer_state = 5, \
           transfer_state_st = NULL, created_raw = 10, created_st = NULL, created_unit = 9, \
           created_utc_ns = NULL, start_raw = NULL, start_st = 2, start_unit = NULL, \
           start_utc_ns = NULL WHERE id = ?1",
        [att],
    )
    .expect("attachment");
    c.execute(
        "UPDATE chat SET display_name = CAST(?1 AS BLOB), display_name_st = NULL, style = 43, \
           style_st = NULL, is_filtered = NULL, is_filtered_st = 2 WHERE id = ?2",
        rusqlite::params![name, chat],
    )
    .expect("chat");
    c.execute(
        "UPDATE handle SET address = CAST(?1 AS BLOB), address_st = NULL, \
           country = CAST('zz' AS BLOB), country_st = NULL WHERE id = ?2",
        rusqlite::params![address, handle],
    )
    .expect("handle");
    let r = lib.open_read(&id).expect("reader");
    snapshot(&r, &lib, &id, |s| {
        let x = s.assemble(&[MessageId::new(m)], Reveal::default(), ExportDepth::Items)?;
        let d = &x[0].detail;
        whole(&d.subject, &subject);
        whole(&d.account, &account);
        assert_eq!(d.date_edited.raw, Some(123));
        assert_eq!(d.date_edited.unit, Some(Err(9)));
        assert_eq!(d.date_retracted.raw, None);
        assert_eq!(d.date_retracted.state, Some(Ok(ValueState::Absent)));
        let [rd] = d.recently_deleted.as_slice() else {
            panic!("one listing");
        };
        assert_eq!((rd.session, rd.chat), (1, Some(ChatId::new(chat))));
        assert_eq!((rd.deleted.raw, rd.deleted.unit), (Some(777), Some(Err(9))));
        assert_eq!(rd.ck_sync_state.raw, Some(3));
        assert!(x[0].derivation.derived_by >= 1);
        assert!(x[0].derivation.decoder_version.is_some());
        let atts = s.attachment_facts(&x[0])?;
        let a = atts
            .iter()
            .find(|a| a.facts.id == oma_archive::AttachmentId::new(att))
            .expect("the attachment");
        assert_eq!(a.access, oma_archive::AttachmentAccess::Included);
        assert_eq!(
            (a.is_outgoing.raw, a.transfer_state.raw),
            (Some(1), Some(5))
        );
        assert_eq!((a.created.raw, a.created.unit), (Some(10), Some(Err(9))));
        assert_eq!(a.start.state, Some(Ok(ValueState::Absent)));
        let ch = s.chat_columns(ChatId::new(chat))?;
        whole(&ch.display_name, &name);
        assert_eq!(ch.style.value, Some(43));
        assert_eq!(ch.is_filtered.state, Some(ValueState::Absent));
        assert!(!ch.participants.is_empty());
        let h = s.handle_columns(oma_archive::HandleId::new(handle))?;
        whole(&h.address, &address);
        whole(&h.country, "zz");
        assert!(matches!(
            s.chat_columns(ChatId::new(i64::MAX)),
            Err(ArchiveError::AnchorNotFound)
        ));
        assert!(matches!(
            s.assemble(
                &[MessageId::new(i64::MAX)],
                Reveal::default(),
                ExportDepth::Items
            ),
            Err(ArchiveError::AnchorNotFound)
        ));
        Ok(())
    });
    assert_eq!(TimeUnit::from_code(9), None, "9 stays an unknown unit");
}

#[test]
fn subject_history_of_an_unsent_message_is_withheld_unless_revealed() {
    let (tmp, lib, id) = bulk(200, 0);
    let c = raw(&db_path(&tmp, &id));
    let m = one(
        &c,
        "SELECT id FROM message WHERE assoc_present = 0 AND current_body_id IS NOT NULL \
         ORDER BY id LIMIT 1",
        &[],
    );
    let body = one(
        &c,
        "SELECT current_body_id FROM message WHERE id = ?1",
        &[&m],
    );
    c.execute_batch(&format!(
        "INSERT INTO value_history (entity, row_id, col, value_hash, value, first_rank, \
           first_session_id) SELECT 3, id, 318, zeroblob(32), CAST('older subject' AS BLOB), \
           first_rank, 1 FROM message WHERE id = {m};
         INSERT INTO value_history (entity, row_id, col, value_hash, value, first_rank, \
           first_session_id) SELECT 3, id, 321, zeroblob(32), CAST('older title' AS BLOB), \
           first_rank, 1 FROM message WHERE id = {m};"
    ))
    .expect("history");
    let r = lib.open_read(&id).expect("reader");
    let read = |reveal: Reveal| -> ExportMessage {
        snapshot(&r, &lib, &id, |s| {
            Ok(
                s.assemble(&[MessageId::new(m)], reveal, ExportDepth::Items)?
                    .remove(0),
            )
        })
    };
    // Without an unsent part both values are written.
    let x = read(Reveal::default());
    assert_eq!(x.value_history.len(), 2);
    assert!(x
        .value_history
        .iter()
        .all(|v| matches!(v, ValueHistoryEntry::Value(_))));
    // With one, the subject's is withheld unless earlier content is
    // revealed; other columns are not.
    c.execute(
        "INSERT INTO message_unsent_part (body_id, ordinal, part) VALUES (?1, 0, 0)",
        [body],
    )
    .expect("unsent part");
    let x = read(Reveal::default());
    let withheld: Vec<&ValueHistoryEntry> = x
        .value_history
        .iter()
        .filter(|v| matches!(v, ValueHistoryEntry::Withheld { .. }))
        .collect();
    assert_eq!(
        withheld,
        vec![&ValueHistoryEntry::Withheld {
            column: HistoryColumn::Subject,
            reason: WithheldReason::Unsent
        }]
    );
    let x = read(REVEAL_ALL);
    assert!(x.value_history.iter().any(|v| matches!(
        v,
        ValueHistoryEntry::Value(h) if h.column == HistoryColumn::Subject
    )));
}

// ---------------------------------------------------------------------------
// Parity with the viewer catalogue
// ---------------------------------------------------------------------------

fn parity(i: &Imported, what: &str) {
    let r = i.lib.open_read(&i.id).expect("reader");
    let outside = r_outside(&i.lib, &i.id);
    let c = read_only_conn(&i.db());
    let all = ints(&c, "SELECT id FROM message ORDER BY id", &[]);
    for reveal in [Reveal::default(), REVEAL_ALL] {
        let ids: Vec<MessageId> = all.iter().map(|m| MessageId::new(*m)).collect();
        snapshot(&r, &i.lib, &i.id, |s| {
            let xs = s.assemble(&ids, reveal, ExportDepth::WithHistory)?;
            for x in &xs {
                let mid = x.item.message.id;
                let mut want =
                    export_item(&outside, None, mid, reveal, LowerCaps::default()).expect("item");
                // The one difference (slice 6b2, §3.8): an attachment of an
                // unsent part carries no file unless earlier content is
                // revealed.
                assert_eq!(x.attachment_access.len(), want.message.attachments.len());
                for (i, acc) in x.attachment_access.iter().enumerate() {
                    if acc.is_withheld() {
                        assert!(!reveal.earlier_text);
                        want.message.attachments[i].blob = None;
                        want.message.attachments[i].stored_bytes = None;
                        want.codes.attachments[i].stored_bytes = None;
                    }
                }
                assert_eq!(x.item, want, "{what}: item {}", mid.get());
                let d = export_detail(&outside, mid, reveal, LowerCaps::default()).expect("detail");
                assert_eq!(x.detail.subject, d.subject, "{what}");
                assert_eq!(x.detail.account, d.account, "{what}");
                let chats: Vec<ChatId> = x.chats.iter().map(|m| m.chat).collect();
                let detail_chats: Vec<ChatId> = d.chats.iter().map(|f| f.chat).collect();
                assert_eq!(chats, detail_chats, "{what}");
                let unsent = !x.item.message.edit.unsent_parts.is_empty()
                    || x.item.message.edit.unsent_whole;
                let values: Vec<_> = x
                    .value_history
                    .iter()
                    .filter_map(|v| match v {
                        ValueHistoryEntry::Value(h) => Some(h.clone()),
                        _ => None,
                    })
                    .collect();
                if unsent && !reveal.earlier_text {
                    let kept: Vec<_> = d
                        .history
                        .iter()
                        .filter(|h| h.column != HistoryColumn::Subject)
                        .cloned()
                        .collect();
                    assert_eq!(values, kept, "{what}");
                } else {
                    assert_eq!(values, d.history, "{what}");
                }
                let mut h = s.history(x, reveal)?;
                let mut versions = Vec::new();
                while let Some(v) = h.next_version()? {
                    versions.push(v);
                }
                assert_eq!(versions, d.versions, "{what}: versions of {}", mid.get());
                let rh = outside
                    .reaction_history(mid, None)
                    .expect("reaction history");
                if !rh.has_more {
                    let ids: Vec<MessageId> = x.reaction_rows.iter().map(|r| r.id).collect();
                    let want: Vec<MessageId> = rh.rows.iter().map(|r| r.id).collect();
                    assert_eq!(ids, want, "{what}: reaction rows");
                }
            }
            Ok(())
        });
    }
    check_timelines(&r, &i.lib, &i.id, &i.db());
}

/// A second reader, outside the snapshot (the snapshot's own reader
/// refuses nested reads).
fn r_outside(lib: &Library, id: &ArchiveId) -> ArchiveReader {
    lib.open_read(id).expect("reader")
}

#[test]
fn export_reads_match_the_details_over_the_viewer_catalogue() {
    for (what, model) in model_docs() {
        let i: Imported = import_order(&model["orders"][0], &what);
        parity(&i, &what);
    }
}
