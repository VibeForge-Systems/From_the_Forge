//! Create, list, open, lock, write-open recovery, close and sweep (spec
//! §5, §18.2).
#![allow(clippy::disallowed_methods)]

mod common;

use std::fs;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::Duration;

use common::*;
use oma_archive::hooks::{install, Faults};
use oma_archive::{
    ArchiveEntry, ArchiveError, CheckpointOutcome, Label, LabelState, Library, LocationRefusal,
    Recovery,
};

#[test]
fn create_builds_the_layout_with_private_modes() {
    let (tmp, lib) = library();
    let label = Label::parse("Family phone").expect("label");
    let id = lib.create(Some(label.clone())).expect("create");
    let dir = archive_dir(&tmp, &id);

    let names: Vec<String> = tree(&dir).into_keys().collect();
    assert_eq!(
        names,
        [
            "archive.lock",
            "archive.sqlite",
            "attachments",
            "attachments|.incoming",
            "imports"
        ],
        "a closed archive holds exactly the layout of §5.1"
    );
    // No creation directory is left behind.
    let leftovers: Vec<_> = fs::read_dir(root(&tmp))
        .expect("read_dir")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(leftovers, vec![id.as_str().to_owned()]);

    #[cfg(unix)]
    {
        for d in ["", "attachments", "attachments/.incoming", "imports"] {
            assert_eq!(mode(&dir.join(d)), 0o700, "{d}");
        }
        for f in ["archive.lock", "archive.sqlite"] {
            assert_eq!(mode(&dir.join(f)), 0o600, "{f}");
        }
        // -wal and -shm take the database's mode while a writer is open.
        let mut archive = lib.open(&id).expect("open");
        archive.begin_write(Recovery::Run).expect("begin_write");
        for f in ["archive.sqlite-wal", "archive.sqlite-shm"] {
            assert_eq!(mode(&dir.join(f)), 0o600, "{f}");
        }
        archive.close();
    }

    let reader = lib.open_read(&id).expect("open_read");
    let info = reader.info().expect("info");
    assert_eq!(info.id, id);
    assert_eq!(info.label, LabelState::Ok(label));
    assert_eq!(info.schema_version, 1);
    assert!(info.created_at_ns > 0);
    assert_eq!(info.stats, None);
    assert_eq!(info.sessions.total(), 0);
    assert_eq!(info.resumable, None);
    assert_eq!(info.import_blocked, None);
    assert!(!info.stat_tables);

    // The header and meta row as created.
    drop(reader);
    let c = raw(&db_path(&tmp, &id));
    let (app_id, uv, jm): (i64, i64, String) = (
        c.query_row("PRAGMA application_id", [], |r| r.get(0))
            .expect("app id"),
        c.query_row("PRAGMA user_version", [], |r| r.get(0))
            .expect("uv"),
        c.query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .expect("jm"),
    );
    assert_eq!(app_id, 0x4F4D_4131);
    assert_eq!(uv, 1);
    assert_eq!(jm, "wal");
    let (fmt, sv, uuid_len, tok, deriver): (String, i64, i64, String, i64) = c
        .query_row(
            "SELECT format, schema_version, length(archive_uuid), fts_tokenizer, \
             max_deriver_version FROM archive_meta",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .expect("meta");
    assert_eq!(
        (fmt.as_str(), sv, uuid_len, tok.as_str(), deriver),
        (
            "openmessage-archive",
            1,
            16,
            "unicode61 remove_diacritics 2",
            0
        )
    );
    let secure_delete: i64 = c
        .query_row(
            "SELECT v FROM message_fts_config WHERE k = 'secure-delete'",
            [],
            |r| r.get(0),
        )
        .expect("secure-delete");
    assert_eq!(secure_delete, 1);
}

#[test]
fn an_archive_without_a_label_and_two_archives_list_in_id_order() {
    let (tmp, lib) = library();
    let a = lib.create(None).expect("a");
    let b = lib.create(None).expect("b");
    assert_ne!(a, b);
    // Foreign names, dot-directories and files are skipped.
    let r = root(&tmp);
    fs::create_dir(r.join("notes")).expect("mkdir");
    fs::create_dir(r.join(".oma-staging")).expect("mkdir");
    fs::write(r.join("a-readme.txt"), b"x").expect("write");
    fs::create_dir(r.join(format!("A{}", "0".repeat(32)))).expect("upper");
    let list = lib.list().expect("list");
    assert_eq!(list.len(), 2, "{list:?}");
    let mut ids = [a.clone(), b.clone()];
    ids.sort();
    for (entry, id) in list.iter().zip(ids.iter()) {
        match entry {
            ArchiveEntry::Ready(info) => {
                assert_eq!(&info.id, id);
                assert_eq!(info.label, LabelState::None);
            }
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn a_second_open_is_locked_and_list_reports_in_use_without_waiting() {
    let (_tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let first = lib.open(&id).expect("open");
    let started = std::time::Instant::now();
    assert!(matches!(lib.open(&id), Err(ArchiveError::Locked)));
    assert!(started.elapsed() >= Duration::from_millis(900), "retries");
    let started = std::time::Instant::now();
    let list = lib.list().expect("list");
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "list never waits"
    );
    assert!(matches!(&list[..], [ArchiveEntry::InUse { id: got }] if *got == id));
    // The reader takes no lock.
    let reader = lib.open_read(&id).expect("reader while locked");
    assert_eq!(reader.info().expect("info").id, id);
    drop(first);
    assert!(matches!(
        &lib.list().expect("list")[..],
        [ArchiveEntry::Ready(_)]
    ));
    lib.open(&id).expect("open after release");
}

/// Run by [`a_lock_held_by_a_child_process_is_respected`]: holds the lock
/// named by `OMA_TEST_LOCK_PATH` until stdin closes.
#[test]
#[ignore = "helper process for a_lock_held_by_a_child_process_is_respected"]
fn child_lock_holder() {
    let Ok(path) = std::env::var("OMA_TEST_LOCK_PATH") else {
        return;
    };
    let f = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("open lock");
    f.lock().expect("lock");
    println!("OMA-LOCK-HELD");
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    drop(f);
}

#[test]
fn a_lock_held_by_a_child_process_is_respected() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let lock = archive_dir(&tmp, &id).join("archive.lock");
    let mut child = Command::new(std::env::current_exe().expect("exe"))
        .args([
            "--ignored",
            "--exact",
            "child_lock_holder",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("OMA_TEST_LOCK_PATH", &lock)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn");
    let stdout = child.stdout.take().expect("stdout");
    let mut lines = BufReader::new(stdout).lines();
    loop {
        let line = lines.next().expect("child output").expect("line");
        if line.contains("OMA-LOCK-HELD") {
            break;
        }
    }
    assert!(matches!(lib.open(&id), Err(ArchiveError::Locked)));
    assert!(matches!(
        &lib.list().expect("list")[..],
        [ArchiveEntry::InUse { .. }]
    ));
    drop(child.stdin.take());
    assert!(child.wait().expect("wait").success());
    lib.open(&id).expect("open after the child exits");
}

#[test]
fn a_failed_lock_self_test_gives_lock_unreliable() {
    let (_tmp, lib) = library();
    let id = lib.create(None).expect("create");
    {
        let _faults = install(Faults {
            lock_self_test_fails: true,
            ..Faults::default()
        });
        assert!(matches!(lib.open(&id), Err(ArchiveError::LockUnreliable)));
        assert!(matches!(
            lib.create(None),
            Err(ArchiveError::LockUnreliable)
        ));
    }
    // The failed creation left nothing behind.
    lib.open(&id).expect("open without the fault");
}

#[test]
fn the_lock_file_survives_close_and_a_missing_one_is_recreated() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let lock = archive_dir(&tmp, &id).join("archive.lock");
    lib.open(&id).expect("open").close();
    assert!(lock.is_file());
    fs::remove_file(&lock).expect("remove");
    // Listing never writes: no lock file appears.
    assert!(matches!(
        &lib.list().expect("list")[..],
        [ArchiveEntry::Ready(_)]
    ));
    assert!(!lock.exists());
    let archive = lib.open(&id).expect("open recreates the lock");
    assert!(lock.is_file());
    assert!(matches!(lib.open(&id), Err(ArchiveError::Locked)));
    drop(archive);
}

#[test]
fn open_list_read_and_drop_write_no_archive_data() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    // Leave real content behind first: a session row and a clean close.
    {
        let mut a = lib.open(&id).expect("open");
        a.begin_write(Recovery::Run).expect("begin_write");
        a.close();
    }
    let before = data_snapshot(&tmp, &id);
    for _ in 0..2 {
        let a = lib.open(&id).expect("open");
        let _ = a.reader().info().expect("info");
        drop(a);
        let _ = lib.list().expect("list");
        let r = lib.open_read(&id).expect("open_read");
        let _ = r.sessions().expect("sessions");
        drop(r);
        assert_eq!(data_snapshot(&tmp, &id), before);
    }
}

#[test]
fn a_crash_left_wal_opens_read_only_and_shows_committed_rows() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let db = db_path(&tmp, &id);
    // Commit rows through a connection that never checkpoints, then
    // "crash": copy the database and its -wal while the writer is open.
    let crash = tempfile::tempdir().expect("crash dir");
    {
        let c = raw(&db);
        c.pragma_update(None, "wal_autocheckpoint", 0)
            .expect("no autockpt");
        insert_session(&c, 0);
        insert_session(&c, 1);
        let wal = db.with_file_name("archive.sqlite-wal");
        assert!(
            fs::metadata(&wal).expect("wal").len() > 0,
            "frames in the WAL"
        );
        copy_tree(&archive_dir(&tmp, &id), &crash.path().join(id.as_str()));
    }
    let crash_lib = Library::new(crash.path()).expect("library");
    let main_before = file_hash(&crash.path().join(id.as_str()).join("archive.sqlite"));
    let wal_before = file_hash(&crash.path().join(id.as_str()).join("archive.sqlite-wal"));
    let reader = crash_lib.open_read(&id).expect("open_read");
    let info = reader.info().expect("info");
    assert_eq!(info.sessions.total(), 2);
    assert_eq!(info.sessions.running, 1);
    assert_eq!(info.sessions.completed, 1);
    drop(reader);
    assert_eq!(
        file_hash(&crash.path().join(id.as_str()).join("archive.sqlite")),
        main_before
    );
    assert_eq!(
        file_hash(&crash.path().join(id.as_str()).join("archive.sqlite-wal")),
        wal_before
    );
    // The write open marks the running session interrupted.
    let mut a = crash_lib.open(&id).expect("open");
    let report = a.begin_write(Recovery::Run).expect("begin_write");
    assert_eq!(report.sessions_interrupted, 1);
    let info = a.reader().info().expect("info");
    assert_eq!(info.sessions.interrupted, 1);
    assert_eq!(info.sessions.running, 0);
    // The newest session completed, so nothing is resumable.
    assert_eq!(info.resumable, None);
    a.close();
}

fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
    fs::create_dir_all(to).expect("mkdir");
    #[cfg(unix)]
    chmod(to, 0o700);
    for e in fs::read_dir(from).expect("read_dir").flatten() {
        let src = e.path();
        let dst = to.join(e.file_name());
        if src.is_dir() {
            copy_tree(&src, &dst);
        } else {
            fs::copy(&src, &dst).expect("copy");
            #[cfg(unix)]
            chmod(&dst, 0o600);
        }
    }
}

#[test]
fn begin_write_recovery_sweeps_reconciles_and_marks_interrupted() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let dir = archive_dir(&tmp, &id);
    let incoming = dir.join("attachments").join(".incoming");
    fs::write(incoming.join("7.partial"), b"partial plaintext").expect("partial");
    fs::write(incoming.join("notes.txt"), b"keep").expect("other");
    fs::write(dir.join("imports").join("session-3.json.tmp"), b"{}").expect("tmp");
    fs::write(dir.join("imports").join("session-3.json"), b"{}").expect("final");
    // Two intents: one whose blob row exists, one rolled back with its
    // placed file on disk.
    let committed = [0x11u8; 32];
    let rolled_back = [0x22u8; 32];
    let placed = store_hh(&dir, "22").join(hex(&rolled_back));
    fs::write(&placed, b"orphan").expect("placed");
    let kept_blob = store_hh(&dir, "11").join(hex(&committed));
    fs::write(&kept_blob, b"kept").expect("blob file");
    {
        let c = raw(&db_path(&tmp, &id));
        let s = insert_session(&c, 0);
        insert_session(&c, 1);
        c.execute(
            "INSERT INTO blob (sha256, byte_len, storage_name, first_session_id) \
             VALUES (?1, 4, ?2, ?3)",
            rusqlite::params![committed.as_slice(), hex(&committed), s],
        )
        .expect("blob");
        for sha in [committed, rolled_back] {
            c.execute(
                "INSERT INTO blob_intent (sha256, session_id) VALUES (?1, ?2)",
                rusqlite::params![sha.as_slice(), s],
            )
            .expect("intent");
        }
    }
    let mut a = lib.open(&id).expect("open");
    // Skip opens the writer without touching anything.
    let skipped = a.begin_write(Recovery::Skip).expect("skip");
    assert!(!skipped.ran);
    assert!(incoming.join("7.partial").exists());
    let r = a.begin_write(Recovery::Run).expect("run");
    assert!(r.ran);
    assert_eq!((r.partials_removed, r.partials_left), (1, 0));
    assert_eq!((r.provenance_tmp_removed, r.provenance_tmp_left), (1, 0));
    assert_eq!(
        (
            r.intents_committed,
            r.intents_rolled_back,
            r.intent_files_left
        ),
        (1, 1, 0)
    );
    assert_eq!(r.sessions_interrupted, 1);
    assert!(!incoming.join("7.partial").exists());
    assert!(incoming.join("notes.txt").exists());
    assert!(!dir.join("imports").join("session-3.json.tmp").exists());
    assert!(dir.join("imports").join("session-3.json").exists());
    assert!(!placed.exists(), "the uncommitted placement is removed");
    assert!(kept_blob.exists(), "a committed blob's file stays");
    // Recovery runs once per open.
    assert!(!a.begin_write(Recovery::Run).expect("again").ran);
    let report = a.close();
    assert_eq!(report.checkpoint, Some(CheckpointOutcome::Done));
    let c = raw(&db_path(&tmp, &id));
    let intents: i64 = c
        .query_row("SELECT count(*) FROM blob_intent", [], |r| r.get(0))
        .expect("count");
    assert_eq!(intents, 0);
    let (state, ended): (i64, Option<i64>) = c
        .query_row(
            "SELECT state, ended_at_ns FROM import_session WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("session");
    assert_eq!(state, 4);
    assert!(ended.is_some());
}

#[test]
fn close_without_a_writer_does_not_checkpoint() {
    let (_tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let a = lib.open(&id).expect("open");
    assert_eq!(a.close().checkpoint, None);
}

#[test]
fn sweep_removes_only_stale_unlocked_expected_creation_directories() {
    let (tmp, lib) = library();
    let r = root(&tmp);
    let hex32 = |c: char| c.to_string().repeat(32);

    // 1. A complete, unlocked creation directory (as a crash before the
    //    rename leaves it): removed.
    let id = lib.create(None).expect("create");
    let complete = r.join(format!(".oma-new-{}", hex32('1')));
    fs::rename(archive_dir(&tmp, &id), &complete).expect("rename back");
    // 2. An early crash: only the lock file. Removed.
    let early = r.join(format!(".oma-new-{}", hex32('2')));
    fs::create_dir(&early).expect("mkdir");
    fs::write(early.join("archive.lock"), b"").expect("lock");
    // 3. A planted directory with other files: left alone.
    let planted = r.join(format!(".oma-new-{}", hex32('3')));
    fs::create_dir(&planted).expect("mkdir");
    fs::write(planted.join("photo.jpg"), b"not ours").expect("write");
    // 4. A file in imports/: left alone.
    let busy_imports = r.join(format!(".oma-new-{}", hex32('4')));
    fs::create_dir_all(busy_imports.join("imports")).expect("mkdir");
    fs::write(busy_imports.join("imports").join("session-1.json"), b"{}").expect("write");
    // 5. A held lock: in use.
    let held = r.join(format!(".oma-new-{}", hex32('5')));
    fs::create_dir(&held).expect("mkdir");
    fs::write(held.join("archive.lock"), b"").expect("lock");
    let holder = fs::OpenOptions::new()
        .read(true)
        .open(held.join("archive.lock"))
        .expect("open");
    holder.lock().expect("lock");
    // 6. A badly named directory is not a creation directory.
    let other = r.join(".oma-new-xyz");
    fs::create_dir(&other).expect("mkdir");

    // Everything is younger than five minutes.
    let fresh = lib.sweep();
    assert_eq!(fresh.removed, 0);
    assert_eq!(fresh.too_recent, 5);

    let report = lib.sweep_older_than(Duration::ZERO);
    assert_eq!(report.removed, 2, "{report:?}");
    assert_eq!(report.left_alone, 2, "{report:?}");
    assert_eq!(report.in_use, 1, "{report:?}");
    assert!(!complete.exists());
    assert!(!early.exists());
    assert!(planted.join("photo.jpg").exists());
    assert!(busy_imports.join("imports").join("session-1.json").exists());
    assert!(held.exists());
    assert!(other.exists());
    drop(holder);
    assert_eq!(lib.sweep_older_than(Duration::ZERO).removed, 1);
    // The listing never showed any of them.
    assert!(lib.list().expect("list").is_empty());
}

#[cfg(unix)]
#[test]
fn a_linked_file_inside_a_creation_directory_is_never_followed() {
    let (tmp, lib) = library();
    let r = root(&tmp);
    let outside = tempfile::tempdir().expect("outside");
    let target = outside.path().join("precious");
    fs::write(&target, b"keep").expect("write");
    let planted = r.join(format!(".oma-new-{}", "6".repeat(32)));
    fs::create_dir(&planted).expect("mkdir");
    std::os::unix::fs::symlink(&target, planted.join("archive.sqlite")).expect("symlink");
    let report = lib.sweep_older_than(Duration::ZERO);
    assert_eq!(report.left_alone, 1);
    assert_eq!(fs::read(&target).expect("read"), b"keep");
}

#[test]
fn library_new_refuses_files_backups_and_missing_paths() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let file = tmp.path().join("file");
    fs::write(&file, b"x").expect("write");
    assert!(matches!(
        Library::new(&file),
        Err(ArchiveError::Location(LocationRefusal::NotADirectory))
    ));
    assert!(matches!(
        Library::new(&tmp.path().join("missing")),
        Err(ArchiveError::Io { .. })
    ));
    // A folder inside a backup (it holds Manifest.db and Info.plist).
    let backup = tmp.path().join("00008030-001A35E22E38802E");
    fs::create_dir(&backup).expect("mkdir");
    fs::write(backup.join("Manifest.db"), b"x").expect("write");
    fs::write(backup.join("Info.plist"), b"x").expect("write");
    fs::write(backup.join("Status.plist"), b"x").expect("write");
    fs::write(backup.join("Manifest.plist"), b"x").expect("write");
    let inside = backup.join("archives");
    fs::create_dir(&inside).expect("mkdir");
    assert!(matches!(
        Library::new(&inside),
        Err(ArchiveError::Location(LocationRefusal::InsideBackup))
    ));
    let mobilesync = tmp.path().join("MobileSync").join("Backup").join("lib");
    fs::create_dir_all(&mobilesync).expect("mkdir");
    assert!(matches!(
        Library::new(&mobilesync),
        Err(ArchiveError::Location(LocationRefusal::InsideBackup))
    ));
    #[cfg(unix)]
    {
        assert!(matches!(
            Library::new(std::path::Path::new("/")),
            Err(ArchiveError::Location(LocationRefusal::FilesystemRoot))
        ));
        let shared = tmp.path().join("shared");
        fs::create_dir(&shared).expect("mkdir");
        chmod(&shared, 0o777);
        assert!(matches!(
            Library::new(&shared),
            Err(ArchiveError::Permissions(
                oma_archive::PermProblem::Loose { .. }
            ))
        ));
        chmod(&shared, 0o755);
        Library::new(&shared).expect("readable by others is fine");
    }
}

#[test]
fn creation_needs_64_mib_and_treats_unknown_space_as_unknown() {
    let (tmp, lib) = library();
    let lib = lib.with_space_probe(fake_space(Some((64 << 20) - 1), None));
    let _ = &tmp;
    assert!(matches!(
        lib.create(None),
        Err(ArchiveError::Location(LocationRefusal::InsufficientSpace))
    ));
    assert!(lib.list().expect("list").is_empty());
    let lib = lib.with_space_probe(fake_space(None, None));
    lib.create(None)
        .expect("unknown space does not block creation");
}
