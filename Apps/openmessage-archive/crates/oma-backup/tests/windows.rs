//! Windows-only checks (§16.2). Run by the Windows CI job; not run on Linux.
#![cfg(windows)]
mod common;

use std::fs;
use std::process::Command;

use common::*;
use oma_backup::{
    CancelToken, CheckId, CheckLevel, Finding, JobCtx, Progress, SMS_DB_DOMAIN, SMS_DB_PATH,
};

#[test]
fn long_library_path() {
    // A failure here is recorded against the long-path item in the spec's
    // open questions (§22.13).
    for (name, password) in [("plain", None), ("enc", Some(PASSWORD))] {
        let mut fx = Fixture::new(name);
        let mut library = fx.tmp.path().join("library-long");
        while library.as_os_str().len() < 200 {
            library.push("long-library-path-segment");
        }
        fs::create_dir_all(&library).expect("mkdir");
        fx.library = library;
        let (report, cleanup) = fx.validate(password);
        assert!(report.importable(), "{name}: {:?}", report.checks);
        assert!(cleanup.removed, "{name}");
        assert!(fx.staged_files().is_empty(), "{name}");
    }
}

#[test]
fn staged_db_delete() {
    let fx = Fixture::new("enc");
    let ob = fx.open(Some(PASSWORD)).expect("open");
    let token = CancelToken::new();
    let mut sink = |_: Progress| {};
    let mut ctx = JobCtx::new(&token, &mut sink);
    let db = ob
        .extract_database(SMS_DB_DOMAIN, SMS_DB_PATH, &mut ctx)
        .expect("extract");
    let c =
        rusqlite::Connection::open_with_flags(&db.path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("open staged");
    let _: i64 = c
        .query_row("SELECT count(*) FROM synthetic_marker", [], |r| r.get(0))
        .expect("query");
    c.close().expect("close connection");
    let session = ob.staging_path().expect("staging").to_path_buf();
    assert!(ob.close().removed);
    assert!(!session.exists());
}

#[test]
fn junction_blob_dir() {
    let fx = Fixture::new("plain");
    let outside = fx.tmp.path().join("outside-dir");
    fs::create_dir(&outside).expect("mkdir");
    fs::write(
        outside.join("3d0d7e5fb2ce288813306e4d4636395e047a3d28"),
        b"OUTSIDE-SENTINEL-CONTENT",
    )
    .expect("write");
    fs::rename(fx.dir.join("3d"), fx.tmp.path().join("moved-3d")).expect("mv");
    let status = Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(fx.dir.join("3d"))
        .arg(&outside)
        .status()
        .expect("mklink");
    assert!(status.success());
    let (report, cleanup) = fx.validate(None);
    assert!(report.stats.unsafe_blobs >= 1);
    let m = report.check(CheckId::MessagesDb).expect("messages-db");
    assert_eq!(m.level, CheckLevel::Error);
    assert!(m.findings.contains(&Finding::SmsBlobNotRegular));
    assert!(cleanup.removed);
    assert!(!format!("{report:?}").contains("OUTSIDE-SENTINEL"));
}
