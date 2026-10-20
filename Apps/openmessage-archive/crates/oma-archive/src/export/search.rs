//! The search scope of an export (M6 spec §3.6, D137).
//!
//! - **The re-run.** By default an export covers the live result set the
//!   user has (at most [`SEARCH_MATCH_CAP`](crate::SEARCH_MATCH_CAP)). With
//!   "all matches", [`ExportSnapshot::search_ids`] runs the same request
//!   again inside the job's snapshot with a cap of up to
//!   [`EXPORT_SEARCH_CAP`]: the same words plans and exact-text scan as
//!   the viewer's, but in the snapshot's cancellable mode (no op budget,
//!   no deadline). The exact scan keeps its
//!   [`SCAN_EXAMINED_LIMIT`](crate::SCAN_EXAMINED_LIMIT) bound, and a set
//!   still over the cap says `capped`.
//! - **Membership never reveals hidden content** (C1.6).
//!   [`ExportSnapshot::search_scope`] re-checks every hit against the text
//!   this export will write for it, after masking under the export's own
//!   reveal and depth: the current body's shown segments, the revealed
//!   earlier text, and (with history) every version's shown text. A hit
//!   whose request matches none of that is **excluded**. Matching
//!   includes the text of one-time codes: the full-text index and the
//!   exact scan both read the whole displayed text, codes included (tested
//!   here, so the spec's open question is settled), which is why a hit
//!   matched only in a masked code is excluded unless codes are revealed.
//!   A phrase never matches across a withheld range: each shown run is
//!   checked on its own.
//! - **How the re-check matches.** Exact text: `str::contains` on each
//!   shown run, as the scan does. Words: each term's own FTS5 expression
//!   (the request's expression is these joined with ` AND `) against an
//!   in-memory FTS5 table with the archive's tokenizer, one row per shown
//!   run; every term must match a run of the same text. The table lives in
//!   a private in-memory database for the call and holds shown text only.
//! - **Fast path.** A hit on its current body whose body has no flagged
//!   code is written as matched (nothing in it is masked), so most hits
//!   cost one primary-key probe.
//! - **The excluded counts** ([`ExcludedHits`]) are for the Review and
//!   result steps only, never for an output (the count would itself say
//!   that hidden content matched).
//! - **Grouping.** Kept hits are grouped by the conversation M5's search
//!   names for them (the lowest chat id), conversations in id order and
//!   "no conversation" last, each group in timeline order (`sort_key`,
//!   then id).
//!
//! The query text never leaves the request: nothing here stores, logs or
//! returns it.

use rusqlite::{Connection, OptionalExtension};

use super::mask::{current_body, has_code};
use super::measure::ExportDepth;
use super::preview::{PreviewOptions, SQL_PRE_MESSAGE};
use super::scope::{ExportScopeIds, ScopeGroup};
use super::ExportSnapshot;
use crate::conn::open_memory;
use crate::error::{ArchiveError, Stage};
use crate::search::{key_ranges, order, scan, words, Parsed, ScanProgress, ScanStop};
use crate::search::{SearchRan, SearchRequest, SearchResultSet, WordsPlan};
use crate::view::assemble::{Caps, Reveal};
use crate::view::uncapped::export_item;
use crate::view::{BodyView, ChatId, EarlierText, MessageId, SegmentKind, TextPiece, VersionText};

/// The most matches an export's "all matches" re-run keeps.
pub const EXPORT_SEARCH_CAP: usize = 1_000_000;

/// Hits left out of an export because the request matches nothing it
/// writes for them, by what would include them. Shown in Review and the
/// result step only; never written to an output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub struct ExcludedHits {
    /// Matched only in a one-time code the phone marked: included when
    /// the export reveals codes.
    pub one_time_codes: u64,
    /// Matched only in content captured before an unsend or edit:
    /// included when the export reveals earlier content.
    pub earlier_content: u64,
    /// Included only with both reveals.
    pub both: u64,
    /// Matched in nothing this export writes under any reveal (an
    /// earlier version when history is not included, a message that no
    /// longer matches, or an event the export leaves out).
    pub not_written: u64,
}

impl ExcludedHits {
    pub fn total(&self) -> u64 {
        self.one_time_codes + self.earlier_content + self.both + self.not_written
    }
}

/// A search result set as an export writes it.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SearchScope {
    /// The kept hits, grouped (see the module comment).
    pub groups: Vec<ScopeGroup>,
    pub kept: u64,
    pub excluded: ExcludedHits,
    /// The set behind it held only its cap's matches.
    pub capped: bool,
    /// When capped: whether the kept matches are the newest ones.
    pub newest_kept: bool,
    /// Every message in range was examined: always `true` for words (the
    /// index is consulted as a whole); for exact text, `false` when the
    /// scan stopped at its examined bound or its cap.
    pub coverage_complete: bool,
}

impl std::fmt::Debug for SearchScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchScope")
            .field("groups", &self.groups.len())
            .field("kept", &self.kept)
            .field("excluded", &self.excluded)
            .field("capped", &self.capped)
            .field("newest_kept", &self.newest_kept)
            .field("coverage_complete", &self.coverage_complete)
            .finish()
    }
}

impl SearchScope {
    /// The scope the export walks.
    pub fn into_scope(self) -> ExportScopeIds {
        ExportScopeIds::Messages(self.groups)
    }
}

pub(crate) const SQL_HIT_CHAT: &str =
    "SELECT chat_id FROM chat_message WHERE message_id = ?1 ORDER BY chat_id LIMIT 1";

#[cfg_attr(not(feature = "fault-injection"), allow(dead_code))]
pub(crate) fn plan_statements() -> Vec<(&'static str, String)> {
    vec![("export hit conversation", SQL_HIT_CHAT.to_owned())]
}

/// The tokenizer of `message_fts` (`schema/v1.sql`; a test compares).
pub(crate) const FTS_TOKENIZE: &str = "unicode61 remove_diacritics 2";

/// How a request is re-checked against shown text.
enum Matcher {
    Exact(String),
    Words { terms: Vec<String>, mem: Connection },
}

fn mem_err(e: &rusqlite::Error) -> ArchiveError {
    ArchiveError::sqlite(Stage::View, e)
}

impl Matcher {
    fn of(parsed: Parsed) -> Result<Matcher, ArchiveError> {
        match parsed {
            Parsed::Exact(n) => Ok(Matcher::Exact(n)),
            Parsed::Words { terms, .. } => {
                let mem = open_memory()?;
                mem.execute_batch(&format!(
                    "CREATE VIRTUAL TABLE w USING fts5(t, tokenize = '{FTS_TOKENIZE}')"
                ))
                .map_err(|e| mem_err(&e))?;
                Ok(Matcher::Words { terms, mem })
            }
        }
    }

    /// Whether the request matches one text, given as its shown runs.
    fn matches(&self, runs: &[String]) -> Result<bool, ArchiveError> {
        match self {
            Matcher::Exact(needle) => Ok(runs.iter().any(|r| r.contains(needle.as_str()))),
            Matcher::Words { terms, mem } => {
                let run = || -> rusqlite::Result<bool> {
                    mem.execute("DELETE FROM w", [])?;
                    {
                        let mut ins = mem.prepare("INSERT INTO w(t) VALUES (?1)")?;
                        for r in runs {
                            ins.execute([r])?;
                        }
                    }
                    let mut probe = mem.prepare("SELECT 1 FROM w WHERE w MATCH ?1 LIMIT 1")?;
                    for t in terms {
                        if probe.query_row([t], |_| Ok(())).optional()?.is_none() {
                            return Ok(false);
                        }
                    }
                    Ok(true)
                };
                let out = run().map_err(|e| mem_err(&e));
                // The table never keeps shown text past one check.
                let _ = mem.execute("DELETE FROM w", []);
                out
            }
        }
    }
}

/// The shown runs of a segment list: consecutive text segments joined,
/// split at anything else (a withheld code, an inline placeholder).
fn segment_runs(segments: &[crate::view::Segment]) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for s in segments {
        match &s.kind {
            SegmentKind::Text(t) => cur.push_str(t.text.expose()),
            _ => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// The shown runs of a text-piece list, split at masked ranges.
fn piece_runs(pieces: &[TextPiece]) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for p in pieces {
        match p {
            TextPiece::Text(t) => cur.push_str(t.text.expose()),
            _ => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// What became of one hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Membership {
    Written,
    Excluded(Exclusion),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Exclusion {
    Codes,
    Earlier,
    Both,
    NotWritten,
}

impl ExportSnapshot<'_> {
    /// Run `req` again inside this snapshot, keeping at most `cap`
    /// matches (clamped to [`EXPORT_SEARCH_CAP`]), with no op budget and
    /// no deadline (M6 spec §3.6). A cancel is `Cancelled`; a refused
    /// query `InvalidQuery`; an FTS error `SearchUnavailable`. The exact
    /// scan pushes `progress` after each batch.
    pub fn search_ids(
        &self,
        req: &SearchRequest,
        cap: usize,
        progress: &mut dyn FnMut(ScanProgress),
    ) -> Result<SearchResultSet, ArchiveError> {
        let cap = cap.clamp(1, EXPORT_SEARCH_CAP);
        let parsed = req.parse()?;
        let ranges = key_ranges(&req.filters)?;
        self.checkpoint()?;
        match &parsed {
            Parsed::Words { expr, prefix, .. } => {
                let (plan, matches, capped) =
                    words::collect_in(self.phase, req, expr, *prefix, &ranges, cap)?;
                let (entries, dated) = order(matches);
                Ok(SearchResultSet {
                    ran: SearchRan::Words,
                    plan: Some(plan),
                    entries,
                    dated,
                    capped,
                    newest_kept: plan == WordsPlan::Scoped,
                    coverage: None,
                })
            }
            Parsed::Exact(needle) => {
                let cap = u64::try_from(cap).unwrap_or(u64::MAX);
                let (matches, coverage) =
                    scan::run_in(self.phase, req, needle, &ranges, cap, progress)?;
                let (entries, dated) = order(matches);
                Ok(SearchResultSet {
                    ran: SearchRan::Exact,
                    plan: None,
                    entries,
                    dated,
                    capped: coverage.stop == Some(ScanStop::MatchCap),
                    newest_kept: true,
                    coverage: Some(coverage),
                })
            }
        }
    }

    /// The hits of `set` (made by `req`) that this export may write under
    /// `opts`, grouped, and how many it leaves out (see the module
    /// comment).
    pub fn search_scope(
        &self,
        req: &SearchRequest,
        set: &SearchResultSet,
        opts: PreviewOptions,
    ) -> Result<SearchScope, ArchiveError> {
        let parsed = req.parse()?;
        if parsed.ran() != set.ran {
            return Err(ArchiveError::Internal("search request changed"));
        }
        let matcher = Matcher::of(parsed)?;
        let mut excluded = ExcludedHits::default();
        // (no conversation last, chat, sort key, id)
        let mut kept: Vec<(bool, i64, Vec<u8>, i64)> = Vec::new();
        for e in &set.entries {
            self.checkpoint()?;
            let id = e.message.get();
            let row: Option<(Vec<u8>, Option<i64>)> = self.run(|| {
                self.stmts
                    .prepare(SQL_PRE_MESSAGE)?
                    .query_row([id], |r| Ok((r.get(0)?, r.get(1)?)))
                    .optional()
            })?;
            let Some((key, current)) = row else {
                excluded.not_written += 1;
                continue;
            };
            // An event this export leaves out (slice 6k2) is written
            // nowhere.
            if (opts.leave_out_events || self.leaves_out_events())
                && super::scope::is_event(self, id)?
            {
                excluded.not_written += 1;
                continue;
            }
            let fast = e.current
                && current == Some(e.body.get())
                && !self.run(|| has_code(&self.stmts, e.body.get()))?;
            let m = if fast {
                Membership::Written
            } else {
                self.classify(&matcher, id, opts)?
            };
            match m {
                Membership::Written => {
                    let chat: Option<i64> = self.run(|| {
                        self.stmts
                            .prepare(SQL_HIT_CHAT)?
                            .query_row([id], |r| r.get(0))
                            .optional()
                    })?;
                    kept.push((chat.is_none(), chat.unwrap_or(0), key, id));
                }
                Membership::Excluded(Exclusion::Codes) => excluded.one_time_codes += 1,
                Membership::Excluded(Exclusion::Earlier) => excluded.earlier_content += 1,
                Membership::Excluded(Exclusion::Both) => excluded.both += 1,
                Membership::Excluded(Exclusion::NotWritten) => excluded.not_written += 1,
            }
        }
        kept.sort_unstable();
        let total = u64::try_from(kept.len()).unwrap_or(u64::MAX);
        let mut groups: Vec<ScopeGroup> = Vec::new();
        for (none, chat, _, id) in kept {
            let chat = (!none).then(|| ChatId::new(chat));
            match groups.last_mut() {
                Some(g) if g.chat == chat => g.messages.push(MessageId::new(id)),
                _ => groups.push(ScopeGroup::new(chat, vec![MessageId::new(id)])),
            }
        }
        Ok(SearchScope {
            groups,
            kept: total,
            excluded,
            capped: set.capped,
            newest_kept: set.newest_kept,
            coverage_complete: set.coverage.is_none_or(|c| c.complete),
        })
    }

    /// Whether `matcher` matches what this export writes for message `id`
    /// under `opts`, and if not, which reveal would include it.
    fn classify(
        &self,
        matcher: &Matcher,
        id: i64,
        opts: PreviewOptions,
    ) -> Result<Membership, ArchiveError> {
        let r = opts.reveal;
        if self.written_matches(matcher, id, r, opts.depth)? {
            return Ok(Membership::Written);
        }
        let with = |codes: bool, earlier: bool| Reveal {
            one_time_code: r.one_time_code || codes,
            earlier_text: r.earlier_text || earlier,
        };
        if !r.one_time_code && self.written_matches(matcher, id, with(true, false), opts.depth)? {
            return Ok(Membership::Excluded(Exclusion::Codes));
        }
        if !r.earlier_text && self.written_matches(matcher, id, with(false, true), opts.depth)? {
            return Ok(Membership::Excluded(Exclusion::Earlier));
        }
        if (!r.one_time_code || !r.earlier_text)
            && self.written_matches(matcher, id, with(true, true), opts.depth)?
        {
            return Ok(Membership::Excluded(Exclusion::Both));
        }
        Ok(Membership::Excluded(Exclusion::NotWritten))
    }

    /// Whether the request matches any text written for message `id`
    /// under `reveal` and `depth`, each text checked run by run.
    fn written_matches(
        &self,
        matcher: &Matcher,
        id: i64,
        reveal: Reveal,
        depth: ExportDepth,
    ) -> Result<bool, ArchiveError> {
        let Some(item) =
            self.run(|| export_item(&self.stmts, &self.ctx, None, id, &Caps::EXPORT, reveal))?
        else {
            return Ok(false);
        };
        let item = crate::view::uncapped::checked(item)?;
        let m = &item.message;
        if let BodyView::Text(t) = &m.body {
            if matcher.matches(&segment_runs(&t.segments))? {
                return Ok(true);
            }
        }
        if let EarlierText::Revealed { text, .. } = &m.edit.earlier {
            if matcher.matches(&piece_runs(text))? {
                return Ok(true);
            }
        }
        if depth == ExportDepth::WithHistory && current_body(m).is_some() {
            let mut h = super::cursor::HistoryCursor::new(self, m, reveal)?;
            while let Some(v) = h.next_version()? {
                if let VersionText::Text(pieces) = &v.text {
                    if matcher.matches(&piece_runs(pieces))? {
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tokenizer_is_the_archives() {
        let v1 = include_str!("../schema/v1.sql");
        assert!(
            v1.contains(&format!("tokenize = '{FTS_TOKENIZE}'")),
            "message_fts's tokenizer changed"
        );
    }

    #[test]
    fn a_phrase_never_matches_across_a_withheld_run() {
        let parsed = SearchRequest::new("\"alpha beta\"").parse().expect("parse");
        let words = Matcher::of(parsed).expect("matcher");
        let joined = vec!["xx alpha beta yy".to_owned()];
        let split = vec!["xx alpha".to_owned(), "beta yy".to_owned()];
        assert!(words.matches(&joined).expect("joined"));
        assert!(!words.matches(&split).expect("split"));
        // Two terms may sit in two runs of the same text.
        let parsed = SearchRequest::new("alpha beta").parse().expect("parse");
        let words = Matcher::of(parsed).expect("matcher");
        assert!(words.matches(&split).expect("terms"));
        assert!(!words.matches(&["alpha".to_owned()]).expect("one term"));
        // Folding is the archive's: case and accents.
        let parsed = SearchRequest::new("Café").parse().expect("parse");
        let words = Matcher::of(parsed).expect("matcher");
        assert!(words.matches(&["un cafe noir".to_owned()]).expect("folded"));
        let mut q = SearchRequest::new("ab cd");
        q.mode = crate::search::SearchMode::Exact;
        let exact = Matcher::of(q.parse().expect("parse")).expect("matcher");
        assert!(exact.matches(&["xab cdx".to_owned()]).expect("exact"));
        assert!(!exact
            .matches(&["xab".to_owned(), " cdx".to_owned()])
            .expect("split"));
    }
}
