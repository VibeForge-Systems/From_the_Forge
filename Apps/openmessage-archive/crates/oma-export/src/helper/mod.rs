//! The export-render helper (M6 spec §5.7, D133), slice 6i.
//!
//! PDF layout and serialisation (slice 6j) and XLSX writing (deferred) do
//! not run in the app's process: each such output is made by a helper
//! process, the app's own binary started with `--oma-export-helper`, one
//! per format of one export. Hostile text that crashes shaping, or a
//! backend that panics or runs out of memory under `panic = "abort"`, ends
//! that export with `exportHelperFailed`, never the app; a compromised
//! helper cannot plant active content, because every output is checked by
//! the main process ([`crate::check`]) before any byte of it is staged.
//! **It is not an OS sandbox:** the helper runs as the same user (spec
//! §11.1).
//!
//! - [`frame`]: the length-prefixed frames, bounded at 16 MiB in both
//!   directions and checked before allocating, and their payloads;
//! - [`host`]: the main process's side: [`host::RenderSession`] starts the
//!   helper, sends records on stdin, reads replies on a **dedicated reader
//!   thread** (so a helper that writes early can never deadlock a worker
//!   blocked on a full stdin pipe), and supervises it: killed on cancel
//!   (from a supervisor thread that polls the cancel token, and from the
//!   shell's cancel path through [`host::HelperKill`]), when it stalls on
//!   a write or does not answer within 120 s of `EndVolume`, when it
//!   sends an oversized, early or malformed frame, and when its output
//!   fails the check;
//! - the helper's own side is `crate::render` (feature `render`, enabled
//!   only by the shell), whose `serve` loop the entry calls.
//!
//! Nothing here logs. Helper stderr is drained by a thread that keeps only
//! its byte count (logged by the shell as a bucket, spec §9.6), never its
//! text.

pub mod frame;
pub mod host;

pub use frame::{BackendKind, Begin, DoneCounts, HelperErrorCode, MAX_FRAME_BYTES};
pub use host::{HelperKill, HelperLaunch, RenderSession, SessionLimits, Volume};
