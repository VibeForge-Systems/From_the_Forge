//! The gate (spec §8.2 step 7, D65, D80): the confirmation inside the
//! import job, after every preflight read and before the archive's first
//! write.
//!
//! [`Archive::import`](crate::Archive::import) builds a [`Preflight`] and
//! calls [`ImportGate::confirm`]. The shell (slice 4l) blocks its worker
//! there until the user answers, cancels, or two minutes pass. **Decline
//! writes nothing**: no write connection has been opened, so
//! `archive.sqlite`, its `-wal`, `attachments/` and `imports/` stay
//! byte-identical (the `-shm`, SQLite's WAL index, may change).
//!
//! The preflight holds counts, closed kinds, booleans, small integers
//! (session and lineage numbers) and backup dates as Unix nanoseconds. It
//! never holds a GUID, digest, rank, rowid, cursor, path, label, device
//! name or product type: device facts are compared here and cross as
//! booleans only (D51).

use rusqlite::types::ValueRef;
use rusqlite::{Connection, OptionalExtension};

use super::preflight::{Binding, KeyTally, RdPlan};
use super::session::SessionFacts;
use super::{LineageChoice, LineageId};
use crate::schema::codes::{OrderKeySource, Pass, QualifierBit, SessionState};
use crate::space::SpaceEstimate;

/// Asked once per import, after the preflight reads.
pub trait ImportGate {
    /// The user's answer to `preflight`.
    fn confirm(&mut self, preflight: &Preflight) -> GateDecision;
}

/// The answer at the gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDecision {
    /// Import, as part of this lineage (D80).
    Proceed { lineage: LineageChoice },
    /// Do not import. Nothing is written.
    Decline,
}

/// A gate that always proceeds with one lineage choice, for tests and
/// tools that already know it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProceedWith(pub LineageChoice);

impl ImportGate for ProceedWith {
    fn confirm(&mut self, _preflight: &Preflight) -> GateDecision {
        GateDecision::Proceed { lineage: self.0 }
    }
}

/// A gate that proceeds with the preflight's suggested lineage.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProceedWithSuggestion;

impl ImportGate for ProceedWithSuggestion {
    fn confirm(&mut self, preflight: &Preflight) -> GateDecision {
        GateDecision::Proceed {
            lineage: preflight.suggested_lineage,
        }
    }
}

/// Whether the import continues an unfinished session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResumePlan {
    /// Session `session` of `lineage` can continue, from pass `from`
    /// ([`Pass::Finalize`] when every pass is done). It continues only if
    /// that lineage is chosen; any other choice sets it aside.
    Continue {
        session: i64,
        lineage: LineageId,
        from: Pass,
    },
    /// A new session. `supersedes` unfinished sessions are set aside
    /// (state 5) after Proceed.
    Fresh {
        supersedes: u32,
        reason: FreshReason,
    },
}

/// Why the import starts a new session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FreshReason {
    /// No unfinished session.
    NothingToContinue,
    /// The unfinished sessions were made from different content, framing,
    /// options or code (the backup changed, or the app was updated).
    SourceOrBuildChanged,
}

/// Whether there is anything to compare the device with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DeviceEvidence {
    /// No lineage has a full observation yet.
    FirstImport,
    /// Compared with each lineage's newest full observation
    /// ([`LineageFacts`]).
    Compared,
}

/// One lineage, as the gate shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct LineageFacts {
    pub id: LineageId,
    /// Its import sessions, in any state.
    pub sessions: u64,
    /// Its full observations (spec §8.7).
    pub full_sessions: u64,
    /// The backup date of its newest full observation (Unix ns), when that
    /// session was dated.
    pub last_backup_ns: Option<i64>,
    /// The backup's product type equals that session's; `None` when either
    /// side lacks one, or the lineage has no full observation.
    pub model_matches: Option<bool>,
    /// The backup's device name equals that session's; `None` likewise.
    pub name_matches: Option<bool>,
}

impl LineageFacts {
    /// Both the model and the name match.
    pub fn device_matches(&self) -> bool {
        self.model_matches == Some(true) && self.name_matches == Some(true)
    }
}

/// The key facts of one keyed table.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct KeyCountFacts {
    /// Rows in the source table.
    pub rows: u64,
    /// Rows with a usable key.
    pub keyable: u64,
    /// Distinct keys that occur more than once (every occurrence is
    /// imported unkeyed, spec §6.2).
    pub duplicated_keys: u64,
    /// Rows to be imported unkeyed.
    pub unkeyed: u64,
}

impl From<KeyTally> for KeyCountFacts {
    fn from(t: KeyTally) -> Self {
        KeyCountFacts {
            rows: t.rows,
            keyable: t.keyable,
            duplicated_keys: t.duplicated_keys,
            unkeyed: t.unkeyed,
        }
    }
}

/// The key scan's facts (spec §8.2 step 3).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct KeyFacts {
    pub handles: KeyCountFacts,
    pub chats: KeyCountFacts,
    pub messages: KeyCountFacts,
    pub attachments: KeyCountFacts,
    /// Message keys unique in this backup and already in the archive. Zero
    /// overlap with a non-empty archive can be legitimate (a "Keep
    /// Messages" limit), and is shown, not refused.
    pub messages_already_archived: u64,
}

/// The Recently Deleted facts (spec §8.8).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RdFacts {
    /// Distinct messages in Recently Deleted.
    pub messages: u64,
    /// Of those, not to be imported (the default exclusion).
    pub excluded: u64,
    /// Of those, already archived: marked, not updated.
    pub already_archived: u64,
    /// Attachments linked only to excluded messages: no row, no file.
    pub attachments_excluded: u64,
}

/// Where the backup date came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OrderSource {
    StatusPlistDate,
    InfoPlistLastBackupDate,
}

impl OrderSource {
    fn of(code: OrderKeySource) -> OrderSource {
        match code {
            OrderKeySource::StatusDate => OrderSource::StatusPlistDate,
            OrderKeySource::InfoLastBackupDate => OrderSource::InfoPlistLastBackupDate,
        }
    }
}

/// How this backup ranks (spec §8.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct OrderFacts {
    /// A usable backup date was found.
    pub dated: bool,
    pub source: Option<OrderSource>,
    /// A plist date was outside 2007-01-01 .. import start + 48 h and was
    /// ignored.
    pub out_of_range: bool,
    /// This backup ranks below the archive's highest-ranked completed
    /// session: it is older than the newest backup already imported.
    pub sorts_before_latest: bool,
    /// Its date comes from a different plist than that session's (whether
    /// the two dates are comparable is UNVERIFIED).
    pub source_differs: bool,
}

/// What the gate shows (spec §4.3, §16). Content-free.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Preflight {
    pub resume: ResumePlan,
    /// A completed session already imported this exact content; proceeding
    /// only re-observes it.
    pub same_content_completed: bool,
    pub device: DeviceEvidence,
    /// Every lineage (at most [`LINEAGES_LIMIT`]), oldest first.
    pub lineages: Vec<LineageFacts>,
    /// Pre-selected at the gate: the lineage of the session that can
    /// continue, else the one lineage whose model and name match, else a
    /// new one.
    pub suggested_lineage: LineageChoice,
    pub keys: KeyFacts,
    pub recently_deleted: RdFacts,
    pub space: SpaceEstimate,
    pub order: OrderFacts,
    /// The backup is encrypted; its content is stored decrypted.
    pub encrypted_source: bool,
    /// The archive lies in a folder a cloud-sync client manages.
    pub sync_folder_warning: bool,
    /// Windows: the archive inherits the library folder's permissions.
    pub windows_acl_inherited: bool,
    /// The session's qualifiers, in code order (`iosVersionUntested` marks
    /// every derived value of this import provisional, D79).
    pub qualifiers: Vec<QualifierBit>,
}

/// The most lineages the preflight lists.
pub const LINEAGES_LIMIT: i64 = 1000;

/// What the preflight reads from the archive (on the read-only
/// connection, under an op budget).
pub(crate) struct ArchiveSide {
    pub(crate) resume: ResumePlan,
    pub(crate) same_content_completed: bool,
    pub(crate) lineages: Vec<LineageFacts>,
    /// Per lineage, its newest full observation's session id (for the
    /// suggestion).
    newest_full: Vec<Option<i64>>,
    latest: Option<(Vec<u8>, Option<i64>)>,
}

/// The first pass of `session` that is not done ([`Pass::Finalize`] when
/// all are).
fn first_open_pass(c: &Connection, session: i64) -> rusqlite::Result<Pass> {
    let mut stmt =
        c.prepare("SELECT pass FROM import_cursor WHERE session_id = ?1 AND done = 1")?;
    let done: Vec<i64> = stmt
        .query_map([session], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(Pass::ALL
        .iter()
        .copied()
        .filter(|p| *p != Pass::Finalize)
        .find(|p| !done.contains(&p.code()))
        .unwrap_or(Pass::Finalize))
}

fn text_bytes(v: ValueRef<'_>) -> Option<Vec<u8>> {
    match v {
        ValueRef::Text(b) => Some(b.to_vec()),
        _ => None,
    }
}

fn matches(stored: Option<&[u8]>, ours: Option<&str>) -> Option<bool> {
    Some(stored? == ours?.as_bytes())
}

/// Read the archive side of the preflight.
pub(crate) fn archive_side(
    c: &Connection,
    facts: &SessionFacts,
    binding: &Binding,
) -> rusqlite::Result<ArchiveSide> {
    let decision = super::preflight::decide_resume(c, binding, true)?;
    let resume = match &decision.resume {
        Some(r) => ResumePlan::Continue {
            session: r.id,
            lineage: LineageId(r.lineage),
            from: first_open_pass(c, r.id)?,
        },
        None => ResumePlan::Fresh {
            supersedes: u32::try_from(decision.supersede.len()).unwrap_or(u32::MAX),
            reason: if decision.supersede.is_empty() {
                FreshReason::NothingToContinue
            } else {
                FreshReason::SourceOrBuildChanged
            },
        },
    };
    let same_content_completed: bool = c.query_row(
        "SELECT EXISTS (SELECT 1 FROM import_session WHERE state = ?1 AND content_digest = ?2 \
             AND main_len = ?3)",
        rusqlite::params![
            SessionState::Completed.code(),
            binding.digest.as_slice(),
            binding.main_len
        ],
        |r| r.get(0),
    )?;
    let mut lineages = Vec::new();
    let mut newest_full = Vec::new();
    {
        let mut list = c.prepare(
            "SELECT l.id, count(s.id), coalesce(sum(s.full_observation = 1), 0) \
             FROM lineage l LEFT JOIN import_session s ON s.lineage_id = l.id \
             GROUP BY l.id ORDER BY l.id LIMIT ?1",
        )?;
        let mut newest = c.prepare(
            "SELECT s.id, s.order_key, b.product_type, b.device_name \
             FROM import_session s JOIN source_backup b ON b.id = s.source_backup_id \
             WHERE s.lineage_id = ?1 AND s.full_observation = 1 \
             ORDER BY s.rank DESC, s.id DESC LIMIT 1",
        )?;
        let rows: Vec<(i64, i64, i64)> = list
            .query_map([LINEAGES_LIMIT], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (id, sessions, full) in rows {
            type Newest = (i64, Option<i64>, Option<Vec<u8>>, Option<Vec<u8>>);
            let found: Option<Newest> = newest
                .query_row([id], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        text_bytes(r.get_ref(2)?),
                        text_bytes(r.get_ref(3)?),
                    ))
                })
                .optional()?;
            let (session, last_backup_ns, model_matches, name_matches) = match &found {
                Some((s, key, model, name)) => (
                    Some(*s),
                    *key,
                    matches(model.as_deref(), facts.product_type()),
                    matches(name.as_deref(), facts.device_name()),
                ),
                None => (None, None, None, None),
            };
            newest_full.push(session);
            lineages.push(LineageFacts {
                id: LineageId(id),
                sessions: u64::try_from(sessions).unwrap_or(0),
                full_sessions: u64::try_from(full).unwrap_or(0),
                last_backup_ns,
                model_matches,
                name_matches,
            });
        }
    }
    let latest = c
        .query_row(
            "SELECT rank, order_key_source FROM import_session WHERE state = ?1 \
             ORDER BY rank DESC LIMIT 1",
            [SessionState::Completed.code()],
            |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Option<i64>>(1)?)),
        )
        .optional()?;
    Ok(ArchiveSide {
        resume,
        same_content_completed,
        lineages,
        newest_full,
        latest,
    })
}

/// Assemble the preflight.
#[allow(clippy::too_many_arguments)]
pub(crate) fn preflight(
    side: ArchiveSide,
    facts: &SessionFacts,
    tallies: &[KeyTally; 4],
    rd: &RdPlan,
    attachments_excluded: u64,
    space: SpaceEstimate,
    encrypted_source: bool,
    sync_folder_warning: bool,
) -> Preflight {
    let device = if side.newest_full.iter().any(Option::is_some) {
        DeviceEvidence::Compared
    } else {
        DeviceEvidence::FirstImport
    };
    let suggested_lineage = match side.resume {
        ResumePlan::Continue { lineage, .. } => LineageChoice::Existing(lineage),
        ResumePlan::Fresh { .. } => {
            let matching: Vec<&LineageFacts> = side
                .lineages
                .iter()
                .filter(|l| l.device_matches())
                .collect();
            match matching.as_slice() {
                [one] => LineageChoice::Existing(one.id),
                _ => LineageChoice::New,
            }
        }
    };
    let ours = facts.rank.as_bytes();
    let (sorts_before_latest, source_differs) = match &side.latest {
        Some((rank, source)) => (
            ours.as_slice() < rank.as_slice(),
            match (
                facts.order_source,
                source.and_then(OrderKeySource::from_code),
            ) {
                (Some(a), Some(b)) => a != b,
                _ => false,
            },
        ),
        None => (false, false),
    };
    let [h, c, m, a] = *tallies;
    let qualifiers: Vec<QualifierBit> = QualifierBit::ALL
        .iter()
        .copied()
        .filter(|q| facts.qualifiers & q.mask() != 0)
        .collect();
    Preflight {
        resume: side.resume,
        same_content_completed: side.same_content_completed,
        device,
        lineages: side.lineages,
        suggested_lineage,
        keys: KeyFacts {
            handles: h.into(),
            chats: c.into(),
            messages: m.into(),
            attachments: a.into(),
            messages_already_archived: m.already_archived,
        },
        recently_deleted: RdFacts {
            messages: u64::try_from(rd.rd.len()).unwrap_or(u64::MAX),
            excluded: u64::try_from(rd.excluded.len()).unwrap_or(u64::MAX),
            already_archived: u64::try_from(rd.archived.len()).unwrap_or(u64::MAX),
            attachments_excluded,
        },
        space,
        order: OrderFacts {
            dated: facts.rank.is_dated(),
            source: facts.order_source.map(OrderSource::of),
            out_of_range: facts.qualifiers & QualifierBit::BackupDateOutOfRange.mask() != 0,
            sorts_before_latest,
            source_differs,
        },
        encrypted_source,
        sync_folder_warning,
        windows_acl_inherited: cfg!(windows),
        qualifiers,
    }
}

#[cfg(test)]
mod tests {
    use super::super::keys::Rank;
    use super::*;

    /// Whether `rank` is below the stored rank `other` (memcmp).
    fn below(rank: &Rank, other: &[u8]) -> bool {
        rank.as_bytes().as_slice() < other
    }

    #[test]
    fn device_facts_compare_bytes_and_need_both_sides() {
        assert_eq!(matches(Some(b"iPhone1,1"), Some("iPhone1,1")), Some(true));
        assert_eq!(matches(Some(b"iPhone1,1"), Some("iPhone1,2")), Some(false));
        assert_eq!(matches(None, Some("x")), None);
        assert_eq!(matches(Some(b"x"), None), None);
        let l = LineageFacts {
            id: LineageId(1),
            sessions: 1,
            full_sessions: 1,
            last_backup_ns: None,
            model_matches: Some(true),
            name_matches: None,
        };
        assert!(!l.device_matches());
    }

    #[test]
    fn ranks_compare_as_stored() {
        let d = [7u8; 32];
        let old = Rank::new(Some(10), &d);
        let new = Rank::new(Some(20), &d);
        assert!(below(&old, new.as_bytes()));
        assert!(!below(&new, old.as_bytes()));
        assert!(below(&Rank::new(None, &d), old.as_bytes()));
    }
}
