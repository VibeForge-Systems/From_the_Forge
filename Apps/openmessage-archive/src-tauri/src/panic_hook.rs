//! The panic hook (M3 §16.6, §14).
//!
//! Rust's default hook prints the panic payload, and a payload can carry
//! anything the panicking code formatted, message text included. This hook
//! replaces it: it writes one `error` line with the thread name and the
//! panic location (source file and line), **never the payload**.
//!
//! Release builds abort after the hook runs (`panic = "abort"`), so a panic
//! during an extraction check leaves the staged plaintext copy of the
//! Messages database until the next startup or pre-job sweep removes it
//! (docs/privacy.md).

use std::panic::PanicHookInfo;

/// A boxed panic hook, as `std::panic::set_hook` takes it.
pub type PanicHook = Box<dyn Fn(&PanicHookInfo<'_>) + Send + Sync + 'static>;

/// The line the hook writes for a panic on `thread` at `location`.
pub fn line(thread: Option<&str>, location: Option<(&str, u32)>) -> String {
    let thread = thread.unwrap_or("<unnamed>");
    match location {
        Some((file, n)) => format!("panic in thread {thread} at {file}:{n}"),
        None => format!("panic in thread {thread} at an unknown location"),
    }
}

/// A hook that hands each panic's line to `write`. The payload is never
/// read.
pub fn hook(write: impl Fn(&str) + Send + Sync + 'static) -> PanicHook {
    Box::new(move |info: &PanicHookInfo<'_>| {
        let current = std::thread::current();
        let location = info.location().map(|l| (l.file(), l.line()));
        write(&line(current.name(), location));
    })
}

/// Install the hook for the application. Its line goes to the log, at
/// `error`.
pub fn install() {
    std::panic::set_hook(hook(|line| tracing::error!("{line}")));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_line_names_thread_and_location_only() {
        assert_eq!(
            line(Some("oma-job-3"), Some(("src/backups/worker.rs", 42))),
            "panic in thread oma-job-3 at src/backups/worker.rs:42"
        );
        assert_eq!(
            line(None, None),
            "panic in thread <unnamed> at an unknown location"
        );
    }
}
