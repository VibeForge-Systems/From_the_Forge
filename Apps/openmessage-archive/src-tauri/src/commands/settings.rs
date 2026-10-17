use tauri::State;

use super::AppState;
use crate::error::IpcResult;
use crate::settings::{SettingsSnapshot, SettingsUpdate};

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> IpcResult<SettingsSnapshot> {
    Ok(state.settings()?.snapshot())
}

#[tauri::command]
pub fn update_settings(
    state: State<'_, AppState>,
    update: SettingsUpdate,
) -> IpcResult<SettingsSnapshot> {
    let mut store = state.settings()?;
    store.update(|settings| update.apply_to(settings))?;
    Ok(store.snapshot())
}
