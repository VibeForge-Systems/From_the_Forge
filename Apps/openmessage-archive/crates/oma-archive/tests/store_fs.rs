//! Archive creation through `StoreFs` (spec §5.2, §18.2): the creation
//! directory's subdirectories are made durably and the directory itself is
//! flushed **before** the no-replace rename, and the library directory is
//! flushed after it. A rename that is refused leaves nothing behind; a
//! failed library flush after the rename still returns the archive; a
//! drive that ignores modes fails creation. The intent reconcile flushes
//! each directory it removed a file from before it commits (spec §9.1).
#![allow(clippy::disallowed_methods)]

mod common;

use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};

use common::*;
use oma_archive::{DirSync, Library, Placed, RealFs, StoreFs};

#[derive(Default)]
struct Recording {
    ops: Mutex<Vec<String>>,
    refuse_rename: bool,
    /// Fail the library directory's flush.
    fail_library_fsync: bool,
    /// Give every created directory mode 0755, as vfat or exFAT mounted
    /// with the usual umask reports.
    loose_dirs: bool,
    /// While reconciling: the database, to count `blob_intent` rows at
    /// each flush of a store directory.
    db: Mutex<Option<std::path::PathBuf>>,
}

fn last(p: &Path) -> String {
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if name.starts_with(".oma-new-") {
        ".oma-new".into()
    } else if name.len() == 33 && name.starts_with('a') {
        "a<id>".into()
    } else if name.starts_with(".tmp") {
        "<library>".into()
    } else {
        name
    }
}

impl StoreFs for Recording {
    fn create_dir_durable(&self, path: &Path) -> io::Result<()> {
        self.ops
            .lock()
            .expect("lock")
            .push(format!("mkdir {}", last(path)));
        RealFs.create_dir_durable(path)?;
        #[cfg(unix)]
        if self.loose_dirs {
            chmod(path, 0o755);
        }
        #[cfg(not(unix))]
        let _ = self.loose_dirs;
        Ok(())
    }

    fn fsync_dir(&self, path: &Path) -> io::Result<DirSync> {
        let name = last(path);
        let mut op = format!("fsync {name}");
        if let Some(db) = self.db.lock().expect("lock").as_ref() {
            // What is on disk and committed at this flush.
            let files = std::fs::read_dir(path).expect("dir").count();
            let intents: i64 = rusqlite::Connection::open_with_flags(
                db,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .expect("open")
            .query_row("SELECT count(*) FROM blob_intent", [], |r| r.get(0))
            .expect("count");
            op.push_str(&format!(" files={files} intents={intents}"));
        }
        self.ops.lock().expect("lock").push(op);
        if self.fail_library_fsync && name == "<library>" {
            return Err(io::Error::other("injected flush failure"));
        }
        RealFs.fsync_dir(path)
    }

    fn rename_dir_noreplace(&self, from: &Path, to: &Path) -> io::Result<Placed> {
        self.ops
            .lock()
            .expect("lock")
            .push(format!("rename {} {}", last(from), last(to)));
        if self.refuse_rename {
            return Err(io::Error::from(io::ErrorKind::AlreadyExists));
        }
        RealFs.rename_dir_noreplace(from, to)
    }

    fn place_noreplace(&self, from: &Path, to: &Path) -> io::Result<Placed> {
        self.ops
            .lock()
            .expect("lock")
            .push(format!("place {} {}", last(from), last(to)));
        RealFs.place_noreplace(from, to)
    }
}

#[test]
fn creation_flushes_before_the_no_replace_rename() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let rec = Arc::new(Recording::default());
    let lib = Library::new(tmp.path())
        .expect("library")
        .with_store_fs(rec.clone());
    let id = lib.create(None).expect("create");
    let ops = rec.ops.lock().expect("lock").clone();
    assert_eq!(
        ops,
        [
            "mkdir attachments",
            "mkdir .incoming",
            "mkdir imports",
            "fsync .oma-new",
            "rename .oma-new a<id>",
            "fsync <library>",
        ]
    );
    let a = lib.open(&id).expect("open");
    drop(a);
}

#[test]
fn a_refused_rename_leaves_no_creation_directory() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let rec = Arc::new(Recording {
        refuse_rename: true,
        ..Recording::default()
    });
    let lib = Library::new(tmp.path())
        .expect("library")
        .with_store_fs(rec.clone());
    let err = lib.create(None).expect_err("refused");
    assert!(
        matches!(
            err,
            oma_archive::ArchiveError::Io {
                kind: io::ErrorKind::AlreadyExists,
                ..
            }
        ),
        "{err:?}"
    );
    assert_eq!(std::fs::read_dir(tmp.path()).expect("dir").count(), 0);
    assert!(lib.list().expect("list").is_empty());
    let _ = root(&tmp);
}

#[test]
fn a_failed_library_flush_after_the_rename_still_returns_the_archive() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let rec = Arc::new(Recording {
        fail_library_fsync: true,
        ..Recording::default()
    });
    let lib = Library::new(tmp.path())
        .expect("library")
        .with_store_fs(rec.clone());
    let id = lib
        .create(None)
        .expect("created, with a durability warning");
    assert_eq!(
        rec.ops.lock().expect("lock").last().map(String::as_str),
        Some("fsync <library>")
    );
    let listed = lib.list().expect("list");
    assert!(
        matches!(&listed[..], [oma_archive::ArchiveEntry::Ready(info)] if info.id == id),
        "{listed:?}"
    );
    drop(lib.open(&id).expect("open"));
}

/// A drive that ignores the requested modes (vfat or exFAT mounted by
/// udisks reports 0755 directories) fails creation, which leaves nothing,
/// instead of creating an archive no open accepts.
#[cfg(unix)]
#[test]
fn a_drive_that_ignores_modes_fails_creation_and_leaves_nothing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let rec = Arc::new(Recording {
        loose_dirs: true,
        ..Recording::default()
    });
    let lib = Library::new(tmp.path())
        .expect("library")
        .with_store_fs(rec.clone());
    let err = lib.create(None).expect_err("refused");
    assert!(
        matches!(
            err,
            oma_archive::ArchiveError::Permissions(oma_archive::PermProblem::Loose { .. })
        ),
        "{err:?}"
    );
    assert!(
        !rec.ops
            .lock()
            .expect("lock")
            .iter()
            .any(|o| o.starts_with("rename")),
        "never renamed into place"
    );
    assert_eq!(std::fs::read_dir(tmp.path()).expect("dir").count(), 0);
    assert!(lib.list().expect("list").is_empty());
}

#[test]
fn the_intent_reconcile_flushes_each_directory_before_it_commits() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let rec = Arc::new(Recording::default());
    let lib = Library::new(tmp.path())
        .expect("library")
        .with_store_fs(rec.clone());
    let id = lib.create(None).expect("create");
    let dir = archive_dir(&tmp, &id);
    let db = db_path(&tmp, &id);
    // Two rolled-back placements in two directories, one committed blob.
    let shas = [[0x22u8; 32], [0x23u8; 32], [0x24u8; 32]];
    let committed = shas[2];
    for sha in &shas {
        let hh = store_hh(&dir, &hex(&sha[..1]));
        std::fs::write(hh.join(hex(sha)), b"placed").expect("placed");
    }
    {
        let c = raw(&db);
        let s = insert_session(&c, 4);
        c.execute(
            "INSERT INTO blob (sha256, byte_len, storage_name, first_session_id) \
             VALUES (?1, 6, ?2, ?3)",
            rusqlite::params![committed.as_slice(), hex(&committed), s],
        )
        .expect("blob");
        for sha in &shas {
            c.execute(
                "INSERT INTO blob_intent (sha256, session_id) VALUES (?1, ?2)",
                rusqlite::params![sha.as_slice(), s],
            )
            .expect("intent");
        }
    }
    rec.ops.lock().expect("lock").clear();
    *rec.db.lock().expect("lock") = Some(db.clone());
    let mut a = lib.open(&id).expect("open");
    let r = a.begin_write(oma_archive::Recovery::Run).expect("recover");
    *rec.db.lock().expect("lock") = None;
    assert_eq!(
        (
            r.intents_committed,
            r.intents_rolled_back,
            r.intent_files_left
        ),
        (1, 2, 0)
    );
    // Each directory is flushed after its file is gone and before the
    // intents' deletion commits; the committed blob's directory is not
    // touched.
    let ops = rec.ops.lock().expect("lock").clone();
    assert_eq!(
        ops,
        ["fsync 22 files=0 intents=3", "fsync 23 files=0 intents=3"],
        "{ops:?}"
    );
    drop(a);
    let left: i64 = raw(&db)
        .query_row("SELECT count(*) FROM blob_intent", [], |r| r.get(0))
        .expect("count");
    assert_eq!(left, 0);
    assert!(dir
        .join("attachments")
        .join("24")
        .join(hex(&committed))
        .exists());
}
