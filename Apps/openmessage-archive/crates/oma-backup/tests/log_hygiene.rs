//! I6: nothing the crate logs contains a password, a device name, the UDID,
//! a path, a relativePath, a fileID or a SnapshotState value (§20).
//!
//! Its own test binary, with one global capturing subscriber: a
//! thread-local subscriber would miss events from other threads.
mod common;

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use common::*;
use oma_backup::{
    sweep_stale, CancelToken, JobCtx, Progress, StagingRoot, SMS_DB_DOMAIN, SMS_DB_PATH,
};
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<String>>>);

struct Fields(String);

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let _ = write!(self.0, " {}={:?}", field.name(), value);
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        let _ = write!(self.0, " {}={}", field.name(), value);
    }
}

impl<S: Subscriber> Layer<S> for Capture {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut fields = Fields(format!(
            "{} {}",
            event.metadata().level(),
            event.metadata().target()
        ));
        event.record(&mut fields);
        self.0.lock().expect("lock").push(fields.0);
    }
}

fn has_40_hex(s: &str) -> bool {
    let mut run = 0;
    for c in s.chars() {
        if c.is_ascii_hexdigit() {
            run += 1;
            if run >= 40 {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

fn exercise(name: &str, roots: &mut Vec<String>) {
    let fx = Fixture::read_only(name);
    roots.push(fx.tmp.path().to_string_lossy().into_owned());
    let _ = fx.inspect();
    for password in [Some(password_for(name)), Some("wrong-password"), None] {
        let Ok(ob) = fx.open(password) else { continue };
        let token = CancelToken::new();
        let mut sink = |_: Progress| {};
        let mut ctx = JobCtx::new(&token, &mut sink);
        let _ = ob.validate(&mut ctx);
        let _ = ob.extract_database(SMS_DB_DOMAIN, SMS_DB_PATH, &mut ctx);
        if let Ok(Some(entry)) = ob.find_file(ATTACHMENT_DOMAIN, ATTACHMENT_PATH) {
            let _ = ob.extract_file(&entry, &mut ctx);
        }
        let _ = ob.close();
    }
    // The fallback paths: an OpenBackup and a session dropped without close.
    if let Ok(ob) = fx.open(Some(password_for(name))) {
        drop(ob);
    }
    if let Ok(root) = StagingRoot::new(&fx.library, &fx.candidate()) {
        if let Ok(session) = root.begin() {
            drop(session);
        }
    }
    let _ = sweep_stale(&fx.library);
}

#[test]
fn nothing_sensitive_is_logged() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    tracing::subscriber::set_global_default(subscriber).expect("global subscriber");

    let mut roots = vec![fixtures_dir().to_string_lossy().into_owned()];
    for name in ALL_FIXTURES {
        exercise(name, &mut roots);
    }

    let events = capture.0.lock().expect("lock").clone();
    assert!(
        events.iter().any(|e| e.contains("validation finished")),
        "the subscriber captured nothing useful: {events:?}"
    );
    for expected in [
        "manifest staged",
        "staging removed",
        "dropped without close",
        "staging sweep",
    ] {
        assert!(
            events.iter().any(|e| e.contains(expected)),
            "no {expected:?} event"
        );
    }
    let forbidden = [
        PASSWORD,
        UNICODE_PASSWORD,
        "wrong-password",
        DEVICE_NAME,
        UDID,
        "Library/SMS",
        SNAPSHOT_SENTINEL,
        "MediaDomain",
        "HomeDomain",
    ];
    for e in &events {
        for f in forbidden {
            assert!(!e.contains(f), "{f:?} logged: {e}");
        }
        for root in &roots {
            assert!(!e.contains(root.as_str()), "a path was logged: {e}");
        }
        assert!(!has_40_hex(e), "a fileID was logged: {e}");
    }
}
