//! The PDF writer through the whole pipeline (M6 slice 6j2, spec §5.6,
//! §7.3, §14.2): every catalogue entry to a checked, tagged PDF with the
//! PDF backend in-process (the `render` feature; the app runs it in the
//! export-render helper, `src-tauri/tests/export_helper.rs`), the blocks
//! it sends (a recording renderer), masking in the drawn text, volumes
//! cut by the estimate, previews, the single file's "About this export",
//! and the hostile catalogue (a 1 MiB body, combining floods, bidi
//! controls).
//!
//! `write_oracle_pdf` (ignored) writes the bundles that
//! `scripts/oracles/pdf_oracle.py` reads back with pypdf and `pdftotext`.

#![allow(clippy::disallowed_methods)] // the oracle's files, reading outputs

mod common;
mod files;

use std::path::Path;
use std::sync::{Arc, Mutex};

use common::*;
use oma_export::check::{check_output, OutputKind};
use oma_export::helper::{DoneCounts, Volume};
use oma_export::pdf::doc::{Block, HeadingLevel, ParaRole, PdfOptions};
use oma_export::plan::{AttachmentMode, ExportOptions, Format, RevealPlan, ScopePlan};
use oma_export::preview::{Preview, PreviewSource, RenditionBytes};
use oma_export::record::AttachmentRecord;
use oma_export::writer::pdf::{PdfRenderer, PdfRendererFactory, PdfWriter, VolumeLimits};
use oma_export::writer::ExportWriter;
use oma_export::{ExportError, ExportZone};

const BUNDLE: &str = "Messages export";

/// A real 1 × 1 PNG (every chunk's CRC is right), as in `tests/html.rs`.
const PNG_1X1: &str = "89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c4890000000d4944415478da636460f85f0f0002870180eb47ba920000000049454e44ae426082";

fn png() -> Vec<u8> {
    (0..PNG_1X1.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&PNG_1X1[i..i + 2], 16).expect("hex"))
        .collect()
}

fn imported(doc: &Doc) -> Imported {
    let i = import(doc);
    if let Some(e) = &doc.export {
        apply_patches(&i.db(), e["patches"].as_array().expect("patches"));
    }
    i
}

fn doc(name: &str) -> Doc {
    docs()
        .into_iter()
        .find(|d| d.name == name)
        .expect("catalogue entry")
}

/// What a recording renderer was sent.
#[derive(Debug, Clone, PartialEq)]
enum Sent {
    Conversation(Block),
    Item(Block),
    Image(u64),
    End(bool),
}

/// A renderer that records the decoded blocks and returns a minimal
/// PDF for each volume (the writer stages whatever the renderer returns).
#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<Sent>>>);

impl Recorder {
    fn sent(&self) -> Vec<Sent> {
        self.0.lock().expect("lock").clone()
    }
}

impl PdfRenderer for Recorder {
    fn conversation(&mut self, block: &[u8]) -> Result<(), ExportError> {
        let b = Block::decode(block).expect("a valid block");
        self.0.lock().expect("lock").push(Sent::Conversation(b));
        Ok(())
    }
    fn item(&mut self, block: &[u8]) -> Result<(), ExportError> {
        let b = Block::decode(block).expect("a valid block");
        self.0.lock().expect("lock").push(Sent::Item(b));
        Ok(())
    }
    fn image(&mut self, n: u64, _bytes: &[u8]) -> Result<(), ExportError> {
        self.0.lock().expect("lock").push(Sent::Image(n));
        Ok(())
    }
    fn end_volume(&mut self, last: bool) -> Result<Volume, ExportError> {
        let mut sent = self.0.lock().expect("lock");
        // It "draws" every image sent since the last volume, as the helper
        // does (the writer checks the count).
        let figures = sent
            .iter()
            .rev()
            .take_while(|s| !matches!(s, Sent::End(_)))
            .filter(|s| matches!(s, Sent::Image(_)))
            .count() as u64;
        sent.push(Sent::End(last));
        Ok(Volume {
            bytes: b"%PDF-1.7\n%%EOF\n".to_vec(),
            counts: DoneCounts {
                units: 1,
                figures,
                ..DoneCounts::default()
            },
        })
    }
    fn close(self: Box<Self>) -> u64 {
        0
    }
}

impl PdfRendererFactory for Recorder {
    fn start(&mut self, options: &PdfOptions) -> Result<Box<dyn PdfRenderer>, ExportError> {
        assert_eq!(options.title, "Messages export");
        Ok(Box::new(self.clone()))
    }
}

/// A renderer that says it drew no preview, whatever it was sent.
#[derive(Clone, Default)]
struct Undercount(Recorder);

impl PdfRenderer for Undercount {
    fn conversation(&mut self, block: &[u8]) -> Result<(), ExportError> {
        self.0.conversation(block)
    }
    fn item(&mut self, block: &[u8]) -> Result<(), ExportError> {
        self.0.item(block)
    }
    fn image(&mut self, n: u64, bytes: &[u8]) -> Result<(), ExportError> {
        self.0.image(n, bytes)
    }
    fn end_volume(&mut self, last: bool) -> Result<Volume, ExportError> {
        let mut v = self.0.end_volume(last)?;
        v.counts.figures = 0;
        Ok(v)
    }
    fn close(self: Box<Self>) -> u64 {
        0
    }
}

impl PdfRendererFactory for Undercount {
    fn start(&mut self, _options: &PdfOptions) -> Result<Box<dyn PdfRenderer>, ExportError> {
        Ok(Box::new(self.clone()))
    }
}

/// Every string the blocks draw, in order.
fn texts(sent: &[Sent]) -> Vec<String> {
    let mut out = Vec::new();
    for s in sent {
        match s {
            Sent::Conversation(Block::Heading { text, .. })
            | Sent::Item(Block::Heading { text, .. }) => out.push(text.as_str().to_owned()),
            Sent::Conversation(Block::Entry { head, body })
            | Sent::Item(Block::Entry { head, body }) => {
                out.extend(head.iter().map(|h| h.as_str().to_owned()));
                out.extend(body.iter().map(|p| p.text.as_str().to_owned()));
            }
            _ => {}
        }
    }
    out
}

fn export_pdf(
    i: &Imported,
    parent: &Path,
    scope: ScopePlan,
    tweak: &dyn Fn(&mut ExportOptions),
    writer: PdfWriter,
) -> Result<oma_export::pipeline::ExportReport, ExportError> {
    export_with(
        i,
        parent,
        scope,
        &[Format::Pdf],
        tweak,
        BUNDLE,
        None,
        &ExportZone::Utc,
        vec![Box::new(writer) as Box<dyn ExportWriter>],
    )
}

fn history(o: &mut ExportOptions) {
    o.history = true;
    o.attachments = AttachmentMode::None;
}

#[test]
fn every_catalogue_entry_exports_to_checked_tagged_pdfs() {
    let mut files_seen = 0;
    for doc in docs() {
        let i = imported(&doc);
        let d = files::dest();
        let report = export_pdf(
            &i,
            &d.parent,
            ScopePlan::Archive,
            &history,
            pdf_writer(None),
        )
        .unwrap_or_else(|e| panic!("{}: {e:?}", doc.name));
        let bundle = d.parent.join(BUNDLE);
        let tree = files::tree(&bundle);
        let pdfs: Vec<&String> = tree.iter().filter(|p| p.ends_with(".pdf")).collect();
        let r = &report.writers[0];
        assert_eq!(
            pdfs.len() as u64,
            r.counts["pdfVolumes"],
            "{}: one file per volume",
            doc.name
        );
        // A volume per file, more where the estimate cut one.
        assert!(r.conversation_files <= pdfs.len() as u64, "{}", doc.name);
        if std::env::var_os("OMA_PDF_SIZES").is_some() {
            for p in &pdfs {
                let n = std::fs::metadata(under(&bundle, p)).expect("meta").len();
                println!("SIZE {} {p} {n} pages={}", doc.name, r.counts["pdfPages"]);
            }
        }
        assert!(r.counts["pdfPages"] >= r.conversation_files, "{}", doc.name);
        for p in pdfs {
            assert!(p.starts_with("pdf/"), "{p}");
            let bytes = std::fs::read(under(&bundle, p)).expect("read");
            check_output(OutputKind::Pdf, &bytes)
                .unwrap_or_else(|e| panic!("{}: {p}: {e:?}", doc.name));
            let has = |needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
            assert!(has(b"/StructTreeRoot"), "{}: tagged", doc.name);
            assert!(has(b"/MarkInfo"), "{}: marked", doc.name);
            assert!(has(b"/Outlines"), "{}: outline", doc.name);
            files_seen += 1;
        }
        // The provenance counts the writer's own facts.
        let m: serde_json::Value =
            serde_json::from_slice(&std::fs::read(bundle.join("manifest.json")).expect("manifest"))
                .expect("json");
        assert_eq!(
            m["counts"]["pdfUndrawableCharacters"],
            r.counts["pdfUndrawableCharacters"]
        );
        assert_eq!(m["counts"]["pdfForcedWraps"], r.counts["pdfForcedWraps"]);
        files::verify_sums(&bundle);
    }
    assert!(files_seen >= 30, "{files_seen} PDF files");
}

#[test]
fn the_blocks_say_what_txt_says_and_never_a_masked_value() {
    let mut scanned = 0;
    let mut revealed = 0;
    for doc in docs().into_iter().filter(|d| d.export.is_some()) {
        let i = imported(&doc);
        let export = doc.export.as_ref().expect("export");
        for reveal in [false, true] {
            let rec = Recorder::default();
            let d = files::dest();
            let tweak = |o: &mut ExportOptions| {
                history(o);
                if reveal {
                    o.reveal = RevealPlan::confirmed(Some(u64::MAX), Some(u64::MAX));
                }
            };
            export_pdf(
                &i,
                &d.parent,
                ScopePlan::Archive,
                &tweak,
                PdfWriter::new(Box::new(rec.clone()), None),
            )
            .unwrap_or_else(|e| panic!("{}: {e:?}", doc.name));
            let all = texts(&rec.sent()).concat();
            for kind in [
                "oneTimeCodes",
                "earlierText",
                "editText",
                "subjectHistory",
                "alternativeText",
                "unsentAttachmentFiles",
            ] {
                for s in export["sentinels"][kind].as_array().expect("sentinels") {
                    let s = s.as_str().expect("str");
                    if reveal {
                        // The positive control: with both reveals every
                        // text sentinel is drawn (an unsent attachment's
                        // sentinel is in its file's bytes, not its name).
                        // U+FFFC stands for an attachment, drawn as its
                        // placeholder: each side of it must be drawn.
                        if kind != "unsentAttachmentFiles" {
                            for part in s.split('\u{FFFC}') {
                                let part = part.trim();
                                assert!(
                                    all.contains(part),
                                    "{}: a {kind} sentinel not drawn",
                                    doc.name
                                );
                            }
                            revealed += 1;
                        }
                        continue;
                    }
                    assert!(!all.contains(s), "{}: a {kind} sentinel drawn", doc.name);
                    scanned += 1;
                }
            }
        }
    }
    assert!(scanned >= 14, "{scanned} sentinels scanned for");
    assert!(
        revealed >= 10,
        "{revealed} sentinels found with both reveals"
    );
}

#[test]
fn a_bundle_is_one_volume_per_conversation_file_cut_by_the_estimate() {
    let doc = doc("view-main-plain");
    let i = imported(&doc);
    let rec = Recorder::default();
    let d = files::dest();
    let small = VolumeLimits {
        pages: 1,
        bytes: 40 << 20,
    };
    let report = export_pdf(
        &i,
        &d.parent,
        ScopePlan::Archive,
        &history,
        PdfWriter::new(Box::new(rec.clone()), None).with_limits(small),
    )
    .expect("export");
    let sent = rec.sent();
    let ends: Vec<bool> = sent
        .iter()
        .filter_map(|s| match s {
            Sent::End(l) => Some(*l),
            _ => None,
        })
        .collect();
    assert!(ends.iter().all(|l| !l), "a bundle never sends End");
    // A volume never ends with a day heading (an entry's day heading is
    // sent with it, after any cut).
    for w in sent.windows(2) {
        if let [Sent::Item(Block::Heading { level, text, .. }), Sent::End(_)] = w {
            assert!(
                *level != HeadingLevel::Day || text.as_str() == "End of conversation file",
                "a volume ends with a day heading"
            );
        }
    }
    let files_ = report.writers[0].conversation_files;
    assert!(
        ends.len() as u64 > files_,
        "{} volumes for {files_} files: the 1-page limit cut some",
        ends.len()
    );
    // After a cut the conversation heading is repeated with its part, and
    // the document title says it.
    let parts: Vec<(String, String)> = sent
        .iter()
        .filter_map(|s| match s {
            Sent::Conversation(Block::Heading {
                level: HeadingLevel::Conversation,
                text,
                title: Some(t),
            }) => Some((text.as_str().to_owned(), t.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(parts.len(), ends.len());
    assert!(parts
        .iter()
        .any(|(t, title)| t.ends_with("(part 2)") && title.ends_with(", part 2")));
    assert!(parts
        .iter()
        .all(|(_, title)| title.starts_with("Messages export \u{2014} ")));
    let bundle = d.parent.join(BUNDLE);
    let tree = files::tree(&bundle);
    assert!(tree.iter().any(|p| p == "pdf/c0001.pdf"), "{tree:?}");
    assert!(tree.iter().any(|p| p.ends_with("-2.pdf")), "{tree:?}");
    assert_eq!(
        tree.iter().filter(|p| p.ends_with(".pdf")).count(),
        ends.len()
    );
    // Without the small limit: exactly one volume per file.
    let rec = Recorder::default();
    let d = files::dest();
    export_pdf(
        &i,
        &d.parent,
        ScopePlan::Archive,
        &history,
        PdfWriter::new(Box::new(rec.clone()), None),
    )
    .expect("export");
    let whole = rec.sent();
    let n = whole.iter().filter(|s| matches!(s, Sent::End(_))).count() as u64;
    assert_eq!(n, files_);
    // Every item is sent exactly once across the cut volumes, in the same
    // order as in the uncut export (review finding: nothing checked that a
    // cut neither drops nor repeats the entry at its boundary).
    let cut = item_numbers(&sent);
    let uncut = item_numbers(&whole);
    assert_eq!(cut.len() as u64, report.writers[0].items);
    assert_eq!(cut, uncut);
    let mut unique = cut.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), cut.len(), "an item was sent twice");
}

/// Each entry's item number piece (`item N`), with the conversation it is
/// in, in the order sent.
fn item_numbers(sent: &[Sent]) -> Vec<(String, String)> {
    let mut conversation = String::new();
    let mut out = Vec::new();
    for s in sent {
        match s {
            Sent::Conversation(Block::Heading {
                level: HeadingLevel::Conversation,
                text,
                ..
            }) => {
                let t = text.as_str();
                conversation = t
                    .strip_suffix(")")
                    .and_then(|t| t.rsplit_once(" (part "))
                    .map_or(t, |(c, _)| c)
                    .to_owned();
            }
            Sent::Item(Block::Entry { head, .. }) => {
                if let Some(n) = head.get(1).filter(|h| h.as_str().starts_with("item ")) {
                    out.push((conversation.clone(), n.as_str().to_owned()));
                }
            }
            _ => {}
        }
    }
    out
}

#[test]
fn a_single_file_is_never_cut_and_ends_with_about_this_export() {
    let doc = doc("view-main-plain");
    let i = imported(&doc);
    let chat = with_snapshot(&i, |s| Ok(s.chat_ids()?[0]));
    let rec = Recorder::default();
    let d = files::dest();
    let tiny = VolumeLimits { pages: 1, bytes: 1 };
    export_with(
        &i,
        &d.parent,
        ScopePlan::Conversation { chat },
        &[Format::Pdf],
        &|_| {},
        "conversation.pdf",
        None,
        &ExportZone::Utc,
        vec![Box::new(
            PdfWriter::new(Box::new(rec.clone()), None).with_limits(tiny),
        )],
    )
    .expect("export");
    let sent = rec.sent();
    let ends: Vec<&Sent> = sent.iter().filter(|s| matches!(s, Sent::End(_))).collect();
    assert_eq!(ends, [&Sent::End(true)]);
    assert_eq!(sent.last(), Some(&Sent::End(true)));
    let about = sent
        .iter()
        .position(|s| {
            matches!(s, Sent::Item(Block::Heading { level: HeadingLevel::Section, text, .. })
                if text.as_str() == "About this export")
        })
        .expect("About this export");
    let after = texts(&sent[about..]).concat();
    assert!(after.contains("exportId: "), "the manifest's fields");
    assert!(!after.contains("files:"), "never the file list");
    // One file, with the single file's name and extension.
    let names: Vec<String> = std::fs::read_dir(&d.parent)
        .expect("dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["conversation.pdf"]);
}

#[test]
fn every_item_has_its_time_number_and_tags_as_separate_pieces() {
    let doc = doc("view-main-plain");
    let i = imported(&doc);
    let rec = Recorder::default();
    let d = files::dest();
    let report = export_pdf(
        &i,
        &d.parent,
        ScopePlan::Archive,
        &history,
        PdfWriter::new(Box::new(rec.clone()), None),
    )
    .expect("export");
    let sent = rec.sent();
    let heads: Vec<&Vec<oma_export::pdf::doc::PdfText>> = sent
        .iter()
        .filter_map(|s| match s {
            Sent::Item(Block::Entry { head, .. }) if !head.is_empty() => Some(head),
            _ => None,
        })
        .collect();
    assert_eq!(heads.len() as u64, report.writers[0].items);
    for h in &heads {
        assert!(h.len() >= 2);
        assert!(h[1].as_str().starts_with("item "), "the item's number");
        for tag in h.iter().skip(2) {
            let t = tag.as_str();
            // A tag is one bracketed piece; a sender has no brackets.
            assert!(!t.contains('\n'));
        }
    }
    // Day headings are in order, and the undated section (if any) last.
    let days: Vec<String> = sent
        .iter()
        .filter_map(|s| match s {
            Sent::Item(Block::Heading {
                level: HeadingLevel::Day,
                text,
                ..
            }) => Some(text.as_str().to_owned()),
            _ => None,
        })
        .collect();
    assert!(days.iter().any(|d| d == "End of conversation file"));
}

/// Previews: a real 1 × 1 PNG for every image the archive holds, and
/// "unavailable" for the GIFs.
#[derive(Clone, Default)]
struct FakeSource {
    bytes: Option<Vec<u8>>,
}

impl PreviewSource for FakeSource {
    fn renditions(&mut self, a: &AttachmentRecord, _display: bool) -> Result<Preview, ExportError> {
        let Some(k) = a.file.sniffed else {
            return Ok(Preview::None);
        };
        Ok(match k.token {
            "gif" => Preview::Unavailable,
            "png" | "jpeg" | "webp" | "heif" => Preview::Ready {
                thumb: RenditionBytes(self.bytes.clone().unwrap_or_else(png)),
                display: None,
            },
            _ => Preview::None,
        })
    }
}

#[test]
fn previews_are_drawn_from_checked_renditions_and_bad_ones_fail_the_export() {
    let doc = doc("view-media-plain");
    let i = imported(&doc);
    let d = files::dest();
    let tweak = |o: &mut ExportOptions| {
        o.attachments = AttachmentMode::CopyOriginals;
        o.previews = true;
    };
    let report = export_pdf(
        &i,
        &d.parent,
        ScopePlan::Archive,
        &tweak,
        pdf_writer(Some(Box::new(FakeSource::default()))),
    )
    .expect("export");
    let embedded = report.counts.attachments.previews_embedded;
    assert!(embedded >= 1, "{embedded} previews");
    assert_eq!(report.writers[0].counts["previewsEmbedded"], embedded);
    let bundle = d.parent.join(BUNDLE);
    let mut images = 0;
    for p in files::tree(&bundle).iter().filter(|p| p.ends_with(".pdf")) {
        let bytes = std::fs::read(under(&bundle, p)).expect("read");
        let has = |needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
        if has(b"/Subtype /Image") || has(b"/Subtype/Image") {
            images += 1;
            assert!(has(b"/Figure"), "a preview is a tagged figure");
        }
    }
    assert!(images >= 1);
    // A rendition that fails its check fails the export.
    let d = files::dest();
    let bad = FakeSource {
        bytes: Some(b"\x89PNG not really".to_vec()),
    };
    let e = export_pdf(
        &i,
        &d.parent,
        ScopePlan::Archive,
        &tweak,
        pdf_writer(Some(Box::new(bad))),
    )
    .expect_err("refused");
    assert!(matches!(e, ExportError::HelperOutputRejected), "{e:?}");
    assert!(files::tree(&d.parent).is_empty(), "nothing published");
    // A renderer that did not draw every preview it was sent fails the
    // export before the volume is staged.
    let d = files::dest();
    let e = export_pdf(
        &i,
        &d.parent,
        ScopePlan::Archive,
        &tweak,
        PdfWriter::new(
            Box::new(Undercount::default()),
            Some(Box::new(FakeSource::default())),
        ),
    )
    .expect_err("refused");
    assert!(
        matches!(
            e,
            ExportError::HelperFailed(oma_export::HelperFailure::Protocol)
        ),
        "{e:?}"
    );
    assert!(files::tree(&d.parent).is_empty(), "nothing published");
}

#[test]
fn the_hostile_catalogue_is_drawn_without_clipping_a_megabyte_or_a_flood() {
    let doc = doc("view-hostile-text-plain");
    let i = imported(&doc);
    // The blocks: no paragraph over the bound, and the 1 MiB body split.
    let rec = Recorder::default();
    let d = files::dest();
    let report = export_pdf(
        &i,
        &d.parent,
        ScopePlan::Archive,
        &history,
        PdfWriter::new(Box::new(rec.clone()), None),
    )
    .expect("export");
    let sent = rec.sent();
    let mut longest = 0;
    for s in &sent {
        if let Sent::Item(Block::Entry { body, .. }) = s {
            for p in body {
                longest = longest.max(p.text.as_str().len());
                if p.role == ParaRole::Text {
                    assert!(!p.text.as_str().contains('\n'));
                }
            }
        }
    }
    assert!(
        longest <= oma_export::writer::pdf::MAX_PARA_BYTES,
        "{longest}"
    );
    assert!(report.writers[0].counts["pdfForcedWraps"] > 0);
    let all = texts(&sent).concat();
    // Controls and bidi overrides never reach the drawn text raw.
    assert!(!all.chars().any(|c| c.is_control()));
    assert!(!all.contains('\u{202E}'), "markers are on by default");
    // And the real backend draws it, checked.
    let d = files::dest();
    let report = export_pdf(
        &i,
        &d.parent,
        ScopePlan::Archive,
        &history,
        pdf_writer(None),
    )
    .expect("export");
    assert!(report.writers[0].counts["pdfPages"] > 10);
}

/// Bundles for `scripts/oracles/pdf_oracle.py`: every catalogue entry
/// exported to JSON + PDF (markers and previews on with copies, names
/// only with markers off, and names only cut into 2-page volumes), and the export catalogue's entries with
/// both reveals (the positive control), listed in `pdf-oracle.json`.
#[test]
#[ignore = "writes the PDF oracle's bundles: OMA_ORACLE_PDF_OUT=<empty dir>"]
fn write_oracle_pdf() {
    let out = std::path::PathBuf::from(std::env::var("OMA_ORACLE_PDF_OUT").expect("set it"));
    std::fs::create_dir_all(&out).expect("create");
    assert!(
        std::fs::read_dir(&out).expect("read").next().is_none(),
        "OMA_ORACLE_PDF_OUT must be empty"
    );
    let out = dunce::canonicalize(&out).expect("canonical");
    let mut listed = Vec::new();
    for doc in docs() {
        let lib_dir = out.join(&doc.name).join("library");
        std::fs::create_dir_all(&lib_dir).expect("library dir");
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
        let mut bundles = Vec::new();
        // `cut`: names only, with a 2-page volume limit, so the oracle
        // sees volumes cut by the estimate (part 2 and later).
        let mut kinds = vec![
            ("files", true, false, false),
            ("names", false, false, false),
            ("cut", false, false, true),
        ];
        if doc.export.is_some() {
            kinds.push(("revealed", true, true, false));
        }
        for (kind, markers, reveal, cut) in kinds {
            let dest = out.join(&doc.name).join(kind);
            std::fs::create_dir_all(&dest).expect("dest");
            let tweak = |o: &mut ExportOptions| {
                o.history = true;
                o.hidden_character_markers = markers;
                if markers {
                    o.attachments = AttachmentMode::CopyOriginals;
                    o.previews = true;
                } else {
                    o.attachments = AttachmentMode::None;
                }
                if reveal {
                    o.reveal = RevealPlan::confirmed(Some(u64::MAX), Some(u64::MAX));
                }
            };
            let pdf: Box<dyn ExportWriter> = if markers {
                Box::new(pdf_writer(Some(Box::new(FakeSource::default()))))
            } else if cut {
                Box::new(pdf_writer(None).with_limits(VolumeLimits {
                    pages: 2,
                    bytes: 40 << 20,
                }))
            } else {
                Box::new(pdf_writer(None))
            };
            export_with(
                &i,
                &dest,
                ScopePlan::Archive,
                &[Format::Json, Format::Pdf],
                &tweak,
                BUNDLE,
                None,
                &ExportZone::Utc,
                vec![Box::new(oma_export::writer::json::JsonWriter::new()), pdf],
            )
            .unwrap_or_else(|e| panic!("{}: {e:?}", doc.name));
            bundles.push(serde_json::json!({
                "kind": kind,
                "markers": markers,
                "bundle": format!("{}/{kind}/{BUNDLE}", doc.name),
            }));
        }
        let export_rel = doc.export.as_ref().map(|_| {
            if common::EXPORT_SCENARIOS.contains(&doc.name.as_str()) {
                format!("scenarios/{}.export.json", doc.name)
            } else {
                format!("{}.expected/export.json", doc.name)
            }
        });
        listed.push(serde_json::json!({
            "name": doc.name,
            "catalogue": doc.catalogue,
            "export": export_rel,
            "bundles": bundles,
        }));
    }
    let listing = serde_json::json!({
        "format": "oma-oracle-pdf",
        "exports": listed,
    });
    std::fs::write(
        out.join("pdf-oracle.json"),
        serde_json::to_string_pretty(&listing).expect("json"),
    )
    .expect("write listing");
}
