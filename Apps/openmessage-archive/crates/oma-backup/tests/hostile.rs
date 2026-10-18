//! Hostile fixtures, and hostile databases built at test time from `plain`.
mod common;

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use common::*;
use oma_backup::{
    BackupError, BackupFile, CheckId, CheckLevel, Classification, FileUnavailable, Finding,
    LimitKind, MalformedDetail, NoteKind, UnsupportedReason,
};

#[test]
fn plist_bombs() {
    let started = Instant::now();
    let info = Fixture::read_only("bomb-info").inspect();
    assert_eq!(info.classification, Classification::Ready);
    assert!(info.info.is_none(), "no device details from a bomb");
    assert!(info
        .notes
        .iter()
        .any(|n| n.file == BackupFile::InfoPlist && n.kind == NoteKind::BudgetExceeded));

    assert_eq!(
        Fixture::read_only("bomb-status").inspect().classification,
        Classification::Unreadable {
            file: BackupFile::StatusPlist,
            detail: MalformedDetail::BudgetExceeded
        }
    );
    assert_eq!(
        Fixture::read_only("bomb-manifest").inspect().classification,
        Classification::Unreadable {
            file: BackupFile::ManifestPlist,
            detail: MalformedDetail::BudgetExceeded
        }
    );
    let fx = Fixture::read_only("bomb-mbfile");
    let (report, cleanup) = fx.validate(None);
    assert_eq!(report.stats.unreadable_records, 1);
    assert_eq!(
        report.check(CheckId::FileRecords).map(|c| c.level),
        Some(CheckLevel::Warning)
    );
    assert!(cleanup.removed);
    // 2^25 nodes would take far longer than this.
    assert!(started.elapsed() < Duration::from_secs(20));
}

#[test]
fn lookup_keys() {
    let fx = Fixture::read_only("hostile-paths");
    let before = snapshot(fx.tmp.path());
    let ob = fx.open(None).expect("open");
    for rel in ["../../escape", "/abs", "C:\\x", "a\\..\\..\\b", "What?.jpg"] {
        let entry = ob
            .find_file("MediaDomain", rel)
            .expect("lookup")
            .expect("found");
        assert_eq!(entry.relative_path, rel);
        // Extraction uses a sequential name, never the relative path.
        let token = oma_backup::CancelToken::new();
        let mut sink = |_: oma_backup::Progress| {};
        let mut ctx = oma_backup::JobCtx::new(&token, &mut sink);
        let x = ob.extract_file(&entry, &mut ctx).expect("extract");
        assert!(x
            .path
            .starts_with(ob.staging_path().expect("staging").join("x")));
    }
    let token = oma_backup::CancelToken::new();
    let mut sink = |_: oma_backup::Progress| {};
    let mut ctx = oma_backup::JobCtx::new(&token, &mut sink);
    let report = ob.validate(&mut ctx).expect("validate");
    assert!(ob.close().removed);
    assert_eq!(report.stats.paths_unusual_for_export, 5);
    assert_eq!(report.stats.invalid_file_ids, 2);
    assert_eq!(
        report.check(CheckId::Paths).map(|c| c.level),
        Some(CheckLevel::Ok)
    );
    assert_eq!(
        report.check(CheckId::FileIds).map(|c| c.level),
        Some(CheckLevel::Warning)
    );
    // Nothing was created outside staging (and staging is gone again).
    let after: Vec<_> = snapshot(fx.tmp.path())
        .into_iter()
        .filter(|(p, _)| !p.starts_with("library/.oma-staging"))
        .collect();
    let before: Vec<_> = before
        .into_iter()
        .filter(|(p, _)| !p.starts_with("library/.oma-staging"))
        .collect();
    assert_eq!(before, after);
}

#[test]
fn fileid_mismatch() {
    let fx = Fixture::read_only("hostile-fileid");
    let ob = fx.open(None).expect("open");
    assert!(matches!(
        ob.find_file("HomeDomain", "Library/SMS/alias.db"),
        Err(BackupError::FileUnavailable(FileUnavailable::InvalidEntry))
    ));
    assert!(ob
        .find_file("HomeDomain", "Library/SMS/other.db")
        .expect("lookup")
        .is_none());
    let token = oma_backup::CancelToken::new();
    let mut sink = |_: oma_backup::Progress| {};
    let mut ctx = oma_backup::JobCtx::new(&token, &mut sink);
    let report = ob.validate(&mut ctx).expect("validate");
    assert!(ob.close().removed);
    assert_eq!(report.stats.file_id_mismatches, 1);
    assert!(report.importable());
}

fn open_error(name: &str) -> BackupError {
    let fx = Fixture::read_only(name);
    let err = fx.open(Some(PASSWORD)).expect_err(name);
    assert!(err.cleanup.removed, "{name}");
    err.error
}

#[test]
fn huge_dpic_and_iter() {
    assert!(matches!(
        open_error("hostile-dpic"),
        BackupError::LimitExceeded {
            what: LimitKind::Dpic,
            value: 0xFFFF_FFFF,
            cap: 20_000_000
        }
    ));
    assert!(matches!(
        open_error("hostile-iter"),
        BackupError::LimitExceeded {
            what: LimitKind::Iter,
            value: 1_000_001,
            cap: 1_000_000
        }
    ));
}

#[test]
fn truncated_keybag() {
    assert!(matches!(
        open_error("hostile-keybag-truncated"),
        BackupError::Malformed {
            file: BackupFile::Keybag,
            detail: MalformedDetail::Truncated
        }
    ));
}

#[test]
fn no_passphrase_class() {
    assert!(matches!(
        open_error("hostile-wrap-device-only"),
        BackupError::Unsupported(UnsupportedReason::NoPassphraseClasses)
    ));
}

#[test]
fn bad_padding_manifest() {
    assert!(matches!(
        open_error("hostile-padding-manifest"),
        BackupError::Malformed {
            file: BackupFile::ManifestDb,
            detail: MalformedDetail::Padding
        }
    ));
}

#[test]
fn bad_padding_sms() {
    let fx = Fixture::read_only("hostile-padding-sms");
    let (report, cleanup) = fx.validate(Some(PASSWORD));
    assert_eq!(report.sms_db.padding_ok, Some(false));
    assert_eq!(report.sms_db.header_ok, Some(true));
    let m = report.check(CheckId::MessagesDb).expect("messages-db");
    assert_eq!(m.level, CheckLevel::Error);
    assert!(m.findings.contains(&Finding::SmsBadPadding));
    assert!(!report.importable());
    assert!(cleanup.removed);
}

#[test]
fn size_max() {
    let fx = Fixture::read_only("hostile-size-max");
    let (report, cleanup) = fx.validate(Some(PASSWORD));
    assert!(report.stats.declared_bytes_saturated);
    assert_eq!(report.stats.declared_bytes, u64::MAX);
    assert!(report.stats.size_mismatches >= 1);
    let sizes = report.check(CheckId::BlobSizes).expect("blob-sizes");
    assert_eq!(sizes.level, CheckLevel::Ok);
    assert!(sizes.findings.contains(&Finding::DeclaredBytesSaturated));
    assert!(cleanup.removed);
}

#[test]
fn oversize_mbfile() {
    let fx = Fixture::read_only("hostile-mbfile-oversize");
    let ob = fx.open(None).expect("open");
    let token = oma_backup::CancelToken::new();
    let mut sink = |_: oma_backup::Progress| {};
    let mut ctx = oma_backup::JobCtx::new(&token, &mut sink);
    let err = ob.validate(&mut ctx).expect_err("oversize");
    assert!(ob.close().removed);
    assert!(matches!(
        err,
        BackupError::LimitExceeded {
            what: LimitKind::MbFileBytes,
            ..
        }
    ));
}

/// A writable copy of `plain` whose Manifest.db is changed by `sql`.
fn plain_with(sql: &str) -> Fixture {
    let fx = Fixture::new("plain");
    let c = rusqlite::Connection::open(fx.dir.join("Manifest.db")).expect("open");
    c.execute_batch(sql).expect("modify");
    drop(c);
    fx
}

const REBUILD_PREFIX: &str =
    "DROP INDEX FilesDomainIdx; DROP INDEX FilesRelativePathIdx; DROP INDEX FilesFlagsIdx;
     ALTER TABLE Files RENAME TO F0;";

#[test]
fn db_structure() {
    // Files as a virtual table.
    let fx = plain_with(&format!(
        "{REBUILD_PREFIX}
         CREATE VIRTUAL TABLE Files USING fts5(fileID, domain, relativePath, flags, file);
         INSERT INTO Files SELECT * FROM F0; DROP TABLE F0;"
    ));
    let (r, _) = fx.validate(None);
    let schema = r.check(CheckId::ManifestSchema).expect("schema");
    assert_eq!(schema.level, CheckLevel::Error);
    assert!(schema
        .findings
        .contains(&Finding::SchemaProblem("Files is a virtual table")));
    assert!(!r.importable());

    // A generated column.
    let fx = plain_with(&format!(
        "{REBUILD_PREFIX}
         CREATE TABLE Files (fileID TEXT, domain TEXT, relativePath TEXT, flags INTEGER,
                             file BLOB GENERATED ALWAYS AS (NULL) VIRTUAL);
         INSERT INTO Files (fileID, domain, relativePath, flags) SELECT fileID, domain, relativePath, flags FROM F0;
         DROP TABLE F0;"
    ));
    let (r, _) = fx.validate(None);
    assert_eq!(
        r.check(CheckId::ManifestSchema).map(|c| c.level),
        Some(CheckLevel::Error)
    );

    // A trigger: counted, never fired, not an error.
    let fx = plain_with(
        "CREATE TABLE Log (n INTEGER);
         CREATE TRIGGER t AFTER INSERT ON Files BEGIN INSERT INTO Log VALUES (1); END;",
    );
    let (r, _) = fx.validate(None);
    let schema = r.check(CheckId::ManifestSchema).expect("schema");
    assert_eq!(schema.level, CheckLevel::Ok);
    assert!(schema.findings.contains(&Finding::Triggers(1)));
    assert!(r.importable());
    let c = rusqlite::Connection::open(fx.dir.join("Manifest.db")).expect("open");
    let fired: i64 = c
        .query_row("SELECT count(*) FROM Log", [], |r| r.get(0))
        .expect("log");
    assert_eq!(fired, 0);
    drop(c);

    // Duplicate fileIDs: sms.db is ambiguous.
    let fx = plain_with(&format!(
        "{REBUILD_PREFIX}
         CREATE TABLE Files (fileID TEXT, domain TEXT, relativePath TEXT, flags INTEGER, file BLOB);
         INSERT INTO Files SELECT * FROM F0;
         INSERT INTO Files SELECT * FROM F0 WHERE relativePath = 'Library/SMS/sms.db';
         DROP TABLE F0;"
    ));
    let (r, _) = fx.validate(None);
    let m = r.check(CheckId::MessagesDb).expect("messages-db");
    assert_eq!(m.level, CheckLevel::Error);
    assert!(m.findings.contains(&Finding::SmsAmbiguous));
    assert!(r.sms_db.ambiguous);

    // Wrong storage classes: counted, and the pass completes.
    let fx = plain_with(
        "INSERT INTO Files VALUES ('abc', X'4142', 'x', 1, NULL);
         INSERT INTO Files VALUES ('abd', 'HomeDomain', 'y', 'one', NULL);
         INSERT INTO Files VALUES ('abe', 'HomeDomain', 'z', 1, 'text');",
    );
    let (r, _) = fx.validate(None);
    assert_eq!(r.stats.invalid_rows, 3);
    assert_eq!(r.stats.total_rows, 9);
    assert_eq!(
        r.check(CheckId::FileRecords).map(|c| c.level),
        Some(CheckLevel::Warning)
    );
    assert!(r.importable());

    // A corrupt page.
    let fx = Fixture::new("plain");
    corrupt_index_page(&fx.dir.join("Manifest.db"));
    let (r, _) = fx.validate(None);
    let integrity = r.check(CheckId::ManifestIntegrity).expect("integrity");
    assert_eq!(integrity.level, CheckLevel::Error);
    assert!(!r.importable());
}

fn corrupt_index_page(path: &Path) {
    let c = rusqlite::Connection::open(path).expect("open");
    let root: i64 = c
        .query_row(
            "SELECT rootpage FROM sqlite_schema WHERE name = 'FilesRelativePathIdx'",
            [],
            |r| r.get(0),
        )
        .expect("rootpage");
    let page_size: i64 = c
        .query_row("PRAGMA page_size", [], |r| r.get(0))
        .expect("page size");
    drop(c);
    let mut bytes = fs::read(path).expect("read");
    let start = usize::try_from((root - 1) * page_size).expect("offset");
    let end = start + usize::try_from(page_size).expect("size");
    for b in &mut bytes[start + 8..end] {
        *b = 0xA5;
    }
    fs::write(path, &bytes).expect("write");
}

#[cfg(unix)]
#[test]
fn symlink_blob() {
    const SENTINEL: &[u8] = b"OUTSIDE-SENTINEL-CONTENT";
    let sms_blob = |fx: &Fixture| {
        fx.dir
            .join("3d")
            .join("3d0d7e5fb2ce288813306e4d4636395e047a3d28")
    };

    // The blob itself is a link to a file outside the backup.
    let fx = Fixture::new("plain");
    let outside = fx.tmp.path().join("outside-file");
    fs::write(&outside, SENTINEL).expect("write");
    fs::remove_file(sms_blob(&fx)).expect("rm");
    std::os::unix::fs::symlink(&outside, sms_blob(&fx)).expect("symlink");
    check_unsafe_blob(&fx, SENTINEL);

    // The 3d/ directory is a link to a directory outside the backup.
    let fx = Fixture::new("plain");
    let outside_dir = fx.tmp.path().join("outside-dir");
    fs::create_dir(&outside_dir).expect("mkdir");
    let mut fake = SENTINEL.to_vec();
    fake.resize(8192, b'S');
    fs::write(
        outside_dir.join("3d0d7e5fb2ce288813306e4d4636395e047a3d28"),
        &fake,
    )
    .expect("write");
    fs::rename(fx.dir.join("3d"), fx.tmp.path().join("moved-3d")).expect("mv");
    std::os::unix::fs::symlink(&outside_dir, fx.dir.join("3d")).expect("symlink");
    check_unsafe_blob(&fx, SENTINEL);
}

#[cfg(unix)]
fn check_unsafe_blob(fx: &Fixture, sentinel: &[u8]) {
    let ob = fx.open(None).expect("open");
    let token = oma_backup::CancelToken::new();
    let mut seen = Vec::new();
    let staging = fx.library.join(".oma-staging");
    let mut sink = |_: oma_backup::Progress| {
        let mut files = Vec::new();
        walk(&staging, &mut files);
        for f in files {
            if let Ok(bytes) = fs::read(&f) {
                if bytes.windows(sentinel.len()).any(|w| w == sentinel) {
                    seen.push(f);
                }
            }
        }
    };
    let mut ctx = oma_backup::JobCtx::new(&token, &mut sink);
    let report = ob.validate(&mut ctx).expect("validate");
    let entry = ob
        .find_file("HomeDomain", "Library/SMS/sms.db")
        .expect("lookup")
        .expect("listed");
    let mut sink = |_: oma_backup::Progress| {};
    let mut ctx = oma_backup::JobCtx::new(&token, &mut sink);
    assert!(matches!(
        ob.extract_file(&entry, &mut ctx),
        Err(BackupError::FileUnavailable(
            FileUnavailable::BlobNotRegular
        ))
    ));
    assert!(ob.close().removed);
    assert!(seen.is_empty(), "outside content reached staging: {seen:?}");
    assert!(report.stats.unsafe_blobs >= 1);
    let m = report.check(CheckId::MessagesDb).expect("messages-db");
    assert_eq!(m.level, CheckLevel::Error);
    assert!(m.findings.contains(&Finding::SmsBlobNotRegular));
    let shown = format!("{report:?}");
    assert!(!shown.contains("OUTSIDE-SENTINEL"));
}
