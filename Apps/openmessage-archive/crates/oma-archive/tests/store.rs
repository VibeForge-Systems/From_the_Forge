//! The content-addressed attachment store (spec §9, §18.5): the copies
//! (plaintext hashes, encrypted backups included), every counted outcome,
//! the store states a later import meets (a deleted, a damaged, a matching
//! and a mismatching orphan file, links), the intents after a crash between
//! placement and the row commit, a source that changes during a copy, and
//! a cancel inside the copy.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::Arc;

use common::import::*;
use common::*;
use oma_archive::hooks::{arm, install, FaultPoint, Faults};
use oma_archive::{
    CancelToken, DirSync, ImportOutcome, Library, LineageChoice, Placed, RealFs, Recovery,
    StopReason, StoreFs,
};
use sha2::{Digest, Sha256};

/// The import of one entry into a fresh archive.
struct Imported {
    tmp: tempfile::TempDir,
    lib: Library,
    id: oma_archive::ArchiveId,
}

impl Imported {
    fn db(&self) -> std::path::PathBuf {
        db_path(&self.tmp, &self.id)
    }
    fn dir(&self) -> std::path::PathBuf {
        archive_dir(&self.tmp, &self.id)
    }
}

fn fresh() -> Imported {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    Imported { tmp, lib, id }
}

fn import_entry(at: &Imported, entry: &str) -> oma_archive::ImportSummary {
    let mut a = at.lib.open(&at.id).expect("open");
    let src = open_entry(entry);
    let s = import(&mut a, &src, &opts(1000), LineageChoice::New);
    drop(a);
    assert!(src.ob.close().removed);
    s
}

/// The rows of `attachment_observation` per copy code.
fn copy_codes(db: &Path) -> BTreeMap<i64, i64> {
    let c = read_only(db);
    let mut stmt = c
        .prepare("SELECT copy, count(*) FROM attachment_observation GROUP BY copy")
        .expect("prepare");
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows")
}

fn store_path(archive: &Path, sha: &str) -> std::path::PathBuf {
    archive.join("attachments").join(&sha[..2]).join(sha)
}

/// The catalogue's blob digests of an entry.
fn blobs_of(entry: &str) -> BTreeSet<String> {
    let m = catalogue_json(&format!("{entry}.expected/content.json"));
    m["content"]["blobs"]
        .as_array()
        .expect("blobs")
        .iter()
        .map(|b| b["sha256"].as_str().expect("sha").to_owned())
        .collect()
}

/// An encrypted backup's attachments are stored as their plaintexts: the
/// store's files are exactly the generator's `.expected/attachments/`
/// plaintexts, under their SHA-256 names.
#[test]
fn encrypted_attachments_are_stored_as_their_plaintexts() {
    let at = fresh();
    let s = import_entry(&at, "imp-dev1-v2-enc");
    let plain_dir = imports_root()
        .join("imp-dev1-v2-enc.expected")
        .join("attachments");
    let mut want = BTreeSet::new();
    for e in fs::read_dir(&plain_dir).expect("dir").flatten() {
        let bytes = fs::read(e.path()).expect("plaintext");
        let sha = hex(&Sha256::digest(&bytes));
        assert_eq!(
            fs::read(store_path(&at.dir(), &sha)).expect("stored"),
            bytes,
            "the stored file is the plaintext"
        );
        want.insert(sha);
    }
    let got: BTreeSet<String> = store_files(&at.db())
        .into_values()
        .collect::<BTreeSet<String>>();
    assert_eq!(got, want);
    assert_eq!(s.counts.attachment_files.blobs_added, want.len() as u64);
    // The estimate used the Manifest `Size` of each encrypted file, so it
    // is exact here (the same bytes under two GUIDs are copied twice and
    // counted twice).
    assert_eq!(
        s.estimate.bytes_to_copy,
        s.counts.attachment_files.bytes_copied
    );
    assert_store_consistent(&at.db(), true);
}

/// Every outcome of the attachment entry is counted once, and the
/// observation rows say the same.
#[test]
fn every_outcome_is_counted_and_observed() {
    let at = fresh();
    let s = import_entry(&at, "imp-attach-plain");
    let f = s.counts.attachment_files;
    // B001, B003 (Size mismatch, stored anyway), B005 (StickerCache) and
    // one more file are copied; B007 has B001's bytes; B002's blob is
    // missing; one file is not in the backup; three names are not looked
    // up (an unusual name, a class not looked up, a NULL filename).
    assert_eq!(
        (
            f.copied,
            f.deduplicated,
            f.blob_unavailable,
            f.not_in_backup,
            f.not_looked_up
        ),
        (4, 1, 1, 1, 3),
        "{f:?}"
    );
    assert_eq!(f.size_mismatch, 1);
    assert_eq!(f.outcomes(), s.counts.attachments.seen);
    assert_eq!(f.bytes_copied, 5 * 32);
    assert_eq!(f.partials_left, 0);
    // One flush per file, and per touched directory.
    assert!(f.fsyncs > 5, "{f:?}");
    let codes = copy_codes(&at.db());
    use oma_archive::schema::codes::CopyCode as C;
    let n = |c: C| codes.get(&c.code()).copied().unwrap_or(0) as u64;
    assert_eq!(
        (
            n(C::Copied),
            n(C::Deduplicated),
            n(C::BlobUnavailable),
            n(C::NotInBackup),
            n(C::NotLookedUp)
        ),
        (4, 1, 1, 1, 3)
    );
    let c = read_only(&at.db());
    let mismatched: i64 = c
        .query_row(
            "SELECT count(*) FROM attachment_observation WHERE size_matched_manifest = 0",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(mismatched, 1);
    // Rows that read no bytes name no blob.
    let without: i64 = c
        .query_row(
            "SELECT count(*) FROM attachment_observation WHERE blob_id IS NULL",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(without, 5);
}

/// A second import of the same backup reads no file again: every file is
/// present with its length (spec §9.2), and the store is unchanged.
#[test]
fn a_second_import_reads_no_archived_file_again() {
    let at = fresh();
    import_entry(&at, "imp-dev1-v1-plain");
    let before = store_files(&at.db());
    let s = import_entry(&at, "imp-dev1-v1-plain");
    let f = s.counts.attachment_files;
    assert_eq!(
        (f.already_archived, f.copied, f.bytes_copied),
        (6, 0, 0),
        "{f:?}"
    );
    assert_eq!(s.estimate.bytes_to_copy, 0);
    assert_eq!(s.estimate.files_to_copy, 0);
    assert_eq!(store_files(&at.db()), before);
}

/// With copying off, no file is looked up or read.
#[test]
fn copying_off_reads_no_file() {
    let at = fresh();
    let mut a = at.lib.open(&at.id).expect("open");
    let src = open_entry("imp-dev1-v1-plain");
    let mut o = opts(1000);
    o.copy_attachments = false;
    let s = import(&mut a, &src, &o, LineageChoice::New);
    drop(a);
    let f = s.counts.attachment_files;
    assert_eq!(f.not_requested, s.counts.attachments.seen);
    assert_eq!(f.blobs_added, 0);
    assert_eq!(s.estimate.files_to_copy, 0);
    assert!(store_files(&at.db()).is_empty());
}

/// The store states a later import meets (spec §18.5): a referenced file
/// deleted on disk is placed again; a referenced file with the wrong
/// length is a conflict and is left untouched; an unreferenced file that
/// matches its name is adopted; one that does not is replaced.
#[test]
fn store_states_are_repaired_or_reported() {
    // The bytes v2 adds, taken from another archive's store.
    let scratch = fresh();
    import_entry(&scratch, "imp-dev1-v2-plain");
    let v1 = blobs_of("imp-dev1-v1-plain");
    let v2 = blobs_of("imp-dev1-v2-plain");
    let only_v2: Vec<&String> = v2.difference(&v1).collect();
    assert_eq!(only_v2.len(), 2);

    let at = fresh();
    import_entry(&at, "imp-dev1-v1-plain");
    let dir = at.dir();
    let deleted = "eb895324"; // attachment 0001, the same bytes in v1 and v2
    let damaged = "faec9453"; // attachment 0008, the same bytes in v1 and v2
    let full = |prefix: &str| -> String {
        v1.iter()
            .find(|s| s.starts_with(prefix))
            .expect("in v1")
            .clone()
    };
    let (deleted, damaged) = (full(deleted), full(damaged));
    fs::remove_file(store_path(&dir, &deleted)).expect("delete");
    let mut bad = fs::read(store_path(&dir, &damaged)).expect("read");
    bad.extend_from_slice(b"+damage");
    fs::write(store_path(&dir, &damaged), &bad).expect("damage");
    // The orphans: one with the right bytes, one with other bytes.
    let (matching, mismatching) = (only_v2[0], only_v2[1]);
    let hh = |sha: &str| store_hh(&dir, &sha[..2]);
    hh(matching);
    hh(mismatching);
    fs::copy(
        store_path(&scratch.dir(), matching),
        store_path(&dir, matching),
    )
    .expect("plant");
    fs::write(store_path(&dir, mismatching), b"not the bytes").expect("plant");

    let s = import_entry(&at, "imp-dev1-v2-plain");
    let f = s.counts.attachment_files;
    assert_eq!(
        (
            f.replaced,
            f.store_conflicts,
            f.adopted,
            f.orphan_replaced,
            f.deduplicated,
            f.already_archived,
            f.blob_unavailable,
            f.copied
        ),
        (1, 1, 1, 1, 1, 1, 1, 0),
        "{f:?}"
    );
    // The deleted file is back; the damaged one is untouched; both orphans
    // now hold their bytes.
    for sha in [&deleted, matching, mismatching] {
        let bytes = fs::read(store_path(&dir, sha)).expect("read");
        assert_eq!(&hex(&Sha256::digest(&bytes)), sha);
    }
    assert_eq!(fs::read(store_path(&dir, &damaged)).expect("read"), bad);
    // The rows match the model's (the same as an import of v1 then v2).
    let m = catalogue_json("scenarios/dev1-v1-v2.content.json");
    assert_eq!(blob_list(&at.db()), m["content"]["blobs"]);
}

/// A link anywhere in the store is never followed, and nothing is written
/// outside the archive (spec §18.5).
#[cfg(unix)]
#[test]
fn links_in_the_store_are_never_followed() {
    let at = fresh();
    import_entry(&at, "imp-dev1-v1-plain");
    let dir = at.dir();
    let outside = tempfile::tempdir().expect("outside");
    let target = outside.path().join("target");
    fs::write(&target, b"outside the archive").expect("target");
    let v1: Vec<String> = blobs_of("imp-dev1-v1-plain").into_iter().collect();
    // A referenced file replaced by a link to a file outside.
    let linked = &v1[0];
    fs::remove_file(store_path(&dir, linked)).expect("rm");
    std::os::unix::fs::symlink(&target, store_path(&dir, linked)).expect("link");
    // A `<hh>` directory replaced by a link to a directory outside.
    let moved = &v1[1];
    assert_ne!(moved[..2], linked[..2]);
    let hh_dir = dir.join("attachments").join(&moved[..2]);
    fs::remove_dir_all(&hh_dir).expect("rm hh");
    std::os::unix::fs::symlink(outside.path(), &hh_dir).expect("link hh");

    let s = import_entry(&at, "imp-dev1-v1-plain");
    let f = s.counts.attachment_files;
    assert_eq!(f.store_conflicts, 2, "{f:?}");
    assert_eq!(fs::read(&target).expect("target"), b"outside the archive");
    let names: Vec<String> = fs::read_dir(outside.path())
        .expect("dir")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["target"]);
}

/// Every copy failing on `EFBIG` is counted per file, and the import goes
/// on (spec §12 item 5).
#[test]
fn a_file_too_large_for_the_volume_is_counted_and_the_import_continues() {
    let code = if cfg!(windows) { 223 } else { 27 };
    let _g = install(Faults {
        copy_write_error: Some(code),
        ..Faults::default()
    });
    let at = fresh();
    let s = import_entry(&at, "imp-dev1-v1-plain");
    let f = s.counts.attachment_files;
    assert_eq!((f.too_large_for_volume, f.blobs_added), (6, 0), "{f:?}");
    assert_eq!(partial_files(&at.db()), 0);
    assert!(store_files(&at.db()).is_empty());
    assert_store_consistent(&at.db(), true);
}

/// Files over `Limits::attachment_bytes` are never read.
#[test]
fn a_file_over_the_limit_is_not_read() {
    let at = fresh();
    let limits = oma_backup::Limits {
        attachment_bytes: 26,
        ..oma_backup::Limits::default()
    };
    let mut a = at.lib.open(&at.id).expect("open");
    let src = open_dir_with(
        &imports_root().join("imp-dev1-v1-plain").join(UDID),
        None,
        &limits,
    );
    let s = import(&mut a, &src, &opts(1000), LineageChoice::New);
    drop(a);
    let f = s.counts.attachment_files;
    // The files are 26, 26, 26, 26, 28 and 29 bytes long.
    assert_eq!((f.over_limit, f.copied), (2, 4), "{f:?}");
    let c = read_only(&at.db());
    let max: i64 = c
        .query_row("SELECT max(byte_len) FROM blob", [], |r| r.get(0))
        .expect("max");
    assert!(max <= 26);
}

/// A store whose removals fail, as a scanner holding a file open makes
/// them fail on Windows.
struct StuckPartials;

impl StoreFs for StuckPartials {
    fn create_dir_durable(&self, path: &Path) -> io::Result<()> {
        RealFs.create_dir_durable(path)
    }
    fn fsync_dir(&self, path: &Path) -> io::Result<DirSync> {
        RealFs.fsync_dir(path)
    }
    fn rename_dir_noreplace(&self, from: &Path, to: &Path) -> io::Result<Placed> {
        RealFs.rename_dir_noreplace(from, to)
    }
    fn place_noreplace(&self, from: &Path, to: &Path) -> io::Result<Placed> {
        RealFs.place_noreplace(from, to)
    }
    fn remove_file(&self, path: &Path) -> io::Result<()> {
        if path.extension().is_some_and(|e| e == "partial") {
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        } else {
            fs::remove_file(path)
        }
    }
}

/// A partial file that cannot be removed is counted, and the next write
/// open sweeps it.
#[test]
fn a_partial_file_that_cannot_be_removed_is_counted_and_swept_later() {
    let _g = install(Faults {
        copy_write_error: Some(5),
        ..Faults::default()
    });
    let (tmp, lib) = library();
    let lib = lib.with_store_fs(Arc::new(StuckPartials));
    let id = lib.create(None).expect("create");
    let mut a = lib.open(&id).expect("open");
    let src = open_entry("imp-dev1-v1-plain");
    let s = import(&mut a, &src, &opts(1000), LineageChoice::New);
    drop(a);
    let db = db_path(&tmp, &id);
    let f = s.counts.attachment_files;
    assert_eq!((f.write_failed, f.partials_left), (6, 6), "{f:?}");
    assert_eq!(partial_files(&db), 6);
    // The next write open, with a store that can remove them, sweeps them.
    let lib = Library::new(tmp.path()).expect("library");
    let mut a = lib.open(&id).expect("open");
    let r = a.begin_write(Recovery::Run).expect("write");
    assert_eq!(r.partials_removed, 6);
    drop(a);
    assert_eq!(partial_files(&db), 0);
}

/// A crash after the files are placed but before their rows commit (an
/// unwinding panic stands in for it in-process; the process kill is in
/// `crash.rs`): the next write open deletes the placed files of the
/// committed intents, and the resume copies them again (spec §18.5).
#[test]
fn intents_remove_placed_files_whose_rows_never_committed() {
    let want = {
        let at = fresh();
        import_entry(&at, "imp-dev1-v1-plain");
        logical_snapshot(&at.db(), Scope::Content)
    };
    let at = fresh();
    let r = {
        let _trip = arm(
            FaultPoint::AttachmentDirSynced,
            1,
            Box::new(|_, _| panic!("crash after placement")),
        );
        catch_unwind(AssertUnwindSafe(|| {
            let mut a = at.lib.open(&at.id).expect("open");
            let src = open_entry("imp-dev1-v1-plain");
            let _ = import_with(
                &mut a,
                &src,
                &opts(1000),
                LineageChoice::New,
                &CancelToken::new(),
            );
        }))
    };
    assert!(r.is_err(), "the crash point was not reached");
    // Placed files with committed intents and no blob rows.
    assert_eq!(store_files(&at.db()).len(), 6);
    let intents: i64 = read_only(&at.db())
        .query_row("SELECT count(*) FROM blob_intent", [], |r| r.get(0))
        .expect("intents");
    assert_eq!(intents, 6);
    let mut a = at.lib.open(&at.id).expect("open");
    let report = a.begin_write(Recovery::Run).expect("write");
    assert_eq!(report.intents_rolled_back, 6);
    drop(a);
    assert!(store_files(&at.db()).is_empty());
    assert_store_consistent(&at.db(), true);
    // The resume copies them again.
    let lineage: i64 = read_only(&at.db())
        .query_row("SELECT max(id) FROM lineage", [], |r| r.get(0))
        .expect("lineage");
    let mut a = at.lib.open(&at.id).expect("open");
    let src = open_entry("imp-dev1-v1-plain");
    let s = import(
        &mut a,
        &src,
        &opts(1000),
        LineageChoice::Existing(oma_archive::LineageId::new(lineage)),
    );
    drop(a);
    assert!(s.resumed);
    assert_eq!(s.counts.attachment_files.copied, 6);
    assert_store_consistent(&at.db(), true);
    assert_eq!(logical_snapshot(&at.db(), Scope::Content), want);
}

/// Set the modification time of every blob in a backup copy except
/// sms.db's, so any file copied afterwards fails its stamp check.
fn touch_attachment_blobs(udid: &Path) {
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
    for d in fs::read_dir(udid).expect("dir").flatten() {
        if !d.file_type().expect("type").is_dir() {
            continue;
        }
        for f in fs::read_dir(d.path()).expect("dir").flatten() {
            if f.file_name() == oma_backup::SMS_DB_FILE_ID {
                continue;
            }
            fs::File::options()
                .write(true)
                .open(f.path())
                .expect("open")
                .set_modified(later)
                .expect("touch");
        }
    }
}

/// A file that changes during its copy stops the import in a resumable
/// state (6) with `source_changed`, after committing the records before
/// it; continuing finishes it (spec §9.3, §18.5).
#[test]
fn a_file_changed_during_its_copy_stops_the_import_and_continuing_finishes() {
    let src_dir = tempfile::tempdir().expect("backup");
    let udid = copy_entry("imp-dev1-v1-plain", src_dir.path());
    let at = fresh();
    let outcome = {
        let u = udid.clone();
        let _trip = arm(
            FaultPoint::AttachmentCopying,
            2,
            Box::new(move |_, _| touch_attachment_blobs(&u)),
        );
        let mut a = at.lib.open(&at.id).expect("open");
        let src = open_dir(&udid, None);
        let o = import_with(
            &mut a,
            &src,
            &opts(1000),
            LineageChoice::New,
            &CancelToken::new(),
        )
        .expect("import");
        drop(a);
        src.ob.close();
        o
    };
    let ImportOutcome::Stopped { reason, summary } = outcome else {
        panic!("not stopped: {outcome:?}");
    };
    assert_eq!(reason, StopReason::SourceChanged);
    // The first copied record is committed with its file.
    assert_eq!(summary.counts.attachment_files.blobs_added, 1);
    assert_eq!(partial_files(&at.db()), 0);
    let c = read_only(&at.db());
    let (state, issue): (i64, i64) = c
        .query_row(
            "SELECT s.state, (SELECT count(*) FROM import_issue i WHERE i.session_id = s.id \
                 AND i.kind = 3) FROM import_session s",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("session");
    assert_eq!((state, issue), (6, 1));
    drop(c);
    assert_store_consistent(&at.db(), true);
    // Continuing (a new open sees the new stamps) finishes the session.
    let lineage = oma_archive::LineageId::new(summary.lineage.get());
    let mut a = at.lib.open(&at.id).expect("open");
    let src = open_dir(&udid, None);
    let s = import(&mut a, &src, &opts(1000), LineageChoice::Existing(lineage));
    drop(a);
    assert!(s.resumed);
    assert_eq!(s.counts.attachment_files.blobs_added, 6);
    let fresh_at = fresh();
    import_entry(&fresh_at, "imp-dev1-v1-plain");
    assert_eq!(
        logical_snapshot(&at.db(), Scope::Content),
        logical_snapshot(&fresh_at.db(), Scope::Content)
    );
}

/// A cancel that `copy_file_to` sees commits the records before it,
/// leaves no partial file, and the resume reaches the uninterrupted result.
#[test]
fn a_cancel_inside_the_copy_commits_the_records_before_it() {
    let at = fresh();
    let token = CancelToken::new();
    let outcome = {
        let t = token.clone();
        let _trip = arm(
            FaultPoint::AttachmentPartialCreated,
            3,
            Box::new(move |_, _| t.cancel()),
        );
        let mut a = at.lib.open(&at.id).expect("open");
        let src = open_entry("imp-dev1-v1-plain");
        let o = import_with(&mut a, &src, &opts(1000), LineageChoice::New, &token).expect("import");
        drop(a);
        o
    };
    let ImportOutcome::Cancelled(Some(summary)) = outcome else {
        panic!("not cancelled: {outcome:?}");
    };
    assert_eq!(summary.counts.attachment_files.blobs_added, 2);
    assert_eq!(partial_files(&at.db()), 0);
    assert_store_consistent(&at.db(), true);
    let mut a = at.lib.open(&at.id).expect("open");
    let src = open_entry("imp-dev1-v1-plain");
    let s = import(
        &mut a,
        &src,
        &opts(1000),
        LineageChoice::Existing(summary.lineage),
    );
    drop(a);
    assert!(s.resumed);
    let fresh_at = fresh();
    import_entry(&fresh_at, "imp-dev1-v1-plain");
    assert_eq!(
        logical_snapshot(&at.db(), Scope::Content),
        logical_snapshot(&fresh_at.db(), Scope::Content)
    );
}

/// The newest session's state and stored `attachmentFiles.partialsLeft`.
fn stored_partials_left(db: &Path) -> (i64, u64) {
    let (state, counts): (i64, String) = read_only(db)
        .query_row(
            "SELECT state, counts FROM import_session ORDER BY id DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("session");
    let v: serde_json::Value = serde_json::from_str(&counts).expect("counts");
    let left = v["attachmentFiles"]["partialsLeft"]
        .as_u64()
        .unwrap_or_else(|| panic!("no partialsLeft in {v}"));
    (state, left)
}

/// Continue the newest session of `at` and compare with the uninterrupted
/// import of `imp-dev1-v1-plain`.
fn resume_to_uninterrupted(at: &Imported, lineage: oma_archive::LineageId) {
    let mut a = at.lib.open(&at.id).expect("open");
    let src = open_entry("imp-dev1-v1-plain");
    let s = import(&mut a, &src, &opts(1000), LineageChoice::Existing(lineage));
    drop(a);
    assert!(s.resumed);
    assert_store_consistent(&at.db(), true);
    let fresh_at = fresh();
    import_entry(&fresh_at, "imp-dev1-v1-plain");
    assert_eq!(
        logical_snapshot(&at.db(), Scope::Content),
        logical_snapshot(&fresh_at.db(), Scope::Content)
    );
}

/// A cancel that lands after a file was copied and synced but before the
/// pass buffered its record: the record is not committed, and its partial
/// file is removed at once rather than left for the next import's sweep.
#[test]
fn a_cancel_after_a_copy_removes_its_partial_file() {
    let at = fresh();
    let token = CancelToken::new();
    let outcome = {
        let t = token.clone();
        let _trip = arm(
            FaultPoint::AttachmentSynced,
            2,
            Box::new(move |_, _| t.cancel()),
        );
        let mut a = at.lib.open(&at.id).expect("open");
        let src = open_entry("imp-dev1-v1-plain");
        let o = import_with(&mut a, &src, &opts(1000), LineageChoice::New, &token).expect("import");
        drop(a);
        o
    };
    let ImportOutcome::Cancelled(Some(summary)) = outcome else {
        panic!("not cancelled: {outcome:?}");
    };
    assert_eq!(summary.counts.attachment_files.blobs_added, 1);
    assert_eq!(summary.counts.attachment_files.partials_left, 0);
    assert_eq!(partial_files(&at.db()), 0, "the copied file was left");
    assert_store_consistent(&at.db(), true);
    resume_to_uninterrupted(&at, summary.lineage);
}

/// Free space that reads low once a flag is set.
struct LowAfter {
    low: std::sync::atomic::AtomicBool,
}

impl oma_archive::space::SpaceProbe for LowAfter {
    fn available(&self, _path: &Path) -> io::Result<u64> {
        let floor = oma_archive::space::reserve_for(None)
            + oma_archive::space::wal_allowance(32 << 20, 64 << 20);
        Ok(if self.low.load(std::sync::atomic::Ordering::SeqCst) {
            floor - 1
        } else {
            TEST_AVAILABLE
        })
    }
    fn total(&self, _path: &Path) -> io::Result<u64> {
        Err(io::Error::other("unknown"))
    }
}

/// The space guard that runs before the intent transaction (phase B)
/// stops the import with a copied file buffered: that file is removed, so
/// the stop gives the space back and leaves no plaintext in `.incoming`.
#[test]
fn a_space_stop_before_the_intents_removes_the_batch_partial_files() {
    let probe = Arc::new(LowAfter {
        low: std::sync::atomic::AtomicBool::new(false),
    });
    let (tmp, lib) = library();
    let lib = lib.with_space_probe(probe.clone());
    let id = lib.create(None).expect("create");
    let outcome = {
        let p = probe.clone();
        let _trip = arm(
            FaultPoint::AttachmentSynced,
            1,
            Box::new(move |_, _| p.low.store(true, std::sync::atomic::Ordering::SeqCst)),
        );
        let mut a = lib.open(&id).expect("open");
        let src = open_entry("imp-dev1-v1-plain");
        let o = import_with(
            &mut a,
            &src,
            &opts(1000),
            LineageChoice::New,
            &CancelToken::new(),
        )
        .expect("import");
        drop(a);
        o
    };
    let ImportOutcome::Stopped { reason, summary } = outcome else {
        panic!("not stopped: {outcome:?}");
    };
    assert_eq!(reason, StopReason::InsufficientSpace);
    let db = db_path(&tmp, &id);
    assert_eq!(partial_files(&db), 0, "the batch's partial file was left");
    assert_eq!(summary.counts.attachment_files.partials_left, 0);
    assert_eq!(stored_partials_left(&db), (6, 0));
    probe.low.store(false, std::sync::atomic::Ordering::SeqCst);
    let at = Imported { tmp, lib, id };
    resume_to_uninterrupted(&at, summary.lineage);
}

/// A failed intent commit (`SQLITE_FULL`, a resumable stop) removes the
/// batch's partial files; with a store that cannot remove them, they are
/// counted in the stopped session's stored counts.
#[test]
fn a_failed_intent_commit_removes_or_counts_the_batch_partial_files() {
    for stuck in [false, true] {
        let (tmp, lib) = library();
        let lib = if stuck {
            lib.with_store_fs(Arc::new(StuckPartials))
        } else {
            lib
        };
        let id = lib.create(None).expect("create");
        let outcome = {
            let _g = install(Faults {
                intent_error: Some(13),
                ..Faults::default()
            });
            let mut a = lib.open(&id).expect("open");
            let src = open_entry("imp-dev1-v1-plain");
            let o = import_with(
                &mut a,
                &src,
                &opts(1000),
                LineageChoice::New,
                &CancelToken::new(),
            )
            .expect("import");
            drop(a);
            o
        };
        let ImportOutcome::Stopped { reason, summary } = outcome else {
            panic!("stuck {stuck}: not stopped: {outcome:?}");
        };
        assert_eq!(reason, StopReason::InsufficientSpace);
        let db = db_path(&tmp, &id);
        let left = if stuck { 6 } else { 0 };
        assert_eq!(partial_files(&db), usize::try_from(left).expect("n"));
        assert_eq!(summary.counts.attachment_files.partials_left, left);
        assert_eq!(stored_partials_left(&db), (6, left), "stuck {stuck}");
        if !stuck {
            let at = Imported { tmp, lib, id };
            resume_to_uninterrupted(&at, summary.lineage);
        }
    }
}

/// Counts kept outside the records (here, partial files a failed copy
/// could not remove) survive a batch that rolls back into a resumable stop:
/// the stopped session records them.
#[test]
fn pending_attachment_counts_survive_a_rolled_back_batch() {
    let _g = install(Faults {
        copy_write_error: Some(5),
        ..Faults::default()
    });
    let (tmp, lib) = library();
    let lib = lib.with_store_fs(Arc::new(StuckPartials));
    let id = lib.create(None).expect("create");
    let outcome = {
        // Every batch from the first attachment copy on fails its COMMIT
        // with `SQLITE_FULL`.
        let _trip = arm(
            FaultPoint::AttachmentPartialCreated,
            1,
            Box::new(|_, _| {
                std::mem::forget(install(Faults {
                    copy_write_error: Some(5),
                    batch_error: Some(13),
                    ..Faults::default()
                }));
            }),
        );
        let mut a = lib.open(&id).expect("open");
        let src = open_entry("imp-dev1-v1-plain");
        let o = import_with(
            &mut a,
            &src,
            &opts(1000),
            LineageChoice::New,
            &CancelToken::new(),
        )
        .expect("import");
        drop(a);
        o
    };
    let ImportOutcome::Stopped { reason, summary } = outcome else {
        panic!("not stopped: {outcome:?}");
    };
    assert_eq!(reason, StopReason::InsufficientSpace);
    let db = db_path(&tmp, &id);
    assert_eq!(partial_files(&db), 6);
    assert_eq!(summary.counts.attachment_files.partials_left, 6);
    // The rolled-back records' own counts are not kept.
    assert_eq!(summary.counts.attachment_files.write_failed, 0);
    assert_eq!(stored_partials_left(&db), (6, 6));
}

/// The stated exception to order independence (`docs/architecture.md`):
/// "already archived" compares presence and length only, so two backups
/// with **different bytes of the same length** under one attachment GUID
/// keep whichever was imported first; the other version is never read.
/// Here v2's copy of `SYNTH-ATT-GUID-0001` (shared with v1) is replaced by
/// other bytes of the same length.
#[test]
fn same_length_different_bytes_keep_the_first_imported_version() {
    let shared = "eb89532441a7d5bc2410cb1d8412a35c9ea1b688ab1b80c22c2a50dbeb4a35cb";
    let src_dir = tempfile::tempdir().expect("backup");
    let v2 = copy_entry("imp-dev1-v2-plain", src_dir.path());
    let mut other = None;
    for d in fs::read_dir(&v2).expect("udid").flatten() {
        if !d.file_type().expect("type").is_dir() {
            continue;
        }
        for f in fs::read_dir(d.path()).expect("dir").flatten() {
            let bytes = fs::read(f.path()).expect("read");
            if hex(&Sha256::digest(&bytes)) == shared {
                let flipped: Vec<u8> = bytes.iter().map(|b| b ^ 0x5a).collect();
                other = Some(hex(&Sha256::digest(&flipped)));
                fs::write(f.path(), &flipped).expect("rewrite");
            }
        }
    }
    let other = other.expect("v2 holds the shared attachment");
    let blobs = |at: &Imported| -> BTreeSet<String> {
        blob_list(&at.db())
            .as_array()
            .expect("blobs")
            .iter()
            .map(|b| b["sha256"].as_str().expect("sha").to_owned())
            .collect()
    };
    let import_v2 = |at: &Imported| {
        let mut a = at.lib.open(&at.id).expect("open");
        let src = open_dir(&v2, None);
        let s = import(&mut a, &src, &opts(1000), LineageChoice::New);
        drop(a);
        src.ob.close();
        s
    };
    // v1 then v2: v1's bytes are stored; v2's are never read.
    let at = fresh();
    import_entry(&at, "imp-dev1-v1-plain");
    import_v2(&at);
    let b = blobs(&at);
    assert!(b.contains(shared) && !b.contains(&other), "{b:?}");
    // v2 then v1: v2's bytes are stored; v1's are never read.
    let at = fresh();
    import_v2(&at);
    import_entry(&at, "imp-dev1-v1-plain");
    let b = blobs(&at);
    assert!(b.contains(&other) && !b.contains(shared), "{b:?}");
}
