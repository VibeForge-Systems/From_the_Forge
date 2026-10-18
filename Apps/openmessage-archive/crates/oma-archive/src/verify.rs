//! The integrity verifier (spec §11, D69). **Report-only:** it repairs
//! nothing (M8 adds repair) and cleans nothing up.
//!
//! [`Archive::verify`] runs under the archive's lock. Its reads use the
//! ReadOnly connection, cancellable instead of op-budgeted (a verify runs
//! only on the user's request). It writes only its results: one
//! `verify_run` row and, in a full verify, `blob.verify_state` and
//! `verified_at_ns`, through [`Archive::begin_write`] with
//! [`Recovery::Skip`], so crash leftovers are reported, not swept.
//!
//! | Check | Quick | Full |
//! |---|---|---|
//! | header, fingerprint, `archive_meta` (`reader::inspect`); rules high-water marks | ✓ | ✓ |
//! | `sqlite_stat*` present (reported, not a problem) | ✓ | ✓ |
//! | `message_fts_config` holds `secure-delete = 1` | ✓ | ✓ |
//! | `PRAGMA quick_check(100)` | ✓ | — |
//! | `PRAGMA integrity_check(100)` | — | ✓ |
//! | `PRAGMA foreign_key_check`, by table | ✓ | ✓ |
//! | FTS rank-1 `integrity-check` (on the writer, under the armed interrupt) | ✓ | ✓ |
//! | `value_issue` / `value_history` rows whose row is missing | ✓ | ✓ |
//! | the label re-validated | ✓ | ✓ |
//! | each `blob`: name, regular non-link file, length | ✓ | ✓ |
//! | each `blob`: streaming re-hash ([`crate::blobs::open_checked`]) | — | ✓ |
//! | `attachments/` walk: two levels, no links followed, `blobs + 10,000` entries | ✓ | ✓ |
//! | owner and mode of every archive file (Unix; links and types elsewhere) | — | ✓ |
//! | leftover partials, provenance `.tmp` files and `blob_intent` rows | ✓ | ✓ |
//! | `imports/*.json`: name, size, format (`provenance::read`) | ✓ | ✓ |
//! | sessions in state 0 while the lock is held | ✓ | ✓ |
//!
//! The full verify's recompute of derived columns from raw values is
//! deferred (§21.2).
//!
//! Everything reported is a count, a boolean, a closed state or one of
//! `v1.sql`'s own table names; nothing read from a row or a file is kept.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;

use rusqlite::types::ValueRef;
use serde::{Deserialize, Serialize};

use crate::archive::{Archive, Recovery};
use crate::blobs::{
    open_checked, store_file_state, walk_store, StorageName, StoreFileState, StoreWalk,
};
use crate::conn::{armed, WriteConn};
use crate::error::{primary_code, ArchiveError, BlobProblem, DamageKind, Stage, SQLITE_INTERRUPT};
use crate::id::LabelState;
use crate::layout::ArchiveDir;
use crate::progress::{ArchiveCtx, ArchiveEvent, VerifyPhase};
use crate::reader::{inspect, meta_label, ArchiveReader};
use crate::schema::codes::{BlobVerifyState, VerifyDepthCode, VerifyOutcome};
use crate::schema::{v1_table_names, SchemaPlan};

/// How much a verify checks (spec §11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VerifyDepth {
    /// `quick_check`, and the store by name, type and length.
    Quick,
    /// `integrity_check`, every file re-hashed, and owner and mode.
    Full,
}

impl VerifyDepth {
    fn code(self) -> VerifyDepthCode {
        match self {
            VerifyDepth::Quick => VerifyDepthCode::Quick,
            VerifyDepth::Full => VerifyDepthCode::Full,
        }
    }

    fn from_code(code: i64) -> Option<VerifyDepth> {
        match VerifyDepthCode::from_code(code)? {
            VerifyDepthCode::Quick => Some(VerifyDepth::Quick),
            VerifyDepthCode::Full => Some(VerifyDepth::Full),
        }
    }
}

/// The most rows `quick_check` and `integrity_check` report (the `100` in
/// their SQL).
pub const SQLITE_CHECK_ROWS: u32 = 100;
/// The `attachments/` walk looks at most at this many entries more than
/// there are `blob` rows (spec §11).
pub const STORE_WALK_SLACK: u64 = 10_000;
/// The `imports/` scan reads at most this many entries more than there are
/// sessions.
pub const IMPORTS_SCAN_SLACK: u64 = 1_000;
/// `blob` rows read per page.
const BLOB_PAGE: i64 = 512;
/// The read buffer of a re-hash.
const HASH_CHUNK: usize = 64 << 10;
/// A full verify reports progress at least every this many bytes.
const BYTES_PER_EVENT: u64 = 8 << 20;

/// The state of one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum CheckState {
    /// The check did not run (the verify stopped before it, or it could
    /// not run; see [`VerifyCounts::checks_not_run`]).
    #[default]
    NotRun,
    /// It ran and found nothing.
    Ok,
    /// It ran and found a problem.
    Failed,
    /// It was cancelled while it ran.
    Cancelled,
}

/// What the header, fingerprint and meta checks found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum SchemaCheck {
    #[default]
    NotRun,
    Ok,
    /// `application_id` is not ours.
    NotAnArchive,
    /// `user_version` is newer than this build, or differs from the
    /// version the archive was opened at.
    VersionChanged,
    /// The schema objects differ from the expected ones (§7.3).
    Mismatch,
    /// The `archive_meta` row is missing.
    MetaMissing,
    /// `archive_meta` contradicts the header or itself.
    MetaInconsistent,
    /// The schema or meta could not be read.
    Unreadable,
}

impl SchemaCheck {
    fn of(e: &ArchiveError) -> SchemaCheck {
        match e {
            ArchiveError::NotAnArchive => SchemaCheck::NotAnArchive,
            ArchiveError::NewerSchema { .. } | ArchiveError::NeedsMigration { .. } => {
                SchemaCheck::VersionChanged
            }
            ArchiveError::Damaged(DamageKind::SchemaMismatch) => SchemaCheck::Mismatch,
            ArchiveError::Damaged(DamageKind::MetaMissing) => SchemaCheck::MetaMissing,
            ArchiveError::Damaged(DamageKind::MetaInconsistent) => SchemaCheck::MetaInconsistent,
            _ => SchemaCheck::Unreadable,
        }
    }
}

/// What a verify found (stored as JSON in `verify_run.counts`).
/// Content-free: counts, booleans, closed states, and table names from
/// `v1.sql` only.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[non_exhaustive]
pub struct VerifyCounts {
    // ---- the database ----
    pub schema: SchemaCheck,
    /// The archive was written by newer derivation or decoder rules than
    /// this build's (reported, not a problem).
    pub newer_rules: bool,
    /// Sessions whose deriver or decoder version is above the archive's
    /// recorded high-water mark.
    pub rules_above_high_water: u64,
    /// `sqlite_stat1` or `sqlite_stat4` exists (reported, not a problem).
    pub stat_tables: bool,
    /// `Failed`: `secure-delete` is missing from `message_fts_config` or is
    /// not 1.
    pub fts_secure_delete: CheckState,
    /// `quick_check` (quick) or `integrity_check` (full).
    pub sqlite_check: CheckState,
    /// Rows of that check other than `ok` (at most [`SQLITE_CHECK_ROWS`];
    /// 1 when the check itself failed with a corruption error).
    pub sqlite_check_messages: u64,
    pub foreign_key_violations: u64,
    /// The violations by child table (`v1.sql` names only).
    pub foreign_key_by_table: BTreeMap<String, u64>,
    /// `value_issue` rows whose entity row does not exist (or whose entity
    /// code is unknown). Link and marker entities have no single row and
    /// are not checked.
    pub value_issue_orphans: u64,
    /// The same for `value_history`.
    pub value_history_orphans: u64,
    /// `archive_meta.label` fails `Label::parse`.
    pub label_unreadable: bool,
    /// Sessions in state 0 (running) while the verify holds the lock: a
    /// crash left them.
    pub sessions_running: u64,
    /// `blob_intent` rows a crash left.
    pub blob_intents: u64,
    // ---- the index ----
    /// The FTS rank-1 `integrity-check`; `Failed` is drift or corruption.
    pub fts_integrity: CheckState,
    // ---- the files ----
    /// `blob` rows checked.
    pub blobs: u64,
    pub blob_bad_name: u64,
    pub blob_missing: u64,
    pub blob_not_regular: u64,
    pub blob_length_mismatch: u64,
    /// Full: files re-hashed to the end.
    pub blobs_hashed: u64,
    /// Full: bytes re-hashed.
    pub bytes_hashed: u64,
    pub blob_hash_mismatch: u64,
    pub blob_unreadable: u64,
    /// Entries the `attachments/` walk looked at.
    pub store_entries: u64,
    /// Names the store never writes, and directories below `<hh>/`.
    pub store_unexpected: u64,
    /// Store files no `blob` row names.
    pub store_orphans: u64,
    /// Links and reparse points in the store (never followed).
    pub store_links: u64,
    pub store_walk_truncated: bool,
    /// `attachments/.incoming/<n>.partial` files a crash left.
    pub partial_files: u64,
    /// Provenance files that read and check.
    pub provenance_files: u64,
    /// Entries of `imports/` that are not a readable provenance file.
    pub provenance_unreadable: u64,
    /// `imports/session-<n>.json.tmp` files a crash left.
    pub provenance_tmp_files: u64,
    pub imports_scan_truncated: bool,
    /// Full: archive files and directories that fail the owner and mode
    /// check (Unix; elsewhere only links and types are checked).
    pub not_private: u64,
    /// Checks that could not run (an unlistable directory, a read error, no
    /// write connection for the FTS check). A verify with any is at best
    /// `Incomplete`.
    pub checks_not_run: u64,
}

impl VerifyCounts {
    /// Whether anything found is a problem. `newer_rules`, `stat_tables`
    /// and the totals are not.
    pub fn has_problems(&self) -> bool {
        let failed = |s: CheckState| s == CheckState::Failed;
        let schema = !matches!(self.schema, SchemaCheck::Ok | SchemaCheck::NotRun);
        let flags = [
            schema,
            failed(self.fts_secure_delete),
            failed(self.sqlite_check),
            failed(self.fts_integrity),
            self.label_unreadable,
            self.store_walk_truncated,
            self.imports_scan_truncated,
        ];
        let counts = [
            self.rules_above_high_water,
            self.foreign_key_violations,
            self.value_issue_orphans,
            self.value_history_orphans,
            self.sessions_running,
            self.blob_intents,
            self.blob_bad_name,
            self.blob_missing,
            self.blob_not_regular,
            self.blob_length_mismatch,
            self.blob_hash_mismatch,
            self.blob_unreadable,
            self.store_unexpected,
            self.store_orphans,
            self.store_links,
            self.partial_files,
            self.provenance_unreadable,
            self.provenance_tmp_files,
            self.not_private,
        ];
        flags.iter().any(|f| *f) || counts.iter().any(|n| *n > 0)
    }

    /// The counts from `verify_run.counts`; `None` when they do not parse.
    /// Table names outside `v1.sql` are dropped, so nothing but our own
    /// names comes back from a crafted file.
    fn from_json(text: &str) -> Option<VerifyCounts> {
        let mut c: VerifyCounts = serde_json::from_str(text).ok()?;
        let known = v1_table_names();
        c.foreign_key_by_table
            .retain(|k, _| known.contains(&k.as_str()));
        Some(c)
    }

    fn outcome(&self, cancelled: bool) -> VerifyOutcome {
        if cancelled {
            VerifyOutcome::Cancelled
        } else if self.has_problems() {
            VerifyOutcome::Problems
        } else if self.checks_not_run > 0 {
            VerifyOutcome::Incomplete
        } else {
            VerifyOutcome::Ok
        }
    }

    fn not_run(&mut self) {
        self.checks_not_run = self.checks_not_run.saturating_add(1);
    }
}

/// The result of [`Archive::verify`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct VerifyReport {
    pub depth: VerifyDepth,
    /// `Cancelled` before `Problems` before `Incomplete` before `Ok`.
    pub outcome: VerifyOutcome,
    pub counts: VerifyCounts,
    pub started_at_ns: i64,
    pub ended_at_ns: i64,
    /// The `verify_run` row holding this result; `None` when it could not
    /// be recorded (no write connection, a failed write, or a schema check
    /// that failed: nothing is written into an archive whose schema is not
    /// ours).
    pub run: Option<i64>,
}

/// The newest `verify_run` row ([`ArchiveReader::last_verify`]).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct VerifySummary {
    pub run: i64,
    pub depth: VerifyDepth,
    pub started_at_ns: i64,
    /// `None` when a crash ended the verify.
    pub ended_at_ns: Option<i64>,
    /// `None` when a crash ended the verify.
    pub outcome: Option<VerifyOutcome>,
    /// `None` when the stored counts do not parse.
    pub counts: Option<VerifyCounts>,
}

impl VerifySummary {
    /// `None` when the depth or outcome code is not one of ours.
    pub(crate) fn from_row(
        run: i64,
        started_at_ns: i64,
        ended_at_ns: Option<i64>,
        depth: i64,
        outcome: Option<i64>,
        counts: Option<&str>,
    ) -> Option<VerifySummary> {
        let outcome = match outcome {
            None => None,
            Some(code) => Some(VerifyOutcome::from_code(code)?),
        };
        Some(VerifySummary {
            run,
            depth: VerifyDepth::from_code(depth)?,
            started_at_ns,
            ended_at_ns,
            outcome,
            counts: counts.and_then(VerifyCounts::from_json),
        })
    }
}

/// The verify was cancelled.
struct Cancelled;

type Step = Result<(), Cancelled>;

/// One verify's working state.
struct Verifier<'r, 'c, 'x> {
    depth: VerifyDepth,
    reader: &'r ArchiveReader,
    writer: Option<&'r mut WriteConn>,
    dir: &'r ArchiveDir,
    ctx: &'c mut ArchiveCtx<'x>,
    c: VerifyCounts,
    /// A `blob.verify_state` write failed.
    record_failed: bool,
    sessions: u64,
}

fn cancelled_or_not_run<T>(
    r: Result<T, ArchiveError>,
    c: &mut VerifyCounts,
) -> Result<Option<T>, Cancelled> {
    match r {
        Ok(v) => Ok(Some(v)),
        Err(ArchiveError::Cancelled) => Err(Cancelled),
        Err(e) => {
            tracing::debug!(error = %e, "a verify check could not run");
            c.not_run();
            Ok(None)
        }
    }
}

/// SQLite reports corruption (`SQLITE_CORRUPT`, including
/// `SQLITE_CORRUPT_VTAB`) or a file that is not a database.
fn is_corruption(code: Option<i32>) -> bool {
    matches!(code, Some(11 | 26))
}

/// The orphan count of `value_issue` or `value_history`: rows whose entity
/// row is missing, or whose entity code is unknown. Entities 6 (chat
/// message) and 7 (Recently Deleted marker) have no single row id and are
/// skipped. The codes are `Entity`'s, pinned in `codes.rs`.
fn orphans_sql(table: &str) -> String {
    format!(
        "SELECT count(*) FROM {table} v WHERE v.entity NOT IN (6, 7) AND NOT CASE v.entity \
           WHEN 1 THEN EXISTS (SELECT 1 FROM handle WHERE id = v.row_id) \
           WHEN 2 THEN EXISTS (SELECT 1 FROM chat WHERE id = v.row_id) \
           WHEN 3 THEN EXISTS (SELECT 1 FROM message WHERE id = v.row_id) \
           WHEN 4 THEN EXISTS (SELECT 1 FROM attachment WHERE id = v.row_id) \
           WHEN 5 THEN EXISTS (SELECT 1 FROM message_body WHERE id = v.row_id) \
           ELSE 0 END"
    )
}

/// Bookkeeping read in one statement group.
struct Bookkeeping {
    rules_above: i64,
    running: i64,
    intents: i64,
    issue_orphans: i64,
    history_orphans: i64,
    secure_delete: Option<i64>,
    sessions: i64,
}

/// What the `blob` read found for the store walk.
struct BlobsRead {
    /// The digests the valid names spell.
    referenced: BTreeSet<[u8; 32]>,
    /// The rows' `count(*)`.
    rows: u64,
    /// Every page was read.
    complete: bool,
}

/// One `blob` row as read, with crafted storage classes tolerated.
struct BlobRow {
    id: i64,
    sha: Vec<u8>,
    len: i64,
    name: Option<String>,
}

impl Verifier<'_, '_, '_> {
    fn event(&mut self, phase: VerifyPhase, done: u64, total: Option<u64>) {
        self.ctx
            .progress
            .event(ArchiveEvent::Verify { phase, done, total });
    }

    fn check_cancel(&self) -> Step {
        if self.ctx.cancel.is_cancelled() {
            Err(Cancelled)
        } else {
            Ok(())
        }
    }

    fn bookkeeping(&mut self) -> Step {
        let issue_sql = orphans_sql("value_issue");
        let history_sql = orphans_sql("value_history");
        let r = self
            .reader
            .conn()
            .cancellable(self.ctx.cancel, Stage::Verify, |c| {
                let one = |sql: &str| c.query_row(sql, [], |r| r.get::<_, i64>(0));
                let secure_delete = {
                    let mut stmt =
                        c.prepare("SELECT v FROM message_fts_config WHERE k = 'secure-delete'")?;
                    let mut rows = stmt.query([])?;
                    match rows.next()? {
                        Some(r) => match r.get_ref(0)? {
                            ValueRef::Integer(n) => Some(n),
                            _ => Some(-1),
                        },
                        None => None,
                    }
                };
                Ok(Bookkeeping {
                    rules_above: one(
                        "SELECT count(*) FROM import_session s, archive_meta m WHERE m.id = 1 \
                         AND (s.deriver_version > m.max_deriver_version \
                              OR s.decoder_version > m.max_decoder_version)",
                    )?,
                    running: one("SELECT count(*) FROM import_session WHERE state = 0")?,
                    intents: one("SELECT count(*) FROM blob_intent")?,
                    issue_orphans: one(&issue_sql)?,
                    history_orphans: one(&history_sql)?,
                    secure_delete,
                    sessions: one("SELECT count(*) FROM import_session")?,
                })
            });
        let Some(b) = cancelled_or_not_run(r, &mut self.c)? else {
            return Ok(());
        };
        let n = |v: i64| u64::try_from(v).unwrap_or(0);
        self.c.rules_above_high_water = n(b.rules_above);
        self.c.sessions_running = n(b.running);
        self.c.blob_intents = n(b.intents);
        self.c.value_issue_orphans = n(b.issue_orphans);
        self.c.value_history_orphans = n(b.history_orphans);
        self.c.fts_secure_delete = if b.secure_delete == Some(1) {
            CheckState::Ok
        } else {
            CheckState::Failed
        };
        self.sessions = n(b.sessions);
        let label = meta_label(self.reader.conn());
        if let Some(label) = cancelled_or_not_run(label, &mut self.c)? {
            self.c.label_unreadable = label == LabelState::Unreadable;
        }
        Ok(())
    }

    fn sqlite_check(&mut self) -> Step {
        let sql = match self.depth {
            VerifyDepth::Quick => "PRAGMA quick_check(100)",
            VerifyDepth::Full => "PRAGMA integrity_check(100)",
        };
        let r = self
            .reader
            .conn()
            .cancellable(self.ctx.cancel, Stage::Verify, |c| {
                let mut stmt = c.prepare(sql)?;
                let mut rows = stmt.query([])?;
                let mut bad = 0u64;
                while let Some(r) = rows.next()? {
                    if !matches!(r.get_ref(0)?, ValueRef::Text(b"ok")) {
                        bad = bad.saturating_add(1);
                    }
                }
                Ok(bad)
            });
        match r {
            Ok(0) => self.c.sqlite_check = CheckState::Ok,
            Ok(bad) => {
                self.c.sqlite_check = CheckState::Failed;
                self.c.sqlite_check_messages = bad;
            }
            Err(ArchiveError::Cancelled) => {
                self.c.sqlite_check = CheckState::Cancelled;
                return Err(Cancelled);
            }
            Err(ArchiveError::Sqlite { code, .. }) if is_corruption(code) => {
                self.c.sqlite_check = CheckState::Failed;
                self.c.sqlite_check_messages = 1;
            }
            Err(ArchiveError::NotAnArchive) => {
                self.c.sqlite_check = CheckState::Failed;
                self.c.sqlite_check_messages = 1;
            }
            Err(e) => {
                tracing::debug!(error = %e, "the SQLite check could not run");
                self.c.not_run();
            }
        }
        Ok(())
    }

    fn foreign_keys(&mut self) -> Step {
        let r = self
            .reader
            .conn()
            .cancellable(self.ctx.cancel, Stage::Verify, |c| {
                let mut stmt = c.prepare("PRAGMA foreign_key_check")?;
                let mut rows = stmt.query([])?;
                let mut by_table: BTreeMap<String, u64> = BTreeMap::new();
                let mut total = 0u64;
                let known = v1_table_names();
                while let Some(r) = rows.next()? {
                    total = total.saturating_add(1);
                    if let ValueRef::Text(t) = r.get_ref(0)? {
                        if let Some(name) = std::str::from_utf8(t)
                            .ok()
                            .and_then(|t| known.iter().find(|k| **k == t))
                        {
                            let slot = by_table.entry((*name).to_owned()).or_insert(0);
                            *slot = slot.saturating_add(1);
                        }
                    }
                }
                Ok((total, by_table))
            });
        if let Some((total, by_table)) = cancelled_or_not_run(r, &mut self.c)? {
            self.c.foreign_key_violations = total;
            self.c.foreign_key_by_table = by_table;
        }
        Ok(())
    }

    fn fts(&mut self) -> Step {
        self.event(VerifyPhase::CheckingIndex, 0, None);
        let Some(w) = self.writer.as_deref() else {
            self.c.not_run();
            return Ok(());
        };
        let cancel = self.ctx.cancel;
        let r = armed(w.conn(), cancel, |c| {
            c.execute(
                "INSERT INTO message_fts(message_fts, rank) VALUES('integrity-check', 1)",
                [],
            )
        });
        match r {
            Ok(_) => self.c.fts_integrity = CheckState::Ok,
            Err(e) if primary_code(&e) == Some(SQLITE_INTERRUPT) && cancel.is_cancelled() => {
                self.c.fts_integrity = CheckState::Cancelled;
                return Err(Cancelled);
            }
            Err(e) if is_corruption(primary_code(&e)) => {
                self.c.fts_integrity = CheckState::Failed;
            }
            Err(e) => {
                tracing::debug!(code = ?primary_code(&e), "the FTS check could not run");
                self.c.not_run();
            }
        }
        Ok(())
    }

    fn blob_page(&mut self, page: u32, after: i64) -> Result<Option<Vec<BlobRow>>, Cancelled> {
        let r = self
            .reader
            .conn()
            .cancellable(self.ctx.cancel, Stage::Verify, |c| {
                crate::fault::verify_blob_error(page)?;
                let mut stmt = c.prepare(
                    "SELECT id, sha256, byte_len, storage_name FROM blob \
                     WHERE id > ?1 ORDER BY id LIMIT ?2",
                )?;
                let mut rows = stmt.query([after, BLOB_PAGE])?;
                let mut out = Vec::new();
                while let Some(r) = rows.next()? {
                    out.push(BlobRow {
                        id: r.get(0)?,
                        sha: match r.get_ref(1)? {
                            ValueRef::Blob(b) => b.to_vec(),
                            _ => Vec::new(),
                        },
                        len: match r.get_ref(2)? {
                            ValueRef::Integer(n) => n,
                            _ => -1,
                        },
                        name: match r.get_ref(3)? {
                            ValueRef::Text(t) => std::str::from_utf8(t).ok().map(str::to_owned),
                            _ => None,
                        },
                    });
                }
                Ok(out)
            });
        cancelled_or_not_run(r, &mut self.c)
    }

    /// Re-hash one file to its end. `Ok(None)` when it matched.
    fn rehash(
        &mut self,
        name: &StorageName,
        len: i64,
        total: u64,
    ) -> Result<Option<BlobVerifyState>, Cancelled> {
        let mut reader = match open_checked(self.dir, name, len) {
            Ok(r) => r,
            Err(p) => {
                return Ok(Some(match p {
                    BlobProblem::Missing => BlobVerifyState::Missing,
                    BlobProblem::LengthMismatch => BlobVerifyState::LengthMismatch,
                    BlobProblem::BadName => BlobVerifyState::BadName,
                    BlobProblem::Io(_) => BlobVerifyState::Unreadable,
                    _ => BlobVerifyState::NotRegular,
                }))
            }
        };
        let mut buf = vec![0u8; HASH_CHUNK];
        let mut since_event = 0u64;
        let state = loop {
            self.check_cancel()?;
            match reader.read(&mut buf) {
                Ok(0) => break None,
                Ok(n) => {
                    let n = u64::try_from(n).unwrap_or(u64::MAX);
                    self.c.bytes_hashed = self.c.bytes_hashed.saturating_add(n);
                    since_event = since_event.saturating_add(n);
                    if since_event >= BYTES_PER_EVENT {
                        since_event = 0;
                        let done = self.c.bytes_hashed;
                        self.event(VerifyPhase::CheckingFiles, done, Some(total));
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                    break Some(BlobVerifyState::HashMismatch)
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break Some(BlobVerifyState::Unreadable),
            }
        };
        self.c.blobs_hashed = self.c.blobs_hashed.saturating_add(1);
        Ok(state)
    }

    fn count_state(&mut self, s: BlobVerifyState) {
        let slot = match s {
            BlobVerifyState::Ok => return,
            BlobVerifyState::Missing => &mut self.c.blob_missing,
            BlobVerifyState::NotRegular => &mut self.c.blob_not_regular,
            BlobVerifyState::LengthMismatch => &mut self.c.blob_length_mismatch,
            BlobVerifyState::HashMismatch => &mut self.c.blob_hash_mismatch,
            BlobVerifyState::Unreadable => &mut self.c.blob_unreadable,
            BlobVerifyState::BadName => &mut self.c.blob_bad_name,
        };
        *slot = slot.saturating_add(1);
    }

    /// Full only: record each row's state (one transaction per page).
    fn record_states(&mut self, states: &[(i64, BlobVerifyState)]) {
        if states.is_empty() {
            return;
        }
        let now = self.ctx.clock.now_ns();
        let Some(w) = self.writer.as_deref_mut() else {
            self.record_failed = true;
            return;
        };
        let write = |w: &mut WriteConn| -> rusqlite::Result<()> {
            let tx =
                rusqlite::Transaction::new(w.conn_mut(), rusqlite::TransactionBehavior::Immediate)?;
            {
                let mut stmt = tx.prepare(
                    "UPDATE blob SET verify_state = ?1, verified_at_ns = ?2 WHERE id = ?3",
                )?;
                for (id, s) in states {
                    stmt.execute(rusqlite::params![s.code(), now, id])?;
                }
            }
            tx.commit()
        };
        if let Err(e) = write(w) {
            tracing::info!(code = ?primary_code(&e), "blob verify states not recorded");
            self.record_failed = true;
        }
    }

    /// The `blob` rows and their files. `None` when the rows could not be
    /// counted (the check is counted as not run).
    fn blobs(&mut self) -> Result<Option<BlobsRead>, Cancelled> {
        let mut referenced = BTreeSet::new();
        let totals = self
            .reader
            .conn()
            .cancellable(self.ctx.cancel, Stage::Verify, |c| {
                crate::fault::verify_blob_error(0)?;
                c.query_row(
                    "SELECT count(*), total(CASE WHEN byte_len > 0 THEN byte_len ELSE 0 END) \
                     FROM blob",
                    [],
                    |r| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?)),
                )
            });
        let Some((rows, bytes)) = cancelled_or_not_run(totals, &mut self.c)? else {
            return Ok(None);
        };
        let mut complete = true;
        let mut page_no = 0u32;
        let full = self.depth == VerifyDepth::Full;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let total = if full {
            bytes.max(0.0) as u64
        } else {
            u64::try_from(rows).unwrap_or(0)
        };
        self.event(VerifyPhase::CheckingFiles, 0, Some(total));
        let mut after = i64::MIN;
        loop {
            self.check_cancel()?;
            page_no = page_no.saturating_add(1);
            let Some(page) = self.blob_page(page_no, after)? else {
                // Counted as not run; the digests read so far are not all.
                complete = false;
                break;
            };
            if page.is_empty() {
                break;
            }
            let mut states: Vec<(i64, BlobVerifyState)> = Vec::with_capacity(page.len());
            for row in page {
                after = row.id;
                self.c.blobs = self.c.blobs.saturating_add(1);
                let name = row
                    .name
                    .as_deref()
                    .and_then(StorageName::parse)
                    .filter(|n| {
                        n.sha256()
                            .is_some_and(|s| s.as_slice() == row.sha.as_slice())
                    });
                let state = match name {
                    None => Some(BlobVerifyState::BadName),
                    Some(ref name) => {
                        if let Some(s) = name.sha256() {
                            referenced.insert(s);
                        }
                        match store_file_state(self.dir, name) {
                            StoreFileState::Regular(l)
                                if u64::try_from(row.len).ok() == Some(l) =>
                            {
                                if full {
                                    self.rehash(name, row.len, total)?
                                } else {
                                    None
                                }
                            }
                            StoreFileState::Regular(_) => Some(BlobVerifyState::LengthMismatch),
                            StoreFileState::Missing => Some(BlobVerifyState::Missing),
                            StoreFileState::Unusable => Some(BlobVerifyState::NotRegular),
                        }
                    }
                };
                let state = state.unwrap_or(BlobVerifyState::Ok);
                self.count_state(state);
                states.push((row.id, state));
            }
            if full {
                self.record_states(&states);
                let done = self.c.bytes_hashed;
                self.event(VerifyPhase::CheckingFiles, done, Some(total));
            } else {
                let done = self.c.blobs;
                self.event(VerifyPhase::CheckingFiles, done, Some(total));
            }
        }
        Ok(Some(BlobsRead {
            referenced,
            rows: u64::try_from(rows).unwrap_or(0),
            complete,
        }))
    }

    fn files(&mut self) -> Step {
        let read = self.blobs()?;
        let full = self.depth == VerifyDepth::Full;
        // The walk's orphans and its limit depend on the `blob` rows. When
        // they could not be counted the walk is not run (and counted so);
        // when only some were read, no store file is called an orphan.
        let walk = match &read {
            None => {
                self.c.not_run();
                StoreWalk::default()
            }
            Some(b) => {
                let limit = b.rows.saturating_add(STORE_WALK_SLACK);
                let referenced = b.complete.then_some(&b.referenced);
                walk_store(self.dir, referenced, limit, full, self.ctx.cancel)
                    .map_err(|_| Cancelled)?
            }
        };
        self.c.store_entries = walk.entries;
        self.c.store_unexpected = walk.unexpected;
        self.c.store_orphans = walk.orphans;
        self.c.store_links = walk.links;
        self.c.partial_files = walk.partials;
        self.c.store_walk_truncated = walk.truncated;
        self.c.checks_not_run = self.c.checks_not_run.saturating_add(walk.unlisted);
        self.check_cancel()?;
        let limit = self.sessions.saturating_add(IMPORTS_SCAN_SLACK);
        let imports =
            crate::provenance::scan(self.dir, limit, full, self.ctx.cancel).ok_or(Cancelled)?;
        self.c.provenance_files = imports.files;
        self.c.provenance_unreadable = imports.unreadable;
        self.c.provenance_tmp_files = imports.tmp;
        self.c.imports_scan_truncated = imports.truncated;
        if imports.unlisted {
            self.c.not_run();
        }
        if full {
            self.c.not_private = self
                .dir
                .count_not_private()
                .saturating_add(walk.not_private)
                .saturating_add(imports.not_private);
        }
        Ok(())
    }

    fn run(&mut self) -> Step {
        self.check_cancel()?;
        self.bookkeeping()?;
        self.check_cancel()?;
        self.sqlite_check()?;
        self.check_cancel()?;
        self.foreign_keys()?;
        self.check_cancel()?;
        self.fts()?;
        self.check_cancel()?;
        self.files()
    }
}

/// The checks that never run after a failed schema check: the
/// bookkeeping, the SQLite check, the foreign keys, the FTS index and the
/// files.
const CHECKS_AFTER_SCHEMA: u64 = 5;

impl Archive {
    /// Verify the archive (spec §11): report-only, cancellable, under the
    /// archive's lock. See the module documentation for what each depth
    /// checks. A cancel gives a report with outcome `Cancelled` (recorded
    /// like any other); errors are only for an archive that needs a
    /// migration.
    pub fn verify(
        &mut self,
        depth: VerifyDepth,
        ctx: &mut ArchiveCtx<'_>,
    ) -> Result<VerifyReport, ArchiveError> {
        if self.needs_migration() {
            return Err(ArchiveError::NeedsMigration {
                found: self.schema_version(),
            });
        }
        let started = ctx.clock.now_ns();
        let mut c = VerifyCounts::default();
        ctx.progress.event(ArchiveEvent::Verify {
            phase: VerifyPhase::CheckingDatabase,
            done: 0,
            total: None,
        });

        // 1. The header, fingerprint and meta, before any table is read.
        let plan: SchemaPlan = self.plan();
        let expected_version = self.schema_version();
        match inspect(self.reader().conn(), &plan) {
            Ok(i) if i.version == expected_version => {
                c.schema = SchemaCheck::Ok;
                c.newer_rules = i.import_blocked.is_some();
                c.stat_tables = i.stat_tables;
            }
            Ok(_) => c.schema = SchemaCheck::VersionChanged,
            Err(e) => c.schema = SchemaCheck::of(&e),
        }
        if c.schema != SchemaCheck::Ok {
            c.checks_not_run = c.checks_not_run.saturating_add(CHECKS_AFTER_SCHEMA);
            let outcome = c.outcome(false);
            tracing::info!(?depth, ?outcome, schema = ?c.schema, "verify stopped at the schema check");
            return Ok(VerifyReport {
                depth,
                outcome,
                counts: c,
                started_at_ns: started,
                ended_at_ns: ctx.clock.now_ns(),
                run: None,
            });
        }

        // 2. The writer, for the result rows and the FTS check.
        let run = match self.begin_write(Recovery::Skip) {
            Ok(_) => self.start_run(depth, started),
            Err(e) => {
                tracing::info!(error = %e, "verify runs without a write connection");
                None
            }
        };

        // 3. The checks.
        let (reader, writer, dir) = self.verify_parts();
        let mut v = Verifier {
            depth,
            reader,
            writer,
            dir,
            ctx: &mut *ctx,
            c,
            record_failed: false,
            sessions: 0,
        };
        let cancelled = v.run().is_err();
        let record_failed = v.record_failed;
        let counts = v.c;
        let outcome = counts.outcome(cancelled);
        let ended = ctx.clock.now_ns();

        // 4. The result.
        let run = run.and_then(|id| self.finish_run(id, ended, outcome, &counts).then_some(id));
        tracing::info!(
            ?depth,
            ?outcome,
            blobs = counts.blobs,
            bytes_hashed = counts.bytes_hashed,
            checks_not_run = counts.checks_not_run,
            recorded = run.is_some() && !record_failed,
            "verify finished"
        );
        Ok(VerifyReport {
            depth,
            outcome,
            counts,
            started_at_ns: started,
            ended_at_ns: ended,
            run: run.filter(|_| !record_failed),
        })
    }

    /// Insert the `verify_run` row (outcome NULL until it ends, so a crash
    /// leaves a visible unfinished run).
    fn start_run(&mut self, depth: VerifyDepth, started: i64) -> Option<i64> {
        let w = self.ensure_writer().ok()?;
        let r = w.conn().execute(
            "INSERT INTO verify_run (started_at_ns, depth) VALUES (?1, ?2)",
            rusqlite::params![started, depth.code().code()],
        );
        match r {
            Ok(_) => Some(w.conn().last_insert_rowid()),
            Err(e) => {
                tracing::info!(code = ?primary_code(&e), "the verify run was not recorded");
                None
            }
        }
    }

    fn finish_run(
        &mut self,
        id: i64,
        ended: i64,
        outcome: VerifyOutcome,
        counts: &VerifyCounts,
    ) -> bool {
        let Ok(json) = serde_json::to_string(counts) else {
            return false;
        };
        let Ok(w) = self.ensure_writer() else {
            return false;
        };
        match w.conn().execute(
            "UPDATE verify_run SET ended_at_ns = ?1, outcome = ?2, counts = ?3 WHERE id = ?4",
            rusqlite::params![ended, outcome.code(), json, id],
        ) {
            Ok(1) => true,
            Ok(_) => false,
            Err(e) => {
                tracing::info!(code = ?primary_code(&e), "the verify result was not recorded");
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcomes_rank_cancel_then_problems_then_incomplete() {
        let mut c = VerifyCounts {
            schema: SchemaCheck::Ok,
            ..VerifyCounts::default()
        };
        assert_eq!(c.outcome(false), VerifyOutcome::Ok);
        c.stat_tables = true;
        c.newer_rules = true;
        c.blobs = 3;
        assert_eq!(c.outcome(false), VerifyOutcome::Ok);
        c.checks_not_run = 1;
        assert_eq!(c.outcome(false), VerifyOutcome::Incomplete);
        c.store_orphans = 1;
        assert_eq!(c.outcome(false), VerifyOutcome::Problems);
        assert_eq!(c.outcome(true), VerifyOutcome::Cancelled);
        let failed = VerifyCounts {
            fts_integrity: CheckState::Failed,
            ..VerifyCounts::default()
        };
        assert!(failed.has_problems());
    }

    #[test]
    fn counts_round_trip_and_drop_foreign_table_names() {
        let mut c = VerifyCounts {
            foreign_key_violations: 2,
            ..VerifyCounts::default()
        };
        c.foreign_key_by_table
            .insert("message_attachment".into(), 1);
        let json = serde_json::to_string(&c).expect("json");
        assert!(json.contains("\"foreignKeyViolations\":2"), "{json}");
        assert_eq!(VerifyCounts::from_json(&json), Some(c.clone()));
        let crafted = json.replace("message_attachment", "Someone's name");
        let back = VerifyCounts::from_json(&crafted).expect("parses");
        assert!(back.foreign_key_by_table.is_empty());
        assert_eq!(VerifyCounts::from_json("{not json"), None);
        // Unknown fields and missing ones are tolerated (a newer or older
        // build's counts).
        let partial = VerifyCounts::from_json("{\"blobs\":4,\"later\":1}").expect("parses");
        assert_eq!(partial.blobs, 4);
    }

    #[test]
    fn summaries_refuse_unknown_codes() {
        assert!(VerifySummary::from_row(1, 0, None, 0, None, None).is_some());
        assert!(VerifySummary::from_row(1, 0, Some(1), 1, Some(3), Some("{}")).is_some());
        assert!(VerifySummary::from_row(1, 0, None, 2, None, None).is_none());
        assert!(VerifySummary::from_row(1, 0, None, 0, Some(4), None).is_none());
    }
}
