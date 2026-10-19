//! The HTML writer end to end (M6 spec §5.5, §14.2 "Per format: HTML"):
//! every catalogue entry is imported, exported to HTML through
//! `pipeline::run`, and every page is read back with the strict reader in
//! `tests/htmlgrammar`.
//!
//! - every page follows the template: the CSP meta first after the
//!   charset, the one stylesheet and its pinned hash, only the template's
//!   elements and attributes (no script, event or style attribute),
//!   links and image sources of our grammar, one `h1`, one `h2` per day,
//!   one labelled `article` per item, `nav` after `main`, backward-only
//!   item links, every text node escaped, markers as elements;
//! - item counts per page and per file equal the manifest's and the
//!   pre-count's; `index.html` lists every conversation file and part;
//! - hostile text: every hostile body reads back exactly (CR and CRLF as
//!   LF) as text, never as markup; names and senders keep their marks;
//! - masking: no masking sentinel appears in any byte or decoded text
//!   unless the export reveals it;
//! - previews: checked renditions embedded as `data:` within the page
//!   budget, larger previews written beside the pages and linked,
//!   "unavailable" and "size limit" cards counted, a bad rendition fails
//!   the export and leaves nothing, previews without a source are
//!   refused, originals never linked;
//! - page splits by items and by bytes, with the parts linked;
//! - a single file with "About this export"; markers off.
#![allow(clippy::disallowed_methods)]

mod common;
mod files;
mod htmlgrammar;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use common::*;
use htmlgrammar::{Element, Expect};
use oma_export::pipeline::ExportReport;
use oma_export::plan::{AttachmentMode, ExportOptions, Format, RevealPlan, ScopePlan};
use oma_export::preview::{Preview, PreviewSource, RenditionBytes};
use oma_export::record::AttachmentRecord;
use oma_export::writer::html::{self, HtmlWriter, PageLimits};
use oma_export::writer::ExportWriter;
use oma_export::{ExportError, ExportZone};
use serde_json::Value;

const BUNDLE: &str = "Messages export";

/// A real 1 × 1 PNG (every chunk's CRC is right), made for these tests.
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
        .unwrap_or_else(|| panic!("{name}"))
}

/// Export `i` to HTML with `writer` under `tweak`ed defaults.
fn export_html(
    i: &Imported,
    parent: &Path,
    scope: ScopePlan,
    tweak: &dyn Fn(&mut ExportOptions),
    target: &str,
    writer: HtmlWriter,
) -> Result<ExportReport, ExportError> {
    export_with(
        i,
        parent,
        scope,
        &[Format::Html],
        tweak,
        target,
        None,
        &ExportZone::Utc,
        vec![Box::new(writer) as Box<dyn ExportWriter>],
    )
}

fn expect(markers: bool, index: bool, budget: u64) -> Expect<'static> {
    let csp: &'static str = Box::leak(html::csp().into_boxed_str());
    Expect {
        csp,
        style: html::STYLE,
        style_sha256_b64: html::STYLE_SHA256,
        markers,
        data_budget: budget,
        index,
    }
}

/// One checked page.
struct Read {
    page: htmlgrammar::Page,
    root: Element,
    bytes: usize,
}

/// Every page of a bundle's `html/` folder, read and checked, by name.
fn read_bundle(bundle: &Path, markers: bool, budget: u64) -> BTreeMap<String, Read> {
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(bundle.join("html"))
        .expect("html folder")
        .flatten()
    {
        if e.path().is_dir() {
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        out.insert(name.clone(), read_page(&e.path(), &name, markers, budget));
    }
    out
}

fn read_page(path: &Path, name: &str, markers: bool, budget: u64) -> Read {
    let bytes = std::fs::read(path).expect("read");
    let text = String::from_utf8(bytes.clone()).expect("UTF-8");
    assert!(!text.starts_with('\u{FEFF}'), "{name}: BOM");
    assert!(!text.contains('\r'), "{name}: CR");
    let root = htmlgrammar::parse(&text).unwrap_or_else(|e| {
        if let Some(dump) = std::env::var_os("OMA_HTML_DUMP") {
            let _ = std::fs::write(dump, &text);
        }
        panic!("{name}: {e}")
    });
    let page = htmlgrammar::check(&root, &expect(markers, name == "index.html", budget))
        .unwrap_or_else(|e| panic!("{name}: {e}"));
    Read {
        page,
        root,
        bytes: bytes.len(),
    }
}

/// Every byte below `root`, then every page's decoded text (entities
/// decoded, markers back to their characters), so a sentinel is found
/// however it was written.
fn all_text(root: &Path) -> Vec<u8> {
    let mut out = Vec::new();
    for rel in files::tree(root) {
        if rel.ends_with('/') {
            continue;
        }
        let b = std::fs::read(under(root, &rel)).expect("read");
        if rel.ends_with(".html") {
            if let Ok(r) = htmlgrammar::parse(&String::from_utf8_lossy(&b)) {
                out.extend(r.text().as_bytes());
                out.extend(r.plain().as_bytes());
            }
        }
        out.extend(b);
        out.push(b'\n');
    }
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

const SENTINEL_KINDS: [&str; 6] = [
    "oneTimeCodes",
    "earlierText",
    "editText",
    "subjectHistory",
    "alternativeText",
    "unsentAttachmentFiles",
];

/// `c0001-2.html` → (`c0001`, 2).
fn stem_part(name: &str) -> (String, u32) {
    let stem = name.trim_end_matches(".html");
    match stem.rsplit_once('-') {
        Some((b, p)) if p.bytes().all(|c| c.is_ascii_digit()) => {
            (b.to_owned(), p.parse().expect("part"))
        }
        _ => (stem.to_owned(), 1),
    }
}

#[test]
fn every_catalogue_entry_exports_to_html_that_follows_the_template() {
    let mut exported = 0;
    for doc in docs() {
        let what = doc.name.clone();
        let i = imported(&doc);
        let d = files::dest();
        let report = export_html(
            &i,
            &d.parent,
            ScopePlan::Archive,
            &|_| {},
            BUNDLE,
            HtmlWriter::new(),
        )
        .unwrap_or_else(|e| panic!("{what}: {e:?}"));
        let bundle = d.parent.join(BUNDLE);
        assert_eq!(std::fs::read_dir(&d.parent).expect("dest").count(), 1);
        files::verify_sums(&bundle);
        let pages = read_bundle(&bundle, true, html::MAX_PAGE_DATA_BYTES);
        let index = pages.get("index.html").expect("index.html");
        let mut items = 0u64;
        let mut per_file: BTreeMap<String, Vec<u64>> = BTreeMap::new();
        for (name, r) in &pages {
            if name == "index.html" {
                continue;
            }
            // Generic titles only (D112).
            assert!(
                r.page.title.starts_with("Messages export \u{2014} "),
                "{what}"
            );
            assert!(
                !r.page.title.contains("SYNTH"),
                "{what} {name}: content in the title"
            );
            let (stem, _) = stem_part(name);
            per_file
                .entry(stem)
                .or_default()
                .extend(r.page.articles.iter().map(|(n, _)| *n));
            items += r.page.articles.len() as u64;
            // Originals are never linked; previews are off.
            assert!(r.page.hrefs.iter().all(|h| !h.contains("attachments")));
            assert_eq!(r.page.images, 0);
            // Every page is linked from the index.
            assert!(
                index.page.hrefs.contains(name),
                "{what}: {name} not in the index"
            );
        }
        for (stem, ns) in &per_file {
            let mut u = ns.clone();
            u.sort_unstable();
            u.dedup();
            assert_eq!(u.len(), ns.len(), "{what} {stem}: an item twice");
        }
        let m = json(&bundle.join("manifest.json"));
        assert_eq!(m["counts"]["items"], items, "{what}");
        assert_eq!(report.counts.items, items, "{what}");
        assert_eq!(m["formats"], serde_json::json!(["html"]));
        let r = report
            .writers
            .iter()
            .find(|w| w.format == Format::Html)
            .expect("html report");
        assert_eq!(r.counts["pages"], pages.len() as u64, "{what}");
        if let Some(e) = &doc.export {
            let bytes = all_text(&bundle);
            for k in SENTINEL_KINDS {
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
        let sentinels: Vec<String> = SENTINEL_KINDS
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
        export_html(
            &i,
            &d.parent,
            ScopePlan::Archive,
            &reveal,
            BUNDLE,
            HtmlWriter::new(),
        )
        .expect("export");
        let bundle = d.parent.join(BUNDLE);
        read_bundle(&bundle, true, html::MAX_PAGE_DATA_BYTES);
        let bytes = all_text(&bundle);
        for s in &sentinels {
            assert!(contains(&bytes, s), "{}: {s} not revealed", doc.name);
            checked += 1;
        }
    }
    assert!(checked >= 5, "{checked}");
}

/// The text of every `p.body` of every article, by item ordinal.
fn bodies(pages: &BTreeMap<String, Read>) -> Vec<(u64, String)> {
    let mut out = Vec::new();
    for r in pages.values() {
        for (n, a) in &r.page.articles {
            for p in a.walk() {
                if p.name == "p" && p.has_class("body") {
                    out.push((*n, p.text()));
                }
            }
        }
    }
    out
}

#[test]
fn hostile_text_stays_text_and_names_keep_their_marks() {
    let doc = doc("export-text-plain");
    let e = doc.export.clone().expect("export");
    let i = imported(&doc);
    let d = files::dest();
    export_html(
        &i,
        &d.parent,
        ScopePlan::Archive,
        &|_| {},
        BUNDLE,
        HtmlWriter::new(),
    )
    .expect("export");
    let bundle = d.parent.join(BUNDLE);
    let pages = read_bundle(&bundle, true, html::MAX_PAGE_DATA_BYTES);
    let texts: Vec<String> = bodies(&pages).into_iter().map(|(_, t)| t).collect();
    let mut found = 0;
    for h in e["hostile"].as_array().expect("hostile") {
        if h["where"] != "body" {
            continue;
        }
        let Some(want) = h["text"].as_str() else {
            continue; // a long text the catalogue states by its digest
        };
        let want = want.replace("\r\n", "\n").replace('\r', "\n");
        assert!(texts.contains(&want), "{}: not read back", h["id"]);
        found += 1;
    }
    assert!(found >= 30, "{found}");
    // The markup-looking bodies are text: no element came from them.
    let raw = std::fs::read_to_string(
        pages
            .keys()
            .find(|n| {
                std::fs::read_to_string(bundle.join("html").join(n))
                    .expect("read")
                    .contains("SYNTH-EXP-Q-HTML")
            })
            .map(|n| bundle.join("html").join(n))
            .expect("page with the html body"),
    )
    .expect("read");
    assert!(raw.contains(
        "&lt;b&gt;SYNTH-EXP-Q-HTML&lt;/b&gt; &amp;amp; &lt;!-- c --&gt; &lt;/article&gt;&lt;script&gt;x&lt;/script&gt;"
    ));
    // The conversation name keeps its line and paragraph separators as
    // text and its controls and bidi override as marker elements.
    let name = "SYNTH-EXP-NAME first\u{2028}second\u{85}third\u{2029}fourth \u{202E}evil";
    let r = pages
        .values()
        .find(|r| r.page.h1.contains("SYNTH-EXP-NAME"))
        .expect("named page");
    assert!(r.page.h1.ends_with(&format!(": {name}")), "{}", r.page.h1);
    let h1 = r
        .root
        .walk()
        .into_iter()
        .find(|x| x.name == "h1")
        .expect("h1");
    let marks: Vec<String> = h1
        .walk()
        .into_iter()
        .filter(|x| x.has_class("mark"))
        .map(|x| x.plain())
        .collect();
    assert_eq!(marks, ["U+0085", "U+202E"]);
    // Hostile senders read back exactly, marks undone.
    let senders: Vec<String> = pages
        .values()
        .flat_map(|r| r.page.articles.iter())
        .filter_map(|(_, a)| {
            a.walk()
                .into_iter()
                .find(|x| x.has_class("who"))
                .map(|w| w.text())
        })
        .collect();
    assert!(senders
        .iter()
        .any(|s| s == "synthetic-export-6\u{2028}line@example.invalid"));
    assert!(senders
        .iter()
        .any(|s| s == "synthetic-export-7\u{85}next\u{202E}@example.invalid"));
}

#[test]
fn markers_off_leaves_hidden_characters_raw_in_bodies_only() {
    let doc = doc("export-text-plain");
    let i = imported(&doc);
    let d = files::dest();
    let off = |o: &mut ExportOptions| o.hidden_character_markers = false;
    let report = export_html(
        &i,
        &d.parent,
        ScopePlan::Archive,
        &off,
        BUNDLE,
        HtmlWriter::new(),
    )
    .expect("export");
    let bundle = d.parent.join(BUNDLE);
    let pages = read_bundle(&bundle, false, html::MAX_PAGE_DATA_BYTES);
    let all: String = pages
        .keys()
        .map(|n| std::fs::read_to_string(bundle.join("html").join(n)).expect("read"))
        .collect();
    // The bidi override of a body is raw; the conversation name's stays
    // marked; controls are marked either way.
    assert!(all.contains("SYNTH-HX-RLO \u{202E}gnirts desrever"));
    assert!(all.contains("fourth <span class=\"mark\">U+202E</span>evil"));
    assert!(all.contains("before<span class=\"mark\">U+0000</span>after"));
    let r = &report.writers[0];
    assert!(r.counts["controlsEscaped"] > 0 && r.counts["hiddenCharactersMarked"] > 0);
}

#[test]
fn one_conversation_is_a_single_file_with_its_about() {
    let doc = doc("view-main-plain");
    let i = imported(&doc);
    let chat = with_snapshot(&i, |s| Ok(s.chat_ids()?[0]));
    let d = files::dest();
    let target = "conversation.html";
    export_html(
        &i,
        &d.parent,
        ScopePlan::Conversation { chat },
        &|o| o.attachments = AttachmentMode::None,
        target,
        HtmlWriter::new(),
    )
    .expect("export");
    assert_eq!(files::tree(&d.parent), [target]);
    let r = read_page(
        &d.parent.join(target),
        target,
        true,
        html::MAX_PAGE_DATA_BYTES,
    );
    assert_eq!(r.page.title, "Messages export \u{2014} conversation 1");
    // No index and no other part to link to; About lists the manifest's
    // fields without files.
    assert!(
        r.page.hrefs.iter().all(|h| h.starts_with('#')),
        "{:?}",
        r.page.hrefs
    );
    let about: Vec<String> = r
        .root
        .walk()
        .into_iter()
        .filter(|x| x.name == "ul" && x.has_class("about"))
        .flat_map(|u| u.elements().map(|li| li.text()).collect::<Vec<_>>())
        .collect();
    assert!(about
        .iter()
        .any(|l| l == "format: openmessage-archive-export-manifest"));
    assert!(about.iter().any(|l| l.starts_with("counts.items: ")));
    assert!(about.iter().all(|l| !l.starts_with("files")));
}

#[test]
fn pages_split_by_items_and_by_bytes_and_link_their_parts() {
    let doc = doc("view-main-plain");
    let i = imported(&doc);
    for (what, limits) in [
        (
            "items",
            PageLimits {
                items: 3,
                ..PageLimits::SPEC
            },
        ),
        (
            "bytes",
            PageLimits {
                bytes: 16_000,
                reserve: 6_000,
                ..PageLimits::SPEC
            },
        ),
    ] {
        let d = files::dest();
        let report = export_html(
            &i,
            &d.parent,
            ScopePlan::Archive,
            &|o| o.attachments = AttachmentMode::None,
            BUNDLE,
            HtmlWriter::new().with_limits(limits),
        )
        .expect("export");
        let bundle = d.parent.join(BUNDLE);
        let pages = read_bundle(&bundle, true, html::MAX_PAGE_DATA_BYTES);
        let mut parts = 0;
        let mut items = 0;
        for (name, r) in &pages {
            if name == "index.html" {
                continue;
            }
            let n = r.page.articles.len() as u64;
            items += n;
            let (stem, part) = stem_part(name);
            if what == "items" {
                assert!(n <= 3, "{name}: {n} items");
            } else if n > 1 {
                assert!(r.bytes as u64 <= limits.bytes, "{name}: {} bytes", r.bytes);
            }
            if part > 1 {
                parts += 1;
                // A later part links to every earlier one.
                for k in 1..part {
                    let earlier = if k == 1 {
                        format!("{stem}.html")
                    } else {
                        format!("{stem}-{k}.html")
                    };
                    assert!(r.page.hrefs.contains(&earlier), "{name} → {earlier}");
                }
            }
            if pages.contains_key(&format!("{stem}-{}.html", part + 1)) {
                assert!(r.page.hrefs.contains(&format!("{stem}-{}.html", part + 1)));
            }
        }
        assert!(parts >= 2, "{what}: {parts} later parts");
        assert_eq!(report.counts.items, items, "{what}");
        let index = &pages["index.html"];
        for name in pages.keys().filter(|n| *n != "index.html") {
            assert!(index.page.hrefs.contains(name), "{what}: {name}");
        }
    }
}

#[test]
fn replies_link_backward_on_their_page_and_name_the_item_otherwise() {
    let doc = doc("view-main-plain");
    let i = imported(&doc);
    let mut texts = Vec::new();
    for limits in [
        PageLimits::SPEC,
        PageLimits {
            items: 1,
            ..PageLimits::SPEC
        },
    ] {
        let d = files::dest();
        let report = export_html(
            &i,
            &d.parent,
            ScopePlan::Archive,
            &|o| o.attachments = AttachmentMode::None,
            BUNDLE,
            HtmlWriter::new().with_limits(limits),
        )
        .expect("export");
        let pages = read_bundle(&d.parent.join(BUNDLE), true, html::MAX_PAGE_DATA_BYTES);
        let links: u64 = pages.values().map(|r| r.page.item_links).sum();
        let all: String = pages.values().map(|r| r.root.plain()).collect();
        texts.push((links, report.writers[0].counts["replyLinks"], all));
    }
    // One item per page: no item link can point backward on its page.
    let (links, counted, _) = &texts[0];
    assert!(*links > 0 && links == counted, "{links} {counted}");
    let (links, counted, all) = &texts[1];
    assert_eq!((*links, *counted), (0, 0));
    assert!(all.contains("Reply to item 1, elsewhere in this conversation"));
}

/// A source of renditions for the tests: a real 1 × 1 PNG thumbnail (and
/// display) for every copied image, "unavailable" for the GIFs, and
/// whatever `bytes` says instead when set.
#[derive(Clone, Default)]
struct FakeSource {
    asked: Arc<Mutex<Vec<(u64, bool)>>>,
    bytes: Option<Vec<u8>>,
    pad: usize,
}

impl PreviewSource for FakeSource {
    fn renditions(&mut self, a: &AttachmentRecord, display: bool) -> Result<Preview, ExportError> {
        self.asked.lock().expect("lock").push((a.n, display));
        let Some(k) = a.file.sniffed else {
            return Ok(Preview::None);
        };
        Ok(match k.token {
            "gif" => Preview::Unavailable,
            "png" | "jpeg" | "webp" | "heif" => {
                let mut b = self.bytes.clone().unwrap_or_else(png);
                if self.pad > 0 {
                    // Room inside the PNG for a big thumbnail: an ancillary
                    // chunk the checks accept (structure only).
                    let iend = b.split_off(b.len() - 12);
                    b.extend_from_slice(&(self.pad as u32).to_be_bytes());
                    b.extend_from_slice(b"tEXt");
                    b.resize(b.len() + self.pad, b'x');
                    b.extend_from_slice(&[0, 0, 0, 0]);
                    b.extend_from_slice(&iend);
                }
                Preview::Ready {
                    thumb: RenditionBytes(b.clone()),
                    display: display.then_some(RenditionBytes(b)),
                }
            }
            _ => Preview::None,
        })
    }
}

fn media_docs() -> Vec<Doc> {
    docs()
        .into_iter()
        .filter(|d| {
            [
                "view-media-plain",
                "view-media2-plain",
                "export-files-plain",
            ]
            .contains(&d.name.as_str())
        })
        .collect()
}

#[test]
fn previews_are_checked_renditions_embedded_as_data_and_never_originals() {
    let mut embedded = 0;
    for doc in media_docs() {
        let what = doc.name.clone();
        let i = imported(&doc);
        let d = files::dest();
        let src = FakeSource::default();
        let asked = src.asked.clone();
        let on = |o: &mut ExportOptions| {
            o.previews = true;
            o.larger_previews = true;
        };
        let report = export_html(
            &i,
            &d.parent,
            ScopePlan::Archive,
            &on,
            BUNDLE,
            HtmlWriter::with_previews(Box::new(src)),
        )
        .unwrap_or_else(|e| panic!("{what}: {e:?}"));
        let bundle = d.parent.join(BUNDLE);
        let pages = read_bundle(&bundle, true, html::MAX_PAGE_DATA_BYTES);
        let r = &report.writers[0].counts;
        let images: u64 = pages.values().map(|p| p.page.images).sum();
        assert_eq!(images, r["previewsEmbedded"], "{what}");
        let m = json(&bundle.join("manifest.json"));
        assert_eq!(m["counts"]["attachments"]["previewsEmbedded"], images);
        assert_eq!(
            m["counts"]["attachments"]["previewsUnavailable"],
            r["previewsUnavailable"]
        );
        let cards = pages
            .values()
            .flat_map(|p| {
                p.root
                    .walk()
                    .into_iter()
                    .map(|e| e.plain())
                    .collect::<Vec<_>>()
            })
            .filter(|t| t == "Preview unavailable")
            .count() as u64;
        assert_eq!(cards, r["previewsUnavailable"], "{what}");
        // Asked only with the larger rendition, once per placement.
        assert!(asked.lock().expect("lock").iter().all(|(_, d)| *d));
        // Larger previews: written once each beside the pages, linked, the
        // rendition's bytes exactly.
        let mut linked: Vec<String> = pages
            .values()
            .flat_map(|p| p.page.hrefs.clone())
            .filter(|h| h.starts_with("previews/"))
            .collect();
        linked.sort();
        linked.dedup();
        assert_eq!(linked.len() as u64, r["largerPreviewsWritten"], "{what}");
        for h in &linked {
            let b = std::fs::read(under(&bundle.join("html"), h)).expect("larger preview");
            assert_eq!(b, png(), "{what} {h}");
        }
        // Originals are never linked or embedded.
        for p in pages.values() {
            assert!(p.page.hrefs.iter().all(|h| !h.contains("attachments")));
        }
        files::verify_sums(&bundle);
        embedded += images;
    }
    assert!(embedded >= 2, "{embedded}");
}

#[test]
fn the_page_budget_stops_embedding_and_counts_it() {
    let mut limited = 0;
    for doc in media_docs() {
        let i = imported(&doc);
        let d = files::dest();
        let src = FakeSource {
            pad: 3_000,
            ..FakeSource::default()
        };
        let budget = 6_000;
        let limits = PageLimits {
            data_bytes: budget,
            ..PageLimits::SPEC
        };
        let report = export_html(
            &i,
            &d.parent,
            ScopePlan::Archive,
            &|o| o.previews = true,
            BUNDLE,
            HtmlWriter::with_previews(Box::new(src)).with_limits(limits),
        )
        .expect("export");
        let pages = read_bundle(&d.parent.join(BUNDLE), true, budget);
        let r = &report.writers[0].counts;
        let cards = pages
            .values()
            .flat_map(|p| {
                p.root
                    .walk()
                    .into_iter()
                    .map(|e| e.plain())
                    .collect::<Vec<_>>()
            })
            .filter(|t| t == "Preview not embedded (size limit)")
            .count() as u64;
        assert_eq!(cards, r["previewsSizeLimited"]);
        // Without larger previews nothing is written beside the pages.
        assert!(!d.parent.join(BUNDLE).join("html").join("previews").exists());
        limited += cards;
    }
    assert!(limited >= 1, "{limited}");
}

#[test]
fn a_bad_rendition_fails_the_export_and_leaves_nothing() {
    let doc = doc("view-media-plain");
    let i = imported(&doc);
    for bad in [
        b"<svg onload=alert(1)>".to_vec(),
        vec![0xFF, 0xD8, 0xFF, 0xD9],
        {
            let mut p = png();
            p[18] = 0x10; // 4,097 px wide: over the thumbnail's limit
            p
        },
    ] {
        let d = files::dest();
        let src = FakeSource {
            bytes: Some(bad),
            ..FakeSource::default()
        };
        let e = export_html(
            &i,
            &d.parent,
            ScopePlan::Archive,
            &|o| o.previews = true,
            BUNDLE,
            HtmlWriter::with_previews(Box::new(src)),
        )
        .expect_err("refused");
        assert!(matches!(e, ExportError::HelperOutputRejected), "{e:?}");
        assert!(
            files::tree(&d.parent).is_empty(),
            "{:?}",
            files::tree(&d.parent)
        );
    }
}

#[test]
fn previews_without_a_source_are_refused() {
    let doc = doc("view-media-plain");
    let i = imported(&doc);
    let d = files::dest();
    let e = export_html(
        &i,
        &d.parent,
        ScopePlan::Archive,
        &|o| o.previews = true,
        BUNDLE,
        HtmlWriter::new(),
    )
    .expect_err("refused");
    assert!(matches!(e, ExportError::Plan(_)), "{e:?}");
    assert!(files::tree(&d.parent).is_empty());
}

/// Writes every catalogue entry's HTML + JSON bundles for the Python HTML
/// oracle (`scripts/oracles/html_oracle.py`): files copied with previews
/// from the test source, and names only with the markers off.
#[test]
#[ignore = "writes the HTML oracle's input; run with OMA_ORACLE_HTML_OUT"]
fn write_oracle_html() {
    let out = std::path::PathBuf::from(std::env::var("OMA_ORACLE_HTML_OUT").expect("set it"));
    std::fs::create_dir_all(&out).expect("create");
    assert!(
        std::fs::read_dir(&out).expect("read").next().is_none(),
        "OMA_ORACLE_HTML_OUT must be empty"
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
        for (kind, markers) in [("files", true), ("names", false)] {
            let dest = out.join(&doc.name).join(kind);
            std::fs::create_dir_all(&dest).expect("dest");
            let tweak = |o: &mut ExportOptions| {
                o.history = true;
                o.hidden_character_markers = markers;
                if markers {
                    o.attachments = AttachmentMode::CopyOriginals;
                    o.previews = true;
                    o.larger_previews = true;
                } else {
                    o.attachments = AttachmentMode::None;
                }
            };
            let html: Box<dyn ExportWriter> = if markers {
                Box::new(HtmlWriter::with_previews(Box::new(FakeSource::default())))
            } else {
                Box::new(HtmlWriter::new())
            };
            export_with(
                &i,
                &dest,
                ScopePlan::Archive,
                &[Format::Json, Format::Html],
                &tweak,
                BUNDLE,
                None,
                &ExportZone::Utc,
                vec![html, Box::new(oma_export::writer::json::JsonWriter::new())],
            )
            .unwrap_or_else(|e| panic!("{}: {e:?}", doc.name));
            bundles.push(serde_json::json!({
                "kind": kind,
                "markers": markers,
                "bundle": format!("{}/{kind}/{BUNDLE}", doc.name),
            }));
        }
        let export = doc.export.as_ref().map(|_| match doc.name.as_str() {
            "history" => "scenarios/history.export.json".to_owned(),
            n => format!("{n}.expected/export.json"),
        });
        listed.push(serde_json::json!({
            "name": doc.name,
            "catalogue": doc.catalogue,
            "bundles": bundles,
            "export": export,
        }));
    }
    let doc = serde_json::json!({
        "format": "oma-oracle-html",
        "formatVersion": 1,
        "csp": html::csp(),
        "exports": listed,
    });
    std::fs::write(
        out.join("html-oracle.json"),
        serde_json::to_vec_pretty(&doc).expect("json"),
    )
    .expect("write");
}

// ------------------------------------------------------- the preview feed

/// A pool for the tests (slice 6h2): it records what it was given and
/// answers with `answer` (`None`: never answers, keeping the senders).
struct FakePool {
    given: Mutex<Vec<(Vec<u8>, bool)>>,
    answer: Option<oma_export::preview::PoolOutcome>,
    parked: Mutex<Vec<std::sync::mpsc::Sender<oma_export::preview::PoolOutcome>>>,
}

impl FakePool {
    fn new(answer: Option<oma_export::preview::PoolOutcome>) -> Arc<FakePool> {
        Arc::new(FakePool {
            given: Mutex::new(Vec::new()),
            answer,
            parked: Mutex::new(Vec::new()),
        })
    }
}

impl oma_export::preview::RenditionPool for FakePool {
    fn submit(&self, bytes: Vec<u8>, display: bool) -> oma_export::preview::RenditionTicket {
        use oma_export::preview::RenditionTicket;
        self.given.lock().expect("lock").push((bytes, display));
        match &self.answer {
            Some(a) => RenditionTicket::ready(a.clone()),
            None => {
                let (tx, t) = RenditionTicket::channel();
                self.parked.lock().expect("lock").push(tx);
                t
            }
        }
    }
}

fn ready_png() -> oma_export::preview::PoolOutcome {
    oma_export::preview::PoolOutcome::Ready {
        thumb: RenditionBytes(png()),
        display: Some(RenditionBytes(png())),
    }
}

/// Export `doc` to HTML with previews through a feed over `pool`.
fn export_fed(
    i: &Imported,
    parent: &Path,
    copy: bool,
    larger: bool,
    pool: Arc<FakePool>,
    cancel: oma_archive::CancelToken,
) -> (
    Result<ExportReport, ExportError>,
    oma_export::preview::PreviewFeed,
) {
    let feed = oma_export::preview::PreviewFeed::new(pool, larger, cancel);
    let tweak = move |o: &mut ExportOptions| {
        o.attachments = if copy {
            AttachmentMode::CopyOriginals
        } else {
            AttachmentMode::None
        };
        o.previews = true;
        o.larger_previews = larger;
    };
    let r = export_with_previews(
        i,
        parent,
        ScopePlan::Archive,
        &[Format::Html],
        &tweak,
        BUNDLE,
        None,
        &ExportZone::Utc,
        vec![Box::new(HtmlWriter::with_previews(feed.source())) as Box<dyn ExportWriter>],
        Some(feed.clone()),
    );
    (r, feed)
}

/// The feed gives the pool the verified bytes of each included image,
/// from the copy stream or from one verified read without a copy, and the
/// same images either way; the writer embeds what the pool made.
#[test]
fn the_feed_sends_verified_image_bytes_with_or_without_copies() {
    let mut total = 0;
    for doc in media_docs() {
        let what = doc.name.clone();
        let i = imported(&doc);
        let mut seen: Vec<Vec<Vec<u8>>> = Vec::new();
        for copy in [true, false] {
            let d = files::dest();
            let pool = FakePool::new(Some(ready_png()));
            let (r, feed) = export_fed(
                &i,
                &d.parent,
                copy,
                copy,
                Arc::clone(&pool),
                oma_archive::CancelToken::new(),
            );
            let report = r.unwrap_or_else(|e| panic!("{what} copy={copy}: {e:?}"));
            let given = pool.given.lock().expect("lock").clone();
            for (b, display) in &given {
                let kind = oma_archive::media::sniff(b, b.len() as u64);
                assert!(oma_export::attach::is_previewable(kind), "{what}: {kind:?}");
                assert_eq!(*display, copy, "{what}");
            }
            let r = &report.writers[0].counts;
            assert_eq!(r["previewsEmbedded"], given.len() as u64, "{what} {copy}");
            assert_eq!(feed.submitted(), given.len() as u64);
            assert_eq!(feed.skipped_after_helper_failures(), 0);
            let m = json(&d.parent.join(BUNDLE).join("manifest.json"));
            assert_eq!(
                m["counts"]["attachments"]["previewsSkippedAfterHelperFailures"],
                0
            );
            files::verify_sums(&d.parent.join(BUNDLE));
            let mut bytes: Vec<Vec<u8>> = given.into_iter().map(|(b, _)| b).collect();
            bytes.sort();
            seen.push(bytes);
        }
        assert_eq!(seen[0], seen[1], "{what}: the same images either way");
        total += seen[0].len();
    }
    assert!(total >= 2, "{total}");
}

/// After three helper crashes in a row previews are off: the rest are
/// cards, counted as skipped in the manifest, and nothing more is sent.
#[test]
fn helper_crashes_turn_previews_off_and_are_counted() {
    for doc in media_docs() {
        let what = doc.name.clone();
        let i = imported(&doc);
        // How many images the export offers.
        let d = files::dest();
        let ok = FakePool::new(Some(ready_png()));
        let (r, _) = export_fed(
            &i,
            &d.parent,
            true,
            false,
            Arc::clone(&ok),
            oma_archive::CancelToken::new(),
        );
        r.expect("export");
        let images = ok.given.lock().expect("lock").len() as u64;
        let d = files::dest();
        let crash = FakePool::new(Some(oma_export::preview::PoolOutcome::Crashed));
        let (r, feed) = export_fed(
            &i,
            &d.parent,
            true,
            false,
            Arc::clone(&crash),
            oma_archive::CancelToken::new(),
        );
        let report = r.unwrap_or_else(|e| panic!("{what}: {e:?}"));
        let sent = crash.given.lock().expect("lock").len() as u64;
        let skipped = feed.skipped_after_helper_failures();
        assert_eq!(sent + skipped, images, "{what}");
        assert_eq!(feed.is_off(), images >= 3, "{what}");
        println!("RECORD {what} images={images} sent={sent} skipped={skipped}");
        let c = &report.writers[0].counts;
        assert_eq!(c["previewsEmbedded"], 0);
        assert_eq!(c["previewsUnavailable"], images, "{what}");
        assert_eq!(
            report
                .counts
                .attachments
                .previews_skipped_after_helper_failures,
            skipped
        );
        let m = json(&d.parent.join(BUNDLE).join("manifest.json"));
        assert_eq!(
            m["counts"]["attachments"]["previewsSkippedAfterHelperFailures"],
            skipped
        );
    }
}

/// Cancel latency while the writer waits for a rendition that never comes
/// (spec §8.5, §12): the export stops within the wait's 50 ms slices, and
/// the unfinished output is removed.
#[test]
fn a_cancel_stops_a_wait_for_a_rendition_and_removes_the_partial() {
    let doc = media_docs().into_iter().next().expect("a media entry");
    let i = imported(&doc);
    let d = files::dest();
    let pool = FakePool::new(None);
    let cancel = oma_archive::CancelToken::new();
    let c = cancel.clone();
    let given = Arc::clone(&pool);
    let canceller = std::thread::spawn(move || {
        // Cancel once the first image waits in the pool.
        let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while given.given.lock().expect("lock").is_empty() {
            assert!(std::time::Instant::now() < until, "an image was submitted");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let at = std::time::Instant::now();
        c.cancel();
        at
    });
    let (r, _) = export_fed(&i, &d.parent, true, false, pool, cancel);
    let ended = std::time::Instant::now();
    let cancelled_at = canceller.join().expect("canceller");
    assert!(matches!(r, Err(ExportError::Cancelled)), "{r:?}");
    let latency = ended.saturating_duration_since(cancelled_at);
    println!(
        "RECORD cancel_latency_preview_wait_ms={}",
        latency.as_millis()
    );
    assert!(
        latency < std::time::Duration::from_secs(1),
        "no new output within 1 s of the cancel: {latency:?}"
    );
    assert_eq!(
        std::fs::read_dir(&d.parent).expect("dir").count(),
        0,
        "nothing left in the destination"
    );
}
