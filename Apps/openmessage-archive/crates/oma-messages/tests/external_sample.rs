//! A manual run of `check()` on a backup outside the repository (M3 §20.4
//! oracle 4, and the manual checklist in `docs/compatibility.md`).
//!
//! Ignored by default. Set `OMA_EXTERNAL_BACKUP` to a backup directory
//! (the folder holding `Manifest.db`) and, for an encrypted backup,
//! `OMA_EXTERNAL_PASSWORD`; then run
//!
//! ```sh
//! cargo test -p oma-messages --release --test external_sample -- --ignored --nocapture
//! ```
//!
//! The backup is copied to a temporary directory first, and an archive
//! library is created there, so the original is never opened for writing.
//! It prints aggregate counts and kinds only: the report holds no string
//! from the database, and the date range is left out (D31).
#![allow(clippy::disallowed_types, clippy::disallowed_methods)]
mod common;

use std::path::PathBuf;

use common::expected::project;
use common::*;
use oma_messages::CheckOptions;
use zeroize::Zeroizing;

#[test]
#[ignore = "manual: needs OMA_EXTERNAL_BACKUP"]
fn external_backup() {
    let Some(src) = std::env::var_os("OMA_EXTERNAL_BACKUP").map(PathBuf::from) else {
        eprintln!("OMA_EXTERNAL_BACKUP is not set; nothing to do");
        return;
    };
    let password = std::env::var("OMA_EXTERNAL_PASSWORD")
        .ok()
        .map(Zeroizing::new);
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("backups").join("external").join(UDID);
    copy_tree(&src, &dir);
    let library = tmp.path().join("library");
    std::fs::create_dir(&library).expect("mkdir");
    let fx = Fixture { dir, library, tmp };
    let (result, cleanup) = run_check(
        &fx,
        password.as_ref().map(|p| p.as_str()),
        &CheckOptions::default(),
    );
    assert!(cleanup.removed, "staging was not removed");
    let report = result.expect("check");
    let mut shown = project(&report);
    if let Some(d) = shown.get_mut("dates").and_then(|d| d.as_object_mut()) {
        d.remove("earliest_message");
        d.remove("latest_message");
    }
    println!("{}", serde_json::to_string_pretty(&shown).expect("json"));
    println!("elapsed_ms {}", report.elapsed.as_millis());
}
