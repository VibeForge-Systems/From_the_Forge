//! The gate, finalize and the provenance file (spec §8.2 step 7, §8.9,
//! §8.10; slice 4j).
//!
//! - **The gate:** the preflight it sees (resume plan, device evidence per
//!   lineage, the suggested lineage, key and Recently Deleted facts, order
//!   facts), Decline and a cancel at the gate writing nothing, and only
//!   the chosen lineage continuing an unfinished session.
//! - **Finalize:** the statistics, the session-local cleanup, the terminal
//!   state, the merge and the checkpoint; a cancel before the terminal
//!   state is resumable, one after it only skips merging; a failed
//!   terminal bookkeeping leaves the session resumable; an interrupt never
//!   reaches a batch commit.
//! - **Provenance:** written at every terminal state, rewritten when a
//!   session ends again, content-free, and best effort.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;

use std::path::Path;

use common::import::*;
use common::*;
use oma_archive::hooks::{arm, install, FaultPoint, Faults};
use oma_archive::{
    Archive, ArchiveCtx, CancelToken, CheckpointOutcome, DeviceEvidence, FixedClock, FreshReason,
    GateDecision, ImportGate, ImportOptions, ImportOutcome, LineageChoice, NoProgress, OrderSource,
    Preflight, ResumePlan, StopReason,
};
use serde_json::Value;

/// What a recording gate answers.
#[derive(Clone)]
enum Answer {
    Decline,
    Proceed(LineageChoice),
    Suggested,
    /// Cancel the job while "waiting", then answer Proceed.
    CancelThenProceed(CancelToken),
}

/// A gate that keeps every preflight it was shown.
struct Recorder {
    seen: Vec<Preflight>,
    answer: Answer,
}

impl Recorder {
    fn new(answer: Answer) -> Recorder {
        Recorder {
            seen: Vec::new(),
            answer,
        }
    }

    fn only(&self) -> &Preflight {
        assert_eq!(self.seen.len(), 1, "the gate is asked exactly once");
        &self.seen[0]
    }
}

impl ImportGate for Recorder {
    fn confirm(&mut self, p: &Preflight) -> GateDecision {
        self.seen.push(p.clone());
        match &self.answer {
            Answer::Decline => GateDecision::Decline,
            Answer::Proceed(l) => GateDecision::Proceed { lineage: *l },
            Answer::Suggested => GateDecision::Proceed {
                lineage: p.suggested_lineage,
            },
            Answer::CancelThenProceed(t) => {
                t.cancel();
                GateDecision::Proceed {
                    lineage: LineageChoice::New,
                }
            }
        }
    }
}

fn run(
    a: &mut Archive,
    src: &Source,
    o: &ImportOptions,
    gate: &mut dyn ImportGate,
    token: &CancelToken,
) -> ImportOutcome {
    let clock = FixedClock(MODEL_NOW_NS);
    let mut ctx = ArchiveCtx {
        cancel: token,
        progress: &mut NoProgress,
        clock: &clock,
    };
    a.import(&src.ob, o, gate, &mut ctx).expect("import")
}

/// Show `entry` to a recording gate that declines; the preflight.
fn preflight_of(lib: &oma_archive::Library, id: &oma_archive::ArchiveId, entry: &str) -> Preflight {
    let mut a = lib.open(id).expect("open");
    let src = open_entry(entry);
    let mut gate = Recorder::new(Answer::Decline);
    let out = run(&mut a, &src, &opts(1000), &mut gate, &CancelToken::new());
    assert_eq!(out, ImportOutcome::Declined);
    src.ob.close();
    gate.only().clone()
}

fn completed(out: ImportOutcome) -> oma_archive::ImportSummary {
    match out {
        ImportOutcome::Completed(s) => s,
        other => panic!("not completed: {other:?}"),
    }
}

#[test]
fn decline_and_a_cancel_at_the_gate_write_nothing() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let before = data_snapshot(&tmp, &id);
    let p = preflight_of(&lib, &id, "imp-dev1-v1-plain");
    assert_eq!(data_snapshot(&tmp, &id), before, "decline");
    // A first import.
    assert_eq!(
        p.resume,
        ResumePlan::Fresh {
            supersedes: 0,
            reason: FreshReason::NothingToContinue
        }
    );
    assert!(!p.same_content_completed);
    assert_eq!(p.device, DeviceEvidence::FirstImport);
    assert!(p.lineages.is_empty());
    assert_eq!(p.suggested_lineage, LineageChoice::New);
    assert_eq!(p.keys.messages_already_archived, 0);
    assert!(p.keys.messages.rows >= p.keys.messages.keyable);
    assert!(p.order.dated && !p.order.out_of_range && !p.order.sorts_before_latest);
    assert_eq!(p.order.source, Some(OrderSource::StatusPlistDate));
    assert!(p.qualifiers.is_empty() && !p.encrypted_source);
    assert_eq!(p.windows_acl_inherited, cfg!(windows));

    // A cancel while the gate waits: nothing written either.
    let mut a = lib.open(&id).expect("open");
    let src = open_entry("imp-dev1-v1-plain");
    let token = CancelToken::new();
    let mut gate = Recorder::new(Answer::CancelThenProceed(token.clone()));
    let out = run(&mut a, &src, &opts(1000), &mut gate, &token);
    assert_eq!(out, ImportOutcome::Cancelled(None));
    drop(a);
    src.ob.close();
    assert_eq!(data_snapshot(&tmp, &id), before, "cancel at the gate");
}

/// The preflight's key and Recently Deleted facts are the counts the
/// import then makes.
#[test]
fn the_preflight_facts_are_what_the_import_counts() {
    for entry in ["imp-keys-hostile-plain", "imp-rd-plain"] {
        let (_tmp, lib) = library();
        let id = lib.create(None).expect("create");
        let mut a = lib.open(&id).expect("open");
        let src = open_entry(entry);
        let mut gate = Recorder::new(Answer::Proceed(LineageChoice::New));
        let s = completed(run(
            &mut a,
            &src,
            &opts(1000),
            &mut gate,
            &CancelToken::new(),
        ));
        drop(a);
        src.ob.close();
        let p = gate.only();
        let c = &s.counts;
        assert_eq!(p.keys.handles.rows, c.handles.seen, "{entry}");
        assert_eq!(p.keys.handles.unkeyed, c.handles.unkeyed, "{entry}");
        assert_eq!(p.keys.chats.rows, c.chats.seen, "{entry}");
        assert_eq!(p.keys.chats.unkeyed, c.chats.unkeyed, "{entry}");
        let rd = p.recently_deleted;
        assert_eq!(rd.messages, c.recently_deleted.messages, "{entry}");
        assert_eq!(rd.excluded, c.recently_deleted.excluded, "{entry}");
        assert_eq!(
            rd.already_archived, c.recently_deleted.not_updated,
            "{entry}"
        );
        assert_eq!(
            rd.attachments_excluded, c.recently_deleted.attachments_excluded,
            "{entry}"
        );
        assert_eq!(
            p.keys.messages.rows,
            c.messages.seen + rd.excluded + rd.already_archived,
            "{entry}"
        );
        if entry == "imp-keys-hostile-plain" {
            // Duplicated keys in every keyed table (catalogue README).
            for k in [
                p.keys.handles,
                p.keys.chats,
                p.keys.messages,
                p.keys.attachments,
            ] {
                assert!(k.duplicated_keys >= 1 && k.unkeyed > 0, "{k:?}");
            }
            assert_eq!(p.keys.messages.unkeyed, c.messages.unkeyed);
            assert_eq!(p.keys.attachments.unkeyed, c.attachments.unkeyed);
        } else {
            assert!(rd.excluded > 0 && rd.attachments_excluded > 0, "{rd:?}");
        }
    }
}

#[test]
fn the_gate_compares_devices_and_suggests_the_lineage() {
    let (_tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let mut a = lib.open(&id).expect("open");
    let src = open_entry("imp-dev1-v1-plain");
    let v1 = import(&mut a, &src, &opts(1000), LineageChoice::New);
    drop(a);
    src.ob.close();

    // A newer backup of the same phone.
    let p = preflight_of(&lib, &id, "imp-dev1-v2-plain");
    assert_eq!(p.device, DeviceEvidence::Compared);
    let [l] = p.lineages.as_slice() else {
        panic!("{:?}", p.lineages);
    };
    assert_eq!((l.id, l.sessions, l.full_sessions), (v1.lineage, 1, 1));
    assert_eq!((l.model_matches, l.name_matches), (Some(true), Some(true)));
    assert!(l.device_matches() && l.last_backup_ns.is_some());
    assert_eq!(p.suggested_lineage, LineageChoice::Existing(v1.lineage));
    assert!(!p.order.sorts_before_latest && !p.same_content_completed);
    assert!(p.keys.messages_already_archived > 0);
    let mut a = lib.open(&id).expect("open");
    let src = open_entry("imp-dev1-v2-plain");
    let mut gate = Recorder::new(Answer::Suggested);
    let v2 = completed(run(
        &mut a,
        &src,
        &opts(1000),
        &mut gate,
        &CancelToken::new(),
    ));
    assert_eq!(v2.lineage, v1.lineage);
    drop(a);
    src.ob.close();

    // Another phone: no match, a new lineage suggested, no overlap.
    let p = preflight_of(&lib, &id, "imp-dev2-plain");
    assert_eq!(
        (p.lineages[0].model_matches, p.lineages[0].name_matches),
        (Some(false), Some(false))
    );
    assert_eq!(p.suggested_lineage, LineageChoice::New);
    assert_eq!(p.keys.messages_already_archived, 0);

    // The older backup again: already imported, and older than the newest.
    let p = preflight_of(&lib, &id, "imp-dev1-v1-plain");
    assert!(p.same_content_completed);
    assert!(p.order.sorts_before_latest && !p.order.source_differs);
    assert_eq!(p.lineages[0].full_sessions, 2);
}

#[test]
fn only_the_chosen_lineage_continues_an_unfinished_session() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let mut a = lib.open(&id).expect("open");
    let src = open_entry("imp-dev1-v1-plain");
    let first = cancelled_at_chats(&mut a, &src, &opts(16), LineageChoice::New);
    drop(a);
    src.ob.close();

    let p = preflight_of(&lib, &id, "imp-dev1-v1-plain");
    assert_eq!(
        p.resume,
        ResumePlan::Continue {
            session: first.session,
            lineage: first.lineage,
            from: oma_archive::schema::codes::Pass::Chats,
        }
    );
    assert_eq!(p.suggested_lineage, LineageChoice::Existing(first.lineage));

    // Another lineage sets it aside.
    let mut a = lib.open(&id).expect("open");
    let src = open_entry("imp-dev1-v1-plain");
    let second = cancelled_at_chats(&mut a, &src, &opts(16), LineageChoice::New);
    assert!(!second.resumed);
    assert_eq!(second.superseded, 1);
    // The suggestion continues it.
    let mut gate = Recorder::new(Answer::Suggested);
    let third = completed(run(&mut a, &src, &opts(16), &mut gate, &CancelToken::new()));
    assert!(third.resumed);
    assert_eq!(third.session, second.session);
    drop(a);
    src.ob.close();
    assert_completed(&db_path(&tmp, &id), third.session);
}

fn wal_len(db: &Path) -> u64 {
    std::fs::metadata(db.with_file_name("archive.sqlite-wal")).map_or(0, |m| m.len())
}

#[test]
fn finalize_writes_the_statistics_and_checkpoints() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let db = db_path(&tmp, &id);
    let mut a = lib.open(&id).expect("open");
    let src = open_entry("imp-dev1-v1-plain");
    let s = import(&mut a, &src, &opts(16), LineageChoice::New);
    let f = s.finalize.expect("report");
    assert_eq!(f.checkpoint, CheckpointOutcome::Done);
    assert!(f.fts_merge_complete && f.fts_merge_steps >= 1);
    assert!(f.session_rows_removed > 0);
    // The checkpoint truncated the -wal while the archive is still open.
    assert_eq!(wal_len(&db), 0);
    let info = a.reader().info().expect("info");
    let stats = info.stats.expect("archive_stats");
    drop(a);
    src.ob.close();
    let c = read_only(&db);
    let count = |t: &str| -> u64 {
        c.query_row(&format!("SELECT count(*) FROM {t}"), [], |r| {
            r.get::<_, i64>(0)
        })
        .map(|n| u64::try_from(n).expect("n"))
        .expect("count")
    };
    assert_eq!(stats.handles, count("handle"));
    assert_eq!(stats.chats, count("chat"));
    assert_eq!(stats.messages, count("message"));
    assert_eq!(stats.body_versions, count("message_body"));
    assert_eq!(stats.attachments, count("attachment"));
    assert_eq!(stats.blobs, count("blob"));
    assert_eq!(stats.computed_at_ns, MODEL_NOW_NS);
    // One chat_stats row per chat; the counts are the chat links, and the
    // undated ones are those without a derived instant.
    assert_eq!(count("chat_stats"), count("chat"));
    let (links, undated): (i64, i64) = c
        .query_row(
            "SELECT (SELECT sum(message_count) FROM chat_stats), \
                    (SELECT sum(undated_count) FROM chat_stats)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("stats");
    assert_eq!(u64::try_from(links).expect("n"), count("chat_message"));
    let want_undated: i64 = c
        .query_row(
            "SELECT count(*) FROM chat_message cm JOIN message m ON m.id = cm.message_id \
             WHERE m.date_utc_ns IS NULL",
            [],
            |r| r.get(0),
        )
        .expect("undated");
    assert!(want_undated > 0, "the entry has undated messages");
    assert_eq!(undated, want_undated);
}

/// The provenance file of `session`, parsed.
fn provenance(db: &Path, session: i64) -> Option<Value> {
    let p = db
        .with_file_name("imports")
        .join(format!("session-{session}.json"));
    std::fs::read(p)
        .ok()
        .map(|b| serde_json::from_slice(&b).expect("json"))
}

fn imports_entries(db: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(db.with_file_name("imports"))
        .expect("imports")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn the_provenance_file_follows_the_session_and_holds_no_content() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let db = db_path(&tmp, &id);
    let mut a = lib.open(&id).expect("open");
    let src = open_entry("imp-dev1-v1-plain");
    let first = cancelled_at_chats(&mut a, &src, &opts(16), LineageChoice::New);
    assert!(first.provenance_written);
    let p = provenance(&db, first.session).expect("written when cancelled");
    assert_eq!(p["state"], "cancelled");
    assert_eq!(p["fullObservation"], false);
    let s = import(
        &mut a,
        &src,
        &opts(16),
        LineageChoice::Existing(first.lineage),
    );
    assert!(s.resumed && s.provenance_written);
    drop(a);
    src.ob.close();
    assert_eq!(imports_entries(&db), ["session-1.json"]);
    let p = provenance(&db, s.session).expect("rewritten when completed");
    assert_eq!(p["format"], "oma-import-provenance");
    assert_eq!(p["formatVersion"], 1);
    assert_eq!(p["session"], s.session);
    assert_eq!(p["state"], "completed");
    assert_eq!(p["fullObservation"], true);
    assert_eq!(p["resumes"], 1);
    assert_eq!(p["orderClass"], "dated");
    assert_eq!(p["orderKeySource"], "statusPlistDate");
    assert_eq!(p["walOutcome"], "notListed");
    assert_eq!(p["lineage"], s.lineage.get());
    assert_eq!(p["source"]["productType"], "iPhone0,0");
    assert_eq!(p["source"]["encrypted"], false);
    assert_eq!(p["options"]["includeRecentlyDeleted"], false);
    assert_eq!(
        p["counts"],
        serde_json::to_value(&s.counts).expect("counts")
    );
    // No device name, GUID, digest, rank, label or path.
    let text = std::fs::read_to_string(
        db.with_file_name("imports")
            .join(format!("session-{}.json", s.session)),
    )
    .expect("text");
    let m = catalogue_json("imp-dev1-v1-plain.expected/content.json");
    let sess = &m["sessions"]["imp-dev1-v1-plain"];
    for banned in [
        "Synthetic Test Phone",
        "SYNTH",
        sess["contentDigest"].as_str().expect("digest"),
        sess["rank"].as_str().expect("rank"),
        id.as_str(),
        &root(&tmp).to_string_lossy(),
    ] {
        assert!(!text.contains(banned), "the provenance file holds {banned}");
    }
    #[cfg(unix)]
    assert_eq!(
        mode(&db.with_file_name("imports").join("session-1.json")),
        0o600
    );
}

#[test]
fn a_provenance_failure_never_changes_the_outcome() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let db = db_path(&tmp, &id);
    let _faults = install(Faults {
        provenance_fails: true,
        ..Faults::default()
    });
    let mut a = lib.open(&id).expect("open");
    let src = open_entry("imp-dev1-v1-plain");
    let s = import(&mut a, &src, &opts(1000), LineageChoice::New);
    assert!(!s.provenance_written);
    drop(a);
    src.ob.close();
    assert!(imports_entries(&db).is_empty());
    assert_completed(&db, s.session);
}

/// A fresh, uninterrupted import of the entry: its content snapshot.
fn fresh_snapshot(entry: &str) -> Value {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let mut a = lib.open(&id).expect("open");
    let src = open_entry(entry);
    import(&mut a, &src, &opts(16), LineageChoice::New);
    drop(a);
    src.ob.close();
    logical_snapshot(&db_path(&tmp, &id), Scope::Content)
}

#[test]
fn a_failed_terminal_bookkeeping_leaves_the_session_resumable() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let db = db_path(&tmp, &id);
    let src = open_entry("imp-dev1-v1-plain");
    let lineage = {
        let _faults = install(Faults {
            terminal_error: Some(13),
            ..Faults::default()
        });
        let mut a = lib.open(&id).expect("open");
        let out = run(
            &mut a,
            &src,
            &opts(16),
            &mut oma_archive::ProceedWith(LineageChoice::New),
            &CancelToken::new(),
        );
        let ImportOutcome::Stopped { reason, summary } = out else {
            panic!("not stopped: {out:?}");
        };
        assert_eq!(reason, StopReason::InsufficientSpace);
        assert!(!summary.provenance_written && summary.finalize.is_none());
        summary.lineage
    };
    // Still running on disk: the next write open marks it interrupted.
    let state: i64 = read_only(&db)
        .query_row("SELECT state FROM import_session", [], |r| r.get(0))
        .expect("state");
    assert_eq!(state, 0);
    let p = preflight_of(&lib, &id, "imp-dev1-v1-plain");
    assert!(matches!(
        p.resume,
        ResumePlan::Continue {
            from: oma_archive::schema::codes::Pass::Finalize,
            ..
        }
    ));
    let mut a = lib.open(&id).expect("open");
    let s = import(&mut a, &src, &opts(16), LineageChoice::Existing(lineage));
    assert!(s.resumed);
    drop(a);
    src.ob.close();
    assert_completed(&db, s.session);
    assert_eq!(
        logical_snapshot(&db, Scope::Content),
        fresh_snapshot("imp-dev1-v1-plain")
    );
}

#[test]
fn a_cancel_in_finalize_resumes_and_after_completion_only_skips_merging() {
    // Before the terminal state: cancelled (2), and continued to the same
    // archive.
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let db = db_path(&tmp, &id);
    let src = open_entry("imp-dev1-v1-plain");
    let mut a = lib.open(&id).expect("open");
    let token = CancelToken::new();
    let t = token.clone();
    let guard = arm(
        FaultPoint::FinalizeStats,
        1,
        Box::new(move |_, _| t.cancel()),
    );
    let out = run(
        &mut a,
        &src,
        &opts(16),
        &mut oma_archive::ProceedWith(LineageChoice::New),
        &token,
    );
    drop(guard);
    let ImportOutcome::Cancelled(Some(first)) = out else {
        panic!("not cancelled: {out:?}");
    };
    let s = import(
        &mut a,
        &src,
        &opts(16),
        LineageChoice::Existing(first.lineage),
    );
    assert!(s.resumed);
    drop(a);
    assert_completed(&db, s.session);
    assert_eq!(
        logical_snapshot(&db, Scope::Content),
        fresh_snapshot("imp-dev1-v1-plain")
    );

    // After it: completed, with no merge step.
    let (_tmp2, lib2) = library();
    let id2 = lib2.create(None).expect("create");
    let mut a = lib2.open(&id2).expect("open");
    let token = CancelToken::new();
    let t = token.clone();
    let guard = arm(
        FaultPoint::TerminalCommitted,
        1,
        Box::new(move |_, _| t.cancel()),
    );
    let out = run(
        &mut a,
        &src,
        &opts(16),
        &mut oma_archive::ProceedWith(LineageChoice::New),
        &token,
    );
    drop(guard);
    let s = completed(out);
    let f = s.finalize.expect("report");
    assert_eq!((f.fts_merge_steps, f.fts_merge_complete), (0, false));
    assert_eq!(f.checkpoint, CheckpointOutcome::Done);
    drop(a);
    src.ob.close();
}

/// A cancel that arrives while a batch is about to commit never interrupts
/// it: the batch commits its records (spec §8.6; the batch path is never
/// armed).
#[test]
fn a_cancel_never_interrupts_a_batch_commit() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let mut a = lib.open(&id).expect("open");
    let src = open_entry("imp-dev1-v1-plain");
    let token = CancelToken::new();
    let t = token.clone();
    let guard = arm(
        FaultPoint::BatchBeforeCommit,
        1,
        Box::new(move |_, _| t.cancel()),
    );
    let out = run(
        &mut a,
        &src,
        &opts(16),
        &mut oma_archive::ProceedWith(LineageChoice::New),
        &token,
    );
    drop(guard);
    let ImportOutcome::Cancelled(Some(s)) = out else {
        panic!("not cancelled: {out:?}");
    };
    drop(a);
    src.ob.close();
    assert_eq!(s.counts.batches, 1);
    let rows: i64 = read_only(&db_path(&tmp, &id))
        .query_row(
            "SELECT rows_done FROM import_cursor WHERE session_id = ?1 AND pass = 1",
            [s.session],
            |r| r.get(0),
        )
        .expect("cursor");
    assert_eq!(u64::try_from(rows).expect("n"), s.counts.handles.seen);
    assert!(rows > 0);
}
