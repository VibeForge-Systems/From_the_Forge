//! `EXPLAIN QUERY PLAN` pins (spec §18.2, C3.22).
//!
//! Every index-dependent statement of the import, taken from the crate
//! exactly as it prepares it (`hooks::plan_statements`), and the read
//! shapes M5 is promised by the ordering contract (§6.5: the timeline
//! keyset, FTS to the current body, a message's attachments in order),
//! each with its plan pinned on the bundled SQLite. A plan that changes
//! (a schema edit, or a rusqlite or libsqlite3-sys upgrade) fails here and
//! is re-pinned deliberately.
//!
//! Beyond the exact text: no import statement scans a table except the
//! finalize statistics, which scan `chat` once by design, and none builds
//! a temporary B-tree.
#![allow(clippy::disallowed_methods)]

mod common;

use common::*;

/// The M5 read shapes of the §6.5 contract.
const CONTRACT: [(&str, &str); 3] = [
    (
        "timeline keyset",
        "SELECT message_id, sort_key FROM chat_message \
         WHERE chat_id = ?1 AND (sort_key, message_id) > (?2, ?3) \
         ORDER BY sort_key, message_id LIMIT ?4",
    ),
    (
        "FTS to the current body",
        "SELECT m.id FROM message_fts f \
         JOIN message_body b ON b.id = f.rowid \
         JOIN message m ON m.id = b.message_id AND m.current_body_id = b.id \
         WHERE message_fts MATCH ?1",
    ),
    (
        "attachments of a message",
        "SELECT a.id FROM message_attachment ma JOIN attachment a ON a.id = ma.attachment_id \
         WHERE ma.message_id = ?1 \
         ORDER BY ma.ord_rank DESC, ma.ord_rowid ASC, a.guid",
    ),
];

/// The pinned plans: each statement's `detail` lines in order.
const PINNED: &[(&str, &[&str])] = &[
    (
        "handle by natural key",
        &["SEARCH handle USING INDEX handle_natural (natural_key=?)"],
    ),
    (
        "handle unkeyed",
        &["SEARCH handle USING INDEX handle_unkeyed (unkeyed_fp=? AND unkeyed_occ=?)"],
    ),
    (
        "chat by guid",
        &["SEARCH chat USING INDEX chat_guid (guid=?)"],
    ),
    (
        "chat unkeyed",
        &["SEARCH chat USING INDEX chat_unkeyed (unkeyed_fp=? AND unkeyed_occ=?)"],
    ),
    (
        "message by guid",
        &["SEARCH message USING INDEX message_guid (guid=?)"],
    ),
    (
        "message unkeyed",
        &["SEARCH message USING INDEX message_unkeyed (unkeyed_fp=? AND unkeyed_occ=?)"],
    ),
    (
        "attachment by guid",
        &["SEARCH attachment USING INDEX attachment_guid (guid=?)"],
    ),
    (
        "attachment unkeyed",
        &["SEARCH attachment USING INDEX attachment_unkeyed (unkeyed_fp=? AND unkeyed_occ=?)"],
    ),
    (
        "value issues of a row",
        &["SEARCH value_issue USING PRIMARY KEY (entity=? AND row_id=?)"],
    ),
    (
        "body version by raw hash",
        &["SEARCH message_body USING INDEX sqlite_autoindex_message_body_1 (message_id=? AND raw_hash=?)"],
    ),
    (
        "overlap lookup",
        &["SEARCH message USING COVERING INDEX message_guid (guid=?)"],
    ),
    (
        "current blob by guid",
        &[
            "SEARCH a USING INDEX attachment_guid (guid=?)",
            "SEARCH b USING INTEGER PRIMARY KEY (rowid=?)",
        ],
    ),
    (
        "map resolution",
        &["SEARCH import_rowid_map USING PRIMARY KEY (session_id=? AND src=? AND src_rowid=?)"],
    ),
    (
        "occurrence counter",
        &["SEARCH import_occurrence USING PRIMARY KEY (session_id=? AND entity=? AND fp=?)"],
    ),
    (
        "finalize chat_stats",
        &[
            "SCAN c",
            "SEARCH cm USING COVERING INDEX chat_message_timeline (chat_id=?) LEFT-JOIN",
            "SEARCH m USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN",
        ],
    ),
    (
        "timeline keyset",
        &["SEARCH chat_message USING COVERING INDEX chat_message_timeline (chat_id=? AND (sort_key,message_id)>(?,?))"],
    ),
    (
        "FTS to the current body",
        &[
            "SCAN f VIRTUAL TABLE INDEX 0:M1",
            "SEARCH b USING INTEGER PRIMARY KEY (rowid=?)",
            "SEARCH m USING INTEGER PRIMARY KEY (rowid=?)",
        ],
    ),
    (
        "attachments of a message",
        &[
            "SEARCH ma USING PRIMARY KEY (message_id=?)",
            "SEARCH a USING INTEGER PRIMARY KEY (rowid=?)",
            "USE TEMP B-TREE FOR ORDER BY",
        ],
    ),
];

fn plan(c: &rusqlite::Connection, sql: &str) -> Vec<String> {
    let mut st = c
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .expect("explain");
    let nulls = vec![rusqlite::types::Null; st.parameter_count()];
    st.query_map(rusqlite::params_from_iter(nulls), |r| r.get::<_, String>(3))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows")
}

#[test]
fn every_index_dependent_statement_has_its_pinned_plan() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let c = raw(&db_path(&tmp, &id));
    let statements: Vec<(&str, String)> = oma_archive::hooks::plan_statements()
        .into_iter()
        .chain(CONTRACT.iter().map(|(n, s)| (*n, (*s).to_owned())))
        .collect();
    assert_eq!(statements.len(), PINNED.len());
    let mut mismatches = Vec::new();
    for (name, sql) in &statements {
        let got = plan(&c, sql);
        let want = PINNED
            .iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("{name} is not pinned"))
            .1;
        if got != want {
            mismatches.push(format!("{name}: {got:?}"));
        }
    }
    assert!(
        mismatches.is_empty(),
        "plans changed:\n{}",
        mismatches.join("\n")
    );
}

#[test]
fn import_statements_never_scan_or_sort() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let c = raw(&db_path(&tmp, &id));
    for (name, sql) in oma_archive::hooks::plan_statements() {
        for line in plan(&c, &sql) {
            assert!(!line.contains("TEMP B-TREE"), "{name}: {line}");
            if line.starts_with("SCAN") {
                assert_eq!((name, line.as_str()), ("finalize chat_stats", "SCAN c"));
            }
        }
    }
}
