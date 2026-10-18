//! sms.db's `-wal` (M3 §3.3, §19.3 `wal.rs`).
//!
//! The catalogue's WAL fixtures hold two messages and a link only in the
//! `-wal`. These tests are the first evidence that a staged sms.db `-wal`
//! is read, plain and encrypted, on the platform that runs them. The report
//! says "staged", with the number of frames our parser validated; that
//! SQLite applied them is inferred from the counts, never claimed.
//!
//! The encrypted case proves only the generator's model of an encrypted
//! sms.db `-wal` (an ordinary per-file-key blob), which oma-backup decrypts.
#![allow(clippy::disallowed_types, clippy::disallowed_methods)]
mod common;

use common::*;
use oma_backup::{FileUnavailable, WalOutcome};
use oma_messages::{CheckOptions, ExtractionReport, Qualifier};
use rusqlite::Connection;

fn check(name: &str) -> ExtractionReport {
    let fx = Fixture::messages(name);
    let (report, cleanup) = run_check(&fx, catalogue_password(name), &CheckOptions::default());
    assert!(cleanup.removed, "{name}");
    report.unwrap_or_else(|e| panic!("{name}: {e:?}"))
}

#[test]
fn rows_only_in_the_wal_are_counted() {
    for name in ["msgs-wal-plain", "msgs-wal-enc"] {
        let r = check(name);
        assert!(
            matches!(r.database.wal.outcome, WalOutcome::Staged { .. }),
            "{name}"
        );
        assert_eq!(r.database.wal.header_says_wal, Some(true), "{name}");
        let facts = r.database.wal.facts.expect("facts");
        assert!(facts.header_valid && facts.page_size_matches, "{name}");
        assert!(facts.valid_committed_frames > 0, "{name}");
        assert_eq!(facts.frames_after_first_invalid, 0, "{name}");
        assert_eq!(
            r.messages.as_ref().expect("messages").rows,
            7,
            "{name}: 5 + 2 in the -wal"
        );
        assert_eq!(
            r.links.as_ref().expect("links").chat_message_rows,
            6,
            "{name}"
        );
        assert!(r.qualifiers().is_empty(), "{name}: {:?}", r.qualifiers());
    }
}

#[test]
fn a_listed_wal_without_its_blob_is_skipped() {
    let r = check("msgs-wal-missing-plain");
    assert_eq!(
        r.database.wal.outcome,
        WalOutcome::Skipped(FileUnavailable::BlobMissing)
    );
    assert!(r.database.wal.listed);
    assert_eq!(r.database.wal.facts, None);
    assert_eq!(r.qualifiers(), [Qualifier::WalSkipped]);
    assert_eq!(r.messages.as_ref().expect("messages").rows, 5);
}

#[test]
fn frames_after_a_bad_one_are_not_used() {
    let r = check("msgs-wal-badframes-plain");
    let facts = r.database.wal.facts.expect("facts");
    assert!(facts.frames_after_first_invalid > 0);
    assert!(facts.valid_committed_frames > 0);
    assert_eq!(r.qualifiers(), [Qualifier::WalFramesInvalid]);
    // The first transaction's two messages; not the corrupted second one.
    assert_eq!(r.messages.as_ref().expect("messages").rows, 7);
}

/// No `-wal` is listed, but the main file's header says WAL mode (bytes
/// 18–19 are 2): the newest messages may be missing.
#[test]
fn an_unlisted_wal_with_a_wal_header_is_stated() {
    let main = build_with(|c: &Connection| {
        c.execute_batch(&full_schema()).expect("schema");
        let mode: String = c
            .query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))
            .expect("wal");
        assert_eq!(mode, "wal");
        c.execute_batch("INSERT INTO handle (id) VALUES ('synthetic-handle-9@example.invalid');")
            .expect("insert");
    });
    assert_eq!((main[18], main[19]), (2, 2), "the header says WAL");
    let fx = Fixture::with_sms(&main);
    let (report, cleanup) = run_check(&fx, None, &CheckOptions::default());
    assert!(cleanup.removed);
    let r = report.expect("check");
    assert_eq!(r.database.wal.outcome, WalOutcome::NotListed);
    assert_eq!(r.database.wal.header_says_wal, Some(true));
    assert!(
        r.qualifiers().contains(&Qualifier::WalStateUnknown),
        "{:?}",
        r.qualifiers()
    );
    // The size the M2 fixture's Manifest declares is the stand-in's, not
    // this database's.
    assert!(r.qualifiers().contains(&Qualifier::SizeMismatch));
}

/// A listed `-wal` of zero bytes (SQLite truncates the file) is staged and
/// is not "invalid": no frame can be lost. Found by oracle run 4, whose
/// macOS sample carries an empty `-wal`.
#[test]
fn an_empty_wal_is_not_invalid() {
    let fx = Fixture::m2("plain");
    // The fileID of HomeDomain-Library/SMS/sms.db-wal (M3 §3.11).
    const WAL_ID: &str = "cd47480f213dba9bc38ee792775d17e3f5a73a59";
    let c = Connection::open(fx.dir.join("Manifest.db")).expect("manifest");
    c.execute(
        "INSERT INTO Files (fileID, domain, relativePath, flags, file)
         SELECT ?1, domain, 'Library/SMS/sms.db-wal', flags, file FROM Files
         WHERE relativePath = 'Library/SMS/sms.db'",
        [WAL_ID],
    )
    .expect("insert");
    drop(c);
    let dir = fx.dir.join(&WAL_ID[..2]);
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join(WAL_ID), b"").expect("empty -wal");
    let main = build(&full_schema());
    let blob = fx
        .dir
        .join(&oma_backup::SMS_DB_FILE_ID[..2])
        .join(oma_backup::SMS_DB_FILE_ID);
    std::fs::write(blob, &main).expect("sms blob");

    let (report, cleanup) = run_check(&fx, None, &CheckOptions::default());
    assert!(cleanup.removed);
    let r = report.expect("check");
    assert_eq!(r.database.wal.outcome, WalOutcome::Staged { bytes: 0 });
    let facts = r.database.wal.facts.expect("facts");
    assert!(!facts.header_valid);
    assert_eq!(facts.frames_after_first_invalid, 0);
    assert!(
        !r.qualifiers().contains(&Qualifier::WalFramesInvalid),
        "{:?}",
        r.qualifiers()
    );
}
