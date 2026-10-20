//! Attachment copies (M6 spec §6, §14.2 "Attachments"; owner Q3, D134),
//! end to end through `pipeline::run` on the export catalogue's file
//! cases (`export-files-plain`):
//!
//! - copies are byte-identical to the store, and the digest in
//!   `manifest.json` and `SHA256SUMS` equals the stored digest (taken from
//!   the verified read: the copy is hashed once);
//! - a corrupted store file is excluded as `checksumMismatch` and leaves
//!   no file;
//! - a blob shared by two attachments is written once; both records keep
//!   their own names and point at the same file;
//! - names: case and NFC/NFD collisions, Windows reserved names, over 255
//!   bytes, trailing dots and spaces, invisible characters, `U+202E gpj.exe`,
//!   traversal and separators, unknown and mismatched types (`.blocked`),
//!   allowlisted types kept, an empty name (the sniffed extension);
//! - "Keep original file extensions" turns `.blocked` off;
//! - attachments of unsent parts are withheld by default and copied with
//!   the reveal;
//! - a volume limit excludes larger files before they are read
//!   (`tooLargeForVolume`), and exFAT is never limited (unit test);
//! - a cancel in the middle of a copy leaves nothing;
//! - Linux: a staging folder swapped for a link in the middle of the
//!   copies cannot redirect a write, and the publish fails;
//! - Mark of the Web: written beside each copy on Windows only (the
//!   Windows half runs only on the CI's Windows job).
#![allow(clippy::disallowed_methods)]

mod common;
mod files;
mod txtgrammar;

use std::collections::BTreeMap;
use std::path::Path;

use common::*;
use oma_export::attach::{AttachPolicy, VolumeRule};
use oma_export::fault::hooks::{arm, FaultPoint};
use oma_export::plan::{AttachmentMode, ExportOptions, RevealPlan, ScopePlan};
use oma_export::ExportError;
use serde_json::Value;

const BUNDLE: &str = "Messages export";

fn files_doc() -> Doc {
    docs()
        .into_iter()
        .find(|d| d.name == "export-files-plain")
        .expect("export-files-plain")
}

/// One attachment record as the TXT writes it.
#[derive(Debug, Clone)]
struct Att {
    outcome: String,
    path: Option<String>,
    content: Option<String>,
}

const BLOCKED: &str = "File copied with \".blocked\" added to its name";
const SAME: &str = "Same file as another attachment in this export";

/// Every attachment record of a TXT bundle, by its (unmarked) name.
fn attachments(bundle: &Path) -> BTreeMap<String, Vec<Att>> {
    let mut out: BTreeMap<String, Vec<Att>> = BTreeMap::new();
    for e in std::fs::read_dir(bundle.join("txt"))
        .expect("txt")
        .flatten()
    {
        let text = std::fs::read_to_string(e.path()).expect("read");
        let f = txtgrammar::parse(&text, "\n", true).expect("grammar");
        for item in f.items {
            for (label, lines) in item.labels {
                let Some(rest) = label.strip_prefix("Attachment ") else {
                    continue;
                };
                let (_, name) = rest.split_once(": ").expect("name");
                let name = txtgrammar::unmark(name);
                let first = lines.first().expect("outcome");
                let (outcome, path) = match first.split_once(": attachments/") {
                    Some((o, p)) => (o.to_owned(), Some(format!("attachments/{p}"))),
                    None => (first.clone(), None),
                };
                let content = lines
                    .iter()
                    .find_map(|l| l.strip_prefix("Content: ").map(str::to_owned));
                out.entry(name).or_default().push(Att {
                    outcome,
                    path,
                    content,
                });
            }
        }
    }
    out
}

fn sha256(path: &Path) -> String {
    files::sha256_hex(&std::fs::read(path).expect("read"))
}

/// The name a case's record is written under in TXT.
fn case_name(c: &Value) -> String {
    match c["transferName"]["text"].as_str() {
        Some("") => "an empty name recorded".to_owned(),
        Some(t) => t.to_owned(),
        None => "no name recorded (Not recorded (NULL))".to_owned(),
    }
}

/// The one record of the case named `name`.
fn one<'a>(atts: &'a BTreeMap<String, Vec<Att>>, name: &str) -> &'a Att {
    let v = atts
        .get(name)
        .unwrap_or_else(|| panic!("no record named {name:?}"));
    assert_eq!(v.len(), 1, "{name:?}");
    &v[0]
}

#[test]
fn files_are_copied_once_hashed_once_named_safely_and_defused() {
    let doc = files_doc();
    let e = doc.export.clone().expect("export.json");
    let i = import(&doc);
    // The test corrupts its store file after the import.
    let corrupt_case = &e["corruptStoreFiles"][0];
    corrupt(&store_file(
        &i,
        corrupt_case["sha256"].as_str().expect("sha"),
    ));
    let d = files::dest();
    let report =
        export_txt(&i, &d.parent, ScopePlan::Archive, &|_| {}, BUNDLE, None).expect("export");
    let bundle = d.parent.join(BUNDLE);
    let sums = files::verify_sums(&bundle);
    let manifest = json(&bundle.join("manifest.json"));
    let listed: BTreeMap<String, String> = manifest["files"]
        .as_array()
        .expect("files")
        .iter()
        .map(|f| {
            (
                f["path"].as_str().expect("path").to_owned(),
                f["sha256"].as_str().expect("sha").to_owned(),
            )
        })
        .collect();
    let atts = attachments(&bundle);
    let mut checked = 0;
    for c in e["fileCases"].as_array().expect("cases") {
        let id = c["id"].as_str().expect("id");
        let name = case_name(c);
        let a = one(&atts, &name);
        if id == "corrupt" {
            assert_eq!(
                a.outcome,
                "File not included: the stored file no longer matches its checksum"
            );
            assert!(a.path.is_none());
            continue;
        }
        if id == "shared-two" {
            let first = one(&atts, "SYNTH-EXP-shared-one.png");
            assert_eq!(a.outcome, SAME, "{id}");
            assert_eq!(a.path, first.path, "{id}: one file for one blob");
            continue;
        }
        let path = a.path.clone().unwrap_or_else(|| panic!("{id}: no file"));
        let file = under(&bundle, &path);
        // Byte-identical, and listed with the stored digest.
        let want = c["sha256"].as_str().expect("sha");
        assert_eq!(sha256(&file), want, "{id}");
        assert_eq!(listed.get(&path).map(String::as_str), Some(want), "{id}");
        assert_eq!(sums.get(&path).map(String::as_str), Some(want), "{id}");
        assert_eq!(
            std::fs::metadata(&file).expect("meta").len(),
            c["bytes"].as_u64().expect("bytes"),
            "{id}"
        );
        // Every file is directly in its shard, under the ordinal prefix.
        let parts: Vec<&str> = path.split('/').collect();
        assert_eq!(parts.len(), 3, "{id}: {path}");
        assert_eq!((parts[0], parts[1]), ("attachments", "000"), "{id}");
        let file_name = parts[2];
        assert!(
            file_name.len() > 7 && file_name[..6].chars().all(|c| c.is_ascii_digit()),
            "{id}: {file_name}"
        );
        let blocked = file_name.ends_with(".blocked");
        assert_eq!(a.outcome == BLOCKED, blocked, "{id}: {a:?}");
        let want_blocked = match id {
            "allow-png" | "case-upper" | "case-lower" | "reserved-com1" | "long-256"
            | "trailing-space" | "invisible" | "formula-eq" | "formula-plus" | "formula-at"
            | "formula-minus" | "empty-name" | "null-name" | "traversal-back" | "traversal-fwd"
            | "separator" | "shared-one" => false,
            // Text has no allowed extension (the spec's table), a declared
            // extension that disagrees with the bytes, an active or
            // unknown type, no extension.
            _ => true,
        };
        assert_eq!(blocked, want_blocked, "{id}: {file_name}");
        assert!(
            !file_name
                .chars()
                .any(|c| matches!(c, '/' | '\\' | ':' | '\u{202E}' | '\u{200B}')),
            "{id}: {file_name}"
        );
        checked += 1;
    }
    assert!(checked >= 28, "{checked}");
    // Case and normalisation collisions are distinct files.
    let distinct: std::collections::BTreeSet<String> = [
        "SYNTH-EXP-Report.PDF",
        "SYNTH-EXP-report.pdf",
        "SYNTH-EXP-caf\u{e9}.txt",
        "SYNTH-EXP-cafe\u{301}.txt",
    ]
    .iter()
    .map(|n| {
        one(&atts, n)
            .path
            .clone()
            .expect("file")
            .to_ascii_lowercase()
    })
    .collect();
    assert_eq!(distinct.len(), 4);
    // A nameless attachment takes the sniffed kind's extension.
    for n in [
        "an empty name recorded",
        "no name recorded (Not recorded (NULL))",
    ] {
        let a = one(&atts, n);
        assert!(
            a.path
                .as_deref()
                .is_some_and(|p| p.ends_with("-attachment.png")),
            "{a:?}"
        );
        assert_eq!(a.content.as_deref(), Some("PNG image"));
    }
    // Reserved names and the spoof.
    for (name, file) in [
        ("CON.txt", "-_CON.txt.blocked"),
        ("COM1.png", "-_COM1.png"),
        ("SYNTH-EXP-\u{202E}gpj.exe", "-SYNTH-EXP-gpj.exe.blocked"),
    ] {
        let p = one(&atts, name).path.clone().expect("file");
        assert!(p.ends_with(file), "{name}: {p}");
    }
    // The unsent part's attachment is withheld by default.
    for u in e["unsentAttachments"].as_array().expect("unsent") {
        let a = &atts
            .values()
            .flatten()
            .find(|a| a.outcome.contains("unsent"))
            .expect("withheld record");
        assert!(a.path.is_none(), "{u}");
    }
    let bytes: Vec<u8> = files::tree(&bundle)
        .iter()
        .filter(|r| !r.ends_with('/'))
        .flat_map(|r| std::fs::read(under(&bundle, r)).expect("read"))
        .collect();
    for s in e["sentinels"]["unsentAttachmentFiles"]
        .as_array()
        .expect("sentinels")
    {
        let s = s.as_str().expect("str");
        assert!(!bytes.windows(s.len()).any(|w| w == s.as_bytes()), "{s}");
    }
    // The counts agree with the files.
    let blocked_files = files::tree(&bundle)
        .iter()
        .filter(|p| p.starts_with("attachments/") && p.ends_with(".blocked"))
        .count() as u64;
    let a = &manifest["counts"]["attachments"];
    assert_eq!(a["blocked"], blocked_files);
    assert_eq!(a["deduplicated"], 1);
    assert_eq!(a["excluded"]["checksumMismatch"], 1);
    assert_eq!(a["excluded"]["withheldUnsent"], 1);
    assert_eq!(a["motwNotWritten"], 0);
    let written = files::tree(&bundle)
        .iter()
        .filter(|p| p.starts_with("attachments/") && !p.ends_with('/'))
        .count() as u64;
    assert_eq!(a["filesWritten"], written);
    assert_eq!(report.attachments.files_written, written);
    assert_eq!(report.attachments.checksum_mismatch, 1);
    // No Mark of the Web stream off Windows; on Windows, one per copy.
    #[cfg(not(windows))]
    assert_eq!(report.attachments.motw_written, 0);
    #[cfg(windows)]
    {
        assert_eq!(report.attachments.motw_written, written);
        for p in files::tree(&bundle)
            .iter()
            .filter(|p| p.starts_with("attachments/") && !p.ends_with('/'))
        {
            let mut s = under(&bundle, p).into_os_string();
            s.push(":Zone.Identifier");
            let mark = std::fs::read(&s).expect("Zone.Identifier stream");
            assert_eq!(mark, b"[ZoneTransfer]\r\nZoneId=3\r\n", "{p}");
        }
    }
}

#[test]
fn keeping_original_extensions_turns_the_blocked_suffix_off() {
    let doc = files_doc();
    let i = import(&doc);
    let d = files::dest();
    let keep = |o: &mut ExportOptions| o.keep_extensions = true;
    export_txt(&i, &d.parent, ScopePlan::Archive, &keep, BUNDLE, None).expect("export");
    let bundle = d.parent.join(BUNDLE);
    let tree = files::tree(&bundle);
    assert!(tree.iter().any(|p| p.ends_with("-SYNTH-EXP-page.html")));
    assert!(!tree.iter().any(|p| p.ends_with(".blocked")));
    let m = json(&bundle.join("manifest.json"));
    assert_eq!(m["options"]["keepExtensions"], true);
    assert_eq!(m["counts"]["attachments"]["blocked"], 0);
}

#[test]
fn the_reveal_copies_the_attachments_of_unsent_parts() {
    let doc = files_doc();
    let e = doc.export.clone().expect("export.json");
    let i = import(&doc);
    let d = files::dest();
    let reveal = |o: &mut ExportOptions| {
        o.reveal = RevealPlan::confirmed(None, Some(u64::MAX));
    };
    export_txt(&i, &d.parent, ScopePlan::Archive, &reveal, BUNDLE, None).expect("export");
    let bundle = d.parent.join(BUNDLE);
    let want: Vec<String> = e["unsentAttachments"]
        .as_array()
        .expect("unsent")
        .iter()
        .map(|u| u["sha256"].as_str().expect("sha").to_owned())
        .collect();
    let digests: Vec<String> = files::tree(&bundle)
        .iter()
        .filter(|p| p.starts_with("attachments/") && !p.ends_with('/'))
        .map(|p| sha256(&under(&bundle, p)))
        .collect();
    for w in &want {
        assert!(digests.contains(w), "{w} not copied with the reveal");
    }
    let m = json(&bundle.join("manifest.json"));
    assert_eq!(m["counts"]["attachments"]["excluded"]["withheldUnsent"], 0);
}

#[test]
fn a_volume_limit_excludes_larger_files_before_they_are_read() {
    let doc = files_doc();
    let i = import(&doc);
    let d = files::dest();
    let bundle_name = oma_export::TargetName::new(BUNDLE).expect("name");
    let policy = AttachPolicy {
        keep_extensions: false,
        name_budget: oma_export::naming::attachment_name_budget(&d.parent, &bundle_name),
        volume: VolumeRule::MaxFileBytes(90),
        mark_of_the_web: true,
    };
    let r = export_txt(
        &i,
        &d.parent,
        ScopePlan::Archive,
        &|_| {},
        BUNDLE,
        Some(policy),
    )
    .expect("export");
    let bundle = d.parent.join(BUNDLE);
    for p in files::tree(&bundle)
        .iter()
        .filter(|p| p.starts_with("attachments/") && !p.ends_with('/'))
    {
        let len = std::fs::metadata(under(&bundle, p)).expect("meta").len();
        assert!(len <= 90, "{p}: {len}");
    }
    assert!(
        r.attachments.too_large_for_volume >= 3,
        "{:?}",
        r.attachments
    );
    let m = json(&bundle.join("manifest.json"));
    assert!(
        m["counts"]["attachments"]["excluded"]["tooLargeForVolume"]
            .as_u64()
            .expect("count")
            >= 3
    );
    let atts = attachments(&bundle);
    assert!(atts
        .values()
        .flatten()
        .any(|a| a.outcome == "File not included: too large for the destination's file system"));
}

#[test]
fn a_cancel_in_the_middle_of_a_copy_leaves_nothing() {
    let doc = files_doc();
    let i = import(&doc);
    let d = files::dest();
    let cancel = oma_archive::CancelToken::new();
    let c = cancel.clone();
    let _armed = arm(
        FaultPoint::MidAttachmentCopy,
        3,
        Box::new(move |_, _| c.cancel()),
    );
    let got = cancelled_export(&i, &d.parent, &cancel);
    assert!(matches!(got, Err(ExportError::Cancelled)), "{got:?}");
    assert!(
        files::tree(&d.parent).is_empty(),
        "{:?}",
        files::tree(&d.parent)
    );
}

/// An archive export of `i` into `parent` under `cancel`.
fn cancelled_export(
    i: &Imported,
    parent: &Path,
    cancel: &oma_archive::CancelToken,
) -> Result<oma_export::pipeline::ExportReport, ExportError> {
    use oma_export::plan::{ExportPlan, Format};
    let scope = ScopePlan::Archive;
    let o = ExportOptions::defaults(&scope, &[Format::Txt]);
    assert_eq!(o.attachments, AttachmentMode::CopyOriginals);
    let plan = ExportPlan::new(scope, vec![Format::Txt], o).expect("plan");
    let zone = oma_export::ExportZone::Utc;
    let target = oma_export::TargetName::new(BUNDLE).expect("name");
    let r = i.lib.open_read(&i.id).expect("reader");
    let hold = i.lib.hold_for_export(&i.id).expect("hold");
    r.with_export_snapshot(&hold, &oma_archive::CancelToken::new(), |snap| {
        let scope = oma_export::pipeline::scope_ids(&plan, &zone)?.expect("scope");
        let run = oma_export::pipeline::ExportRun {
            plan: &plan,
            zone: &zone,
            scope: &scope,
            export_id: oma_export::EntryId::from_bytes([3; 16]),
            created_utc_ns: MODEL_NOW_NS,
            app_version: "0.0.0-test",
            parent,
            target: &target,
            replace: oma_export::stage::Replace::Never,
            attach: None,
            search: None,
            previews: None,
        };
        oma_export::pipeline::run(
            snap,
            &run,
            vec![Box::new(oma_export::writer::txt::TxtWriter::new())],
            cancel,
            &mut |_| {},
        )
    })
}

/// D143, during the copies: the staging folder is renamed away and a link
/// to a victim folder planted under its name after the second copy began.
/// Every later copy still lands in the moved folder (writes are relative
/// to its handle), the victim stays empty, and the publish refuses with
/// `DestinationChanged`.
#[cfg(target_os = "linux")]
#[test]
fn a_staging_folder_swapped_during_the_copies_cannot_redirect_a_write() {
    let doc = files_doc();
    let i = import(&doc);
    let d = files::dest();
    let victim = d.tmp.path().join("victim");
    std::fs::create_dir(&victim).expect("victim");
    let moved = d.tmp.path().join("moved");
    let (parent, v, m) = (d.parent.clone(), victim.clone(), moved.clone());
    let _armed = arm(
        FaultPoint::MidAttachmentCopy,
        2,
        Box::new(move |_, _| {
            let staging = std::fs::read_dir(&parent)
                .expect("dest")
                .flatten()
                .map(|e| e.path())
                .find(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.ends_with(".partial"))
                })
                .expect("staging folder");
            std::fs::rename(&staging, &m).expect("rename away");
            std::os::unix::fs::symlink(&v, &staging).expect("plant link");
        }),
    );
    let got = cancelled_export(&i, &d.parent, &oma_archive::CancelToken::new());
    assert!(
        matches!(got, Err(ExportError::DestinationChanged)),
        "{got:?}"
    );
    assert!(
        files::tree(&victim).is_empty(),
        "{:?}",
        files::tree(&victim)
    );
    // The removal after the failure also went through the moved folder's
    // handle: it is emptied but kept (its name no longer matches), and
    // nothing was published.
    assert!(moved.is_dir());
    assert!(
        files::tree(&moved).iter().all(|p| p.ends_with('/')),
        "{:?}",
        files::tree(&moved)
    );
    let top: Vec<String> = std::fs::read_dir(&d.parent)
        .expect("dest")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(top.len(), 1, "only the planted link: {top:?}");
    assert!(top[0].starts_with(".oma-export-"));
}
