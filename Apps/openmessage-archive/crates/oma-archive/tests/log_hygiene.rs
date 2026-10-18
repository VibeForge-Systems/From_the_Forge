//! I6 and §13.2: nothing the crate logs, and no `Debug` or `Display` of a
//! public type, contains the archive label, an archive id, a path, a hex
//! digest, or a value from an imported backup (the catalogue's sentinels:
//! addresses, GUIDs, chat names, the device name).
//!
//! Its own test binary, with one global capturing subscriber: a
//! thread-local subscriber would miss events from other threads.
#![allow(clippy::disallowed_methods)]

mod common;

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use common::import::{import, import_with, open_entry, opts, ENTRIES, PASSWORD};
use common::*;
use oma_archive::hooks::{install, Faults, TestSchema};
use oma_archive::{
    ArchiveCtx, ArchiveError, CancelToken, FixedClock, Label, LineageChoice, LineageId, NoProgress,
    Recovery,
};
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};

const LABEL: &str = "SENTINEL-LABEL-7f3a";

/// Values planted in the import catalogue (its README).
const SENTINELS: &[&str] = &[
    "SYNTH",
    "+1555",
    "example.invalid",
    "synthetic-handle",
    "Synthetic Second Phone",
    "Synthetic Test Phone",
    "SYNTHETIC0",
];

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

fn has_hex_run(s: &str, n: usize) -> bool {
    let mut run = 0;
    for c in s.chars() {
        if c.is_ascii_hexdigit() {
            run += 1;
            if run >= n {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

fn assert_clean(text: &str, banned: &[String], what: &str) {
    for b in banned {
        assert!(!text.contains(b.as_str()), "{what} leaked {b:?}: {text}");
    }
    assert!(
        !has_hex_run(text, 32),
        "{what} holds a hex id or digest: {text}"
    );
}

#[test]
fn logs_errors_and_debug_output_hold_no_label_id_or_path() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    tracing::subscriber::set_global_default(subscriber).expect("subscriber");
    let mut shown: Vec<String> = Vec::new();

    let (tmp, lib) = library();
    let root_text = root(&tmp).to_string_lossy().into_owned();
    let label = Label::parse(LABEL).expect("label");
    let id = lib.create(Some(label.clone())).expect("create");
    shown.push(format!("{lib:?} {id:?} {label:?}"));
    let mut a = lib.open(&id).expect("open");
    shown.push(format!("{a:?} {:?}", a.reader()));
    shown.push(format!("{:?}", a.reader().info().expect("info")));
    {
        let c = raw(&db_path(&tmp, &id));
        insert_session(&c, 0);
    }
    shown.push(format!(
        "{:?}",
        a.begin_write(Recovery::Run).expect("write")
    ));
    // Errors along the way.
    let locked = lib.open(&id).expect_err("locked");
    shown.push(format!("{locked} {locked:?}"));
    shown.push(format!("{:?}", lib.list().expect("list")));
    shown.push(format!("{:?}", a.close()));
    {
        let _faults = install(Faults {
            schema: Some(TestSchema::V2FtsCorruption),
            ..Faults::default()
        });
        let mut a = lib.open(&id).expect("open");
        let e = a
            .migrate(&mut ArchiveCtx {
                cancel: &CancelToken::new(),
                progress: &mut NoProgress,
                clock: &FixedClock(1),
            })
            .expect_err("fails");
        shown.push(format!("{e} {e:?}"));
    }
    {
        let _faults = install(Faults {
            schema: Some(TestSchema::V2),
            ..Faults::default()
        });
        let mut a = lib.open(&id).expect("open");
        let r = a
            .migrate(&mut ArchiveCtx {
                cancel: &CancelToken::new(),
                progress: &mut NoProgress,
                clock: &FixedClock(1),
            })
            .expect("migrates");
        shown.push(format!("{r:?}"));
    }
    let newer = lib.open(&id).expect_err("newer");
    assert!(matches!(newer, ArchiveError::NewerSchema { .. }));
    shown.push(format!("{newer} {newer:?} {:?}", lib.list().expect("list")));
    shown.push(format!("{:?}", lib.sweep()));

    // An import of every catalogue entry (the encrypted one with its
    // password), its outcomes and its errors (spec §18.9). Each import is a
    // new lineage, so each supersedes the session before it.
    let (itmp, ilib) = library();
    let iid = ilib.create(None).expect("create");
    let mut a = ilib.open(&iid).expect("open");
    for entry in ENTRIES {
        let src = open_entry(entry);
        let s = import(&mut a, &src, &opts(16), LineageChoice::New);
        shown.push(format!("{s:?}"));
        let e = import_with(
            &mut a,
            &src,
            &opts(16),
            LineageChoice::Existing(LineageId::new(999)),
            &CancelToken::new(),
        )
        .expect_err("unknown lineage");
        shown.push(format!("{e} {e:?}"));
        let cancel = CancelToken::new();
        cancel.cancel();
        let c = import_with(&mut a, &src, &opts(16), LineageChoice::New, &cancel).expect("cancel");
        shown.push(format!("{c:?}"));
        src.ob.close();
    }
    // The Recently Deleted option, superseding then continuing a session.
    for entry in ["imp-rd-plain", "imp-dev1-v1-plain"] {
        let src = open_entry(entry);
        let first = common::import::cancelled_at_chats(&mut a, &src, &opts(16), LineageChoice::New);
        let mut rd = opts(16);
        rd.include_recently_deleted = true;
        let s = common::import::cancelled_at_chats(
            &mut a,
            &src,
            &rd,
            LineageChoice::Existing(first.lineage),
        );
        assert_eq!(s.superseded, 1, "{entry}");
        shown.push(format!("{first:?} {s:?}"));
        let again = import(&mut a, &src, &rd, LineageChoice::Existing(first.lineage));
        assert!(again.resumed, "{entry}");
        shown.push(format!("{again:?}"));
        src.ob.close();
    }
    shown.push(format!(
        "{a:?} {:?}",
        a.reader().sessions().expect("sessions")
    ));
    drop(a);
    let iroot = root(&itmp).to_string_lossy().into_owned();
    // The provenance files every terminal state wrote (spec §18.9:
    // `imports/*.json`).
    let mut provenance = 0;
    for e in std::fs::read_dir(archive_dir(&itmp, &iid).join("imports"))
        .expect("imports")
        .flatten()
    {
        shown.push(std::fs::read_to_string(e.path()).expect("provenance"));
        provenance += 1;
    }
    assert!(provenance >= ENTRIES.len(), "{provenance} provenance files");

    let mut banned = vec![LABEL.to_owned(), id.as_str().to_owned(), root_text, iroot];
    banned.push(iid.as_str().to_owned());
    banned.extend(SENTINELS.iter().map(|s| (*s).to_owned()));
    banned.push(PASSWORD.to_owned());
    banned.push(id.as_str()[1..].to_owned());
    let logs = capture.0.lock().expect("lock").clone();
    assert!(
        logs.iter().any(|l| l.contains("archive created")),
        "the capture works: {logs:?}"
    );
    assert!(
        logs.iter().any(|l| l.contains("import passes done")),
        "the import logged: {logs:?}"
    );
    for line in &logs {
        // oma-backup logs its own hashed backup id (M2's design); every
        // other rule holds for every crate.
        if line
            .split(' ')
            .nth(1)
            .is_some_and(|t| t.starts_with("oma_archive"))
        {
            assert_clean(line, &banned, "log");
        } else {
            for b in &banned {
                assert!(!line.contains(b.as_str()), "log leaked {b:?}: {line}");
            }
        }
    }
    for s in &shown {
        assert_clean(s, &banned, "debug/display");
    }
}
