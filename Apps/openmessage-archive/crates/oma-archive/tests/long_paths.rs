//! An import, a full verify, `open_blob` and a reopen under a library path
//! of about 230 characters (spec §18.8): the store files
//! (`<library>/a<32 hex>/attachments/<hh>/<64 hex>`, over 310 characters),
//! the partial files, the provenance file and its temporary, and the
//! staged sms.db are all longer than `MAX_PATH`.
//!
//! It runs on every platform. On Linux it checks the procedure; on the
//! `windows-2022` runner it is the long-path test of slices 4g–4k, and
//! its `RECORD` lines are what `docs/compatibility.md` cites. The
//! database is read only through the crate (its win32-longpath VFS), never
//! through a default-VFS connection.
#![allow(clippy::disallowed_methods)]

mod common;

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use common::import::*;
use common::*;
use oma_archive::{
    ArchiveCtx, BlobId, CancelToken, FixedClock, Library, LineageChoice, NoProgress, VerifyDepth,
    VerifyOutcome,
};
use sha2::{Digest, Sha256};

/// A library directory whose path is about `len` characters long.
fn long_library(base: &Path, len: usize) -> PathBuf {
    let mut p = dunce::canonicalize(base).expect("canonical");
    let mut i = 0;
    while p.as_os_str().len() < len {
        p.push(format!("long-path-segment-{i:02}"));
        i += 1;
    }
    fs::create_dir_all(&p).expect("create long library");
    p
}

#[test]
fn an_import_verify_and_read_back_work_under_a_long_library_path() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = long_library(tmp.path(), 230);
    let lib = Library::new(&root)
        .expect("library")
        .with_space_probe(fake_space(Some(TEST_AVAILABLE), Some(2 * TEST_AVAILABLE)));
    let id = lib.create(None).expect("create under a long path");
    let entry = "imp-dev1-v1-plain";

    let mut a = lib.open(&id).expect("open");
    let src = open_entry(entry);
    let s = import(&mut a, &src, &opts(1000), LineageChoice::New);
    src.ob.close();
    let f = s.counts.attachment_files;
    println!(
        "RECORD long-path import: library {} chars, copied {}, blobs added {}, partials left {}",
        root.as_os_str().len(),
        f.copied,
        f.blobs_added,
        f.partials_left
    );
    assert_eq!(
        (f.copied, f.blobs_added, f.partials_left),
        (6, 6, 0),
        "{f:?}"
    );
    assert!(
        s.provenance_written,
        "the provenance file under a long path"
    );

    // A full verify re-hashes every store file under the long path.
    let clock = FixedClock(MODEL_NOW_NS);
    let r = a
        .verify(
            VerifyDepth::Full,
            &mut ArchiveCtx {
                cancel: &CancelToken::new(),
                progress: &mut NoProgress,
                clock: &clock,
            },
        )
        .expect("verify");
    println!(
        "RECORD long-path full verify: {:?}, {} blobs hashed",
        r.outcome, r.counts.blobs_hashed
    );
    assert_eq!(r.outcome, VerifyOutcome::Ok, "{:?}", r.counts);
    assert_eq!(r.counts.blobs_hashed, 6);
    assert_eq!(r.counts.provenance_files, 1);
    drop(a);

    // Every store file reads back through `open_blob`, after a reopen.
    let a = lib.open(&id).expect("reopen");
    let mut want = blob_digests(entry);
    let mut got = Vec::new();
    for n in 1..=6 {
        let mut reader = a.open_blob(BlobId::new(n)).expect("open_blob");
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).expect("read to the end");
        got.push(hex(&Sha256::digest(&bytes)));
    }
    drop(a);
    want.sort();
    got.sort();
    assert_eq!(got, want);

    let dir = root.join(id.as_str());
    let deepest = fs::read_dir(dir.join("attachments"))
        .expect("attachments")
        .flatten()
        .filter(|e| e.file_name() != ".incoming")
        .flat_map(|e| fs::read_dir(e.path()).expect("hh").flatten())
        .map(|e| e.path().as_os_str().len())
        .max()
        .expect("a store file");
    println!("RECORD long-path store file: {deepest} chars");
    assert!(deepest > 300, "the store files must exceed MAX_PATH");
    assert!(dir.join("imports").join("session-1.json").is_file());
    assert_eq!(
        fs::read_dir(dir.join("attachments").join(".incoming"))
            .expect("incoming")
            .count(),
        0
    );
}

/// The catalogue's blob digests of an entry.
fn blob_digests(entry: &str) -> Vec<String> {
    let m = catalogue_json(&format!("{entry}.expected/content.json"));
    m["content"]["blobs"]
        .as_array()
        .expect("blobs")
        .iter()
        .map(|b| b["sha256"].as_str().expect("sha256").to_owned())
        .collect()
}
