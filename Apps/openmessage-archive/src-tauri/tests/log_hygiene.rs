//! I6 in the shell: nothing the job worker, the listing or the error
//! mapping logs contains a password, a device name, the UDID, a path, a
//! relativePath, a fileID or a SnapshotState value (§20).
//!
//! M3 (§15): extraction-check jobs over every entry of the synthetic
//! messages catalogue, including the damaged, unsupported, virtual-table and
//! hostile ones, plus a forced panic through the panic hook. No sentinel
//! planted in those fixtures may appear in a log line, in a `JobEvent`'s
//! JSON, in the extraction report's JSON, in `BackupDetails` JSON or in the
//! panic hook's output.
//!
//! M4 (§18.9): import jobs over every entry of the synthetic import
//! catalogue, each confirmed at its gate, then a verify job; the events,
//! the archive details and the listing are checked like the reports, and
//! no log line carries an archive id or the archive's label.
//!
//! M5 (§7.7): every viewer operation over archives imported from the
//! synthetic viewer catalogue, with filter and query strings that carry
//! sentinels, refusals included: no content, filter or query text and no
//! archive id reaches a log line. The `oma-media` handler is driven for
//! every attachment token, with the real decode helper (this package's
//! binary): no token, URI, range or exact size is logged (§6.4 rule 5).
//!
//! Its own test binary with one global capturing subscriber: a thread-local
//! subscriber would miss events from worker threads.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use oma_archive::space::SpaceProbe;
use oma_archive::{Label, LineageChoice, VerifyDepth};
use oma_backup::{inspect, Candidate, Limits, Password, Zeroizing};
use openmessage_archive_lib::archives::dto::ImportOptionsInput;
use openmessage_archive_lib::archives::gate::GateAnswer;
use openmessage_archive_lib::archives::worker::{run_import, run_verify, ImportSpec, VerifySpec};
use openmessage_archive_lib::archives::{self, ArchiveEnv};
use openmessage_archive_lib::backups::dto::{JobEvent, JobKind};
use openmessage_archive_lib::backups::listing::{self, build_registry, ArchiveFacts, ListInputs};
use openmessage_archive_lib::backups::worker::{self, JobSpec};
use openmessage_archive_lib::backups::JobSubject;
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

fn messages_fixtures() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tests/fixtures/messages"
    ))
}

/// Strings planted in the synthetic messages catalogue (the same list as
/// `crates/oma-messages/tests/common/mod.rs`).
const MESSAGE_SENTINELS: &[&str] = &[
    "SYNTH-BODY",
    "SYNTH-EDIT",
    "SYNTH-DELETED",
    "synthetic-handle",
    "+1555",
    "1555SYNTH",
    "SYNTH-CHAT",
    "chatSYNTH",
    "SYNTH-GROUP",
    "SYNTH-PERSON",
    "SYNTH-FILE",
    "SYNTH-GENMOJI",
    "SYNTH-MENTION",
    "SYNTH-SUBJECT",
    "SYNTH-MSG-GUID",
    "SYNTH-ATT-GUID",
    "SYNTH-MISSING",
    "SYNTH_SENTINEL",
    "SYNTH-TRANSLATION",
    "SYNTH-LANG",
    "SYNTH-PAYLOAD",
    "SYNTH-PROPERTY",
    "SYNTH-KEY",
    "SYNTH-VALUE",
    "SYNTH-CORRUPT",
    "SYNTH-LONG",
    "SYNTH-SERVICE",
    "SYNTH-TEXT",
    "SYNTH-NOT-A",
    "SYNTHLinkClass",
    "SYNTHUnknownValue",
    "__kSYNTHUnknownKey",
    "at_0_00000000",
    "\u{1F525}",
    "example.invalid",
    "com.example.synthetic",
];

/// Every entry of the messages catalogue.
fn messages_catalogue() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(messages_fixtures())
        .expect("messages dir")
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.ends_with(".expected"))
        .collect();
    names.sort();
    names
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

const LABEL_SENTINEL: &str = "SYNTH-LABEL-SENTINEL";

fn imports_fixtures() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tests/fixtures/imports"
    ))
}

/// Every backup entry of the import catalogue.
fn imports_catalogue() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(imports_fixtures())
        .expect("imports dir")
        .flatten()
        .filter(|e| e.path().join(UDID).is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

struct FakeSpace;

impl SpaceProbe for FakeSpace {
    fn available(&self, _: &Path) -> std::io::Result<u64> {
        Ok(1 << 40)
    }
    fn total(&self, _: &Path) -> std::io::Result<u64> {
        Ok(2 << 40)
    }
}

/// Confirms the gate from inside the event stream (the gate is open
/// before `importPreflight` is sent).
struct Confirming<'a> {
    jobs: &'a Jobs,
    job_id: String,
    events: Vec<JobEvent>,
}

impl JobEventSink for Confirming<'_> {
    fn send(&mut self, event: JobEvent) {
        if matches!(event, JobEvent::ImportPreflight { .. }) {
            self.jobs
                .confirm(&self.job_id, GateAnswer::Proceed(LineageChoice::New))
                .expect("confirm");
        }
        self.events.push(event);
    }
}

/// Import `name` into a fresh labelled archive, then verify it. Returns
/// the archive id; `texts` receives the events, the details and the list.
fn import_and_verify(name: &str, tmp: &Path, texts: &mut Vec<String>) -> String {
    import_and_verify_from(&imports_fixtures(), name, tmp, texts)
}

/// [`import_and_verify`] for an entry of the catalogue at `root`. The
/// library is `tmp/implib-<name>`.
fn import_and_verify_from(root: &Path, name: &str, tmp: &Path, texts: &mut Vec<String>) -> String {
    let dir = tmp.join(format!("imp-{name}")).join(UDID);
    copy_tree(&root.join(name).join(UDID), &dir);
    let library = tmp.join(format!("implib-{name}"));
    std::fs::create_dir_all(&library).expect("mkdir");
    let env = ArchiveEnv {
        space: Some(std::sync::Arc::new(FakeSpace)),
    };
    let id = env
        .library(&library)
        .expect("library")
        .create(Some(Label::parse(LABEL_SENTINEL).expect("label")))
        .expect("create");
    let limits = Limits::default();
    let candidate = Candidate::for_dir(&dir).expect("candidate");
    let inspection = inspect(&candidate, &limits, &mut 0).expect("inspect");
    let backup_id = inspection.id.as_str().to_owned();
    let jobs = Jobs::default();
    let r = jobs
        .reserve(JobSubject::import(&backup_id, id.as_str()), JobKind::Import)
        .expect("reserve");
    let password = name
        .ends_with("-enc")
        .then(|| Password::new(Zeroizing::new(PASSWORD.to_owned()), &limits).expect("pw"));
    let spec = ImportSpec {
        job: JobSpec {
            job_id: r.job_id.clone(),
            backup_id,
            kind: JobKind::Import,
            inspection,
            library: library.clone(),
            password,
            limits,
            cancel: r.cancel.clone(),
        },
        archive_id: id.clone(),
        options: ImportOptionsInput {
            include_recently_deleted: true,
            copy_attachments: true,
        },
        gate: r.gate.clone().expect("gate"),
        env: env.clone(),
    };
    let mut sink = Confirming {
        jobs: &jobs,
        job_id: r.job_id.clone(),
        events: Vec::new(),
    };
    run_import(spec, &jobs, &mut sink);
    let r = jobs
        .reserve(JobSubject::archive(id.as_str()), JobKind::Verify)
        .expect("reserve");
    let mut verify_events = Sink::default();
    run_verify(
        VerifySpec {
            job_id: r.job_id.clone(),
            archive_id: id.clone(),
            library: library.clone(),
            depth: VerifyDepth::Full,
            cancel: r.cancel.clone(),
            env: env.clone(),
        },
        &jobs,
        &mut verify_events,
    );
    for e in sink.events.iter().chain(verify_events.0.iter()) {
        texts.push(serde_json::to_string(e).expect("json"));
    }
    let details = archives::details(&library, &env, &id, &jobs, &[]).expect("details");
    texts.push(serde_json::to_string(&details).expect("json"));
    let list = archives::listing(Some(&library), &env, &jobs);
    texts.push(serde_json::to_string(&list).expect("json"));
    id.as_str().to_owned()
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
    run_job_in(
        &fixtures(),
        name,
        password,
        tmp,
        JobKind::Validate,
        &mut Vec::new(),
    )
}

/// Run one job; `texts` receives the JSON of the extraction report and of
/// `BackupDetails` afterwards.
fn run_job_in(
    root: &Path,
    name: &str,
    password: Option<&str>,
    tmp: &Path,
    kind: JobKind,
    texts: &mut Vec<String>,
) -> Vec<JobEvent> {
    let dir = tmp.join(format!("job-{name}-{kind:?}")).join(UDID);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).expect("clean");
    }
    copy_tree(&root.join(name).join(UDID), &dir);
    let library = tmp.join(format!("lib-{name}"));
    std::fs::create_dir_all(&library).expect("mkdir");
    let limits = Limits::default();
    let candidate = Candidate::for_dir(&dir).expect("candidate");
    let inspection = inspect(&candidate, &limits, &mut 0).expect("inspect");
    if !inspection.is_importable() {
        return Vec::new();
    }
    let jobs = Jobs::default();
    let backup_id = inspection.id.as_str().to_owned();
    let reservation = jobs.reserve(&backup_id, kind).expect("reserve");
    let spec = JobSpec {
        job_id: reservation.job_id.clone(),
        backup_id: backup_id.clone(),
        kind,
        inspection,
        library: library.clone(),
        password: password
            .map(|p| Password::new(Zeroizing::new(p.to_owned()), &limits).expect("password")),
        limits: limits.clone(),
        cancel: reservation.cancel.clone(),
    };
    let mut sink = Sink::default();
    worker::run(spec, &jobs, &mut sink);
    if let Some(ext) = jobs.last(&backup_id).extraction {
        texts.push(serde_json::to_string(&ext).expect("json"));
    }
    let insp = inspect(&candidate, &limits, &mut 0).expect("inspect");
    let facts = ArchiveFacts {
        library: Some(library),
        configured: true,
        sync_provider: None,
    };
    let details = listing::details(&insp, "loc", &facts, &jobs, &limits);
    texts.push(serde_json::to_string(&details).expect("json"));
    sink.0
}

fn viewer_fixtures() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tests/fixtures/viewer"
    ))
}

/// Values planted in the viewer catalogue (its README), and the filter
/// and query sentinels this test sends.
const VIEWER_SENTINELS: &[&str] = &[
    "SYNTH-VIEW",
    "SYNTH-HX",
    "SYNTH-MEDIA",
    "SYNTH-LIN",
    "SYNTH-OLD",
    "SYNTH-OTC",
    "example.invalid",
    "+1555",
    FILTER_SENTINEL,
    QUERY_SENTINEL,
];
const FILTER_SENTINEL: &str = "SYNTH-FILTER-SENTINEL";
const QUERY_SENTINEL: &str = "SYNTH-QUERY-SENTINEL";

fn req<T: serde::de::DeserializeOwned>(v: serde_json::Value) -> T {
    serde_json::from_value(v).expect("request")
}

/// Run every viewer operation on one archive, refusals included. Returns
/// the DTOs as JSON (they carry content, so they are never added to the
/// content-free texts).
fn exercise_viewer(
    library: &Path,
    id: &str,
    tokens: &mut Vec<String>,
    sizes: &mut std::collections::BTreeSet<u64>,
) -> Vec<String> {
    use openmessage_archive_lib::settings::MediaPlayback;
    use openmessage_archive_lib::viewer::media::engine::MediaConfig;
    use openmessage_archive_lib::viewer::media::helper::{
        HelperCommand, HelperHost, RenderBackend, HELPER_FLAG,
    };
    use openmessage_archive_lib::viewer::media::{self, MediaRequest, Reply};
    use openmessage_archive_lib::viewer::{self, ViewerHost};
    use serde_json::json;
    let env = ArchiveEnv {
        space: Some(std::sync::Arc::new(FakeSpace)),
    };
    let helper = HelperCommand::new(
        PathBuf::from(env!("CARGO_BIN_EXE_openmessage-archive")),
        vec![std::ffi::OsString::from(HELPER_FLAG)],
    );
    let host = ViewerHost::with_media(MediaConfig {
        backend: std::sync::Arc::new(move || {
            Box::new(HelperHost::new(Some(helper.clone()))) as Box<dyn RenderBackend>
        }),
        crash_cap: 5,
    });
    // Every exact size the protocol served: bodies, Content-Length and
    // the totals of Content-Range.
    let served_sizes = std::sync::Arc::new(Mutex::new(Vec::<u64>::new()));
    let sink = std::sync::Arc::clone(&served_sizes);
    let ask = move |host: &ViewerHost, target: &str, ranges: &[&str]| -> u16 {
        let (tx, rx) = std::sync::mpsc::channel();
        let sink = std::sync::Arc::clone(&sink);
        let reply = Reply::new(Box::new(move |r: media::MediaResponse| {
            if let Ok(mut v) = sink.lock() {
                v.push(r.body.len() as u64);
                if let Some(n) = r.header("Content-Length").and_then(|l| l.parse().ok()) {
                    v.push(n);
                }
                if let Some(n) = r
                    .header("Content-Range")
                    .and_then(|c| c.rsplit('/').next())
                    .and_then(|t| t.parse().ok())
                {
                    v.push(n);
                }
            }
            let _ = tx.send(r.status);
        }));
        media::dispatch(
            host,
            &MediaRequest {
                label: "main",
                method: "GET",
                target,
                ranges,
                origins: &[],
                playback_allowed: true,
            },
            reply,
        );
        rx.recv_timeout(std::time::Duration::from_secs(60))
            .expect("media answer")
    };
    let jobs = Jobs::default();
    let opened = viewer::open(&host, library, &env, &jobs, id).expect("open viewer");
    let v = opened.viewer;
    let mut out = vec![serde_json::to_string(&opened).expect("json")];
    let all = json!({ "earlierText": true, "oneTimeCode": true });
    viewer::with_session(&host, v, |s| {
        let mut push = |j: String| out.push(j);
        for filter in [None, Some(FILTER_SENTINEL), Some("SYNTH")] {
            let mut r = json!({ "viewer": v, "sort": "name" });
            if let Some(f) = filter {
                r["filter"] = json!(f);
            }
            push(serde_json::to_string(&viewer::list_conversations(s, &req(r))?).expect("json"));
        }
        let long = format!("{FILTER_SENTINEL}{}", "x".repeat(200));
        let refused =
            viewer::list_conversations(s, &req(json!({ "viewer": v, "sort": "name", "filter": long })));
        assert!(refused.is_err());
        let index = viewer::list_conversations(s, &req(json!({ "viewer": v, "sort": "recent" })))?;
        let ids: Vec<i64> = index.ids.iter().map(|i| i.get()).collect();
        push(
            serde_json::to_string(&viewer::get_conversations(
                s,
                &req(json!({ "viewer": v, "ids": ids })),
                true,
            )?)
            .expect("json"),
        );
        for chat in &ids {
            for anchor in [json!({ "kind": "newest" }), json!({ "kind": "undated" })] {
                let page = viewer::thread_page(
                    s,
                    &req(json!({ "viewer": v, "chatId": chat, "anchor": anchor,
                                 "direction": "older", "limit": 100, "reveal": all })),
                )?;
                for m in page.items.iter().take(5) {
                    let d = viewer::message_detail(
                        s,
                        &req(json!({ "viewer": v, "messageId": m.id.get(), "reveal": all, "reactionPage": {} })),
                    )?;
                    for b in &d.versions {
                        push(
                            serde_json::to_string(&viewer::message_text(
                                s,
                                &req(json!({ "viewer": v, "bodyId": b.body.get(), "fromUtf16": 3, "reveal": all })),
                            )?)
                            .expect("json"),
                        );
                    }
                    push(serde_json::to_string(&d).expect("json"));
                    push(
                        serde_json::to_string(&viewer::reply_thread(
                            s,
                            &req(json!({ "viewer": v, "messageId": m.id.get() })),
                        )?)
                        .expect("json"),
                    );
                }
                for m in &page.items {
                    for a in &m.attachments {
                        for playback in [MediaPlayback::Confirm, MediaPlayback::Off] {
                            push(
                                serde_json::to_string(&viewer::inspect(
                                    s,
                                    &req(json!({ "viewer": v, "attachmentId": a.id.get(), "forPlayback": true })),
                                    playback,
                                )?)
                                .expect("json"),
                            );
                        }
                        // The protocol, for every token: renditions through
                        // the real helper, streams, refusals.
                        if let Some(t) = &a.token {
                            let t = t.as_str();
                            tokens.push(t.to_owned());
                            for (rendition, ranges) in [
                                ("thumb", &[][..]),
                                ("display", &[][..]),
                                ("thumb", &[][..]),
                                ("stream", &["bytes=0-"][..]),
                                ("stream", &["bytes=-0"][..]),
                                ("stream", &["bytes= 0-1"][..]),
                                ("stream", &[][..]),
                                ("raw", &[][..]),
                            ] {
                                let status = ask(&host, &format!("/v1/{t}/{rendition}"), ranges);
                                push(format!("media {rendition} {status}"));
                            }
                        }
                    }
                }
                push(serde_json::to_string(&page).expect("json"));
            }
            push(
                serde_json::to_string(&viewer::day_counts(
                    s,
                    &req(json!({ "viewer": v, "chatId": chat, "boundsUtcMs": [-1_000_000_000_000_i64, 0, 4_000_000_000_000_i64] })),
                )?)
                .expect("json"),
            );
            push(
                serde_json::to_string(&viewer::seek_date(
                    s,
                    &req(json!({ "viewer": v, "chatId": chat, "utcMs": 0, "dir": "onOrAfter" })),
                )?)
                .expect("json"),
            );
        }
        for (query, mode) in [
            ("SYNTH", "auto"),
            ("SYNTH", "exact"),
            (QUERY_SENTINEL, "auto"),
            (QUERY_SENTINEL, "exact"),
        ] {
            let started = viewer::search(
                s,
                &req(json!({ "viewer": v, "query": query, "mode": mode, "includeEarlier": true, "includeEvents": true })),
                &mut |_| {},
            )?;
            if started.total > 0 {
                push(
                    serde_json::to_string(&viewer::search_results(
                        s,
                        &req(json!({ "viewer": v, "searchId": started.search_id, "offset": 0, "limit": 50 })),
                    )?)
                    .expect("json"),
                );
            }
            viewer::cancel_search(s, started.search_id)?;
        }
        let bad = format!("{QUERY_SENTINEL}\u{1}");
        assert!(viewer::search(
            s,
            &req(json!({ "viewer": v, "query": bad, "mode": "auto" })),
            &mut |_| {}
        )
        .is_err());
        // The privacy screen (slice 5j).
        viewer::hide(s)?;
        Ok(())
    })
    .expect("viewer operations");
    host.close(v).expect("close");
    sizes.extend(served_sizes.lock().expect("sizes").iter().copied());
    out
}

/// The `name=value` fields of a captured event line.
fn fields_of(e: &str) -> Vec<(&str, &str)> {
    e.split(' ').filter_map(|t| t.split_once('=')).collect()
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

    // M3: extraction checks over the messages catalogue, failure paths
    // included, and a forced panic through the hook.
    let mut texts = Vec::new();
    for name in messages_catalogue() {
        let password = name.ends_with("-enc").then_some(PASSWORD);
        for event in run_job_in(
            &messages_fixtures(),
            &name,
            password,
            tmp.path(),
            JobKind::ExtractionCheck,
            &mut texts,
        ) {
            texts.push(serde_json::to_string(&event).expect("json"));
            if let JobEvent::Failed { error, .. } = &event {
                messages_seen.push(error.message.clone());
            }
        }
    }
    assert_eq!(
        texts
            .iter()
            .filter(|t| t.contains("\"extractionFinished\""))
            .count(),
        messages_catalogue().len(),
        "every catalogue entry ended with an extraction summary"
    );
    // M4: an import and a full verify for every import catalogue entry.
    let mut archive_ids = Vec::new();
    let mut archive_texts = Vec::new();
    for name in imports_catalogue() {
        archive_ids.push(import_and_verify(&name, tmp.path(), &mut archive_texts));
    }
    assert!(archive_ids.len() >= 10, "the import catalogue was found");
    assert_eq!(
        archive_texts
            .iter()
            .filter(|t| t.contains("\"importFinished\"") && t.contains("\"completed\""))
            .count(),
        archive_ids.len(),
        "every import completed"
    );
    for t in &archive_texts {
        for forbidden in [PASSWORD, UDID, DEVICE_NAME] {
            assert!(!t.contains(forbidden), "{forbidden:?} in {t}");
        }
    }
    texts.extend(archive_texts);

    // M5: every viewer operation over the viewer catalogue.
    let mut viewer_outputs = Vec::new();
    let mut media_tokens = Vec::new();
    let mut media_sizes = std::collections::BTreeSet::new();
    for f in std::fs::read_dir(viewer_fixtures().join("media")).expect("media") {
        media_sizes.insert(f.expect("entry").metadata().expect("meta").len());
    }
    let mut viewer_events = Vec::new();
    for name in [
        "view-main-plain",
        "view-hostile-text-plain",
        "view-media-plain",
    ] {
        let mut viewer_texts = Vec::new();
        let id = import_and_verify_from(&viewer_fixtures(), name, tmp.path(), &mut viewer_texts);
        let library = tmp.path().join(format!("implib-{name}"));
        let from = capture.0.lock().expect("lock").len();
        viewer_outputs.extend(exercise_viewer(
            &library,
            &id,
            &mut media_tokens,
            &mut media_sizes,
        ));
        let to = capture.0.lock().expect("lock").len();
        viewer_events.push(from..to);
        archive_ids.push(id);
        texts.extend(viewer_texts);
    }
    assert!(
        viewer_outputs.iter().any(|o| o.contains("SYNTH-")),
        "the viewer returned content (so the log check means something)"
    );

    openmessage_archive_lib::panic_hook::install();
    let panicked = std::panic::catch_unwind(|| {
        panic!("SYNTH-BODY panic payload sentinel");
    });
    let _ = std::panic::take_hook();
    assert!(panicked.is_err());

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
    assert!(
        events.iter().any(|e| e.contains("panic in thread")),
        "the panic hook wrote its line"
    );
    for line in events
        .iter()
        .chain(texts.iter())
        .chain(messages_seen.iter())
    {
        for sentinel in MESSAGE_SENTINELS {
            assert!(!line.contains(sentinel), "{sentinel:?} in {line}");
        }
    }
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
            LABEL_SENTINEL,
        ] {
            assert!(!e.contains(forbidden), "{forbidden:?} logged in {e}");
        }
        for id in &archive_ids {
            assert!(!e.contains(id.as_str()), "an archive id was logged: {e}");
        }
        for sentinel in VIEWER_SENTINELS {
            assert!(!e.contains(sentinel), "{sentinel:?} logged in {e}");
        }
        assert!(!has_40_hex(e), "a fileID-like value was logged: {e}");
        for t in &media_tokens {
            assert!(!e.contains(t.as_str()), "a media token was logged: {e}");
        }
        assert!(!e.contains("/v1/"), "a media URI was logged: {e}");
        assert!(!e.contains("bytes="), "a range was logged: {e}");
    }
    assert!(media_tokens.len() > 10, "the protocol was exercised");
    // No exact size (§6.4 rule 5, §13.6): every `media` event carries the
    // outcome, the status, a size bucket and the elapsed time only, and no
    // viewer-time event carries a size the protocol served or a media
    // fixture's size as a field value (statuses and elapsed times aside;
    // values under 64 could be anything).
    let buckets = ["<64KiB", "<1MiB", "<16MiB", ">=16MiB"];
    let exact: Vec<u64> = media_sizes.iter().copied().filter(|n| *n >= 64).collect();
    assert!(!exact.is_empty(), "sizes were served");
    let mut media_events = 0;
    for range in &viewer_events {
        for e in &events[range.clone()] {
            let fields = fields_of(e);
            if fields.contains(&("message", "media")) {
                media_events += 1;
                for (k, v) in &fields {
                    assert!(
                        ["message", "outcome", "status", "size", "elapsed_ms"].contains(k),
                        "a media event logged {k}: {e}"
                    );
                    if *k == "size" {
                        assert!(buckets.contains(v), "an exact size was logged: {e}");
                    }
                }
            }
            for (k, v) in &fields {
                if ["status", "elapsed_ms"].contains(k) {
                    continue;
                }
                if let Ok(n) = v.parse::<u64>() {
                    assert!(!exact.contains(&n), "an exact size was logged ({k}): {e}");
                }
            }
        }
    }
    assert!(media_events > 10, "media events were checked");
    assert!(
        events
            .iter()
            .any(|e| e.contains("media") && e.contains("decoded")),
        "the media handler logged its outcomes (so the check means something)"
    );
    // User-facing messages from errors and checks never carry device text,
    // passwords or SnapshotState values either.
    for m in &messages_seen {
        for forbidden in [PASSWORD, DEVICE_NAME, UDID, SNAPSHOT_SENTINEL] {
            assert!(!m.contains(forbidden), "{forbidden:?} in message {m}");
        }
    }
}
