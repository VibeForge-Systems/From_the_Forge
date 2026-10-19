//! The viewer's preferences (M5 spec §7.6). Additive and defaulted, so
//! `SETTINGS_SCHEMA_VERSION` stays 1. Content-free: every field is a closed
//! choice, a boolean or a small number.
//!
//! - `mediaPlayback` defaults to `confirm` (owner decision Q4).
//! - `revealEarlierText` defaults to off (owner decision Q3b).
//! - `uiZoom` and `captureProtection` are applied from Rust
//!   ([`WindowEffects`]), at startup and on every change (D116); no core
//!   webview permission is granted for either.

use serde::{Deserialize, Deserializer, Serialize};
use ts_rs::TS;

/// How times are written.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum TimeFormat {
    #[default]
    System,
    H12,
    H23,
}

/// Which time zone times are shown in. Rust is time-zone-free (D104): the
/// webview converts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum TimeZoneChoice {
    #[default]
    System,
    Utc,
}

/// Animation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum MotionChoice {
    #[default]
    System,
    Reduce,
}

/// Audio and video playback (owner decision Q4, D99).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum MediaPlayback {
    /// No playback at all: `inspect_attachment` refuses it in Rust.
    Off,
    /// A dialog naming the risk on the first Play of each file in a
    /// session (the default).
    #[default]
    Confirm,
    /// No dialog.
    ClickToPlay,
}

/// Minutes of inactivity before the viewer hides: 0 (off), 1, 5, 15 or 30.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(try_from = "u16", into = "u16")]
#[ts(as = "u16")]
pub struct IdleHideMinutes(u16);

impl IdleHideMinutes {
    pub const ALLOWED: [u16; 5] = [0, 1, 5, 15, 30];

    pub fn get(self) -> u16 {
        self.0
    }
}

impl TryFrom<u16> for IdleHideMinutes {
    type Error = &'static str;
    fn try_from(v: u16) -> Result<Self, Self::Error> {
        if Self::ALLOWED.contains(&v) {
            Ok(IdleHideMinutes(v))
        } else {
            Err("idleHideMinutes must be 0, 1, 5, 15 or 30")
        }
    }
}

impl From<IdleHideMinutes> for u16 {
    fn from(v: IdleHideMinutes) -> u16 {
        v.0
    }
}

/// The in-app zoom, in percent: 100 to 400 in steps of 10 (D116).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(try_from = "u16", into = "u16")]
#[ts(as = "u16")]
pub struct UiZoom(u16);

impl UiZoom {
    pub const MIN: u16 = 100;
    pub const MAX: u16 = 400;
    pub const STEP: u16 = 10;

    pub fn get(self) -> u16 {
        self.0
    }

    /// The webview's scale factor.
    pub fn scale(self) -> f64 {
        f64::from(self.0) / 100.0
    }
}

impl Default for UiZoom {
    fn default() -> Self {
        UiZoom(100)
    }
}

impl TryFrom<u16> for UiZoom {
    type Error = &'static str;
    fn try_from(v: u16) -> Result<Self, Self::Error> {
        if (Self::MIN..=Self::MAX).contains(&v) && v.is_multiple_of(Self::STEP) {
            Ok(UiZoom(v))
        } else {
            Err("uiZoom must be 100 to 400 in steps of 10")
        }
    }
}

impl From<UiZoom> for u16 {
    fn from(v: UiZoom) -> u16 {
        v.0
    }
}

/// The viewer's preferences. In `SettingsUpdate` the whole object is sent
/// and replaces the stored one; unknown fields are refused there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
#[ts(export)]
pub struct ViewerPreferences {
    pub time_format: TimeFormat,
    pub time_zone: TimeZoneChoice,
    pub show_seconds: bool,
    pub motion: MotionChoice,
    /// Last-message text in the conversation list (built and masked in
    /// Rust; when off, `get_conversations` sends no preview text).
    pub list_previews: bool,
    /// Owner decision Q3b: when on, the UI sends `reveal.earlierText` on
    /// page requests. Off by default.
    pub reveal_earlier_text: bool,
    pub media_playback: MediaPlayback,
    pub show_hidden_characters: bool,
    pub blur_on_focus_loss: bool,
    pub idle_hide_minutes: IdleHideMinutes,
    /// Windows only: the window is excluded from screen capture.
    pub capture_protection: bool,
    pub ui_zoom: UiZoom,
}

impl Default for ViewerPreferences {
    fn default() -> Self {
        Self {
            time_format: TimeFormat::default(),
            time_zone: TimeZoneChoice::default(),
            show_seconds: false,
            motion: MotionChoice::default(),
            list_previews: true,
            reveal_earlier_text: false,
            media_playback: MediaPlayback::default(),
            show_hidden_characters: false,
            blur_on_focus_loss: false,
            idle_hide_minutes: IdleHideMinutes::default(),
            capture_protection: false,
            ui_zoom: UiZoom::default(),
        }
    }
}

/// Reading `settings.json`: a malformed `viewer` object falls back to the
/// defaults instead of making the whole file count as corrupt (which would
/// set it aside and lose `archiveDirectory`).
pub(super) fn lenient<'de, D: Deserializer<'de>>(d: D) -> Result<ViewerPreferences, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(value).unwrap_or_else(|_| {
        tracing::warn!("viewer preferences unreadable; defaults in use");
        ViewerPreferences::default()
    }))
}

/// What the window must look like for some preferences (Tauri-free, so it
/// is tested without a window).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowEffects {
    /// `Webview::set_zoom`'s scale factor.
    pub zoom: f64,
    /// `WebviewWindow::set_content_protected`. Only ever true on Windows.
    pub content_protected: bool,
}

impl WindowEffects {
    pub fn of(p: &ViewerPreferences) -> WindowEffects {
        WindowEffects {
            zoom: p.ui_zoom.scale(),
            content_protected: cfg!(windows) && p.capture_protection,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_follow_the_owner_decisions() {
        let p = ViewerPreferences::default();
        assert_eq!(p.media_playback, MediaPlayback::Confirm, "owner Q4");
        assert!(!p.reveal_earlier_text, "owner Q3b");
        assert!(p.list_previews);
        assert!(!p.capture_protection);
        assert_eq!(p.ui_zoom.get(), 100);
        assert_eq!(p.idle_hide_minutes.get(), 0);
        let e = WindowEffects::of(&p);
        assert!((e.zoom - 1.0).abs() < f64::EPSILON);
        assert!(!e.content_protected);
    }

    #[test]
    fn zoom_and_idle_values_are_validated() {
        for ok in [100u16, 110, 250, 400] {
            assert!(UiZoom::try_from(ok).is_ok(), "{ok}");
        }
        for bad in [0u16, 90, 105, 410, 1000] {
            assert!(UiZoom::try_from(bad).is_err(), "{bad}");
        }
        for ok in IdleHideMinutes::ALLOWED {
            assert!(IdleHideMinutes::try_from(ok).is_ok());
        }
        for bad in [2u16, 10, 60] {
            assert!(IdleHideMinutes::try_from(bad).is_err());
        }
        assert!(serde_json::from_str::<ViewerPreferences>(r#"{"uiZoom":410}"#).is_err());
        assert!(serde_json::from_str::<ViewerPreferences>(r#"{"uiZoom":95}"#).is_err());
        assert!(serde_json::from_str::<ViewerPreferences>(r#"{"idleHideMinutes":7}"#).is_err());
        let p: ViewerPreferences =
            serde_json::from_str(r#"{"uiZoom":200,"captureProtection":true}"#).expect("ok");
        assert_eq!(p.ui_zoom.get(), 200);
        let e = WindowEffects::of(&p);
        assert!((e.zoom - 2.0).abs() < f64::EPSILON);
        assert_eq!(e.content_protected, cfg!(windows));
    }

    #[test]
    fn unknown_fields_and_values_are_refused() {
        assert!(serde_json::from_str::<ViewerPreferences>(r#"{"zoom":200}"#).is_err());
        assert!(
            serde_json::from_str::<ViewerPreferences>(r#"{"mediaPlayback":"always"}"#).is_err()
        );
        let p: ViewerPreferences =
            serde_json::from_str(r#"{"mediaPlayback":"off"}"#).expect("partial");
        assert_eq!(p.media_playback, MediaPlayback::Off);
        assert!(p.list_previews, "missing fields take defaults");
    }
}
