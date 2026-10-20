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
pub mod archives;
pub mod backups;
pub mod exports;
pub mod jobs;
pub mod settings;
pub mod system;
pub mod viewer;

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::archives::ArchiveEnv;
use crate::backups::{BackupRegistry, Jobs};
use crate::error::{IpcError, IpcResult};
use crate::exports::ExportHost;
use crate::settings::SettingsStore;
use crate::viewer::ViewerHost;

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
    /// How archive libraries are built (a fake free-space probe in tests).
    pub archive_env: ArchiveEnv,
    /// The viewer session, if one is open (M5).
    pub viewer: Arc<ViewerHost>,
    /// The export draft and finished exports' folders (M6).
    pub exports: Arc<ExportHost>,
}

impl AppState {
    pub fn new(settings: SettingsStore) -> Self {
        Self {
            settings: Mutex::new(settings),
            backups: Mutex::new(BackupRegistry::default()),
            listing: AtomicBool::new(false),
            jobs: Arc::new(Jobs::default()),
            archive_env: ArchiveEnv::default(),
            viewer: Arc::new(ViewerHost::default()),
            exports: Arc::new(ExportHost::default()),
        }
    }

    /// `close_viewer`: close the session named by `viewer` and discard any
    /// export draft (M6 §9.2: the privacy controls discard drafts and stop
    /// their counts). Idle hide closes the viewer through this too.
    pub fn close_viewer(&self, viewer: u32) -> IpcResult<()> {
        self.exports.discard_any();
        self.viewer.close(viewer)
    }

    /// `hide_viewer`: the privacy screen is up. Any export draft is
    /// discarded first, whatever the session.
    pub fn hide_viewer(&self, viewer: u32) -> IpcResult<()> {
        self.exports.discard_any();
        let s = self.viewer.get(viewer)?;
        crate::viewer::hide(&s)
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
