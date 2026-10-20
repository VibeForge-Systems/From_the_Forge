//! PDF (M6 spec §5.6, slice 6j1): what the main process prepares for the
//! export-render helper's PDF backend.
//!
//! - [`fonts`]: the bundled fonts' SHA-256 pins and [`fonts::verify`], the
//!   check every font passes before its bytes reach the helper (owner Q2);
//! - [`doc`]: the [`doc::Block`]s the helper lays out (headings and
//!   transcript entries), whose every string is a [`doc::PdfText`] built
//!   with the hidden-character marker rule, and the backend's `Begin`
//!   options.
//!
//! Layout and serialisation (parley and krilla) run only in the helper
//! (`render::pdf`, feature `render`). The record-to-block mapping, the
//! volume estimate and the export pipeline's PDF writer are
//! `writer::pdf` (slice 6j2).

pub mod doc;
pub mod fonts;
