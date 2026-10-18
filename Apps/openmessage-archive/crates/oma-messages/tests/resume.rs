//! Break and resume for every `for_each_*` pass (M3 §4.4,
//! §19.3 `resume.rs`).
//!
//! The fixture has rowid gaps, deleted rows, and rows keyed `i64::MIN` and
//! `i64::MAX`, in the rowid tables and in the join tables keyed by their
//! implicit `rowid`. Pages are the smallest allowed (16 rows), so breaks and
//! resumes land on, before and after page boundaries.
//!
//! The property test breaks after k rows for pseudo-random k (a fixed-seed
//! xorshift, so failures reproduce), resumes from `Cursor::After(last)`, and
//! asserts the concatenation equals one full pass: no gaps, no duplicates.
#![allow(clippy::disallowed_types, clippy::disallowed_methods)]
mod common;

use std::ops::ControlFlow;

use common::*;
use oma_messages::{
    Cursor, GuidSets, KnownTable, MessageReadOptions, MessagesCtx, PassOutcome, Probe, ProbeOptions,
};
use rusqlite::Connection;

/// The keys inserted into every table, before deletions.
fn inserted_keys() -> Vec<i64> {
    let mut keys = vec![i64::MIN, -3];
    keys.extend((1..=150).filter(|k| k % 7 != 0));
    keys.push(i64::MAX);
    keys
}

/// The keys left after deleting every multiple of 11, in key order.
fn expected_keys() -> Vec<i64> {
    inserted_keys()
        .into_iter()
        .filter(|k| *k == i64::MIN || *k == i64::MAX || k % 11 != 0)
        .collect()
}

/// (table, key column, insert statement taking the key as ?1)
const TABLES: [(&str, &str, &str); 8] = [
    ("message", "ROWID", "INSERT INTO message (ROWID, guid, date, is_from_me, text) VALUES (?1, 'SYNTH-MSG-' || ?1, 700000000000000000, 0, 'SYNTH-BODY-' || ?1)"),
    ("handle", "ROWID", "INSERT INTO handle (ROWID, id, service) VALUES (?1, 'synthetic-handle-' || ?1 || '@example.invalid', 'iMessage')"),
    ("chat", "ROWID", "INSERT INTO chat (ROWID, guid) VALUES (?1, 'SYNTH-CHAT-GUID-' || ?1)"),
    ("chat_handle_join", "rowid", "INSERT INTO chat_handle_join (rowid, chat_id, handle_id) VALUES (?1, 1, ?1)"),
    ("chat_message_join", "rowid", "INSERT INTO chat_message_join (rowid, chat_id, message_id, message_date) VALUES (?1, 1, ?1, 700000000000000000)"),
    ("message_attachment_join", "rowid", "INSERT INTO message_attachment_join (rowid, message_id, attachment_id) VALUES (?1, ?1, 1)"),
    ("attachment", "ROWID", "INSERT INTO attachment (ROWID, guid, filename) VALUES (?1, 'SYNTH-ATT-GUID-' || ?1, '~/Library/SMS/Attachments/aa/' || ?1 || '/SYNTH-FILE.heic')"),
    ("chat_recoverable_message_join", "rowid", "INSERT INTO chat_recoverable_message_join (rowid, chat_id, message_id, delete_date, ck_sync_state) VALUES (?1, 1, ?1, 0, 0)"),
];

fn fixture() -> Vec<u8> {
    build_with(|c: &Connection| {
        c.execute_batch(&full_schema()).expect("schema");
        for (table, key, insert) in TABLES {
            let mut st = c.prepare(insert).expect("prepare");
            for k in inserted_keys() {
                st.execute([k]).expect("insert");
            }
            c.execute(
                &format!("DELETE FROM {table} WHERE {key} % 11 = 0 AND {key} NOT IN (?1, ?2)"),
                [i64::MIN, i64::MAX],
            )
            .expect("delete");
        }
    })
}

/// A tiny xorshift64; deterministic, so failures reproduce.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        usize::try_from(self.0 % u64::try_from(n).expect("small")).expect("small")
    }
}

type Callback<'f> = &'f mut dyn FnMut(i64) -> ControlFlow<u64>;

/// Collect keys from `from` to the end, or until the callback breaks after
/// `stop_after` rows.
fn collect(
    run: &mut dyn FnMut(Cursor, Callback<'_>) -> PassOutcome<u64>,
    from: Cursor,
    stop_after: Option<u64>,
) -> (Vec<i64>, PassOutcome<u64>) {
    let mut keys = Vec::new();
    let out = run(from, &mut |k| {
        keys.push(k);
        let n = u64::try_from(keys.len()).expect("small");
        if Some(n) == stop_after {
            ControlFlow::Break(n)
        } else {
            ControlFlow::Continue(())
        }
    });
    (keys, out)
}

fn check_resume(
    name: &str,
    seed: u64,
    count: Option<u64>,
    run: &mut dyn FnMut(Cursor, Callback<'_>) -> PassOutcome<u64>,
) {
    let expected = expected_keys();
    let (all, out) = collect(run, Cursor::Start, None);
    assert_eq!(all, expected, "{name}: full pass");
    assert_eq!(out.rows, u64::try_from(all.len()).expect("small"), "{name}");
    assert_eq!(out.last, Some(i64::MAX), "{name}");
    assert_eq!(out.broke, None, "{name}");
    assert!(
        out.value_issues.is_empty(),
        "{name}: {:?}",
        out.value_issues
    );
    assert_eq!(count, Some(out.rows), "{name}: rows streamed == count(*)");

    // Break after k rows, then resume once.
    let n = all.len();
    let mut rng = Rng(seed);
    let mut ks: Vec<usize> = vec![1, 2, 15, 16, 17, 32, n - 1, n];
    ks.extend((0..24).map(|_| 1 + rng.below(n)));
    for k in ks {
        let k64 = u64::try_from(k).expect("small");
        let (head, out) = collect(run, Cursor::Start, Some(k64));
        assert_eq!(head, all[..k], "{name}: head k={k}");
        assert_eq!(out.broke, Some(k64), "{name}: broke k={k}");
        assert_eq!(out.rows, k64, "{name}");
        let last = out.last.expect("a row was delivered");
        assert_eq!(last, all[k - 1], "{name}");
        let (tail, out) = collect(run, Cursor::After(last), None);
        assert_eq!(tail, all[k..], "{name}: tail k={k}");
        assert_eq!(out.broke, None);
        assert_eq!(out.last, all[k..].last().copied().or(None), "{name}");
    }

    // A chain of random breaks covers the table exactly once.
    let mut joined = Vec::new();
    let mut cursor = Cursor::Start;
    loop {
        let step = u64::try_from(1 + rng.below(40)).expect("small");
        let (keys, out) = collect(run, cursor, Some(step));
        joined.extend_from_slice(&keys);
        match out.last {
            Some(last) if out.broke.is_some() => cursor = Cursor::After(last),
            _ => break,
        }
    }
    assert_eq!(joined, all, "{name}: chained resumes");
}

#[test]
fn every_pass_breaks_and_resumes_exactly() {
    let fx = Fixture::with_sms(&fixture());
    let opts = ProbeOptions::default().with_page_rows(16);
    with_probe(&fx, None, &opts, |p, ctx: &mut MessagesCtx<'_>| {
        let Probe::Supported(db) = p.expect("probe") else {
            panic!("not supported");
        };
        let rows = |t: KnownTable| db.capabilities().table(t).and_then(|c| c.rows);
        let opts = MessageReadOptions::default();
        let mut guids = GuidSets::new();
        check_resume("message", 7, rows(KnownTable::Message), &mut |from, f| {
            db.for_each_message(from, &opts, Some(&mut guids), ctx, |r| f(r.rowid))
                .expect("pass")
        });
        check_resume("handle", 1, rows(KnownTable::Handle), &mut |from, f| {
            db.for_each_handle(from, ctx, |r| f(r.rowid)).expect("pass")
        });
        check_resume("chat", 2, rows(KnownTable::Chat), &mut |from, f| {
            db.for_each_chat(from, ctx, |r| f(r.rowid)).expect("pass")
        });
        check_resume(
            "chat_handle_join",
            3,
            rows(KnownTable::ChatHandleJoin),
            &mut |from, f| {
                db.for_each_chat_handle(from, ctx, |r| f(r.rowid))
                    .expect("pass")
            },
        );
        check_resume(
            "chat_message_join",
            4,
            rows(KnownTable::ChatMessageJoin),
            &mut |from, f| {
                db.for_each_chat_message(from, ctx, |r| f(r.rowid))
                    .expect("pass")
            },
        );
        check_resume(
            "message_attachment_join",
            5,
            rows(KnownTable::MessageAttachmentJoin),
            &mut |from, f| {
                db.for_each_message_attachment(from, ctx, |r| f(r.rowid))
                    .expect("pass")
            },
        );
        check_resume(
            "attachment",
            8,
            rows(KnownTable::Attachment),
            &mut |from, f| {
                db.for_each_attachment(from, None, ctx, |r| f(r.rowid))
                    .expect("pass")
            },
        );
        check_resume(
            "chat_recoverable_message_join",
            6,
            rows(KnownTable::ChatRecoverableMessageJoin),
            &mut |from, f| {
                db.for_each_recoverable(from, ctx, |r| f(r.rowid))
                    .expect("pass")
            },
        );
    })
    .expect("connected");
}

/// The link records carry the join columns; the key is the implicit rowid,
/// not a column.
#[test]
fn link_records_carry_their_columns() {
    let fx = Fixture::with_sms(&fixture());
    with_probe(&fx, None, &ProbeOptions::default(), |p, ctx| {
        let Probe::Supported(db) = p.expect("probe") else {
            panic!("not supported");
        };
        let mut first = None;
        db.for_each_chat_message(Cursor::After(-3), ctx, |l| {
            first = Some((
                l.rowid,
                l.chat_id.value().copied(),
                l.message_id.value().copied(),
                l.message_date.value().map(|d| d.raw),
            ));
            ControlFlow::Break(())
        })
        .expect("pass");
        assert_eq!(
            first,
            Some((1, Some(1), Some(1), Some(700_000_000_000_000_000)))
        );
        let mut rec = None;
        db.for_each_recoverable(Cursor::Start, ctx, |l| {
            rec = Some(format!("{l:?}"));
            ControlFlow::Break(())
        })
        .expect("pass");
        let rec = rec.expect("a row");
        assert!(
            rec.starts_with("RecoverableLink { rowid: -9223372036854775808, chat_id: Value"),
            "{rec}"
        );
    })
    .expect("connected");
}

/// Optional link tables that are absent give empty passes, not errors.
#[test]
fn absent_optional_tables_give_empty_passes() {
    let fx = Fixture::with_sms(&build(&minimal_schema()));
    with_probe(&fx, None, &ProbeOptions::default(), |p, ctx| {
        let Probe::Supported(db) = p.expect("probe") else {
            panic!("not supported");
        };
        let out = db
            .for_each_message_attachment::<()>(Cursor::Start, ctx, |_| ControlFlow::Continue(()))
            .expect("pass");
        assert_eq!((out.rows, out.last), (0, None));
        let out = db
            .for_each_recoverable::<()>(Cursor::Start, ctx, |_| ControlFlow::Continue(()))
            .expect("pass");
        assert_eq!((out.rows, out.last), (0, None));
    })
    .expect("connected");
}
