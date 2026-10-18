//! Windows behaviour of the archive (spec §18.8), run on the
//! `windows-2022` CI runner. Each test prints `RECORD` lines, so the CI log
//! is the record `docs/compatibility.md` cites.
//!
//! The first test is the one slice 4d must run first: WAL and `-shm` on the
//! `win32-longpath` VFS. If it fails, D86's fallback applies (refuse
//! library paths whose deepest archive path exceeds 240 characters), and
//! this test changes to assert that refusal.
#![cfg(windows)]
#![allow(clippy::disallowed_methods)]

mod common;

use std::fs;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::*;
use oma_archive::{
    ArchiveEntry, ArchiveError, CheckpointOutcome, Library, PermProblem, Recovery, RefusalKind,
};

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

fn make_junction(link: &Path, target: &Path) -> bool {
    Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(link)
        .arg(target)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[test]
fn wal_and_shm_work_on_win32_longpath_under_a_long_library() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let lib_root = long_library(tmp.path(), 230);
    let lib = Library::new(&lib_root).expect("library");
    let id = lib.create(None).expect("create under a long path");
    let dir = lib_root.join(id.as_str());
    let db = dir.join("archive.sqlite");
    let shm = dir.join("archive.sqlite-shm");
    println!(
        "RECORD long-path archive: library {} chars, database {} chars, -shm {} chars",
        lib_root.as_os_str().len(),
        db.as_os_str().len(),
        shm.as_os_str().len()
    );
    assert!(shm.as_os_str().len() > 260, "the test must exceed MAX_PATH");

    let mut archive = lib.open(&id).expect("open");
    archive.begin_write(Recovery::Run).expect("writer");
    let wal_present = fs::symlink_metadata(dir.join("archive.sqlite-wal")).is_ok();
    let shm_present = fs::symlink_metadata(&shm).is_ok();
    println!("RECORD long-path WAL open: -wal present {wal_present}, -shm present {shm_present}");
    assert!(shm_present, "WAL index (-shm) on win32-longpath");
    // A lock-free reader alongside the writer.
    let reader = lib.open_read(&id).expect("reader alongside the writer");
    assert_eq!(reader.info().expect("info").sessions.total(), 0);
    drop(reader);
    let report = archive.close();
    println!("RECORD long-path close checkpoint: {:?}", report.checkpoint);
    assert_eq!(report.checkpoint, Some(CheckpointOutcome::Done));

    // Reopen, read, list.
    let archive = lib.open(&id).expect("reopen");
    drop(archive);
    assert!(matches!(
        &lib.list().expect("list")[..],
        [ArchiveEntry::Ready(_)]
    ));
    // The header still says WAL (bytes 18 and 19 are 2).
    let header = fs::read(&db).expect("read header");
    assert_eq!((header[18], header[19]), (2, 2), "WAL mode persisted");
}

#[test]
fn archive_files_carry_no_temporary_attribute() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let dir = archive_dir(&tmp, &id);
    for name in ["archive.sqlite", "archive.lock"] {
        let attrs = fs::metadata(dir.join(name))
            .expect("meta")
            .file_attributes();
        println!("RECORD {name} attributes: {attrs:#x}");
        assert_eq!(attrs & 0x100, 0, "{name}: FILE_ATTRIBUTE_TEMPORARY");
    }
}

#[test]
fn a_unc_library_is_a_network_volume() {
    for p in [r"\\server\share\library", r"\\?\UNC\server\share\library"] {
        let got = Library::new(Path::new(p));
        println!("RECORD Library::new({p}): {got:?}");
        assert!(matches!(got, Err(ArchiveError::NetworkVolume)), "{p}");
    }
}

#[test]
fn a_junction_in_place_of_an_archive_directory_is_refused() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let elsewhere = tempfile::tempdir().expect("elsewhere");
    let moved = elsewhere.path().join("real");
    fs::rename(archive_dir(&tmp, &id), &moved).expect("move out");
    assert!(
        make_junction(&archive_dir(&tmp, &id), &moved),
        "mklink /J failed"
    );
    let got = lib.open(&id);
    println!("RECORD open through a junction: {got:?}");
    assert!(matches!(
        got,
        Err(ArchiveError::Permissions(PermProblem::Link))
    ));
    assert!(matches!(
        &lib.list().expect("list")[..],
        [ArchiveEntry::Refused {
            why: RefusalKind::Link,
            ..
        }]
    ));
}

#[test]
fn renaming_a_creation_directory_with_a_handle_left_open_is_recorded() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let from = tmp.path().join(".oma-new-held");
    fs::create_dir(&from).expect("mkdir");
    fs::write(from.join("archive.lock"), b"").expect("write");
    let held = fs::File::open(from.join("archive.lock")).expect("hold");
    let to = tmp.path().join("a-target");
    let with_handle = oma_fs::rename_dir_noreplace(&from, &to);
    println!(
        "RECORD directory rename with a handle open inside: {:?}",
        with_handle
            .as_ref()
            .map_err(|e| (e.kind(), e.raw_os_error()))
    );
    drop(held);
    if with_handle.is_err() {
        oma_fs::rename_dir_noreplace(&from, &to).expect("rename once the handle is closed");
    }
    assert!(to.join("archive.lock").is_file());
}

/// The junction variant of `store.rs`'s `links_in_the_store_are_never_followed`
/// (spec §18.5): a `<hh>` directory replaced by a junction to a directory
/// outside the archive is never written through; the copy is counted as a
/// store conflict.
#[test]
fn a_junction_in_the_store_is_never_followed() {
    use common::import::*;
    use oma_archive::LineageChoice;

    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let import_v1 = || {
        let mut a = lib.open(&id).expect("open");
        let src = open_entry("imp-dev1-v1-plain");
        let s = import(&mut a, &src, &opts(1000), LineageChoice::New);
        drop(a);
        src.ob.close();
        s
    };
    import_v1();
    let store = archive_dir(&tmp, &id).join("attachments");
    let hh = fs::read_dir(&store)
        .expect("attachments")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.len() == 2)
        .min()
        .expect("a fan-out directory");
    let files_in_hh = fs::read_dir(store.join(&hh)).expect("hh").count();
    let outside = tempfile::tempdir().expect("outside");
    fs::remove_dir_all(store.join(&hh)).expect("rm hh");
    assert!(
        make_junction(&store.join(&hh), outside.path()),
        "mklink /J failed"
    );
    let s = import_v1();
    let f = s.counts.attachment_files;
    println!(
        "RECORD import with a junction at <hh>: store conflicts {}, files that were there {files_in_hh}",
        f.store_conflicts
    );
    assert_eq!(
        f.store_conflicts,
        u64::try_from(files_in_hh).expect("n"),
        "{f:?}"
    );
    assert_eq!(
        fs::read_dir(outside.path()).expect("outside").count(),
        0,
        "nothing is written through the junction"
    );
}
