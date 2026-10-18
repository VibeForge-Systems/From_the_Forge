//! Every `LimitKind` is produced at least once, with small injected limits.
mod common;

use std::fs;

use common::*;
use oma_backup::bplist::{read_bounded, PlistReadError, ReadOpts};
use oma_backup::{
    scan_root, BackupError, CancelToken, JobCtx, LimitKind, Limits, MbFile, Password, Progress,
    RootScanState,
};
use zeroize::Zeroizing;

fn kind(err: &BackupError) -> Option<LimitKind> {
    match err {
        BackupError::LimitExceeded { what, .. } => Some(*what),
        _ => None,
    }
}

fn open_kind(name: &str, limits: &Limits, password: Option<&str>) -> Option<LimitKind> {
    let fx = Fixture::read_only(name);
    let mut sink = |_: Progress| {};
    let err = fx
        .open_with(password, limits, &mut sink, &CancelToken::new())
        .expect_err("limit");
    assert!(err.cleanup.removed);
    kind(&err.error)
}

fn validate_kind(name: &str, limits: &Limits) -> Option<LimitKind> {
    let fx = Fixture::read_only(name);
    let mut sink = |_: Progress| {};
    let token = CancelToken::new();
    let ob = fx.open_with(None, limits, &mut sink, &token).expect("open");
    let mut sink = |_: Progress| {};
    let mut ctx = JobCtx::new(&token, &mut sink);
    let err = ob.validate(&mut ctx).expect_err("limit");
    assert!(ob.close().removed);
    kind(&err)
}

#[test]
fn keybag_and_iteration_limits() {
    let d = Limits::default();
    assert_eq!(
        open_kind("hostile-dpic", &d, Some(PASSWORD)),
        Some(LimitKind::Dpic)
    );
    assert_eq!(
        open_kind("hostile-iter", &d, Some(PASSWORD)),
        Some(LimitKind::Iter)
    );
    let small = Limits {
        keybag_bytes: 100,
        ..Limits::default()
    };
    assert_eq!(
        open_kind("enc", &small, Some(PASSWORD)),
        Some(LimitKind::KeybagBytes)
    );
    let few = Limits {
        keybag_classes: 3,
        ..Limits::default()
    };
    assert_eq!(
        open_kind("enc", &few, Some(PASSWORD)),
        Some(LimitKind::KeybagClasses)
    );
    let tight = Limits {
        max_dpic: 999,
        ..Limits::default()
    };
    assert_eq!(
        open_kind("enc", &tight, Some(PASSWORD)),
        Some(LimitKind::Dpic)
    );
}

#[test]
fn database_limits() {
    let small = Limits {
        manifest_db_bytes: 1000,
        ..Limits::default()
    };
    assert_eq!(
        open_kind("plain", &small, None),
        Some(LimitKind::ManifestDbBytes)
    );
    let rows = Limits {
        manifest_rows: 2,
        ..Limits::default()
    };
    assert_eq!(validate_kind("plain", &rows), Some(LimitKind::ManifestRows));
    let record = Limits {
        mbfile_bytes: 100,
        ..Limits::default()
    };
    assert_eq!(
        validate_kind("plain", &record),
        Some(LimitKind::MbFileBytes)
    );
}

#[test]
fn a_sparse_manifest_over_the_default_cap() {
    let fx = Fixture::new("plain");
    // Sparse: metadata says 5 GiB, almost nothing is written.
    let f = fs::OpenOptions::new()
        .write(true)
        .open(fx.dir.join("Manifest.db"))
        .expect("open");
    f.set_len(5 << 30).expect("set_len");
    drop(f);
    let err = fx.open(None).expect_err("too large");
    assert_eq!(kind(&err.error), Some(LimitKind::ManifestDbBytes));
    assert!(err.cleanup.removed);
}

#[test]
fn plist_limits() {
    let fx = Fixture::read_only("plain");
    // Depth: Manifest.plist's Lockdown is a nested dictionary.
    match read_bounded(&fx.dir.join("Manifest.plist"), 1 << 20, ReadOpts::new(1)) {
        Err(PlistReadError::Limit { what, .. }) => assert_eq!(what, LimitKind::PlistDepth),
        other => panic!("{other:?}"),
    }
    // Events: the Status.plist reference bomb.
    let bomb = Fixture::read_only("bomb-status");
    match read_bounded(&bomb.dir.join("Status.plist"), 1 << 20, ReadOpts::new(32)) {
        Err(PlistReadError::Limit { what, .. }) => assert_eq!(what, LimitKind::PlistEvents),
        other => panic!("{other:?}"),
    }
    // Bytes: one data object referenced many times, written by hand.
    let path = fx.tmp.path().join("shared-data.plist");
    fs::write(&path, shared_data_plist(4096, 64)).expect("write");
    match read_bounded(&path, 1 << 20, ReadOpts::new(32)) {
        Err(PlistReadError::Limit { what, .. }) => assert_eq!(what, LimitKind::PlistBytes),
        other => panic!("{other:?}"),
    }
}

/// A binary plist: an array of `refs` references to one `size`-byte data
/// object.
fn shared_data_plist(size: u32, refs: u8) -> Vec<u8> {
    let mut out = b"bplist00".to_vec();
    let data_off = out.len() as u32;
    out.push(0x4F);
    out.push(0x12);
    out.extend_from_slice(&size.to_be_bytes());
    out.extend(std::iter::repeat_n(0u8, size as usize));
    let arr_off = out.len() as u32;
    out.push(0xAF);
    out.push(0x10);
    out.push(refs);
    out.extend(std::iter::repeat_n(0u8, usize::from(refs)));
    let table = out.len() as u64;
    out.extend_from_slice(&data_off.to_be_bytes());
    out.extend_from_slice(&arr_off.to_be_bytes());
    out.extend_from_slice(&[0; 6]);
    out.push(4);
    out.push(1);
    out.extend_from_slice(&2u64.to_be_bytes());
    out.extend_from_slice(&1u64.to_be_bytes());
    out.extend_from_slice(&table.to_be_bytes());
    out
}

#[test]
fn mbfile_object_limit() {
    let fx = Fixture::read_only("plain");
    let c = rusqlite::Connection::open_with_flags(
        fx.dir.join("Manifest.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("open copy");
    let blob: Vec<u8> = c
        .query_row(
            "SELECT file FROM Files WHERE relativePath = 'Library/SMS/sms.db'",
            [],
            |r| r.get(0),
        )
        .expect("record");
    drop(c);
    assert!(MbFile::decode(&blob, &Limits::default()).is_ok());
    let few = Limits {
        mbfile_objects: 2,
        ..Limits::default()
    };
    assert_eq!(
        MbFile::decode(&blob, &few).err().as_ref().and_then(kind),
        Some(LimitKind::MbFileObjects)
    );
}

#[test]
fn root_entries_and_password_bytes() {
    let none = Limits {
        root_entries: 0,
        ..Limits::default()
    };
    // RootEntries surfaces as a scan state, not an error.
    let scan = scan_root(&fixtures_dir().join("plain"), &none);
    assert_eq!(scan.state, RootScanState::TooManyEntries);

    let long = Zeroizing::new("p".repeat(1025));
    let err = Password::new(long, &Limits::default()).expect_err("too long");
    assert_eq!(kind(&err), Some(LimitKind::PasswordBytes));
}
