//! Generates the golden archive `tests/fixtures/archives/golden-v1/`
//! (spec §7.3, §19.2).
//!
//! ```sh
//! cargo run -p oma-archive --example make_golden            # the default path
//! cargo run -p oma-archive --example make_golden -- <dir>   # elsewhere
//! ```
//!
//! It creates a library with one archive labelled "Golden archive v1",
//! imports the synthetic catalogue entries `imp-dev1-v1-plain` and then
//! `imp-dev1-v2-plain` (the lineage the gate suggests, so one lineage)
//! with a fixed clock and no time budget, closes it, and writes:
//!
//! - `library/<a…>/`: the archive as this build leaves it (`archive.lock`,
//!   `archive.sqlite`, `attachments/<hh>/<sha256>`, `imports/session-*.json`),
//!   without the empty `-wal` and the `-shm` that closing leaves; git keeps
//!   no empty directory, so `attachments/.incoming/` is recreated by the
//!   test;
//! - `SHA256SUMS`: every file under `library/`;
//! - `logical_snapshot.json`: the test suite's `logical_snapshot` with the
//!   session bookkeeping.
//!
//! It refuses to overwrite an existing directory. The golden archive is
//! regenerated only until schema v1 freezes at the M4 release tag, and is
//! frozen after that (C1.8, C3.14). Synthetic content only.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

#[path = "../tests/common/mod.rs"]
mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::import::{logical_snapshot, open_entry, Scope, MODEL_NOW_NS};
use common::{fake_space, hex, TEST_AVAILABLE};
use oma_archive::{
    ArchiveCtx, BatchBudget, CancelToken, FixedClock, ImportOptions, ImportOutcome, Label, Library,
    NoProgress, ProceedWithSuggestion,
};
use sha2::{Digest, Sha256};

const ENTRIES: [&str; 2] = ["imp-dev1-v1-plain", "imp-dev1-v2-plain"];

fn files(base: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .expect("read_dir")
        .map(|e| e.expect("entry"))
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        if e.file_type().expect("type").is_dir() {
            files(base, &p, out);
        } else {
            let rel: Vec<String> = p
                .strip_prefix(base)
                .expect("prefix")
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            out.push((rel.join("/"), p));
        }
    }
}

fn main() {
    let out = std::env::args().nth(1).map_or_else(
        || {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("..")
                .join("tests")
                .join("fixtures")
                .join("archives")
                .join("golden-v1")
        },
        PathBuf::from,
    );
    if out.exists() {
        eprintln!("refusing to overwrite an existing directory; remove it first");
        std::process::exit(2);
    }
    let library = out.join("library");
    fs::create_dir_all(&library).expect("library dir");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&library, fs::Permissions::from_mode(0o700)).expect("chmod");
    }
    // A fixed clock, and a space probe that reports room (the runner's
    // disk does not matter for a 1 MB archive).
    let lib = Library::new(&library)
        .expect("library")
        .with_space_probe(fake_space(Some(TEST_AVAILABLE), Some(2 * TEST_AVAILABLE)))
        .with_clock(Arc::new(FixedClock(MODEL_NOW_NS)));
    let id = lib
        .create(Some(Label::parse("Golden archive v1").expect("label")))
        .expect("create");
    let mut a = lib.open(&id).expect("open");
    let mut opts = ImportOptions::default();
    opts.batch = BatchBudget::new(1000, 32 << 20, None);
    for entry in ENTRIES {
        let src = open_entry(entry);
        let clock = FixedClock(MODEL_NOW_NS);
        let mut ctx = ArchiveCtx {
            cancel: &CancelToken::new(),
            progress: &mut NoProgress,
            clock: &clock,
        };
        let outcome = a
            .import(&src.ob, &opts, &mut ProceedWithSuggestion, &mut ctx)
            .expect("import");
        assert!(
            matches!(outcome, ImportOutcome::Completed(_)),
            "{entry}: {outcome:?}"
        );
        src.ob.close();
    }
    a.close();

    let db = library.join(id.as_str()).join("archive.sqlite");
    let snapshot = logical_snapshot(&db, Scope::WithSessions);
    let mut json = serde_json::to_vec_pretty(&snapshot).expect("json");
    json.push(b'\n');
    fs::write(out.join("logical_snapshot.json"), json).expect("snapshot");
    // Every connection is closed, the snapshot's too. The checkpoint left
    // an empty `-wal`, and a read-only connection closing last leaves the
    // `-shm` (SQLite's WAL index, no content): neither is fixture data.
    let wal = db.with_file_name("archive.sqlite-wal");
    if let Ok(m) = fs::metadata(&wal) {
        assert_eq!(m.len(), 0, "the -wal was not checkpointed");
        fs::remove_file(&wal).expect("remove -wal");
    }
    let shm = db.with_file_name("archive.sqlite-shm");
    if shm.exists() {
        fs::remove_file(&shm).expect("remove -shm");
    }
    let mut list = Vec::new();
    files(&out, &library, &mut list);
    let mut sums = String::new();
    for (rel, p) in &list {
        let bytes = fs::read(p).expect("read");
        sums.push_str(&hex(&Sha256::digest(&bytes)));
        sums.push_str("  ");
        sums.push_str(rel);
        sums.push('\n');
    }
    fs::write(out.join("SHA256SUMS"), sums).expect("sums");
    println!("wrote {} files", list.len());
}
