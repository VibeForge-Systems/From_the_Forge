//! The `backupFolders` setting: folders the user added on the Backups page.
//!
//! The field is additive to settings format 1. It is read leniently, so a
//! hand-edited or damaged value costs only the folder list, never the rest of
//! the file. Every entry is re-validated on load; anything dropped is
//! explained in a notice.

use std::collections::HashSet;
use std::ops::{Deref, DerefMut};
use std::path::Path;

use serde::{Deserialize, Deserializer, Serialize};
use ts_rs::TS;

/// At most this many folders can be added.
pub const MAX_BACKUP_FOLDERS: usize = 32;

/// What an added folder is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum AddedFolderKind {
    /// The folder is one backup.
    Backup,
    /// The folder contains backups.
    Parent,
}

/// One folder the user added.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AddedFolderEntry {
    pub path: String,
    pub kind: AddedFolderKind,
}

/// The `backupFolders` list. Serializes as a plain array. Deserializes
/// leniently: elements that are not `{ path, kind }` objects are dropped, and
/// a value that is not an array becomes empty, so a bad value can never make
/// the whole settings file count as corrupt. The notices for what was dropped
/// are produced on load (`structural_notices`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct BackupFolders(pub Vec<AddedFolderEntry>);

impl<'de> Deserialize<'de> for BackupFolders {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(d)?;
        Ok(BackupFolders(structural(&value).0))
    }
}

impl Deref for BackupFolders {
    type Target = Vec<AddedFolderEntry>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for BackupFolders {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/// The entries that parse, and how many elements did not (or `None` when the
/// value is not an array at all).
fn structural(value: &serde_json::Value) -> (Vec<AddedFolderEntry>, Option<usize>) {
    match value {
        serde_json::Value::Array(items) => {
            let mut dropped = 0usize;
            let mut kept = Vec::new();
            for item in items {
                match serde_json::from_value::<AddedFolderEntry>(item.clone()) {
                    Ok(entry) => kept.push(entry),
                    Err(_) => dropped = dropped.saturating_add(1),
                }
            }
            (kept, Some(dropped))
        }
        _ => (Vec::new(), None),
    }
}

/// The notices for the structural pass over the raw JSON value of
/// `backupFolders`, if the file had one.
pub(super) fn structural_notices(raw: Option<&serde_json::Value>) -> Vec<String> {
    let Some(value) = raw else {
        return Vec::new();
    };
    match structural(value).1 {
        None => vec![
            "The list of added backup folders in the settings file was not readable and has been reset."
                .to_owned(),
        ],
        Some(0) => Vec::new(),
        Some(n) => vec![format!(
            "{n} entr{} in the list of added backup folders could not be read and {} ignored.",
            if n == 1 { "y" } else { "ies" },
            if n == 1 { "was" } else { "were" }
        )],
    }
}

/// Validation on load: drop entries that are not absolute, duplicates,
/// entries inside `<library>/.oma-staging`, and entries beyond the cap. A
/// folder that no longer exists is kept: it may be on an unplugged drive.
pub(super) fn validate_loaded(
    entries: Vec<AddedFolderEntry>,
    archive_directory: Option<&str>,
) -> (Vec<AddedFolderEntry>, Vec<String>) {
    let staging = archive_directory.map(|lib| Path::new(lib).join(oma_backup::STAGING_DIR_NAME));
    let mut seen = HashSet::new();
    let mut kept = Vec::new();
    let (mut relative, mut duplicate, mut in_staging, mut over_cap) = (0u32, 0u32, 0u32, 0u32);
    for entry in entries {
        let path = Path::new(&entry.path);
        if !path.is_absolute() {
            relative = relative.saturating_add(1);
        } else if !seen.insert(entry.path.clone()) {
            duplicate = duplicate.saturating_add(1);
        } else if staging.as_deref().is_some_and(|s| path.starts_with(s)) {
            in_staging = in_staging.saturating_add(1);
        } else if kept.len() >= MAX_BACKUP_FOLDERS {
            over_cap = over_cap.saturating_add(1);
        } else {
            kept.push(entry);
        }
    }
    let mut notices = Vec::new();
    if relative > 0 {
        notices.push(format!(
            "{relative} added backup folder{} with a relative path {} ignored.",
            plural(relative),
            were(relative)
        ));
    }
    if duplicate > 0 {
        notices.push(format!(
            "{duplicate} duplicate added backup folder{} {} ignored.",
            plural(duplicate),
            were(duplicate)
        ));
    }
    if in_staging > 0 {
        notices.push(format!(
            "{in_staging} added backup folder{} inside OpenMessage Archive's temporary folder {} ignored.",
            plural(in_staging),
            were(in_staging)
        ));
    }
    if over_cap > 0 {
        notices.push(format!(
            "Only {MAX_BACKUP_FOLDERS} added backup folders are kept; {over_cap} more {} ignored.",
            were(over_cap)
        ));
    }
    (kept, notices)
}

fn plural(n: u32) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

fn were(n: u32) -> &'static str {
    if n == 1 {
        "was"
    } else {
        "were"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, kind: AddedFolderKind) -> AddedFolderEntry {
        AddedFolderEntry {
            path: path.to_owned(),
            kind,
        }
    }

    fn abs(name: &str) -> String {
        if cfg!(windows) {
            format!("C:\\data\\{name}")
        } else {
            format!("/data/{name}")
        }
    }

    #[test]
    fn a_non_array_value_is_reset_with_a_notice() {
        let v = serde_json::json!({"path": "/x"});
        assert!(structural(&v).0.is_empty());
        assert_eq!(structural_notices(Some(&v)).len(), 1);
        assert!(structural_notices(None).is_empty());
    }

    #[test]
    fn bad_elements_are_dropped_with_a_count() {
        let v = serde_json::json!([
            {"path": "/a", "kind": "backup"},
            7,
            {"path": "/b", "kind": "folder"},
            {"path": "/c"},
            {"path": 4, "kind": "parent"}
        ]);
        let (kept, dropped) = structural(&v);
        assert_eq!(kept, vec![entry("/a", AddedFolderKind::Backup)]);
        assert_eq!(dropped, Some(4));
        let notices = structural_notices(Some(&v));
        assert!(notices[0].starts_with("4 entries"));
    }

    #[test]
    fn load_validation_drops_relative_duplicate_staging_and_over_cap() {
        let lib = abs("lib");
        let mut entries = vec![
            entry("relative/x", AddedFolderKind::Parent),
            entry(&abs("one"), AddedFolderKind::Backup),
            entry(&abs("one"), AddedFolderKind::Backup),
            entry(
                &format!(
                    "{lib}{}.oma-staging{}s0123",
                    std::path::MAIN_SEPARATOR,
                    std::path::MAIN_SEPARATOR
                ),
                AddedFolderKind::Parent,
            ),
        ];
        for i in 0..40 {
            entries.push(entry(&abs(&format!("f{i}")), AddedFolderKind::Parent));
        }
        let (kept, notices) = validate_loaded(entries, Some(&lib));
        assert_eq!(kept.len(), MAX_BACKUP_FOLDERS);
        assert_eq!(kept[0], entry(&abs("one"), AddedFolderKind::Backup));
        assert_eq!(notices.len(), 4, "{notices:?}");
        assert!(notices.iter().all(|n| n.ends_with('.')));
    }

    #[test]
    fn a_missing_folder_is_kept() {
        let (kept, notices) = validate_loaded(
            vec![entry(&abs("unplugged-drive"), AddedFolderKind::Parent)],
            None,
        );
        assert_eq!(kept.len(), 1);
        assert!(notices.is_empty());
    }
}
