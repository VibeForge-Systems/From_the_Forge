//! M3 additions to staged databases (M3 §3, §19.3 "oma-backup tests"):
//! the authorizer and the other SQL guards, op budgets, the schema-load
//! retry, best-effort WAL staging, header and WAL facts, the content
//! digest, blob stamps, combined progress, the size cap and `remove`.
//!
//! Each test builds its sms.db stand-in with SQLite in a temporary
//! directory and swaps it into a copy of the synthetic `plain` fixture.
//! The committed fixtures are never written.
mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use common::*;
use oma_backup::{
    BackupError, Budgeted, CancelToken, FileUnavailable, JobCtx, LimitKind, Limits, OpenBackup,
    OpenMode, Progress, ReadPolicy, StagedDatabase, StagedDbLimits, TextEncoding, WalOutcome,
    WalPolicy, SMS_DB_DOMAIN, SMS_DB_FILE_ID, SMS_DB_PATH,
};
use rusqlite::config::DbConfig;
use rusqlite::Connection;
use sha2::{Digest, Sha256};

/// `SHA1("HomeDomain-Library/SMS/sms.db-wal")`.
const SMS_WAL_FILE_ID: &str = "cd47480f213dba9bc38ee792775d17e3f5a73a59";

/// The policy `oma-messages` uses: `part_text` is never read.
const POLICY: ReadPolicy = ReadPolicy::deny_columns(&[("recoverable_message_part", "part_text")]);

const SCHEMA: &str = "
    CREATE TABLE message (ROWID INTEGER PRIMARY KEY AUTOINCREMENT, guid TEXT, text TEXT);
    CREATE TABLE recoverable_message_part (chat_id INTEGER, message_id INTEGER, part_index INTEGER, part_text BLOB);
    INSERT INTO recoverable_message_part VALUES (1, 1, 0, 'SYNTH-DELETED-0');
";

fn blob(dir: &Path, file_id: &str) -> PathBuf {
    dir.join(&file_id[..2]).join(file_id)
}

/// A synthetic sms.db built by `build`, as its main file and (when
/// `wal_rows > 0`) a `-wal` holding that many rows that are not in the main
/// file.
struct SmsDb {
    main: Vec<u8>,
    wal: Option<Vec<u8>>,
}

fn make_sms(build: &str, wal_rows: u32) -> SmsDb {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("sms.db");
    let c = Connection::open(&path).expect("create");
    if wal_rows == 0 {
        c.execute_batch(build).expect("schema");
        drop(c);
        return SmsDb {
            main: fs::read(&path).expect("read"),
            wal: None,
        };
    }
    let mode: String = c
        .query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))
        .expect("wal");
    assert_eq!(mode, "wal");
    c.execute_batch(build).expect("schema");
    c.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
        .expect("checkpoint");
    c.execute_batch("PRAGMA wal_autocheckpoint=0;")
        .expect("no autocheckpoint");
    c.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
        .expect("no checkpoint on close");
    for i in 0..wal_rows {
        c.execute(
            "INSERT INTO message (guid, text) VALUES (?1, 'SYNTH-BODY-wal')",
            [format!("SYNTH-GUID-{i}")],
        )
        .expect("insert");
    }
    drop(c);
    let wal = fs::read(tmp.path().join("sms.db-wal")).expect("wal kept");
    assert!(!wal.is_empty());
    SmsDb {
        main: fs::read(&path).expect("read"),
        wal: Some(wal),
    }
}

/// A writable copy of `plain` whose sms.db blob is `sms.main`, and whose
/// Manifest.db lists a `-wal` when `list_wal` is set (its blob written only
/// when `sms.wal` is `Some`).
fn fixture_with(sms: &SmsDb, list_wal: bool) -> Fixture {
    let fx = Fixture::new("plain");
    fs::write(blob(&fx.dir, SMS_DB_FILE_ID), &sms.main).expect("sms blob");
    if list_wal {
        let m = Connection::open(fx.dir.join("Manifest.db")).expect("manifest");
        m.execute(
            "INSERT INTO Files (fileID, domain, relativePath, flags, file) VALUES (?1, ?2, ?3, 1, NULL)",
            [SMS_WAL_FILE_ID, SMS_DB_DOMAIN, "Library/SMS/sms.db-wal"],
        )
        .expect("list -wal");
        drop(m);
        if let Some(wal) = &sms.wal {
            fs::create_dir_all(fx.dir.join("cd")).expect("mkdir");
            fs::write(blob(&fx.dir, SMS_WAL_FILE_ID), wal).expect("wal blob");
        }
    }
    fx
}

fn stage<'a>(ob: &'a OpenBackup, policy: WalPolicy) -> Result<StagedDatabase<'a>, BackupError> {
    let token = CancelToken::new();
    let mut sink = |_: Progress| {};
    let mut ctx = JobCtx::new(&token, &mut sink);
    ob.extract_database_with(SMS_DB_DOMAIN, SMS_DB_PATH, policy, &mut ctx)
}

fn count(c: &oma_backup::StagedConnection<'_>, sql: &str) -> i64 {
    c.prepare_read(sql)
        .expect("prepare")
        .query_row([], |r| r.get(0))
        .expect("query")
}

#[test]
fn sms_wal_file_id_known_answer() {
    use sha1::Sha1;
    let digest = Sha1::digest(b"HomeDomain-Library/SMS/sms.db-wal");
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(hex, SMS_WAL_FILE_ID);
}

/// Every guard of §7.5 on a staged copy, without and with a `-wal`. Both
/// open read-only on Linux CI; the read-write `query_only` fallback is
/// platform-dependent, so the same guards are also run on a read-write
/// connection by the unit test `staged_conn::tests::guards_hold_on_a_read_write_connection`.
#[test]
fn sql_guards() {
    let mut modes = Vec::new();
    for wal_rows in [0, 3] {
        let sms = make_sms(SCHEMA, wal_rows);
        let fx = fixture_with(&sms, wal_rows > 0);
        let ob = fx.open(None).expect("open");
        let staged = stage(&ob, WalPolicy::BestEffort).expect("stage");
        let c = staged
            .open_with(&CancelToken::new(), &StagedDbLimits::ceiling(), &POLICY)
            .expect("open staged");
        modes.push(c.mode());

        // Allowed: the forms oma-messages uses.
        assert_eq!(
            count(&c, "SELECT count(*) FROM recoverable_message_part"),
            1
        );
        assert_eq!(
            count(&c, "SELECT count(*) FROM message"),
            i64::from(wal_rows)
        );
        let scan =
            "SELECT type, name, tbl_name, sql IS NULL, substr(sql, 1, 32) FROM sqlite_schema";
        assert!(c.prepare_read(scan).expect("scan").query([]).is_ok());
        let mut xinfo = c
            .prepare_read("SELECT name, type, pk, hidden FROM pragma_table_xinfo(?1)")
            .expect("table_xinfo is allowed");
        let cols: Vec<String> = xinfo
            .query_map(["message"], |r| r.get(0))
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("rows");
        assert_eq!(cols, ["ROWID", "guid", "text"]);
        drop(xinfo);
        let mut il = c
            .prepare_read("SELECT origin FROM pragma_index_list(?1)")
            .expect("index_list is allowed");
        assert!(il.query(["message"]).is_ok());
        drop(il);
        let qc: String = c
            .prepare_read("PRAGMA quick_check(1)")
            .expect("quick_check is allowed")
            .query_row([], |r| r.get(0))
            .expect("quick_check");
        assert_eq!(qc, "ok");
        assert_eq!(
            count(
                &c,
                "SELECT count(*) FROM message WHERE typeof(guid) = 'text' AND octet_length(guid) > length('') AND instr(guid, 'G') > 0"
            ),
            i64::from(wal_rows)
        );
        assert!(c.query_only().expect("query_only"));
        assert_eq!(c.encoding().expect("encoding"), TextEncoding::Utf8);
        let ps = c.page_size().expect("page size");
        assert!(ps.is_power_of_two() && (512..=65_536).contains(&ps));
        assert!(c.page_count().expect("page count") >= 1);
        assert_eq!(c.authorizer_denials(), 0, "nothing allowed is counted");

        // Denied, each counted at least once. SQLite reports most denials as
        // SQLITE_AUTH (23) and a denied function as SQLITE_ERROR (1).
        let denied = [
            "PRAGMA query_only=0",
            "PRAGMA user_version=9",
            "PRAGMA user_version",
            "PRAGMA writable_schema=1",
            "ATTACH DATABASE ':memory:' AS other",
            "CREATE TEMP TABLE scratch (a)",
            "INSERT INTO message (guid) VALUES ('x')",
            "DELETE FROM message",
            "SELECT part_text FROM recoverable_message_part",
            "SELECT count(part_text) FROM recoverable_message_part",
            "SELECT max(ROWID) FROM message",
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n) SELECT i FROM n",
        ];
        let mut before = c.authorizer_denials();
        for sql in denied {
            let r = c.prepare_read(sql);
            assert!(
                matches!(r, Err(BackupError::Sqlite { code: Some(1 | 23) })),
                "{sql}: {r:?}"
            );
            let after = c.authorizer_denials();
            assert!(after > before, "{sql} was not counted");
            before = after;
        }
        // Double-quoted strings are off: an unknown identifier fails to
        // prepare instead of reading as a string.
        assert!(matches!(
            c.prepare_read("SELECT \"nosuch\" FROM message"),
            Err(BackupError::Sqlite { code: Some(1) })
        ));
        assert!(c.query_only().expect("query_only"));
        assert_eq!(c.total_changes(), 0);
        drop(c);
        staged.remove().expect("remove");
        assert!(ob.close().removed);
    }
    assert_eq!(modes[0], OpenMode::ReadOnly);
}

/// Triggers and views are off: a schema with both loads, the view cannot be
/// used, and a schema with double-quoted literals in `DEFAULT` and `CHECK`
/// loads with `DQS_DDL = 0` (M3 Appendix B item 15).
#[test]
fn hostile_schema_objects_load_but_do_not_run() {
    let sms = make_sms(
        &format!(
            "{SCHEMA}
             CREATE TABLE quoted (a TEXT DEFAULT \"dq-default\", b TEXT CHECK (b <> \"dq-check\"));
             CREATE TABLE log (n INTEGER);
             CREATE TRIGGER t AFTER INSERT ON message BEGIN INSERT INTO log VALUES (1); END;
             CREATE VIEW v AS SELECT guid FROM message;"
        ),
        0,
    );
    let fx = fixture_with(&sms, false);
    let ob = fx.open(None).expect("open");
    let staged = stage(&ob, WalPolicy::BestEffort).expect("stage");
    let c = staged
        .open_with(&CancelToken::new(), &StagedDbLimits::ceiling(), &POLICY)
        .expect("a schema with DQS literals, a trigger and a view loads");
    assert_eq!(count(&c, "SELECT count(*) FROM quoted"), 0);
    assert!(
        c.prepare_read("SELECT guid FROM v").is_err(),
        "views are off"
    );
    assert_eq!(count(&c, "SELECT count(*) FROM log"), 0);
    drop(c);
    assert!(ob.close().removed);
}

/// A schema that loads under the ceiling but not under tighter limits is
/// `SchemaExceedsLimits`; a file that loads under neither keeps its error.
#[test]
fn schema_load_retry() {
    let wide: String = (0..40).map(|i| format!(", c{i} INTEGER")).collect();
    let sms = make_sms(
        &format!("{SCHEMA} CREATE TABLE wide (id INTEGER PRIMARY KEY{wide});"),
        0,
    );
    let fx = fixture_with(&sms, false);
    let ob = fx.open(None).expect("open");
    let staged = stage(&ob, WalPolicy::BestEffort).expect("stage");
    let token = CancelToken::new();
    for tight in [
        StagedDbLimits::ceiling().with_column(10),
        StagedDbLimits::ceiling().with_sql_length(100),
    ] {
        assert!(matches!(
            staged.open_with(&token, &tight, &POLICY),
            Err(BackupError::SchemaExceedsLimits)
        ));
    }
    // Loose enough: opens.
    let ok = StagedDbLimits::ceiling()
        .with_column(100)
        .with_sql_length(4096);
    drop(staged.open_with(&token, &ok, &POLICY).expect("open"));
    staged.remove().expect("remove");
    assert!(ob.close().removed);

    // Not a database at all: the original error stands.
    let junk = SmsDb {
        main: vec![0x42; 8192],
        wal: None,
    };
    let fx = fixture_with(&junk, false);
    let ob = fx.open(None).expect("open");
    let staged = stage(&ob, WalPolicy::BestEffort).expect("stage");
    assert!(matches!(
        staged.open_with(&token, &StagedDbLimits::ceiling().with_column(10), &POLICY),
        Err(BackupError::Malformed {
            detail: oma_backup::MalformedDetail::NotSqlite,
            ..
        })
    ));
    assert!(ob.close().removed);
}

#[test]
fn op_budgets() {
    let sms = make_sms(
        &format!(
            "{SCHEMA}
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 20000)
             INSERT INTO message (guid, text) SELECT 'SYNTH-GUID-' || i, NULL FROM n;"
        ),
        0,
    );
    let fx = fixture_with(&sms, false);
    let ob = fx.open(None).expect("open");
    let staged = stage(&ob, WalPolicy::BestEffort).expect("stage");
    let token = CancelToken::new();
    let c = staged
        .open_with(&token, &StagedDbLimits::ceiling(), &POLICY)
        .expect("open");
    let heavy = "SELECT count(*) FROM message AS a, message AS b WHERE a.ROWID <> b.ROWID";
    let run = |c: &oma_backup::StagedConnection<'_>, sql: &str| -> Result<i64, BackupError> {
        let mut s = c.prepare_read(sql)?;
        s.query_row([], |r| r.get(0))
            .map_err(|e| c.sqlite_error(&e))
    };

    assert_eq!(
        c.with_op_budget(10_000_000, |c| run(c, "SELECT count(*) FROM message"))
            .expect("budget"),
        Budgeted::Done(20_000)
    );
    assert_eq!(
        c.with_op_budget(50_000, |c| run(c, heavy)).expect("budget"),
        Budgeted::OverBudget
    );
    // The budget does not outlive its call.
    assert_eq!(count(&c, "SELECT count(*) FROM message"), 20_000);
    // A swallowed interrupt is still over budget.
    assert_eq!(
        c.with_op_budget(50_000, |c| Ok(run(c, heavy).is_err()))
            .expect("budget"),
        Budgeted::OverBudget
    );
    // Budgets do not nest.
    assert!(matches!(
        c.with_op_budget(1_000_000, |c| c.with_op_budget(1_000, |_| Ok(()))),
        Err(BackupError::Internal(_))
    ));
    // Cancellation wins over the budget, and is seen inside a long statement
    // by the progress handler.
    let started = std::time::Instant::now();
    let canceller = {
        let token = token.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            token.cancel();
        })
    };
    let r = c.with_op_budget(u64::MAX, |c| run(c, heavy));
    canceller.join().expect("join");
    assert!(matches!(r, Err(BackupError::Cancelled)), "{r:?}");
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    assert_eq!(c.authorizer_denials(), 0);
    drop(c);
    assert!(ob.close().removed);
}

/// Rows that live only in the `-wal` are read from the staged copy; the
/// `-wal` is staged, its frames validate, and the header says WAL.
#[test]
fn wal_rows_only_in_the_wal_are_staged() {
    let sms = make_sms(SCHEMA, 5);
    let fx = fixture_with(&sms, true);
    let ob = fx.open(None).expect("open");
    let staged = stage(&ob, WalPolicy::BestEffort).expect("stage");
    let wal_len = sms.wal.as_ref().map_or(0, |w| w.len() as u64);
    assert_eq!(staged.wal_outcome, WalOutcome::Staged { bytes: wal_len });
    assert_eq!(staged.main_bytes, sms.main.len() as u64);
    assert!(staged.wal.as_ref().is_some_and(|w| w.is_file()));
    assert!(staged.header_facts().expect("header").says_wal());
    let facts = staged
        .wal_facts(&CancelToken::new())
        .expect("wal facts")
        .expect("staged");
    assert!(facts.magic_valid && facts.header_valid && facts.page_size_matches);
    assert!(facts.valid_committed_frames >= 1);
    assert_eq!(facts.frames_after_first_invalid, 0);
    assert_eq!(facts.uncommitted_valid_frames, 0);
    let c = staged
        .open_with(&CancelToken::new(), &StagedDbLimits::ceiling(), &POLICY)
        .expect("open");
    assert_eq!(count(&c, "SELECT count(*) FROM message"), 5);
    drop(c);
    // The same database through the Required policy stages identically.
    let again = stage(&ob, WalPolicy::Required).expect("stage");
    assert_eq!(again.content_digest(), staged.content_digest());
    staged.remove().expect("remove");
    again.remove().expect("remove");
    let mut left = Vec::new();
    walk(&ob.staging_path().expect("staging").join("db"), &mut left);
    assert!(left.is_empty(), "{left:?}");
    assert!(ob.close().removed);
}

#[test]
fn a_listed_wal_that_is_missing_is_skipped_only_when_best_effort() {
    let mut sms = make_sms(SCHEMA, 2);
    sms.wal = None; // listed, blob absent
    let fx = fixture_with(&sms, true);
    let ob = fx.open(None).expect("open");
    let staged = stage(&ob, WalPolicy::BestEffort).expect("stage");
    assert_eq!(
        staged.wal_outcome,
        WalOutcome::Skipped(FileUnavailable::BlobMissing)
    );
    assert!(staged.wal.is_none());
    assert!(staged
        .wal_facts(&CancelToken::new())
        .expect("facts")
        .is_none());
    assert!(staged.header_facts().expect("header").says_wal());
    let dir = staged.path.parent().expect("dir").to_path_buf();
    let names: Vec<_> = fs::read_dir(&dir)
        .expect("read_dir")
        .map(|e| e.expect("entry").file_name())
        .collect();
    assert_eq!(names, ["sms.db"], "nothing else is left on disk");
    drop(staged);
    assert!(matches!(
        stage(&ob, WalPolicy::Required),
        Err(BackupError::FileUnavailable(FileUnavailable::BlobMissing))
    ));
    assert!(ob.close().removed);

    // Not listed at all.
    let fx = fixture_with(&make_sms(SCHEMA, 0), false);
    let ob = fx.open(None).expect("open");
    let staged = stage(&ob, WalPolicy::BestEffort).expect("stage");
    assert_eq!(staged.wal_outcome, WalOutcome::NotListed);
    assert!(!staged.header_facts().expect("header").says_wal());
    assert!(ob.close().removed);
}

#[test]
fn corrupted_later_wal_frames_are_counted() {
    let mut sms = make_sms(SCHEMA, 40);
    let wal = sms.wal.as_mut().expect("wal");
    let last = wal.len() - 100;
    wal[last] ^= 0xff; // inside the last frame's page
    let fx = fixture_with(&sms, true);
    let ob = fx.open(None).expect("open");
    let staged = stage(&ob, WalPolicy::BestEffort).expect("stage");
    let facts = staged
        .wal_facts(&CancelToken::new())
        .expect("facts")
        .expect("staged");
    assert!(facts.header_valid);
    assert!(facts.frames_after_first_invalid >= 1);
    assert!(facts.valid_committed_frames >= 1);
    assert!(ob.close().removed);
}

#[test]
fn content_digest_is_stable_and_covers_main_then_wal() {
    let sms = make_sms(SCHEMA, 3);
    let fx = fixture_with(&sms, true);
    let ob = fx.open(None).expect("open");
    let a = stage(&ob, WalPolicy::BestEffort).expect("stage");
    let b = stage(&ob, WalPolicy::BestEffort).expect("stage");
    assert_eq!(a.content_digest(), b.content_digest());
    let mut h = Sha256::new();
    h.update(&sms.main);
    h.update(sms.wal.as_ref().expect("wal"));
    let expected: [u8; 32] = h.finalize().into();
    assert_eq!(a.content_digest().as_bytes(), &expected);
    assert_eq!(format!("{:?}", a.content_digest()), "<digest>");
    drop((a, b));
    assert!(ob.close().removed);

    // Different content, different digest; the encrypted path digests the
    // plaintext, which equals the plain fixture's.
    let other = fixture_with(&make_sms(SCHEMA, 0), false);
    let ob = other.open(None).expect("open");
    let c = stage(&ob, WalPolicy::BestEffort).expect("stage");
    assert_ne!(c.content_digest().as_bytes(), &expected);
    drop(c);
    assert!(ob.close().removed);
    let plain_digest = {
        let fx = Fixture::read_only("plain");
        let ob = fx.open(None).expect("open");
        let d = stage(&ob, WalPolicy::BestEffort)
            .expect("stage")
            .content_digest()
            .clone();
        assert!(ob.close().removed);
        d
    };
    let fx = Fixture::read_only("enc");
    let ob = fx.open(Some(PASSWORD)).expect("open");
    let d = stage(&ob, WalPolicy::BestEffort).expect("stage");
    assert_eq!(d.content_digest(), &plain_digest);
    let sum: [u8; 32] = Sha256::digest(fs::read(&d.path).expect("staged")).into();
    assert_eq!(d.content_digest().as_bytes(), &sum);
    drop(d);
    assert!(ob.close().removed);
}

#[test]
fn combined_progress_is_one_running_total() {
    // Big enough for several copy chunks.
    let sms = make_sms(
        &format!(
            "{SCHEMA}
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 3000)
             INSERT INTO message (guid, text) SELECT 'SYNTH-GUID-' || i, printf('%.500c', 'x') FROM n;"
        ),
        200,
    );
    let total = (sms.main.len() + sms.wal.as_ref().expect("wal").len()) as u64;
    let fx = fixture_with(&sms, true);
    let ob = fx.open(None).expect("open");
    let seen: Arc<Mutex<Vec<Progress>>> = Arc::default();
    let token = CancelToken::new();
    let sink_seen = Arc::clone(&seen);
    let mut sink = move |p: Progress| sink_seen.lock().expect("lock").push(p);
    let mut ctx = JobCtx::new(&token, &mut sink);
    let staged = ob
        .extract_database_with(SMS_DB_DOMAIN, SMS_DB_PATH, WalPolicy::BestEffort, &mut ctx)
        .expect("stage");
    let seen = seen.lock().expect("lock").clone();
    assert!(seen.len() >= 3, "{}", seen.len());
    assert!(seen.iter().all(|p| p.total == Some(total)));
    assert!(seen.windows(2).all(|w| w[0].done <= w[1].done));
    assert_eq!(seen.last().map(|p| p.done), Some(total));
    drop(staged);
    assert!(ob.close().removed);

    // The encrypted path reports the same way (one file, ciphertext bytes).
    let fx = Fixture::read_only("enc");
    let ob = fx.open(Some(PASSWORD)).expect("open");
    let mut seen = Vec::new();
    let mut sink = |p: Progress| seen.push(p);
    let mut ctx = JobCtx::new(&token, &mut sink);
    drop(
        ob.extract_database_with(SMS_DB_DOMAIN, SMS_DB_PATH, WalPolicy::BestEffort, &mut ctx)
            .expect("stage"),
    );
    let total = seen.first().and_then(|p| p.total).expect("total");
    assert!(seen.iter().all(|p| p.total == Some(total)));
    assert_eq!(seen.last().map(|p| p.done), Some(total));
    assert!(ob.close().removed);
}

#[test]
fn blob_stamps_detect_a_replaced_sms_db_or_wal() {
    for target in [SMS_DB_FILE_ID, SMS_WAL_FILE_ID] {
        let sms = make_sms(SCHEMA, 2);
        let fx = fixture_with(&sms, true);
        let ob = fx.open(None).expect("open");
        let staged = stage(&ob, WalPolicy::BestEffort).expect("stage");
        assert!(ob.unchanged_since_open().expect("stamps"));
        let path = blob(&fx.dir, target);
        let mut bytes = fs::read(&path).expect("read");
        bytes.extend_from_slice(&[0; 512]);
        fs::write(&path, &bytes).expect("replace");
        assert!(!ob.unchanged_since_open().expect("stamps"), "{target}");
        drop(staged);
        assert!(ob.close().removed);
    }
}

#[test]
fn a_change_during_the_copy_is_source_changed() {
    let sms = make_sms(SCHEMA, 2);
    let fx = fixture_with(&sms, true);
    let ob = fx.open(None).expect("open");
    let source = blob(&fx.dir, SMS_DB_FILE_ID);
    let token = CancelToken::new();
    let mut touched = false;
    let mut sink = |p: Progress| {
        if !touched && p.done > 0 {
            touched = true;
            let mut bytes = fs::read(&source).expect("read");
            bytes.extend_from_slice(&[0; 512]);
            fs::write(&source, &bytes).expect("change the source");
        }
    };
    let mut ctx = JobCtx::new(&token, &mut sink);
    let r = ob.extract_database_with(SMS_DB_DOMAIN, SMS_DB_PATH, WalPolicy::BestEffort, &mut ctx);
    assert!(matches!(r, Err(BackupError::SourceChanged)), "{r:?}");
    let mut left = Vec::new();
    walk(&ob.staging_path().expect("staging").join("db"), &mut left);
    assert!(left.is_empty(), "no copy is handed out or left: {left:?}");
    assert!(ob.close().removed);
}

#[test]
fn the_size_cap_is_checked_before_copying() {
    let sms = make_sms(SCHEMA, 2);
    let fx = fixture_with(&sms, true);
    let total = (sms.main.len() + sms.wal.as_ref().expect("wal").len()) as u64;
    let limits = Limits {
        sms_db_bytes: total - 1,
        ..Limits::default()
    };
    let mut sink = |_: Progress| {};
    let token = CancelToken::new();
    let ob = fx
        .open_with(None, &limits, &mut sink, &token)
        .expect("open");
    let r = stage(&ob, WalPolicy::BestEffort);
    assert!(
        matches!(
            r,
            Err(BackupError::LimitExceeded { what: LimitKind::SmsDbBytes, value, cap })
                if value == total && cap == total - 1
        ),
        "{r:?}"
    );
    assert!(
        !ob.staging_path().expect("staging").join("db").exists() || {
            let mut left = Vec::new();
            walk(&ob.staging_path().expect("staging").join("db"), &mut left);
            left.is_empty()
        }
    );
    assert!(ob.close().removed);
    assert_eq!(Limits::default().sms_db_bytes, 32 << 30);
}

#[test]
fn remove_deletes_the_database_wal_and_shm() {
    let sms = make_sms(SCHEMA, 2);
    let fx = fixture_with(&sms, true);
    let ob = fx.open(None).expect("open");
    let staged = stage(&ob, WalPolicy::BestEffort).expect("stage");
    let dir = staged.path.parent().expect("dir").to_path_buf();
    let c = staged.open(&CancelToken::new()).expect("open");
    assert_eq!(count(&c, "SELECT count(*) FROM message"), 2);
    drop(c);
    staged.remove().expect("remove");
    assert_eq!(fs::read_dir(&dir).expect("read_dir").count(), 0);
    assert!(ob.close().removed);
}

#[test]
fn attachment_keys_and_the_file_prober() {
    use oma_backup::BlobProbe;
    for (name, password, len) in [("plain", None, 87u64), ("enc", Some(PASSWORD), 96)] {
        let fx = Fixture::read_only(name);
        let ob = fx.open(password).expect("open");
        let key = ob
            .find_attachment(ATTACHMENT_DOMAIN, ATTACHMENT_PATH)
            .expect("lookup")
            .expect("listed");
        let shown = format!("{key:?}");
        assert!(
            !shown.contains("SYNTH") && !shown.contains("note.txt"),
            "{shown}"
        );
        let mut prober = ob.file_prober();
        assert_eq!(prober.probe(&key), BlobProbe::Present { len }, "{name}");
        // The key is accepted by extract_file.
        let token = CancelToken::new();
        let mut sink = |_: Progress| {};
        let mut ctx = JobCtx::new(&token, &mut sink);
        let x = ob.extract_file(&key, &mut ctx).expect("extract");
        assert_eq!(x.len, 87);
        x.remove().expect("remove");
        // A directory row.
        let dir = ob
            .find_attachment("HomeDomain", "Library/SMS")
            .expect("lookup")
            .expect("listed");
        assert_eq!(prober.probe(&dir), BlobProbe::NotAFile);
        assert!(ob
            .find_attachment(ATTACHMENT_DOMAIN, "Library/SMS/Attachments/none")
            .expect("lookup")
            .is_none());
        drop(prober);
        assert!(ob.close().removed);
        assert!(fx.staged_files().is_empty());
    }

    // A blob truncated in place is not "present": its length no longer
    // matches the Manifest's Size (padded to a block when encrypted).
    let attachment_id: String = {
        use sha1::Digest as _;
        sha1::Sha1::digest(format!("{ATTACHMENT_DOMAIN}-{ATTACHMENT_PATH}").as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    };
    for (name, password, keep) in [
        ("plain", None, 0u64),
        ("plain", None, 50),
        ("enc", Some(PASSWORD), 0),
        ("enc", Some(PASSWORD), 80),
    ] {
        let fx = Fixture::new(name);
        let path = blob(&fx.dir, &attachment_id);
        let file = fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("blob");
        file.set_len(keep).expect("truncate");
        drop(file);
        let ob = fx.open(password).expect("open");
        let key = ob
            .find_attachment(ATTACHMENT_DOMAIN, ATTACHMENT_PATH)
            .expect("lookup")
            .expect("listed");
        assert_eq!(
            ob.file_prober().probe(&key),
            BlobProbe::SizeMismatch { len: keep },
            "{name} {keep}"
        );
        assert!(ob.close().removed);
    }

    let fx = Fixture::read_only("missing-sms-blob");
    let ob = fx.open(None).expect("open");
    let key = ob
        .find_attachment(SMS_DB_DOMAIN, SMS_DB_PATH)
        .expect("lookup")
        .expect("listed");
    assert_eq!(ob.file_prober().probe(&key), BlobProbe::Missing);
    assert!(ob.close().removed);

    // A blob replaced by a directory is not regular.
    let fx = Fixture::new("plain");
    let path = blob(&fx.dir, SMS_DB_FILE_ID);
    fs::remove_file(&path).expect("rm");
    fs::create_dir(&path).expect("mkdir");
    let ob = fx.open(None).expect("open");
    let key = ob
        .find_attachment(SMS_DB_DOMAIN, SMS_DB_PATH)
        .expect("lookup")
        .expect("listed");
    assert_eq!(ob.file_prober().probe(&key), BlobProbe::NotRegular);
    assert!(ob.close().removed);
}
