//! The CSV writer end to end (M6 spec §5.2, §14.2 "Per format: CSV"):
//! every catalogue entry is imported and exported to CSV through
//! `pipeline::run`, and every file is read back with the `csv` reader.
//!
//! - all nine files always exist, each with its header row (the pinned
//!   column contract, `tests/csv_columns.pin`), CRLF record ends, every
//!   field quoted, and a BOM only in the "for spreadsheets" profile;
//! - `messages.csv` has one row per timeline item and folded reaction row,
//!   equal to the manifest's counts;
//! - **default-deny**: in every cell of every file, a leading `'` is ours
//!   exactly where the row's `neutralised_columns` names the column, and
//!   no other cell would trigger (outside a typed column whose grammar it
//!   matches); the manifest's `neutralisedCells` is their count;
//! - every formula case the export catalogue plants (bodies, addresses, a
//!   conversation name, file names; after whitespace, the BOM and each
//!   default-ignorable; full-width and small forms) is neutralised, and
//!   the one on a second line is not;
//! - quotes, commas, CRLF, lone CR and NUL round-trip through the reader;
//!   controls are `⟨U+XXXX⟩` for spreadsheets and raw for data tools, and
//!   U+27E8 is escaped in both;
//! - files left out by an option hold their header only;
//! - `masked_pieces` counts the masked codes the text shows.
#![allow(clippy::disallowed_methods)]

mod common;
mod files;

use std::collections::BTreeMap;
use std::path::Path;

use common::*;
use oma_export::plan::{CsvProfile, ExportOptions, Format, ScopePlan};
use oma_export::writer::csv::{columns_text, triggers, Kind, NEUTRALISED, TABLES};
use oma_export::ExportZone;
use serde_json::Value;

const BUNDLE: &str = "CSV export";

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

fn export_csv(
    i: &Imported,
    parent: &Path,
    tweak: &dyn Fn(&mut ExportOptions),
) -> oma_export::pipeline::ExportReport {
    export_formats(
        i,
        parent,
        ScopePlan::Archive,
        &[Format::Csv],
        tweak,
        BUNDLE,
        None,
        &ExportZone::Utc,
    )
    .expect("export")
}

/// One CSV file read back: its header and rows.
struct Table {
    header: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl Table {
    fn col(&self, name: &str) -> usize {
        self.header
            .iter()
            .position(|h| h == name)
            .unwrap_or_else(|| panic!("no column {name}"))
    }
}

/// Check a file's bytes (BOM, CRLF outside quotes, every field quoted) and
/// read it.
fn read_table(path: &Path, bom: bool) -> Table {
    let bytes = std::fs::read(path).expect("read");
    let body = match bytes.strip_prefix("\u{FEFF}".as_bytes()) {
        Some(b) => {
            assert!(bom, "{}: BOM in the data-tools profile", path.display());
            b
        }
        None => {
            assert!(!bom, "{}: no BOM for spreadsheets", path.display());
            &bytes[..]
        }
    };
    assert!(body.ends_with(b"\r\n"), "{}", path.display());
    // Outside quotes: only `"`, `,` and CRLF; every field opens with `"`.
    let mut quoted = false;
    let mut field_start = true;
    let mut i = 0;
    while i < body.len() {
        let c = body[i];
        if quoted {
            if c == b'"' {
                if body.get(i + 1) == Some(&b'"') {
                    i += 1;
                } else {
                    quoted = false;
                }
            }
        } else {
            match c {
                b'"' if field_start => quoted = true,
                b',' => {
                    field_start = true;
                    i += 1;
                    continue;
                }
                b'\r' => {
                    assert_eq!(body.get(i + 1), Some(&b'\n'), "{}: bare CR", path.display());
                    i += 2;
                    field_start = true;
                    continue;
                }
                other => panic!("{}: unquoted byte {other:#x}", path.display()),
            }
        }
        field_start = false;
        i += 1;
    }
    assert!(!quoted, "{}: open quote", path.display());
    let mut r = csv::ReaderBuilder::new()
        .has_headers(false)
        .from_reader(body);
    let mut records = r.records().map(|x| {
        x.expect("record")
            .iter()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    });
    let header = records.next().expect("a header row");
    let rows: Vec<Vec<String>> = records.collect();
    for row in &rows {
        assert_eq!(row.len(), header.len(), "{}", path.display());
    }
    Table { header, rows }
}

fn read_bundle(bundle: &Path, bom: bool) -> BTreeMap<String, Table> {
    let mut out = BTreeMap::new();
    let names: Vec<String> = std::fs::read_dir(bundle.join("csv"))
        .expect("csv folder")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names.len(), TABLES.len(), "nine files");
    for n in names {
        out.insert(n.clone(), read_table(&bundle.join("csv").join(&n), bom));
    }
    out
}

/// The default-deny invariant over every cell; returns the prefixed
/// cells.
fn check_neutralisation(tables: &BTreeMap<String, Table>, spreadsheet: bool) -> u64 {
    let mut prefixed = 0;
    for t in TABLES {
        let file = format!("{}.csv", stem_name(t));
        let table = &tables[&file];
        let want: Vec<&str> = t.columns.iter().map(|(n, _)| *n).collect();
        assert_eq!(table.header, want, "{file}: header");
        let at = table.col(NEUTRALISED);
        for row in &table.rows {
            let named: Vec<&str> = if row[at].is_empty() {
                Vec::new()
            } else {
                row[at].split(';').collect()
            };
            if !spreadsheet {
                assert!(named.is_empty(), "{file}: neutralised for data tools");
            }
            for (k, ((name, kind), cell)) in t.columns.iter().zip(row).enumerate() {
                if k == at {
                    continue;
                }
                if named.contains(name) {
                    let rest = cell
                        .strip_prefix('\'')
                        .unwrap_or_else(|| panic!("{file}: {name} named but not prefixed"));
                    assert!(
                        triggers(rest) && !kind.matches(rest),
                        "{file}: {name} prefixed without a trigger"
                    );
                    prefixed += 1;
                } else if spreadsheet {
                    assert!(
                        !triggers(cell) || kind.matches(cell),
                        "{file}: {name} would trigger and is not neutralised"
                    );
                }
            }
        }
    }
    prefixed
}

fn stem_name(t: &oma_export::writer::csv::TableDef) -> String {
    let p = oma_export::naming::BundlePath::format_file(
        Format::Csv,
        oma_export::naming::FileStem::Table(t.stem),
        oma_export::naming::FileExt::Csv,
    );
    p.file().trim_end_matches(".csv").to_owned()
}

#[test]
fn the_column_contract_is_pinned() {
    let pin = files::path_of("csv_columns.pin");
    let text = columns_text();
    if std::env::var_os("OMA_REPIN_CSV_COLUMNS").is_some() {
        std::fs::write(&pin, &text).expect("write pin");
        return;
    }
    let pinned = std::fs::read_to_string(&pin)
        .expect("tests/csv_columns.pin")
        .replace("\r\n", "\n");
    // Columns are only appended: every pinned line of a file keeps its place.
    let by_file = |t: &str| -> BTreeMap<String, Vec<String>> {
        let mut m: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for l in t.lines() {
            let (f, rest) = l.split_once('\t').expect("line");
            m.entry(f.to_owned()).or_default().push(rest.to_owned());
        }
        m
    };
    let (now, then) = (by_file(&text), by_file(&pinned));
    for (f, cols) in &then {
        let got = now.get(f).unwrap_or_else(|| panic!("{f} removed"));
        assert!(
            got.starts_with(cols),
            "{f}: a pinned column moved, changed or went"
        );
    }
    assert_eq!(
        text, pinned,
        "columns appended: re-pin with OMA_REPIN_CSV_COLUMNS=1 and bump tabularColumnsVersion's MINOR"
    );
}

#[test]
fn every_catalogue_entry_exports_to_csv_that_reads_back_and_is_neutralised() {
    let mut exported = 0;
    let mut prefixed_total = 0;
    for doc in docs() {
        let what = doc.name.clone();
        let i = imported(&doc);
        let d = files::dest();
        let history = |o: &mut ExportOptions| o.history = true;
        let report = export_csv(&i, &d.parent, &history);
        let bundle = d.parent.join(BUNDLE);
        files::verify_sums(&bundle);
        let tables = read_bundle(&bundle, true);
        let prefixed = check_neutralisation(&tables, true);
        let m = json(&bundle.join("manifest.json"));
        assert_eq!(m["counts"]["neutralisedCells"], prefixed, "{what}");
        assert_eq!(m["tabularColumnsVersion"], "1.0.0");
        let rows = tables["messages.csv"].rows.len() as u64;
        let c = &m["counts"];
        assert_eq!(
            rows,
            c["items"].as_u64().expect("items") + c["foldedReactionRows"].as_u64().expect("f"),
            "{what}"
        );
        assert_eq!(report.counts.neutralised_cells, prefixed);
        // `masked_pieces` counts the placeholders in `text`.
        let t = &tables["messages.csv"];
        let (text, masked) = (t.col("text"), t.col("masked_pieces"));
        for row in &t.rows {
            let shown = row[text]
                .matches("\u{27E8}one-time code hidden\u{27E9}")
                .count();
            assert_eq!(row[masked], shown.to_string(), "{what}");
        }
        prefixed_total += prefixed;
        exported += 1;
    }
    assert!(exported >= 20);
    assert!(prefixed_total >= 30, "{prefixed_total}");
}

/// Every cell of `tables`, unprefixed where ours, with the file and
/// column.
fn cells(tables: &BTreeMap<String, Table>) -> Vec<(String, String, String, bool)> {
    let mut out = Vec::new();
    for (file, t) in tables {
        let at = t.col(NEUTRALISED);
        for row in &t.rows {
            let named: Vec<&str> = row[at].split(';').collect();
            for (k, cell) in row.iter().enumerate() {
                let name = &t.header[k];
                let ours = named.contains(&name.as_str());
                let v = if ours {
                    cell[1..].to_owned()
                } else {
                    cell.clone()
                };
                out.push((file.clone(), name.clone(), v, ours));
            }
        }
    }
    out
}

#[test]
fn every_planted_formula_case_is_neutralised_for_spreadsheets_only() {
    for name in ["export-text-plain", "export-files-plain"] {
        let doc = doc(name);
        let e = doc.export.clone().expect("export");
        let i = imported(&doc);
        for profile in [CsvProfile::Spreadsheet, CsvProfile::DataTools] {
            let d = files::dest();
            let tweak = |o: &mut ExportOptions| o.csv_profile = Some(profile);
            export_csv(&i, &d.parent, &tweak);
            let spreadsheet = profile == CsvProfile::Spreadsheet;
            let tables = read_bundle(&d.parent.join(BUNDLE), spreadsheet);
            check_neutralisation(&tables, spreadsheet);
            let all = cells(&tables);
            let mut found = 0;
            for case in e["formulaCases"].as_array().expect("cases") {
                let text = case["text"].as_str().expect("text");
                let id = case["id"].as_str().expect("id");
                let hits: Vec<_> = all.iter().filter(|(_, _, v, _)| v == text).collect();
                assert!(!hits.is_empty(), "{name}: {id} not found");
                let second_line = case["after"] == "LF";
                for (file, col, _, ours) in hits {
                    assert_eq!(
                        *ours,
                        spreadsheet && !second_line,
                        "{name}: {id} in {file} {col}"
                    );
                }
                found += 1;
            }
            assert!(found >= 4, "{name}: {found}");
        }
    }
}

#[test]
fn quoting_and_controls_round_trip_by_profile() {
    let doc = doc("export-text-plain");
    let e = doc.export.clone().expect("export");
    let i = imported(&doc);
    let quoting: BTreeMap<String, String> = e["hostile"]
        .as_array()
        .expect("hostile")
        .iter()
        .filter(|h| h["where"] == "body" && h["text"].is_string())
        .map(|h| {
            (
                h["id"].as_str().expect("id").to_owned(),
                h["text"].as_str().expect("text").to_owned(),
            )
        })
        .collect();
    for profile in [CsvProfile::Spreadsheet, CsvProfile::DataTools] {
        let d = files::dest();
        let tweak = |o: &mut ExportOptions| o.csv_profile = Some(profile);
        export_csv(&i, &d.parent, &tweak);
        let spreadsheet = profile == CsvProfile::Spreadsheet;
        let tables = read_bundle(&d.parent.join(BUNDLE), spreadsheet);
        let t = &tables["messages.csv"];
        let col = t.col("text");
        let texts: Vec<&String> = t.rows.iter().map(|r| &r[col]).collect();
        for id in [
            "body-quotes",
            "body-crlf",
            "body-lone-cr",
            "body-lf",
            "body-nul",
            "body-esc",
        ] {
            let raw = &quoting[id];
            // The profile's escapes, applied by hand.
            let mut want = String::new();
            for c in raw.chars() {
                let cp = u32::from(c);
                let control =
                    !matches!(c, '\t' | '\r' | '\n') && (cp < 0x20 || (0x7F..=0x9F).contains(&cp));
                if c == '\u{27E8}' || (spreadsheet && control) {
                    want.push_str(&format!("\u{27E8}U+{cp:04X}\u{27E9}"));
                } else {
                    want.push(c);
                }
            }
            assert!(
                texts.iter().any(|t| **t == want),
                "{id} ({profile:?}) did not round-trip"
            );
        }
        // No raw control from content in the spreadsheet profile.
        if spreadsheet {
            for (_, _, v, _) in cells(&tables) {
                assert!(
                    !v.chars().any(|c| {
                        let cp = u32::from(c);
                        !matches!(c, '\t' | '\r' | '\n')
                            && (cp < 0x20 || (0x7F..=0x9F).contains(&cp))
                    }),
                    "a raw control"
                );
            }
        }
        // The reserved character never stands alone at a marker's start.
        for (_, _, v, _) in cells(&tables) {
            let mut it = v.match_indices('\u{27E8}');
            for (k, _) in it.by_ref() {
                let rest = &v[k + '\u{27E8}'.len_utf8()..];
                assert!(
                    rest.starts_with("U+")
                        || rest.starts_with("one-time code hidden")
                        || rest.starts_with("Attachment ")
                        || rest.starts_with("Inline emoji image")
                        || rest.starts_with("Attachment placeholder"),
                    "an unescaped U+27E8"
                );
            }
        }
    }
}

#[test]
fn files_left_out_by_an_option_hold_their_header_only() {
    let doc = doc("export-hist-1-plain");
    let i = imported(&doc);
    let d = files::dest();
    let off = |o: &mut ExportOptions| {
        o.history = false;
        o.reactions = false;
    };
    export_csv(&i, &d.parent, &off);
    let tables = read_bundle(&d.parent.join(BUNDLE), true);
    assert!(tables["edit_versions.csv"].rows.is_empty());
    assert!(tables["reactions.csv"].rows.is_empty());
    assert!(!tables["messages.csv"].rows.is_empty());
    let t = &tables["messages.csv"];
    let (h, r) = (t.col("history_state"), t.col("reactions_state"));
    assert!(t
        .rows
        .iter()
        .all(|x| x[h] == "notIncluded" && x[r] == "notIncluded"));
    // With history on, the versions are there.
    let d = files::dest();
    export_csv(&i, &d.parent, &|o: &mut ExportOptions| o.history = true);
    let tables = read_bundle(&d.parent.join(BUNDLE), true);
    assert!(!tables["edit_versions.csv"].rows.is_empty());
}

#[test]
fn identifiers_fill_the_source_columns_only_under_the_opt_in() {
    use oma_export::ids::IdentifierMode;
    let doc = doc("view-main-plain");
    let i = imported(&doc);
    for mode in [IdentifierMode::Ordinals, IdentifierMode::ArchiveAndSource] {
        let d = files::dest();
        export_csv(&i, &d.parent, &|o: &mut ExportOptions| o.identifiers = mode);
        let tables = read_bundle(&d.parent.join(BUNDLE), true);
        for (file, t) in &tables {
            for (k, name) in t.header.iter().enumerate() {
                if !name.starts_with("source_") {
                    continue;
                }
                let filled = t.rows.iter().filter(|r| !r[k].is_empty()).count();
                if mode == IdentifierMode::Ordinals {
                    assert_eq!(filled, 0, "{file} {name}");
                } else if name == "source_archive_id" && !t.rows.is_empty() {
                    assert_eq!(filled, t.rows.len(), "{file} {name}");
                    assert!(t.rows.iter().all(|r| Kind::Int.matches(&r[k])));
                }
            }
        }
    }
}

#[test]
fn value_json_and_csv_agree_on_every_message_row() {
    // A minimal in-Rust cross-check (the Python oracle does the full one):
    // the same export's JSON items and CSV rows carry the same ordinals in
    // the same order, with the same text for plain bodies.
    let doc = doc("export-text-plain");
    let i = imported(&doc);
    let d = files::dest();
    let tweak = |o: &mut ExportOptions| {
        o.attachments = oma_export::plan::AttachmentMode::None;
        o.csv_profile = Some(CsvProfile::DataTools);
    };
    export_formats(
        &i,
        &d.parent,
        ScopePlan::Archive,
        &[Format::Csv, Format::Json],
        &tweak,
        BUNDLE,
        None,
        &ExportZone::Utc,
    )
    .expect("export");
    let bundle = d.parent.join(BUNDLE);
    let tables = read_bundle(&bundle, false);
    let t = &tables["messages.csv"];
    let mut json_rows: Vec<(u64, Option<u64>, String)> = Vec::new();
    let mut files: Vec<String> = std::fs::read_dir(bundle.join("json"))
        .expect("json")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".jsonl"))
        .collect();
    files.sort_by_key(|n| (n == "no-conversation.jsonl", n.clone()));
    for f in files {
        let text = std::fs::read_to_string(bundle.join("json").join(f)).expect("read");
        for l in text.lines().skip(1) {
            let v: Value = serde_json::from_str(l).expect("line");
            if v["type"] != "item" {
                continue;
            }
            let body = match v["body"]["kind"].as_str() {
                Some("text") => v["body"]["segments"]
                    .as_array()
                    .expect("segments")
                    .iter()
                    .filter_map(|s| s["text"].as_str())
                    .collect::<String>(),
                _ => String::new(),
            };
            json_rows.push((
                v["n"].as_u64().expect("n"),
                v["context"]["conversation"].as_u64(),
                body,
            ));
        }
    }
    assert_eq!(json_rows.len(), t.rows.len());
    let (n, c, text) = (t.col("item"), t.col("conversation"), t.col("text"));
    for ((jn, jc, jt), row) in json_rows.iter().zip(&t.rows) {
        assert_eq!(row[n], jn.to_string());
        assert_eq!(row[c], jc.map(|x| x.to_string()).unwrap_or_default());
        if !row[text].contains('\u{27E8}') {
            assert_eq!(&row[text], jt, "item {jn}");
        }
    }
}
