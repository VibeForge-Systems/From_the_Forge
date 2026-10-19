//! What the human-readable writers (TXT, HTML) share: the item's tags in
//! words, its time, the "About this export" lines and a joiner.
//!
//! Every function here returns **unescaped** words and values: each
//! writer escapes them for its own grammar (TXT's header fields, HTML's
//! text nodes). Moved here from `txt.rs` in slice 6g without a change of
//! wording, so the TXT goldens did not move.

use oma_archive::schema::codes::EditLabelCode;

use crate::codes::{Code, Direction, MessageLabelCode, SenderReasonCode};
use crate::record::{Decimal, ItemRecord, RecordedByRecord, TimeRecord};
use crate::time::{self, format_human, CivilDate, HumanStyle, Localized};

use super::{is, phrase, Words};

/// `a, b, c` (strings are joined here without `.join`, which the source
/// rules keep for path building in `naming.rs`).
pub(crate) trait Comma {
    fn comma(&self) -> String;
}

impl Comma for [String] {
    fn comma(&self) -> String {
        let mut out = String::new();
        for (i, s) in self.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(s);
        }
        out
    }
}

/// An item's time as the human formats show it, and its local date (UTC
/// for an instant outside the zone window). `None` without a usable time.
pub(crate) fn time_text(d: &TimeRecord, style: HumanStyle) -> Option<(String, CivilDate)> {
    let ns = d.utc_ns?;
    let offset = if d.outside_zone_window {
        None
    } else {
        d.offset_seconds
    };
    let at = match offset {
        Some(o) => Localized::Local { offset_seconds: o },
        None => Localized::OutsideWindow,
    };
    Some((format_human(ns, offset, style), time::local_date(ns, at)))
}

/// `YYYY-MM-DD` of a local date (the day lines and the HTML day ids).
pub(crate) fn date_text(day: CivilDate) -> String {
    format!(
        "{}-{:02}-{:02}",
        time::year_text(day.year),
        day.month,
        day.day
    )
}

/// Is `it` one the archive sorts as undated (its `undated` label)?
pub(crate) fn is_undated(it: &ItemRecord) -> bool {
    it.labels
        .iter()
        .any(|l| is(l.kind, MessageLabelCode::Undated))
}

/// The direction and sender tags of an item, and whether its sender is a
/// resolved handle (then the writer shows that handle's address, or the
/// sender reason's words when the handle is not known).
pub(crate) struct SenderTags {
    pub resolved: bool,
    pub tags: Vec<String>,
}

pub(crate) fn sender_tags(w: &Words, it: &ItemRecord) -> SenderTags {
    let mut tags = Vec::new();
    let mut resolved = false;
    let d = &it.direction;
    if is(d.token, Direction::FromMe) {
        tags.push(w.say("direction", d.token));
    } else {
        resolved = is(it.sender.reason, SenderReasonCode::Resolved);
        if is(d.token, Direction::NotRecorded) {
            tags.push(w.say("direction", d.token));
        } else if is(d.token, Direction::UnknownValue) {
            let raw = d.raw.as_ref().map_or("", |r| r.as_str());
            tags.push(w.say("direction", d.token).replace("{raw}", raw));
        } else if !resolved {
            tags.push(w.say("direction", d.token));
        }
        if !resolved
            && (!is(it.sender.reason, SenderReasonCode::DirectionNotRecorded)
                || !is(d.token, Direction::NotRecorded))
        {
            tags.push(w.say("senderReason", it.sender.reason));
        }
    }
    if let RecordedByRecord::Lineage { lineage, .. } = d.recorded_by {
        tags.push(phrase("directionRecordedBy").replace("{lineage}", &lineage.to_string()));
    }
    SenderTags { resolved, tags }
}

/// The item's other tags, in the TXT order: time states, the zone window,
/// planned sends, order ties, the service, the item kind, edits and
/// unsends, the edit label, message labels, Recently Deleted, and the
/// other conversations it is also in.
pub(crate) fn item_tags(w: &Words, it: &ItemRecord, tags: &mut Vec<String>) {
    let date = &it.date;
    if date.utc_ns.is_none() {
        if let Some(s) = date.state {
            tags.push(w.say("timeState", s));
        } else if let Some(u) = date.unit {
            let raw = date.raw.as_ref().map_or("", |r| r.as_str());
            tags.push(w.say("timeUnit", u).replace("{raw}", raw));
        }
    }
    let strip = |p: &str| p.trim_start_matches('[').trim_end_matches(']').to_owned();
    if date.outside_zone_window {
        tags.push(strip(phrase("outsideZoneWindow")));
    }
    if date.planned {
        tags.push(strip(phrase("plannedSendTime")));
    }
    if it.context.order_tie {
        tags.push(strip(phrase("orderTie")));
    }
    if let Some(s) = it.service {
        tags.push(w.say("serviceClass", s));
    }
    if it.item.token != "message" {
        tags.push(w.say("itemKind", it.item));
    }
    let e = &it.edit;
    let parts = |v: &[Decimal]| {
        v.iter()
            .map(|d| d.as_str().to_owned())
            .collect::<Vec<_>>()
            .as_slice()
            .comma()
    };
    if !e.edited_parts.is_empty() {
        let n = e.edited_parts.len();
        tags.push(format!(
            "Edited: {} {}",
            if n == 1 { "part" } else { "parts" },
            parts(&e.edited_parts)
        ));
    }
    if e.unsent_whole {
        tags.push("Unsent by the sender".to_owned());
    } else if !e.unsent_parts.is_empty() {
        let n = e.unsent_parts.len();
        let of = e.part_count.map(|c| format!(" of {c}")).unwrap_or_default();
        tags.push(format!(
            "Unsent by the sender: {} {}{of}",
            if n == 1 { "part" } else { "parts" },
            parts(&e.unsent_parts)
        ));
    }
    if let Some(l) = e.label {
        let quiet = [
            EditLabelCode::NotRecorded,
            EditLabelCode::NoEditRecorded,
            EditLabelCode::UnsentParts,
        ]
        .into_iter()
        .any(|q| is(l, q));
        let edited = [
            EditLabelCode::EditHistory,
            EditLabelCode::UnsentPartsAndEditHistory,
        ]
        .into_iter()
        .any(|q| is(l, q));
        if edited {
            if e.edited_parts.is_empty() {
                tags.push(w.say("editLabel", Code::of(EditLabelCode::EditHistory)));
            }
        } else if !quiet {
            tags.push(w.say("editLabel", l));
        }
    }
    for l in &it.labels {
        if is(l.kind, MessageLabelCode::Undated) {
            continue;
        }
        let mut s = w.say("messageLabel", l.kind);
        if let Some(v) = &l.value {
            s = s.replace("{value}", v.as_str());
        }
        tags.push(s);
    }
    if !it.recently_deleted.is_empty() {
        tags.push(phrase("recentlyDeleted").to_owned());
    }
    let a = &it.context.also_in;
    if !a.in_export.is_empty() || a.elsewhere > 0 {
        let mut s = String::from("Also in");
        if !a.in_export.is_empty() {
            let list: Vec<String> = a.in_export.iter().map(|n| n.to_string()).collect();
            s.push_str(&format!(" conversation {}", list.as_slice().comma()));
        }
        if a.elsewhere > 0 {
            if !a.in_export.is_empty() {
                s.push(';');
            }
            s.push_str(&format!(
                " {} {} not in this export",
                a.elsewhere,
                if a.elsewhere == 1 {
                    "conversation"
                } else {
                    "conversations"
                }
            ));
        }
        tags.push(s);
    }
}

/// `key: value` lines of a JSON value, nested keys joined with `.`; a long
/// list (the zone's transitions) is summarised by its length.
pub(crate) fn flatten(prefix: &str, v: &serde_json::Value, out: &mut Vec<String>) {
    use serde_json::Value as V;
    match v {
        V::Object(m) => {
            for (k, x) in m {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten(&key, x, out);
            }
        }
        V::Array(a) if a.len() > 16 => out.push(format!("{prefix}: {} entries", a.len())),
        V::Array(a) if a.iter().all(|x| !x.is_object() && !x.is_array()) => {
            let list: Vec<String> = a.iter().map(scalar).collect();
            out.push(format!("{prefix}: {}", list.as_slice().comma()));
        }
        V::Array(a) => {
            for (i, x) in a.iter().enumerate() {
                flatten(&format!("{prefix}.{}", i + 1), x, out);
            }
        }
        other => out.push(format!("{prefix}: {}", scalar(other))),
    }
}

fn scalar(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => "none".to_owned(),
        other => other.to_string(),
    }
}
