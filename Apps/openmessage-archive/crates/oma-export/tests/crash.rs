//! Crash by kill during an export (M6 spec §14.2 "Crash", D144; the
//! `oma-archive` `tests/crash.rs` pattern).
//!
//! The parent re-executes this test binary with
//! `OMA_EXPORT_TEST_FAULT=<point>:<n>`. The child (`crash_child`, a no-op
//! without the variable) runs a small export into the destination the
//! parent names, through the real staging, fan-out, provenance and
//! publish; at the n-th hit of the point it prints `FAULT <point> <n>` and
//! blocks, and the parent kills it (`Child::kill`, a real kill on both
//! systems). After every kill:
//!
//! - nothing exists under a final name, unless the kill came after the
//!   publish rename;
//! - nothing exists outside the destination;
//! - every leftover matches the sweep's pattern, carries its marker (a
//!   staging folder killed before its marker existed is empty), is found
//!   by `find_leftovers` and removed by `remove_leftovers`;
//! - a published bundle verifies against its `SHA256SUMS`, which never
//!   lists the stray marker;
//! - a re-run never overwrites: it publishes under ` (2)`.
//!
//! The kill in the middle of an attachment copy runs the whole pipeline
//! (`pipeline::run` with the TXT writer) on an archive the parent imported
//! from the export catalogue's file cases; its leftover holds a partial
//! copy under its final name inside the marked staging folder, and no
//! masking sentinel of that entry.
//!
//! What this shows: consistency after a **process kill**, which leaves the
//! kernel's page cache intact; power loss is not tested.
#![allow(clippy::disallowed_methods)]

mod common;
mod files;

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use files::*;
use oma_export::fault::hooks::{arm, FaultPoint};
use oma_export::manifest::write_provenance;
use oma_export::map::ExportZone;
use oma_export::naming::{EntryKind, EntryName, TargetName, MARKER};
use oma_export::plan::Format;
use oma_export::stage::{find_leftovers, remove_leftovers, Replace, Staging};
use oma_export::writer::{AboutDraft, ConversationFile, ExportWriter, FanOut};

const ENV_FAULT: &str = "OMA_EXPORT_TEST_FAULT";
const ENV_DEST: &str = "OMA_EXPORT_TEST_DEST";
const ENV_SINGLE: &str = "OMA_EXPORT_TEST_SINGLE";
/// A library and archive id: the child runs the real pipeline on it.
const ENV_LIB: &str = "OMA_EXPORT_TEST_LIB";
const ENV_ARCHIVE: &str = "OMA_EXPORT_TEST_ARCHIVE";
const CHILD_TIMEOUT: Duration = Duration::from_secs(60);

const BUNDLE: &str = "Crash export";
const SINGLE: &str = "crash.txt";

/// Run one export into `parent`: a bundle with one conversation file, or a
/// single file.
fn export(parent: &Path, single: bool) {
    let plan = plan(single);
    let zone = ExportZone::Utc;
    let mut staging = if single {
        Staging::single_file(parent).expect("single")
    } else {
        Staging::bundle(parent).expect("bundle")
    };
    let writers: Vec<Box<dyn ExportWriter>> = vec![Box::new(LinesWriter::new(Format::Txt))];
    let mut fan = FanOut::new(writers, &plan).expect("fan-out");
    fan.begin(&ctx(&plan, &zone), &mut staging).expect("begin");
    fan.begin_conversation(ConversationFile::NoConversation, &mut staging)
        .expect("conversation");
    fan.end_conversation(&mut staging).expect("end");
    let body = body(&plan, 1);
    fan.finish(&AboutDraft { body: &body }, &mut staging)
        .expect("finish");
    if single {
        staging
            .publish(&TargetName::new(SINGLE).expect("name"), Replace::Never)
            .expect("publish");
    } else {
        write_provenance(&mut staging, &plan, &body).expect("provenance");
        staging
            .publish(&TargetName::new(BUNDLE).expect("name"), Replace::Never)
            .expect("publish");
    }
}

/// Export the whole archive `id` of the library in `lib` to a TXT bundle
/// with its files copied, through the pipeline.
fn export_archive(lib: &Path, id: &str, parent: &Path) {
    let lib = oma_archive::Library::new(lib).expect("library");
    let id = oma_archive::ArchiveId::parse(id).expect("id");
    let i = common::Imported {
        tmp: tempfile::tempdir().expect("unused"),
        lib,
        id,
    };
    common::export_txt(
        &i,
        parent,
        oma_export::plan::ScopePlan::Archive,
        &|_| {},
        BUNDLE,
        None,
    )
    .expect("export");
}

/// The child side. Does nothing unless the parent set the variables.
#[test]
fn crash_child() {
    let Ok(dest) = std::env::var(ENV_DEST) else {
        return;
    };
    let single = std::env::var(ENV_SINGLE).is_ok();
    let _armed = std::env::var(ENV_FAULT).ok().map(|spec| {
        let (name, n) = spec.split_once(':').expect("<point>:<n>");
        let point = FaultPoint::from_name(name).expect("known point");
        arm(
            point,
            n.parse().expect("n"),
            Box::new(|p, n| {
                let mut out = std::io::stdout();
                let _ = writeln!(out, "\nFAULT {} {n}", p.name());
                let _ = out.flush();
                let mut line = String::new();
                let _ = std::io::stdin().read_line(&mut line);
                std::process::exit(3);
            }),
        )
    });
    match (std::env::var(ENV_LIB), std::env::var(ENV_ARCHIVE)) {
        (Ok(lib), Ok(id)) => export_archive(Path::new(&lib), &id, Path::new(&dest)),
        _ => export(Path::new(&dest), single),
    }
    println!("\nDONE");
}

#[derive(Debug, PartialEq, Eq)]
enum Ran {
    Killed,
    Finished,
}

fn run_child(dest: &Path, single: bool, fault: Option<FaultPoint>) -> Ran {
    run_child_with(dest, single, fault, None)
}

fn run_child_with(
    dest: &Path,
    single: bool,
    fault: Option<FaultPoint>,
    archive: Option<(&Path, &str)>,
) -> Ran {
    let exe = std::env::current_exe().expect("test binary");
    let mut cmd = Command::new(exe);
    cmd.args([
        "crash_child",
        "--exact",
        "--nocapture",
        "--test-threads",
        "1",
    ])
    .env(ENV_DEST, dest)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
    if single {
        cmd.env(ENV_SINGLE, "1");
    }
    if let Some((lib, id)) = archive {
        cmd.env(ENV_LIB, lib).env(ENV_ARCHIVE, id);
    }
    if let Some(p) = fault {
        cmd.env(ENV_FAULT, format!("{}:1", p.name()));
    }
    let mut child = cmd.spawn().expect("spawn");
    let stdout = child.stdout.take().expect("stdout");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut done = false;
    loop {
        match rx.recv_timeout(CHILD_TIMEOUT) {
            Ok(line) if line.starts_with("FAULT ") => {
                child.kill().expect("kill");
                child.wait().expect("wait");
                return Ran::Killed;
            }
            Ok(line) => done |= line == "DONE",
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("the child neither reached the point nor finished");
            }
        }
    }
    let status = child.wait().expect("wait");
    assert!(status.success() && done, "the child failed: {status:?}");
    Ran::Finished
}

const BUNDLE_POINTS: [FaultPoint; 6] = [
    FaultPoint::StagingCreated,
    FaultPoint::FirstFileStaged,
    FaultPoint::BeforeReadme,
    FaultPoint::AfterSums,
    FaultPoint::PublishedBeforeDirFlush,
    FaultPoint::PublishedBeforeMarkerRemoval,
];
const SINGLE_POINTS: [FaultPoint; 1] = [FaultPoint::SingleFileStaged];
const PIPELINE_POINTS: [FaultPoint; 1] = [FaultPoint::MidAttachmentCopy];

#[test]
fn every_fault_point_has_a_kill() {
    let mut all: Vec<FaultPoint> = BUNDLE_POINTS
        .iter()
        .chain(&SINGLE_POINTS)
        .chain(&PIPELINE_POINTS)
        .copied()
        .collect();
    all.sort();
    let mut want = FaultPoint::ALL.to_vec();
    want.sort();
    assert_eq!(all, want);
}

fn top(parent: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(parent)
        .expect("dest")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn check_after_kill(d: &Dest, point: FaultPoint, final_name: &str, single: bool) {
    // Nothing outside the destination.
    let beside: Vec<String> = top(d.tmp.path());
    assert_eq!(beside, ["dest"], "{point:?}");
    let entries = top(&d.parent);
    let published = entries.iter().any(|e| e == final_name);
    assert_eq!(published, point.after_publish(), "{point:?}: {entries:?}");
    // Every other entry is a leftover of ours.
    let leftovers: Vec<EntryName> = entries
        .iter()
        .filter(|e| *e != final_name)
        .map(|e| EntryName::parse(e).unwrap_or_else(|| panic!("{point:?}: stray {e}")))
        .collect();
    if point.after_publish() {
        assert!(leftovers.is_empty(), "{point:?}");
        let bundle = d.parent.join(final_name);
        let sums = verify_sums(&bundle);
        assert!(!sums.contains_key(MARKER));
        // The stray marker is still there: it is harmless and unlisted.
        assert!(bundle.join(MARKER).is_file(), "{point:?}");
    } else {
        assert_eq!(leftovers.len(), 1, "{point:?}");
        let kind = leftovers[0].kind();
        let want = if single {
            EntryKind::PartialFile
        } else {
            EntryKind::PartialDir
        };
        assert_eq!(kind, want, "{point:?}");
        let path = d.parent.join(leftovers[0].to_name());
        if kind == EntryKind::PartialDir {
            let marked = path.join(MARKER).is_file();
            if point == FaultPoint::StagingCreated {
                assert!(!marked && tree(&path).is_empty(), "{point:?}");
            } else {
                assert!(marked, "{point:?}");
            }
        }
        // The dead child holds no lock: the sweep finds and removes it.
        let scan = find_leftovers(&d.parent, Duration::ZERO).expect("scan");
        assert_eq!(scan.found.len(), 1, "{point:?}: {scan:?}");
        assert_eq!(scan.found[0].name, leftovers[0]);
        let report = remove_leftovers(&d.parent, &scan.found);
        assert_eq!(report.removed, 1, "{point:?}");
        assert!(top(&d.parent).is_empty(), "{point:?}");
    }
}

#[test]
fn a_kill_at_every_bundle_point_leaves_only_sweepable_leftovers() {
    for point in BUNDLE_POINTS {
        let d = dest();
        assert_eq!(
            run_child(&d.parent, false, Some(point)),
            Ran::Killed,
            "{point:?}"
        );
        check_after_kill(&d, point, BUNDLE, false);
        // A re-run never overwrites.
        assert_eq!(run_child(&d.parent, false, None), Ran::Finished);
        let entries = top(&d.parent);
        if point.after_publish() {
            assert_eq!(entries, [BUNDLE, &format!("{BUNDLE} (2)")], "{point:?}");
        } else {
            assert_eq!(entries, [BUNDLE], "{point:?}");
        }
        for e in &entries {
            verify_sums(&d.parent.join(e));
            // Only a bundle published before the kill keeps the stray
            // marker; the re-run's own is removed.
            let stray = point.after_publish() && e == BUNDLE;
            assert_eq!(
                d.parent.join(e).join(MARKER).exists(),
                stray,
                "{point:?} {e}"
            );
        }
    }
}

#[test]
fn a_kill_before_a_single_file_is_placed_leaves_a_sweepable_partial_file() {
    for point in SINGLE_POINTS {
        let d = dest();
        assert_eq!(run_child(&d.parent, true, Some(point)), Ran::Killed);
        check_after_kill(&d, point, SINGLE, true);
        assert_eq!(run_child(&d.parent, true, None), Ran::Finished);
        assert_eq!(top(&d.parent), [SINGLE]);
        // A second run refuses rather than overwrite.
        let mut s = Staging::single_file(&d.parent).expect("single");
        let mut f = s
            .create(&oma_export::naming::BundlePath::readme())
            .expect("create");
        f.write_all(b"other").expect("write");
        s.close(f).expect("close");
        assert!(matches!(
            s.publish(&TargetName::new(SINGLE).expect("name"), Replace::Never),
            Err(oma_export::ExportError::TargetExists)
        ));
        assert!(s.remove());
        assert_eq!(top(&d.parent), [SINGLE]);
    }
}

#[test]
fn a_kill_in_the_middle_of_an_attachment_copy_leaves_a_sweepable_folder() {
    let doc = common::docs()
        .into_iter()
        .find(|d| d.name == "export-files-plain")
        .expect("export-files-plain");
    let e = doc.export.clone().expect("export.json");
    let lib = tempfile::tempdir().expect("library");
    let (l, id) = common::import_in(&doc, lib.path());
    drop(l);
    let archive = Some((lib.path(), id.as_str()));
    for point in PIPELINE_POINTS {
        let d = dest();
        assert_eq!(
            run_child_with(&d.parent, false, Some(point), archive),
            Ran::Killed,
            "{point:?}"
        );
        // The leftover holds the partial copy, under its final name in
        // the shard, and nothing masked.
        let entries = top(&d.parent);
        assert_eq!(entries.len(), 1, "{entries:?}");
        let left = d.parent.join(&entries[0]);
        let inside = tree(&left);
        assert!(
            inside.iter().any(|p| p.starts_with("attachments/000/0000")),
            "{inside:?}"
        );
        assert!(!inside.iter().any(|p| p == "SHA256SUMS"), "{inside:?}");
        let bytes: Vec<u8> = inside
            .iter()
            .filter(|p| !p.ends_with('/'))
            .flat_map(|p| std::fs::read(common::under(&left, p)).expect("read"))
            .collect();
        for s in e["sentinels"]["unsentAttachmentFiles"]
            .as_array()
            .expect("sentinels")
        {
            let s = s.as_str().expect("str");
            assert!(!bytes.windows(s.len()).any(|w| w == s.as_bytes()), "{s}");
        }
        check_after_kill(&d, point, BUNDLE, false);
        assert_eq!(
            run_child_with(&d.parent, false, None, archive),
            Ran::Finished
        );
        assert_eq!(top(&d.parent), [BUNDLE]);
        verify_sums(&d.parent.join(BUNDLE));
    }
}
