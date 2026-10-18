//! The three top-level plists: typed, optional keys only.
//!
//! A missing key is `None`. A key of the wrong type is `None` plus a
//! [`ParseNote`]; values are never coerced (the string `"true"` is not a
//! boolean). Strings become [`DisplayText`] before they reach any struct the
//! UI sees.
//!
//! Identifying fields are never read (D14): `Unique Identifier`, `Target
//! Identifier`, `Serial Number`, `Phone Number`, `IMEI`, `ICCID`, `MEID` and
//! `GUID` have no fields here and are skipped while streaming, together with
//! the large application subtrees.

use std::fmt;

use crate::bplist::PValue;
use crate::keybag::KeybagNote;
use crate::text::DisplayText;

/// A file in a backup, named in errors, notes and provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackupFile {
    /// `Info.plist`.
    InfoPlist,
    /// `Status.plist`.
    StatusPlist,
    /// `Manifest.plist`.
    ManifestPlist,
    /// `Manifest.db`.
    ManifestDb,
    /// `Manifest.db-wal`.
    ManifestDbWal,
    /// `Manifest.mbdb` (format 2.4).
    ManifestMbdb,
    /// The `BackupKeyBag` in `Manifest.plist`.
    Keybag,
    /// The `ManifestKey` in `Manifest.plist`.
    ManifestKey,
    /// A file blob (`xx/<fileID>`) or its MBFile record.
    Blob,
}

/// A value and the file it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sourced<T> {
    /// The value.
    pub value: T,
    /// The file it was read from.
    pub source: BackupFile,
}

/// `Info.plist` keys that are consumed but never kept: identifiers (D14) and
/// the application subtrees, which can be large.
pub(crate) const INFO_SKIPPED_KEYS: &[&str] = &[
    "Applications",
    "Installed Applications",
    "iTunes Files",
    "iTunes Settings",
    "Unique Identifier",
    "Target Identifier",
    "Serial Number",
    "Phone Number",
    "IMEI",
    "IMEI 2",
    "ICCID",
    "MEID",
    "GUID",
];

/// `Manifest.plist` keys that are consumed but never kept.
pub(crate) const MANIFEST_SKIPPED_KEYS: &[&str] = &["Applications"];

/// Device facts from `Info.plist`. No identifier fields exist (D14).
#[derive(Clone, Default)]
pub struct InfoPlist {
    /// `Device Name`.
    pub device_name: Option<DisplayText>,
    /// `Display Name`.
    pub display_name: Option<DisplayText>,
    /// `Product Type`, such as `iPhone0,0`.
    pub product_type: Option<DisplayText>,
    /// `Product Name`.
    pub product_name: Option<DisplayText>,
    /// `Product Version` (the iOS version).
    pub product_version: Option<DisplayText>,
    /// `Build Version`.
    pub build_version: Option<DisplayText>,
    /// `Last Backup Date`.
    pub last_backup_date: Option<plist::Date>,
}

/// Field names and presence only.
impl fmt::Debug for InfoPlist {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InfoPlist")
            .field("device_name", &self.device_name.is_some())
            .field("display_name", &self.display_name.is_some())
            .field("product_type", &self.product_type.is_some())
            .field("product_name", &self.product_name.is_some())
            .field("product_version", &self.product_version.is_some())
            .field("build_version", &self.build_version.is_some())
            .field("last_backup_date", &self.last_backup_date.is_some())
            .finish()
    }
}

/// `SnapshotState`, as a closed set. The raw value is kept separately, for
/// display only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SnapshotState {
    /// Exactly `"finished"`.
    Finished,
    /// The key (or `Status.plist`) is absent.
    Absent,
    /// The key is not a string.
    WrongType,
    /// Any other string.
    Other,
}

/// Facts from `Status.plist`.
#[derive(Clone)]
pub struct StatusPlist {
    /// The closed classification of `SnapshotState`.
    pub snapshot_state: SnapshotState,
    /// The sanitised raw value, for display only. Never put it in an error,
    /// a `Debug` string or a log.
    pub snapshot_state_raw: Option<DisplayText>,
    /// `IsFullBackup`. Its exact meaning is **UNVERIFIED**.
    pub is_full_backup: Option<bool>,
    /// `Version` (the backup format version, such as `3.3`).
    pub version: Option<DisplayText>,
    /// `Date`.
    pub date: Option<plist::Date>,
    /// `BackupState`.
    pub backup_state: Option<DisplayText>,
}

impl fmt::Debug for StatusPlist {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StatusPlist")
            .field("snapshot_state", &self.snapshot_state)
            .field("is_full_backup", &self.is_full_backup)
            .field("version", &self.version.is_some())
            .field("date", &self.date.is_some())
            .field("backup_state", &self.backup_state.is_some())
            .finish()
    }
}

/// Facts from `Manifest.plist`.
#[derive(Clone, Default)]
pub struct ManifestPlist {
    /// `IsEncrypted`, when present and a boolean.
    pub is_encrypted: Option<bool>,
    pub(crate) backup_key_bag: Option<Vec<u8>>,
    pub(crate) manifest_key: Option<Vec<u8>>,
    /// `BackupKeyBag` is present (as data).
    pub has_backup_key_bag: bool,
    /// `ManifestKey` is present (as data).
    pub has_manifest_key: bool,
    /// `Lockdown.ProductVersion`.
    pub lockdown_product_version: Option<DisplayText>,
    /// `Lockdown.ProductType` (key name **UNVERIFIED**).
    pub lockdown_product_type: Option<DisplayText>,
    /// `Lockdown.BuildVersion` (key name **UNVERIFIED**).
    pub lockdown_build_version: Option<DisplayText>,
    /// `Lockdown.DeviceName` (key name **UNVERIFIED**).
    pub lockdown_device_name: Option<DisplayText>,
}

/// Key bytes print as `<n bytes>`.
impl fmt::Debug for ManifestPlist {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let len = |v: &Option<Vec<u8>>| v.as_ref().map(|b| format!("<{} bytes>", b.len()));
        f.debug_struct("ManifestPlist")
            .field("is_encrypted", &self.is_encrypted)
            .field("backup_key_bag", &len(&self.backup_key_bag))
            .field("manifest_key", &len(&self.manifest_key))
            .field(
                "lockdown_product_version",
                &self.lockdown_product_version.is_some(),
            )
            .field(
                "lockdown_product_type",
                &self.lockdown_product_type.is_some(),
            )
            .field(
                "lockdown_build_version",
                &self.lockdown_build_version.is_some(),
            )
            .field("lockdown_device_name", &self.lockdown_device_name.is_some())
            .finish()
    }
}

/// Something noticed while inspecting that is not fatal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseNote {
    /// The file it concerns.
    pub file: BackupFile,
    /// The plist key it concerns, or `""`.
    pub key: &'static str,
    /// What was noticed.
    pub kind: NoteKind,
}

/// The kinds of [`ParseNote`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteKind {
    /// A key has an unexpected type; its value is ignored.
    UnexpectedType,
    /// The file ended early.
    Truncated,
    /// The file exceeded its size cap or a processing budget.
    BudgetExceeded,
    /// A `Snapshot/` directory is present (meaning **UNVERIFIED**).
    SnapshotDirPresent,
    /// A `Manifest.db-wal` was present and not applied.
    WalUnapplied,
    /// A keybag note.
    KeybagNote(KeybagNote),
    /// The backup sits inside the archive library (outside staging).
    InsideArchiveLibrary,
    /// The backup sits inside a folder this sync client manages.
    InSyncFolder(&'static str),
    /// The file could not be read; the detail says why. Used for
    /// `Info.plist`, whose failures are never fatal.
    Unreadable(crate::error::MalformedDetail),
    /// An unencrypted backup without a `BackupKeyBag` (whether unencrypted
    /// backups always carry one is **UNVERIFIED**).
    NoKeybag,
    /// The archive library sits inside this backup.
    ArchiveLibraryInsideBackup,
}

/// Read a string key, sanitised.
fn text(
    dict: &PValue,
    key: &'static str,
    file: BackupFile,
    notes: &mut Vec<ParseNote>,
) -> Option<DisplayText> {
    match dict.get(key)? {
        PValue::String(s) => Some(DisplayText::from_untrusted(s)),
        _ => {
            notes.push(ParseNote {
                file,
                key,
                kind: NoteKind::UnexpectedType,
            });
            None
        }
    }
}

fn boolean(
    dict: &PValue,
    key: &'static str,
    file: BackupFile,
    notes: &mut Vec<ParseNote>,
) -> Option<bool> {
    match dict.get(key)? {
        PValue::Bool(b) => Some(*b),
        _ => {
            notes.push(ParseNote {
                file,
                key,
                kind: NoteKind::UnexpectedType,
            });
            None
        }
    }
}

fn date(
    dict: &PValue,
    key: &'static str,
    file: BackupFile,
    notes: &mut Vec<ParseNote>,
) -> Option<plist::Date> {
    match dict.get(key)? {
        PValue::Date(d) => Some(*d),
        _ => {
            notes.push(ParseNote {
                file,
                key,
                kind: NoteKind::UnexpectedType,
            });
            None
        }
    }
}

fn data(
    dict: &PValue,
    key: &'static str,
    file: BackupFile,
    notes: &mut Vec<ParseNote>,
) -> Option<Vec<u8>> {
    match dict.get(key)? {
        PValue::Data(d) => Some(d.clone()),
        _ => {
            notes.push(ParseNote {
                file,
                key,
                kind: NoteKind::UnexpectedType,
            });
            None
        }
    }
}

/// The root must be a dictionary.
pub(crate) fn root_is_dict(v: &PValue) -> bool {
    matches!(v, PValue::Dict(_))
}

impl InfoPlist {
    pub(crate) fn from_value(v: &PValue, notes: &mut Vec<ParseNote>) -> Self {
        let f = BackupFile::InfoPlist;
        InfoPlist {
            device_name: text(v, "Device Name", f, notes),
            display_name: text(v, "Display Name", f, notes),
            product_type: text(v, "Product Type", f, notes),
            product_name: text(v, "Product Name", f, notes),
            product_version: text(v, "Product Version", f, notes),
            build_version: text(v, "Build Version", f, notes),
            last_backup_date: date(v, "Last Backup Date", f, notes),
        }
    }
}

impl StatusPlist {
    pub(crate) fn from_value(v: &PValue, notes: &mut Vec<ParseNote>) -> Self {
        let f = BackupFile::StatusPlist;
        let (snapshot_state, snapshot_state_raw) = match v.get("SnapshotState") {
            None => (SnapshotState::Absent, None),
            Some(PValue::String(s)) => {
                let state = if s == "finished" {
                    SnapshotState::Finished
                } else {
                    SnapshotState::Other
                };
                (state, Some(DisplayText::from_untrusted(s)))
            }
            Some(_) => {
                notes.push(ParseNote {
                    file: f,
                    key: "SnapshotState",
                    kind: NoteKind::UnexpectedType,
                });
                (SnapshotState::WrongType, None)
            }
        };
        StatusPlist {
            snapshot_state,
            snapshot_state_raw,
            is_full_backup: boolean(v, "IsFullBackup", f, notes),
            version: text(v, "Version", f, notes),
            date: date(v, "Date", f, notes),
            backup_state: text(v, "BackupState", f, notes),
        }
    }
}

impl ManifestPlist {
    pub(crate) fn from_value(v: &PValue, notes: &mut Vec<ParseNote>) -> Self {
        let f = BackupFile::ManifestPlist;
        let backup_key_bag = data(v, "BackupKeyBag", f, notes);
        let manifest_key = data(v, "ManifestKey", f, notes);
        let mut m = ManifestPlist {
            is_encrypted: boolean(v, "IsEncrypted", f, notes),
            has_backup_key_bag: backup_key_bag.is_some(),
            has_manifest_key: manifest_key.is_some(),
            backup_key_bag,
            manifest_key,
            ..ManifestPlist::default()
        };
        match v.get("Lockdown") {
            None => {}
            Some(lockdown @ PValue::Dict(_)) => {
                m.lockdown_product_version = text(lockdown, "ProductVersion", f, notes);
                m.lockdown_product_type = text(lockdown, "ProductType", f, notes);
                m.lockdown_build_version = text(lockdown, "BuildVersion", f, notes);
                m.lockdown_device_name = text(lockdown, "DeviceName", f, notes);
            }
            Some(_) => notes.push(ParseNote {
                file: f,
                key: "Lockdown",
                kind: NoteKind::UnexpectedType,
            }),
        }
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(entries: Vec<(&str, PValue)>) -> PValue {
        PValue::Dict(
            entries
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect(),
        )
    }

    #[test]
    fn wrong_types_become_none_with_a_note_and_are_never_coerced() {
        let v = dict(vec![
            ("IsEncrypted", PValue::String("true".into())),
            ("BackupKeyBag", PValue::String("x".into())),
        ]);
        let mut notes = Vec::new();
        let m = ManifestPlist::from_value(&v, &mut notes);
        assert_eq!(m.is_encrypted, None);
        assert!(!m.has_backup_key_bag);
        assert_eq!(notes.len(), 2);
        assert!(notes.iter().all(|n| n.kind == NoteKind::UnexpectedType));
        assert!(notes.iter().any(|n| n.key == "IsEncrypted"));
    }

    #[test]
    fn snapshot_state_is_closed() {
        let mut notes = Vec::new();
        let finished = StatusPlist::from_value(
            &dict(vec![("SnapshotState", PValue::String("finished".into()))]),
            &mut notes,
        );
        assert_eq!(finished.snapshot_state, SnapshotState::Finished);
        let other = StatusPlist::from_value(
            &dict(vec![("SnapshotState", PValue::String("Finished".into()))]),
            &mut notes,
        );
        assert_eq!(other.snapshot_state, SnapshotState::Other);
        assert_eq!(
            other.snapshot_state_raw.as_ref().map(|t| t.as_str()),
            Some("Finished")
        );
        let absent = StatusPlist::from_value(&dict(vec![]), &mut notes);
        assert_eq!(absent.snapshot_state, SnapshotState::Absent);
        let wrong = StatusPlist::from_value(
            &dict(vec![("SnapshotState", PValue::Bool(true))]),
            &mut notes,
        );
        assert_eq!(wrong.snapshot_state, SnapshotState::WrongType);
        // The raw value is never in Debug output.
        assert!(!format!("{other:?}").contains("Finished\""));
    }

    #[test]
    fn manifest_debug_hides_key_bytes_and_lockdown_text() {
        let v = dict(vec![
            ("IsEncrypted", PValue::Bool(true)),
            ("BackupKeyBag", PValue::Data(vec![0x5A; 12])),
            ("ManifestKey", PValue::Data(vec![0x5A; 44])),
            (
                "Lockdown",
                dict(vec![(
                    "DeviceName",
                    PValue::String("Synthetic Test Phone".into()),
                )]),
            ),
        ]);
        let mut notes = Vec::new();
        let m = ManifestPlist::from_value(&v, &mut notes);
        assert!(m.has_backup_key_bag && m.has_manifest_key);
        let d = format!("{m:?}");
        assert!(d.contains("<12 bytes>"));
        assert!(!d.contains("90"), "key byte values must not print: {d}");
        assert!(!d.contains("Synthetic"));
    }
}
