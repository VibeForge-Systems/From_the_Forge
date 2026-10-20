//! The export engine of OpenMessage Archive (Milestone 6). It does not
//! depend on Tauri and never names SQLite: every archive read goes through
//! `oma-archive`'s export APIs (`with_export_snapshot`, its cursors and
//! `ExportSnapshot::assemble`), which apply the masking of D115 and owner
//! Q4 before anything reaches this crate (D123).
//!
//! Slice 6d, the record core:
//!
//! - [`plan`]: the validated [`ExportPlan`] (formats, options, the two
//!   reveal opt-ins, the packaging of D126 and owner Q3a's attachment
//!   defaults);
//! - [`codes`]: the code registry, explicit never-reused numbers for every
//!   enum a record carries;
//! - [`ledger`]: every v1 column's disposition (exported raw, derived, or
//!   excluded for a reason code), from which a manifest's `exclusions`
//!   are generated;
//! - [`record`]: the canonical record, dedicated structs whose serde form
//!   is the JSON format, with tagged absence;
//! - [`map`]: export reads to records, with the release-mode `Limit`
//!   checks;
//! - [`ids`]: export-local ordinals by default (owner Q5's lean, awaiting
//!   confirmation), archive and source identifiers only under an opt-in;
//! - [`words`]: Rust sentence tables over every code (`export-words.json`
//!   is checked against `viewerText.ts`);
//! - [`time`]: the validated zone table, the clamped window, Rust civil
//!   arithmetic and local-day bounds, the stale-table refusal and UTC
//!   outliers;
//! - [`hidden`]: hidden-character markers (D121's rule inverted), the
//!   reserved `⟨` marker form, and the combining-run limit.
//!
//! Slice 6e1, the files core:
//!
//! - [`naming`]: the only builder of paths inside or beside a bundle
//!   (fixed names, the ordinal grammar, attachment names through
//!   `oma_fs::sanitize_filename_max` within the path budget, shards, the
//!   passive allowlist of owner Q3b, the staging entries' names);
//! - [`stage`]: the staging folder or `.partial-file` inside the
//!   destination, written relative to `O_DIRECTORY | O_NOFOLLOW` handles
//!   on Linux, the locked marker kept until after the no-replace publish,
//!   removal on cancel or failure, and the sweep of crash leftovers;
//! - [`manifest`]: `README.txt`, `manifest.json` and `SHA256SUMS`, in that
//!   order, with the counting identities;
//! - [`writer`]: the [`ExportWriter`] trait and the [`FanOut`] that drives
//!   every format with the same records;
//! - [`fault`]: crash points for the kill tests.
//!
//! Slice 6e2, attachments, the first writer and the pipeline:
//!
//! - [`attach`]: the attachment copy through the archive's `BlobReader`
//!   to final names inside staging, hashed once, deduplicated per blob,
//!   defused by the passive allowlist (owner Q3b), with a Mark of the Web
//!   on Windows (Q3c) and the FAT 4 GiB rule;
//! - [`writer::txt`]: the TXT writer and its line grammar (D138);
//! - [`pipeline`]: one export from the pre-count to the publish
//!   (`pipeline::run`, the spec's `Exporter::run`).
//!
//! Slice 6f, the structured writers:
//!
//! - [`writer::json`]: JSON Lines per conversation file plus the
//!   conversation and handle tables, or one streamed single file, through
//!   [`jsonfmt::to_line`]; the open, strict and manifest schemas
//!   (`schema/`, made by `scripts/make-export-schemas.py`);
//! - [`writer::csv`]: nine RFC 4180 tables through `csv`'s
//!   `WriterBuilder::from_writer`, both profiles, default-deny formula
//!   neutralisation listed per row and counted.
//!
//! Slice 6g, HTML:
//!
//! - [`writer::html`]: static pages from one fixed template (escaping
//!   into text nodes only, markers as elements, the CSP meta first with
//!   the one hashed stylesheet, the days' navigation after `main`,
//!   backward-only reply links, pages split by items and bytes) and the
//!   bundle's `html/index.html`;
//! - [`preview`]: the [`preview::PreviewSource`] the HTML writer asks for
//!   decode-helper renditions, and the checks every rendition passes
//!   before it is embedded as `data:` or written as a larger preview.
//!
//! Slice 6i, the export-render helper (spec §5.7, D133):
//!
//! - [`helper`]: the frames (bounded at 16 MiB, checked before
//!   allocating) and the main process's [`helper::RenderSession`] (a
//!   reader thread, a supervisor that kills on cancel, stalls and the
//!   120 s reply timeout, and the output check before any byte is
//!   returned);
//! - [`check`]: the main process's checks on helper output (PDF);
//! - `render` (feature `render`, enabled only by the shell): the helper's
//!   own `serve` loop and, in debug builds, its test backend.
//!
//! Slice 6j1, PDF layout (spec §5.6; owner Q1, Q2):
//!
//! - [`pdf`]: the bundled fonts' pins and their check before any byte
//!   reaches the helper, and the block model the helper lays out, built
//!   with the marker rule (§4.7);
//! - `render::pdf` (feature `render`): the helper's PDF backend, parley
//!   for shaping, bidi and line breaking, krilla for the PDF.
//!
//! Slice 6j2, the PDF writer: `writer::pdf` maps records to blocks (the
//! item's lines shared with TXT through `writer::lines`), cuts volumes by
//! an estimate, sends checked previews, and stages the volumes the helper
//! returns checked.
//!
//! Slice 6k2: [`copy`], "Save a copy" of one attachment (no manifest, no
//! records: the verified bytes under the user's name, placed no-replace
//! or by a confirmed replace, with a Mark of the Web on Windows), and the
//! "events" option off (the snapshot leaves group and system events out,
//! [`pipeline`]).
//!
//! PDF is the helper's first real backend; XLSX is deferred
//! (its writer crate is not yet old enough to pin) and stays refused.
//! Nothing here logs; errors carry kinds only (I6).
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

pub mod attach;
pub mod check;
pub mod codes;
pub mod copy;
pub mod error;
pub mod fault;
pub mod helper;
pub mod hidden;
pub mod ids;
pub mod jsonfmt;
pub mod ledger;
pub mod manifest;
pub mod map;
pub mod naming;
pub mod pdf;
pub mod pipeline;
pub mod plan;
pub mod preview;
pub mod record;
#[cfg(feature = "render")]
pub mod render;
pub mod stage;
pub mod time;
mod unicode_tables;
pub mod words;
pub mod writer;

pub use codes::{Code, ExportCode, Registry, REGISTRIES, UNRECOGNISED_TOKEN};
pub use error::{
    DateRangeRefusal, ExportError, HelperFailure, LimitKind, PlanRefusal, ZoneTableRefusal,
};
pub use ids::{IdentifierMode, Ordinal, Ordinals};
pub use manifest::{write_provenance, ManifestBody, ManifestCounts, ManifestInput};
pub use map::{
    order_ties, AttachmentFiles, ExportZone, FileDecision, FileOutcome, Mapper, Placement,
    RecordCounts,
};
pub use naming::{BundlePath, EntryId, EntryKind, EntryName, TargetName};
pub use plan::{
    AttachmentMode, CsvProfile, ExportOptions, ExportPlan, Format, Packaging, RecordOptions,
    RevealPlan, ScopePlan,
};
pub use record::{ConversationRecord, HandleRecord, ItemRecord, VersionRecord};
pub use stage::{
    find_leftovers, probe_writable, remove_leftovers, FileEntry, Leftover, LeftoverScan, Published,
    Replace, StagedFile, Staging, SweepReport, SWEEP_MIN_AGE,
};
pub use time::{CivilDate, ZoneTable, ZoneTableInput};
pub use writer::{
    AboutDraft, ConversationFile, ConversationTotals, ExportContext, ExportWriter, FanOut,
    WriterReport,
};
