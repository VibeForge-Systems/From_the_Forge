//! Schema scan, integrity check and probe (M3 §6).

mod integrity;
pub(crate) mod known;
mod probe;
mod scan;

pub(crate) use integrity::{quick_check, Integrity};
pub(crate) use known::TableLayout;
pub use known::{ColumnKind, KnownColumn, KnownColumnSet, KnownTable};
pub(crate) use probe::probe;
pub use probe::{
    Capabilities, DamageReason, Features, IncompleteReason, Probe, TableCaps, Unsupported,
    UnsupportedReason,
};
pub(crate) use scan::scan;

/// `true` when `name` starts with `prefix`, ASCII-case-insensitively.
pub(crate) fn starts_with_ci(name: &[u8], prefix: &[u8]) -> bool {
    name.get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}
