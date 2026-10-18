//! Windows behaviour of the M4 primitives, run on the `windows-2022` CI
//! runner. Each test prints what it observed, so the CI log is the record
//! that `docs/compatibility.md` cites.
#![cfg(windows)]

use std::fs;
use std::io;
use std::path::Path;
use std::process::Command;

use oma_fs::{
    check_private, create_dir_durable, fsync_dir, open_nofollow, open_refusal, place_noreplace,
    volume_kind, DirSync, Expect, NetworkKind, OpenRefusal, PermProblem, Placed, VolumeKind,
};

/// `mklink /J` needs no privilege, unlike a symbolic link.
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
fn fsync_dir_outcome_is_recorded() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let outcome = fsync_dir(tmp.path()).expect("fsync_dir must not error on a real directory");
    println!("RECORD fsync_dir on NTFS temp directory: {outcome:?}");
    assert!(matches!(outcome, DirSync::Synced | DirSync::Unsupported));
}

#[test]
fn create_dir_durable_works_on_ntfs() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("imports");
    create_dir_durable(&dir).expect("create");
    create_dir_durable(&dir).expect("existing");
    assert!(dir.is_dir());
}

#[test]
fn place_noreplace_links_and_never_replaces() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let from = tmp.path().join("x.partial");
    let to = tmp.path().join("x");
    fs::write(&from, b"one").expect("write");
    let placed = place_noreplace(&from, &to).expect("place");
    println!("RECORD place_noreplace on NTFS: {placed:?}");
    assert_eq!(
        placed,
        Placed::Linked {
            source_removed: true
        }
    );
    fs::write(&from, b"two").expect("write");
    let err = place_noreplace(&from, &to).expect_err("existing target");
    assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read(&to).expect("read"), b"one");
}

#[test]
fn a_held_source_is_reported_not_lost() {
    use std::os::windows::fs::OpenOptionsExt;
    let tmp = tempfile::tempdir().expect("tempdir");
    let from = tmp.path().join("held.partial");
    let to = tmp.path().join("held");
    fs::write(&from, b"bytes").expect("write");
    // A reader that shares read only (no FILE_SHARE_DELETE), as a scanner
    // might, makes the source removal fail after the link succeeds.
    let holder = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&from)
        .expect("hold");
    // Whether CreateHardLinkW itself is refused while the source is held
    // is UNVERIFIED, so either outcome is recorded; what must hold is that
    // nothing is lost and nothing half-placed.
    match place_noreplace(&from, &to) {
        Ok(placed) => {
            println!("RECORD place_noreplace with the source held open: {placed:?}");
            assert!(matches!(placed, Placed::Linked { .. }));
            assert_eq!(fs::read(&to).expect("read"), b"bytes");
        }
        Err(e) => {
            println!(
                "RECORD place_noreplace with the source held open failed: {:?} (os error {:?})",
                e.kind(),
                e.raw_os_error()
            );
            assert!(!to.exists());
            assert_eq!(fs::read(&from).expect("read"), b"bytes");
        }
    }
    drop(holder);
}

#[test]
fn open_nofollow_refuses_a_junction() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let target = tmp.path().join("target");
    fs::create_dir(&target).expect("mkdir");
    fs::write(target.join("f"), b"x").expect("write");
    let junction = tmp.path().join("junction");
    assert!(make_junction(&junction, &target), "mklink /J failed");

    let err = open_nofollow(&junction).expect_err("junction");
    println!(
        "RECORD open_nofollow on a junction: kind {:?}, refusal {:?}",
        err.kind(),
        open_refusal(&err)
    );
    // A file reached through the junction is also refused only by the
    // caller's directory checks (open_nofollow guards the last component);
    // check_private catches the junction itself.
    assert_eq!(
        check_private(&junction, Expect::Dir),
        Err(PermProblem::Link)
    );
    let std_says = fs::symlink_metadata(&junction)
        .expect("meta")
        .file_type()
        .is_symlink();
    println!("RECORD std is_symlink() on a junction: {std_says}");
    // create_dir_durable must not accept a junction as the directory.
    assert_eq!(
        create_dir_durable(&junction).expect_err("junction").kind(),
        io::ErrorKind::AlreadyExists
    );
}

#[test]
fn open_nofollow_refuses_a_symlink_when_one_can_be_made() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let target = tmp.path().join("target");
    fs::write(&target, b"secret").expect("write");
    let link = tmp.path().join("link");
    match std::os::windows::fs::symlink_file(&target, &link) {
        Ok(()) => {
            let err = open_nofollow(&link).expect_err("symlink");
            assert_eq!(open_refusal(&err), Some(OpenRefusal::Link));
        }
        Err(e) => println!(
            "RECORD skipped: creating a symbolic link failed ({:?}); needs Developer Mode or elevation",
            e.kind()
        ),
    }
}

#[test]
fn a_unc_library_is_a_network_volume() {
    for p in [r"\\server\share\library", r"\\?\UNC\server\share\library"] {
        assert_eq!(
            volume_kind(Path::new(p)).expect("lexical"),
            VolumeKind::Network(NetworkKind::Unc),
            "{p}"
        );
    }
}

/// Records whether `std::fs::canonicalize` reveals a mapped network drive
/// as `\\?\UNC\…` (spec §3.1). Ignored by default: it maps a drive letter
/// to the administrative share `\\localhost\C$` with `net use`, which
/// changes machine state. Run it by hand with `--ignored` on a disposable
/// machine.
#[test]
#[ignore = "maps a network drive with `net use`; run by hand"]
fn mapped_drive_canonicalize_probe() {
    let letter = "O:";
    let mapped = Command::new("net")
        .args(["use", letter, r"\\localhost\C$", "/persistent:no"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !mapped {
        println!("RECORD mapped-drive probe: could not map {letter}");
        return;
    }
    let root = format!("{letter}\\");
    let canonical = fs::canonicalize(&root);
    let kind = volume_kind(Path::new(&root));
    let _ = Command::new("net")
        .args(["use", letter, "/delete", "/y"])
        .status();
    println!("RECORD mapped drive canonicalize: {canonical:?}; volume_kind: {kind:?}");
}
