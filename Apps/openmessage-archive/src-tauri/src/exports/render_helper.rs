//! The export-render helper's entry (M6 spec §5.7, D133), slice 6i.
//!
//! PDF layout (slice 6j) and XLSX writing (deferred) run in this binary
//! started again with [`EXPORT_HELPER_FLAG`], one process per format of
//! one export, so a backend that crashes, panics or runs out of memory on
//! hostile text ends one export (`exportHelperFailed`), never the app. It
//! is modelled on the decode helper (D113): `main` calls
//! [`run_export_helper`] **before** Tauri, logging or settings are
//! touched, and the helper speaks only the frames of
//! `oma_export::helper::frame` on stdin and stdout. **It is not an OS
//! sandbox:** it runs as the same user.
//!
//! - **The panic hook** is the payload-free one of [`crate::panic_hook`],
//!   writing its line (a thread name and a source location) to stderr,
//!   which the host only counts.
//! - **Backends.** PDF (slice 6j1: parley layout and krilla output with
//!   the pinned fonts, which the main process checks first,
//!   [`super::pdf_fonts`]); XLSX is deferred and refused with
//!   `Unsupported`. Debug builds also serve the test backend
//!   (`oma_export::render::stub`) that `tests/export_helper.rs` drives.
//! - **The host** is `oma_export::helper::RenderSession`; [`launch`] is
//!   how the app starts it, and [`register_kill`] puts its kill on the
//!   job's cancel path (`Jobs::on_cancel`), so a cancel kills the helper
//!   at once, even while the worker is blocked writing to it.

use std::ffi::OsString;

use oma_export::helper::{HelperKill, HelperLaunch};

use crate::backups::Jobs;

/// The argument that starts the binary as an export-render helper.
pub const EXPORT_HELPER_FLAG: &str = "--oma-export-helper";

/// `true` when the process was started as an export-render helper:
/// exactly one argument, [`EXPORT_HELPER_FLAG`].
pub fn is_export_helper_invocation() -> bool {
    let mut args = std::env::args_os().skip(1);
    matches!(
        (args.next(), args.next()),
        (Some(a), None) if a == EXPORT_HELPER_FLAG
    )
}

/// The helper's main: install the payload-free panic hook, then serve one
/// session on stdin and stdout. Returns the process exit code.
pub fn run_export_helper() -> i32 {
    std::panic::set_hook(crate::panic_hook::hook(|line| {
        use std::io::Write;
        let _ = writeln!(std::io::stderr(), "{line}");
    }));
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut input = stdin.lock();
    let mut output = stdout.lock();
    oma_export::render::serve(&mut input, &mut output)
}

/// How the app starts an export-render helper: this binary with
/// [`EXPORT_HELPER_FLAG`]. `None` when the binary's path cannot be found
/// (every session then fails to start, `exportHelperFailed`).
pub fn launch() -> Option<HelperLaunch> {
    std::env::current_exe()
        .ok()
        .map(|exe| HelperLaunch::new(exe, vec![OsString::from(EXPORT_HELPER_FLAG)]))
}

/// Put `kill` on `job_id`'s cancel path: `Jobs::cancel` (and the exit
/// handler's cancel-all) kills the helper directly. Returns `false` when
/// the job is not running.
pub fn register_kill(jobs: &Jobs, job_id: &str, kill: HelperKill) -> bool {
    jobs.on_cancel(job_id, std::sync::Arc::new(move || kill.kill()))
}
