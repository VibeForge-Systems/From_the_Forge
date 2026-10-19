//! Provenance (M6 spec §7, D135): `README.txt`, then `manifest.json`, then
//! `SHA256SUMS`.
//!
//! - **Order.** Content files first, then `README.txt`, then
//!   `manifest.json` (whose `files` lists every file so far, `README.txt`
//!   included), then `SHA256SUMS` (every file, `manifest.json` included),
//!   then the staging folder is flushed. [`write_provenance`] is the only
//!   writer of the three.
//! - **Identities.** The counts satisfy the four stated identities
//!   ([`ManifestCounts::check_identities`]); a manifest that fails them is
//!   not written (`Limit(RowCountMismatch)`).
//! - **Never written** (D135): the search query, the count of hits
//!   excluded under §3.6, device facts (name, model, iOS version, the
//!   "untested iOS version" flag), the archive id or label, UDIDs, GUIDs
//!   (outside D141), backup paths, the destination path, storage names of
//!   files not exported, or the WebView engine. [`ManifestBody`] has no
//!   field that could hold any of them: every value is a number, a
//!   boolean, a closed token, a validated zone name, a date, or a path of
//!   this bundle built by `naming.rs`.
//! - **`files`** covers every file except `SHA256SUMS`, `manifest.json`
//!   itself (which cannot carry its own hash), the marker and anything
//!   under `.xlsx-tmp` (removed before this point, slice 6i).
//! - **`exclusions`** come from the column ledger (`ledger::exclusions`),
//!   followed by two export-level notes that are not columns.

use std::collections::BTreeMap;
use std::io::Write;

use serde::Serialize;

use crate::codes::ExportCode;
use crate::error::{ExportError, LimitKind};
use crate::fault::{self, FaultPoint};
use crate::ids::{IdentifierMode, Ordinal};
use crate::jsonfmt;
use crate::ledger;
use crate::map::{ExportZone, RecordCounts};
use crate::naming::{BundlePath, EntryId};
use crate::plan::{
    AttachmentMode, Clock, CsvProfile, ExportPlan, LineEndings, Packaging, PageSize, ScopePlan,
};
use crate::stage::{FileEntry, Staging};
use crate::time::{format_iso, CivilDate, Precision};
use crate::words::readme;

/// `format` of the manifest.
pub const MANIFEST_FORMAT: &str = "openmessage-archive-export-manifest";
pub const MANIFEST_VERSION: &str = "1.0.0";
/// The JSON record schema's version (spec §5.1).
pub const SCHEMA_VERSION: &str = "1.0.0";
/// The CSV and XLSX column contract's version (spec §5.2).
pub const TABULAR_COLUMNS_VERSION: &str = "1.0.0";

/// Export-level exclusions that are not archive columns.
pub const EXPORT_NOTES: [&str; 2] = [
    "recentlyDeletedOnlyNeverImported",
    "attachmentsNotConverted",
];

const NS_PER_MS: i64 = 1_000_000;

fn limit(k: LimitKind) -> ExportError {
    ExportError::Limit(k)
}

/// The snapshot an export read (§3.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotFacts {
    pub kind: &'static str,
    pub archive_lock_held: bool,
}

impl SnapshotFacts {
    /// One read transaction under the archive lock (D124).
    pub const JOB: SnapshotFacts = SnapshotFacts {
        kind: "singleReadTransaction",
        archive_lock_held: true,
    };
}

/// `scope`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopeFacts {
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_local_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until_local_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_utc: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until_utc: Option<String>,
    pub conversations: u64,
}

fn date_text(d: CivilDate) -> String {
    format!("{:04}-{:02}-{:02}", d.year, d.month, d.day)
}

fn utc_text(ns: i64) -> String {
    format_iso(ns, None, Precision::Seconds)
}

impl ScopeFacts {
    /// The facts of `scope` with `conversations` conversation files, its
    /// local dates turned into instants in `zone` (the same arithmetic as
    /// the export's reads, `ZoneTable::day_range`).
    pub fn new(
        scope: &ScopePlan,
        zone: &ExportZone,
        conversations: u64,
    ) -> Result<ScopeFacts, ExportError> {
        let mut f = ScopeFacts {
            kind: scope.kind(),
            from_local_date: None,
            until_local_date: None,
            from_utc: None,
            until_utc: None,
            conversations,
        };
        if let ScopePlan::DateRange { from, until, .. } = scope {
            let (a, b) = match zone {
                ExportZone::Table(t) => t.day_range(*from, *until)?,
                ExportZone::Utc => {
                    if until <= from {
                        return Err(ExportError::DateRange(
                            crate::error::DateRangeRefusal::Empty,
                        ));
                    }
                    let day = 86_400 * 1_000_000_000_i64;
                    (from.days_from_epoch() * day, until.days_from_epoch() * day)
                }
            };
            f.from_local_date = Some(date_text(*from));
            f.until_local_date = Some(date_text(*until));
            f.from_utc = Some(utc_text(a));
            f.until_utc = Some(utc_text(b));
        }
        Ok(f)
    }
}

/// `options.reveal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RevealFacts {
    pub one_time_codes: bool,
    pub earlier_content: bool,
    pub confirmed_up_to: Option<u64>,
}

/// `options`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OptionFacts {
    pub attachments: &'static str,
    pub keep_extensions: bool,
    pub previews: bool,
    pub larger_previews: bool,
    pub history: bool,
    pub reactions: bool,
    pub events: bool,
    pub reveal: RevealFacts,
    pub clock: &'static str,
    pub seconds: bool,
    pub line_endings: &'static str,
    pub hidden_character_markers: bool,
    pub csv_profile: Option<&'static str>,
    pub identifiers: &'static str,
    pub page_size: Option<&'static str>,
}

impl OptionFacts {
    pub fn new(plan: &ExportPlan) -> OptionFacts {
        let o = plan.options();
        OptionFacts {
            attachments: match o.attachments {
                AttachmentMode::None => "namesOnly",
                AttachmentMode::CopyOriginals => "copyOriginals",
            },
            keep_extensions: o.keep_extensions,
            previews: o.previews,
            larger_previews: o.larger_previews,
            history: o.history,
            reactions: o.reactions,
            events: o.events,
            reveal: RevealFacts {
                one_time_codes: o.reveal.one_time_codes,
                earlier_content: o.reveal.earlier_content,
                confirmed_up_to: o.reveal.confirmed_up_to,
            },
            clock: match o.clock {
                Clock::H23 => "h23",
                Clock::H12 => "h12",
            },
            seconds: o.seconds,
            line_endings: match o.line_endings {
                LineEndings::Lf => "lf",
                LineEndings::Crlf => "crlf",
            },
            hidden_character_markers: o.hidden_character_markers,
            csv_profile: o.csv_profile.map(|p| match p {
                CsvProfile::Spreadsheet => "spreadsheet",
                CsvProfile::DataTools => "dataTools",
            }),
            identifiers: match o.identifiers {
                IdentifierMode::Ordinals => "ordinals",
                IdentifierMode::ArchiveAndSource => "archiveAndSource",
            },
            page_size: o.page_size.map(|p| match p {
                PageSize::A4 => "a4",
                PageSize::Letter => "letter",
            }),
        }
    }
}

/// `zone`: the name (validated, `^[A-Za-z0-9_+\-/]{1,64}$`), its source,
/// the window and the transitions the times were written with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ZoneFacts {
    pub name: String,
    pub source: &'static str,
    pub window_utc: Option<[String; 2]>,
    pub transitions: Vec<(String, i32)>,
}

impl ZoneFacts {
    pub fn new(zone: &ExportZone) -> ZoneFacts {
        match zone {
            ExportZone::Utc => ZoneFacts {
                name: "UTC".to_owned(),
                source: "utc",
                window_utc: None,
                transitions: Vec::new(),
            },
            ExportZone::Table(t) => {
                let (a, b) = t.window_ms();
                ZoneFacts {
                    name: t.name().to_owned(),
                    source: "intl",
                    window_utc: Some([
                        utc_text(a.saturating_mul(NS_PER_MS)),
                        utc_text(b.saturating_mul(NS_PER_MS)),
                    ]),
                    transitions: t
                        .transitions()
                        .iter()
                        .map(|&(ms, o)| (utc_text(ms.saturating_mul(NS_PER_MS)), o))
                        .collect(),
                }
            }
        }
    }
}

/// `derivation`: rows by deriver and decoder version.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DerivationFacts {
    pub derived_by: BTreeMap<String, u64>,
    pub decoder_version: BTreeMap<String, u64>,
}

/// `counts.attachments`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentCounts {
    pub records: u64,
    pub with_file: u64,
    pub not_requested: u64,
    /// By `cardState` token or export code token.
    pub excluded: BTreeMap<String, u64>,
    pub files_written: u64,
    pub deduplicated: u64,
    pub blocked: u64,
    pub names_shortened: u64,
    pub previews_embedded: u64,
    pub previews_unavailable: u64,
    pub previews_skipped_after_helper_failures: u64,
    pub motw_not_written: u64,
}

/// `counts` (spec §7.1).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestCounts {
    pub conversations: u64,
    pub timeline_rows: u64,
    pub distinct_messages: u64,
    pub cross_conversation_rows: u64,
    pub folded_reaction_rows: u64,
    pub items: u64,
    pub undated_excluded: u64,
    pub before_2007: u64,
    pub no_text: u64,
    pub lossy: u64,
    pub order_ties: u64,
    pub events_not_included: u64,
    pub one_time_codes_masked: u64,
    pub bodies_withheld_whole: u64,
    pub earlier_text_withheld: u64,
    pub edit_text_withheld: u64,
    pub alternative_text_withheld: u64,
    pub hidden_characters_marked: u64,
    pub controls_escaped: u64,
    pub combining_runs_limited: u64,
    pub neutralised_cells: u64,
    pub continuation_rows: u64,
    pub pdf_undrawable_characters: u64,
    pub pdf_forced_wraps: u64,
    pub times_outside_zone_window: u64,
    pub attachments: AttachmentCounts,
}

impl ManifestCounts {
    /// The counts the record mapping kept, with every export exclusion
    /// code present (zero when unused).
    pub fn from_records(r: &RecordCounts) -> ManifestCounts {
        use crate::codes::AttachmentOutcome as O;
        let mut excluded: BTreeMap<String, u64> = [
            O::WithheldUnsent,
            O::ChecksumMismatch,
            O::TooLargeForVolume,
            O::WriteFailed,
        ]
        .into_iter()
        .map(|o| (o.token().to_owned(), 0))
        .collect();
        for (k, v) in &r.attachments_excluded {
            *excluded.entry(k.clone()).or_insert(0) += v;
        }
        ManifestCounts {
            before_2007: r.before_2007,
            no_text: r.no_text,
            lossy: r.lossy,
            order_ties: r.order_ties,
            one_time_codes_masked: r.one_time_codes_masked,
            earlier_text_withheld: r.earlier_text_withheld,
            edit_text_withheld: r.edit_text_withheld,
            alternative_text_withheld: r.alternative_text_withheld,
            times_outside_zone_window: r.times_outside_zone_window,
            attachments: AttachmentCounts {
                records: r.attachment_records,
                with_file: r.attachments_with_file,
                not_requested: r.attachments_not_requested,
                excluded,
                files_written: r.attachments_with_file.saturating_sub(r.files_deduplicated),
                deduplicated: r.files_deduplicated,
                blocked: r.files_blocked,
                ..AttachmentCounts::default()
            },
            ..ManifestCounts::default()
        }
    }

    /// The counting identities of §7.1:
    ///
    /// - `items = timelineRows − foldedReactionRows`;
    /// - `crossConversationRows = timelineRows − distinctMessages`;
    /// - `attachments.records = withFile + notRequested + Σ excluded`;
    /// - `filesWritten = withFile − deduplicated`.
    pub fn check_identities(&self) -> Result<(), ExportError> {
        let a = &self.attachments;
        let excluded = a.excluded.values().try_fold(0u64, |s, v| s.checked_add(*v));
        let ok = self.timeline_rows.checked_sub(self.folded_reaction_rows) == Some(self.items)
            && self.timeline_rows.checked_sub(self.distinct_messages)
                == Some(self.cross_conversation_rows)
            && excluded
                .and_then(|e| e.checked_add(a.with_file))
                .and_then(|e| e.checked_add(a.not_requested))
                == Some(a.records)
            && a.with_file.checked_sub(a.deduplicated) == Some(a.files_written);
        if ok {
            Ok(())
        } else {
            Err(limit(LimitKind::RowCountMismatch))
        }
    }
}

/// `search`, for a search scope (D135, D137): never the query, never the
/// count of hits excluded under §3.6.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchFacts {
    pub mode: &'static str,
    pub filter_kinds: Vec<&'static str>,
    pub include_earlier_for_matching: bool,
    pub matches_written: u64,
    pub capped: bool,
    pub newest_kept: bool,
    pub coverage_complete: bool,
    pub all_matches: bool,
}

/// One import session the export's labels or observations reference, by
/// export-local ordinal. Never a device fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportSessionFact {
    pub session: Ordinal,
    pub backup_date: Option<CivilDate>,
    /// A closed token (`Status.plist`, …).
    pub date_source: &'static str,
    pub completed: bool,
}

impl Serialize for ImportSessionFact {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("ImportSession", 4)?;
        st.serialize_field("session", &self.session)?;
        st.serialize_field("backupDate", &self.backup_date.map(date_text))?;
        st.serialize_field("dateSource", self.date_source)?;
        st.serialize_field("completed", &self.completed)?;
        st.end()
    }
}

/// Everything in `manifest.json` except `files` (and so, in prose, a
/// single file's "About this export", §7.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestBody {
    pub format: &'static str,
    pub manifest_version: &'static str,
    pub schema_version: &'static str,
    pub tabular_columns_version: &'static str,
    pub app_version: &'static str,
    pub archive_schema_version: u32,
    pub export_id: String,
    pub created_utc: String,
    pub snapshot: SnapshotFacts,
    pub scope: ScopeFacts,
    pub formats: Vec<&'static str>,
    pub options: OptionFacts,
    pub zone: ZoneFacts,
    pub derivation: DerivationFacts,
    pub counts: ManifestCounts,
    pub search: Option<SearchFacts>,
    pub import_sessions: Vec<ImportSessionFact>,
    pub exclusions: Vec<&'static str>,
}

/// What a manifest is built from.
#[derive(Debug, Clone)]
pub struct ManifestInput<'a> {
    pub plan: &'a ExportPlan,
    pub zone: &'a ExportZone,
    pub export_id: EntryId,
    pub created_utc_ns: i64,
    /// The application's version (the shell's `CARGO_PKG_VERSION`).
    pub app_version: &'static str,
    pub conversations: u64,
    pub counts: ManifestCounts,
    pub derivation: DerivationFacts,
    pub search: Option<SearchFacts>,
    pub import_sessions: Vec<ImportSessionFact>,
}

impl ManifestBody {
    pub fn new(input: ManifestInput<'_>) -> Result<ManifestBody, ExportError> {
        let mut exclusions: Vec<&'static str> = ledger::exclusions()
            .into_iter()
            .map(|r| r.token())
            .collect();
        exclusions.extend(EXPORT_NOTES);
        Ok(ManifestBody {
            format: MANIFEST_FORMAT,
            manifest_version: MANIFEST_VERSION,
            schema_version: SCHEMA_VERSION,
            tabular_columns_version: TABULAR_COLUMNS_VERSION,
            app_version: input.app_version,
            archive_schema_version: oma_archive::SCHEMA_VERSION,
            export_id: input.export_id.hex(),
            created_utc: utc_text(input.created_utc_ns),
            snapshot: SnapshotFacts::JOB,
            scope: ScopeFacts::new(input.plan.scope(), input.zone, input.conversations)?,
            formats: input.plan.formats().iter().map(|f| f.token()).collect(),
            options: OptionFacts::new(input.plan),
            zone: ZoneFacts::new(input.zone),
            derivation: input.derivation,
            counts: input.counts,
            search: input.search,
            import_sessions: input.import_sessions,
            exclusions,
        })
    }
}

/// One entry of `files`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileFact {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

impl FileFact {
    fn of(e: &FileEntry) -> FileFact {
        FileFact {
            path: e.path().to_slash(),
            bytes: e.bytes(),
            sha256: e.sha256_hex(),
        }
    }
}

/// `manifest.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Manifest<'a> {
    #[serde(flatten)]
    pub body: &'a ManifestBody,
    pub files: Vec<FileFact>,
}

/// `README.txt` for `body`, in the plan's line endings, ending with one.
pub fn readme_text(plan: &ExportPlan) -> String {
    let nl = match plan.options().line_endings {
        LineEndings::Lf => "\n",
        LineEndings::Crlf => "\r\n",
    };
    let mut s = String::new();
    s.push_str(readme::TITLE);
    s.push_str(nl);
    for p in readme::paragraphs(plan.formats(), plan.options().identifiers) {
        s.push_str(nl);
        s.push_str(p);
        s.push_str(nl);
    }
    s
}

/// `SHA256SUMS` over `files` (coreutils format: `<hex>  <path>`, `/`
/// separators, LF), sorted by path. Names of this bundle never hold a
/// backslash or a newline (`sanitize_filename` replaces both), so no line
/// needs coreutils' escaped form.
pub fn sums_text(files: &[FileEntry]) -> String {
    let mut lines: Vec<(String, String)> = files
        .iter()
        .map(|e| (e.path().to_slash(), e.sha256_hex()))
        .collect();
    lines.sort();
    let mut s = String::new();
    for (path, hex) in lines {
        s.push_str(&hex);
        s.push_str("  ");
        s.push_str(&path);
        s.push('\n');
    }
    s
}

/// What [`write_provenance`] wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvenanceReport {
    /// Files listed in `manifest.json`'s `files`.
    pub files_listed: usize,
    /// Lines of `SHA256SUMS`.
    pub sums_lines: usize,
}

fn write_file(staging: &mut Staging, path: &BundlePath, text: &str) -> Result<(), ExportError> {
    let mut f = staging.create(path)?;
    f.write_all(text.as_bytes())
        .map_err(|e| ExportError::io(&e))?;
    staging.close(f)?;
    Ok(())
}

/// Write a bundle's provenance into `staging` (§7.2, §8.3 step 4), after
/// every content file is closed: `README.txt`, `manifest.json`,
/// `SHA256SUMS`, then flush the files and the staging folders.
pub fn write_provenance(
    staging: &mut Staging,
    plan: &ExportPlan,
    body: &ManifestBody,
) -> Result<ProvenanceReport, ExportError> {
    if staging.packaging() != Packaging::Bundle {
        return Err(limit(LimitKind::RowOrder));
    }
    if staging.open_files() != 0 {
        return Err(limit(LimitKind::FileStillOpen));
    }
    body.counts.check_identities()?;
    staging.sync_batch()?;
    fault::point(FaultPoint::BeforeReadme);
    write_file(staging, &BundlePath::readme(), &readme_text(plan))?;
    let mut files: Vec<FileFact> = staging.files().iter().map(FileFact::of).collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let files_listed = files.len();
    let manifest = Manifest { body, files };
    write_file(
        staging,
        &BundlePath::manifest(),
        &jsonfmt::to_pretty(&manifest)?,
    )?;
    let sums = sums_text(staging.files());
    let sums_lines = staging.files().len();
    write_file(staging, &BundlePath::sums(), &sums)?;
    staging.sync_batch()?;
    staging.sync_dirs()?;
    fault::point(FaultPoint::AfterSums);
    Ok(ProvenanceReport {
        files_listed,
        sums_lines,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts() -> ManifestCounts {
        let mut c = ManifestCounts {
            conversations: 2,
            timeline_rows: 4213,
            distinct_messages: 4209,
            cross_conversation_rows: 4,
            folded_reaction_rows: 312,
            items: 3901,
            ..ManifestCounts::default()
        };
        c.attachments = AttachmentCounts {
            records: 310,
            with_file: 298,
            not_requested: 0,
            excluded: [
                ("notFoundAtAssumedKey".to_owned(), 9),
                ("copyOffForImport".to_owned(), 3),
            ]
            .into_iter()
            .collect(),
            files_written: 292,
            deduplicated: 6,
            ..AttachmentCounts::default()
        };
        c
    }

    #[test]
    fn the_spec_example_satisfies_the_identities_and_each_break_is_caught() {
        assert!(counts().check_identities().is_ok());
        let breaks: [&dyn Fn(&mut ManifestCounts); 5] = [
            &|c| c.items += 1,
            &|c| c.cross_conversation_rows += 1,
            &|c| c.attachments.records += 1,
            &|c| c.attachments.files_written -= 1,
            &|c| {
                c.folded_reaction_rows = c.timeline_rows + 1;
            },
        ];
        for b in breaks {
            let mut c = counts();
            b(&mut c);
            assert!(matches!(
                c.check_identities(),
                Err(ExportError::Limit(LimitKind::RowCountMismatch))
            ));
        }
    }

    #[test]
    fn record_counts_carry_over_with_every_export_exclusion_code() {
        let mut r = RecordCounts {
            attachment_records: 5,
            attachments_with_file: 3,
            attachments_not_requested: 1,
            files_deduplicated: 1,
            ..RecordCounts::default()
        };
        r.attachments_excluded
            .insert("withheldUnsent".to_owned(), 1);
        let c = ManifestCounts::from_records(&r);
        assert_eq!(
            c.attachments.excluded.keys().cloned().collect::<Vec<_>>(),
            [
                "checksumMismatch",
                "tooLargeForVolume",
                "withheldUnsent",
                "writeFailed"
            ]
        );
        assert_eq!(c.attachments.files_written, 2);
        assert!(c.check_identities().is_ok());
    }

    #[test]
    fn sums_use_the_coreutils_form_sorted_by_path() {
        assert_eq!(sums_text(&[]), "");
        let t = sums_text(&[]);
        assert!(!t.contains('\r'));
    }
}
