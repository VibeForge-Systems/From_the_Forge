//! `check()` end to end on databases built in the test, for paths the
//! catalogue does not reach: qualifiers no catalogue entry produces
//! (`NonUtf8Encoding`, `WrongCaseColumns`, `SourceChanged`), a counter whose
//! column is absent, and an sms.db that cannot be opened or read. Every
//! name and value is synthetic.
#![allow(clippy::disallowed_types, clippy::disallowed_methods)]
mod common;

use std::fs;

use common::*;
use oma_backup::{BackupError, CancelToken, MalformedDetail, SMS_DB_FILE_ID};
use oma_messages::{
    check, BackupStage, CheckOptions, ExtractionReport, ExtractionStatus, KnownTable, MessagesCtx,
    MessagesError, MessagesPhase, MessagesProgress, Qualifier,
};
use rusqlite::Connection;

/// The minimal schema with one required table's DDL replaced.
fn minimal_with(t: KnownTable, ddl: &str) -> String {
    KnownTable::ALL
        .iter()
        .filter(|k| k.required())
        .map(|k| {
            if *k == t {
                format!("{ddl};\n")
            } else {
                create_table(*k, true)
            }
        })
        .collect()
}

fn readable(report: &ExtractionReport) -> &[Qualifier] {
    match &report.status {
        ExtractionStatus::Readable { qualifiers } => qualifiers,
        other => panic!("not readable: {other:?}"),
    }
}

/// Run `check()` on `main` as the sms.db and assert the staging is gone.
fn check_db(main: &[u8]) -> Result<ExtractionReport, MessagesError> {
    let fx = Fixture::with_sms(main);
    let (result, cleanup) = run_check(&fx, None, &CheckOptions::default());
    assert!(cleanup.removed);
    assert!(fx.staged_files().is_empty());
    result
}

#[test]
fn a_utf16_database_is_qualified_as_not_utf8() {
    for encoding in ["UTF-16le", "UTF-16be"] {
        let main = build_with(|c: &Connection| {
            c.execute_batch(&format!("PRAGMA encoding = '{encoding}';"))
                .expect("encoding");
            c.execute_batch(&minimal_schema()).expect("schema");
            c.execute(
                "INSERT INTO handle (id) VALUES (?1)",
                ["synthetic-handle@example.invalid"],
            )
            .expect("insert");
        });
        let report = check_db(&main).expect("check");
        assert!(
            readable(&report).contains(&Qualifier::NonUtf8Encoding),
            "{encoding}: {:?}",
            report.status
        );
    }
    // UTF-8 carries no such qualifier.
    let report = check_db(&build(&minimal_schema())).expect("check");
    assert!(!readable(&report).contains(&Qualifier::NonUtf8Encoding));
}

#[test]
fn a_wrong_case_column_is_qualified() {
    let ddl = minimal_with(
        KnownTable::Handle,
        "CREATE TABLE handle (ROWID INTEGER PRIMARY KEY, id TEXT, Service TEXT)",
    );
    let report = check_db(&build(&ddl)).expect("check");
    assert!(
        readable(&report).contains(&Qualifier::WrongCaseColumns(1)),
        "{:?}",
        report.status
    );
}

#[test]
fn a_backup_changed_during_the_check_is_qualified() {
    let mut sql = full_schema();
    sql.push_str("INSERT INTO handle (id) VALUES ('synthetic-handle-1@example.invalid');");
    let fx = Fixture::with_sms(&build(&sql));
    let blob = fx.dir.join(&SMS_DB_FILE_ID[..2]).join(SMS_DB_FILE_ID);
    let ob = fx.open(None);
    let token = CancelToken::new();
    let mut changed = false;
    // After staging, the source sms.db blob grows: the backup is being
    // rewritten while the staged copy is read.
    let mut sink = |p: MessagesProgress| {
        if p.phase == MessagesPhase::ReadingHandles && !changed {
            changed = true;
            let mut bytes = fs::read(&blob).expect("blob");
            bytes.extend_from_slice(&[0u8; 512]);
            fs::write(&blob, bytes).expect("rewrite");
        }
    };
    let mut ctx = MessagesCtx::new(&token, &mut sink);
    let report = check(&ob, &CheckOptions::default(), &mut ctx).expect("check");
    assert!(ob.close().removed);
    assert!(report.database.source_changed_during_check);
    assert!(
        readable(&report).contains(&Qualifier::SourceChanged),
        "{:?}",
        report.status
    );
}

/// Without `associated_message_guid` no association target can be
/// collected, so "targets not found" is not recorded (`None`), never 0.
#[test]
fn association_targets_are_not_recorded_without_their_guid_column() {
    let mut sql = minimal_schema();
    sql.push_str(
        "ALTER TABLE message ADD COLUMN associated_message_type INTEGER;
         INSERT INTO message (guid, date, is_from_me, associated_message_type)
             VALUES ('SYNTH-MSG-GUID-1', 0, 0, 2000);",
    );
    let report = check_db(&build(&sql)).expect("check");
    readable(&report);
    let assoc = report
        .messages
        .as_ref()
        .and_then(|m| m.associations.as_ref())
        .expect("associations are counted from the type column");
    assert_eq!(assoc.target_not_found, None);

    // With the GUID column the count is recorded.
    let mut sql = minimal_schema();
    sql.push_str(
        "ALTER TABLE message ADD COLUMN associated_message_type INTEGER;
         ALTER TABLE message ADD COLUMN associated_message_guid TEXT;
         INSERT INTO message (guid, date, is_from_me, associated_message_type, associated_message_guid)
             VALUES ('SYNTH-MSG-GUID-1', 0, 0, 2000, 'p:0/SYNTH-MSG-GUID-MISSING');",
    );
    let report = check_db(&build(&sql)).expect("check");
    let assoc = report
        .messages
        .as_ref()
        .and_then(|m| m.associations.as_ref())
        .expect("associations");
    assert_eq!(assoc.target_not_found, Some(1));
}

/// A database large enough to need several pages, with its schema on page 1.
fn multi_page() -> Vec<u8> {
    let mut sql = full_schema();
    sql.push_str(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 2000)
         INSERT INTO handle (id) SELECT 'synthetic-handle-' || i || '@example.invalid' FROM n;",
    );
    let main = build(&sql);
    assert!(main.len() > 16 * 4096);
    main
}

/// What opening a damaged copy fails with: SQLite's `SQLITE_CORRUPT` (11)
/// while the schema loads, at the opening step. Never `Internal`.
fn assert_corrupt_at_open(r: Result<ExtractionReport, MessagesError>) {
    match r {
        Err(MessagesError::Backup {
            source: BackupError::Sqlite { code: Some(11) },
            stage: BackupStage::Opening,
        }) => {}
        other => panic!("{:?}", other.map(|r| r.status)),
    }
}

#[test]
fn a_truncated_sms_db_fails_to_open_and_cleans_up() {
    let main = multi_page();
    let page = 4096;
    for keep in [page, 3 * page, main.len() / 2] {
        assert_corrupt_at_open(check_db(&main[..keep]));
    }
}

#[test]
fn a_damaged_first_page_fails_to_open_and_cleans_up() {
    let mut main = multi_page();
    // Keep the 100-byte header; overwrite the schema's b-tree page.
    for b in &mut main[100..4096] {
        *b = 0xA5;
    }
    assert_corrupt_at_open(check_db(&main));
    // Not a database at all.
    match check_db(&[0x42u8; 8192]) {
        Err(MessagesError::Backup {
            source:
                BackupError::Malformed {
                    detail: MalformedDetail::NotSqlite,
                    ..
                },
            stage: BackupStage::Opening,
        }) => {}
        other => panic!("{:?}", other.map(|r| r.status)),
    }
}

#[test]
fn an_empty_sms_db_has_no_tables() {
    let report = check_db(&[]).expect("check");
    assert!(
        matches!(report.status, ExtractionStatus::Unsupported(_)),
        "{:?}",
        report.status
    );
}
