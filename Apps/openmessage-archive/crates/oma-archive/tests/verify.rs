//! The integrity verifier and the blob reader (spec §11, §4.3 `open_blob`,
//! §9.5, §18.6 and §18.7): a clean archive passes both depths and its runs
//! are recorded; store damage, crash leftovers, planted entries and
//! database damage are each reported by kind; the walk is bounded; a
//! cancel is recorded as cancelled; a verify never repairs or sweeps
//! anything; `open_blob` hands out only checked, re-hashed bytes.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use common::import::*;
use common::*;
use oma_archive::hooks::{install, Faults, TestSchema};
use oma_archive::{
    ArchiveCtx, ArchiveError, ArchiveEvent, ArchiveSink, BlobId, BlobProblem, CancelToken,
    CheckState, FixedClock, Library, LineageChoice, NoProgress, SchemaCheck, VerifyCounts,
    VerifyDepth, VerifyOutcome, VerifyPhase, VerifyReport, STORE_WALK_SLACK,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// The verify clock.
const NOW: i64 = 1_900_000_000_000_000_000;

/// One catalogue entry imported into a fresh archive, which is closed.
struct Imported {
    tmp: tempfile::TempDir,
    lib: Library,
    id: oma_archive::ArchiveId,
}

impl Imported {
    fn db(&self) -> PathBuf {
        db_path(&self.tmp, &self.id)
    }
    fn dir(&self) -> PathBuf {
        archive_dir(&self.tmp, &self.id)
    }
    fn att(&self) -> PathBuf {
        self.dir().join("attachments")
    }
    /// The store file of `sha`.
    fn file(&self, sha: &str) -> PathBuf {
        self.att().join(&sha[..2]).join(sha)
    }
}

fn imported(entry: &str) -> Imported {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let mut a = lib.open(&id).expect("open");
    let src = open_entry(entry);
    import(&mut a, &src, &opts(1000), LineageChoice::New);
    a.close();
    assert!(src.ob.close().removed);
    Imported { tmp, lib, id }
}

/// Records every verify event.
#[derive(Default)]
struct Events(Vec<(VerifyPhase, u64, Option<u64>)>);

impl ArchiveSink for Events {
    fn event(&mut self, event: ArchiveEvent) {
        if let ArchiveEvent::Verify { phase, done, total } = event {
            self.0.push((phase, done, total));
        }
    }
}

/// Cancels at the first event of `phase`.
struct CancelAt(VerifyPhase, CancelToken);

impl ArchiveSink for CancelAt {
    fn event(&mut self, event: ArchiveEvent) {
        if matches!(event, ArchiveEvent::Verify { phase, .. } if phase == self.0) {
            self.1.cancel();
        }
    }
}

fn verify_with(at: &Imported, depth: VerifyDepth, sink: &mut dyn ArchiveSink) -> VerifyReport {
    let mut a = at.lib.open(&at.id).expect("open");
    let cancel = CancelToken::new();
    verify_on(&mut a, depth, sink, &cancel)
}

fn verify_on(
    a: &mut oma_archive::Archive,
    depth: VerifyDepth,
    sink: &mut dyn ArchiveSink,
    cancel: &CancelToken,
) -> VerifyReport {
    a.verify(
        depth,
        &mut ArchiveCtx {
            cancel,
            progress: sink,
            clock: &FixedClock(NOW),
        },
    )
    .expect("verify")
}

fn verify(at: &Imported, depth: VerifyDepth) -> VerifyReport {
    verify_with(at, depth, &mut NoProgress)
}

/// The counts a clean archive gives, apart from the totals.
fn clean(c: &VerifyCounts) -> VerifyCounts {
    let mut want = VerifyCounts::default();
    want.blobs = c.blobs;
    want.blobs_hashed = c.blobs_hashed;
    want.bytes_hashed = c.bytes_hashed;
    want.store_entries = c.store_entries;
    want.provenance_files = c.provenance_files;
    want.schema = SchemaCheck::Ok;
    want.fts_secure_delete = CheckState::Ok;
    want.sqlite_check = CheckState::Ok;
    want.fts_integrity = CheckState::Ok;
    want
}

/// `(sha256 hex, byte_len, id)` of every blob row.
fn blob_rows(db: &Path) -> Vec<(String, i64, i64)> {
    let c = read_only(db);
    let mut stmt = c
        .prepare("SELECT lower(hex(sha256)), byte_len, id FROM blob ORDER BY id")
        .expect("prepare");
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows")
}

/// `(verify_state, verified_at_ns)` by blob id.
fn blob_states(db: &Path) -> BTreeMap<i64, (Option<i64>, Option<i64>)> {
    let c = read_only(db);
    let mut stmt = c
        .prepare("SELECT id, verify_state, verified_at_ns FROM blob")
        .expect("prepare");
    stmt.query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows")
}

/// `(depth, outcome, counts)` of every verify run.
fn runs(db: &Path) -> Vec<(i64, Option<i64>, Option<String>)> {
    let c = read_only(db);
    let mut stmt = c
        .prepare("SELECT depth, outcome, counts FROM verify_run ORDER BY id")
        .expect("prepare");
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows")
}

/// The logical snapshot without the columns a full verify writes.
fn snapshot_without_verify_columns(db: &Path) -> Value {
    let mut s = logical_snapshot(db, Scope::WithSessions);
    for row in s["blob"].as_array_mut().expect("blob rows") {
        let o = row.as_object_mut().expect("row");
        o.remove("verified_at_ns");
        o.remove("verify_state");
    }
    s
}

#[test]
fn a_clean_archive_passes_both_depths_and_its_runs_are_recorded() {
    let at = imported("imp-dev1-v1-plain");
    let rows = blob_rows(&at.db());
    assert_eq!(rows.len(), 6);
    let bytes: i64 = rows.iter().map(|r| r.1).sum();
    let before = data_snapshot(&at.tmp, &at.id)
        .into_iter()
        .filter(|(k, _)| k != "db" && k != "wal")
        .collect::<Vec<_>>();
    let logical = snapshot_without_verify_columns(&at.db());

    let mut events = Events::default();
    let quick = verify_with(&at, VerifyDepth::Quick, &mut events);
    assert_eq!(quick.outcome, VerifyOutcome::Ok, "{:?}", quick.counts);
    assert_eq!(quick.counts, clean(&quick.counts));
    assert_eq!(quick.counts.blobs, 6);
    assert_eq!(quick.counts.blobs_hashed, 0);
    // Six store files, their fan-out directories and `.incoming`.
    assert!(quick.counts.store_entries > 6, "{:?}", quick.counts);
    assert_eq!(quick.counts.provenance_files, 1);
    assert!(quick.run.is_some());
    assert_eq!(
        (quick.started_at_ns, quick.ended_at_ns, quick.depth),
        (NOW, NOW, VerifyDepth::Quick)
    );
    // The phases in order, and the files counted in rows.
    let phases: Vec<VerifyPhase> = events.0.iter().map(|e| e.0).collect();
    let first = |p| phases.iter().position(|q| *q == p).expect("phase");
    assert!(first(VerifyPhase::CheckingDatabase) < first(VerifyPhase::CheckingIndex));
    assert!(first(VerifyPhase::CheckingIndex) < first(VerifyPhase::CheckingFiles));
    assert_eq!(
        events.0.last(),
        Some(&(VerifyPhase::CheckingFiles, 6, Some(6)))
    );
    // A quick verify sets no blob state.
    assert!(blob_states(&at.db()).values().all(|s| *s == (None, None)));

    let mut events = Events::default();
    let full = verify_with(&at, VerifyDepth::Full, &mut events);
    assert_eq!(full.outcome, VerifyOutcome::Ok, "{:?}", full.counts);
    assert_eq!(full.counts, clean(&full.counts));
    assert_eq!(full.counts.blobs_hashed, 6);
    assert_eq!(
        full.counts.bytes_hashed,
        u64::try_from(bytes).expect("bytes")
    );
    let total = Some(full.counts.bytes_hashed);
    assert_eq!(
        events.0.last(),
        Some(&(VerifyPhase::CheckingFiles, full.counts.bytes_hashed, total))
    );
    assert!(blob_states(&at.db())
        .values()
        .all(|s| *s == (Some(0), Some(NOW))));

    // Recorded, readable back, and nothing else written.
    let recorded = runs(&at.db());
    assert_eq!(
        recorded.iter().map(|r| (r.0, r.1)).collect::<Vec<_>>(),
        vec![(0, Some(0)), (1, Some(0))]
    );
    let stored: Value =
        serde_json::from_str(recorded[1].2.as_deref().expect("counts")).expect("json");
    assert_eq!(stored["blobsHashed"], 6);
    let reader = at.lib.open_read(&at.id).expect("reader");
    let last = reader.last_verify().expect("last").expect("a run");
    assert_eq!(Some(last.run), full.run);
    assert_eq!(last.depth, VerifyDepth::Full);
    assert_eq!(last.outcome, Some(VerifyOutcome::Ok));
    assert_eq!(last.ended_at_ns, Some(NOW));
    assert_eq!(last.counts.as_ref(), Some(&full.counts));
    drop(reader);
    let after = data_snapshot(&at.tmp, &at.id)
        .into_iter()
        .filter(|(k, _)| k != "db" && k != "wal")
        .collect::<Vec<_>>();
    assert_eq!(before, after, "a verify changed the store or imports/");
    assert_eq!(snapshot_without_verify_columns(&at.db()), logical);
    assert_consistent(&at.db());
}

#[test]
fn store_damage_and_leftovers_are_reported_by_kind_and_left_alone() {
    let at = imported("imp-dev1-v1-plain");
    let rows = blob_rows(&at.db());
    let (missing, short, flipped) = (&rows[0], &rows[1], &rows[2]);
    fs::remove_file(at.file(&missing.0)).expect("rm");
    let bytes = fs::read(at.file(&short.0)).expect("read");
    fs::write(at.file(&short.0), &bytes[1..]).expect("truncate");
    let mut bytes = fs::read(at.file(&flipped.0)).expect("read");
    bytes[0] ^= 0xff;
    fs::write(at.file(&flipped.0), &bytes).expect("flip");
    // An orphan with a valid name, a misnamed file, a directory below a
    // fan-out directory, a partial file, provenance leftovers and an
    // intent.
    let orphan = hex(&Sha256::digest(b"an orphan"));
    let hh = store_hh(&at.dir(), &orphan[..2]);
    let orphan_path = hh.join(&orphan);
    fs::write(&orphan_path, b"an orphan").expect("orphan");
    fs::write(at.att().join("notes.txt"), b"x").expect("misnamed");
    let deep = hh.join("deeper");
    fs::create_dir(&deep).expect("deep");
    fs::write(deep.join("hidden"), b"never looked at").expect("hidden");
    let partial = at.att().join(".incoming").join("7.partial");
    fs::write(&partial, b"partial").expect("partial");
    let imports = at.dir().join("imports");
    fs::write(imports.join("session-99.json"), b"{not json").expect("bad provenance");
    fs::write(imports.join("session-1.json.tmp"), b"{}").expect("tmp");
    fs::write(imports.join("README"), b"x").expect("misnamed provenance");
    // A valid provenance file's bytes under a non-canonical spelling of its
    // number: an entry the store never writes, so it is not readable
    // provenance (it is not read as `session-1.json`).
    fs::copy(
        imports.join("session-1.json"),
        imports.join("session-0001.json"),
    )
    .expect("planted provenance");
    #[cfg(unix)]
    for p in [&orphan_path, &partial, &deep.join("hidden")] {
        chmod(p, 0o600);
    }
    {
        let c = raw(&at.db());
        c.execute(
            "INSERT INTO blob_intent (sha256, session_id) VALUES (zeroblob(32), 1)",
            [],
        )
        .expect("intent");
    }
    // A link to a file outside the archive, at a store name.
    #[cfg(unix)]
    let outside = {
        let outside = at.tmp.path().join("outside");
        fs::write(&outside, b"outside").expect("outside");
        let name = hex(&Sha256::digest(b"outside"));
        let hh = store_hh(&at.dir(), &name[..2]);
        std::os::unix::fs::symlink(&outside, hh.join(&name)).expect("link");
        outside
    };
    let tree_before = tree(&at.dir().join("attachments"));
    let imports_before = tree(&imports);

    let quick = verify(&at, VerifyDepth::Quick);
    assert_eq!(quick.outcome, VerifyOutcome::Problems);
    let c = &quick.counts;
    assert_eq!(
        (c.blob_missing, c.blob_length_mismatch, c.blob_hash_mismatch),
        (1, 1, 0),
        "{c:?}"
    );
    assert_eq!((c.blob_bad_name, c.blob_not_regular), (0, 0));
    assert_eq!(c.store_orphans, 1);
    assert_eq!(c.store_unexpected, 2, "notes.txt and the deep directory");
    assert_eq!(c.store_links, u64::from(cfg!(unix)));
    assert_eq!(c.partial_files, 1);
    assert_eq!(c.blob_intents, 1);
    assert_eq!(
        (
            c.provenance_files,
            c.provenance_unreadable,
            c.provenance_tmp_files
        ),
        (1, 3, 1)
    );
    assert!(!c.store_walk_truncated);
    assert_eq!(
        (c.sqlite_check, c.fts_integrity),
        (CheckState::Ok, CheckState::Ok)
    );
    assert_eq!(c.not_private, 0, "a quick verify checks no modes");

    // The full verify finds the flipped byte too, and records each state.
    #[cfg(unix)]
    chmod(&orphan_path, 0o644);
    let full = verify(&at, VerifyDepth::Full);
    let c = &full.counts;
    assert_eq!(full.outcome, VerifyOutcome::Problems);
    assert_eq!(
        (c.blob_missing, c.blob_length_mismatch, c.blob_hash_mismatch),
        (1, 1, 1),
        "{c:?}"
    );
    assert_eq!(c.blobs_hashed, 4);
    assert_eq!(c.not_private, u64::from(cfg!(unix)));
    let states = blob_states(&at.db());
    assert_eq!(states[&missing.2], (Some(1), Some(NOW)));
    assert_eq!(states[&short.2], (Some(3), Some(NOW)));
    assert_eq!(states[&flipped.2], (Some(4), Some(NOW)));
    assert_eq!(
        states.values().filter(|s| s.0 == Some(0)).count(),
        3,
        "{states:?}"
    );

    // Report-only: nothing was swept, repaired or followed.
    #[cfg(unix)]
    chmod(&orphan_path, 0o600);
    assert_eq!(tree(&at.dir().join("attachments")), tree_before);
    assert_eq!(tree(&imports), imports_before);
    #[cfg(unix)]
    assert_eq!(fs::read(&outside).expect("outside"), b"outside");
    let intents: i64 = read_only(&at.db())
        .query_row("SELECT count(*) FROM blob_intent", [], |r| r.get(0))
        .expect("intents");
    assert_eq!(intents, 1);
}

#[test]
fn database_damage_is_reported_by_kind() {
    let at = imported("imp-dev1-v1-plain");
    let running;
    {
        let c = raw(&at.db());
        // A foreign key that points nowhere.
        c.execute(
            "INSERT INTO message_attachment (message_id, attachment_id, ord_rank, ord_rowid, \
                 first_session_id, last_session_id, last_rank) \
             VALUES ((SELECT min(id) FROM message), 999999, zeroblob(41), 1, 1, 1, zeroblob(41))",
            [],
        )
        .expect("dangling link");
        // Value detail about rows that do not exist.
        c.execute(
            "INSERT INTO value_issue (entity, row_id, col, kind) VALUES (3, 999999, 1, 1)",
            [],
        )
        .expect("issue");
        c.execute(
            "INSERT INTO value_issue (entity, row_id, col, kind) VALUES (42, 1, 1, 1)",
            [],
        )
        .expect("unknown entity");
        c.execute(
            "INSERT INTO value_history (entity, row_id, col, value_hash, first_rank, \
                 first_session_id) VALUES (1, 999999, 1, zeroblob(32), zeroblob(41), 1)",
            [],
        )
        .expect("history");
        // An invalid label, rules above the high-water mark, a session a
        // crash left running, secure-delete off, and statistics tables.
        c.execute("UPDATE archive_meta SET label = 'bad' || char(7)", [])
            .expect("label");
        c.execute(
            "UPDATE import_session SET deriver_version = 5 WHERE id = 1",
            [],
        )
        .expect("rules");
        running = insert_session(&c, 0);
        c.execute(
            "INSERT INTO message_fts(message_fts, rank) VALUES('secure-delete', 0)",
            [],
        )
        .expect("secure-delete off");
        c.execute_batch("ANALYZE").expect("analyze");
    }
    let r = verify(&at, VerifyDepth::Quick);
    let c = &r.counts;
    assert_eq!(r.outcome, VerifyOutcome::Problems, "{c:?}");
    assert_eq!(c.schema, SchemaCheck::Ok);
    assert!(c.stat_tables);
    assert_eq!(c.foreign_key_violations, 1);
    assert_eq!(
        c.foreign_key_by_table,
        BTreeMap::from([("message_attachment".to_owned(), 1)])
    );
    assert_eq!((c.value_issue_orphans, c.value_history_orphans), (2, 1));
    assert!(c.label_unreadable);
    assert_eq!(c.rules_above_high_water, 1);
    assert_eq!(c.sessions_running, 1);
    assert_eq!(c.fts_secure_delete, CheckState::Failed);
    assert_eq!(c.sqlite_check, CheckState::Ok);
    assert_eq!(c.fts_integrity, CheckState::Ok);
    // Recovery was skipped: the running session is still running.
    let state: i64 = read_only(&at.db())
        .query_row(
            "SELECT state FROM import_session WHERE id = ?1",
            [running],
            |r| r.get(0),
        )
        .expect("state");
    assert_eq!(state, 0);
    // The statistics alone are not a problem.
    let fresh = imported("imp-fts-plain");
    raw(&fresh.db()).execute_batch("ANALYZE").expect("analyze");
    let r = verify(&fresh, VerifyDepth::Full);
    assert!(r.counts.stat_tables);
    assert_eq!(r.outcome, VerifyOutcome::Ok, "{:?}", r.counts);
}

/// FTS drift: a body changed with the triggers off (spec §18.6, "drift is
/// reported by the verifier"). Dropping a trigger itself changes the
/// schema, which the fingerprint refuses at open.
#[test]
fn fts_drift_is_reported_by_both_depths() {
    let at = imported("imp-fts-plain");
    {
        let c = raw(&at.db());
        c.set_db_config(
            rusqlite::config::DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER,
            false,
        )
        .expect("triggers off");
        let n = c
            .execute(
                "UPDATE message_body SET display_text = 'drifted words' WHERE id = \
                     (SELECT min(id) FROM message_body WHERE display_text IS NOT NULL)",
                [],
            )
            .expect("drift");
        assert_eq!(n, 1);
    }
    for depth in [VerifyDepth::Quick, VerifyDepth::Full] {
        let r = verify(&at, depth);
        assert_eq!(r.outcome, VerifyOutcome::Problems, "{depth:?}");
        assert_eq!(r.counts.fts_integrity, CheckState::Failed, "{depth:?}");
        // Observed on the bundled SQLite: `quick_check` and
        // `integrity_check` do not compare an external-content index with
        // its content, so only the rank-1 check sees the drift.
        assert_eq!(r.counts.sqlite_check, CheckState::Ok, "{depth:?}");
    }
}

/// A crafted file that bypasses the `storage_name` CHECK (spec §18.7): the
/// parser refuses the name, the verifier reports it and `open_blob` refuses
/// it; no path outside the archive is touched.
#[test]
fn a_crafted_storage_name_is_reported_and_refused() {
    let at = imported("imp-fts-plain");
    let ids: Vec<i64> = {
        let c = raw(&at.db());
        c.pragma_update(None, "ignore_check_constraints", true)
            .expect("checks off");
        let mut ids = Vec::new();
        for (sha, name) in [
            ([0xab_u8; 32], "../".repeat(21) + "a"),
            ([0xcd_u8; 32], "ab".repeat(32)),
        ] {
            c.execute(
                "INSERT INTO blob (sha256, byte_len, storage_name, first_session_id) \
                 VALUES (?1, 1, ?2, 1)",
                rusqlite::params![sha.as_slice(), name],
            )
            .expect("crafted row");
            ids.push(c.last_insert_rowid());
        }
        ids
    };
    let before = tree(at.tmp.path());
    let r = verify(&at, VerifyDepth::Full);
    assert_eq!(r.counts.blob_bad_name, 2, "{:?}", r.counts);
    assert_eq!(r.outcome, VerifyOutcome::Problems);
    let states = blob_states(&at.db());
    assert!(ids.iter().all(|id| states[id].0 == Some(6)));
    let reader = at.lib.open_read(&at.id).expect("reader");
    for id in ids {
        let e = reader.open_blob(BlobId::new(id)).expect_err("refused");
        assert!(
            matches!(e, ArchiveError::Blob(BlobProblem::BadName)),
            "{e:?}"
        );
    }
    drop(reader);
    let after = tree(at.tmp.path());
    let changed: Vec<&String> = after
        .iter()
        .filter(|(k, v)| before.get(*k) != Some(*v))
        .map(|(k, _)| k)
        .filter(|k| !k.ends_with("archive.sqlite") && !k.contains("archive.sqlite-"))
        .collect();
    assert!(changed.is_empty(), "{changed:?}");
}

#[test]
fn open_blob_hands_out_only_checked_and_rehashed_bytes() {
    let at = imported("imp-dev1-v1-plain");
    let rows = blob_rows(&at.db());
    // While an import-like holder has the lock, the lock-free reader reads.
    let a = at.lib.open(&at.id).expect("open");
    let reader = at.lib.open_read(&at.id).expect("reader");
    for (sha, len, id) in &rows {
        for r in [
            reader.open_blob(BlobId::new(*id)).expect("open_blob"),
            a.open_blob(BlobId::new(*id)).expect("open_blob"),
        ] {
            let mut r = r;
            assert_eq!(r.len(), u64::try_from(*len).expect("len"));
            let mut bytes = Vec::new();
            r.read_to_end(&mut bytes).expect("read");
            assert_eq!(&hex(&Sha256::digest(&bytes)), sha);
            // Reads after the end stay at the end.
            assert_eq!(r.read(&mut [0u8; 8]).expect("eof"), 0);
        }
    }
    let unknown = reader.open_blob(BlobId::new(999_999)).expect_err("none");
    assert!(matches!(unknown, ArchiveError::Blob(BlobProblem::NotFound)));

    // Damage: same length but different bytes, shorter, missing.
    let (flipped, short, missing) = (&rows[0], &rows[1], &rows[2]);
    let mut bytes = fs::read(at.file(&flipped.0)).expect("read");
    let last = bytes.len() - 1;
    bytes[last] ^= 0x01;
    fs::write(at.file(&flipped.0), &bytes).expect("flip");
    let mut r = reader.open_blob(BlobId::new(flipped.2)).expect("opens");
    let mut out = Vec::new();
    let e = r.read_to_end(&mut out).expect_err("mismatch at the end");
    assert_eq!(e.kind(), std::io::ErrorKind::InvalidData);
    assert!(r.read(&mut [0u8; 8]).is_err(), "the failure sticks");
    let b = fs::read(at.file(&short.0)).expect("read");
    fs::write(at.file(&short.0), &b[..b.len() - 1]).expect("truncate");
    let e = reader.open_blob(BlobId::new(short.2)).expect_err("short");
    assert!(matches!(e, ArchiveError::Blob(BlobProblem::LengthMismatch)));
    fs::remove_file(at.file(&missing.0)).expect("rm");
    let e = reader
        .open_blob(BlobId::new(missing.2))
        .expect_err("missing");
    assert!(matches!(e, ArchiveError::Blob(BlobProblem::Missing)));
    #[cfg(unix)]
    {
        // A link in place of the file is refused; its target is untouched.
        let outside = at.tmp.path().join("outside");
        fs::write(&outside, b"outside").expect("outside");
        std::os::unix::fs::symlink(&outside, at.file(&missing.0)).expect("link");
        let e = reader.open_blob(BlobId::new(missing.2)).expect_err("link");
        assert!(matches!(e, ArchiveError::Blob(BlobProblem::NotRegular)));
        assert_eq!(fs::read(&outside).expect("outside"), b"outside");
    }
    drop(a);
}

#[test]
fn a_cancelled_verify_is_recorded_as_cancelled_and_sets_no_state() {
    let at = imported("imp-dev1-v1-plain");
    // Cancelled when the files start: the database checks ran.
    let token = CancelToken::new();
    let mut sink = CancelAt(VerifyPhase::CheckingFiles, token.clone());
    let mut a = at.lib.open(&at.id).expect("open");
    let r = verify_on(&mut a, VerifyDepth::Full, &mut sink, &token);
    assert_eq!(r.outcome, VerifyOutcome::Cancelled);
    assert_eq!(r.counts.sqlite_check, CheckState::Ok);
    assert_eq!(r.counts.blobs_hashed, 0);
    assert!(r.run.is_some());
    // Cancelled before it starts.
    let r = verify_on(&mut a, VerifyDepth::Quick, &mut NoProgress, &token);
    assert_eq!(r.outcome, VerifyOutcome::Cancelled);
    assert_eq!(r.counts.sqlite_check, CheckState::NotRun);
    drop(a);
    assert_eq!(
        runs(&at.db())
            .iter()
            .map(|r| (r.0, r.1))
            .collect::<Vec<_>>(),
        vec![(1, Some(2)), (0, Some(2))]
    );
    assert!(blob_states(&at.db()).values().all(|s| *s == (None, None)));
    let last = at
        .lib
        .open_read(&at.id)
        .expect("reader")
        .last_verify()
        .expect("last")
        .expect("run");
    assert_eq!(last.outcome, Some(VerifyOutcome::Cancelled));
}

/// A wide `attachments/` (spec §18.7): the walk stops at `blobs + 10,000`
/// entries and says so.
#[test]
fn the_store_walk_is_bounded_and_reports_truncation() {
    let at = imported("imp-fts-plain");
    let junk = at.att().join("junk");
    fs::create_dir(&junk).expect("junk");
    let wide = store_hh(&at.dir(), "0f");
    let limit = STORE_WALK_SLACK + 1;
    for n in 0..limit {
        fs::write(wide.join(format!("x{n}")), b"").expect("file");
    }
    let r = verify(&at, VerifyDepth::Quick);
    assert!(r.counts.store_walk_truncated, "{:?}", r.counts);
    assert_eq!(r.counts.blobs, 0);
    assert_eq!(r.counts.store_entries, STORE_WALK_SLACK);
    assert_eq!(r.outcome, VerifyOutcome::Problems);
}

#[test]
fn an_archive_that_needs_a_migration_is_not_verified() {
    let (_tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let _faults = install(Faults {
        schema: Some(TestSchema::V2),
        ..Faults::default()
    });
    let mut a = lib.open(&id).expect("open");
    let e = a
        .verify(
            VerifyDepth::Quick,
            &mut ArchiveCtx {
                cancel: &CancelToken::new(),
                progress: &mut NoProgress,
                clock: &FixedClock(NOW),
            },
        )
        .expect_err("needs migration");
    assert!(matches!(e, ArchiveError::NeedsMigration { found: 1 }));
}

/// A fresh archive with no import verifies clean, and its reader reports
/// no run before the first one.
#[test]
fn an_empty_archive_verifies_clean() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    assert!(lib
        .open_read(&id)
        .expect("reader")
        .last_verify()
        .expect("last")
        .is_none());
    let at = Imported { tmp, lib, id };
    for depth in [VerifyDepth::Quick, VerifyDepth::Full] {
        let r = verify(&at, depth);
        assert_eq!(r.outcome, VerifyOutcome::Ok, "{:?}", r.counts);
        assert_eq!(r.counts.provenance_files, 0);
    }
}

/// A damaged index page (spec §11): `quick_check` and `integrity_check`
/// both report it, as a corruption error or as rows other than `ok`.
#[test]
fn a_damaged_page_is_reported_by_both_sqlite_checks() {
    let at = imported("imp-dev1-v1-plain");
    let (page, page_size): (i64, i64) = {
        let c = read_only(&at.db());
        let page = c
            .query_row(
                "SELECT pageno FROM dbstat WHERE name = 'message_sort' ORDER BY pageno LIMIT 1",
                [],
                |r| r.get(0),
            )
            .expect("an index page");
        let size = c
            .query_row("PRAGMA page_size", [], |r| r.get(0))
            .expect("page size");
        (page, size)
    };
    assert!(page > 1);
    // Byte 0 of a b-tree page is its type; 0xff is none of them.
    let mut bytes = fs::read(at.db()).expect("read");
    let at_byte = usize::try_from((page - 1) * page_size).expect("offset");
    bytes[at_byte] = 0xff;
    fs::write(at.db(), &bytes).expect("damage");
    for depth in [VerifyDepth::Quick, VerifyDepth::Full] {
        let r = verify(&at, depth);
        assert_eq!(r.counts.sqlite_check, CheckState::Failed, "{depth:?}");
        assert!(r.counts.sqlite_check_messages >= 1);
        assert_eq!(r.outcome, VerifyOutcome::Problems);
    }
}

/// A `blob` read that fails is a check not run, never a store full of
/// orphans: with no row count the walk is not run either; with a page
/// missing, the walk calls no file an orphan (and its limit comes from the
/// row count).
#[test]
fn a_failed_blob_read_reports_no_orphans() {
    let at = imported("imp-dev1-v1-plain");
    for (step, not_run, entries) in [(0u32, 2u64, false), (1, 1, true)] {
        let r = {
            let _g = install(Faults {
                verify_blob_error: Some(step),
                ..Faults::default()
            });
            verify(&at, VerifyDepth::Quick)
        };
        let c = &r.counts;
        assert_eq!(r.outcome, VerifyOutcome::Incomplete, "step {step}: {c:?}");
        assert_eq!(c.checks_not_run, not_run, "step {step}");
        assert_eq!(c.store_orphans, 0, "step {step}");
        assert!(!c.store_walk_truncated, "step {step}");
        assert_eq!(c.store_entries > 0, entries, "step {step}");
    }
    // Without the fault the same archive is clean.
    assert_eq!(verify(&at, VerifyDepth::Quick).outcome, VerifyOutcome::Ok);
}
