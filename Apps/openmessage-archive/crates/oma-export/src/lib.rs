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
//! The writers, staging, attachment copy and manifest follow in slices 6e
//! to 6g. Nothing here logs; errors carry kinds only (I6).
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

pub mod codes;
pub mod error;
pub mod hidden;
pub mod ids;
pub mod ledger;
pub mod map;
pub mod plan;
pub mod record;
pub mod time;
mod unicode_tables;
pub mod words;

pub use codes::{Code, ExportCode, Registry, REGISTRIES, UNRECOGNISED_TOKEN};
pub use error::{DateRangeRefusal, ExportError, LimitKind, PlanRefusal, ZoneTableRefusal};
pub use ids::{IdentifierMode, Ordinal, Ordinals};
pub use map::{
    order_ties, AttachmentFiles, ExportZone, FileDecision, Mapper, Placement, RecordCounts,
};
pub use plan::{
    AttachmentMode, CsvProfile, ExportOptions, ExportPlan, Format, Packaging, RecordOptions,
    RevealPlan, ScopePlan,
};
pub use record::{ConversationRecord, HandleRecord, ItemRecord, VersionRecord};
pub use time::{CivilDate, ZoneTable, ZoneTableInput};
