//! `ArchiveReader`: lock-free, query-only reads (spec §4.3, D84, C3.1).
//!
//! A reader holds one ReadOnly connection. It takes no lock, so it can read
//! while an import holds the archive's lock; in WAL mode it sees only
//! committed state. Before any table is read, the header is checked
//! (`application_id`, `user_version`), then the schema fingerprint, then
//! `archive_meta` (§5.3 steps 5–7). Every statement runs under an op
//! budget.
//!
//! [`ArchiveReader::open_blob`] is the one way attachment bytes are read
//! back (spec §4.3, §9.5); M5 adds thread pages and search here.

use rusqlite::types::ValueRef;
use rusqlite::Connection;

use crate::blobs::{open_checked, BlobReader, StorageName};
use crate::conn::{ReadConn, READ_OP_BUDGET};
use crate::error::BlobProblem;
use crate::error::{ArchiveError, DamageKind, Stage};
use crate::id::{ArchiveId, Label, LabelState};
use crate::import::{ImportCounts, LineageId, StopReason};
use crate::layout::ArchiveDir;
use crate::schema::codes::{IssueCode, SessionState};
use crate::schema::{
    fingerprint, SchemaPlan, APPLICATION_ID, DECODER_VERSION, DERIVER_VERSION, FORMAT,
};
use crate::verify::VerifySummary;

/// Why imports into an archive are blocked although it can be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ImportBlock {
    /// The archive was written by newer derivation or decoder rules than
    /// this build's (C1.7).
    NewerRules,
}

/// `archive_stats`, the derived totals recomputed at the end of every
/// import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ArchiveStats {
    pub handles: u64,
    pub chats: u64,
    pub messages: u64,
    pub body_versions: u64,
    pub attachments: u64,
    pub blobs: u64,
    pub blob_bytes: u64,
    pub computed_at_ns: i64,
}

/// Import sessions by state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct SessionCounts {
    pub running: u64,
    pub completed: u64,
    pub cancelled: u64,
    pub failed: u64,
    pub interrupted: u64,
    pub superseded: u64,
    pub stopped: u64,
}

impl SessionCounts {
    /// Every session.
    pub fn total(&self) -> u64 {
        [
            self.running,
            self.completed,
            self.cancelled,
            self.failed,
            self.interrupted,
            self.superseded,
            self.stopped,
        ]
        .iter()
        .fold(0u64, |a, b| a.saturating_add(*b))
    }
}

/// The newest session, when it is unfinished: running (or interrupted by a
/// crash, which only the next write open can tell), cancelled, interrupted
/// or stopped. Whether it can actually be continued is decided by the
/// import's resume rules (§8.2), not here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ResumableSession {
    pub session: i64,
    pub state: SessionState,
    /// The session's import options. A continue must use the same ones
    /// (D64), so "Continue importing…" starts from them.
    pub include_recently_deleted: bool,
    pub copy_attachments: bool,
}

/// What listing and details show about an archive. Content-free, except
/// the label, which is the user's own text.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ArchiveInfo {
    pub id: ArchiveId,
    pub label: LabelState,
    pub schema_version: u32,
    pub created_at_ns: i64,
    /// `None` until the first import finishes.
    pub stats: Option<ArchiveStats>,
    pub sessions: SessionCounts,
    pub resumable: Option<ResumableSession>,
    pub import_blocked: Option<ImportBlock>,
    /// `sqlite_stat1` or `sqlite_stat4` exists (the app never creates
    /// them; tolerated by the fingerprint and reported, §7.3).
    pub stat_tables: bool,
}

/// One import session, content-free.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct SessionInfo {
    pub id: i64,
    pub state: SessionState,
    pub started_at_ns: i64,
    pub ended_at_ns: Option<i64>,
    pub schema_version: u32,
    pub resumes: u32,
    /// Set at a terminal state (§8.7); `None` before.
    pub full_observation: Option<bool>,
}

/// The newest import session, as [`ArchiveReader::last_import`] reads it.
/// Content-free.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct LastImport {
    pub session: i64,
    pub lineage: LineageId,
    pub state: SessionState,
    pub started_at_ns: i64,
    pub ended_at_ns: Option<i64>,
    pub resumes: u32,
    /// The counts stored with the session's last batch; `None` before the
    /// first batch, or when they do not parse.
    pub counts: Option<ImportCounts>,
    /// Why a stopped session stopped (state 6), from `import_issue`.
    pub stop_reason: Option<StopReason>,
    /// The session's derived values are provisional (D79).
    pub ios_version_untested: bool,
}

/// What the shell knows about a listed backup, for pre-selecting the
/// backup an unfinished session came from
/// ([`ArchiveReader::resume_matches`]). Compared inside this crate; the
/// archive's stored device facts never leave it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceFacts<'a> {
    pub device_name: Option<&'a str>,
    pub product_type: Option<&'a str>,
    /// Info.plist "Last Backup Date", in whole seconds since the Unix epoch.
    pub last_backup_secs: Option<i64>,
}

/// The id of a `blob` row: one file in the attachment store. Ids are
/// archive-local numbers, never content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlobId(i64);

impl BlobId {
    /// The id with this number (M5 gets ids from the reader's queries).
    pub fn new(id: i64) -> BlobId {
        BlobId(id)
    }

    /// The number.
    pub fn get(self) -> i64 {
        self.0
    }
}

/// The most sessions [`ArchiveReader::sessions`] returns (newest first).
pub const SESSIONS_LIMIT: u32 = 1000;

/// What the checks before any table read found.
#[derive(Debug, Clone)]
pub(crate) struct Inspected {
    pub(crate) version: u32,
    pub(crate) import_blocked: Option<ImportBlock>,
    pub(crate) stat_tables: bool,
}

/// `archive_meta`, as read and checked.
#[derive(Debug, Clone)]
struct Meta {
    schema_version: i64,
    created_at_ns: i64,
    label: LabelState,
    max_deriver_version: i64,
    max_decoder_version: i64,
}

fn unreadable() -> ArchiveError {
    ArchiveError::Damaged(DamageKind::Unreadable)
}

fn read_meta(conn: &Connection) -> rusqlite::Result<Option<Result<Meta, ArchiveError>>> {
    let mut stmt = conn.prepare(
        "SELECT format, schema_version, created_at_ns, label, name_scheme, \
                max_deriver_version, max_decoder_version \
         FROM archive_meta WHERE id = 1",
    )?;
    let mut rows = stmt.query([])?;
    let Some(r) = rows.next()? else {
        return Ok(None);
    };
    let format_ok = matches!(r.get_ref(0)?, ValueRef::Text(t) if t == FORMAT.as_bytes());
    let int = |i: usize| -> rusqlite::Result<Option<i64>> {
        Ok(match r.get_ref(i)? {
            ValueRef::Integer(n) => Some(n),
            _ => None,
        })
    };
    let (
        Some(schema_version),
        Some(created_at_ns),
        Some(name_scheme),
        Some(deriver),
        Some(decoder),
    ) = (int(1)?, int(2)?, int(4)?, int(5)?, int(6)?)
    else {
        return Ok(Some(Err(unreadable())));
    };
    if !format_ok || name_scheme != 1 {
        return Ok(Some(Err(ArchiveError::Damaged(
            DamageKind::MetaInconsistent,
        ))));
    }
    let label = match r.get_ref(3)? {
        ValueRef::Null => LabelState::None,
        ValueRef::Text(t) => match std::str::from_utf8(t).ok().map(Label::parse) {
            Some(Ok(l)) => LabelState::Ok(l),
            _ => LabelState::Unreadable,
        },
        _ => LabelState::Unreadable,
    };
    Ok(Some(Ok(Meta {
        schema_version,
        created_at_ns,
        label,
        max_deriver_version: deriver,
        max_decoder_version: decoder,
    })))
}

pub(crate) fn meta_label(conn: &ReadConn) -> Result<LabelState, ArchiveError> {
    meta(conn).map(|m| m.label)
}

fn meta(conn: &ReadConn) -> Result<Meta, ArchiveError> {
    conn.budgeted(READ_OP_BUDGET, Stage::Meta, read_meta)?
        .ok_or(ArchiveError::Damaged(DamageKind::MetaMissing))?
}

/// §5.3 steps 5–7 on a fresh ReadOnly connection: the header, the
/// fingerprint for the file's version, then `archive_meta`. An older
/// version is not an error here; the caller decides.
pub(crate) fn inspect(conn: &ReadConn, plan: &SchemaPlan) -> Result<Inspected, ArchiveError> {
    let (app_id, user_version): (i64, i64) =
        conn.budgeted(READ_OP_BUDGET, Stage::Fingerprint, |c| {
            let a: i64 = c.query_row("PRAGMA application_id", [], |r| r.get(0))?;
            let v: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
            Ok((a, v))
        })?;
    if app_id != i64::from(APPLICATION_ID) {
        return Err(ArchiveError::NotAnArchive);
    }
    let version = u32::try_from(user_version)
        .map_err(|_| ArchiveError::Damaged(DamageKind::SchemaMismatch))?;
    if version > plan.version {
        return Err(ArchiveError::NewerSchema { found: version });
    }
    if version < 1 {
        return Err(ArchiveError::Damaged(DamageKind::SchemaMismatch));
    }
    let expected = fingerprint::expected(plan, version)?;
    let facts = conn.budgeted(READ_OP_BUDGET, Stage::Fingerprint, |c| {
        fingerprint::check_raw(c, &expected)
    })??;
    let m = meta(conn)?;
    if m.schema_version != i64::from(version) {
        return Err(ArchiveError::Damaged(DamageKind::MetaInconsistent));
    }
    let newer_rules = m.max_deriver_version > i64::from(DERIVER_VERSION)
        || m.max_decoder_version > i64::from(DECODER_VERSION);
    Ok(Inspected {
        version,
        import_blocked: newer_rules.then_some(ImportBlock::NewerRules),
        stat_tables: facts.stat_tables,
    })
}

/// A lock-free, query-only view of one archive.
pub struct ArchiveReader {
    conn: ReadConn,
    dir: ArchiveDir,
    id: ArchiveId,
    inspected: Inspected,
    build_version: u32,
}

impl std::fmt::Debug for ArchiveReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArchiveReader")
            .field("schema_version", &self.inspected.version)
            .finish_non_exhaustive()
    }
}

fn u64_of(n: i64) -> Result<u64, ArchiveError> {
    u64::try_from(n).map_err(|_| unreadable())
}

impl ArchiveReader {
    /// Open a reader on `dir`, whose directory, permission and volume
    /// checks the caller has run. Refuses a newer schema; an older one is
    /// allowed here (the caller decides).
    pub(crate) fn open(
        dir: &ArchiveDir,
        id: ArchiveId,
        plan: &SchemaPlan,
    ) -> Result<ArchiveReader, ArchiveError> {
        let conn = ReadConn::open(dir)?;
        let inspected = inspect(&conn, plan)?;
        Ok(ArchiveReader {
            conn,
            dir: dir.clone(),
            id,
            inspected,
            build_version: plan.version,
        })
    }

    pub(crate) fn inspected(&self) -> &Inspected {
        &self.inspected
    }

    pub(crate) fn conn(&self) -> &ReadConn {
        &self.conn
    }

    pub(crate) fn set_version(&mut self, version: u32) {
        self.inspected.version = version;
    }

    /// The archive's id.
    pub fn id(&self) -> &ArchiveId {
        &self.id
    }

    /// The archive's schema version.
    pub fn schema_version(&self) -> u32 {
        self.inspected.version
    }

    /// Reads need the current schema; an older one gives `NeedsMigration`.
    fn require_current(&self) -> Result<(), ArchiveError> {
        if self.inspected.version < self.build_version {
            Err(ArchiveError::NeedsMigration {
                found: self.inspected.version,
            })
        } else {
            Ok(())
        }
    }

    /// The archive's details.
    pub fn info(&self) -> Result<ArchiveInfo, ArchiveError> {
        self.require_current()?;
        let m = meta(&self.conn)?;
        let stats = self
            .conn
            .budgeted(READ_OP_BUDGET, Stage::Read, |c| {
                let mut stmt = c.prepare(
                    "SELECT handles, chats, messages, body_versions, attachments, blobs, \
                            blob_bytes, computed_at_ns FROM archive_stats WHERE id = 1",
                )?;
                let mut rows = stmt.query([])?;
                let Some(r) = rows.next()? else {
                    return Ok(None);
                };
                let mut v = [0i64; 8];
                for (i, slot) in v.iter_mut().enumerate() {
                    *slot = r.get(i)?;
                }
                Ok(Some(v))
            })?
            .map(|v| -> Result<ArchiveStats, ArchiveError> {
                let [h, c, m, b, a, bl, bb, at] = v;
                Ok(ArchiveStats {
                    handles: u64_of(h)?,
                    chats: u64_of(c)?,
                    messages: u64_of(m)?,
                    body_versions: u64_of(b)?,
                    attachments: u64_of(a)?,
                    blobs: u64_of(bl)?,
                    blob_bytes: u64_of(bb)?,
                    computed_at_ns: at,
                })
            })
            .transpose()?;
        let by_state: Vec<(i64, i64)> = self.conn.budgeted(READ_OP_BUDGET, Stage::Read, |c| {
            let mut stmt =
                c.prepare("SELECT state, count(*) FROM import_session GROUP BY state")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect()
        })?;
        let mut sessions = SessionCounts::default();
        for (state, n) in by_state {
            let n = u64_of(n)?;
            let slot = match SessionState::from_code(state).ok_or_else(unreadable)? {
                SessionState::Running => &mut sessions.running,
                SessionState::Completed => &mut sessions.completed,
                SessionState::Cancelled => &mut sessions.cancelled,
                SessionState::Failed => &mut sessions.failed,
                SessionState::Interrupted => &mut sessions.interrupted,
                SessionState::Superseded => &mut sessions.superseded,
                SessionState::Stopped => &mut sessions.stopped,
            };
            *slot = n;
        }
        type Newest = (i64, i64, i64, i64);
        let newest: Option<Newest> = self.conn.budgeted(READ_OP_BUDGET, Stage::Read, |c| {
            let mut stmt = c.prepare(
                "SELECT id, state, include_recently_deleted, copy_attachments \
                 FROM import_session ORDER BY id DESC LIMIT 1",
            )?;
            let mut rows = stmt.query([])?;
            match rows.next()? {
                Some(r) => Ok(Some((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))),
                None => Ok(None),
            }
        })?;
        let resumable = match newest {
            Some((session, state, rd, copy)) => {
                let state = SessionState::from_code(state).ok_or_else(unreadable)?;
                matches!(
                    state,
                    SessionState::Running
                        | SessionState::Cancelled
                        | SessionState::Interrupted
                        | SessionState::Stopped
                )
                .then_some(ResumableSession {
                    session,
                    state,
                    include_recently_deleted: rd == 1,
                    copy_attachments: copy == 1,
                })
            }
            None => None,
        };
        Ok(ArchiveInfo {
            id: self.id.clone(),
            label: m.label,
            schema_version: self.inspected.version,
            created_at_ns: m.created_at_ns,
            stats,
            sessions,
            resumable,
            import_blocked: self.inspected.import_blocked,
            stat_tables: self.inspected.stat_tables,
        })
    }

    /// Run `f` on the reader's connection under an op budget (the import's
    /// read-only preflight steps).
    pub(crate) fn budgeted<T>(
        &self,
        ops: u64,
        stage: Stage,
        f: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> Result<T, ArchiveError> {
        self.conn.budgeted(ops, stage, f)
    }

    /// Open the attachment file of blob `id` for reading (spec §4.3, §9.5):
    /// the row's `storage_name` is parsed and must spell its `sha256`;
    /// `attachments/` and `<hh>/` must be private real directories; the
    /// file is opened without following a link and must be a regular file
    /// of `byte_len` bytes. The [`BlobReader`] re-hashes what it yields and
    /// fails its last read when the content differs from the digest.
    /// Works while an import holds the archive's lock: placed files are
    /// never replaced.
    pub fn open_blob(&self, id: BlobId) -> Result<BlobReader, ArchiveError> {
        self.require_current()?;
        let row: Option<(Vec<u8>, i64, Option<String>)> =
            self.conn.budgeted(READ_OP_BUDGET, Stage::Read, |c| {
                let mut stmt =
                    c.prepare("SELECT sha256, byte_len, storage_name FROM blob WHERE id = ?1")?;
                let mut rows = stmt.query([id.0])?;
                let Some(r) = rows.next()? else {
                    return Ok(None);
                };
                let sha = match r.get_ref(0)? {
                    ValueRef::Blob(b) => b.to_vec(),
                    _ => Vec::new(),
                };
                let len = match r.get_ref(1)? {
                    ValueRef::Integer(n) => n,
                    _ => -1,
                };
                let name = match r.get_ref(2)? {
                    ValueRef::Text(t) => std::str::from_utf8(t).ok().map(str::to_owned),
                    _ => None,
                };
                Ok(Some((sha, len, name)))
            })?;
        let (sha, len, name) = row.ok_or(ArchiveError::Blob(BlobProblem::NotFound))?;
        let name = name
            .as_deref()
            .and_then(StorageName::parse)
            .filter(|n| n.sha256().is_some_and(|s| s.as_slice() == sha.as_slice()))
            .ok_or(ArchiveError::Blob(BlobProblem::BadName))?;
        open_checked(&self.dir, &name, len).map_err(ArchiveError::Blob)
    }

    /// The newest verify run, if any (spec §4.3). A run a crash
    /// interrupted has no outcome; counts that do not parse are `None`.
    pub fn last_verify(&self) -> Result<Option<VerifySummary>, ArchiveError> {
        self.require_current()?;
        let row = self.conn.budgeted(READ_OP_BUDGET, Stage::Read, |c| {
            let mut stmt = c.prepare(
                "SELECT id, started_at_ns, ended_at_ns, depth, outcome, counts \
                 FROM verify_run ORDER BY id DESC LIMIT 1",
            )?;
            let mut rows = stmt.query([])?;
            let Some(r) = rows.next()? else {
                return Ok(None);
            };
            Ok(Some((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, Option<i64>>(4)?,
                r.get::<_, Option<String>>(5)?,
            )))
        })?;
        row.map(|(run, started, ended, depth, outcome, counts)| {
            VerifySummary::from_row(run, started, ended, depth, outcome, counts.as_deref())
                .ok_or_else(unreadable)
        })
        .transpose()
    }

    /// The newest import session, if any (spec §15.2: the details' last
    /// import summary).
    pub fn last_import(&self) -> Result<Option<LastImport>, ArchiveError> {
        self.require_current()?;
        type Row = (i64, i64, i64, i64, Option<i64>, i64, Option<String>, i64);
        let row: Option<Row> = self.conn.budgeted(READ_OP_BUDGET, Stage::Read, |c| {
            let mut stmt = c.prepare(
                "SELECT id, lineage_id, state, started_at_ns, ended_at_ns, resumes, counts, \
                        ios_version_untested \
                 FROM import_session ORDER BY id DESC LIMIT 1",
            )?;
            let mut rows = stmt.query([])?;
            let Some(r) = rows.next()? else {
                return Ok(None);
            };
            Ok(Some((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
            )))
        })?;
        let Some((session, lineage, state, started, ended, resumes, counts, untested)) = row else {
            return Ok(None);
        };
        let state = SessionState::from_code(state).ok_or_else(unreadable)?;
        let stop_reason = if state == SessionState::Stopped {
            let kinds: Vec<i64> = self.conn.budgeted(READ_OP_BUDGET, Stage::Read, |c| {
                let mut stmt = c.prepare(
                    "SELECT kind FROM import_issue WHERE session_id = ?1 AND kind IN (?2, ?3) \
                     ORDER BY pass DESC, kind",
                )?;
                let rows = stmt.query_map(
                    rusqlite::params![
                        session,
                        IssueCode::InsufficientSpace.code(),
                        IssueCode::SourceChanged.code()
                    ],
                    |r| r.get(0),
                )?;
                rows.collect()
            })?;
            kinds.first().and_then(|k| match IssueCode::from_code(*k) {
                Some(IssueCode::InsufficientSpace) => Some(StopReason::InsufficientSpace),
                Some(IssueCode::SourceChanged) => Some(StopReason::SourceChanged),
                _ => None,
            })
        } else {
            None
        };
        Ok(Some(LastImport {
            session,
            lineage: LineageId::new(lineage),
            state,
            started_at_ns: started,
            ended_at_ns: ended,
            resumes: u32::try_from(resumes).map_err(|_| unreadable())?,
            counts: counts.as_deref().and_then(ImportCounts::from_json),
            stop_reason,
            ios_version_untested: untested == 1,
        }))
    }

    /// For each of `backups`, whether it looks like the source of the
    /// archive's newest session when that session is unfinished (the one
    /// [`ArchiveInfo::resumable`] names): the same device name, product
    /// type and Info.plist backup date (to the second; both absent counts
    /// as equal). Only a pre-selection for the UI: whether the session can
    /// actually continue is decided by the import's resume rules (§8.2),
    /// which compare content digests. All `false` when nothing is
    /// unfinished.
    pub fn resume_matches(&self, backups: &[SourceFacts<'_>]) -> Result<Vec<bool>, ArchiveError> {
        self.require_current()?;
        type Row = (i64, Option<Vec<u8>>, Option<Vec<u8>>, Option<i64>);
        let row: Option<Row> = self.conn.budgeted(READ_OP_BUDGET, Stage::Read, |c| {
            let mut stmt = c.prepare(
                "SELECT s.state, b.device_name, b.product_type, b.info_last_backup_ns \
                 FROM import_session s JOIN source_backup b ON b.id = s.source_backup_id \
                 ORDER BY s.id DESC LIMIT 1",
            )?;
            let mut rows = stmt.query([])?;
            let Some(r) = rows.next()? else {
                return Ok(None);
            };
            let text = |i: usize| -> rusqlite::Result<Option<Vec<u8>>> {
                Ok(match r.get_ref(i)? {
                    ValueRef::Text(t) => Some(t.to_vec()),
                    _ => None,
                })
            };
            let date = match r.get_ref(3)? {
                ValueRef::Integer(n) => Some(n),
                _ => None,
            };
            Ok(Some((r.get(0)?, text(1)?, text(2)?, date)))
        })?;
        let unfinished = |state: i64| {
            matches!(
                SessionState::from_code(state),
                Some(
                    SessionState::Running
                        | SessionState::Cancelled
                        | SessionState::Interrupted
                        | SessionState::Stopped
                )
            )
        };
        let Some((_, name, model, date_ns)) = row.filter(|r| unfinished(r.0)) else {
            return Ok(vec![false; backups.len()]);
        };
        let date_secs = date_ns.map(|ns| ns.div_euclid(1_000_000_000));
        let same = |stored: &Option<Vec<u8>>, ours: Option<&str>| match (stored, ours) {
            (Some(s), Some(o)) => s.as_slice() == o.as_bytes(),
            _ => false,
        };
        Ok(backups
            .iter()
            .map(|b| {
                same(&name, b.device_name)
                    && same(&model, b.product_type)
                    && date_secs == b.last_backup_secs
            })
            .collect())
    }

    /// The newest [`SESSIONS_LIMIT`] sessions, newest first. Content-free.
    pub fn sessions(&self) -> Result<Vec<SessionInfo>, ArchiveError> {
        self.require_current()?;
        type Row = (i64, i64, i64, Option<i64>, i64, i64, Option<i64>);
        let rows: Vec<Row> = self.conn.budgeted(READ_OP_BUDGET, Stage::Read, |c| {
            let mut stmt = c.prepare(
                "SELECT id, state, started_at_ns, ended_at_ns, schema_version, resumes, \
                        full_observation \
                 FROM import_session ORDER BY id DESC LIMIT ?1",
            )?;
            let rows = stmt.query_map([SESSIONS_LIMIT], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            })?;
            rows.collect()
        })?;
        rows.into_iter()
            .map(|(id, state, started, ended, schema, resumes, full)| {
                Ok(SessionInfo {
                    id,
                    state: SessionState::from_code(state).ok_or_else(unreadable)?,
                    started_at_ns: started,
                    ended_at_ns: ended,
                    schema_version: u32::try_from(schema).map_err(|_| unreadable())?,
                    resumes: u32::try_from(resumes).map_err(|_| unreadable())?,
                    full_observation: match full {
                        None => None,
                        Some(0) => Some(false),
                        Some(1) => Some(true),
                        Some(_) => return Err(unreadable()),
                    },
                })
            })
            .collect()
    }
}
