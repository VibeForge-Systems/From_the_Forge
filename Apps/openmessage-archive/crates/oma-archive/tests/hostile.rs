//! The threat model for archive files (spec §4.5, §18.7): directories and
//! files planted or altered by another program or user.
#![allow(clippy::disallowed_methods)]

mod common;

use std::fs;

use common::*;
use oma_archive::hooks::{install, Faults};
use oma_archive::{ArchiveEntry, ArchiveError, ArchiveId, DamageKind, PermProblem, RefusalKind};

#[test]
fn ids_with_separators_or_traversal_never_reach_the_file_system() {
    let zeros = |n: usize| "0".repeat(n);
    for bad in [
        format!("../a{}", zeros(32)),
        format!("a{}/..", zeros(32)),
        format!("a{}\\", zeros(31)),
        format!("a{}", zeros(33)),
        String::new(),
    ] {
        assert!(ArchiveId::parse(&bad).is_err(), "{bad}");
    }
}

#[cfg(unix)]
#[test]
fn a_symlinked_archive_directory_is_refused() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let elsewhere = tempfile::tempdir().expect("elsewhere");
    let moved = elsewhere.path().join("real");
    fs::rename(archive_dir(&tmp, &id), &moved).expect("move out");
    std::os::unix::fs::symlink(&moved, archive_dir(&tmp, &id)).expect("symlink");
    assert!(matches!(
        lib.open(&id),
        Err(ArchiveError::Permissions(PermProblem::Link))
    ));
    assert!(matches!(
        lib.open_read(&id),
        Err(ArchiveError::Permissions(PermProblem::Link))
    ));
    assert!(matches!(
        &lib.list().expect("list")[..],
        [ArchiveEntry::Refused {
            why: RefusalKind::Link,
            ..
        }]
    ));
}

#[cfg(unix)]
#[test]
fn symlinked_files_inside_an_archive_are_refused() {
    for name in ["archive.lock", "archive.sqlite", "imports"] {
        let (tmp, lib) = library();
        let id = lib.create(None).expect("create");
        let dir = archive_dir(&tmp, &id);
        let elsewhere = tempfile::tempdir().expect("elsewhere");
        let target = elsewhere.path().join(name);
        fs::rename(dir.join(name), &target).expect("move out");
        std::os::unix::fs::symlink(&target, dir.join(name)).expect("symlink");
        let got = lib.open(&id);
        assert!(
            matches!(got, Err(ArchiveError::Permissions(PermProblem::Link))),
            "{name}: {got:?}"
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn group_or_other_permissions_are_refused() {
    for (rel, m) in [
        ("", 0o750),
        ("archive.sqlite", 0o640),
        ("archive.lock", 0o604),
        ("attachments", 0o755),
        ("attachments/.incoming", 0o701),
        ("imports", 0o770),
    ] {
        let (tmp, lib) = library();
        let id = lib.create(None).expect("create");
        let p = archive_dir(&tmp, &id).join(rel);
        chmod(&p, m);
        let got = lib.open(&id);
        assert!(
            matches!(
                got,
                Err(ArchiveError::Permissions(PermProblem::Loose { .. }))
            ),
            "{rel}: {got:?}"
        );
        assert!(matches!(
            &lib.list().expect("list")[..],
            [ArchiveEntry::Refused {
                why: RefusalKind::Permissions(PermProblem::Loose { .. }),
                ..
            }]
        ));
    }
    // A loose -wal is refused too.
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let wal = archive_dir(&tmp, &id).join("archive.sqlite-wal");
    fs::write(&wal, b"").expect("wal");
    chmod(&wal, 0o644);
    assert!(matches!(
        lib.open(&id),
        Err(ArchiveError::Permissions(PermProblem::Loose { .. }))
    ));
}

#[test]
fn a_missing_store_directory_is_refused() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    fs::remove_dir(archive_dir(&tmp, &id).join("imports")).expect("rmdir");
    assert!(matches!(
        lib.open(&id),
        Err(ArchiveError::Permissions(PermProblem::Io(
            std::io::ErrorKind::NotFound
        )))
    ));
}

#[test]
fn a_crafted_bypass_of_the_storage_name_check_is_rejected_by_the_schema() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let c = raw(&db_path(&tmp, &id));
    let s = insert_session(&c, 1);
    for name in [
        "../".repeat(21) + "a",
        "AB".repeat(32),
        "00".repeat(32),
        "0".repeat(63),
    ] {
        let r = c.execute(
            "INSERT INTO blob (sha256, byte_len, storage_name, first_session_id) \
             VALUES (?1, 1, ?2, ?3)",
            rusqlite::params![[0xab_u8; 32].as_slice(), name, s],
        );
        assert!(r.is_err(), "{name}");
    }
    c.execute(
        "INSERT INTO blob (sha256, byte_len, storage_name, first_session_id) VALUES (?1, 1, ?2, ?3)",
        rusqlite::params![[0xab_u8; 32].as_slice(), "ab".repeat(32), s],
    )
    .expect("the right name is accepted");
}

#[test]
fn the_reader_op_budget_stops_a_crafted_archive() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    {
        let c = raw(&db_path(&tmp, &id));
        let tx = c.unchecked_transaction().expect("tx");
        for _ in 0..5000 {
            insert_session(&tx, 1);
        }
        tx.commit().expect("commit");
    }
    lib.open_read(&id)
        .expect("reader")
        .info()
        .expect("normal budget");
    let _faults = install(Faults {
        read_op_budget: Some(20_000),
        ..Faults::default()
    });
    let reader = lib.open_read(&id).expect("fingerprint fits the budget");
    assert!(matches!(
        reader.info(),
        Err(ArchiveError::Damaged(DamageKind::WorkBudget))
    ));
    assert!(matches!(
        &lib.list().expect("list")[..],
        [ArchiveEntry::Damaged {
            kind: DamageKind::WorkBudget,
            ..
        }]
    ));
}

#[test]
fn a_schema_padded_with_thousands_of_objects_is_rejected_after_a_few_rows() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    {
        let c = raw(&db_path(&tmp, &id));
        let tx = c.unchecked_transaction().expect("tx");
        for i in 0..3000 {
            tx.execute_batch(&format!("CREATE TABLE pad{i} (x);"))
                .expect("pad");
        }
        tx.commit().expect("commit");
    }
    assert!(matches!(
        lib.open_read(&id),
        Err(ArchiveError::Damaged(DamageKind::SchemaMismatch))
    ));
}

/// A planted `attachments/<hh>` link and a dangling intent: the reconcile
/// never follows the link, so a file with a store-like name outside the
/// archive survives, and the intent stays for the verifier (spec §4.5).
#[cfg(unix)]
#[test]
fn a_linked_store_directory_is_never_followed_by_the_reconcile() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let dir = archive_dir(&tmp, &id);
    let elsewhere = tempfile::tempdir().expect("elsewhere");
    let sha = [0xabu8; 32];
    let victim = elsewhere.path().join(hex(&sha));
    fs::write(&victim, b"not the archive's").expect("victim");
    std::os::unix::fs::symlink(elsewhere.path(), dir.join("attachments").join("ab"))
        .expect("symlink");
    // A loose (but real) store directory is refused as well.
    let loose_sha = [0xcdu8; 32];
    let loose = store_hh(&dir, "cd");
    chmod(&loose, 0o755);
    fs::write(loose.join(hex(&loose_sha)), b"placed").expect("placed");
    {
        let c = raw(&db_path(&tmp, &id));
        let s = insert_session(&c, 4);
        for sha in [sha, loose_sha] {
            c.execute(
                "INSERT INTO blob_intent (sha256, session_id) VALUES (?1, ?2)",
                rusqlite::params![sha.as_slice(), s],
            )
            .expect("intent");
        }
    }
    let mut a = lib.open(&id).expect("open");
    let r = a.begin_write(oma_archive::Recovery::Run).expect("recover");
    assert_eq!(
        (
            r.intents_committed,
            r.intents_rolled_back,
            r.intent_files_left
        ),
        (0, 0, 2)
    );
    drop(a);
    assert_eq!(fs::read(&victim).expect("victim"), b"not the archive's");
    assert!(loose.join(hex(&loose_sha)).exists());
    let left: i64 = raw(&db_path(&tmp, &id))
        .query_row("SELECT count(*) FROM blob_intent", [], |r| r.get(0))
        .expect("count");
    assert_eq!(left, 2);
}
