//! The "events" option off (M6 spec §4.2, §9.2; slice 6k2), end to end
//! through `pipeline::run`: no catalogue holds a group or system event,
//! so the test makes some (`item_type` 2, `item_kind` 5, a group renamed)
//! in its own copy of an imported archive, then exports the whole archive
//! with events included and left out.
//!
//! - Included: every event is an item, and `eventsNotIncluded` is 0.
//! - Left out: no event is written, the manifest's `timelineRows` falls
//!   by exactly `eventsNotIncluded`, the item ordinals still run from 1
//!   without gaps, and the counting identities hold (the pipeline checks
//!   its rows against the pre-count, which leaves out the same rows).
#![allow(clippy::disallowed_methods)]

mod common;
mod files;

use std::collections::BTreeSet;
use std::path::Path;

use common::*;
use oma_export::map::ExportZone;
use oma_export::plan::{AttachmentMode, Format, ScopePlan};
use serde_json::Value;

/// JSON items of a bundle's conversation files: (n, item token).
fn items(bundle: &Path) -> Vec<(u64, String)> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(bundle.join("json"))
        .expect("json")
        .flatten()
    {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".jsonl") {
            continue;
        }
        for line in std::fs::read_to_string(e.path()).expect("read").lines() {
            let v: Value = serde_json::from_str(line).expect("line");
            if v["type"] == "item" {
                out.push((
                    v["n"].as_u64().expect("n"),
                    v["item"]["token"].as_str().expect("token").to_owned(),
                ));
            }
        }
    }
    out
}

fn counts(bundle: &Path) -> Value {
    json(&bundle.join("manifest.json"))["counts"].clone()
}

#[test]
fn events_left_out_are_counted_and_leave_no_gap() {
    let doc = docs()
        .into_iter()
        .find(|d| d.name == "view-main-plain")
        .expect("view-main-plain");
    let i = import(&doc);
    let events = {
        let c = rusqlite::Connection::open(i.db()).expect("raw open");
        c.execute(
            "UPDATE message SET item_type = 2, item_kind = 5 \
             WHERE assoc_present = 0 AND item_kind = 1 AND id % 3 = 0",
            [],
        )
        .expect("events")
    };
    assert!(events > 3, "{events} events");
    let d = files::dest();
    let export = |events: bool, target: &str| {
        export_formats(
            &i,
            &d.parent,
            ScopePlan::Archive,
            &[Format::Json],
            &|o| {
                o.events = events;
                o.attachments = AttachmentMode::None;
            },
            target,
            None,
            &ExportZone::Utc,
        )
        .expect("export")
    };
    let on = export(true, "with events");
    let off = export(false, "without events");
    let (b_on, b_off) = (
        d.parent.join("with events"),
        d.parent.join("without events"),
    );
    let (c_on, c_off) = (counts(&b_on), counts(&b_off));
    assert_eq!(c_on["eventsNotIncluded"], 0);
    let left_out = c_off["eventsNotIncluded"].as_u64().expect("count");
    assert!(left_out >= events as u64, "{left_out} < {events}");
    assert_eq!(
        c_on["timelineRows"].as_u64().expect("rows"),
        c_off["timelineRows"].as_u64().expect("rows") + left_out
    );
    assert_eq!(on.counts.events_not_included, 0);
    assert_eq!(off.counts.events_not_included, left_out);
    // Written events with them, none without.
    let (i_on, i_off) = (items(&b_on), items(&b_off));
    assert!(i_on.iter().filter(|(_, t)| t == "groupRenamed").count() >= events);
    assert!(i_off.iter().all(|(_, t)| t != "groupRenamed"));
    // Ordinals from 1 without gaps (messages numbered by the scope index).
    let ns: BTreeSet<u64> = i_off.iter().map(|(n, _)| *n).collect();
    let distinct = c_off["distinctMessages"].as_u64().expect("distinct");
    assert_eq!(ns, (1..=distinct).collect::<BTreeSet<u64>>());
    // The options say so.
    let m = json(&b_off.join("manifest.json"));
    assert_eq!(m["options"]["events"], false);
    files::verify_sums(&b_off);
}
