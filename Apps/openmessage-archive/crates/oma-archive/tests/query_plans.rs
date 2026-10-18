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
//!
//! The viewer's statements (`hooks::viewer_statements`, M5 spec §3.5) are
//! pinned the same way, and every read of the timeline is checked to stay
//! on `chat_message_timeline` with its dated bound (`sort_key<?`, the
//! literal `X'01'`) where the statement means "dated". Only the
//! conversation list scans `chat` (once per call, by design: v1 has no
//! recency index, §3.10), and only its orders sort in a temporary B-tree.
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
    (
        "viewer value issue",
        &[
            "SEARCH value_issue USING PRIMARY KEY (entity=? AND row_id=? AND col=? AND kind=?)",
        ],
    ),
    (
        "viewer fold row",
        &[
            "SEARCH message USING INTEGER PRIMARY KEY (rowid=?)",
        ],
    ),
    (
        "viewer keyed message by guid",
        &[
            "SEARCH message USING COVERING INDEX message_guid (guid=?)",
        ],
    ),
    (
        "viewer timeline membership",
        &[
            "SEARCH chat_message USING PRIMARY KEY (chat_id=? AND message_id=?)",
        ],
    ),
    (
        "viewer conversation order (recent)",
        &[
            "SCAN c",
            "CORRELATED SCALAR SUBQUERY 1",
            "SEARCH chat_message USING COVERING INDEX chat_message_timeline (chat_id=? AND sort_key<?)",
            "USE TEMP B-TREE FOR ORDER BY",
        ],
    ),
    (
        "viewer conversation order (count)",
        &[
            "SCAN c",
            "SEARCH s USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN",
            "USE TEMP B-TREE FOR ORDER BY",
        ],
    ),
    (
        "viewer chat names",
        &[
            "SCAN chat",
        ],
    ),
    (
        "viewer chat participants (all)",
        &[
            "SCAN ch",
            "SEARCH h USING INTEGER PRIMARY KEY (rowid=?)",
        ],
    ),
    (
        "viewer newest session",
        &[
            "SCAN import_session",
        ],
    ),
    (
        "viewer conversation row",
        &[
            "SEARCH c USING INTEGER PRIMARY KEY (rowid=?)",
            "SEARCH s USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN",
        ],
    ),
    (
        "viewer participants",
        &[
            "SEARCH ch USING PRIMARY KEY (chat_id=?)",
            "SEARCH h USING INTEGER PRIMARY KEY (rowid=?)",
        ],
    ),
    (
        "viewer participant count",
        &[
            "SEARCH chat_handle USING PRIMARY KEY (chat_id=?)",
        ],
    ),
    (
        "viewer newest dated rows",
        &[
            "SEARCH chat_message USING COVERING INDEX chat_message_timeline (chat_id=? AND sort_key<?)",
        ],
    ),
    (
        "viewer first dated from",
        &[
            "SEARCH chat_message USING COVERING INDEX chat_message_timeline (chat_id=? AND sort_key>? AND sort_key<?)",
        ],
    ),
    (
        "viewer count dated before",
        &[
            "CO-ROUTINE (subquery-1)",
            "SEARCH chat_message USING COVERING INDEX chat_message_timeline (chat_id=? AND sort_key<?)",
            "SCAN (subquery-1)",
        ],
    ),
    (
        "viewer count undated",
        &[
            "CO-ROUTINE (subquery-1)",
            "SEARCH chat_message USING COVERING INDEX chat_message_timeline (chat_id=? AND sort_key>?)",
            "SCAN (subquery-1)",
        ],
    ),
    (
        "viewer preview row",
        &[
            "SEARCH m USING INTEGER PRIMARY KEY (rowid=?)",
            "SEARCH b USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN",
        ],
    ),
    (
        "viewer flagged parts",
        &[
            "SEARCH message_body_part USING PRIMARY KEY (body_id=?)",
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
        .chain(oma_archive::hooks::viewer_statements())
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
fn viewer_statements_stay_on_their_indexes() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let c = raw(&db_path(&tmp, &id));
    let list_scans = [
        "viewer conversation order (recent)",
        "viewer conversation order (count)",
        "viewer chat names",
        "viewer chat participants (all)",
    ];
    for (name, sql) in oma_archive::hooks::viewer_statements() {
        let lines = plan(&c, &sql);
        for line in &lines {
            if line.starts_with("SCAN") && !line.starts_with("SCAN (subquery") {
                let allowed = list_scans.contains(&name)
                    || (name, line.as_str()) == ("viewer newest session", "SCAN import_session");
                assert!(allowed, "{name}: {line}");
            }
            if line.contains("TEMP B-TREE") {
                assert!(
                    name.starts_with("viewer conversation order"),
                    "{name}: {line}"
                );
            }
        }
        // "Dated" reads keep their bound to the dated range.
        if sql.contains("chat_message") && sql.contains("X'01'") {
            assert!(
                lines.iter().any(|l| l.contains("chat_message_timeline")),
                "{name}: {lines:?}"
            );
        }
    }
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
