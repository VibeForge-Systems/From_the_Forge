//! `check()` over every backup of the messages catalogue, plain and
//! encrypted, compared with `<name>.expected/report.json` through the
//! `ExpectedReport` projection (M3 §19.3, `tests/common/expected.rs`).
//!
//! The JSON is computed by the generator from its own model, so agreement
//! proves self-consistency only; external agreement comes from the oracle
//! runs recorded in `docs/compatibility.md`.
#![recursion_limit = "512"]
#![allow(clippy::disallowed_types, clippy::disallowed_methods)]
mod common;

use std::fs;

use common::expected::{diff, project};
use common::*;
use oma_backup::OpenMode;
use oma_messages::{CheckOptions, ExtractionReport, Qualifier};

fn check_entry(name: &str) -> (ExtractionReport, Vec<String>) {
    let fx = Fixture::messages(name);
    let (result, cleanup) = run_check(&fx, catalogue_password(name), &CheckOptions::default());
    let report = result.unwrap_or_else(|e| panic!("{name}: check failed: {e:?}"));
    assert!(cleanup.removed, "{name}: staging not removed");
    assert!(fx.staged_files().is_empty(), "{name}: staged files left");

    // Compared by predicate.
    let d = &report.database;
    assert!(report.elapsed.as_nanos() > 0, "{name}");
    assert!(
        d.open_mode.is_none()
            || matches!(
                d.open_mode,
                Some(OpenMode::ReadOnly | OpenMode::ReadWriteQueryOnly)
            ),
        "{name}"
    );
    if let Some(p) = d.page_size {
        assert!(
            p.is_power_of_two() && (512..=65_536).contains(&p),
            "{name}: {p}"
        );
    }
    assert!(!d.source_changed_during_check, "{name}");
    assert!(
        !report.qualifiers().contains(&Qualifier::SourceChanged),
        "{name}"
    );
    assert!(d.plaintext_removed_after_row_pass, "{name}");

    let want: serde_json::Value = serde_json::from_slice(
        &fs::read(messages_dir().join(format!("{name}.expected/report.json")))
            .expect("report.json"),
    )
    .expect("json");
    let got = project(&report);
    let mut out = Vec::new();
    diff(name, &got, &want, &mut out);
    (report, out)
}

#[test]
fn every_catalogue_entry_matches_its_model() {
    let names = catalogue();
    assert!(names.len() >= 20, "{names:?}");
    let mut all = Vec::new();
    for name in &names {
        let (_, out) = check_entry(name);
        all.extend(out);
    }
    assert!(
        all.is_empty(),
        "{} differences:\n{}",
        all.len(),
        all.join("\n")
    );
}

/// An aggregate over its op budget leaves its counter `None` ("not
/// recorded", never 0) and states `AggregatesIncomplete` (§7.3). Budgets
/// are counted in ticks of 1,000 operations, so the database must be large
/// enough for a statement to reach one.
#[test]
fn aggregates_over_budget_are_not_recorded_and_stated() {
    let mut sql = full_schema();
    sql.push_str(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 5000)
         INSERT INTO message (ROWID, guid, date, is_from_me, handle_id, other_handle,
                              thread_originator_guid)
             SELECT i, 'SYNTH-MSG-GUID-' || i, 0, 0, i, 0, 'SYNTH-MISSING-' || i FROM n;
         WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 5000)
         INSERT INTO chat_message_join (chat_id, message_id) SELECT 1, i + 7 FROM n;
         INSERT INTO chat (guid) VALUES ('SYNTH-CHAT-GUID-1');",
    );
    let fx = Fixture::with_sms(&build(&sql));
    let opts = CheckOptions::default().with_aggregate_budget(0, 0);
    let (result, cleanup) = run_check(&fx, None, &opts);
    assert!(cleanup.removed);
    let r = result.expect("check");
    assert!(
        r.qualifiers().contains(&Qualifier::AggregatesIncomplete),
        "{:?}",
        r.qualifiers()
    );
    let f = r.failures.as_ref().expect("failures");
    assert!(f.aggregate_over_budget >= 3, "{}", f.aggregate_over_budget);
    let l = r.links.as_ref().expect("links");
    assert_eq!(l.messages_in_no_chat, None);
    assert_eq!(l.orphan_chat_message_message, None);
    let m = r.messages.as_ref().expect("messages");
    assert_eq!(m.handle_unmatched, None);
    // The GUID counts run in Rust and are unaffected.
    assert_eq!(m.reply_originator_not_found, Some(5000));

    // The same database with the default budget records every count.
    let (result, _) = run_check(&fx, None, &CheckOptions::default());
    let r = result.expect("check");
    assert!(!r.qualifiers().contains(&Qualifier::AggregatesIncomplete));
    let l = r.links.as_ref().expect("links");
    assert_eq!(l.messages_in_no_chat, Some(7));
    assert_eq!(l.orphan_chat_message_message, Some(7));
    assert_eq!(
        r.messages.as_ref().expect("messages").handle_unmatched,
        Some(5000)
    );
}

/// `quick_check` over its budget: `Incomplete`, and nothing else is read.
#[test]
fn an_integrity_check_over_budget_is_incomplete() {
    let fx = Fixture::messages("msgs-full-plain");
    let opts = CheckOptions::default().with_integrity_budget(0, 0);
    let (result, cleanup) = run_check(&fx, None, &opts);
    assert!(cleanup.removed);
    let r = result.expect("check");
    assert_eq!(
        r.status,
        oma_messages::ExtractionStatus::Incomplete(
            oma_messages::IncompleteReason::IntegrityCheckOverBudget
        )
    );
    assert_eq!(
        r.database.integrity,
        Some(oma_messages::IntegrityOutcome::OverBudget)
    );
    assert!(r.messages.is_none() && r.failures.is_none());
    assert!(r.database.plaintext_removed_after_row_pass);
}

/// M2's `plain` and `enc` fixtures have no Messages tables: unsupported,
/// with no error and staging removed (§19.3 `m2_fixtures.rs`, via check()).
#[test]
fn m2_fixtures_are_unsupported_through_check() {
    for (name, password) in [("plain", None), ("enc", Some(PASSWORD))] {
        let fx = Fixture::m2(name);
        let (result, cleanup) = run_check(&fx, password, &CheckOptions::default());
        assert!(cleanup.removed, "{name}");
        let r = result.unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let oma_messages::ExtractionStatus::Unsupported(reasons) = &r.status else {
            panic!("{name}: {:?}", r.status);
        };
        assert_eq!(reasons.len(), 5, "{name}: {reasons:?}");
        assert_eq!(r.schema.unknown_tables, 1, "{name}");
        assert!(r.handles.is_none(), "{name}");
        assert!(fx.staged_files().is_empty(), "{name}");
    }
}
