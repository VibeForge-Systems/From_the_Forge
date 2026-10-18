//! Archive creation through `StoreFs` (spec §5.2, §18.2): the creation
//! directory's subdirectories are made durably and the directory itself is
//! flushed **before** the no-replace rename, and the library directory is
//! flushed after it. A rename that is refused leaves nothing behind.
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
        RealFs.create_dir_durable(path)
    }

    fn fsync_dir(&self, path: &Path) -> io::Result<DirSync> {
        self.ops
            .lock()
            .expect("lock")
            .push(format!("fsync {}", last(path)));
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
