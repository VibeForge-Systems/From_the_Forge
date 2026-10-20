//! Checks on helper output, run by the main process before anything of it
//! is staged (M6 spec §5.7, C1.17).
//!
//! A helper that hostile text compromised must not be able to plant active
//! content in an export. Every output the export-render helper returns
//! passes the check of its kind here first; a refusal fails the export
//! with `HelperOutputRejected`, and nothing of the output is written.
//!
//! - [`pdf`]: the PDF check (header, `%%EOF`, every stream bounded and
//!   inflated, the objects tokenised with strings skipped, and a closed
//!   list of name objects refused);
//! - XLSX output has no check yet: the XLSX writer is deferred, and
//!   [`check_output`] refuses every XLSX output until its package check
//!   exists;
//! - decode-helper renditions are checked by `preview::check_rendition`
//!   (slice 6g).

pub mod pdf;

/// What a helper output claims to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OutputKind {
    Pdf,
    Xlsx,
}

/// Why an output was refused (kinds only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CheckRefusal {
    Pdf(pdf::PdfRefusal),
    /// No check exists for this kind yet, so it is never accepted.
    Unchecked,
}

/// Check `bytes` as an output of `kind`.
pub fn check_output(kind: OutputKind, bytes: &[u8]) -> Result<(), CheckRefusal> {
    match kind {
        OutputKind::Pdf => pdf::check(bytes).map_err(CheckRefusal::Pdf),
        OutputKind::Xlsx => Err(CheckRefusal::Unchecked),
    }
}
