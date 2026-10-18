//! The golden v1 archive (spec §7.3, §18.2 `golden.rs`, §19.2).
//!
//! `tests/fixtures/archives/golden-v1/` was made by this milestone's build
//! (`cargo run -p oma-archive --example make_golden`) from the synthetic
//! entries `imp-dev1-v1-plain` and `imp-dev1-v2-plain`. Every later build
//! must open it read-write and read it as it was written: a schema,
//! fingerprint, opener, rusqlite or libsqlite3-sys change that breaks an
//! existing archive fails here. The committed files are never written: the
//! test works on a copy.
//!
//! The verifier (quick and full) is slice 4k; until it exists this test
//! runs the checks it will make that already exist: `integrity_check`,
//! `foreign_key_check`, the FTS rank-1 check and the store's agreement with
//! its rows.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use common::import::*;
use common::*;
use oma_archive::{ArchiveEntry, Library, Recovery, SessionInfo};
use serde_json::Value;
use sha2::{Digest, Sha256};

fn golden() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/archives/golden-v1"
    ))
}

/// Every file under `dir`, relative with `/` separators.
fn files(base: &Path, dir: &Path, out: &mut BTreeMap<String, PathBuf>) {
    for e in fs::read_dir(dir).expect("read_dir").flatten() {
        let p = e.path();
        if e.file_type().expect("type").is_dir() {
            files(base, &p, out);
        } else {
            let rel: Vec<String> = p
                .strip_prefix(base)
                .expect("prefix")
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            out.insert(rel.join("/"), p);
        }
    }
}

#[test]
fn the_golden_files_match_their_sums() {
    let root = golden();
    let mut listed = BTreeMap::new();
    for line in fs::read_to_string(root.join("SHA256SUMS"))
        .expect("SHA256SUMS")
        .lines()
    {
        let (sum, rel) = line.split_once("  ").expect("<sha>  <path>");
        listed.insert(rel.to_owned(), sum.to_owned());
    }
    let mut found = BTreeMap::new();
    files(&root, &root.join("library"), &mut found);
    assert_eq!(
        found.keys().collect::<Vec<_>>(),
        listed.keys().collect::<Vec<_>>(),
        "files differ from SHA256SUMS"
    );
    for (rel, p) in &found {
        let got = hex(&Sha256::digest(fs::read(p).expect("read")));
        assert_eq!(&got, &listed[rel], "{rel}");
    }
}

/// Copy the golden library into `to`, as git checks it out: recreate the
/// empty `attachments/.incoming/` git cannot hold, and give it the private
/// modes the archive requires (git keeps none).
fn copy_library(to: &Path) -> PathBuf {
    let from = golden().join("library");
    let mut list = BTreeMap::new();
    files(&from, &from, &mut list);
    for (rel, p) in &list {
        let dst = rel
            .split('/')
            .fold(to.to_path_buf(), |d, part| d.join(part));
        fs::create_dir_all(dst.parent().expect("parent")).expect("mkdir");
        fs::copy(p, &dst).expect("copy");
    }
    let archive = fs::read_dir(to)
        .expect("library")
        .flatten()
        .map(|e| e.path())
        .find(|p| p.is_dir())
        .expect("one archive");
    fs::create_dir_all(archive.join("attachments").join(".incoming")).expect("incoming");
    #[cfg(unix)]
    {
        fn private(p: &Path) {
            for e in fs::read_dir(p).expect("dir").flatten() {
                let q = e.path();
                if e.file_type().expect("type").is_dir() {
                    chmod(&q, 0o700);
                    private(&q);
                } else {
                    chmod(&q, 0o600);
                }
            }
        }
        chmod(to, 0o700);
        private(to);
    }
    archive
}

#[test]
fn the_golden_archive_opens_read_write_and_reads_as_written() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let archive = copy_library(tmp.path());
    let db = archive.join("archive.sqlite");
    let lib = Library::new(tmp.path())
        .expect("library")
        .with_space_probe(fake_space(Some(TEST_AVAILABLE), Some(2 * TEST_AVAILABLE)));
    let listed = lib.list().expect("list");
    let [ArchiveEntry::Ready(info)] = listed.as_slice() else {
        panic!("{listed:?}");
    };
    assert_eq!(info.sessions.completed, 2);
    assert_eq!(info.sessions.total(), 2);
    let stats = info.stats.expect("archive_stats");
    assert!(stats.messages > 0 && stats.blobs > 0);
    assert!(
        matches!(&info.label, oma_archive::LabelState::Ok(l) if l.as_str() == "Golden archive v1")
    );

    // Read-write: the first write and its recovery, then close.
    let mut a = lib.open(&info.id).expect("open");
    let sessions: Vec<SessionInfo> = a.reader().sessions().expect("sessions");
    assert!(sessions.iter().all(|s| s.full_observation == Some(true)));
    let r = a.begin_write(Recovery::Run).expect("write open");
    assert_eq!(r.sessions_interrupted, 0);
    a.close();

    assert_consistent(&db);
    assert_store_consistent(&db, true);
    let pinned: Value = serde_json::from_slice(
        &fs::read(golden().join("logical_snapshot.json")).expect("snapshot"),
    )
    .expect("json");
    assert_eq!(logical_snapshot(&db, Scope::WithSessions), pinned);
    // Its provenance files.
    for n in [1, 2] {
        let p: Value = serde_json::from_slice(
            &fs::read(archive.join("imports").join(format!("session-{n}.json"))).expect("read"),
        )
        .expect("json");
        assert_eq!(p["format"], "oma-import-provenance");
        assert_eq!(
            (p["session"].clone(), p["state"].clone()),
            (n.into(), "completed".into())
        );
    }
}
