//! I1: no step ever changes a backup.
mod common;

use common::*;
use oma_backup::{CancelToken, JobCtx, Progress, SMS_DB_DOMAIN, SMS_DB_PATH};

#[test]
fn backup_untouched() {
    for name in ALL_FIXTURES {
        // Read-only on Unix (a write fails loudly); on every OS the
        // comparison below catches any change.
        let fx = Fixture::read_only(name);
        let before = snapshot(&fx.parent);

        let _ = fx.inspect();
        let password = Some(password_for(name));
        if let Ok(ob) = fx.open(password) {
            let token = CancelToken::new();
            let mut sink = |_: Progress| {};
            let mut ctx = JobCtx::new(&token, &mut sink);
            let _ = ob.validate(&mut ctx);
            let _ = ob.extract_database(SMS_DB_DOMAIN, SMS_DB_PATH, &mut ctx);
            if let Ok(Some(entry)) = ob.find_file(ATTACHMENT_DOMAIN, ATTACHMENT_PATH) {
                let _ = ob.extract_file(&entry, &mut ctx);
            }
            assert!(ob.close().removed, "{name}");
        }

        let after = snapshot(&fx.parent);
        assert_eq!(before, after, "{name}: the backup changed");
        assert!(
            !after
                .iter()
                .any(|(p, _)| p.ends_with("-shm") || p.ends_with("-journal")),
            "{name}: SQLite side files appeared"
        );
        assert!(fx.staged_files().is_empty(), "{name}: staging not emptied");
    }
}

#[cfg(unix)]
#[test]
fn read_only_copies_validate() {
    // With the copy read-only, any attempted write would fail loudly.
    for (name, password) in [
        ("plain", None),
        ("enc", Some(PASSWORD)),
        ("plain-wal", None),
    ] {
        let fx = Fixture::read_only(name);
        let (report, cleanup) = fx.validate(password);
        assert!(report.importable(), "{name}");
        assert!(cleanup.removed);
    }
}
