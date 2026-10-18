//! Lineages and "not in the latest import" (spec §8.7, D80, §18.3).
//!
//! `lineage_seen` records, per lineage, the highest rank at which it saw
//! each chat, message and attachment. A row is "not in the latest import"
//! for lineage L when L's newest **full observation** ranks above that.
//! Finalize (slice 4j) sets `full_observation`; these tests stand in for it
//! with `finish_sessions`, which marks a session full unless its `-wal`
//! was skipped, and leave a cancelled session as the import left it.
//!
//! - dev1 v1 (L1), then dev2 (L2), then dev1 v2 (L1): v1's rows that v2
//!   lacks are labelled by L1 only; dev2's rows are never labelled by L1,
//!   and nothing is labelled by L2.
//! - A cancelled latest session, and a WAL-skipped one, are not full
//!   observations and label nothing.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;

use std::collections::BTreeSet;
use std::path::Path;

use common::import::*;
use common::*;
use oma_archive::hooks::{arm, FaultPoint};
use oma_archive::{CancelToken, CoreOutcome, ImportSummary, LineageChoice, LineageId};

fn step(
    lib: &oma_archive::Library,
    id: &oma_archive::ArchiveId,
    db: &Path,
    entry: &str,
    lineage: LineageChoice,
) -> ImportSummary {
    let mut a = lib.open(id).expect("open");
    let src = open_entry(entry);
    let s = import(&mut a, &src, &opts(1000), lineage);
    drop(a);
    assert!(src.ob.close().removed);
    finish_sessions(db);
    s
}

/// The (entity, GUID) rows lineage `l` labels "not in the latest import".
fn labelled(db: &Path, l: LineageId) -> BTreeSet<(i64, Vec<u8>)> {
    let c = read_only(db);
    let mut stmt = c
        .prepare(
            "SELECT s.entity, coalesce(ch.guid, m.guid, a.guid) FROM lineage_seen s \
             LEFT JOIN chat ch ON s.entity = 2 AND ch.id = s.row_id \
             LEFT JOIN message m ON s.entity = 3 AND m.id = s.row_id \
             LEFT JOIN attachment a ON s.entity = 4 AND a.id = s.row_id \
             WHERE s.lineage_id = ?1 AND s.last_rank < \
               (SELECT max(rank) FROM import_session WHERE lineage_id = ?1 \
                AND full_observation = 1)",
        )
        .expect("prepare");
    stmt.query_map([l.get()], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows")
}

/// The (entity, GUID) rows lineage `l` saw.
fn seen_by(db: &Path, l: LineageId) -> BTreeSet<(i64, Vec<u8>)> {
    let c = read_only(db);
    let mut stmt = c
        .prepare(
            "SELECT s.entity, coalesce(ch.guid, m.guid, a.guid) FROM lineage_seen s \
             LEFT JOIN chat ch ON s.entity = 2 AND ch.id = s.row_id \
             LEFT JOIN message m ON s.entity = 3 AND m.id = s.row_id \
             LEFT JOIN attachment a ON s.entity = 4 AND a.id = s.row_id \
             WHERE s.lineage_id = ?1",
        )
        .expect("prepare");
    stmt.query_map([l.get()], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows")
}

#[test]
fn not_in_the_latest_import_is_per_lineage() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let db = db_path(&tmp, &id);
    let v1 = step(&lib, &id, &db, "imp-dev1-v1-plain", LineageChoice::New);
    let l1 = v1.lineage;
    assert!(labelled(&db, l1).is_empty(), "one session labels nothing");
    let dev2 = step(&lib, &id, &db, "imp-dev2-plain", LineageChoice::New);
    let l2 = dev2.lineage;
    assert_ne!(l1, l2);
    // Another lineage's import labels nothing in L1.
    assert!(labelled(&db, l1).is_empty());
    let v1_rows = seen_by(&db, l1);
    let dev2_rows = seen_by(&db, l2);
    assert!(v1_rows.is_disjoint(&dev2_rows), "zero GUID overlap");
    step(
        &lib,
        &id,
        &db,
        "imp-dev1-v2-plain",
        LineageChoice::Existing(l1),
    );
    let gone = labelled(&db, l1);
    assert!(!gone.is_empty(), "v2 lacks some of v1's rows");
    assert!(gone.is_subset(&v1_rows));
    assert!(gone.is_disjoint(&dev2_rows), "L1 never labels L2's rows");
    assert!(labelled(&db, l2).is_empty());
    // A message and an attachment that v2 dropped (catalogue README).
    let entities: BTreeSet<i64> = gone.iter().map(|(e, _)| *e).collect();
    assert!(
        entities.contains(&3) && entities.contains(&4),
        "{entities:?}"
    );
    // The oracle's labels for the same sequence.
    let m = catalogue_json("scenarios/lineages.content.json");
    let mut want: BTreeSet<(i64, Vec<u8>)> = BTreeSet::new();
    for (plural, e) in [("chats", 2), ("messages", 3), ("attachments", 4)] {
        for r in m["content"][plural].as_array().expect("list") {
            if r["notInLatest"]
                .as_array()
                .is_some_and(|l| l.iter().any(|x| x == "L1"))
            {
                let g = r["ref"]
                    .as_str()
                    .and_then(|s| s.strip_prefix("guid:"))
                    .expect("keyed");
                want.insert((e, g.as_bytes().to_vec()));
            }
        }
    }
    assert_eq!(gone, want);
}

#[test]
fn a_cancelled_latest_session_labels_nothing() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let db = db_path(&tmp, &id);
    let v1 = step(&lib, &id, &db, "imp-dev1-v1-plain", LineageChoice::New);
    // v2 is cancelled in its attachment pass, after its messages were
    // seen at its rank.
    let mut a = lib.open(&id).expect("open");
    let src = open_entry("imp-dev1-v2-plain");
    let token = CancelToken::new();
    let t = token.clone();
    let records = {
        let base = catalogue_json("imp-dev1-v2-plain.expected/content.json");
        base["content"]["handles"].as_array().expect("h").len()
            + base["content"]["chats"].as_array().expect("c").len()
    };
    // Well past the handles and chats: inside P3 or later.
    let at = u32::try_from(records + 30).expect("n");
    let guard = arm(FaultPoint::Record, at, Box::new(move |_, _| t.cancel()));
    let out = import_with(
        &mut a,
        &src,
        &opts(1000),
        LineageChoice::Existing(v1.lineage),
        &token,
    )
    .expect("import");
    drop(guard);
    drop(a);
    src.ob.close();
    assert!(matches!(out, CoreOutcome::Cancelled(Some(_))), "{out:?}");
    // `finish_sessions` finishes only running sessions: the cancelled one
    // stays cancelled and is not a full observation.
    finish_sessions(&db);
    let full: Option<i64> = read_only(&db)
        .query_row(
            "SELECT full_observation FROM import_session WHERE state = 2",
            [],
            |r| r.get(0),
        )
        .expect("cancelled session");
    assert_ne!(full, Some(1));
    // v2's rows were seen at its rank, yet nothing is labelled: the newest
    // full observation is still v1.
    let c = read_only(&db);
    let higher: i64 = c
        .query_row(
            "SELECT count(*) FROM lineage_seen s JOIN import_session i ON i.id = s.last_session_id \
             WHERE i.state = 2",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert!(higher > 0);
    drop(c);
    assert!(labelled(&db, v1.lineage).is_empty());
}

#[test]
fn a_wal_skipped_latest_session_labels_nothing() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let db = db_path(&tmp, &id);
    let full = step(&lib, &id, &db, "imp-wal-plain", LineageChoice::New);
    let skipped = step(
        &lib,
        &id,
        &db,
        "imp-wal-missing-plain",
        LineageChoice::Existing(full.lineage),
    );
    let c = read_only(&db);
    let flags: Vec<(i64, Option<i64>)> = c
        .prepare("SELECT id, full_observation FROM import_session ORDER BY id")
        .expect("prepare")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    assert_eq!(flags, [(full.session, Some(1)), (skipped.session, Some(0))]);
    drop(c);
    // The rows only the -wal held are not in the WAL-skipped import, but
    // that import is no full observation, so nothing is labelled.
    assert!(labelled(&db, full.lineage).is_empty());
    // Had it been one (it ranks above the full one), the rows only the
    // -wal held would be labelled: the flag is what keeps them unlabelled.
    raw(&db)
        .execute(
            "UPDATE import_session SET full_observation = 1 WHERE id = ?1",
            [skipped.session],
        )
        .expect("counterfactual");
    assert!(!labelled(&db, full.lineage).is_empty());
    let m = catalogue_json("scenarios/wal-missing.content.json");
    for plural in ["chats", "messages", "attachments"] {
        for r in m["content"][plural].as_array().expect("list") {
            assert_eq!(r["notInLatest"], serde_json::json!([]), "{plural}");
        }
    }
}
