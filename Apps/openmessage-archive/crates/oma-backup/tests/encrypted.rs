//! Encrypted fixtures: unlocking, decryption and keybag behaviour.
mod common;

use std::fs;

use common::*;
use oma_backup::{
    BackupError, BackupFile, CancelToken, CheckId, CheckLevel, Classification, Finding, JobCtx,
    KeybagNote, LimitKind, Limits, MalformedDetail, Phase, Progress,
};

#[test]
fn validates() {
    let fx = Fixture::read_only("enc");
    assert_eq!(fx.inspect().classification, Classification::NeedsPassword);
    let ob = fx.open(Some(PASSWORD)).expect("open");
    assert!(ob.is_encrypted());
    let staged = ob
        .staging_path()
        .expect("staging")
        .join("m")
        .join("Manifest.db");
    let expected = fs::read(fixtures_dir().join("enc.expected/Manifest.db")).expect("expected");
    assert_eq!(fs::read(&staged).expect("staged"), expected);

    let token = CancelToken::new();
    let mut sink = |_: Progress| {};
    let mut ctx = JobCtx::new(&token, &mut sink);
    let report = ob.validate(&mut ctx).expect("validate");
    let mut staged_files = Vec::new();
    walk(ob.staging_path().expect("staging"), &mut staged_files);
    assert!(
        !staged_files
            .iter()
            .any(|p| p.to_string_lossy().contains("sms.db")),
        "no plaintext sms.db in staging"
    );
    let cleanup = ob.close();

    assert!(report.importable(), "{:?}", report.checks);
    assert_eq!(report.sms_db.header_ok, Some(true));
    assert_eq!(report.sms_db.padding_ok, Some(true));
    assert_eq!(report.sms_db.size_consistent, Some(true));
    assert_eq!(report.stats.size_mismatches, 0);
    assert_eq!(report.stats.files_without_key, 0);
    assert_eq!(report.stats.files_with_unavailable_class, 0);
    assert_eq!(report.stats.total_rows, 6);
    assert_eq!(
        report.check(CheckId::Encryption).map(|c| c.level),
        Some(CheckLevel::Ok)
    );
    assert!(cleanup.removed);
    assert!(fx.staged_files().is_empty());
}

#[test]
fn xml() {
    let fx = Fixture::read_only("enc-xml");
    let (report, cleanup) = fx.validate(Some(PASSWORD));
    assert!(report.importable(), "{:?}", report.checks);
    assert_eq!(report.sms_db.header_ok, Some(true));
    assert_eq!(report.sms_db.padding_ok, Some(true));
    assert!(cleanup.removed);
}

#[test]
fn wal_unapplied() {
    let fx = Fixture::read_only("enc-wal");
    let (report, cleanup) = fx.validate(Some(PASSWORD));
    let copy = report.check(CheckId::ManifestCopy).expect("manifest-copy");
    assert_eq!(copy.level, CheckLevel::Warning);
    assert!(copy.findings.contains(&Finding::WalNotApplied));
    // Only the checkpointed rows: the -wal was not applied.
    assert_eq!(report.stats.total_rows, 6);
    assert!(report.importable());
    assert!(cleanup.removed);
}

#[test]
fn wrong_password() {
    let fx = Fixture::read_only("enc");
    let err = fx
        .open(Some("not-the-password"))
        .expect_err("wrong password");
    assert!(
        matches!(err.error, BackupError::WrongPassword),
        "{:?}",
        err.error
    );
    assert!(err.cleanup.removed);
    assert!(fx.staged_files().is_empty());
    let sessions: Vec<_> = fs::read_dir(fx.library.join(".oma-staging"))
        .expect("staging")
        .collect();
    assert!(sessions.is_empty(), "no staging directory remains");
}

#[test]
fn missing_or_empty_password() {
    let fx = Fixture::read_only("enc");
    let err = fx.open(None).expect_err("no password");
    assert!(matches!(err.error, BackupError::PasswordRequired));
    assert!(err.cleanup.removed);
    let err = fx.open(Some("")).expect_err("empty password");
    assert!(matches!(err.error, BackupError::PasswordRequired));
}

#[test]
fn unicode_password() {
    let fx = Fixture::read_only("enc-unicode-pw");
    let (report, cleanup) = fx.validate(Some(UNICODE_PASSWORD));
    assert!(report.importable());
    assert!(cleanup.removed);
    // The ASCII default password does not unlock it.
    assert!(matches!(
        fx.open(Some(PASSWORD)).expect_err("wrong").error,
        BackupError::WrongPassword
    ));
}

#[test]
fn cancel_during_kdf() {
    let fx = Fixture::read_only("enc-slow");
    let token = CancelToken::new();
    let mut reports = Vec::new();
    let t = token.clone();
    let mut sink = |p: Progress| {
        if p.phase == Phase::DerivingKey {
            reports.push(p);
            t.cancel();
        }
    };
    let err = fx
        .open_with(Some(PASSWORD), &Limits::default(), &mut sink, &token)
        .expect_err("cancelled");
    assert!(matches!(err.error, BackupError::Cancelled));
    assert_eq!(reports.len(), 1, "cancelled before the next report");
    assert!(err.cleanup.removed);
    assert!(fx.staged_files().is_empty());
}

#[test]
fn keybag_divergences() {
    for (name, note) in [
        ("enc-type2", KeybagNote::UnexpectedType(2)),
        ("enc-clas-unused-0x10", KeybagNote::UnusualClass(0x10)),
        ("enc-wpky39-unused", KeybagNote::BadWrappedKeyLength(12)),
    ] {
        let fx = Fixture::read_only(name);
        let (report, cleanup) = fx.validate(Some(PASSWORD));
        assert!(report.importable(), "{name}: {:?}", report.checks);
        assert!(
            report.keybag_notes.contains(&note),
            "{name}: {:?}",
            report.keybag_notes
        );
        let enc = report.check(CheckId::Encryption).expect("encryption");
        assert_eq!(enc.level, CheckLevel::Warning, "{name}");
        assert!(cleanup.removed);
    }
}

#[test]
fn duplicate_clas() {
    let fx = Fixture::read_only("enc-dup-clas");
    let err = fx.open(Some(PASSWORD)).expect_err("dup clas");
    assert!(
        matches!(
            err.error,
            BackupError::Malformed {
                file: BackupFile::Keybag,
                detail: MalformedDetail::DuplicateClass(3)
            }
        ),
        "{:?}",
        err.error
    );
    assert!(err.cleanup.removed);
}

#[test]
fn encryption_key_shapes() {
    // Rust-only shapes: the reference implementation handles only NS.data.
    for name in ["enc-key-uid-data", "enc-key-inline"] {
        let fx = Fixture::read_only(name);
        let (report, cleanup) = fx.validate(Some(PASSWORD));
        assert!(report.importable(), "{name}: {:?}", report.checks);
        assert_eq!(report.sms_db.padding_ok, Some(true), "{name}");
        assert_eq!(report.stats.unreadable_records, 0, "{name}");
        assert!(cleanup.removed);
    }
}

#[test]
fn slow_fixture_reports_derivation_progress() {
    let fx = Fixture::read_only("enc-slow");
    let token = CancelToken::new();
    let mut last = None;
    let mut sink = |p: Progress| {
        if p.phase == Phase::DerivingKey {
            if let Some(prev) = last {
                assert!(p.done > prev, "monotonic");
            }
            last = Some(p.done);
            assert_eq!(p.total, Some(2_000_000 + 2 * 10));
        }
    };
    let ob = fx
        .open_with(Some(PASSWORD), &Limits::default(), &mut sink, &token)
        .expect("open");
    assert!(ob.close().removed);
    assert_eq!(last, Some(2_000_020));
}

#[test]
fn hostile_iteration_counts_never_start_derivation() {
    for (name, kind) in [
        ("hostile-dpic", LimitKind::Dpic),
        ("hostile-iter", LimitKind::Iter),
    ] {
        let fx = Fixture::read_only(name);
        let token = CancelToken::new();
        let mut sink = |p: Progress| assert_ne!(p.phase, Phase::DerivingKey, "derivation started");
        let err = fx
            .open_with(Some(PASSWORD), &Limits::default(), &mut sink, &token)
            .expect_err("refused");
        match err.error {
            BackupError::LimitExceeded { what, .. } => assert_eq!(what, kind, "{name}"),
            other => panic!("{name}: {other:?}"),
        }
        assert!(err.cleanup.removed);
    }
}
