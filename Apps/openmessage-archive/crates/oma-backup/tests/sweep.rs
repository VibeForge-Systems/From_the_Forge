//! The startup sweep against sessions left by a job that never closed.
mod common;

use std::fs;
use std::time::{Duration, SystemTime};

use common::*;
use oma_backup::{sweep_stale, StagingRoot};

fn age(path: &std::path::Path) {
    let old = SystemTime::now() - Duration::from_secs(10 * 60);
    #[cfg(windows)]
    let f = {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_WRITE_ATTRIBUTES, plus FILE_FLAG_BACKUP_SEMANTICS, without
        // which Windows refuses to open a directory handle.
        fs::OpenOptions::new()
            .access_mode(0x0100)
            .custom_flags(0x0200_0000)
            .open(path)
    };
    #[cfg(not(windows))]
    let f = fs::File::open(path);
    f.expect("open dir").set_modified(old).expect("age");
}

#[test]
fn a_crashed_session_is_removed_and_a_live_one_is_kept() {
    let fx = Fixture::read_only("plain");
    // A live job.
    let live = fx.open(None).expect("open");
    let live_dir = live.staging_path().expect("staging").to_path_buf();
    age(&live_dir);

    // What a killed process leaves: a session with plaintext and an
    // unlocked .lock.
    let root = StagingRoot::new(&fx.library, &fx.candidate()).expect("root");
    let dead = root.path().join("s0123456789abcdef");
    fs::create_dir_all(dead.join("m")).expect("mkdir");
    fs::write(dead.join(".lock"), b"").expect("lock");
    fs::write(dead.join("m").join("Manifest.db"), b"stale plaintext").expect("write");
    age(&dead);

    let report = sweep_stale(&fx.library);
    assert_eq!(report.removed, 1);
    assert_eq!(report.in_use, 1);
    assert!(!dead.exists());
    assert!(live_dir.exists());
    assert!(live.close().removed);
    assert!(!live_dir.exists());
    assert_eq!(sweep_stale(&fx.library).removed, 0);
}
