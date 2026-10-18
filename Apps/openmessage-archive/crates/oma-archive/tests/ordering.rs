//! The operation-ordering test (spec §18.4 `ordering.rs`, §9.1, §5.2).
//!
//! A recording `StoreFs` logs every ordered file-system call, and the
//! fault hooks' observer logs every fault point and, through rusqlite's
//! commit hook on the archive's write connections, every `COMMIT`, all in
//! one sequence. From it the test asserts, **for every file placed in the
//! store**:
//!
//! file write → `sync_all` → intent `COMMIT` → rename → directory fsync →
//! row `COMMIT`
//!
//! that is: the `sync_all` of **that same** `.partial` file (the store's
//! `StoreFs::sync_file`, logged with the file's name and its length at the
//! flush, which must be the blob's full length, so the write came first)
//! precedes an intent commit that precedes the rename; the rename's `<hh>`
//! directory is flushed before any later commit; and the next commit after
//! the rename is the batch's row commit. For creation it asserts that the database is
//! committed and the `.oma-new-*` directory flushed before the no-replace
//! rename, and the library flushed after it.
//!
//! What this shows: the order of calls on one run, on the real file
//! system. It does not show that the drive honours the flushes (§9.4).
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;

use std::collections::BTreeMap;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};

use common::import::*;
use common::*;
use oma_archive::hooks::{observe, FaultPoint, Observed};
use oma_archive::{DirSync, Library, LineageChoice, Placed, RealFs, StoreFs};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Ev {
    Point(FaultPoint),
    Commit,
    Mkdir(String),
    Fsync(String),
    Place {
        from: String,
        to: String,
    },
    RenameDir {
        from: String,
        to: String,
    },
    Remove(String),
    /// A file's `sync_all`: its name and its length when flushed.
    SyncFile {
        name: String,
        len: u64,
    },
}

type Log = Arc<Mutex<Vec<Ev>>>;

fn name(p: &Path) -> String {
    let n = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if n.starts_with(".oma-new-") {
        ".oma-new".into()
    } else if n.len() == 33 && n.starts_with('a') {
        "a<id>".into()
    } else {
        n
    }
}

struct Recording {
    log: Log,
    library: String,
}

impl Recording {
    fn push(&self, e: Ev) {
        self.log.lock().expect("lock").push(e);
    }

    fn dir_name(&self, p: &Path) -> String {
        if p.file_name().map(|n| n.to_string_lossy().into_owned()) == Some(self.library.clone()) {
            "<library>".into()
        } else {
            name(p)
        }
    }
}

impl StoreFs for Recording {
    fn create_dir_durable(&self, path: &Path) -> io::Result<()> {
        self.push(Ev::Mkdir(self.dir_name(path)));
        RealFs.create_dir_durable(path)
    }

    fn fsync_dir(&self, path: &Path) -> io::Result<DirSync> {
        self.push(Ev::Fsync(self.dir_name(path)));
        RealFs.fsync_dir(path)
    }

    fn rename_dir_noreplace(&self, from: &Path, to: &Path) -> io::Result<Placed> {
        self.push(Ev::RenameDir {
            from: name(from),
            to: name(to),
        });
        RealFs.rename_dir_noreplace(from, to)
    }

    fn place_noreplace(&self, from: &Path, to: &Path) -> io::Result<Placed> {
        self.push(Ev::Place {
            from: name(from),
            to: name(to),
        });
        RealFs.place_noreplace(from, to)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.push(Ev::Remove(name(path)));
        RealFs.remove_file(path)
    }

    fn sync_file(&self, file: &std::fs::File, path: &Path) -> io::Result<()> {
        let len = file.metadata()?.len();
        let out = RealFs.sync_file(file, path);
        self.push(Ev::SyncFile {
            name: name(path),
            len,
        });
        out
    }
}

fn position(log: &[Ev], from: usize, f: impl Fn(&Ev) -> bool) -> Option<usize> {
    log.iter()
        .enumerate()
        .skip(from)
        .find(|(_, e)| f(e))
        .map(|(i, _)| i)
}

/// Check the §9.1 order for every placement in `log`; returns how many.
/// `lens` gives each placed digest's `byte_len`.
fn assert_store_order(log: &[Ev], lens: &BTreeMap<String, u64>) -> usize {
    let mut placed = 0;
    for (p, e) in log.iter().enumerate() {
        let Ev::Place { from, to } = e else { continue };
        placed += 1;
        assert!(from.ends_with(".partial"), "{from}");
        assert_eq!(to.len(), 64, "{to}");
        let before = log.get(..p).expect("prefix");
        // write → sync_all → intent COMMIT → rename, for this very file:
        // its own flush, with all of its bytes already written.
        let synced = before
            .iter()
            .rposition(|e| matches!(e, Ev::SyncFile { name, .. } if name == from))
            .unwrap_or_else(|| panic!("{to}: {from} placed without its sync_all"));
        let Some(Ev::SyncFile { len, .. }) = before.get(synced) else {
            unreachable!()
        };
        assert_eq!(
            Some(len),
            lens.get(to),
            "{to}: {from} was flushed before all of it was written"
        );
        let intent = position(log, synced, |e| {
            *e == Ev::Point(FaultPoint::AttachmentIntentCommitted)
        })
        .unwrap_or_else(|| panic!("{to}: no intent commit"));
        assert!(intent < p, "{to}: renamed before its intent committed");
        let commit = before
            .get(synced..intent)
            .expect("range")
            .contains(&Ev::Commit);
        assert!(commit, "{to}: the intent was not committed");
        // rename → directory fsync → row COMMIT.
        let hh = to.get(..2).expect("hh").to_owned();
        let fsync = position(log, p, |e| *e == Ev::Fsync(hh.clone()))
            .unwrap_or_else(|| panic!("{to}: its directory was never flushed"));
        let next_commit =
            position(log, p, |e| *e == Ev::Commit).unwrap_or_else(|| panic!("{to}: no row commit"));
        assert!(fsync < next_commit, "{to}: rows committed before the flush");
        let row_batch = log
            .get(..next_commit)
            .expect("prefix")
            .iter()
            .rposition(|e| *e == Ev::Point(FaultPoint::BatchBeforeCommit))
            .expect("a batch");
        assert!(
            row_batch > fsync,
            "{to}: the next commit after the rename is not its row batch"
        );
    }
    placed
}

/// Each blob's `byte_len`, by lowercase hex digest.
fn blob_lens(db: &Path) -> BTreeMap<String, u64> {
    let c = read_only(db);
    let mut stmt = c
        .prepare("SELECT lower(hex(sha256)), byte_len FROM blob")
        .expect("prepare");
    stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            u64::try_from(r.get::<_, i64>(1)?).unwrap_or(0),
        ))
    })
    .expect("query")
    .collect::<Result<_, _>>()
    .expect("rows")
}

#[test]
fn every_store_file_follows_the_write_sync_intent_rename_flush_commit_order() {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&log);
    let _observe = observe(Box::new(move |e| {
        sink.lock().expect("lock").push(match e {
            Observed::Point(p) => Ev::Point(p),
            Observed::Commit => Ev::Commit,
        });
    }));
    let tmp = tempfile::tempdir().expect("tempdir");
    let library_name = name(&dunce::canonicalize(tmp.path()).expect("canonical"));
    let store: Arc<dyn StoreFs> = Arc::new(Recording {
        log: Arc::clone(&log),
        library: library_name,
    });
    let lib = Library::new(tmp.path())
        .expect("library")
        .with_space_probe(fake_space(Some(TEST_AVAILABLE), Some(2 * TEST_AVAILABLE)))
        .with_store_fs(store);

    // Creation (spec §5.2): the database committed and the creation
    // directory flushed before the no-replace rename; the library after.
    let id = lib.create(None).expect("create");
    {
        let l = log.lock().expect("lock");
        let rename = l
            .iter()
            .position(|e| matches!(e, Ev::RenameDir { .. }))
            .expect("rename");
        let flushed = l
            .iter()
            .position(|e| *e == Ev::Fsync(".oma-new".into()))
            .expect("creation directory flushed");
        let committed = l.iter().position(|e| *e == Ev::Commit).expect("commit");
        assert!(committed < flushed && flushed < rename, "{l:?}");
        let mkdirs = l
            .iter()
            .rposition(|e| matches!(e, Ev::Mkdir(_)))
            .expect("mkdir");
        assert!(mkdirs < flushed, "{l:?}");
        let library = position(&l, rename, |e| *e == Ev::Fsync("<library>".into()));
        assert!(library.is_some(), "the library was not flushed: {l:?}");
        assert_eq!(
            l.get(rename),
            Some(&Ev::RenameDir {
                from: ".oma-new".into(),
                to: "a<id>".into()
            })
        );
    }

    // Two imports: new files, then shared and new bytes. One batch per
    // pass for the first, small batches for the second.
    let mut placed = 0;
    for (entry, rows) in [("imp-dev1-v1-plain", 1000), ("imp-dev1-v2-plain", 16)] {
        log.lock().expect("lock").clear();
        let mut a = lib.open(&id).expect("open");
        let src = open_entry(entry);
        let s = import(&mut a, &src, &opts(rows), LineageChoice::New);
        drop(a);
        src.ob.close();
        let l = log.lock().expect("lock").clone();
        let lens = blob_lens(&db_path(&tmp, &id));
        let n = assert_store_order(&l, &lens);
        assert_eq!(
            u64::try_from(n).expect("n"),
            s.counts.attachment_files.copied,
            "{entry}: every copy placed once"
        );
        placed += n;
    }
    assert!(placed >= 7, "{placed} files placed");
    // Every placed file is in the store with its row.
    let db = db_path(&tmp, &id);
    assert_store_consistent(&db, true);
}
