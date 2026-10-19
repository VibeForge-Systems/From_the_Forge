use tauri::{State, WebviewWindow};

use super::AppState;
use crate::error::IpcResult;
use crate::settings::{SettingsSnapshot, SettingsUpdate, ViewerPreferences, WindowEffects};

/// Apply the viewer's zoom and capture protection to the window (D116).
/// Failures are logged by kind and never fail the command.
pub(crate) fn apply_window_effects(window: &WebviewWindow, prefs: &ViewerPreferences) {
    let e = WindowEffects::of(prefs);
    if window.set_zoom(e.zoom).is_err() {
        tracing::warn!("zoom could not be applied");
    }
    if cfg!(windows) && window.set_content_protected(e.content_protected).is_err() {
        tracing::warn!("capture protection could not be applied");
    }
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> IpcResult<SettingsSnapshot> {
    Ok(state.settings()?.snapshot())
}

#[tauri::command]
pub fn update_settings(
    window: WebviewWindow,
    state: State<'_, AppState>,
    update: SettingsUpdate,
) -> IpcResult<SettingsSnapshot> {
    let snapshot = {
        let mut store = state.settings()?;
        store.update(|settings| update.apply_to(settings))?;
        store.snapshot()
    };
    if update.viewer.is_some() {
        apply_window_effects(&window, &snapshot.settings.viewer);
    }
    Ok(snapshot)
}
