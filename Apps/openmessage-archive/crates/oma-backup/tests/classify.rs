//! Classification of every fixture that stops before decryption (§5).
mod common;

use common::*;
use oma_backup::{
    scan_root, BackupError, BackupFile, Classification, Limits, MalformedDetail, NoteKind,
    SnapshotState, UnsupportedReason,
};

fn classification(name: &str) -> Classification {
    Fixture::read_only(name).inspect().classification
}

#[test]
fn incomplete() {
    let fx = Fixture::read_only("incomplete");
    let i = fx.inspect();
    assert_eq!(
        i.classification,
        Classification::Incomplete {
            snapshot: SnapshotState::Other,
            snapshot_dir: false
        }
    );
    // The raw value is kept for display only.
    let raw = i
        .status
        .as_ref()
        .and_then(|s| s.snapshot_state_raw.as_ref())
        .map(|t| t.as_str().to_owned());
    assert_eq!(raw.as_deref(), Some(SNAPSHOT_SENTINEL));
    assert!(!format!("{i:?}").contains(SNAPSHOT_SENTINEL));
    let err = fx.open(None).expect_err("incomplete");
    assert!(matches!(
        err.error,
        BackupError::Incomplete {
            snapshot: SnapshotState::Other
        }
    ));
    assert!(err.cleanup.removed);
}

#[test]
fn incomplete_no_status() {
    assert_eq!(
        classification("incomplete-no-status"),
        Classification::Incomplete {
            snapshot: SnapshotState::Absent,
            snapshot_dir: false
        }
    );
}

#[test]
fn snapshot_dir() {
    let i = Fixture::read_only("snapshot-dir").inspect();
    assert_eq!(
        i.classification,
        Classification::Incomplete {
            snapshot: SnapshotState::Other,
            snapshot_dir: true
        }
    );
    assert!(i.layout.has_snapshot_dir);
    assert!(i
        .notes
        .iter()
        .any(|n| n.kind == NoteKind::SnapshotDirPresent));
}

#[test]
fn legacy_mbdb() {
    let i = Fixture::read_only("legacy-mbdb").inspect();
    assert_eq!(
        i.classification,
        Classification::Unsupported(UnsupportedReason::LegacyMbdb)
    );
    assert!(i.layout.has_manifest_mbdb);
    assert_eq!(i.layout.mbdb_signature_ok, Some(true));
    assert!(!i.is_importable());
}

#[test]
fn encryption_layouts() {
    assert_eq!(
        classification("enc-no-manifestkey"),
        Classification::Unsupported(UnsupportedReason::EncryptedWithoutManifestKey)
    );
    assert_eq!(
        classification("enc-no-keybag"),
        Classification::Unsupported(UnsupportedReason::NoKeybag)
    );
    assert_eq!(
        classification("wrong-type-isencrypted"),
        Classification::Unsupported(UnsupportedReason::EncryptionUnknown)
    );
    assert_eq!(
        classification("encrypted-plaintext-db"),
        Classification::Unreadable {
            file: BackupFile::ManifestDb,
            detail: MalformedDetail::Inconsistent("encrypted but plaintext")
        }
    );
}

#[test]
fn plain_no_keybag_is_ready_with_a_note() {
    let i = Fixture::read_only("plain-no-keybag").inspect();
    assert_eq!(i.classification, Classification::Ready);
    assert!(i.notes.iter().any(|n| n.kind == NoteKind::NoKeybag));
}

#[test]
fn device_details_and_provenance() {
    let i = Fixture::read_only("enc").inspect();
    let name = i.device_name().expect("device name");
    assert_eq!(name.value.as_str(), DEVICE_NAME);
    assert_eq!(name.source, BackupFile::InfoPlist);
    assert_eq!(
        i.ios_version().map(|v| v.value.as_str().to_owned()),
        Some("18.0".to_owned())
    );
    assert!(i
        .info
        .as_ref()
        .is_some_and(|x| x.last_backup_date.is_some()));
    assert_eq!(i.is_encrypted(), Some(true));
}

#[test]
fn every_fixture_parent_is_a_scannable_root() {
    for name in ALL_FIXTURES {
        let scan = scan_root(&fixtures_dir().join(name), &Limits::default());
        assert_eq!(scan.candidates.len(), 1, "{name}");
    }
}
