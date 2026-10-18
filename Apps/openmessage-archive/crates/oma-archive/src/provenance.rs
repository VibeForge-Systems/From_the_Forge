//! `imports/session-<n>.json`, the content-free provenance files (spec
//! §8.10). The writer and reader arrive with the import's finalize step;
//! M4's first slice needs only the sweep of temporary files a crash left
//! behind (`begin_write`, §5.3).

use crate::blobs::{sweep_matching, Swept, SWEEP_LIMIT};
use crate::error::ArchiveError;
use crate::layout::ArchiveDir;

/// `true` for `^session-[0-9]+\.json\.tmp$`.
fn is_provenance_tmp(name: &str) -> bool {
    name.strip_prefix("session-")
        .and_then(|n| n.strip_suffix(".json.tmp"))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Remove `imports/session-<n>.json.tmp` files.
pub(crate) fn sweep_tmp(dir: &ArchiveDir) -> Result<Swept, ArchiveError> {
    sweep_matching(&dir.imports(), is_provenance_tmp, SWEEP_LIMIT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tmp_names() {
        assert!(is_provenance_tmp("session-1.json.tmp"));
        assert!(is_provenance_tmp("session-42.json.tmp"));
        assert!(!is_provenance_tmp("session-.json.tmp"));
        assert!(!is_provenance_tmp("session-1.json"));
        assert!(!is_provenance_tmp("session-x.json.tmp"));
        assert!(!is_provenance_tmp("other-1.json.tmp"));
    }
}
