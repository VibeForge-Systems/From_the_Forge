//! Schema identification, the fingerprint and migrations (spec §7, §18.2).
#![allow(clippy::disallowed_methods)]

mod common;

use std::fs;

use common::*;
use oma_archive::hooks::{install, Faults, TestSchema};
use oma_archive::{
    ArchiveCtx, ArchiveEntry, ArchiveError, ArchiveEvent, ArchiveSink, CancelToken, DamageKind,
    FixedClock, ImportBlock, Library, LocationRefusal, NoProgress, Recovery,
};

fn v2() -> Faults {
    Faults {
        schema: Some(TestSchema::V2),
        ..Faults::default()
    }
}

fn ctx_parts() -> (CancelToken, NoProgress, FixedClock) {
    (
        CancelToken::new(),
        NoProgress,
        FixedClock(1_700_000_000_000_000_000),
    )
}

#[derive(Default)]
struct Events(Vec<ArchiveEvent>);
impl ArchiveSink for Events {
    fn event(&mut self, e: ArchiveEvent) {
        self.0.push(e);
    }
}

/// Tamper with an archive's schema through an unhardened connection and
/// check that open, open_read and list all refuse it without changing the
/// file.
fn assert_mismatch_untouched(sql: &str) {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    {
        let c = raw(&db_path(&tmp, &id));
        c.execute_batch(sql).expect("tamper");
        c.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
            .expect("checkpoint");
    }
    let before = data_snapshot(&tmp, &id);
    assert!(
        matches!(
            lib.open(&id),
            Err(ArchiveError::Damaged(DamageKind::SchemaMismatch))
        ),
        "{sql}"
    );
    assert!(matches!(
        lib.open_read(&id),
        Err(ArchiveError::Damaged(DamageKind::SchemaMismatch))
    ));
    assert!(matches!(
        &lib.list().expect("list")[..],
        [ArchiveEntry::Damaged {
            kind: DamageKind::SchemaMismatch,
            ..
        }]
    ));
    assert_eq!(data_snapshot(&tmp, &id), before, "{sql}");
}

#[test]
fn extra_triggers_views_indexes_and_tables_are_schema_mismatches() {
    for sql in [
        "CREATE TRIGGER planted AFTER INSERT ON lineage BEGIN DELETE FROM lineage; END;",
        "CREATE VIEW planted AS SELECT * FROM message;",
        "CREATE INDEX planted ON message(service);",
        "CREATE TABLE planted (x);",
        "DROP INDEX message_sort;",
        "DROP TRIGGER message_body_au; CREATE TRIGGER message_body_au AFTER UPDATE OF display_text \
         ON message_body BEGIN SELECT 1; END;",
    ] {
        assert_mismatch_untouched(sql);
    }
}

#[test]
fn shadow_table_ddl_differences_are_tolerated() {
    let (_tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let _faults = install(Faults {
        skew_shadow_ddl: true,
        ..Faults::default()
    });
    lib.open(&id).expect("open with skewed expected shadow DDL");
    lib.open_read(&id).expect("open_read");
}

#[test]
fn sqlite_stat1_is_tolerated_and_reported() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    {
        let c = raw(&db_path(&tmp, &id));
        c.execute_batch("ANALYZE;").expect("analyze");
    }
    let info = lib.open_read(&id).expect("open_read").info().expect("info");
    assert!(info.stat_tables);
    let a = lib.open(&id).expect("open");
    assert!(a.reader().info().expect("info").stat_tables);
}

#[test]
fn a_newer_user_version_is_refused_without_writing() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    {
        let c = raw(&db_path(&tmp, &id));
        c.pragma_update(None, "user_version", 2).expect("uv");
        c.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
            .expect("checkpoint");
    }
    let before = data_snapshot(&tmp, &id);
    assert!(matches!(
        lib.open(&id),
        Err(ArchiveError::NewerSchema { found: 2 })
    ));
    assert!(matches!(
        lib.open_read(&id),
        Err(ArchiveError::NewerSchema { found: 2 })
    ));
    assert!(matches!(
        &lib.list().expect("list")[..],
        [ArchiveEntry::NewerSchema { version: 2, .. }]
    ));
    assert_eq!(data_snapshot(&tmp, &id), before);
}

#[test]
fn foreign_empty_and_garbage_files_are_not_archives() {
    let (tmp, lib) = library();
    for case in ["foreign", "empty", "garbage", "zero-version"] {
        let id = lib.create(None).expect("create");
        let db = db_path(&tmp, &id);
        match case {
            "foreign" => {
                let c = raw(&db);
                c.pragma_update(None, "application_id", 0x1234_5678)
                    .expect("app id");
            }
            "empty" | "garbage" => {
                fs::remove_file(&db).expect("remove");
                let bytes: &[u8] = if case == "empty" {
                    b""
                } else {
                    b"this is not a SQLite database, just some bytes long enough to be read....."
                };
                fs::write(&db, bytes).expect("write");
                #[cfg(unix)]
                chmod(&db, 0o600);
            }
            _ => {
                let c = raw(&db);
                c.pragma_update(None, "user_version", 0).expect("uv");
            }
        }
        let got = lib.open(&id);
        if case == "zero-version" {
            assert!(
                matches!(got, Err(ArchiveError::Damaged(DamageKind::SchemaMismatch))),
                "{case}: {got:?}"
            );
        } else {
            assert!(
                matches!(got, Err(ArchiveError::NotAnArchive)),
                "{case}: {got:?}"
            );
        }
    }
    // A directory without archive.sqlite is not an archive either.
    let id = lib.create(None).expect("create");
    fs::remove_file(db_path(&tmp, &id)).expect("remove");
    assert!(matches!(lib.open(&id), Err(ArchiveError::NotAnArchive)));
    let list = lib.list().expect("list");
    assert_eq!(list.len(), 5);
    assert!(list.iter().all(|e| matches!(
        e,
        ArchiveEntry::NotAnArchive { .. } | ArchiveEntry::Damaged { .. }
    )));
}

#[test]
fn meta_problems_are_damage() {
    let (tmp, lib) = library();
    let cases = [
        (
            "UPDATE archive_meta SET schema_version = 3",
            DamageKind::MetaInconsistent,
        ),
        ("DELETE FROM archive_meta", DamageKind::MetaMissing),
    ];
    for (sql, kind) in cases {
        let id = lib.create(None).expect("create");
        raw(&db_path(&tmp, &id)).execute_batch(sql).expect("tamper");
        let got = lib.open(&id);
        assert!(
            matches!(got, Err(ArchiveError::Damaged(k)) if k == kind),
            "{sql}: {got:?}"
        );
    }
    // A label that is not valid any more is shown as unreadable, not used.
    let id = lib.create(None).expect("create");
    raw(&db_path(&tmp, &id))
        .execute(
            "UPDATE archive_meta SET label = ?1",
            ["bidi \u{202E} label"],
        )
        .expect("label");
    let info = lib.open_read(&id).expect("reader").info().expect("info");
    assert_eq!(info.label, oma_archive::LabelState::Unreadable);
}

#[test]
fn newer_rules_open_for_reading_but_block_imports() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    raw(&db_path(&tmp, &id))
        .execute(
            "UPDATE archive_meta SET max_deriver_version = ?1",
            [i64::from(oma_archive::DERIVER_VERSION) + 1],
        )
        .expect("rules");
    let a = lib.open(&id).expect("open");
    assert_eq!(a.import_blocked(), Some(ImportBlock::NewerRules));
    assert_eq!(
        a.reader().info().expect("info").import_blocked,
        Some(ImportBlock::NewerRules)
    );
    drop(a);
    match &lib.list().expect("list")[..] {
        [ArchiveEntry::Ready(info)] => {
            assert_eq!(info.import_blocked, Some(ImportBlock::NewerRules))
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_older_schema_needs_migration_everywhere_but_open() {
    let (_tmp, lib) = library();
    let id = lib.create(None).expect("create v1");
    let _faults = install(v2());
    assert!(matches!(
        lib.open_read(&id),
        Err(ArchiveError::NeedsMigration { found: 1 })
    ));
    assert!(matches!(
        &lib.list().expect("list")[..],
        [ArchiveEntry::NeedsMigration { version: 1, .. }]
    ));
    let mut a = lib.open(&id).expect("open");
    assert!(a.needs_migration());
    assert!(matches!(
        a.begin_write(Recovery::Run),
        Err(ArchiveError::NeedsMigration { found: 1 })
    ));
    assert!(matches!(
        a.reader().info(),
        Err(ArchiveError::NeedsMigration { found: 1 })
    ));
}

#[test]
fn the_test_v2_migration_succeeds_and_is_recorded() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create v1");
    {
        // Unfinished sessions are superseded before the first step.
        let c = raw(&db_path(&tmp, &id));
        insert_session(&c, 0);
        insert_session(&c, 6);
        insert_session(&c, 1);
    }
    let _faults = install(v2());
    let mut a = lib.open(&id).expect("open");
    let (cancel, _, clock) = ctx_parts();
    let mut events = Events::default();
    let report = a
        .migrate(&mut ArchiveCtx {
            cancel: &cancel,
            progress: &mut events,
            clock: &clock,
        })
        .expect("migrate");
    assert_eq!((report.from, report.to, report.steps), (1, 2, 1));
    assert_eq!(report.sessions_superseded, 2);
    assert!(report.space_checked);
    assert_eq!(
        events.0,
        vec![ArchiveEvent::MigrationStep { from: 1, to: 2 }]
    );
    assert!(!a.needs_migration());
    assert_eq!(a.schema_version(), 2);
    let info = a.reader().info().expect("info after migration");
    assert_eq!(info.schema_version, 2);
    assert_eq!(info.sessions.superseded, 2);
    a.begin_write(Recovery::Run).expect("writable at v2");
    a.close();

    // No pre-migration copy exists for the test-only step.
    let names: Vec<String> = tree(&archive_dir(&tmp, &id)).into_keys().collect();
    assert!(
        !names.iter().any(|n| n.starts_with("archive.pre-v")),
        "{names:?}"
    );
    let c = raw(&db_path(&tmp, &id));
    let (uv, sv): (i64, i64) = (
        c.query_row("PRAGMA user_version", [], |r| r.get(0))
            .expect("uv"),
        c.query_row("SELECT schema_version FROM archive_meta", [], |r| r.get(0))
            .expect("sv"),
    );
    assert_eq!((uv, sv), (2, 2));
    let row: (i64, i64, i64, String) = c
        .query_row(
            "SELECT version, from_version, applied_at_ns, app_version FROM schema_migration",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .expect("schema_migration");
    assert_eq!((row.0, row.1, row.2), (2, 1, 1_700_000_000_000_000_000));
    assert!(!row.3.is_empty());
    drop(c);
    // Reopened at v2 under the v2 plan; the build plan (v1) calls it newer.
    lib.open_read(&id).expect("v2 reader");
    drop(_faults);
    assert!(matches!(
        lib.open(&id),
        Err(ArchiveError::NewerSchema { found: 2 })
    ));
}

fn migrate_with(
    schema: TestSchema,
) -> (
    tempfile::TempDir,
    Library,
    oma_archive::ArchiveId,
    Result<(), ArchiveError>,
) {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create v1");
    let _faults = install(Faults {
        schema: Some(schema),
        ..Faults::default()
    });
    let mut a = lib.open(&id).expect("open");
    let (cancel, mut progress, clock) = ctx_parts();
    let result = a
        .migrate(&mut ArchiveCtx {
            cancel: &cancel,
            progress: &mut progress,
            clock: &clock,
        })
        .map(|_| ());
    if result.is_err() {
        assert!(a.needs_migration(), "{schema:?}: still at v1");
    }
    a.close();
    (tmp, lib, id, result)
}

/// After a failed step: still v1, no trace of v2, and the next connection
/// has `foreign_keys` on.
fn assert_rolled_back(tmp: &tempfile::TempDir, lib: &Library, id: &oma_archive::ArchiveId) {
    let c = raw(&db_path(tmp, id));
    let uv: i64 = c
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .expect("uv");
    assert_eq!(uv, 1);
    let v2_objects: i64 = c
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name LIKE 'test_v2%'",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(v2_objects, 0);
    let migrations: i64 = c
        .query_row("SELECT count(*) FROM schema_migration", [], |r| r.get(0))
        .expect("count");
    assert_eq!(migrations, 0);
    drop(c);
    // The build plan opens it as a healthy v1 archive, and its writer
    // enforces foreign keys.
    let mut a = lib.open(id).expect("v1 again");
    a.begin_write(Recovery::Skip).expect("writer");
    a.close();
    let c = raw(&db_path(tmp, id));
    c.pragma_update(None, "foreign_keys", true).expect("fk");
    assert!(c
        .execute(
            "INSERT INTO import_cursor (session_id, pass, rows_done, done) VALUES (9, 1, 0, 0)",
            [],
        )
        .is_err());
}

#[test]
fn an_injected_failure_mid_step_leaves_version_1() {
    let (tmp, lib, id, result) = migrate_with(TestSchema::V2FailsMidStep);
    assert!(
        matches!(result, Err(ArchiveError::Sqlite { code: Some(4), .. })),
        "{result:?}"
    );
    assert_rolled_back(&tmp, &lib, &id);
}

#[test]
fn foreign_key_violations_block_the_step() {
    let (tmp, lib, id, result) = migrate_with(TestSchema::V2ForeignKeyViolation);
    assert!(
        matches!(
            result,
            Err(ArchiveError::Damaged(DamageKind::MigrationFailed))
        ),
        "{result:?}"
    );
    assert_rolled_back(&tmp, &lib, &id);
}

#[test]
fn an_fts_integrity_failure_blocks_the_step() {
    let (tmp, lib, id, result) = migrate_with(TestSchema::V2FtsCorruption);
    assert!(
        matches!(
            result,
            Err(ArchiveError::Damaged(DamageKind::MigrationFailed))
        ),
        "{result:?}"
    );
    assert_rolled_back(&tmp, &lib, &id);
}

#[test]
fn a_step_whose_result_does_not_match_the_target_fingerprint_is_refused() {
    let (tmp, lib, id, result) = migrate_with(TestSchema::V2WrongDdl);
    assert!(
        matches!(
            result,
            Err(ArchiveError::Damaged(DamageKind::MigrationFailed))
        ),
        "{result:?}"
    );
    assert_rolled_back(&tmp, &lib, &id);
}

#[test]
fn a_real_migration_without_the_pre_migration_copy_is_refused() {
    let (tmp, lib, id, result) = migrate_with(TestSchema::V2NeedsCopy);
    assert!(
        matches!(result, Err(ArchiveError::Internal(_))),
        "{result:?}"
    );
    assert_rolled_back(&tmp, &lib, &id);
}

#[test]
fn a_fingerprint_mismatch_before_the_step_refuses_it() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create v1");
    let _faults = install(v2());
    let mut a = lib.open(&id).expect("open");
    // Another program changes the schema after the open.
    raw(&db_path(&tmp, &id))
        .execute_batch("CREATE INDEX planted ON message(service);")
        .expect("tamper");
    let (cancel, mut progress, clock) = ctx_parts();
    let result = a.migrate(&mut ArchiveCtx {
        cancel: &cancel,
        progress: &mut progress,
        clock: &clock,
    });
    assert!(
        matches!(
            result,
            Err(ArchiveError::Damaged(DamageKind::SchemaMismatch))
        ),
        "{result:?}"
    );
    a.close();
    let uv: i64 = raw(&db_path(&tmp, &id))
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .expect("uv");
    assert_eq!(uv, 1);
}

#[test]
fn the_space_check_uses_three_times_the_database_plus_the_reserve() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create v1");
    {
        let c = raw(&db_path(&tmp, &id));
        insert_session(&c, 2);
    }
    let dir = archive_dir(&tmp, &id);
    let db_bytes = |dir: &std::path::Path| -> u64 {
        ["archive.sqlite", "archive.sqlite-wal"]
            .iter()
            .filter_map(|n| fs::metadata(dir.join(n)).ok())
            .map(|m| m.len())
            .sum()
    };
    let reserve = oma_archive::space::RESERVE_MIN;
    let _faults = install(v2());

    let short = lib.with_space_probe(fake_space(Some(0), None));
    let mut a = short.open(&id).expect("open");
    let needed = 3 * db_bytes(&dir) + reserve;
    drop(a);
    let short = short.with_space_probe(fake_space(Some(needed - 1), None));
    a = short.open(&id).expect("open");
    let (cancel, mut progress, clock) = ctx_parts();
    let result = a.migrate(&mut ArchiveCtx {
        cancel: &cancel,
        progress: &mut progress,
        clock: &clock,
    });
    assert!(
        matches!(
            result,
            Err(ArchiveError::Location(
                LocationRefusal::InsufficientSpaceForMigration
            ))
        ),
        "{result:?}"
    );
    // Refused before anything changed: the cancelled session is untouched.
    let info_state: i64 = raw(&db_path(&tmp, &id))
        .query_row("SELECT state FROM import_session", [], |r| r.get(0))
        .expect("state");
    assert_eq!(info_state, 2);
    drop(a);

    let enough = short.with_space_probe(fake_space(Some(3 * db_bytes(&dir) + reserve), None));
    let mut a = enough.open(&id).expect("open");
    let report = a
        .migrate(&mut ArchiveCtx {
            cancel: &cancel,
            progress: &mut progress,
            clock: &clock,
        })
        .expect("exactly enough space");
    assert!(report.space_checked);
    drop(a);

    // Unknown space does not block (an error is never zero).
    let (tmp2, lib2) = library();
    let id2 = {
        let _no = install(Faults::default());
        lib2.create(None).expect("create")
    };
    let unknown = lib2.with_space_probe(fake_space(None, None));
    let mut a = unknown.open(&id2).expect("open");
    let report = a
        .migrate(&mut ArchiveCtx {
            cancel: &cancel,
            progress: &mut progress,
            clock: &clock,
        })
        .expect("unknown space");
    assert!(!report.space_checked);
    drop(tmp2);
}

#[test]
fn a_cancelled_migration_changes_nothing() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create v1");
    let _faults = install(v2());
    let mut a = lib.open(&id).expect("open");
    let cancel = CancelToken::new();
    cancel.cancel();
    let result = a.migrate(&mut ArchiveCtx {
        cancel: &cancel,
        progress: &mut NoProgress,
        clock: &FixedClock(1),
    });
    assert!(matches!(result, Err(ArchiveError::Cancelled)));
    a.close();
    let uv: i64 = raw(&db_path(&tmp, &id))
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .expect("uv");
    assert_eq!(uv, 1);
}

#[test]
fn migrate_on_a_current_archive_does_nothing() {
    let (_tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let mut a = lib.open(&id).expect("open");
    let report = a
        .migrate(&mut ArchiveCtx {
            cancel: &CancelToken::new(),
            progress: &mut NoProgress,
            clock: &FixedClock(1),
        })
        .expect("no-op");
    assert_eq!((report.from, report.to, report.steps), (1, 1, 0));
    assert_eq!(a.close().checkpoint, None, "no writer was opened");
}

#[test]
fn an_archive_created_under_a_v2_plan_is_v2_and_matches_its_fingerprint() {
    let (tmp, lib) = library();
    let _faults = install(v2());
    let id = lib.create(None).expect("create v2");
    let info = lib.open_read(&id).expect("reader").info().expect("info");
    assert_eq!(info.schema_version, 2);
    let uv: i64 = raw(&db_path(&tmp, &id))
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .expect("uv");
    assert_eq!(uv, 2);
}
