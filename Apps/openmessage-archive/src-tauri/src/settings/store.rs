//! Loading and saving settings without ever leaving a half-written file.
//!
//! Loading never fails: every problem degrades to defaults plus a notice the
//! UI shows the user. Saving is atomic (write a sibling temp file, flush it,
//! rename over the original), so a crash or power loss leaves either the old
//! file or the new one — never a truncated mix.

use std::fs::{self, File};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{Settings, SETTINGS_SCHEMA_VERSION};
use crate::error::{ErrorCode, IpcError, IpcResult};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SettingsPersistence {
    /// Where settings are stored, so the user can find (and delete) the file.
    pub file: String,
    /// When true, changes apply for this session only and are not saved.
    pub read_only: bool,
    /// Something the user should know about how settings were loaded.
    pub notice: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SettingsSnapshot {
    pub settings: Settings,
    pub persistence: SettingsPersistence,
}

#[derive(Debug)]
pub struct SettingsStore {
    path: PathBuf,
    settings: Settings,
    read_only: bool,
    notice: Option<String>,
}

/// Only the version is read first, so a file from a newer build is
/// recognized even when the rest of its shape is unknown to this build.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct VersionProbe {
    schema_version: u32,
}

impl SettingsStore {
    pub fn load(path: PathBuf) -> Self {
        let mut store = Self {
            path,
            settings: Settings::default(),
            read_only: false,
            notice: None,
        };

        let bytes = match fs::read(&store.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == ErrorKind::NotFound => return store,
            Err(e) => {
                tracing::warn!(kind = ?e.kind(), "settings file unreadable");
                store.read_only = true;
                store.notice = Some(format!(
                    "The settings file could not be read ({}). Defaults are in use, and changes will not be saved this session.",
                    e.kind()
                ));
                return store;
            }
        };

        match serde_json::from_slice::<VersionProbe>(&bytes) {
            Ok(probe) if probe.schema_version > SETTINGS_SCHEMA_VERSION => {
                tracing::warn!(
                    found = probe.schema_version,
                    "settings from a newer version"
                );
                store.read_only = true;
                store.notice = Some(format!(
                    "These settings were saved by a newer version of OpenMessage Archive (settings format {}; this build understands {}). Defaults are in use, and the file will not be changed.",
                    probe.schema_version, SETTINGS_SCHEMA_VERSION
                ));
                return store;
            }
            Ok(_) => {}
            Err(_) => {
                store.set_aside_corrupt();
                return store;
            }
        }

        match serde_json::from_slice::<Settings>(&bytes) {
            Ok(settings) => {
                store.settings = settings;
                store.revalidate_backup_folders(&bytes);
            }
            Err(_) => store.set_aside_corrupt(),
        }
        store
    }

    /// Explain any `backupFolders` entry the lenient reader dropped, then
    /// re-validate the rest against the current archive location.
    fn revalidate_backup_folders(&mut self, bytes: &[u8]) {
        let raw = serde_json::from_slice::<serde_json::Value>(bytes).ok();
        let raw_folders = raw.as_ref().and_then(|v| v.get("backupFolders"));
        let mut notices = super::folders::structural_notices(raw_folders);
        let entries = std::mem::take(&mut self.settings.backup_folders.0);
        let (kept, more) =
            super::folders::validate_loaded(entries, self.settings.archive_directory.as_deref());
        self.settings.backup_folders.0 = kept;
        notices.extend(more);
        for notice in notices {
            self.add_notice(notice);
        }
    }

    /// Notices are complete sentences, joined with a space in the order they
    /// occurred.
    fn add_notice(&mut self, notice: String) {
        tracing::warn!("settings loaded with a notice");
        self.notice = Some(match self.notice.take() {
            Some(existing) => format!("{existing} {notice}"),
            None => notice,
        });
    }

    /// Whether changes are held in memory only this session.
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Move an unparseable file out of the way rather than overwrite it, so
    /// nothing the user had is destroyed by this build's opinion of it.
    fn set_aside_corrupt(&mut self) {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        let aside = self.path.with_extension(format!("json.corrupt-{stamp}"));
        match fs::rename(&self.path, &aside) {
            Ok(()) => {
                tracing::warn!("settings file was corrupt and has been set aside");
                self.notice = Some(format!(
                    "The settings file was damaged and has been kept as {}. Defaults are in use.",
                    aside.file_name().unwrap_or_default().to_string_lossy()
                ));
            }
            Err(e) => {
                tracing::warn!(kind = ?e.kind(), "corrupt settings file could not be set aside");
                self.read_only = true;
                self.notice = Some(
                    "The settings file is damaged and could not be moved aside. Defaults are in use, and changes will not be saved this session."
                        .to_owned(),
                );
            }
        }
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn snapshot(&self) -> SettingsSnapshot {
        SettingsSnapshot {
            settings: self.settings.clone(),
            persistence: SettingsPersistence {
                file: self.path.display().to_string(),
                read_only: self.read_only,
                notice: self.notice.clone(),
            },
        }
    }

    /// Apply `change` and persist it. The in-memory settings change only if
    /// the write succeeds, so memory and disk never disagree. In read-only
    /// mode the change is applied for this session and an error explains why
    /// it was not saved.
    pub fn update(&mut self, change: impl FnOnce(&mut Settings)) -> IpcResult<()> {
        let mut next = self.settings.clone();
        change(&mut next);
        if self.read_only {
            self.settings = next;
            return Err(IpcError::new(
                ErrorCode::SettingsReadOnly,
                "Settings are read-only this session, so the change applies until you close the application but was not saved.",
            ));
        }
        write_atomically(&self.path, &next).map_err(|e| {
            tracing::error!(kind = ?e.kind(), "settings write failed");
            IpcError::new(
                ErrorCode::Io,
                format!("Settings could not be saved ({}).", e.kind()),
            )
        })?;
        self.settings = next;
        Ok(())
    }
}

fn write_atomically(path: &Path, settings: &Settings) -> std::io::Result<()> {
    let dir = path.parent().ok_or_else(|| {
        std::io::Error::new(ErrorKind::InvalidInput, "settings path has no parent")
    })?;
    fs::create_dir_all(dir)?;
    let mut json = serde_json::to_vec_pretty(settings)?;
    json.push(b'\n');

    let tmp = path.with_extension("json.tmp");
    let result = (|| {
        let mut file = File::create(&tmp)?;
        file.write_all(&json)?;
        file.sync_all()?;
        drop(file);
        // On Windows, std's rename replaces an existing destination
        // (MoveFileExW with MOVEFILE_REPLACE_EXISTING), matching POSIX.
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
        return result;
    }
    // The rename is durable only once the directory entry is flushed. The
    // new settings are already in place, so a failed flush is logged and
    // does not turn a saved change into an error (memory and disk agree).
    match oma_fs::fsync_dir(dir) {
        Ok(oma_fs::DirSync::Synced) => {}
        Ok(oma_fs::DirSync::Unsupported) => {
            tracing::debug!("settings directory flush is not supported here");
        }
        Err(e) => tracing::warn!(kind = ?e.kind(), "settings directory flush failed"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::Theme;
    use super::*;

    fn settings_path(dir: &Path) -> PathBuf {
        dir.join("config").join("settings.json")
    }

    #[test]
    fn a_missing_file_yields_defaults_and_is_not_created_until_a_change() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = settings_path(tmp.path());
        let store = SettingsStore::load(path.clone());
        assert_eq!(store.settings(), &Settings::default());
        assert!(!store.snapshot().persistence.read_only);
        assert!(store.snapshot().persistence.notice.is_none());
        assert!(!path.exists());
    }

    #[test]
    fn changes_round_trip_through_disk() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = settings_path(tmp.path());
        let mut store = SettingsStore::load(path.clone());
        store
            .update(|s| {
                s.theme = Theme::Dark;
                s.archive_directory = Some("/data/Messages Archive".into());
            })
            .expect("save");

        let reloaded = SettingsStore::load(path.clone());
        assert_eq!(reloaded.settings().theme, Theme::Dark);
        assert_eq!(
            reloaded.settings().archive_directory.as_deref(),
            Some("/data/Messages Archive")
        );
        assert!(
            !path.with_extension("json.tmp").exists(),
            "temp file left behind"
        );
    }

    #[test]
    fn a_corrupt_file_is_set_aside_not_overwritten() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = settings_path(tmp.path());
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(&path, b"{ this is not json").expect("write");

        let store = SettingsStore::load(path.clone());
        assert_eq!(store.settings(), &Settings::default());
        let snapshot = store.snapshot();
        assert!(!snapshot.persistence.read_only);
        assert!(snapshot
            .persistence
            .notice
            .expect("notice")
            .contains("damaged"));

        assert!(!path.exists());
        let aside: Vec<_> = fs::read_dir(path.parent().expect("parent"))
            .expect("read_dir")
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".corrupt-"))
            .collect();
        assert_eq!(aside.len(), 1);
        assert_eq!(
            fs::read(aside[0].path()).expect("read"),
            b"{ this is not json",
            "the original bytes must be preserved"
        );
    }

    #[test]
    fn a_wrongly_typed_field_is_treated_as_corrupt() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = settings_path(tmp.path());
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(&path, br#"{"schemaVersion":1,"theme":"sepia"}"#).expect("write");
        let store = SettingsStore::load(path.clone());
        assert_eq!(store.settings(), &Settings::default());
        assert!(!path.exists());
    }

    #[test]
    fn a_file_from_a_newer_version_is_never_overwritten() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = settings_path(tmp.path());
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let future = br#"{"schemaVersion":99,"somethingNew":{"a":1}}"#;
        fs::write(&path, future).expect("write");

        let mut store = SettingsStore::load(path.clone());
        assert!(store.snapshot().persistence.read_only);
        assert!(store
            .snapshot()
            .persistence
            .notice
            .expect("notice")
            .contains("newer version"));

        let err = store
            .update(|s| s.theme = Theme::Light)
            .expect_err("read-only");
        assert_eq!(err.code, ErrorCode::SettingsReadOnly);
        assert_eq!(
            store.settings().theme,
            Theme::Light,
            "applies for the session"
        );
        assert_eq!(fs::read(&path).expect("read"), future, "file untouched");
    }

    #[test]
    fn a_wrongly_typed_backup_folders_value_keeps_the_archive_directory() {
        for raw in [
            r#"{"schemaVersion":1,"archiveDirectory":"/data/lib","backupFolders":"nope"}"#,
            r#"{"schemaVersion":1,"archiveDirectory":"/data/lib","backupFolders":[5,{"path":"/b","kind":"parent"}]}"#,
        ] {
            let tmp = tempfile::tempdir().expect("tempdir");
            let path = settings_path(tmp.path());
            fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            fs::write(&path, raw).expect("write");
            let store = SettingsStore::load(path.clone());
            assert_eq!(
                store.settings().archive_directory.as_deref(),
                Some("/data/lib")
            );
            assert!(path.exists(), "not set aside as corrupt");
            let notice = store.snapshot().persistence.notice.expect("notice");
            assert!(notice.contains("added backup folders"), "{notice}");
        }
    }

    #[test]
    fn backup_folders_round_trip() {
        use super::super::{AddedFolderEntry, AddedFolderKind};
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = settings_path(tmp.path());
        let folder = tmp.path().join("backups").to_string_lossy().into_owned();
        let mut store = SettingsStore::load(path.clone());
        store
            .update(|s| {
                s.backup_folders.push(AddedFolderEntry {
                    path: folder.clone(),
                    kind: AddedFolderKind::Parent,
                })
            })
            .expect("save");
        let reloaded = SettingsStore::load(path);
        assert_eq!(reloaded.settings().backup_folders.len(), 1);
        assert_eq!(reloaded.settings().backup_folders[0].path, folder);
        assert!(reloaded.snapshot().persistence.notice.is_none());
    }

    #[test]
    fn a_failed_write_leaves_memory_unchanged() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = settings_path(tmp.path());
        let mut store = SettingsStore::load(path.clone());
        // After loading, put a file where the settings directory belongs, so
        // create_dir_all fails at save time.
        fs::write(path.parent().expect("parent"), b"").expect("write");
        let err = store
            .update(|s| s.theme = Theme::Dark)
            .expect_err("must fail");
        assert_eq!(err.code, ErrorCode::Io);
        assert_eq!(store.settings().theme, Theme::System);
    }
}
