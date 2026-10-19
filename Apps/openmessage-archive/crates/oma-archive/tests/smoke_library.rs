//! A library for the headless smoke test of the viewer (M5 slice 5k,
//! `docs/development.md`). Ignored by default; it writes only below the
//! folder named by `OMA_SMOKE_LIB`, which must be empty or absent:
//!
//! ```sh
//! OMA_SMOKE_LIB=/dev/shm/oma-smoke/lib \
//!   cargo test -p oma-archive --test smoke_library -- --ignored --nocapture
//! ```
//!
//! It creates one archive named "Smoke viewer" and imports three entries
//! of the synthetic viewer catalogue into it, each as a new import set:
//! `view-main-plain` (reactions, replies, edits, unsends, events),
//! `view-media-plain` (one attachment per media file) and
//! `view-hostile-text-plain`. The committed fixtures are read in place and
//! never written. Point the app's `archiveDirectory` at the folder.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;

use std::path::PathBuf;

use common::import::{import, open_dir, opts, UDID};
use common::viewer::viewer_root;
use common::{fake_space, TEST_AVAILABLE};
use oma_archive::{Label, Library, LineageChoice};

const ENTRIES: [&str; 3] = [
    "view-main-plain",
    "view-media-plain",
    "view-hostile-text-plain",
];

#[test]
#[ignore = "writes a library below OMA_SMOKE_LIB; run by hand"]
fn make_smoke_library() {
    let root = PathBuf::from(std::env::var("OMA_SMOKE_LIB").expect("set OMA_SMOKE_LIB"));
    std::fs::create_dir_all(&root).expect("create the library folder");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    }
    assert!(
        std::fs::read_dir(&root).expect("read").next().is_none(),
        "OMA_SMOKE_LIB must be empty"
    );
    // The fake probe keeps the import's space reserve from refusing on a
    // nearly full development disk; the files themselves are small.
    let lib = Library::new(&root)
        .expect("library")
        .with_space_probe(fake_space(Some(TEST_AVAILABLE), Some(2 * TEST_AVAILABLE)));
    let id = lib
        .create(Some(Label::parse("Smoke viewer").expect("label")))
        .expect("create");
    for entry in ENTRIES {
        let mut archive = lib.open(&id).expect("open");
        let src = open_dir(&viewer_root().join(entry).join(UDID), None);
        let summary = import(&mut archive, &src, &opts(1000), LineageChoice::New);
        drop(archive);
        assert!(src.ob.close().removed, "{entry}: staging not removed");
        println!("imported {entry} as session {}", summary.session);
    }
}
