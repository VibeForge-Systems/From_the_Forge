//! Classifying a backup folder (§5).
//!
//! Cheap: a few stats, two short header reads and three budgeted plist reads.
//! SQLite is never opened. Problems become a [`Classification`]; `inspect`
//! returns `Err` only when the folder cannot be read at all.

use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use crate::bplist::{read_bounded, PValue, PlistReadError, ReadOpts};
use crate::discover::{Candidate, RefusalReason};
use crate::error::{BackupError, IoOp, MalformedDetail, UnsupportedReason};
use crate::guard::{is_link, open_source_file, probe, OpenFailure, Probe};
use crate::id::BackupId;
use crate::limits::Limits;
use crate::plists::{
    root_is_dict, BackupFile, InfoPlist, ManifestPlist, NoteKind, ParseNote, SnapshotState,
    Sourced, StatusPlist, INFO_SKIPPED_KEYS, MANIFEST_SKIPPED_KEYS,
};
use crate::text::DisplayText;

/// The SQLite file header.
pub(crate) const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";
/// The format 2.4 manifest signature.
const MBDB_SIGNATURE: &[u8; 6] = b"mbdb\x05\x00";

/// Which marker files exist, and what their headers look like.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Layout {
    /// `Info.plist` is a regular file.
    pub has_info_plist: bool,
    /// `Status.plist` is a regular file.
    pub has_status_plist: bool,
    /// `Manifest.plist` is a regular file.
    pub has_manifest_plist: bool,
    /// `Manifest.db` is a regular file.
    pub has_manifest_db: bool,
    /// `Manifest.db-wal` is a regular file.
    pub has_manifest_db_wal: bool,
    /// `Manifest.db-shm` is a regular file (never read or copied).
    pub has_manifest_db_shm: bool,
    /// `Manifest.mbdb` is a regular file (format 2.4).
    pub has_manifest_mbdb: bool,
    /// `Manifest.mbdb` starts with `mbdb\x05\x00`, when it was read.
    pub mbdb_signature_ok: Option<bool>,
    /// A `Snapshot/` directory exists.
    pub has_snapshot_dir: bool,
    /// `Manifest.db` starts with the SQLite header, when it was read.
    pub manifest_db_has_sqlite_header: Option<bool>,
    /// Markers that exist but are links, FIFOs, devices or directories
    /// (never opened).
    pub non_regular_markers: u32,
}

/// What the backup is, by the first matching rule of §5.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Classification {
    /// Unencrypted and ready to check.
    Ready,
    /// Encrypted; a password is needed.
    NeedsPassword,
    /// The backup did not finish.
    Incomplete {
        /// The closed SnapshotState (raw text is in [`StatusPlist`]).
        snapshot: SnapshotState,
        /// A `Snapshot/` directory is present.
        snapshot_dir: bool,
    },
    /// Recognised but not readable by this version.
    Unsupported(UnsupportedReason),
    /// A file needed to classify the backup is missing or damaged.
    Unreadable {
        /// Which file.
        file: BackupFile,
        /// What is wrong with it.
        detail: MalformedDetail,
    },
}

/// Everything learned about a backup without opening SQLite.
#[derive(Clone)]
pub struct Inspection {
    /// The folder.
    pub root: Candidate,
    /// Its stable id.
    pub id: BackupId,
    /// Marker files.
    pub layout: Layout,
    /// `None` when Info.plist is missing, unreadable or over budget (never fatal).
    pub info: Option<InfoPlist>,
    /// `None` when Status.plist is missing or unreadable.
    pub status: Option<StatusPlist>,
    /// `None` when Manifest.plist is missing or unreadable.
    pub manifest: Option<ManifestPlist>,
    /// Non-fatal observations.
    pub notes: Vec<ParseNote>,
    /// The result of the §5 rules.
    pub classification: Classification,
}

/// Redacting: no paths, no device text.
impl fmt::Debug for Inspection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Inspection")
            .field("id", &self.id)
            .field("layout", &self.layout)
            .field("info", &self.info)
            .field("status", &self.status)
            .field("manifest", &self.manifest)
            .field("notes", &self.notes)
            .field("classification", &self.classification)
            .finish_non_exhaustive()
    }
}

impl Inspection {
    /// The iOS version: Info.plist first, then Manifest.plist's Lockdown.
    pub fn ios_version(&self) -> Option<Sourced<DisplayText>> {
        sourced(
            self.info.as_ref().and_then(|i| i.product_version.clone()),
            self.manifest
                .as_ref()
                .and_then(|m| m.lockdown_product_version.clone()),
        )
    }

    /// The device name: Info.plist first, then Manifest.plist's Lockdown.
    pub fn device_name(&self) -> Option<Sourced<DisplayText>> {
        sourced(
            self.info.as_ref().and_then(|i| i.device_name.clone()),
            self.manifest
                .as_ref()
                .and_then(|m| m.lockdown_device_name.clone()),
        )
    }

    /// The product type: Info.plist first, then Manifest.plist's Lockdown.
    pub fn product_type(&self) -> Option<Sourced<DisplayText>> {
        sourced(
            self.info.as_ref().and_then(|i| i.product_type.clone()),
            self.manifest
                .as_ref()
                .and_then(|m| m.lockdown_product_type.clone()),
        )
    }

    /// `Ready` or `NeedsPassword`.
    pub fn is_importable(&self) -> bool {
        matches!(
            self.classification,
            Classification::Ready | Classification::NeedsPassword
        )
    }

    /// `IsEncrypted`, when it is a boolean.
    pub fn is_encrypted(&self) -> Option<bool> {
        self.manifest.as_ref().and_then(|m| m.is_encrypted)
    }

    /// The backup folder used for all file access (the containment path).
    pub(crate) fn dir(&self) -> &Path {
        &self.root.containment
    }
}

fn sourced(
    info: Option<DisplayText>,
    lockdown: Option<DisplayText>,
) -> Option<Sourced<DisplayText>> {
    match (info, lockdown) {
        (Some(value), _) => Some(Sourced {
            value,
            source: BackupFile::InfoPlist,
        }),
        (None, Some(value)) => Some(Sourced {
            value,
            source: BackupFile::ManifestPlist,
        }),
        (None, None) => None,
    }
}

fn detail_of(e: &PlistReadError) -> MalformedDetail {
    match e {
        PlistReadError::Missing => MalformedDetail::Missing,
        PlistReadError::NotRegular => MalformedDetail::NotRegularFile,
        PlistReadError::TooLarge { .. } => MalformedDetail::TooLarge,
        PlistReadError::Io(e) => MalformedDetail::ReadError(e.kind()),
        PlistReadError::Syntax => MalformedDetail::PlistSyntax,
        PlistReadError::Limit { .. } => MalformedDetail::BudgetExceeded,
    }
}

/// Read up to `N` bytes from the start of a regular file. `Ok(None)` when it
/// is missing or not regular; `Ok(Some(false))` when shorter than `N` or
/// different.
fn header_matches<const N: usize>(path: &Path, want: &[u8; N]) -> io::Result<Option<bool>> {
    match open_source_file(path) {
        Ok((file, _)) => {
            let mut buf = [0u8; N];
            let mut taken = file.take(u64::try_from(N).unwrap_or(u64::MAX));
            let mut filled = 0usize;
            while let Some(slot) = buf.get_mut(filled..) {
                if slot.is_empty() {
                    break;
                }
                match taken.read(slot)? {
                    0 => break,
                    n => filled = filled.saturating_add(n),
                }
            }
            Ok(Some(filled == N && &buf == want))
        }
        Err(OpenFailure::Missing | OpenFailure::NotRegular) => Ok(None),
        Err(OpenFailure::Io(e)) => Err(e),
    }
}

struct Marker {
    probe: Result<Probe, io::ErrorKind>,
}

impl Marker {
    fn at(dir: &Path, name: &str) -> Marker {
        Marker {
            probe: probe(&dir.join(name)).map_err(|e| e.kind()),
        }
    }
    fn regular(&self) -> bool {
        matches!(self.probe, Ok(p) if p.is_regular())
    }
    fn non_regular(&self) -> bool {
        matches!(self.probe, Ok(Probe::NotRegular))
    }
    /// "Missing" in the §5 rules includes "not a regular file".
    fn missing_detail(&self) -> Option<MalformedDetail> {
        match self.probe {
            Ok(Probe::Regular { .. }) => None,
            Ok(Probe::Missing) => Some(MalformedDetail::Missing),
            Ok(Probe::NotRegular) => Some(MalformedDetail::NotRegularFile),
            Err(kind) => Some(MalformedDetail::ReadError(kind)),
        }
    }
}

/// Inspect one backup folder. `bytes_read` is increased by the plist bytes
/// read, so the caller can enforce a per-refresh budget.
pub fn inspect(
    dir: &Candidate,
    limits: &Limits,
    bytes_read: &mut u64,
) -> Result<Inspection, BackupError> {
    let id = BackupId::for_path(&dir.display_canonical)
        .ok_or(BackupError::NotABackup(RefusalReason::NonUnicodePath))?;
    let root: PathBuf = dir.containment.clone();
    let meta = fs::metadata(&root).map_err(|e| BackupError::source_io(IoOp::Metadata, &e))?;
    if !meta.is_dir() {
        return Err(BackupError::NotABackup(RefusalReason::NotADirectory));
    }

    let info_m = Marker::at(&root, "Info.plist");
    let status_m = Marker::at(&root, "Status.plist");
    let manifest_m = Marker::at(&root, "Manifest.plist");
    let db_m = Marker::at(&root, "Manifest.db");
    let wal_m = Marker::at(&root, "Manifest.db-wal");
    let shm_m = Marker::at(&root, "Manifest.db-shm");
    let mbdb_m = Marker::at(&root, "Manifest.mbdb");
    let non_regular = [
        &info_m,
        &status_m,
        &manifest_m,
        &db_m,
        &wal_m,
        &shm_m,
        &mbdb_m,
    ]
    .iter()
    .filter(|m| m.non_regular())
    .count();

    let snapshot = root.join("Snapshot");
    let has_snapshot_dir =
        fs::symlink_metadata(&snapshot).is_ok_and(|m| m.is_dir() && !is_link(&m));

    let mut db_header_error = None;
    let manifest_db_has_sqlite_header = if db_m.regular() {
        match header_matches(&root.join("Manifest.db"), SQLITE_HEADER) {
            Ok(v) => v,
            Err(e) => {
                db_header_error = Some(e.kind());
                None
            }
        }
    } else {
        None
    };
    let mbdb_signature_ok = if mbdb_m.regular() {
        header_matches(&root.join("Manifest.mbdb"), MBDB_SIGNATURE).unwrap_or(None)
    } else {
        None
    };

    let layout = Layout {
        has_info_plist: info_m.regular(),
        has_status_plist: status_m.regular(),
        has_manifest_plist: manifest_m.regular(),
        has_manifest_db: db_m.regular(),
        has_manifest_db_wal: wal_m.regular(),
        has_manifest_db_shm: shm_m.regular(),
        has_manifest_mbdb: mbdb_m.regular(),
        mbdb_signature_ok,
        has_snapshot_dir,
        manifest_db_has_sqlite_header,
        non_regular_markers: u32::try_from(non_regular).unwrap_or(u32::MAX),
    };

    let mut notes = Vec::new();
    let depth = limits.plist_depth;

    // Manifest.plist.
    let manifest_result = if manifest_m.regular() {
        read_dict(
            &root.join("Manifest.plist"),
            limits.manifest_plist_bytes,
            ReadOpts {
                skip: MANIFEST_SKIPPED_KEYS,
                ..ReadOpts::new(depth)
            },
            bytes_read,
        )
    } else {
        Err(manifest_m
            .missing_detail()
            .unwrap_or(MalformedDetail::Missing))
    };
    let manifest = manifest_result
        .as_ref()
        .ok()
        .map(|v| ManifestPlist::from_value(v, &mut notes));

    // Status.plist.
    let status_result = if status_m.regular() {
        read_dict(
            &root.join("Status.plist"),
            limits.status_plist_bytes,
            ReadOpts::new(depth),
            bytes_read,
        )
    } else {
        Err(status_m
            .missing_detail()
            .unwrap_or(MalformedDetail::Missing))
    };
    let status = status_result
        .as_ref()
        .ok()
        .map(|v| StatusPlist::from_value(v, &mut notes));

    // Info.plist: never fatal.
    let info = if info_m.regular() {
        match read_dict(
            &root.join("Info.plist"),
            limits.info_plist_bytes,
            ReadOpts {
                skip: INFO_SKIPPED_KEYS,
                ..ReadOpts::new(depth)
            },
            bytes_read,
        ) {
            Ok(v) => Some(InfoPlist::from_value(&v, &mut notes)),
            Err(detail) => {
                let kind = match detail {
                    MalformedDetail::TooLarge | MalformedDetail::BudgetExceeded => {
                        NoteKind::BudgetExceeded
                    }
                    other => NoteKind::Unreadable(other),
                };
                notes.push(ParseNote {
                    file: BackupFile::InfoPlist,
                    key: "",
                    kind,
                });
                None
            }
        }
    } else {
        if let Some(MalformedDetail::NotRegularFile | MalformedDetail::ReadError(_)) =
            info_m.missing_detail()
        {
            notes.push(ParseNote {
                file: BackupFile::InfoPlist,
                key: "",
                kind: NoteKind::Unreadable(
                    info_m.missing_detail().unwrap_or(MalformedDetail::Missing),
                ),
            });
        }
        None
    };

    if has_snapshot_dir {
        notes.push(ParseNote {
            file: BackupFile::StatusPlist,
            key: "",
            kind: NoteKind::SnapshotDirPresent,
        });
    }
    if let Some(m) = &manifest {
        if m.is_encrypted == Some(false) && !m.has_backup_key_bag {
            notes.push(ParseNote {
                file: BackupFile::ManifestPlist,
                key: "BackupKeyBag",
                kind: NoteKind::NoKeybag,
            });
        }
    }
    if let Some(provider) = oma_fs::cloud_sync_provider(&dir.display_canonical) {
        notes.push(ParseNote {
            file: BackupFile::ManifestPlist,
            key: "",
            kind: NoteKind::InSyncFolder(provider),
        });
    }

    // The §5 rules, in order; the first match wins.
    let classification = (|| {
        // 1. Format 2.4.
        if mbdb_m.regular() && !db_m.regular() {
            return Classification::Unsupported(UnsupportedReason::LegacyMbdb);
        }
        // 2. Manifest.plist.
        let manifest = match (&manifest_result, &manifest) {
            (Ok(_), Some(m)) => m,
            (Err(detail), _) => {
                return Classification::Unreadable {
                    file: BackupFile::ManifestPlist,
                    detail: *detail,
                }
            }
            (Ok(_), None) => {
                return Classification::Unreadable {
                    file: BackupFile::ManifestPlist,
                    detail: MalformedDetail::Missing,
                }
            }
        };
        // 3 and 4. Status.plist.
        let status = match (&status_result, &status) {
            (Ok(_), Some(s)) => s,
            (Err(MalformedDetail::Missing), _) => {
                return Classification::Incomplete {
                    snapshot: SnapshotState::Absent,
                    snapshot_dir: has_snapshot_dir,
                }
            }
            (Err(detail), _) => {
                return Classification::Unreadable {
                    file: BackupFile::StatusPlist,
                    detail: *detail,
                }
            }
            (Ok(_), None) => {
                return Classification::Unreadable {
                    file: BackupFile::StatusPlist,
                    detail: MalformedDetail::Missing,
                }
            }
        };
        // 5. Not finished.
        if status.snapshot_state != SnapshotState::Finished {
            return Classification::Incomplete {
                snapshot: status.snapshot_state,
                snapshot_dir: has_snapshot_dir,
            };
        }
        // 6. Encryption unknown.
        let Some(encrypted) = manifest.is_encrypted else {
            return Classification::Unsupported(UnsupportedReason::EncryptionUnknown);
        };
        // 7. Manifest.db.
        if let Some(detail) = db_m.missing_detail() {
            return Classification::Unreadable {
                file: BackupFile::ManifestDb,
                detail,
            };
        }
        if let Some(kind) = db_header_error {
            return Classification::Unreadable {
                file: BackupFile::ManifestDb,
                detail: MalformedDetail::ReadError(kind),
            };
        }
        if encrypted {
            // 8 and 9.
            if !manifest.has_backup_key_bag {
                return Classification::Unsupported(UnsupportedReason::NoKeybag);
            }
            if !manifest.has_manifest_key {
                return Classification::Unsupported(UnsupportedReason::EncryptedWithoutManifestKey);
            }
            // 11.
            if manifest_db_has_sqlite_header == Some(true) {
                return Classification::Unreadable {
                    file: BackupFile::ManifestDb,
                    detail: MalformedDetail::Inconsistent("encrypted but plaintext"),
                };
            }
            // 12.
            Classification::NeedsPassword
        } else {
            // 10.
            if manifest_db_has_sqlite_header != Some(true) {
                return Classification::Unreadable {
                    file: BackupFile::ManifestDb,
                    detail: MalformedDetail::Inconsistent("not encrypted but not SQLite"),
                };
            }
            // 13.
            Classification::Ready
        }
    })();

    Ok(Inspection {
        root: dir.clone(),
        id,
        layout,
        info,
        status,
        manifest,
        notes,
        classification,
    })
}

/// Read a plist whose root must be a dictionary.
fn read_dict(
    path: &Path,
    cap: u64,
    opts: ReadOpts<'_>,
    bytes_read: &mut u64,
) -> Result<PValue, MalformedDetail> {
    match read_bounded(path, cap, opts) {
        Ok((value, n)) => {
            *bytes_read = bytes_read.saturating_add(n);
            if root_is_dict(&value) {
                Ok(value)
            } else {
                Err(MalformedDetail::UnexpectedType("root"))
            }
        }
        Err(e) => {
            if let PlistReadError::TooLarge { .. } = e {
                // Nothing was parsed, but the cap was read.
                *bytes_read = bytes_read.saturating_add(cap);
            }
            Err(detail_of(&e))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plist::Value;

    struct Built {
        _tmp: tempfile::TempDir,
        candidate: Candidate,
    }

    fn dict(entries: Vec<(&str, Value)>) -> Value {
        let mut d = plist::Dictionary::new();
        for (k, v) in entries {
            d.insert(k.into(), v);
        }
        Value::Dictionary(d)
    }

    fn write_plist(path: &Path, v: &Value) {
        v.to_file_binary(path).expect("plist");
    }

    /// A backup folder; `edit` adjusts it before inspection.
    fn build(encrypted: bool, edit: impl FnOnce(&Path)) -> Built {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("SYNTHETIC0-0000000000000000");
        fs::create_dir(&dir).expect("mkdir");
        write_plist(
            &dir.join("Info.plist"),
            &dict(vec![
                ("Device Name", Value::String("Synthetic Test Phone".into())),
                ("Product Version", Value::String("18.0".into())),
            ]),
        );
        write_plist(
            &dir.join("Status.plist"),
            &dict(vec![("SnapshotState", Value::String("finished".into()))]),
        );
        let mut m = vec![
            ("IsEncrypted", Value::Boolean(encrypted)),
            ("BackupKeyBag", Value::Data(vec![1; 8])),
            (
                "Lockdown",
                dict(vec![("ProductVersion", Value::String("17.0".into()))]),
            ),
        ];
        if encrypted {
            m.push(("ManifestKey", Value::Data(vec![1; 44])));
        }
        write_plist(&dir.join("Manifest.plist"), &dict(m));
        if encrypted {
            fs::write(dir.join("Manifest.db"), [0x9Cu8; 4096]).expect("db");
        } else {
            let mut db = SQLITE_HEADER.to_vec();
            db.resize(4096, 0);
            fs::write(dir.join("Manifest.db"), db).expect("db");
        }
        edit(&dir);
        let candidate = Candidate::for_dir(&dir).expect("candidate");
        Built {
            _tmp: tmp,
            candidate,
        }
    }

    fn classify(b: &Built) -> Inspection {
        let mut n = 0;
        inspect(&b.candidate, &Limits::default(), &mut n).expect("inspect")
    }

    #[test]
    fn ready_and_needs_password() {
        let plain = classify(&build(false, |_| {}));
        assert_eq!(plain.classification, Classification::Ready);
        assert!(plain.is_importable());
        assert_eq!(
            plain
                .ios_version()
                .map(|s| (s.value.as_str().to_owned(), s.source)),
            Some(("18.0".to_owned(), BackupFile::InfoPlist))
        );
        let enc = classify(&build(true, |_| {}));
        assert_eq!(enc.classification, Classification::NeedsPassword);
    }

    #[test]
    fn rule_1_legacy_mbdb() {
        let b = build(false, |d| {
            fs::remove_file(d.join("Manifest.db")).expect("rm");
            fs::write(d.join("Manifest.mbdb"), b"mbdb\x05\x00rest").expect("mbdb");
        });
        let i = classify(&b);
        assert_eq!(
            i.classification,
            Classification::Unsupported(UnsupportedReason::LegacyMbdb)
        );
        assert_eq!(i.layout.mbdb_signature_ok, Some(true));
    }

    #[test]
    fn rule_2_manifest_plist() {
        let missing = classify(&build(false, |d| {
            fs::remove_file(d.join("Manifest.plist")).expect("rm")
        }));
        assert_eq!(
            missing.classification,
            Classification::Unreadable {
                file: BackupFile::ManifestPlist,
                detail: MalformedDetail::Missing
            }
        );
        let garbage = classify(&build(false, |d| {
            fs::write(d.join("Manifest.plist"), b"not a plist at all").expect("w")
        }));
        assert_eq!(
            garbage.classification,
            Classification::Unreadable {
                file: BackupFile::ManifestPlist,
                detail: MalformedDetail::PlistSyntax
            }
        );
        let b = build(false, |_| {});
        let small = Limits {
            manifest_plist_bytes: 8,
            ..Limits::default()
        };
        let mut n = 0;
        let over = inspect(&b.candidate, &small, &mut n).expect("inspect");
        assert_eq!(
            over.classification,
            Classification::Unreadable {
                file: BackupFile::ManifestPlist,
                detail: MalformedDetail::TooLarge
            }
        );
    }

    #[test]
    fn rules_3_to_5_status() {
        let missing = classify(&build(false, |d| {
            fs::remove_file(d.join("Status.plist")).expect("rm")
        }));
        assert_eq!(
            missing.classification,
            Classification::Incomplete {
                snapshot: SnapshotState::Absent,
                snapshot_dir: false
            }
        );
        let bad = classify(&build(false, |d| {
            fs::write(d.join("Status.plist"), b"\x00\x01").expect("w")
        }));
        assert!(matches!(
            bad.classification,
            Classification::Unreadable {
                file: BackupFile::StatusPlist,
                ..
            }
        ));
        let unfinished = classify(&build(false, |d| {
            write_plist(
                &d.join("Status.plist"),
                &dict(vec![("SnapshotState", Value::String("uploading".into()))]),
            );
            fs::create_dir(d.join("Snapshot")).expect("mkdir");
        }));
        assert_eq!(
            unfinished.classification,
            Classification::Incomplete {
                snapshot: SnapshotState::Other,
                snapshot_dir: true
            }
        );
        assert!(unfinished
            .notes
            .iter()
            .any(|n| n.kind == NoteKind::SnapshotDirPresent));
    }

    /// The shape of a Status.plist that iMazing 3.x left after an interrupted
    /// backup, as observed on a Windows 11 machine (iOS 26.6.2): binary, with
    /// a byte-identical `Status.plist.backup` beside it. The UUID and date
    /// here are synthetic; the observed ones are not recorded anywhere.
    #[test]
    fn an_interrupted_imazing_backup_is_incomplete() {
        let b = build(false, |d| {
            let date = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
            write_plist(
                &d.join("Status.plist"),
                &dict(vec![
                    ("IsFullBackup", Value::Boolean(true)),
                    ("Version", Value::String("3.3".into())),
                    (
                        "UUID",
                        Value::String("00000000-0000-4000-8000-000000000000".into()),
                    ),
                    ("Date", Value::Date(date.into())),
                    ("BackupState", Value::String("empty".into())),
                    ("SnapshotState", Value::String("uploading".into())),
                ]),
            );
            fs::copy(d.join("Status.plist"), d.join("Status.plist.backup")).expect("copy");
        });
        let dir = &b.candidate.display_canonical;
        let bytes = fs::read(dir.join("Status.plist")).expect("read");
        assert!(bytes.starts_with(b"bplist00"), "written as a binary plist");
        assert_eq!(
            fs::read(dir.join("Status.plist.backup")).expect("read"),
            bytes
        );

        let i = classify(&b);
        assert_eq!(
            i.classification,
            Classification::Incomplete {
                snapshot: SnapshotState::Other,
                snapshot_dir: false
            }
        );
        assert!(!i.is_importable());
        let status = i.status.as_ref().expect("status");
        assert_eq!(
            status.snapshot_state_raw.as_ref().map(DisplayText::as_str),
            Some("uploading")
        );
        // Carried as raw text only; nothing interprets "empty".
        assert_eq!(
            status.backup_state.as_ref().map(DisplayText::as_str),
            Some("empty")
        );
        assert_eq!(status.is_full_backup, Some(true));
        assert_eq!(
            status.version.as_ref().map(DisplayText::as_str),
            Some("3.3")
        );
        assert!(status.date.is_some());
        // Every key had the expected type, and the extra file is ignored.
        assert!(
            !i.notes.iter().any(|n| n.file == BackupFile::StatusPlist),
            "{:?}",
            i.notes
        );
        assert!(i.info.is_some() && i.manifest.is_some());
    }

    #[test]
    fn rules_6_to_13() {
        let unknown = classify(&build(false, |d| {
            write_plist(
                &d.join("Manifest.plist"),
                &dict(vec![("IsEncrypted", Value::String("true".into()))]),
            )
        }));
        assert_eq!(
            unknown.classification,
            Classification::Unsupported(UnsupportedReason::EncryptionUnknown)
        );
        let no_db = classify(&build(false, |d| {
            fs::remove_file(d.join("Manifest.db")).expect("rm")
        }));
        assert_eq!(
            no_db.classification,
            Classification::Unreadable {
                file: BackupFile::ManifestDb,
                detail: MalformedDetail::Missing
            }
        );
        let no_keybag = classify(&build(true, |d| {
            write_plist(
                &d.join("Manifest.plist"),
                &dict(vec![
                    ("IsEncrypted", Value::Boolean(true)),
                    ("ManifestKey", Value::Data(vec![1; 44])),
                ]),
            )
        }));
        assert_eq!(
            no_keybag.classification,
            Classification::Unsupported(UnsupportedReason::NoKeybag)
        );
        let no_mkey = classify(&build(true, |d| {
            write_plist(
                &d.join("Manifest.plist"),
                &dict(vec![
                    ("IsEncrypted", Value::Boolean(true)),
                    ("BackupKeyBag", Value::Data(vec![1; 8])),
                ]),
            )
        }));
        assert_eq!(
            no_mkey.classification,
            Classification::Unsupported(UnsupportedReason::EncryptedWithoutManifestKey)
        );
        let not_sqlite = classify(&build(false, |d| {
            fs::write(d.join("Manifest.db"), [7u8; 100]).expect("w")
        }));
        assert_eq!(
            not_sqlite.classification,
            Classification::Unreadable {
                file: BackupFile::ManifestDb,
                detail: MalformedDetail::Inconsistent("not encrypted but not SQLite")
            }
        );
        let plaintext = classify(&build(true, |d| {
            let mut db = SQLITE_HEADER.to_vec();
            db.resize(4096, 0);
            fs::write(d.join("Manifest.db"), db).expect("w")
        }));
        assert_eq!(
            plaintext.classification,
            Classification::Unreadable {
                file: BackupFile::ManifestDb,
                detail: MalformedDetail::Inconsistent("encrypted but plaintext")
            }
        );
        let no_keybag_plain = classify(&build(false, |d| {
            write_plist(
                &d.join("Manifest.plist"),
                &dict(vec![("IsEncrypted", Value::Boolean(false))]),
            )
        }));
        assert_eq!(no_keybag_plain.classification, Classification::Ready);
        assert!(no_keybag_plain
            .notes
            .iter()
            .any(|n| n.kind == NoteKind::NoKeybag));
    }

    #[test]
    fn info_plist_failures_are_never_fatal() {
        let b = build(false, |d| {
            fs::write(d.join("Info.plist"), b"garbage").expect("w");
        });
        let i = classify(&b);
        assert_eq!(i.classification, Classification::Ready);
        assert!(i.info.is_none());
        // Falls back to Manifest.plist's Lockdown, and says so.
        assert_eq!(
            i.ios_version()
                .map(|s| (s.value.as_str().to_owned(), s.source)),
            Some(("17.0".to_owned(), BackupFile::ManifestPlist))
        );
        let small = Limits {
            info_plist_bytes: 4,
            ..Limits::default()
        };
        let b = build(false, |_| {});
        let mut n = 0;
        let i = inspect(&b.candidate, &small, &mut n).expect("inspect");
        assert_eq!(i.classification, Classification::Ready);
        assert!(i.notes.iter().any(|n| n.kind == NoteKind::BudgetExceeded));
    }

    #[test]
    fn forbidden_keys_never_reach_debug_output() {
        let b = build(true, |d| {
            write_plist(
                &d.join("Info.plist"),
                &dict(vec![
                    ("Device Name", Value::String("Synthetic Test Phone".into())),
                    (
                        "Unique Identifier",
                        Value::String("UDID-SENTINEL-0001".into()),
                    ),
                    (
                        "Target Identifier",
                        Value::String("UDID-SENTINEL-0001".into()),
                    ),
                    ("Serial Number", Value::String("SERIAL-SENTINEL".into())),
                    (
                        "Phone Number",
                        Value::String("+1 555 0100 PHONE-SENTINEL".into()),
                    ),
                    ("IMEI", Value::String("IMEI-SENTINEL".into())),
                    ("ICCID", Value::String("ICCID-SENTINEL".into())),
                    ("MEID", Value::String("MEID-SENTINEL".into())),
                    ("GUID", Value::String("GUID-SENTINEL".into())),
                ]),
            );
            write_plist(
                &d.join("Manifest.plist"),
                &dict(vec![
                    ("IsEncrypted", Value::Boolean(true)),
                    ("BackupKeyBag", Value::Data(b"KEYBAG-SENTINEL".to_vec())),
                    ("ManifestKey", Value::Data(vec![0x41; 44])),
                ]),
            );
        });
        let i = classify(&b);
        let shown = format!("{i:?} {:?} {:?}", i.info, i.manifest);
        for sentinel in [
            "SENTINEL",
            "Synthetic",
            "555",
            "KEYBAG",
            "AAAA",
            "SYNTHETIC0",
        ] {
            assert!(!shown.contains(sentinel), "{sentinel} in {shown}");
        }
        assert!(i.info.is_some());
    }

    #[cfg(unix)]
    #[test]
    fn fifo_and_symlink_markers_are_not_regular_and_never_opened() {
        let fifo = build(false, |d| {
            fs::remove_file(d.join("Manifest.db")).expect("rm");
            let ok = std::process::Command::new("mkfifo")
                .arg(d.join("Manifest.db"))
                .status()
                .expect("mkfifo")
                .success();
            assert!(ok);
        });
        // Would block forever if the FIFO were opened.
        let i = classify(&fifo);
        assert_eq!(
            i.classification,
            Classification::Unreadable {
                file: BackupFile::ManifestDb,
                detail: MalformedDetail::NotRegularFile
            }
        );
        assert_eq!(i.layout.non_regular_markers, 1);

        let link = build(false, |d| {
            let target = d.join("real.db");
            fs::rename(d.join("Manifest.db"), &target).expect("mv");
            std::os::unix::fs::symlink(&target, d.join("Manifest.db")).expect("symlink");
        });
        assert_eq!(
            classify(&link).classification,
            Classification::Unreadable {
                file: BackupFile::ManifestDb,
                detail: MalformedDetail::NotRegularFile
            }
        );
    }

    #[test]
    fn plist_bytes_are_counted() {
        let b = build(false, |_| {});
        let mut n = 0;
        inspect(&b.candidate, &Limits::default(), &mut n).expect("inspect");
        assert!(n > 50);
    }
}
