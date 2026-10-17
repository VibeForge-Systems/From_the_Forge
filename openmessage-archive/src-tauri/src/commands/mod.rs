//! IPC commands: the only entry points the webview has into the backend.
//!
//! Every command is listed in build.rs, which makes Tauri generate an
//! `allow-<command>` permission for it; capabilities/default.json grants
//! exactly those. A command added here but not granted there is unreachable,
//! which is the safe failure.
//!
//! Commands validate their own inputs (serde rejects unknown fields and
//! out-of-range enum values before any handler runs) and take no paths from
//! the frontend: filesystem locations come from native dialogs the backend
//! opens itself, or from settings it already validated.

pub mod archive_location;
pub mod settings;
pub mod system;

use std::sync::{Mutex, MutexGuard};

use crate::error::{IpcError, IpcResult};
use crate::settings::SettingsStore;

pub struct AppState {
    settings: Mutex<SettingsStore>,
}

impl AppState {
    pub fn new(settings: SettingsStore) -> Self {
        Self {
            settings: Mutex::new(settings),
        }
    }

    pub(crate) fn settings(&self) -> IpcResult<MutexGuard<'_, SettingsStore>> {
        self.settings
            .lock()
            .map_err(|_| IpcError::internal("settings lock poisoned"))
    }
}
