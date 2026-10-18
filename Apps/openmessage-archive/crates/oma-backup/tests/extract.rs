//! The extraction API for M3 (§9).
mod common;

use std::fs;

use common::*;
use oma_backup::{
    BackupError, CancelToken, EntryKind, FileUnavailable, JobCtx, OpenBackup, Progress,
    SMS_DB_DOMAIN, SMS_DB_PATH,
};

fn ctx_run<T>(f: impl FnOnce(&mut JobCtx<'_>) -> T) -> T {
    let token = CancelToken::new();
    let mut sink = |_: Progress| {};
    let mut ctx = JobCtx::new(&token, &mut sink);
    f(&mut ctx)
}

fn check_extraction(name: &str, ob: &OpenBackup) {
    let expected = fixtures_dir().join(format!("{name}.expected"));
    let session = ob.staging_path().expect("staging").to_path_buf();

    let db =
        ctx_run(|ctx| ob.extract_database(SMS_DB_DOMAIN, SMS_DB_PATH, ctx)).expect("extract db");
    assert!(db.path.starts_with(session.join("db")), "{name}");
    assert_eq!(db.path.file_name().and_then(|n| n.to_str()), Some("sms.db"));
    assert!(db.wal.is_none());
    assert_eq!(
        fs::read(&db.path).expect("staged sms"),
        fs::read(expected.join("sms.db")).expect("expected sms"),
        "{name}"
    );
    // Opens through the public hardened open M3 will use.
    let c = db.open(&CancelToken::new()).expect("open staged sms");
    let note: String = c
        .conn()
        .query_row("SELECT note FROM synthetic_marker", [], |r| r.get(0))
        .expect("query");
    assert!(note.starts_with("synthetic"));
    assert!(
        c.conn()
            .execute("DELETE FROM synthetic_marker", [])
            .is_err(),
        "query_only"
    );
    drop(c);

    let entry = ob
        .find_file(ATTACHMENT_DOMAIN, ATTACHMENT_PATH)
        .expect("lookup")
        .expect("listed");
    let x = ctx_run(|ctx| ob.extract_file(&entry, ctx)).expect("extract file");
    assert!(x.path.starts_with(session.join("x")), "{name}");
    assert_eq!(
        fs::read(&x.path).expect("staged"),
        fs::read(expected.join("attachment.txt")).expect("expected")
    );
    assert_eq!(x.len, 87);
    assert_eq!(x.declared_size, Some(87));
    assert_eq!(x.size_matches, Some(true));

    // A directory row is not a file.
    let dir = ob
        .find_file("HomeDomain", "Library/SMS")
        .expect("lookup")
        .expect("listed");
    assert_eq!(dir.kind, EntryKind::Directory);
    assert!(matches!(
        ctx_run(|ctx| ob.extract_file(&dir, ctx)),
        Err(BackupError::FileUnavailable(FileUnavailable::NotAFile))
    ));
    // An unlisted database.
    assert!(matches!(
        ctx_run(|ctx| ob.extract_database("HomeDomain", "Library/Nope/none.db", ctx)),
        Err(BackupError::FileUnavailable(FileUnavailable::NotListed))
    ));
}

#[test]
fn sms_db() {
    for (name, password) in [
        ("plain", None),
        ("enc", Some(PASSWORD)),
        ("plain-xml", None),
        ("enc-xml", Some(PASSWORD)),
        ("enc-key-uid-data", Some(PASSWORD)),
        ("enc-key-inline", Some(PASSWORD)),
        ("enc-unicode-pw", Some(UNICODE_PASSWORD)),
    ] {
        let fx = Fixture::read_only(name);
        let ob = fx.open(password).expect("open");
        check_extraction(name, &ob);
        let cleanup = ob.close();
        assert!(cleanup.removed, "{name}");
        assert!(fx.staged_files().is_empty(), "{name}");
    }
}

#[test]
fn missing_blob_and_padding_errors() {
    let fx = Fixture::read_only("missing-sms-blob");
    let ob = fx.open(None).expect("open");
    let entry = ob
        .find_file(SMS_DB_DOMAIN, SMS_DB_PATH)
        .expect("lookup")
        .expect("listed");
    assert!(matches!(
        ctx_run(|ctx| ob.extract_file(&entry, ctx)),
        Err(BackupError::FileUnavailable(FileUnavailable::BlobMissing))
    ));
    assert!(ob.close().removed);

    let fx = Fixture::read_only("hostile-padding-sms");
    let ob = fx.open(Some(PASSWORD)).expect("open");
    assert!(matches!(
        ctx_run(|ctx| ob.extract_database(SMS_DB_DOMAIN, SMS_DB_PATH, ctx)),
        Err(BackupError::Malformed {
            detail: oma_backup::MalformedDetail::Padding,
            ..
        })
    ));
    // The failed extraction left no plaintext (no `.partial`) behind, even
    // before the session closes.
    let mut left = Vec::new();
    walk(&ob.staging_path().expect("staging").join("db"), &mut left);
    assert!(left.is_empty(), "{left:?}");
    assert!(ob.close().removed);
    assert!(fx.staged_files().is_empty());
}

#[test]
fn a_cancelled_extraction_leaves_no_partial_file() {
    for (name, password) in [("plain", None), ("enc", Some(PASSWORD))] {
        let fx = Fixture::read_only(name);
        let ob = fx.open(password).expect("open");
        let entry = ob
            .find_file(ATTACHMENT_DOMAIN, ATTACHMENT_PATH)
            .expect("lookup")
            .expect("listed");
        let token = CancelToken::new();
        token.cancel();
        let mut sink = |_: Progress| {};
        let mut ctx = JobCtx::new(&token, &mut sink);
        assert!(matches!(
            ob.extract_file(&entry, &mut ctx),
            Err(BackupError::Cancelled)
        ));
        let mut left = Vec::new();
        walk(&ob.staging_path().expect("staging").join("x"), &mut left);
        assert!(left.is_empty(), "{name}: {left:?}");
        assert!(ob.close().removed);
    }
}

#[test]
fn an_extracted_file_can_be_removed_before_close() {
    let fx = Fixture::read_only("plain");
    let ob = fx.open(None).expect("open");
    let entry = ob
        .find_file(ATTACHMENT_DOMAIN, ATTACHMENT_PATH)
        .expect("lookup")
        .expect("listed");
    let x = ctx_run(|ctx| ob.extract_file(&entry, ctx)).expect("extract");
    let path = x.path.clone();
    assert!(path.exists());
    x.remove().expect("remove");
    assert!(!path.exists());
    assert!(ob.close().removed);
}
