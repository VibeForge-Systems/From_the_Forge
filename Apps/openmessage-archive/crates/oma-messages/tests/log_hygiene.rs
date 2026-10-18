//! I6 for the extraction check (M3 §15, §19.3 `log_hygiene.rs`): nothing
//! oma-messages logs, at any level, holds a sentinel from the fixtures:
//! bodies, edit and deleted text, addresses, chat names, group titles,
//! filenames, mentions, GUIDs, or the names of sentinel tables, views,
//! triggers, indexes and columns (including the table `quick_check` would
//! name in `msgs-corrupt-plain`). The failure paths run too, and every
//! error's `Display` and `Debug` is checked as well.
//!
//! Its own test binary, with one global capturing subscriber at TRACE: a
//! thread-local subscriber would miss events from other threads. (The panic
//! hook belongs to the shell and is tested there, §16.6.)
#![allow(clippy::disallowed_types, clippy::disallowed_methods)]
mod common;

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use common::*;
use oma_backup::CancelToken;
use oma_messages::{
    check, CheckOptions, MessagesCtx, MessagesError, MessagesPhase, MessagesProgress,
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

fn record_error(e: &MessagesError, texts: &mut Vec<String>) {
    texts.push(format!("{e}"));
    texts.push(format!("{e:?}"));
    let mut source = std::error::Error::source(e);
    while let Some(s) = source {
        texts.push(format!("{s}"));
        texts.push(format!("{s:?}"));
        source = s.source();
    }
}

#[test]
fn nothing_sensitive_is_logged() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    tracing::subscriber::set_global_default(subscriber).expect("global subscriber");

    let mut texts = Vec::new();
    // Every catalogue entry, including the failure paths (damaged,
    // unsupported, virtual table, hostile values and names).
    for name in catalogue() {
        let fx = Fixture::messages(&name);
        let (result, _) = run_check(&fx, catalogue_password(&name), &CheckOptions::default());
        match result {
            Ok(report) => texts.push(format!("{report:?}")),
            Err(e) => record_error(&e, &mut texts),
        }
    }
    // Cancelled in the middle of the message pass.
    let fx = Fixture::messages("msgs-hostile-plain");
    let ob = fx.open(None);
    let token = CancelToken::new();
    let trigger = token.clone();
    let mut sink = move |p: MessagesProgress| {
        if p.phase == MessagesPhase::ReadingMessages {
            trigger.cancel();
        }
    };
    let mut ctx = MessagesCtx::new(&token, &mut sink);
    let e =
        check(&ob, &CheckOptions::default().with_page_rows(16), &mut ctx).expect_err("cancelled");
    record_error(&e, &mut texts);
    let _ = ob.close();
    // A backup whose sms.db blob is missing: a staging error.
    let fx = Fixture::m2("missing-sms-blob");
    let (result, _) = run_check(&fx, None, &CheckOptions::default());
    record_error(&result.expect_err("no sms.db blob"), &mut texts);

    let events = capture.0.lock().expect("lock").clone();
    for expected in ["extraction check finished", "messages check phase"] {
        assert!(
            events.iter().any(|e| e.contains(expected)),
            "no {expected:?} event in {events:?}"
        );
    }
    for line in events.iter().chain(texts.iter()) {
        let found = sentinels_in(line);
        assert!(found.is_empty(), "{found:?} in {line}");
    }
}
