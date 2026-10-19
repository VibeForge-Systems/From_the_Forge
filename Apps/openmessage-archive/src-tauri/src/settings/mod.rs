//! Application settings: the model, its IPC shapes, and its persistence.
//!
//! Settings hold user preferences, the location of the archive library and
//! the backup folders the user added — never message content, contact data,
//! or credentials. Backup passwords are never stored; M2 asks for them each
//! time (see docs/privacy.md).

mod folders;
mod location;
mod store;
mod viewer;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub use folders::{AddedFolderEntry, AddedFolderKind, BackupFolders, MAX_BACKUP_FOLDERS};
pub use location::{
    inspect_location, ArchiveLocationStatus, CheckLevel, LocationCheck, LocationState,
};
pub use store::{SettingsPersistence, SettingsSnapshot, SettingsStore};
pub use viewer::{
    IdleHideMinutes, MediaPlayback, MotionChoice, TimeFormat, TimeZoneChoice, UiZoom,
    ViewerPreferences, WindowEffects,
};

/// Bumped whenever the on-disk shape changes incompatibly. A file with a
/// higher number was written by a newer build and is never overwritten.
pub const SETTINGS_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct LayoutPreferences {
    pub left_panel_open: bool,
    pub right_panel_open: bool,
}

impl Default for LayoutPreferences {
    fn default() -> Self {
        Self {
            left_panel_open: true,
            right_panel_open: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Settings {
    pub schema_version: u32,
    /// The archive library: the directory under which each archive gets its
    /// own folder. Stored as UTF-8; non-Unicode paths are rejected at
    /// selection time rather than mangled.
    #[serde(default)]
    pub archive_directory: Option<String>,
    #[serde(default)]
    pub theme: Theme,
    #[serde(default)]
    pub layout: LayoutPreferences,
    /// Backup folders the user added with "Add backup folder…", as
    /// display-canonical UTF-8 paths. Only Rust writes this field (from a
    /// native picker); `SettingsUpdate` cannot. Deserialized leniently: a
    /// malformed value or element is dropped with a notice instead of making
    /// the whole file count as corrupt, which would lose `archiveDirectory`.
    #[serde(default)]
    #[ts(as = "Vec<AddedFolderEntry>")]
    pub backup_folders: BackupFolders,
    /// The viewer's preferences (M5). Read leniently, field by field: an
    /// unreadable field takes its most private value, with a notice.
    #[serde(default, deserialize_with = "viewer::lenient")]
    pub viewer: ViewerPreferences,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: SETTINGS_SCHEMA_VERSION,
            archive_directory: None,
            theme: Theme::default(),
            layout: LayoutPreferences::default(),
            backup_folders: BackupFolders::default(),
            viewer: ViewerPreferences::default(),
        }
    }
}

/// The subset of settings the frontend may change directly. The archive
/// directory is deliberately absent: it can only be set from a native folder
/// picker the backend opens itself, so the webview can never point the
/// application at an arbitrary path.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SettingsUpdate {
    #[ts(optional)]
    pub theme: Option<Theme>,
    #[ts(optional)]
    pub layout: Option<LayoutPreferences>,
    /// Replaces the viewer's preferences as a whole (unknown fields and
    /// out-of-range values are refused).
    #[ts(optional)]
    pub viewer: Option<ViewerPreferences>,
}

impl SettingsUpdate {
    pub fn apply_to(&self, settings: &mut Settings) {
        if let Some(theme) = self.theme {
            settings.theme = theme;
        }
        if let Some(layout) = self.layout {
            settings.layout = layout;
        }
        if let Some(viewer) = self.viewer {
            settings.viewer = viewer;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_changes_only_the_fields_it_names() {
        let mut settings = Settings {
            archive_directory: Some("/data/archives".into()),
            ..Settings::default()
        };
        SettingsUpdate {
            theme: Some(Theme::Dark),
            layout: None,
            viewer: None,
        }
        .apply_to(&mut settings);
        assert_eq!(settings.theme, Theme::Dark);
        assert_eq!(settings.layout, LayoutPreferences::default());
        assert_eq!(
            settings.archive_directory.as_deref(),
            Some("/data/archives")
        );
    }

    #[test]
    fn update_rejects_unknown_fields_and_cannot_set_the_archive_directory() {
        let attempt = r#"{"theme":"dark","archiveDirectory":"/etc"}"#;
        assert!(serde_json::from_str::<SettingsUpdate>(attempt).is_err());
        assert!(serde_json::from_str::<SettingsUpdate>(r#"{"theme":"sepia"}"#).is_err());
        let ok: SettingsUpdate = serde_json::from_str(r#"{"theme":"light"}"#).expect("valid");
        assert_eq!(ok.theme, Some(Theme::Light));
    }

    #[test]
    fn update_cannot_set_backup_folders() {
        let attempt = r#"{"backupFolders":[{"path":"/etc","kind":"parent"}]}"#;
        assert!(serde_json::from_str::<SettingsUpdate>(attempt).is_err());
    }

    #[test]
    fn viewer_preferences_load_leniently_and_update_strictly() {
        // An old file without `viewer` loads with the defaults.
        let old: Settings = serde_json::from_str(r#"{"schemaVersion":1,"archiveDirectory":"/a"}"#)
            .expect("old file");
        assert_eq!(old.viewer, ViewerPreferences::default());
        // A malformed `viewer` keeps the rest of the file and every
        // preference that can still be read.
        let bad: Settings = serde_json::from_str(
            r#"{"schemaVersion":1,"archiveDirectory":"/a",
                "viewer":{"uiZoom":9000,"mediaPlayback":"off","surprise":true}}"#,
        )
        .expect("lenient");
        assert_eq!(bad.archive_directory.as_deref(), Some("/a"));
        assert_eq!(bad.viewer.media_playback, MediaPlayback::Off);
        assert_eq!(bad.viewer.ui_zoom.get(), 100);
        // An update is strict.
        for attempt in [
            r#"{"viewer":{"uiZoom":410}}"#,
            r#"{"viewer":{"uiZoom":90}}"#,
            r#"{"viewer":{"surprise":true}}"#,
            r#"{"viewer":{"mediaPlayback":"auto"}}"#,
        ] {
            assert!(
                serde_json::from_str::<SettingsUpdate>(attempt).is_err(),
                "{attempt}"
            );
        }
        let ok: SettingsUpdate =
            serde_json::from_str(r#"{"viewer":{"mediaPlayback":"off","uiZoom":150}}"#)
                .expect("valid");
        let mut s = Settings::default();
        ok.apply_to(&mut s);
        assert_eq!(s.viewer.media_playback, MediaPlayback::Off);
        assert_eq!(s.viewer.ui_zoom.get(), 150);
        // The round trip keeps every field.
        let json = serde_json::to_string(&s).expect("json");
        let back: Settings = serde_json::from_str(&json).expect("parse");
        assert_eq!(back, s);
    }

    #[test]
    fn missing_optional_fields_take_defaults() {
        let parsed: Settings = serde_json::from_str(r#"{"schemaVersion":1}"#).expect("valid");
        assert_eq!(parsed, Settings::default());
    }
}
