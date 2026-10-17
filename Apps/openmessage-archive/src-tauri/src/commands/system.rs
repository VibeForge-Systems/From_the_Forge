use serde::Serialize;
use tauri::{AppHandle, State};
use ts_rs::TS;

use super::AppState;
use crate::error::IpcResult;

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AppInfo {
    pub name: String,
    pub version: String,
    pub identifier: String,
    /// `linux` or `windows` (`std::env::consts::OS`).
    pub os: String,
    pub arch: String,
    pub debug_build: bool,
    pub settings_file: String,
    pub license: String,
    pub source_url: String,
}

#[tauri::command]
pub fn get_app_info(app: AppHandle, state: State<'_, AppState>) -> IpcResult<AppInfo> {
    let config = app.config();
    Ok(AppInfo {
        name: config
            .product_name
            .clone()
            .unwrap_or_else(|| "OpenMessage Archive".to_owned()),
        version: app.package_info().version.to_string(),
        identifier: config.identifier.clone(),
        os: std::env::consts::OS.to_owned(),
        arch: std::env::consts::ARCH.to_owned(),
        debug_build: cfg!(debug_assertions),
        settings_file: state.settings()?.snapshot().persistence.file,
        license: env!("CARGO_PKG_LICENSE").to_owned(),
        source_url: env!("CARGO_PKG_REPOSITORY").to_owned(),
    })
}
