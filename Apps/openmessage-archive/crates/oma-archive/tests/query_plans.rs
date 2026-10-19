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
//! recency index, §3.10) and only it and the per-call session read scan a
//! table; only its orders and a message's attachments (at most one page's
//! worth, ordered by their M4 order key) sort in a temporary B-tree. The
//! detail's unkeyed identifier probe walks the partial index of unkeyed
//! messages, under its own budget.
//!
//! Search (M5 §4, slice 5c): FTS5 is reached only through its `MATCH`
//! index (the global collection newest rowid first with no sort, the
//! per-body probe and `highlight()` with `rowid=`); the exact-text scan
//! and the scoped plan walk `message_sort` and `chat_message_timeline` by
//! keyset; only the once-per-session unsearchable count scans `message`.
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
    (
        "viewer timeline key",
        &[
            "SEARCH chat_message USING PRIMARY KEY (chat_id=? AND message_id=?)",
        ],
    ),
    (
        "viewer thread dated older",
        &[
            "SEARCH chat_message USING COVERING INDEX chat_message_timeline (chat_id=? AND (sort_key,message_id)<(?,?))",
        ],
    ),
    (
        "viewer thread dated newer",
        &[
            "SEARCH chat_message USING COVERING INDEX chat_message_timeline (chat_id=? AND (sort_key,message_id)>(?,?) AND sort_key<?)",
        ],
    ),
    (
        "viewer thread undated older",
        &[
            "SEARCH chat_message USING COVERING INDEX chat_message_timeline (chat_id=? AND sort_key>? AND (sort_key,message_id)<(?,?))",
        ],
    ),
    (
        "viewer thread undated newer",
        &[
            "SEARCH chat_message USING COVERING INDEX chat_message_timeline (chat_id=? AND (sort_key,message_id)>(?,?))",
        ],
    ),
    (
        "viewer reply originator",
        &[
            "SEARCH message USING INTEGER PRIMARY KEY (rowid=?)",
        ],
    ),
    (
        "viewer replies",
        &[
            "SEARCH message USING INDEX message_reply (thread_originator_guid=?)",
        ],
    ),
    (
        "viewer sessions",
        &[
            "SCAN import_session",
        ],
    ),
    (
        "viewer message row",
        &[
            "SEARCH message USING INTEGER PRIMARY KEY (rowid=?)",
        ],
    ),
    (
        "viewer body row",
        &[
            "SEARCH message_body USING INTEGER PRIMARY KEY (rowid=?)",
        ],
    ),
    (
        "viewer body parts",
        &[
            "SEARCH message_body_part USING PRIMARY KEY (body_id=?)",
        ],
    ),
    (
        "viewer body part indexes",
        &[
            "SEARCH message_body_part USING PRIMARY KEY (body_id=?)",
        ],
    ),
    (
        "viewer edited parts",
        &[
            "SEARCH message_edit_entry USING PRIMARY KEY (body_id=?)",
        ],
    ),
    (
        "viewer unsent parts",
        &[
            "SEARCH message_unsent_part USING PRIMARY KEY (body_id=?)",
        ],
    ),
    (
        "viewer body versions",
        &[
            "SEARCH b USING INDEX sqlite_autoindex_message_body_1 (message_id=?)",
            "SEARCH c USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN",
        ],
    ),
    (
        "viewer body text",
        &[
            "SEARCH message_body USING INTEGER PRIMARY KEY (rowid=?)",
        ],
    ),
    (
        "viewer lineages seen",
        &[
            "SEARCH lineage_seen USING PRIMARY KEY (entity=? AND row_id=?)",
        ],
    ),
    (
        "viewer recently deleted markers",
        &[
            "SEARCH message_recently_deleted USING INDEX message_recently_deleted_by_message (message_id=?)",
        ],
    ),
    (
        "viewer attachments",
        &[
            "SEARCH ma USING PRIMARY KEY (message_id=?)",
            "SEARCH a USING INTEGER PRIMARY KEY (rowid=?)",
            "SEARCH bl USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN",
            "USE TEMP B-TREE FOR ORDER BY",
        ],
    ),
    (
        "viewer attachment count",
        &[
            "SEARCH message_attachment USING PRIMARY KEY (message_id=?)",
        ],
    ),
    (
        "viewer reactions per target",
        &[
            "SEARCH message USING INDEX message_assoc (assoc_target_guid=?)",
        ],
    ),
    (
        "viewer shared chat",
        &[
            "SEARCH a USING COVERING INDEX chat_message_by_message (message_id=?)",
            "SEARCH b USING COVERING INDEX chat_message_by_message (message_id=? AND chat_id=?)",
        ],
    ),
    (
        "viewer first chat of a message",
        &[
            "SEARCH chat_message USING COVERING INDEX chat_message_by_message (message_id=?)",
        ],
    ),
    (
        "viewer reply count",
        &[
            "CO-ROUTINE (subquery-1)",
            "SEARCH message USING INDEX message_reply (thread_originator_guid=?)",
            "SCAN (subquery-1)",
        ],
    ),
    (
        "viewer reply reference",
        &[
            "SEARCH message USING INTEGER PRIMARY KEY (rowid=?)",
        ],
    ),
    (
        "viewer handle",
        &[
            "SEARCH handle USING INTEGER PRIMARY KEY (rowid=?)",
        ],
    ),
    (
        "viewer detail row",
        &[
            "SEARCH message USING INTEGER PRIMARY KEY (rowid=?)",
        ],
    ),
    (
        "viewer detail versions",
        &[
            "SEARCH message_body USING INDEX sqlite_autoindex_message_body_1 (message_id=?)",
        ],
    ),
    (
        "viewer edit entries",
        &[
            "SEARCH message_edit_entry USING PRIMARY KEY (body_id=?)",
        ],
    ),
    (
        "viewer edit entry count",
        &[
            "SEARCH message_edit_entry USING PRIMARY KEY (body_id=?)",
        ],
    ),
    (
        "viewer summary raw",
        &[
            "SEARCH message_body USING INTEGER PRIMARY KEY (rowid=?)",
        ],
    ),
    (
        "viewer unkeyed identifier probe",
        &[
            "CO-ROUTINE (subquery-1)",
            "SCAN message USING INDEX message_unkeyed",
            "SCAN (subquery-1)",
        ],
    ),
    (
        "viewer message chats",
        &[
            "SEARCH cm USING COVERING INDEX chat_message_by_message (message_id=?)",
            "SEARCH c USING INTEGER PRIMARY KEY (rowid=?)",
        ],
    ),
    (
        "viewer value history",
        &[
            "SEARCH value_history USING PRIMARY KEY (entity=? AND row_id=?)",
        ],
    ),
    (
        "viewer attachment ids",
        &[
            "SEARCH message_attachment USING PRIMARY KEY (message_id=?)",
            "USE TEMP B-TREE FOR ORDER BY",
        ],
    ),
    (
        "viewer blob row",
        &["SEARCH blob USING INTEGER PRIMARY KEY (rowid=?)"],
    ),
    (
        "viewer attachment row",
        &[
            "SEARCH a USING INTEGER PRIMARY KEY (rowid=?)",
            "SEARCH b USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN",
        ],
    ),
    (
        "viewer attachment observations",
        &[
            "SEARCH o USING PRIMARY KEY (attachment_id=?)",
            "SEARCH s USING INTEGER PRIMARY KEY (rowid=?)",
        ],
    ),
    (
        "viewer reaction target",
        &[
            "SEARCH message USING INTEGER PRIMARY KEY (rowid=?)",
        ],
    ),
    (
        "viewer reaction history",
        &[
            "SEARCH message USING INDEX message_assoc (assoc_target_guid=?)",
        ],
    ),
    (
        "viewer text body",
        &[
            "SEARCH b USING INTEGER PRIMARY KEY (rowid=?)",
            "SEARCH m USING INTEGER PRIMARY KEY (rowid=?)",
        ],
    ),
    (
        "viewer current body state",
        &[
            "SEARCH message_body USING INTEGER PRIMARY KEY (rowid=?)",
        ],
    ),
    (
        "viewer any unsent part",
        &[
            "SEARCH message_unsent_part USING PRIMARY KEY (body_id=?)",
        ],
    ),
    (
        "viewer day count",
        &[
            "SEARCH cm USING COVERING INDEX chat_message_timeline (chat_id=? AND sort_key>? AND sort_key<?)",
            "SEARCH m USING INTEGER PRIMARY KEY (rowid=?)",
            // Re-pinned after the PR A review: the target's own fold
            // (depth one) is a nested subquery on the same indexes.
            "CORRELATED SCALAR SUBQUERY 2",
            "SEARCH t USING INDEX message_guid (guid=?)",
            "CORRELATED SCALAR SUBQUERY 1",
            "SEARCH u USING COVERING INDEX message_guid (guid=?)",
            "SEARCH y USING COVERING INDEX chat_message_by_message (message_id=? AND chat_id=?)",
            "SEARCH x USING COVERING INDEX chat_message_by_message (message_id=? AND chat_id=?)",
        ],
    ),
    (
        "search hit message",
        &["SEARCH message USING INTEGER PRIMARY KEY (rowid=?)"],
    ),
    (
        "search hit body",
        &["SEARCH message_body USING INTEGER PRIMARY KEY (rowid=?)"],
    ),
    (
        "search hit chats",
        &["SEARCH chat_message USING COVERING INDEX chat_message_by_message (message_id=?)"],
    ),
    (
        "search highlight",
        &["SCAN message_fts VIRTUAL TABLE INDEX 0:=M1"],
    ),
    (
        "search unsearchable count",
        &[
            "SCAN m",
            "SEARCH b USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN",
        ],
    ),
    (
        "search words global",
        &[
            "SCAN f VIRTUAL TABLE INDEX 192:M1",
            "SEARCH b USING INTEGER PRIMARY KEY (rowid=?)",
            "SEARCH m USING INTEGER PRIMARY KEY (rowid=?)",
            "CORRELATED SCALAR SUBQUERY 1",
            "SEARCH ma USING PRIMARY KEY (message_id=?)",
            "CORRELATED SCALAR SUBQUERY 2",
            "SEARCH cm USING PRIMARY KEY (chat_id=? AND message_id=?)",
        ],
    ),
    (
        "search chat rows",
        &[
            "CO-ROUTINE (subquery-1)",
            "SEARCH chat_message USING COVERING INDEX chat_message_timeline (chat_id=?)",
            "SCAN (subquery-1)",
        ],
    ),
    (
        "search chat batch",
        &["SEARCH chat_message USING COVERING INDEX chat_message_timeline (chat_id=? AND sort_key>? AND (sort_key,message_id)<(?,?))"],
    ),
    (
        "search message filter row",
        &["SEARCH message USING INTEGER PRIMARY KEY (rowid=?)"],
    ),
    (
        "search body ids",
        &["SEARCH message_body USING COVERING INDEX sqlite_autoindex_message_body_1 (message_id=?)"],
    ),
    (
        "search words probe",
        &["SCAN message_fts VIRTUAL TABLE INDEX 0:=M1"],
    ),
    (
        "search scan batch",
        &[
            "SEARCH m USING INDEX message_sort (sort_key>? AND sort_key<?)",
            "SEARCH b USING INTEGER PRIMARY KEY (rowid=?) LEFT-JOIN",
        ],
    ),
    (
        "search scan body text",
        &["SEARCH message_body USING INTEGER PRIMARY KEY (rowid=?)"],
    ),
    (
        "search scan body texts",
        &["SEARCH message_body USING INDEX sqlite_autoindex_message_body_1 (message_id=?)"],
    ),
    (
        "search attachment exists",
        &["SEARCH message_attachment USING PRIMARY KEY (message_id=?)"],
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
    let mut mismatches = Vec::new();
    for (name, sql) in &statements {
        let got = plan(&c, sql);
        match PINNED.iter().find(|(n, _)| n == name) {
            Some((_, want)) if got == *want => {}
            Some(_) => mismatches.push(format!("{name}: {got:?}")),
            None => mismatches.push(format!("{name} is not pinned: {got:?}")),
        }
    }
    assert_eq!(statements.len(), PINNED.len(), "{mismatches:?}");
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
                // The import sessions are few (one per import) and read
                // whole once per call.
                let allowed = list_scans.contains(&name)
                    || (name, line.as_str()) == ("viewer newest session", "SCAN import_session")
                    || (name, line.as_str()) == ("viewer sessions", "SCAN import_session")
                    // The unsearchable count is one pass over `message`,
                    // once per viewer session, under the Search budget.
                    || (name, line.as_str()) == ("search unsearchable count", "SCAN m")
                    // FTS5 is reached only through its MATCH index
                    // (`M1`), alone or with `rowid=` (`=M1`).
                    || (name.starts_with("search ")
                        && line.contains("VIRTUAL TABLE INDEX")
                        && (line.ends_with(":M1") || line.ends_with(":=M1")))
                    // The unkeyed probe walks the partial index of unkeyed
                    // messages only, under its own budgeted phase.
                    || (name, line.as_str())
                        == (
                            "viewer unkeyed identifier probe",
                            "SCAN message USING INDEX message_unkeyed",
                        );
                assert!(allowed, "{name}: {line}");
            }
            if line.contains("TEMP B-TREE") {
                // A message's attachments (at most one page's worth) are
                // ordered by their M4 order key, which no index holds.
                assert!(
                    name.starts_with("viewer conversation order")
                        || name == "viewer attachments"
                        || name == "viewer attachment ids",
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
