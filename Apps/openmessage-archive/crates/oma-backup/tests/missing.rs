//! A backup that lists sms.db but lacks its blob.
mod common;

use common::*;
use oma_backup::{CheckId, CheckLevel, Finding};

#[test]
fn sms_blob() {
    let fx = Fixture::read_only("missing-sms-blob");
    let (report, cleanup) = fx.validate(None);
    assert_eq!(report.stats.missing_blobs, 1);
    let blobs = report.check(CheckId::Blobs).expect("blobs");
    assert_eq!(blobs.level, CheckLevel::Warning);
    let m = report.check(CheckId::MessagesDb).expect("messages-db");
    assert_eq!(m.level, CheckLevel::Error);
    assert!(m.findings.contains(&Finding::SmsBlobMissing));
    assert_eq!(report.sms_db.blob_present, Some(false));
    assert!(!report.importable());
    assert!(cleanup.removed);
}

/// A blob folder that cannot be searched is counted, and the report is still
/// produced. Root ignores permissions, so the test checks that the chmod took
/// effect and returns early otherwise.
#[cfg(unix)]
#[test]
fn an_unreadable_blob_folder_is_counted_not_fatal() {
    use oma_backup::SMS_DB_FILE_ID;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    let fx = Fixture::new("plain");
    let file_id = {
        let ob = fx.open(None).expect("open");
        let entry = ob
            .find_file(ATTACHMENT_DOMAIN, ATTACHMENT_PATH)
            .expect("lookup")
            .expect("listed");
        assert!(ob.close().removed);
        entry.file_id
    };
    let prefix = &file_id[..2];
    assert_ne!(prefix, &SMS_DB_FILE_ID[..2], "sms.db shares the folder");
    let dir = fx.dir.join(prefix);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o600)).expect("chmod");
    let effective = fs::symlink_metadata(dir.join(&file_id)).is_err();
    if effective {
        let (report, cleanup) = fx.validate(None);
        assert!(report.stats.unreadable_blobs >= 1);
        let blobs = report.check(CheckId::Blobs).expect("blobs");
        assert_eq!(blobs.level, CheckLevel::Warning);
        assert!(blobs
            .findings
            .iter()
            .any(|f| matches!(f, Finding::UnreadableBlobs(_))));
        assert!(cleanup.removed);
    }
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("chmod back");
}
