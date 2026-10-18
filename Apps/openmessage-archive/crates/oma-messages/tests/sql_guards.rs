//! The SQL guards as oma-messages uses them (M3 §3.1, §7.4, §7.5, §19.3
//! `sql_guards.rs`): double-quoted strings off, the authorizer's refusals
//! (including `part_text`), triggers and views off, statement sizes under
//! [`SMS_DB_LIMITS`], and a check that trips no guard.
//!
//! Both `OpenMode`s: on Linux a staged copy always opens read-only, so the
//! read-write `query_only` fallback is exercised by oma-backup's unit test
//! `staged_conn::tests::guards_hold_on_a_read_write_connection`, which
//! installs the same authorizer with the same denied column.
#![allow(clippy::disallowed_types, clippy::disallowed_methods)]
mod common;

use std::ops::ControlFlow;

use common::*;
use oma_backup::OpenMode;
use oma_messages::{Cursor, Probe, ProbeOptions, SMS_DB_LIMITS};
use rusqlite::Connection;

const DENIED: [&str; 8] = [
    "PRAGMA query_only=0",
    "PRAGMA user_version=9",
    "ATTACH DATABASE ':memory:' AS other",
    "CREATE TEMP TABLE scratch (a)",
    "INSERT INTO handle (id) VALUES ('x')",
    "DELETE FROM handle",
    "SELECT part_text FROM recoverable_message_part",
    "SELECT count(*) FROM recoverable_message_part WHERE part_text IS NOT NULL",
];

#[test]
fn writes_pragmas_attach_and_part_text_are_refused() {
    let fx = Fixture::with_sms(&build(&full_schema()));
    with_probe(&fx, None, &ProbeOptions::default(), |p, _| {
        let Probe::Supported(db) = p.expect("probe") else {
            panic!("not supported");
        };
        let conn = db.connection();
        assert_eq!(conn.mode(), OpenMode::ReadOnly);
        assert_eq!(conn.authorizer_denials(), 0, "the probe trips no guard");

        // DQS is off: an unknown double-quoted name fails to prepare instead
        // of becoming the string 'nosuch'.
        assert!(conn.prepare_read("SELECT \"nosuch\" FROM handle").is_err());
        assert!(conn
            .prepare_read("SELECT count(*) FROM handle WHERE id = \"nosuch\"")
            .is_err());

        for sql in DENIED {
            assert!(conn.prepare_read(sql).is_err(), "{sql}");
        }
        assert!(
            conn.authorizer_denials() >= 8,
            "{}",
            conn.authorizer_denials()
        );
        // Counting the table is allowed; it names no column.
        let n: i64 = conn
            .prepare_read("SELECT count(*) FROM recoverable_message_part")
            .expect("count")
            .query_row([], |r| r.get(0))
            .expect("row");
        assert_eq!(n, 1);
        assert!(conn.query_only().expect("query_only"));
        assert_eq!(conn.total_changes(), 0);
    })
    .expect("connected");
}

#[test]
fn triggers_and_views_load_but_views_are_unusable() {
    let mut sql = full_schema();
    sql.push_str(
        "CREATE VIEW synthetic_v AS SELECT id FROM handle;
         CREATE TRIGGER synthetic_t AFTER INSERT ON handle BEGIN SELECT 1; END;",
    );
    let fx = Fixture::with_sms(&build(&sql));
    with_probe(&fx, None, &ProbeOptions::default(), |p, _| {
        let Probe::Supported(db) = p.expect("probe") else {
            panic!("not supported");
        };
        let caps = db.capabilities();
        assert_eq!((caps.views, caps.triggers), (1, 1));
        assert!(db
            .connection()
            .prepare_read("SELECT * FROM synthetic_v")
            .is_err());
    })
    .expect("connected");
}

/// A whole probe and every pass of this slice leave the guards untouched.
#[test]
fn the_passes_trip_no_guard() {
    let mut sql = full_schema();
    sql.push_str(
        "INSERT INTO handle (id, service) VALUES ('synthetic-handle-1@example.invalid', 'SMS');
         INSERT INTO chat (guid, properties) VALUES ('SYNTH-CHAT-GUID-1', X'00');
         INSERT INTO chat_handle_join VALUES (1, 1);
         INSERT INTO chat_message_join VALUES (1, 1, 0);
         INSERT INTO message_attachment_join VALUES (1, 1);
         INSERT INTO chat_recoverable_message_join VALUES (1, 1, 0, 0);",
    );
    let fx = Fixture::with_sms(&build(&sql));
    with_probe(&fx, None, &ProbeOptions::default(), |p, ctx| {
        let Probe::Supported(db) = p.expect("probe") else {
            panic!("not supported");
        };
        assert_eq!(
            db.for_each_handle(Cursor::Start, ctx, go::<_>)
                .expect("pass")
                .rows,
            1
        );
        assert_eq!(
            db.for_each_chat(Cursor::Start, ctx, go::<_>)
                .expect("pass")
                .rows,
            1
        );
        assert_eq!(
            db.for_each_chat_handle(Cursor::Start, ctx, go::<_>)
                .expect("pass")
                .rows,
            1
        );
        assert_eq!(
            db.for_each_chat_message(Cursor::Start, ctx, go::<_>)
                .expect("pass")
                .rows,
            1
        );
        assert_eq!(
            db.for_each_message_attachment(Cursor::Start, ctx, go::<_>)
                .expect("pass")
                .rows,
            1
        );
        assert_eq!(
            db.for_each_recoverable(Cursor::Start, ctx, go::<_>)
                .expect("pass")
                .rows,
            1
        );
        let conn = db.connection();
        assert!(conn.query_only().expect("query_only"));
        assert_eq!(conn.authorizer_denials(), 0);
        assert_eq!(conn.total_changes(), 0);
    })
    .expect("connected");
}

/// Every record query fits `sql_length` with a 2× margin, and compiles to
/// fewer than half of `vdbe_op` operations (§7.4).
#[test]
fn record_statements_fit_the_limits() {
    let main = build(&full_schema());
    let fx = Fixture::with_sms(&main);
    let queries: Vec<String> = with_probe(&fx, None, &ProbeOptions::default(), |p, _| {
        let Probe::Supported(db) = p.expect("probe") else {
            panic!("not supported");
        };
        db.record_queries().map(str::to_owned).collect()
    })
    .expect("connected");
    // Two forms for each of message, handle, chat, attachment and the four
    // join tables.
    assert_eq!(queries.len(), 16);
    let [_, sql_length, _, _, vdbe_op, _, _, _] = SMS_DB_LIMITS.values();
    let longest = queries.iter().map(String::len).max().expect("queries");
    assert!(
        u32::try_from(longest).expect("small") * 2 < sql_length,
        "longest statement is {longest} bytes"
    );

    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("sms.db");
    std::fs::write(&path, &main).expect("write");
    let c = Connection::open(&path).expect("open");
    for q in &queries {
        let mut st = c.prepare(&format!("EXPLAIN {q}")).expect("explain");
        let params = vec![0_i64; st.parameter_count()];
        let ops = st
            .query_map(rusqlite::params_from_iter(params), |_| Ok(()))
            .expect("query")
            .count();
        assert!(
            u32::try_from(ops).expect("small") * 2 < vdbe_op,
            "{ops} operations"
        );
    }
}

fn go<T>(_: T) -> ControlFlow<()> {
    ControlFlow::Continue(())
}
