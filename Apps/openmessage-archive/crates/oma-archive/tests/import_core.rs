//! The import core (slice 4f) on the import catalogue: P1 handles and P2
//! chats against the generator's content model and counts, every scenario
//! in every order, the session's facts, and the refusals.
//!
//! The model is a second statement of the spec's rules, so agreement shows
//! self-consistency, not correctness against a real device.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use common::import::*;
use common::*;
use oma_archive::hooks::{arm, install, FaultPoint, Faults, TestSchema};
use oma_archive::{
    ArchiveCtx, ArchiveError, CancelToken, CoreOutcome, FixedClock, LineageChoice, LineageId,
    NoProgress, Recovery,
};
use serde_json::Value;

fn manifest_of(rel: &str) -> Value {
    catalogue_json(rel)
}

/// Compare the archive with a manifest; returns how many history values
/// and unkeyed rows the comparison covered.
fn assert_view(db: &std::path::Path, m: &Value, what: &str) -> (usize, usize) {
    let view = manifest_view(db);
    let mut covered = (0, 0);
    for plural in ["handles", "chats"] {
        let want = manifest_expected(m, plural);
        for r in &want {
            covered.0 += r["history"].as_object().map_or(0, |h| {
                h.values().map(|v| v.as_array().map_or(0, Vec::len)).sum()
            });
            covered.1 += usize::from(r["ref"] == "unkeyed");
        }
        let got = &view[plural];
        if got != &want {
            let g: BTreeSet<String> = got.iter().map(Value::to_string).collect();
            let w: BTreeSet<String> = want.iter().map(Value::to_string).collect();
            panic!(
                "{what}: {plural} differ\n  only in the archive: {:#?}\n  only in the model: {:#?}",
                g.difference(&w).collect::<Vec<_>>(),
                w.difference(&g).collect::<Vec<_>>()
            );
        }
    }
    covered
}

#[test]
fn every_entry_imports_its_handles_and_chats_as_the_model_says() {
    for entry in ENTRIES {
        let (tmp, lib) = library();
        let id = lib.create(None).expect("create");
        let mut a = lib.open(&id).expect("open");
        let src = open_entry(entry);
        let s = import(&mut a, &src, &opts(1000), LineageChoice::New);
        assert!(!s.resumed && s.superseded == 0, "{entry}");
        drop(a);
        assert!(src.ob.close().removed, "{entry}: staging not removed");
        let db = db_path(&tmp, &id);
        assert_consistent(&db);
        let (_, unkeyed) = assert_view(
            &db,
            &manifest_of(&format!("{entry}.expected/content.json")),
            entry,
        );
        assert_eq!(unkeyed > 0, *entry == "imp-keys-hostile-plain", "{entry}");
        let want = manifest_of(&format!("{entry}.expected/import.json"));
        assert_eq!(want["entry"], *entry);
        assert_eq!(
            counts_json(&s.counts.handles),
            want["counts"]["handles"],
            "{entry}: handle counts"
        );
        assert_eq!(
            counts_json(&s.counts.chats),
            want["counts"]["chats"],
            "{entry}: chat counts"
        );
        assert_eq!(s.counts.batches, 2, "{entry}: one batch per pass");
    }
}

/// Stand in for finalize (slice 4j), which does not exist yet: mark the
/// running session completed and empty its session-local tables, so the
/// next import of the same content starts a **fresh** session instead of
/// continuing this one (spec §8.2 step 1 resumes only unfinished ones).
fn finish_sessions(db: &std::path::Path) {
    let c = raw(db);
    c.execute_batch(
        "DELETE FROM import_rowid_map WHERE session_id IN \
             (SELECT id FROM import_session WHERE state = 0); \
         DELETE FROM import_occurrence WHERE session_id IN \
             (SELECT id FROM import_session WHERE state = 0); \
         DELETE FROM import_dup_key WHERE session_id IN \
             (SELECT id FROM import_session WHERE state = 0); \
         UPDATE import_session SET state = 1, ended_at_ns = started_at_ns, \
             full_observation = (qualifiers & 2) = 0 WHERE state = 0;",
    )
    .expect("finish");
}

/// Every scenario's content, in every order it lists; the orders give equal
/// content snapshots; each step's counts match the model's.
///
/// Each step's session is completed before the next step
/// ([`finish_sessions`]), so a step whose staged content equals an earlier
/// step's is a fresh session that re-observes it, as it will be once
/// finalize exists: unkeyed rows must be idempotent and an equal or
/// undated rank must merge as the model says.
#[test]
fn every_scenario_holds_in_every_order() {
    let mut names: Vec<String> = fs::read_dir(imports_root().join("scenarios"))
        .expect("scenarios")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter_map(|n| n.strip_suffix(".content.json").map(str::to_owned))
        .collect();
    names.sort();
    assert_eq!(names.len(), 10);
    let mut history_values = 0;
    let mut repeated_steps = 0;
    for name in &names {
        let m = manifest_of(&format!("scenarios/{name}.content.json"));
        let counts = manifest_of(&format!("scenarios/{name}.counts.json"));
        let orders = m["orders"].as_array().expect("orders");
        let mut snapshots = Vec::new();
        for (oi, order) in orders.iter().enumerate() {
            let (tmp, lib) = library();
            let id = lib.create(None).expect("create");
            let db = db_path(&tmp, &id);
            let mut labels: BTreeMap<String, LineageId> = BTreeMap::new();
            let mut digests: BTreeSet<String> = BTreeSet::new();
            for (si, step) in order.as_array().expect("steps").iter().enumerate() {
                let entry = step["entry"].as_str().expect("entry");
                let label = step["lineage"].as_str().expect("lineage");
                let mut o = opts(1000);
                o.include_recently_deleted = step["includeRecentlyDeleted"] == true;
                let mut a = lib.open(&id).expect("open");
                let src = open_entry(entry);
                let lineage = lineage_for(&mut labels, label);
                let s = import(&mut a, &src, &o, lineage);
                labels.insert(label.to_owned(), s.lineage);
                drop(a);
                assert!(src.ob.close().removed);
                finish_sessions(&db);
                assert!(!s.resumed, "{name} order {oi} step {si}");
                let digest = m["sessions"][entry]["contentDigest"]
                    .as_str()
                    .expect("digest")
                    .to_owned();
                if !digests.insert(digest) {
                    repeated_steps += 1;
                }
                let want = &counts["orders"][oi][si];
                assert_eq!(want["entry"], entry);
                for (plural, got) in [("handles", &s.counts.handles), ("chats", &s.counts.chats)] {
                    assert_eq!(
                        counts_json(got),
                        want["counts"][plural],
                        "{name} order {oi} step {si} ({entry}): {plural} counts"
                    );
                }
            }
            assert_consistent(&db);
            history_values += assert_view(&db, &m, &format!("{name} order {oi}")).0;
            snapshots.push(logical_snapshot(&db, Scope::Content));
        }
        // Order independence (spec §8.7): equal content snapshots.
        for s in &snapshots[1..] {
            assert_eq!(s, &snapshots[0], "{name}: the orders differ");
        }
    }
    // dev1-v1-v2 (both orders) and lineages carry history values.
    assert!(history_values >= 3, "{history_values}");
    // keys-hostile-twice, dev1-v1-undated, dev1-v1-baddate and
    // dev1-v2-plain-enc each repeat a digest.
    assert!(repeated_steps >= 4, "{repeated_steps}");
}

/// Unkeyed rows are idempotent across sessions: the hostile entry imported
/// twice, in two distinct sessions, leaves the content unchanged and finds
/// every row already present.
#[test]
fn a_second_session_of_the_same_content_inserts_nothing() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let db = db_path(&tmp, &id);
    let src = open_entry("imp-keys-hostile-plain");
    let mut a = lib.open(&id).expect("open");
    let first = import(&mut a, &src, &opts(16), LineageChoice::New);
    drop(a);
    finish_sessions(&db);
    let before = logical_snapshot(&db, Scope::Content);
    let mut a = lib.open(&id).expect("open");
    let second = import(
        &mut a,
        &src,
        &opts(16),
        LineageChoice::Existing(first.lineage),
    );
    drop(a);
    src.ob.close();
    assert!(!second.resumed);
    assert_ne!(second.session, first.session);
    assert!(first.counts.handles.unkeyed > 0 && first.counts.chats.unkeyed > 0);
    for (got, seen) in [
        (&second.counts.handles, first.counts.handles.seen),
        (&second.counts.chats, first.counts.chats.seen),
    ] {
        assert_eq!(got.seen, seen);
        assert_eq!(got.inserted, 0, "{got:?}");
        assert_eq!(got.already_present, seen, "{got:?}");
    }
    let after = logical_snapshot(&db, Scope::Content);
    for t in ["handle", "chat", "value_issue", "value_history"] {
        assert_eq!(before[t], after[t], "{t}");
    }
}

/// rank, order_class, order_key, order_key_source, qualifiers, state,
/// content_digest.
type SessionRow = (Vec<u8>, i64, Option<i64>, Option<i64>, i64, i64, Vec<u8>);

#[test]
fn the_session_records_the_rank_and_qualifiers_the_model_computes() {
    for entry in ENTRIES {
        let (tmp, lib) = library();
        let id = lib.create(None).expect("create");
        let mut a = lib.open(&id).expect("open");
        let src = open_entry(entry);
        let s = import(&mut a, &src, &opts(1000), LineageChoice::New);
        drop(a);
        src.ob.close();
        let c = read_only(&db_path(&tmp, &id));
        let (rank, class, key, source, qualifiers, state, digest): SessionRow = c
            .query_row(
                "SELECT rank, order_class, order_key, order_key_source, qualifiers, state, \
                        content_digest FROM import_session WHERE id = ?1",
                [s.session],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                },
            )
            .expect("session");
        let m = manifest_of(&format!("{entry}.expected/content.json"));
        let want = &m["sessions"][*entry];
        assert_eq!(hex(&rank), want["rank"].as_str().expect("rank"), "{entry}");
        assert_eq!(
            hex(&digest),
            want["contentDigest"].as_str().expect("d"),
            "{entry}"
        );
        assert_eq!(class == 1, want["orderClass"] == "dated", "{entry}");
        assert_eq!(key, want["orderKeyNs"].as_i64(), "{entry}");
        let want_source = match want["orderKeySource"].as_str() {
            Some("statusPlistDate") => Some(1),
            Some("infoPlistLastBackupDate") => Some(2),
            _ => None,
        };
        assert_eq!(source, want_source, "{entry}");
        let model_q: BTreeSet<&str> = want["qualifiers"]
            .as_array()
            .expect("q")
            .iter()
            .map(|q| q.as_str().expect("q"))
            .collect();
        assert_eq!(
            qualifiers & 1 != 0,
            model_q.contains("backupDateOutOfRange"),
            "{entry}"
        );
        assert_eq!(
            qualifiers & 2 != 0,
            model_q.contains("walSkipped"),
            "{entry}"
        );
        // Until finalize exists, a session whose passes ran stays running.
        assert_eq!(state, 0, "{entry}");
        let dup: i64 = c
            .query_row(
                "SELECT count(*) FROM import_dup_key WHERE session_id = ?1",
                [s.session],
                |r| r.get(0),
            )
            .expect("dups");
        assert_eq!(dup > 0, *entry == "imp-keys-hostile-plain", "{entry}");
    }
}

#[test]
fn duplicated_keys_are_unkeyed_in_every_occurrence_and_persisted() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let mut a = lib.open(&id).expect("open");
    let src = open_entry("imp-keys-hostile-plain");
    let s = import(&mut a, &src, &opts(16), LineageChoice::New);
    drop(a);
    src.ob.close();
    let c = read_only(&db_path(&tmp, &id));
    let per_entity: BTreeMap<i64, i64> = c
        .prepare(
            "SELECT entity, count(*) FROM import_dup_key WHERE session_id = ?1 GROUP BY entity",
        )
        .expect("prepare")
        .query_map([s.session], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    // Duplicated (id, service) handles, chat GUIDs, message GUIDs and
    // attachment GUIDs (catalogue README).
    for e in [1, 2, 3, 4] {
        assert!(
            per_entity.get(&e).copied().unwrap_or(0) >= 1,
            "entity {e}: {per_entity:?}"
        );
    }
    // Every unkeyed row has a distinct (fp, occ), and occurrences count
    // from zero per fingerprint.
    let unkeyed: Vec<(Vec<u8>, i64)> = c
        .prepare("SELECT unkeyed_fp, unkeyed_occ FROM handle WHERE key_kind = 2 ORDER BY 1, 2")
        .expect("prepare")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    assert_eq!(unkeyed.len(), 5);
    let mut by_fp: BTreeMap<Vec<u8>, Vec<i64>> = BTreeMap::new();
    for (fp, occ) in unkeyed {
        by_fp.entry(fp).or_default().push(occ);
    }
    for occs in by_fp.values() {
        assert_eq!(
            *occs,
            (0..i64::try_from(occs.len()).expect("n")).collect::<Vec<_>>()
        );
    }
    let occ: i64 = c
        .query_row(
            "SELECT sum(seen) FROM import_occurrence WHERE session_id = ?1 AND entity = 1",
            [s.session],
            |r| r.get(0),
        )
        .expect("occ");
    assert_eq!(occ, 5);
}

#[test]
fn other_options_supersede_and_the_fresh_session_changes_no_content() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let mut a = lib.open(&id).expect("open");
    let src = open_entry("imp-dev1-v2-plain");
    let first = import(&mut a, &src, &opts(1000), LineageChoice::New);
    drop(a);
    src.ob.close();
    let before = logical_snapshot(&db_path(&tmp, &id), Scope::Content);

    // The same content with a different option, then with another lineage,
    // then with a different batch budget (which is not part of the
    // binding, so it continues).
    let mut o = opts(1000);
    o.include_recently_deleted = true;
    let mut a = lib.open(&id).expect("open");
    let src = open_entry("imp-dev1-v2-plain");
    let second = import(&mut a, &src, &o, LineageChoice::Existing(first.lineage));
    assert!(!second.resumed);
    assert_eq!(second.superseded, 1);
    assert_eq!(second.counts.handles.inserted, 0);
    assert_eq!(second.counts.handles.already_present, 4);
    assert_eq!(second.counts.handles.cells_replaced, 0);
    let third = import(&mut a, &src, &o, LineageChoice::New);
    assert!(!third.resumed);
    assert_eq!(third.superseded, 1);
    let fourth = import(
        &mut a,
        &src,
        &opts(16),
        LineageChoice::Existing(third.lineage),
    );
    assert!(!fourth.resumed, "the RD option differs again");
    let mut o16 = opts(16);
    o16.include_recently_deleted = false;
    let fifth = import(&mut a, &src, &o16, LineageChoice::Existing(third.lineage));
    assert!(fifth.resumed, "same binding: continued");
    assert_eq!(fifth.session, fourth.session);
    drop(a);
    src.ob.close();
    let db = db_path(&tmp, &id);
    assert_consistent(&db);
    let c = read_only(&db);
    let states: Vec<i64> = c
        .prepare("SELECT state FROM import_session ORDER BY id")
        .expect("prepare")
        .query_map([], |r| r.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    assert_eq!(states, [5, 5, 5, 0]);
    // A superseded session keeps no map, counter or duplicate rows.
    let leftovers: i64 = c
        .query_row(
            "SELECT (SELECT count(*) FROM import_rowid_map WHERE session_id <> ?1) + \
                    (SELECT count(*) FROM import_cursor WHERE session_id <> ?1 AND done = 0)",
            [fourth.session],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(leftovers, 0);
    drop(c);
    // Re-observing identical content changes no content, except that a
    // second lineage now saw the chats.
    let after = logical_snapshot(&db, Scope::Content);
    assert_eq!(before["handle"], after["handle"]);
    assert_eq!(before["chat"], after["chat"]);
    assert_eq!(before["value_issue"], after["value_issue"]);
    assert_eq!(before["value_history"], after["value_history"]);
    assert_ne!(before["lineage_seen"], after["lineage_seen"]);
}

#[test]
fn a_cancel_before_the_session_starts_writes_nothing() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let before = data_snapshot(&tmp, &id);
    let mut a = lib.open(&id).expect("open");
    let src = open_entry("imp-dev1-v1-plain");
    let token = CancelToken::new();
    token.cancel();
    let out = import_with(&mut a, &src, &opts(1000), LineageChoice::New, &token).expect("import");
    assert_eq!(out, CoreOutcome::Cancelled(None));
    drop(a);
    assert!(src.ob.close().removed);
    assert_eq!(data_snapshot(&tmp, &id), before);
}

#[test]
fn an_unknown_lineage_is_refused_and_nothing_is_recorded() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let mut a = lib.open(&id).expect("open");
    let src = open_entry("imp-dev1-v1-plain");
    let err = import_with(
        &mut a,
        &src,
        &opts(1000),
        LineageChoice::Existing(LineageId::new(42)),
        &CancelToken::new(),
    )
    .expect_err("unknown lineage");
    assert!(matches!(err, ArchiveError::UnknownLineage), "{err:?}");
    // The archive was opened for writing; the bookkeeping rolled back.
    a.begin_write(Recovery::Skip).expect("writer");
    drop(a);
    src.ob.close();
    let c = read_only(&db_path(&tmp, &id));
    let n: i64 = c
        .query_row(
            "SELECT (SELECT count(*) FROM import_session) + (SELECT count(*) FROM source_backup) \
                  + (SELECT count(*) FROM lineage) + (SELECT count(*) FROM handle)",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(n, 0);
}

#[test]
fn a_migration_pending_or_newer_rules_refuse_the_import() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    {
        let c = raw(&db_path(&tmp, &id));
        c.execute(
            "UPDATE archive_meta SET max_deriver_version = max_deriver_version + 99",
            [],
        )
        .expect("bump");
    }
    let mut a = lib.open(&id).expect("open");
    let src = open_entry("imp-dev1-v1-plain");
    let err = import_with(
        &mut a,
        &src,
        &opts(1000),
        LineageChoice::New,
        &CancelToken::new(),
    )
    .expect_err("newer rules");
    assert!(matches!(err, ArchiveError::NewerRules), "{err:?}");
    drop(a);
    src.ob.close();
}

/// The rows of the session-local tables that belong to `session`.
fn session_local_rows(db: &std::path::Path, session: i64) -> i64 {
    read_only(db)
        .query_row(
            "SELECT (SELECT count(*) FROM import_rowid_map WHERE session_id = ?1) \
                  + (SELECT count(*) FROM import_occurrence WHERE session_id = ?1) \
                  + (SELECT count(*) FROM import_dup_key WHERE session_id = ?1)",
            [session],
            |r| r.get(0),
        )
        .expect("count")
}

/// A cancelled import of the hostile entry (map rows, occurrence counters
/// and duplicate keys committed): its summary.
fn cancelled_hostile(
    lib: &oma_archive::Library,
    id: &oma_archive::ArchiveId,
) -> oma_archive::ImportSummary {
    let mut a = lib.open(id).expect("open");
    let src = open_entry("imp-keys-hostile-plain");
    let token = CancelToken::new();
    let t = token.clone();
    let guard = arm(FaultPoint::Record, 9, Box::new(move |_, _| t.cancel()));
    let out = import_with(&mut a, &src, &opts(16), LineageChoice::New, &token).expect("import");
    drop(guard);
    drop(a);
    src.ob.close();
    match out {
        CoreOutcome::Cancelled(Some(s)) => s,
        other => panic!("not cancelled: {other:?}"),
    }
}

/// A failed session (3) is never resumed, so its map rows, occurrence
/// counters and duplicate keys go with the state that ends it.
#[test]
fn a_failed_session_keeps_no_session_local_rows() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let db = db_path(&tmp, &id);
    let first = cancelled_hostile(&lib, &id);
    assert!(session_local_rows(&db, first.session) > 0);
    // Continue it with every batch failing on an I/O error.
    let mut a = lib.open(&id).expect("open");
    let src = open_entry("imp-keys-hostile-plain");
    let err = {
        let _faults = install(Faults {
            batch_error: Some(10),
            ..Faults::default()
        });
        import_with(
            &mut a,
            &src,
            &opts(16),
            LineageChoice::Existing(first.lineage),
            &CancelToken::new(),
        )
        .expect_err("fails")
    };
    assert!(matches!(err, ArchiveError::Sqlite { .. }), "{err:?}");
    drop(a);
    src.ob.close();
    let state: i64 = read_only(&db)
        .query_row(
            "SELECT state FROM import_session WHERE id = ?1",
            [first.session],
            |r| r.get(0),
        )
        .expect("state");
    assert_eq!(state, 3);
    assert_eq!(session_local_rows(&db, first.session), 0);
    assert_consistent(&db);
}

/// A migration supersedes unfinished sessions as an import does: state 5,
/// `full_observation = 0`, and no session-local rows left.
#[test]
fn a_migration_supersedes_sessions_like_an_import() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let db = db_path(&tmp, &id);
    let first = cancelled_hostile(&lib, &id);
    assert!(session_local_rows(&db, first.session) > 0);
    {
        let _faults = install(Faults {
            schema: Some(TestSchema::V2),
            ..Faults::default()
        });
        let mut a = lib.open(&id).expect("open");
        let r = a
            .migrate(&mut ArchiveCtx {
                cancel: &CancelToken::new(),
                progress: &mut NoProgress,
                clock: &FixedClock(MODEL_NOW_NS),
            })
            .expect("migrates");
        assert_eq!(r.sessions_superseded, 1);
    }
    let (state, full): (i64, Option<i64>) = read_only(&db)
        .query_row(
            "SELECT state, full_observation FROM import_session WHERE id = ?1",
            [first.session],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("session");
    assert_eq!((state, full), (5, Some(0)));
    assert_eq!(session_local_rows(&db, first.session), 0);
}
