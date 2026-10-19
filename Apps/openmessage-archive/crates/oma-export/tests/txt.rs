//! The TXT writer and the pipeline end to end (M6 spec §5.4, §4.3, §14.2
//! "Per format: TXT"): every catalogue entry is imported, exported to TXT
//! through `pipeline::run` (real staging, attachment copies, provenance
//! and publish), and every file is parsed back with the strict grammar
//! parser in `tests/txtgrammar`.
//!
//! - every file follows the grammar: item lines at column 0, labels
//!   indented by four spaces, recorded text by six; no raw C0/C1 control,
//!   DEL, CR (in an LF file), U+2028 or U+2029; every `⟨` one of ours;
//!   no hidden character on an item or label line, nor anywhere with the
//!   markers on;
//! - the footer's counts equal the parsed items, and the sums of every
//!   file equal the manifest's `items` and `foldedReactionRows`; every
//!   item's number is unique per file and the per-file items plus folded
//!   rows equal the pre-count;
//! - header forgery through the conversation name and the sender (LF, CR,
//!   U+2028, U+0085, bidi) is written as markers on one line; a body's
//!   CRLF becomes two indented lines;
//! - masking: no masking sentinel of the export catalogue appears in any
//!   byte of the bundle unless the export reveals it, and then it does;
//! - the options: CRLF line ends, the 12-hour clock without seconds,
//!   markers off (hidden characters raw in recorded text only), a single
//!   file with "About this export".
#![allow(clippy::disallowed_methods)]

mod common;
mod files;
mod txtgrammar;

use std::collections::BTreeMap;
use std::path::Path;

use common::*;
use oma_archive::CancelToken;
use oma_export::pipeline::{self, ExportReport, ExportRun};
use oma_export::plan::{
    AttachmentMode, Clock, ExportOptions, ExportPlan, Format, LineEndings, RevealPlan, ScopePlan,
};
use oma_export::stage::Replace;
use oma_export::writer::txt::TxtWriter;
use oma_export::{EntryId, ExportError, ExportZone, TargetName};
use serde_json::Value;

const BUNDLE: &str = "Messages export";

fn imported(doc: &Doc) -> Imported {
    let i = import(doc);
    if let Some(e) = &doc.export {
        apply_patches(&i.db(), e["patches"].as_array().expect("patches"));
    }
    i
}

/// Export `i` to TXT under `scope` and `tweak`ed defaults into `parent`.
fn export_into(
    i: &Imported,
    parent: &Path,
    scope: ScopePlan,
    tweak: &dyn Fn(&mut ExportOptions),
    target: &str,
) -> Result<ExportReport, ExportError> {
    export_txt(i, parent, scope, tweak, target, None)
}

/// Every TXT file of a bundle, by its `/` path, parsed.
fn parse_bundle(bundle: &Path, nl: &str, markers: bool) -> BTreeMap<String, txtgrammar::File> {
    let mut out = BTreeMap::new();
    let dir = bundle.join("txt");
    for e in std::fs::read_dir(&dir).expect("txt folder").flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let text = String::from_utf8(std::fs::read(e.path()).expect("read")).expect("UTF-8");
        assert!(!text.starts_with('\u{FEFF}'), "{name}: BOM");
        let f = txtgrammar::parse(&text, nl, markers).unwrap_or_else(|e| {
            if let Some(dump) = std::env::var_os("OMA_TXT_DUMP") {
                let _ = std::fs::write(dump, &text);
            }
            panic!("{name}: {e}")
        });
        out.insert(format!("txt/{name}"), f);
    }
    out
}

/// Every byte of every file below `root`, then the same text with every
/// `⟨U+XXXX⟩` marker turned back into its character (so a sentinel holding
/// a marked character is found too).
fn all_bytes(root: &Path) -> Vec<u8> {
    let mut out = Vec::new();
    for rel in files::tree(root) {
        if rel.ends_with('/') {
            continue;
        }
        out.extend(std::fs::read(under(root, &rel)).expect("read"));
        out.push(b'\n');
    }
    let unmarked = txtgrammar::unmark(&String::from_utf8_lossy(&out));
    out.extend(unmarked.as_bytes());
    out
}

fn contains(hay: &[u8], needle: &str) -> bool {
    hay.windows(needle.len()).any(|w| w == needle.as_bytes())
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn manifest(bundle: &Path) -> Value {
    json(&bundle.join("manifest.json"))
}

#[test]
fn every_catalogue_entry_exports_to_txt_that_follows_the_grammar() {
    let mut exported = 0;
    for doc in docs() {
        let what = doc.name.clone();
        let i = imported(&doc);
        let d = files::dest();
        let report = export_into(&i, &d.parent, ScopePlan::Archive, &|_| {}, BUNDLE)
            .unwrap_or_else(|e| panic!("{what}: {e:?}"));
        let bundle = d.parent.join(BUNDLE);
        assert_eq!(
            files::tree(d.tmp.path()).first().map(String::as_str),
            Some("dest/")
        );
        assert_eq!(
            std::fs::read_dir(&d.parent).expect("dest").count(),
            1,
            "{what}"
        );
        files::verify_sums(&bundle);
        let parsed = parse_bundle(&bundle, "\n", true);
        let (mut items, mut folded) = (0, 0);
        let mut convs = Vec::new();
        for (path, f) in &parsed {
            items += f.footer_items;
            folded += f.footer_folded;
            let mut ns: Vec<u64> = f.items.iter().map(|i| i.n).collect();
            ns.sort_unstable();
            ns.dedup();
            assert_eq!(ns.len(), f.items.len(), "{what} {path}: an item twice");
            assert_eq!(f.zone, "UTC");
            if let Some(n) = f.conversation {
                assert_eq!(path, &format!("txt/c{n:04}.txt"), "{what}");
                convs.push(n);
            } else {
                assert_eq!(path, "txt/no-conversation.txt", "{what}");
            }
        }
        // Conversation ordinals are dense from 1.
        convs.sort_unstable();
        assert_eq!(
            convs,
            (1..=convs.len() as u64).collect::<Vec<_>>(),
            "{what}"
        );
        let m = manifest(&bundle);
        assert_eq!(m["counts"]["items"], items, "{what}");
        assert_eq!(m["counts"]["foldedReactionRows"], folded, "{what}");
        assert_eq!(report.counts.items, items, "{what}");
        assert_eq!(m["formats"], serde_json::json!(["txt"]));
        // Masking: nothing withheld by default reaches any byte.
        if let Some(e) = &doc.export {
            let bytes = all_bytes(&bundle);
            for k in [
                "oneTimeCodes",
                "earlierText",
                "editText",
                "subjectHistory",
                "alternativeText",
                "unsentAttachmentFiles",
            ] {
                for s in strings(&e["sentinels"][k]) {
                    assert!(!contains(&bytes, &s), "{what}: {s} leaked");
                }
            }
            for s in strings(&e["visible"]) {
                assert!(contains(&bytes, &s), "{what}: {s} not exported");
            }
        }
        exported += 1;
    }
    assert!(exported >= 20, "{exported}");
}

#[test]
fn revealed_content_is_exported_only_with_the_reveal() {
    let mut checked = 0;
    for doc in docs().into_iter().filter(|d| d.export.is_some()) {
        let e = doc.export.as_ref().expect("export");
        let sentinels: Vec<String> = [
            "oneTimeCodes",
            "earlierText",
            "editText",
            "subjectHistory",
            "alternativeText",
            "unsentAttachmentFiles",
        ]
        .iter()
        .flat_map(|k| strings(&e["sentinels"][k]))
        .collect();
        if sentinels.is_empty() {
            continue;
        }
        let i = imported(&doc);
        let d = files::dest();
        let reveal = |o: &mut ExportOptions| {
            o.history = true;
            o.reveal = RevealPlan {
                one_time_codes: true,
                earlier_content: true,
                confirmed_up_to: Some(u64::MAX),
            };
        };
        export_into(&i, &d.parent, ScopePlan::Archive, &reveal, BUNDLE).expect("export");
        let bundle = d.parent.join(BUNDLE);
        parse_bundle(&bundle, "\n", true);
        let bytes = all_bytes(&bundle);
        for s in &sentinels {
            assert!(contains(&bytes, s), "{}: {s} not revealed", doc.name);
            checked += 1;
        }
        let m = manifest(&bundle);
        assert_eq!(m["options"]["reveal"]["earlierContent"], true);
    }
    assert!(checked >= 5, "{checked}");
}

fn doc(name: &str) -> Doc {
    docs()
        .into_iter()
        .find(|d| d.name == name)
        .unwrap_or_else(|| panic!("{name}"))
}

/// The hostile entry's file whose name line names `marker`.
fn file_named<'a>(
    parsed: &'a BTreeMap<String, txtgrammar::File>,
    marker: &str,
) -> &'a txtgrammar::File {
    parsed
        .values()
        .find(|f| f.name_line.contains(marker))
        .unwrap_or_else(|| panic!("no file named {marker}"))
}

#[test]
fn header_fields_cannot_break_a_line_or_forge_an_item() {
    let doc = doc("export-text-plain");
    let e = doc.export.clone().expect("export");
    let i = imported(&doc);
    let d = files::dest();
    export_into(&i, &d.parent, ScopePlan::Archive, &|_| {}, BUNDLE).expect("export");
    let bundle = d.parent.join(BUNDLE);
    let parsed = parse_bundle(&bundle, "\n", true);
    // The conversation name with U+2028, U+0085, U+2029 and U+202E stays
    // on its line, every one of them a marker.
    let f = file_named(&parsed, "SYNTH-EXP-NAME");
    assert!(
        f.name_line.ends_with(
            ": SYNTH-EXP-NAME first\u{27E8}U+2028\u{27E9}second\u{27E8}U+0085\u{27E9}third\
             \u{27E8}U+2029\u{27E9}fourth \u{27E8}U+202E\u{27E9}evil"
        ),
        "{}",
        f.name_line
    );
    // Hostile addresses as senders: one line each, markers, no bracket.
    let senders: Vec<String> = parsed
        .values()
        .flat_map(|f| f.items.iter().filter_map(|i| i.sender.clone()))
        .collect();
    assert!(senders
        .iter()
        .any(|s| s == "synthetic-export-6\u{27E8}U+2028\u{27E9}line@example.invalid"));
    assert!(senders.iter().any(|s| s
        == "synthetic-export-7\u{27E8}U+0085\u{27E9}next\u{27E8}U+202E\u{27E9}@example.invalid"));
    // Every hostile body is in some item's text, unmarked back exactly
    // (CRLF and lone CR become line breaks).
    let texts: Vec<String> = parsed
        .values()
        .flat_map(|f| f.items.iter())
        .filter_map(|i| i.block("Text:"))
        .map(|lines| {
            lines
                .iter()
                .map(|l| txtgrammar::unmark(l))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect();
    let mut bodies = 0;
    for h in e["hostile"].as_array().expect("hostile") {
        if h["where"] != "body" {
            continue;
        }
        let Some(want) = h["text"].as_str() else {
            continue; // a long text the catalogue states by its digest
        };
        let want = want
            .replace("\r\n", "\n")
            .replace(['\r', '\u{2028}', '\u{2029}', '\u{85}'], "\n");
        // A combining flood is cut to 16 marks and a count (§4.7).
        let flood = texts.iter().any(|t| {
            t.contains(" combining marks\u{27E9}")
                && t.chars()
                    .filter(|c| !oma_export::hidden::is_mark(*c))
                    .count()
                    <= want
                        .chars()
                        .filter(|c| !oma_export::hidden::is_mark(*c))
                        .count()
                        + 32
        });
        assert!(texts.contains(&want) || flood, "{}: not found", h["id"]);
        bodies += 1;
    }
    assert!(bodies >= 3, "{bodies}");
    // A body that pretends to start an item is indented text.
    let crlf = texts
        .iter()
        .find(|t| t.starts_with("SYNTH-EXP-Q-CRLF first"))
        .expect("crlf body");
    assert_eq!(crlf, "SYNTH-EXP-Q-CRLF first\nsecond");
}

#[test]
fn the_options_change_line_ends_clock_and_markers_only() {
    let doc = doc("export-text-plain");
    let i = imported(&doc);
    let d = files::dest();
    let tweak = |o: &mut ExportOptions| {
        o.line_endings = LineEndings::Crlf;
        o.clock = Clock::H12;
        o.seconds = false;
        o.hidden_character_markers = false;
        o.attachments = AttachmentMode::None;
    };
    export_into(&i, &d.parent, ScopePlan::Archive, &tweak, BUNDLE).expect("export");
    let bundle = d.parent.join(BUNDLE);
    let parsed = parse_bundle(&bundle, "\r\n", false);
    let times: Vec<&String> = parsed
        .values()
        .flat_map(|f| f.items.iter().filter_map(|i| i.time.as_ref()))
        .collect();
    assert!(!times.is_empty());
    for t in &times {
        assert!(
            (t.contains(" AM ") || t.contains(" PM ")) && t.matches(':').count() == 2,
            "{t}"
        );
    }
    // Markers off: a hidden character in a body is raw; on a header line
    // it is still a marker (the parser refuses a raw one there).
    let raw = parsed.values().flat_map(|f| f.items.iter()).any(|i| {
        i.labels
            .iter()
            .flat_map(|(_, c)| c)
            .any(|l| txtgrammar::has_hidden(l))
    });
    assert!(raw, "no raw hidden character with markers off");
    assert_eq!(
        manifest(&bundle)["options"]["hiddenCharacterMarkers"],
        false
    );
}

#[test]
fn one_conversation_in_one_format_is_a_single_file_with_its_about() {
    let doc = doc("view-main-plain");
    let i = imported(&doc);
    let chat = with_snapshot(&i, |s| Ok(s.chat_ids()?[0]));
    let d = files::dest();
    let scope = ScopePlan::Conversation { chat };
    let r = export_into(&i, &d.parent, scope, &|_| {}, "conversation.txt").expect("export");
    assert_eq!(r.published.name.as_str(), "conversation.txt");
    assert_eq!(files::tree(&d.parent), ["conversation.txt"]);
    let text = std::fs::read_to_string(d.parent.join("conversation.txt")).expect("read");
    let f = txtgrammar::parse(&text, "\n", true).expect("grammar");
    assert_eq!(f.conversation, Some(1));
    assert!(f.footer_items > 0);
    let about = f
        .tail
        .iter()
        .position(|l| l == "==== About this export ====")
        .expect("about");
    let about: Vec<&String> = f.tail[about..].iter().collect();
    assert!(about
        .iter()
        .any(|l| l.as_str() == "scope.kind: conversation"));
    assert!(about
        .iter()
        .any(|l| l.as_str() == "options.attachments: namesOnly"));
    assert!(about
        .iter()
        .any(|l| l.as_str() == format!("counts.items: {}", f.footer_items)));
    // Names only: no file is copied, and the records say so.
    assert_eq!(r.attachments.files_written, 0);
    assert!(!text.contains("attachments/"));
}

#[test]
fn a_dated_range_exports_only_its_days() {
    let doc = doc("view-main-plain");
    let i = imported(&doc);
    let d = files::dest();
    let all = export_into(&i, &d.parent, ScopePlan::Archive, &|_| {}, BUNDLE).expect("all");
    let parsed = parse_bundle(&d.parent.join(BUNDLE), "\n", true);
    let days: Vec<String> = parsed.values().flat_map(|f| f.days.clone()).collect();
    let first = days.iter().min().expect("a day").clone();
    let parts: Vec<i64> = first
        .split('-')
        .map(|x| x.parse().expect("digit"))
        .collect();
    let from = oma_export::CivilDate::new(parts[0], parts[1] as u8, parts[2] as u8).expect("date");
    let until = oma_export::CivilDate::from_days(from.days_from_epoch() + 1);
    let d2 = files::dest();
    let scope = ScopePlan::DateRange {
        from,
        until,
        chat: None,
    };
    let r = export_into(&i, &d2.parent, scope, &|_| {}, BUNDLE).expect("range");
    let parsed = parse_bundle(&d2.parent.join(BUNDLE), "\n", true);
    for f in parsed.values() {
        assert!(!f.undated_section);
        assert!(f.days.iter().all(|x| *x == first), "{:?}", f.days);
    }
    assert!(r.counts.items > 0 && r.counts.items < all.counts.items);
    let m = manifest(&d2.parent.join(BUNDLE));
    assert_eq!(m["scope"]["kind"], "dateRange");
    assert_eq!(m["scope"]["fromLocalDate"], first);
}

/// `parent` holds nothing but `want`.
fn only(parent: &Path, want: &[&str]) {
    let mut got: Vec<String> = std::fs::read_dir(parent)
        .expect("dest")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    got.sort();
    assert_eq!(got, want);
}

#[test]
fn a_cancelled_export_leaves_nothing() {
    let doc = doc("view-main-plain");
    let i = imported(&doc);
    let d = files::dest();
    let plan_scope = ScopePlan::Archive;
    let mut o = ExportOptions::defaults(&plan_scope, &[Format::Txt]);
    o.attachments = AttachmentMode::None;
    let plan = ExportPlan::new(plan_scope, vec![Format::Txt], o).expect("plan");
    let zone = ExportZone::Utc;
    let target = TargetName::new(BUNDLE).expect("name");
    let cancel = CancelToken::new();
    let r = i.lib.open_read(&i.id).expect("reader");
    let hold = i.lib.hold_for_export(&i.id).expect("hold");
    let got = r.with_export_snapshot(&hold, &CancelToken::new(), |snap| {
        let scope = pipeline::scope_ids(&plan, &zone)?.expect("scope");
        let run = ExportRun {
            plan: &plan,
            zone: &zone,
            scope: &scope,
            export_id: EntryId::from_bytes([1; 16]),
            created_utc_ns: MODEL_NOW_NS,
            app_version: "0.0.0-test",
            parent: &d.parent,
            target: &target,
            replace: Replace::Never,
            attach: None,
            search: None,
            previews: None,
        };
        let c = cancel.clone();
        let mut seen = 0;
        pipeline::run(
            snap,
            &run,
            vec![Box::new(TxtWriter::new())],
            &cancel,
            &mut |p| {
                seen += 1;
                if p.rows > 0 {
                    c.cancel();
                }
            },
        )
    });
    assert!(matches!(got, Err(ExportError::Cancelled)), "{got:?}");
    only(&d.parent, &[]);
}

/// Writes, for the independent TXT oracle (`scripts/oracles/txt_grammar.py`,
/// Linux CI), one library per entry and scenario of both catalogues below
/// `OMA_ORACLE_TXT_OUT` (empty or absent), each with its archive and a
/// TXT bundle of the whole archive (files copied), plus `txt-oracle.json`
/// listing them.
#[test]
#[ignore = "writes the TXT oracle's inputs; run by CI's oracle step"]
fn write_oracle_txt() {
    let out = std::path::PathBuf::from(std::env::var("OMA_ORACLE_TXT_OUT").expect("set it"));
    std::fs::create_dir_all(&out).expect("create");
    assert!(
        std::fs::read_dir(&out).expect("read").next().is_none(),
        "OMA_ORACLE_TXT_OUT must be empty"
    );
    let out = dunce::canonicalize(&out).expect("canonical");
    let mut listed = Vec::new();
    for doc in docs() {
        let lib_dir = out.join(&doc.name).join("library");
        let dest = out.join(&doc.name).join("export");
        std::fs::create_dir_all(&lib_dir).expect("library dir");
        std::fs::create_dir_all(&dest).expect("export dir");
        let (lib, id) = import_in(&doc, &lib_dir);
        let db = dunce::canonicalize(&lib_dir)
            .expect("canonical")
            .join(id.as_str())
            .join("archive.sqlite");
        if let Some(e) = &doc.export {
            apply_patches(&db, e["patches"].as_array().expect("patches"));
        }
        let i = Imported {
            tmp: tempfile::tempdir().expect("unused"),
            lib,
            id,
        };
        export_into(&i, &dest, ScopePlan::Archive, &|_| {}, BUNDLE).expect("export");
        let export = doc.export.as_ref().map(|_| match doc.name.as_str() {
            "history" => "scenarios/history.export.json".to_owned(),
            n => format!("{n}.expected/export.json"),
        });
        listed.push(serde_json::json!({
            "name": doc.name,
            "catalogue": doc.catalogue,
            "archive": format!("{}/library/{}", doc.name, i.id.as_str()),
            "bundle": format!("{}/export/{BUNDLE}", doc.name),
            "export": export,
        }));
    }
    let doc =
        serde_json::json!({"format": "oma-oracle-txt", "formatVersion": 1, "exports": listed});
    std::fs::write(
        out.join("txt-oracle.json"),
        serde_json::to_vec_pretty(&doc).expect("json"),
    )
    .expect("write");
}
