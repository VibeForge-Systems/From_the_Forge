//! `imports/session-<n>.json`, the content-free provenance files (spec
//! §8.10, D70).
//!
//! One file per import session, written whenever the session reaches a
//! terminal state this import records: completed, cancelled, failed or
//! stopped. A continued session rewrites its file when it ends again. The
//! file outlives damage to the database and holds **no** device name,
//! GUID, digest, rank, message date or range, path, file name or label:
//! only versions, states, closed kinds, booleans, the import's own times,
//! the lineage number, the product type and version (D70, a lean awaiting
//! the owner's confirmation) and the [`ImportCounts`] numbers.
//!
//! The write is atomic: `session-<n>.json.tmp` is created new (0600),
//! written, `sync_all`ed and renamed over `session-<n>.json`, then
//! `imports/` is flushed. It is **best effort**: a failure is logged by
//! kind and reported in the summary, and never changes the import's
//! outcome. A crash leaves at most the `.tmp`, which the next write open
//! sweeps.
//!
//! The reader accepts only names matching `^session-[0-9]+\.json$`, files of
//! at most [`MAX_BYTES`], opened without following links, whose `format`,
//! `formatVersion` and `session` match.

use std::io::{Read, Write};

use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Map, Value};

use crate::blobs::{sweep_matching, Swept, SWEEP_LIMIT};
use crate::error::ArchiveError;
use crate::import::ImportCounts;
use crate::layout::ArchiveDir;
use crate::schema::codes::{OrderKeySource, QualifierBit, SessionState, WalOutcomeCode};

/// The `format` value.
pub(crate) const FORMAT: &str = "oma-import-provenance";
/// The `formatVersion` value.
pub(crate) const FORMAT_VERSION: i64 = 1;
/// The largest provenance file the reader accepts.
pub(crate) const MAX_BYTES: u64 = 64 * 1024;

/// `true` for `^session-[0-9]+\.json\.tmp$`.
fn is_provenance_tmp(name: &str) -> bool {
    name.strip_prefix("session-")
        .and_then(|n| n.strip_suffix(".json.tmp"))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The session number of a name matching `^session-[0-9]+\.json$`.
pub(crate) fn provenance_session(name: &str) -> Option<i64> {
    let digits = name.strip_prefix("session-")?.strip_suffix(".json")?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// Remove `imports/session-<n>.json.tmp` files.
pub(crate) fn sweep_tmp(dir: &ArchiveDir) -> Result<Swept, ArchiveError> {
    sweep_matching(&dir.imports(), is_provenance_tmp, SWEEP_LIMIT)
}

fn state_name(s: SessionState) -> &'static str {
    match s {
        SessionState::Running => "running",
        SessionState::Completed => "completed",
        SessionState::Cancelled => "cancelled",
        SessionState::Failed => "failed",
        SessionState::Interrupted => "interrupted",
        SessionState::Superseded => "superseded",
        SessionState::Stopped => "stopped",
    }
}

fn qualifier_name(q: QualifierBit) -> &'static str {
    match q {
        QualifierBit::BackupDateOutOfRange => "backupDateOutOfRange",
        QualifierBit::WalSkipped => "walSkipped",
        QualifierBit::WalStateUnknown => "walStateUnknown",
        QualifierBit::WalFramesInvalid => "walFramesInvalid",
        QualifierBit::NonUtf8Encoding => "nonUtf8Encoding",
        QualifierBit::SizeMismatch => "sizeMismatch",
        QualifierBit::IosVersionUntested => "iosVersionUntested",
        QualifierBit::UnknownTables => "unknownTables",
        QualifierBit::UnknownColumns => "unknownColumns",
        QualifierBit::WrongCaseColumns => "wrongCaseColumns",
    }
}

fn wal_name(code: i64) -> Value {
    match WalOutcomeCode::from_code(code) {
        Some(WalOutcomeCode::NotListed) => json!("notListed"),
        Some(WalOutcomeCode::Staged) => json!("staged"),
        Some(WalOutcomeCode::Skipped) => json!("skipped"),
        None => Value::Null,
    }
}

fn source_name(code: Option<i64>) -> Value {
    match code.and_then(OrderKeySource::from_code) {
        Some(OrderKeySource::StatusDate) => json!("statusPlistDate"),
        Some(OrderKeySource::InfoLastBackupDate) => json!("infoPlistLastBackupDate"),
        None => Value::Null,
    }
}

/// The provenance document of `session`, from the database. `None` when
/// the session does not exist.
pub(crate) fn document(conn: &Connection, session: i64) -> rusqlite::Result<Option<Value>> {
    conn.query_row(
        "SELECT s.state, s.app_version, s.schema_version, s.deriver_version, \
                s.decoder_version, s.started_at_ns, s.ended_at_ns, s.resumes, s.order_class, \
                s.order_key_source, s.full_observation, s.lineage_id, s.qualifiers, \
                s.wal_outcome, s.include_recently_deleted, s.copy_attachments, s.counts, \
                b.product_type, b.product_version, b.encrypted \
         FROM import_session s JOIN source_backup b ON b.id = s.source_backup_id \
         WHERE s.id = ?1",
        [session],
        |r| {
            let state = SessionState::from_code(r.get(0)?)
                .map_or(Value::Null, |s| Value::String(state_name(s).to_owned()));
            let qualifiers: i64 = r.get(12)?;
            let names: Vec<Value> = QualifierBit::ALL
                .iter()
                .filter(|q| qualifiers & q.mask() != 0)
                .map(|q| Value::String(qualifier_name(*q).to_owned()))
                .collect();
            // The counts are re-serialised from the typed struct, so only
            // numbers reach the file whatever the column holds.
            let counts = r
                .get::<_, Option<String>>(16)?
                .as_deref()
                .and_then(ImportCounts::from_json)
                .unwrap_or_default();
            let counts = serde_json::to_value(&counts).unwrap_or(Value::Null);
            let full = match r.get::<_, Option<i64>>(10)? {
                Some(1) => json!(true),
                Some(0) => json!(false),
                _ => Value::Null,
            };
            let order_class = match r.get::<_, i64>(8)? {
                1 => json!("dated"),
                0 => json!("undated"),
                _ => Value::Null,
            };
            let mut source = Map::new();
            source.insert(
                "productType".into(),
                r.get::<_, Option<String>>(17)?
                    .map_or(Value::Null, Value::String),
            );
            source.insert(
                "productVersion".into(),
                r.get::<_, Option<String>>(18)?
                    .map_or(Value::Null, Value::String),
            );
            source.insert("encrypted".into(), json!(r.get::<_, i64>(19)? == 1));
            Ok(json!({
                "format": FORMAT,
                "formatVersion": FORMAT_VERSION,
                "session": session,
                "state": state,
                "appVersion": r.get::<_, String>(1)?,
                "schemaVersion": r.get::<_, i64>(2)?,
                "deriverVersion": r.get::<_, i64>(3)?,
                "decoderVersion": r.get::<_, i64>(4)?,
                "startedAtNs": r.get::<_, i64>(5)?,
                "endedAtNs": r.get::<_, Option<i64>>(6)?,
                "resumes": r.get::<_, i64>(7)?,
                "orderClass": order_class,
                "orderKeySource": source_name(r.get(9)?),
                "fullObservation": full,
                "lineage": r.get::<_, i64>(11)?,
                "source": Value::Object(source),
                "qualifiers": names,
                "walOutcome": wal_name(r.get(13)?),
                "options": {
                    "includeRecentlyDeleted": r.get::<_, i64>(14)? == 1,
                    "copyAttachments": r.get::<_, i64>(15)? == 1,
                },
                "counts": counts,
            }))
        },
    )
    .optional()
}

/// Why a provenance file was not written. Kinds only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteFailure {
    Read(Option<i32>),
    Missing,
    Io(std::io::ErrorKind),
    Injected,
}

fn write_inner(dir: &ArchiveDir, conn: &Connection, session: i64) -> Result<(), WriteFailure> {
    if crate::fault::provenance_fails() {
        return Err(WriteFailure::Injected);
    }
    let doc = document(conn, session)
        .map_err(|e| WriteFailure::Read(crate::error::primary_code(&e)))?
        .ok_or(WriteFailure::Missing)?;
    let mut bytes = serde_json::to_vec_pretty(&doc).map_err(|_| WriteFailure::Missing)?;
    bytes.push(b'\n');
    let io = |e: std::io::Error| WriteFailure::Io(e.kind());
    let tmp = dir.provenance_tmp(session);
    // A `.tmp` of this session can only be ours (the sweep ran at the write
    // open); a stale one is replaced.
    match std::fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(io(e)),
    }
    let mut f = oma_fs::create_new_private(&tmp).map_err(io)?;
    crate::fault::point(crate::fault::FaultPoint::ProvenanceTmpCreated);
    f.write_all(&bytes).map_err(io)?;
    f.sync_all().map_err(io)?;
    drop(f);
    crate::fault::point(crate::fault::FaultPoint::ProvenanceBeforeRename);
    std::fs::rename(&tmp, dir.provenance_file(session)).map_err(io)?;
    oma_fs::fsync_dir(&dir.imports()).map_err(io)?;
    Ok(())
}

/// Write `imports/session-<n>.json` for `session`, best effort: `true` when
/// written. A failure is logged by kind only.
pub(crate) fn write(dir: &ArchiveDir, conn: &Connection, session: i64) -> bool {
    match write_inner(dir, conn, session) {
        Ok(()) => true,
        Err(failure) => {
            if let Err(e) = std::fs::remove_file(dir.provenance_tmp(session)) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::debug!(kind = ?e.kind(), "provenance temporary file left");
                }
            }
            tracing::warn!(?failure, "the provenance file was not written");
            false
        }
    }
}

/// Why a provenance file was not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReadProblem {
    /// The name does not match `^session-[0-9]+\.json$`.
    Name,
    /// Over [`MAX_BYTES`].
    TooLarge,
    /// A link, not a regular file, or unreadable.
    Unreadable,
    /// Not JSON, or not this format, version or session.
    NotProvenance,
}

/// Read and check one provenance file. `name` is its file name.
pub(crate) fn read(dir: &ArchiveDir, name: &str) -> Result<Value, ReadProblem> {
    let session = provenance_session(name).ok_or(ReadProblem::Name)?;
    let f = oma_fs::open_nofollow(&dir.provenance_file(session))
        .map_err(|_| ReadProblem::Unreadable)?;
    let meta = f.metadata().map_err(|_| ReadProblem::Unreadable)?;
    if !meta.is_file() {
        return Err(ReadProblem::Unreadable);
    }
    if meta.len() > MAX_BYTES {
        return Err(ReadProblem::TooLarge);
    }
    let mut bytes = Vec::new();
    f.take(MAX_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| ReadProblem::Unreadable)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_BYTES {
        return Err(ReadProblem::TooLarge);
    }
    let v: Value = serde_json::from_slice(&bytes).map_err(|_| ReadProblem::NotProvenance)?;
    let ok = v.get("format").and_then(Value::as_str) == Some(FORMAT)
        && v.get("formatVersion").and_then(Value::as_i64) == Some(FORMAT_VERSION)
        && v.get("session").and_then(Value::as_i64) == Some(session);
    if ok {
        Ok(v)
    } else {
        Err(ReadProblem::NotProvenance)
    }
}

/// What [`scan`] found in `imports/`. Counts only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ImportsScan {
    /// Provenance files that read and check.
    pub(crate) files: u64,
    /// Entries that are not a readable provenance file: a wrong name, over
    /// [`MAX_BYTES`], a link, not a regular file, not JSON, or the wrong
    /// format, version or session.
    pub(crate) unreadable: u64,
    /// `session-<n>.json.tmp` leftovers.
    pub(crate) tmp: u64,
    /// With `check_modes`: files that fail `check_private`.
    pub(crate) not_private: u64,
    /// `imports/` could not be listed.
    pub(crate) unlisted: bool,
    /// The scan stopped at its entry limit.
    pub(crate) truncated: bool,
}

/// Read every entry of `imports/` (spec §11), at most `limit` of them.
/// `None` when cancelled.
pub(crate) fn scan(
    dir: &ArchiveDir,
    limit: u64,
    check_modes: bool,
    cancel: &oma_backup::CancelToken,
) -> Option<ImportsScan> {
    let mut out = ImportsScan::default();
    let Ok(entries) = std::fs::read_dir(dir.imports()) else {
        out.unlisted = true;
        return Some(out);
    };
    let mut seen = 0u64;
    for entry in entries {
        if seen >= limit {
            out.truncated = true;
            break;
        }
        seen = seen.saturating_add(1);
        if cancel.is_cancelled() {
            return None;
        }
        let Some(name) = entry
            .ok()
            .and_then(|e| e.file_name().to_str().map(str::to_owned))
        else {
            out.unreadable = out.unreadable.saturating_add(1);
            continue;
        };
        if is_provenance_tmp(&name) {
            out.tmp = out.tmp.saturating_add(1);
            continue;
        }
        match read(dir, &name) {
            Ok(_) => {
                out.files = out.files.saturating_add(1);
                if check_modes {
                    if let Some(session) = provenance_session(&name) {
                        if oma_fs::check_private(
                            &dir.provenance_file(session),
                            oma_fs::Expect::File,
                        )
                        .is_err()
                        {
                            out.not_private = out.not_private.saturating_add(1);
                        }
                    }
                }
            }
            Err(_) => out.unreadable = out.unreadable.saturating_add(1),
        }
    }
    Some(out)
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

    #[test]
    fn provenance_names() {
        assert_eq!(provenance_session("session-1.json"), Some(1));
        assert_eq!(provenance_session("session-0042.json"), Some(42));
        for bad in [
            "session-.json",
            "session-1.json.tmp",
            "session-1a.json",
            "session--1.json",
            "Session-1.json",
            "session-1.JSON",
            "session-99999999999999999999.json",
        ] {
            assert_eq!(provenance_session(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_reader_checks_name_size_and_format() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = ArchiveDir::at(tmp.path().to_path_buf());
        std::fs::create_dir(dir.imports()).expect("imports");
        assert_eq!(read(&dir, "notes.txt"), Err(ReadProblem::Name));
        assert_eq!(read(&dir, "session-1.json"), Err(ReadProblem::Unreadable));
        let good = json!({ "format": FORMAT, "formatVersion": 1, "session": 2 });
        std::fs::write(dir.provenance_file(2), good.to_string()).expect("write");
        assert_eq!(read(&dir, "session-2.json"), Ok(good.clone()));
        // The wrong session number for the name.
        std::fs::write(dir.provenance_file(3), good.to_string()).expect("write");
        assert_eq!(
            read(&dir, "session-3.json"),
            Err(ReadProblem::NotProvenance)
        );
        std::fs::write(dir.provenance_file(4), b"{not json").expect("write");
        assert_eq!(
            read(&dir, "session-4.json"),
            Err(ReadProblem::NotProvenance)
        );
        let big = vec![b' '; usize::try_from(MAX_BYTES).expect("small") + 1];
        std::fs::write(dir.provenance_file(5), big).expect("write");
        assert_eq!(read(&dir, "session-5.json"), Err(ReadProblem::TooLarge));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.provenance_file(2), dir.provenance_file(6))
                .expect("link");
            assert_eq!(read(&dir, "session-6.json"), Err(ReadProblem::Unreadable));
        }
    }
}
