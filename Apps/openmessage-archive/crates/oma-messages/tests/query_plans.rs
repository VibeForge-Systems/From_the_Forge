//! The aggregate statements' query plans (M3 §7.3, §19.3 `query_plans.rs`).
//!
//! On a database with **no** indexes, `EXPLAIN QUERY PLAN` must show no
//! correlated subquery: a correlated `NOT EXISTS` would scan the inner table
//! once per outer row. The plan-safe forms (`NOT IN (SELECT …)`,
//! `GROUP BY … HAVING`) plan as a list subquery or a temporary B-tree.
//! Every statement also fits `sql_length` with a 2× margin and compiles to
//! fewer than half of `vdbe_op` operations (§7.4).
//!
//! The plans are read on a plain connection: they are SQLite's, and the
//! staged connection's authorizer has nothing to add to them.
#![allow(clippy::disallowed_types, clippy::disallowed_methods)]
mod common;

use common::*;
use oma_messages::{aggregate_queries, SMS_DB_LIMITS};
use rusqlite::Connection;

fn plan(c: &Connection, sql: &str) -> Vec<String> {
    let mut st = c
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .expect("explain query plan");
    st.query_map([], |r| r.get::<_, String>(3))
        .expect("query")
        .map(|r| r.expect("row"))
        .collect()
}

fn assert_plan_safe(c: &Connection, what: &str) -> usize {
    let mut planned = 0;
    for sql in aggregate_queries() {
        let rows = plan(c, sql);
        assert!(!rows.is_empty(), "{what}: no plan for {sql}");
        for r in &rows {
            assert!(
                !r.contains("CORRELATED"),
                "{what}: a correlated subquery in {sql}: {rows:?}"
            );
        }
        planned += 1;
    }
    planned
}

#[test]
fn no_aggregate_plans_a_correlated_scan_on_an_index_free_database() {
    let c = Connection::open_in_memory().expect("open");
    c.execute_batch(&full_schema()).expect("schema");
    let indexes: i64 = c
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type = 'index'",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(indexes, 0, "the database must be index-free");
    assert_eq!(assert_plan_safe(&c, "full schema"), 17);

    // The detector works: the draft's correlated form is reported as such.
    let correlated = plan(
        &c,
        "SELECT count(*) FROM message m WHERE NOT EXISTS \
         (SELECT 1 FROM chat_message_join j WHERE j.message_id = m.ROWID)",
    );
    assert!(
        correlated.iter().any(|r| r.contains("CORRELATED")),
        "{correlated:?}"
    );
}

/// The same on the catalogue's `msgs-minimal-plain`, for the statements
/// whose tables it has (the others do not apply to it).
#[test]
fn the_minimal_fixture_plans_safely_too() {
    let main =
        std::fs::read(messages_dir().join("msgs-minimal-plain.expected/sms.db")).expect("fixture");
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("sms.db");
    std::fs::write(&path, main).expect("write");
    let c = Connection::open(&path).expect("open");
    let mut applied = 0;
    for sql in aggregate_queries() {
        if c.prepare(sql).is_err() {
            continue; // a table or column this database does not have
        }
        for r in plan(&c, sql) {
            assert!(!r.contains("CORRELATED"), "{sql}: {r}");
        }
        applied += 1;
    }
    assert!(applied >= 8, "{applied}");
}

#[test]
fn aggregate_statements_fit_the_limits() {
    let [_, sql_length, _, _, vdbe_op, _, _, _] = SMS_DB_LIMITS.values();
    let c = Connection::open_in_memory().expect("open");
    c.execute_batch(&full_schema()).expect("schema");
    for sql in aggregate_queries() {
        assert!(
            u32::try_from(sql.len()).expect("small") * 2 < sql_length,
            "{sql}"
        );
        let mut st = c.prepare(&format!("EXPLAIN {sql}")).expect("explain");
        let ops = st.query_map([], |_| Ok(())).expect("query").count();
        assert!(
            u32::try_from(ops).expect("small") * 2 < vdbe_op,
            "{ops} operations: {sql}"
        );
    }
}
