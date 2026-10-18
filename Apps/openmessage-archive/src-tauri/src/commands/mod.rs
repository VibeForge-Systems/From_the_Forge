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
pub mod backups;
pub mod jobs;
pub mod settings;
pub mod system;

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::backups::{BackupRegistry, Jobs};
use crate::error::{IpcError, IpcResult};
use crate::settings::SettingsStore;

/// Application state. No lock is held across file I/O, a dialog, a blocking
/// call or an await: each command clones what it needs and drops the guard.
pub struct AppState {
    settings: Mutex<SettingsStore>,
    /// The last listing: `LocationId` and `BackupId` → what they mean.
    backups: Mutex<BackupRegistry>,
    /// A `list_backups` is in progress.
    pub(crate) listing: AtomicBool,
    /// Running jobs, and the last report and cleanup per backup (in memory
    /// only). Shared with the worker threads.
    pub jobs: Arc<Jobs>,
}

impl AppState {
    pub fn new(settings: SettingsStore) -> Self {
        Self {
            settings: Mutex::new(settings),
            backups: Mutex::new(BackupRegistry::default()),
            listing: AtomicBool::new(false),
            jobs: Arc::new(Jobs::default()),
        }
    }

    pub(crate) fn settings(&self) -> IpcResult<MutexGuard<'_, SettingsStore>> {
        self.settings
            .lock()
            .map_err(|_| IpcError::internal("settings lock poisoned"))
    }

    pub(crate) fn registry(&self) -> IpcResult<MutexGuard<'_, BackupRegistry>> {
        self.backups
            .lock()
            .map_err(|_| IpcError::internal("backup registry lock poisoned"))
    }
}
