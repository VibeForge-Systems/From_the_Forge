//! Unencrypted fixtures: validation end to end.
mod common;

use std::cell::RefCell;
use std::fs;

use common::*;
use oma_backup::{
    CancelToken, CheckId, CheckLevel, Classification, Finding, JobCtx, Limits, ManifestStats,
    OpenMode, Progress,
};

/// The generator's base content: 3 directories (one domain root), sms.db,
/// one attachment, one symlink.
fn base_stats(declared: u64) -> ManifestStats {
    ManifestStats {
        total_rows: 6,
        files: 2,
        directories: 3,
        symlinks: 1,
        declared_bytes: declared,
        has_properties_table: true,
        ..ManifestStats::default()
    }
}

#[test]
fn validates() {
    let fx = Fixture::read_only("plain");
    assert_eq!(fx.inspect().classification, Classification::Ready);
    let sms_len = fs::metadata(fixtures_dir().join("plain.expected/sms.db"))
        .expect("expected sms")
        .len();

    // Watch staging at every progress report: no sms.db may ever appear.
    let staging = fx.library.join(".oma-staging");
    let violations = RefCell::new(0u32);
    let token = CancelToken::new();
    let mut sink = |_: Progress| {
        let mut files = Vec::new();
        walk(&staging, &mut files);
        if files.iter().any(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().contains("sms.db"))
        }) {
            *violations.borrow_mut() += 1;
        }
    };
    let ob = fx
        .open_with(None, &Limits::default(), &mut sink, &token)
        .expect("open");
    let report = {
        let mut sink2 = |_: Progress| {
            let mut files = Vec::new();
            walk(&staging, &mut files);
            if files.iter().any(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().contains("sms.db"))
            }) {
                *violations.borrow_mut() += 1;
            }
        };
        let mut ctx = JobCtx::new(&token, &mut sink2);
        ob.validate(&mut ctx).expect("validate")
    };
    let cleanup = ob.close();
    assert_eq!(
        *violations.borrow(),
        0,
        "a plaintext sms.db appeared in staging"
    );

    assert!(report.importable(), "{:?}", report.checks);
    assert!(report.sms_db.listed);
    assert_eq!(report.sms_db.blob_present, Some(true));
    assert_eq!(report.sms_db.header_ok, Some(true));
    assert_eq!(report.sms_db.padding_ok, None);
    assert_eq!(report.sms_db.size_consistent, Some(true));
    assert!(!report.sms_db.wal_listed && !report.sms_db.shm_listed);
    assert_eq!(report.stats, base_stats(sms_len + 87));
    assert!(report.keybag_notes.is_empty());
    assert!(cleanup.removed);
    assert!(fx.staged_files().is_empty());
    let ids: Vec<&str> = report.checks.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "status",
            "format",
            "encryption",
            "manifest-copy",
            "manifest-integrity",
            "manifest-schema",
            "file-records",
            "file-ids",
            "blobs",
            "blob-sizes",
            "paths",
            "key-classes",
            "messages-db",
            "unchanged"
        ]
    );
}

#[test]
fn xml_plists() {
    let fx = Fixture::read_only("plain-xml");
    let inspection = fx.inspect();
    assert_eq!(inspection.classification, Classification::Ready);
    assert_eq!(
        inspection
            .device_name()
            .map(|s| s.value.as_str().to_owned()),
        Some(DEVICE_NAME.to_owned())
    );
    let (report, cleanup) = fx.validate(None);
    assert!(report.importable(), "{:?}", report.checks);
    assert_eq!(
        report.stats.unreadable_records, 0,
        "XML MBFile records decode"
    );
    assert_eq!(report.stats.total_rows, 6);
    assert_eq!(report.sms_db.header_ok, Some(true));
    assert!(cleanup.removed);
}

#[test]
fn wal_manifest() {
    let fx = Fixture::read_only("plain-wal");
    assert!(fx.dir.join("Manifest.db-wal").is_file());
    assert!(!fx.dir.join("Manifest.db-shm").exists());
    let inspection = fx.inspect();
    assert!(inspection.layout.has_manifest_db_wal);
    assert!(!inspection.layout.has_manifest_db_shm);

    let ob = fx.open(None).expect("open");
    let mode = ob.manifest_open_mode();
    // Recorded for docs/supported-formats.md (§7.2): how a staged WAL
    // database without a -shm was opened.
    eprintln!("plain-wal staged Manifest.db opened as {mode:?}");
    assert!(matches!(
        mode,
        Some(OpenMode::ReadOnly | OpenMode::ReadWriteQueryOnly)
    ));
    let token = CancelToken::new();
    let mut sink = |_: Progress| {};
    let mut ctx = JobCtx::new(&token, &mut sink);
    let report = ob.validate(&mut ctx).expect("validate");
    let staged = ob.staging_path().expect("staging").join("m");
    assert!(staged.join("Manifest.db-wal").exists() || mode == Some(OpenMode::ReadWriteQueryOnly));
    let cleanup = ob.close();

    // The two rows that live only in the -wal are counted.
    assert_eq!(report.stats.total_rows, 8);
    assert_eq!(report.stats.directories, 5);
    let copy = report.check(CheckId::ManifestCopy).expect("manifest-copy");
    assert_eq!(copy.level, CheckLevel::Ok);
    assert!(copy.findings.contains(&Finding::WalApplied));
    assert!(report.importable());
    assert!(cleanup.removed);
    // The backup still has no -shm: nothing was opened in place.
    assert!(!fx.dir.join("Manifest.db-shm").exists());
}

#[test]
fn no_keybag_is_ready_with_a_note() {
    let fx = Fixture::read_only("plain-no-keybag");
    let inspection = fx.inspect();
    assert_eq!(inspection.classification, Classification::Ready);
    assert!(inspection
        .notes
        .iter()
        .any(|n| n.kind == oma_backup::NoteKind::NoKeybag));
    let (report, _) = fx.validate(None);
    assert!(report.importable());
}

#[test]
fn a_password_for_an_unencrypted_backup_is_ignored() {
    let fx = Fixture::read_only("plain");
    let (report, cleanup) = fx.validate(Some("ignored"));
    assert!(report.importable());
    assert!(cleanup.removed);
}
