//! I6 in the shell: nothing the job worker, the listing or the error
//! mapping logs contains a password, a device name, the UDID, a path, a
//! relativePath, a fileID or a SnapshotState value (§20).
//!
//! Its own test binary with one global capturing subscriber: a thread-local
//! subscriber would miss events from worker threads.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use oma_backup::{inspect, Candidate, Limits, Password, Zeroizing};
use openmessage_archive_lib::backups::dto::JobEvent;
use openmessage_archive_lib::backups::listing::{build_registry, ListInputs};
use openmessage_archive_lib::backups::worker::{self, JobSpec};
use openmessage_archive_lib::backups::{messages, JobEventSink, Jobs};
use openmessage_archive_lib::settings::{AddedFolderEntry, AddedFolderKind};
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};

const UDID: &str = "SYNTHETIC0-0000000000000000";
const PASSWORD: &str = "synthetic-test-password";
const UNICODE_PASSWORD: &str = "synthetic-pässwörd-ключ-\u{1F511}";
const DEVICE_NAME: &str = "Synthetic Test Phone";
const SNAPSHOT_SENTINEL: &str = "synthetic-uploading";

/// Fixtures run through the worker: the full flows and every failure kind.
const JOB_FIXTURES: &[&str] = &[
    "plain",
    "plain-wal",
    "enc",
    "enc-unicode-pw",
    "enc-wal",
    "enc-dup-clas",
    "hostile-paths",
    "hostile-fileid",
    "hostile-dpic",
    "hostile-iter",
    "hostile-keybag-truncated",
    "hostile-wrap-device-only",
    "hostile-padding-manifest",
    "hostile-padding-sms",
    "hostile-size-max",
    "hostile-mbfile-oversize",
    "bomb-mbfile",
    "missing-sms-blob",
];

/// Fixtures that only list (they are not importable).
const LIST_FIXTURES: &[&str] = &[
    "incomplete",
    "incomplete-no-status",
    "snapshot-dir",
    "legacy-mbdb",
    "enc-no-manifestkey",
    "enc-no-keybag",
    "wrong-type-isencrypted",
    "encrypted-plaintext-db",
    "bomb-info",
    "bomb-status",
    "bomb-manifest",
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

fn fixtures() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tests/fixtures/backups"
    ))
}

fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).expect("mkdir");
    for e in std::fs::read_dir(src).expect("read_dir") {
        let e = e.expect("entry");
        let to = dst.join(e.file_name());
        if e.file_type().expect("type").is_dir() {
            copy_tree(&e.path(), &to);
        } else {
            std::fs::copy(e.path(), &to).expect("copy");
        }
    }
}

#[derive(Default)]
struct Sink(Vec<JobEvent>);

impl JobEventSink for Sink {
    fn send(&mut self, event: JobEvent) {
        self.0.push(event);
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

fn run_job(name: &str, password: Option<&str>, tmp: &Path) -> Vec<JobEvent> {
    let dir = tmp.join(format!("job-{name}")).join(UDID);
    copy_tree(&fixtures().join(name).join(UDID), &dir);
    let library = tmp.join(format!("lib-{name}"));
    std::fs::create_dir_all(&library).expect("mkdir");
    let limits = Limits::default();
    let candidate = Candidate::for_dir(&dir).expect("candidate");
    let inspection = inspect(&candidate, &limits, &mut 0).expect("inspect");
    if !inspection.is_importable() {
        return Vec::new();
    }
    let jobs = Jobs::default();
    let reservation = jobs.reserve(inspection.id.as_str()).expect("reserve");
    let spec = JobSpec {
        job_id: reservation.job_id.clone(),
        backup_id: inspection.id.as_str().to_owned(),
        inspection,
        library,
        password: password
            .map(|p| Password::new(Zeroizing::new(p.to_owned()), &limits).expect("password")),
        limits,
        cancel: reservation.cancel.clone(),
    };
    let mut sink = Sink::default();
    worker::run(spec, &jobs, &mut sink);
    sink.0
}

#[test]
fn nothing_identifying_is_logged() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    tracing::subscriber::set_global_default(subscriber).expect("global subscriber");

    let tmp = tempfile::tempdir().expect("tempdir");
    let mut messages_seen = Vec::new();

    for name in JOB_FIXTURES {
        let right = if *name == "enc-unicode-pw" {
            UNICODE_PASSWORD
        } else {
            PASSWORD
        };
        for password in [Some(right), Some("wrong-password"), None] {
            for event in run_job(name, password, tmp.path()) {
                if let JobEvent::Failed { error, .. } = &event {
                    messages_seen.push(error.message.clone());
                }
                if let JobEvent::Finished { report, .. } = &event {
                    messages_seen.extend(report.checks.iter().map(|c| c.message.clone()));
                }
            }
        }
    }

    let parent = tmp.path().join("parent");
    for (i, name) in JOB_FIXTURES.iter().chain(LIST_FIXTURES).enumerate() {
        copy_tree(
            &fixtures().join(name).join(UDID),
            &parent.join(format!("b{i}")),
        );
    }
    let reg = build_registry(&ListInputs {
        default_roots: Vec::new(),
        added: vec![AddedFolderEntry {
            path: parent.to_string_lossy().into_owned(),
            kind: AddedFolderKind::Parent,
        }],
        library: Some(tmp.path().join("lib-plain")),
        limits: Limits::default(),
    });
    let jobs = Jobs::default();
    for b in reg.list(&jobs).backups {
        if b.state != openmessage_archive_lib::backups::dto::BackupState::Incomplete {
            messages_seen.push(b.state_reason.clone());
        }
    }
    let _ = messages::to_ipc(&oma_backup::BackupError::WrongPassword, &Limits::default());

    let root = tmp.path().to_string_lossy().into_owned();
    let events = capture.0.lock().expect("lock").clone();
    assert!(!events.is_empty(), "the worker logs something");
    for e in &events {
        for forbidden in [
            PASSWORD,
            UNICODE_PASSWORD,
            "wrong-password",
            DEVICE_NAME,
            UDID,
            "Library/SMS",
            SNAPSHOT_SENTINEL,
            root.as_str(),
        ] {
            assert!(!e.contains(forbidden), "{forbidden:?} logged in {e}");
        }
        assert!(!has_40_hex(e), "a fileID-like value was logged: {e}");
    }
    // User-facing messages from errors and checks never carry device text,
    // passwords or SnapshotState values either.
    for m in &messages_seen {
        for forbidden in [PASSWORD, DEVICE_NAME, UDID, SNAPSHOT_SENTINEL] {
            assert!(!m.contains(forbidden), "{forbidden:?} in message {m}");
        }
    }
}
