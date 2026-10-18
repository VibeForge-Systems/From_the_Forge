//! The `ExpectedReport` projection (M3 §19.3): `check()`'s report as the
//! JSON the messages generator writes in `<name>.expected/report.json`.
//!
//! Compared exactly: the status and its qualifiers (except `SourceChanged`),
//! every schema count, every row, link, orphan, association, reply,
//! edit-label, body, date-unit and attachment-outcome count, every failure
//! kind, the date range, the WAL facts, the integrity kind, the encoding and
//! the staged bytes. Compared by predicate, in `profiles.rs`: `elapsed`,
//! `open_mode`, `page_size`, `source_changed_during_check` and
//! `plaintext_removed_after_row_pass`.
#![allow(dead_code)]

use oma_backup::{FileUnavailable, TextEncoding, WalOutcome};
use oma_messages::{
    AttachmentCounts, AttributedState, ChatCounts, DamageReason, DateCounts, ExtractionReport,
    ExtractionStatus, FailureCounts, Features, HandleCounts, IncompleteReason, IntegrityOutcome,
    LinkCounts, MessageCounts, NotAttemptedReason, PathClass, Qualifier, RawHistogram,
    ReactionCounts, RecentlyDeletedCounts, SchemaReport, ServiceCounts, TextState, TsErrorKind,
    UnitHistogram, UnsupportedReason,
};
use serde_json::{json, Map, Value};

fn opt<T: Into<Value>>(v: Option<T>) -> Value {
    v.map_or(Value::Null, Into::into)
}

/// `CamelCase` to `snake_case`.
fn snake(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn status(s: &ExtractionStatus) -> Value {
    match s {
        ExtractionStatus::Readable { qualifiers } => {
            let q: Vec<Value> = qualifiers
                .iter()
                .filter(|q| **q != Qualifier::SourceChanged)
                .map(|q| {
                    Value::from(match q {
                        Qualifier::UnknownTables(n) => format!("unknown_tables:{n}"),
                        Qualifier::UnknownColumns(n) => format!("unknown_columns:{n}"),
                        Qualifier::WrongCaseColumns(n) => format!("wrong_case_columns:{n}"),
                        Qualifier::WalSkipped => "wal_skipped".into(),
                        Qualifier::WalStateUnknown => "wal_state_unknown".into(),
                        Qualifier::WalFramesInvalid => "wal_frames_invalid".into(),
                        Qualifier::AggregatesIncomplete => "aggregates_incomplete".into(),
                        Qualifier::ValuesWithheld => "values_withheld".into(),
                        Qualifier::NonUtf8Encoding => "non_utf8_encoding".into(),
                        Qualifier::SizeMismatch => "size_mismatch".into(),
                        other => format!("{other:?}"),
                    })
                })
                .collect();
            json!({"kind": "readable", "qualifiers": q})
        }
        ExtractionStatus::Unsupported(reasons) => {
            let r: Vec<Value> = reasons
                .iter()
                .map(|r| {
                    Value::from(match r {
                        UnsupportedReason::MissingTable(t) => format!("missing_table:{}", t.name()),
                        UnsupportedReason::NotOrdinaryTable(t) => {
                            format!("not_ordinary_table:{}", t.name())
                        }
                        UnsupportedReason::MissingRequiredColumn(c) => {
                            format!("missing_required_column:{c:?}")
                        }
                        UnsupportedReason::RowidShape(t) => format!("rowid_shape:{}", t.name()),
                        UnsupportedReason::VirtualTablePresent => "virtual_table_present".into(),
                        UnsupportedReason::SchemaExceedsLimits => "schema_exceeds_limits".into(),
                        other => format!("{other:?}"),
                    })
                })
                .collect();
            json!({"kind": "unsupported", "reasons": r})
        }
        ExtractionStatus::Damaged(d) => {
            let reason = match d {
                DamageReason::IntegrityProblems => "integrity_problems".to_owned(),
                DamageReason::RowsNotStreamed(t) => format!("rows_not_streamed:{}", t.name()),
                other => format!("{other:?}"),
            };
            json!({"kind": "damaged", "reason": reason})
        }
        ExtractionStatus::Incomplete(i) => {
            let reason = match i {
                IncompleteReason::IntegrityCheckOverBudget => "integrity_check_over_budget",
                _ => "other",
            };
            json!({"kind": "incomplete", "reason": reason})
        }
        other => json!({"kind": format!("{other:?}")}),
    }
}

fn features(f: &Features) -> Value {
    let mut v = Vec::new();
    for (name, on) in [
        ("attributed_body", f.attributed_body),
        ("tapbacks", f.tapbacks),
        ("emoji_tapbacks", f.emoji_tapbacks),
        ("replies", f.replies),
        ("edits_and_unsends", f.edits_and_unsends),
        ("recently_deleted", f.recently_deleted),
        ("scheduled", f.scheduled),
        ("filtering", f.filtering),
        ("attachments", f.attachments),
        ("genmoji", f.genmoji),
    ] {
        if on {
            v.push(Value::from(name));
        }
    }
    Value::Array(v)
}

fn encoding(e: Option<TextEncoding>) -> Value {
    match e {
        None => Value::Null,
        Some(TextEncoding::Utf8) => "utf8".into(),
        Some(TextEncoding::Utf16le) => "utf16le".into(),
        Some(TextEncoding::Utf16be) => "utf16be".into(),
        Some(other) => format!("{other:?}").into(),
    }
}

fn schema(s: &SchemaReport) -> Value {
    let tables: Vec<Value> = s
        .tables
        .iter()
        .map(|t| {
            json!({
                "table": t.table.name(),
                "present": t.present,
                "rows": opt(t.rows),
                "missing": t.missing_columns.iter().map(|c| c.name()).collect::<Vec<_>>(),
                "unknown": t.unknown_columns,
                "generated": t.generated_columns,
                "wrong_case": t.wrong_case_columns,
            })
        })
        .collect();
    json!({
        "tables": tables,
        "unknown_tables": s.unknown_tables,
        "views": s.views,
        "triggers": s.triggers,
        "indexes": s.indexes,
        "stat_tables": s.stat_tables,
        "internal_objects": s.internal_objects,
        "encoding": encoding(s.encoding),
        "client_version": opt(s.client_version),
        "client_version_parsed": s.client_version_parsed,
        "features": features(&s.features),
    })
}

fn file_unavailable(f: FileUnavailable) -> String {
    snake(&format!("{f:?}"))
}

fn database(r: &ExtractionReport) -> Value {
    let d = &r.database;
    let outcome = match d.wal.outcome {
        WalOutcome::NotListed => "not_listed".to_owned(),
        WalOutcome::Staged { .. } => "staged".to_owned(),
        WalOutcome::Skipped(f) => format!("skipped:{}", file_unavailable(f)),
        other => format!("{other:?}"),
    };
    let integrity = d.integrity.map(|i| match i {
        IntegrityOutcome::Ok => "ok",
        IntegrityOutcome::Problems => "problems",
        IntegrityOutcome::OverBudget => "over_budget",
        _ => "other",
    });
    json!({
        "wal_listed": d.wal.listed,
        "wal_outcome": outcome,
        "wal_bytes": opt(d.wal.bytes),
        "header_says_wal": opt(d.wal.header_says_wal),
        "wal_valid_committed_frames": opt(d.wal.facts.map(|f| f.valid_committed_frames)),
        "wal_frames_after_first_invalid": opt(d.wal.facts.map(|f| f.frames_after_first_invalid)),
        "integrity": opt(integrity),
        "staged_bytes": d.staged_bytes,
        "size_matches": opt(d.size_matches),
        "wal_size_matches": opt(d.wal.size_matches),
    })
}

fn service(s: &Option<ServiceCounts>) -> Value {
    match s {
        None => Value::Null,
        Some(s) => json!({
            "imessage": s.imessage, "sms": s.sms, "rcs": s.rcs,
            "imessage_lite": s.imessage_lite, "empty": s.empty, "other": s.other,
            "not_text": s.not_text,
        }),
    }
}

fn hist(h: &Option<RawHistogram>) -> Value {
    match h {
        None => Value::Null,
        Some(h) => json!({
            "entries": h.entries.iter().map(|(v, c)| json!([v, c])).collect::<Vec<_>>(),
            "other": h.other, "null": h.null, "unreadable": h.unreadable,
        }),
    }
}

fn units(h: &UnitHistogram) -> Value {
    json!({
        "zero": h.zero, "negative": h.negative, "seconds": h.seconds,
        "nanoseconds": h.nanoseconds, "ambiguous": h.ambiguous, "null": h.null,
        "unreadable": h.unreadable,
    })
}

fn units_opt(h: &Option<UnitHistogram>) -> Value {
    h.as_ref().map_or(Value::Null, units)
}

fn handles(h: &HandleCounts) -> Value {
    json!({"rows": h.rows, "by_service": service(&h.by_service)})
}

fn chats(c: &ChatCounts) -> Value {
    json!({
        "rows": c.rows,
        "by_service": service(&c.by_service),
        "with_no_other_handle": opt(c.with_no_other_handle),
        "with_one_other_handle": opt(c.with_one_other_handle),
        "with_several_other_handles": opt(c.with_several_other_handles),
        "archived": opt(c.archived),
        "is_filtered": hist(&c.is_filtered),
        "style": hist(&c.style),
        "with_properties": opt(c.with_properties),
    })
}

fn reactions(r: &ReactionCounts) -> Value {
    json!({
        "love": r.love, "like": r.like, "dislike": r.dislike, "laugh": r.laugh,
        "emphasize": r.emphasize, "question": r.question, "emoji": r.emoji,
        "sticker": r.sticker,
    })
}

fn text_state(t: TextState) -> &'static str {
    match t {
        TextState::Absent => "absent",
        TextState::Null => "null",
        TextState::Empty => "empty",
        TextState::NonEmpty => "non_empty",
        _ => "withheld",
    }
}

fn attributed_state(a: AttributedState) -> &'static str {
    match a {
        AttributedState::Absent => "absent",
        AttributedState::Null => "null",
        AttributedState::Present => "present",
        _ => "withheld",
    }
}

fn messages(m: &MessageCounts) -> Value {
    let associations = m.associations.as_ref().map_or(Value::Null, |a| {
        let t = &a.targets;
        json!({
            "sticker_legacy": a.sticker_legacy,
            "tapback_added": reactions(&a.tapback_added),
            "tapback_removed": reactions(&a.tapback_removed),
            "poll_vote": a.poll_vote,
            "no_association_type": a.no_association_type,
            "unknown_type": a.unknown_type,
            "type_not_recorded": a.type_not_recorded,
            "targets": {
                "part": t.part, "bubble": t.bubble, "bare": t.bare, "unparsed": t.unparsed,
                "null": t.null, "oversized": t.oversized, "unreadable": t.unreadable,
            },
            "target_not_found": opt(a.target_not_found),
        })
    });
    let item_kinds = m.item_kinds.as_ref().map_or(Value::Null, |k| {
        json!({
            "message": k.message, "participant_added": k.participant_added,
            "own_number_changed": k.own_number_changed,
            "participant_removed": k.participant_removed, "group_renamed": k.group_renamed,
            "participant_left": k.participant_left,
            "group_photo_changed": k.group_photo_changed,
            "group_photo_removed": k.group_photo_removed,
            "background_changed": k.background_changed,
            "background_removed": k.background_removed,
            "legacy_shared_location": k.legacy_shared_location, "audio_kept": k.audio_kept,
            "share_play": k.share_play, "unknown": k.unknown,
        })
    });
    let l = &m.edit_labels;
    let summaries = m.summaries.as_ref().map_or(Value::Null, |s| {
        json!({
            "parsed": s.parsed, "null": s.null, "unreadable": s.unreadable,
            "oversized": s.oversized, "tmp_key_present": s.tmp_key_present,
        })
    });
    let b = &m.bodies;
    let mut cross = Map::new();
    for (t, a, n) in &b.text_by_attributed {
        cross.insert(
            format!("{}/{}", text_state(*t), attributed_state(*a)),
            Value::from(*n),
        );
    }
    json!({
        "rows": m.rows,
        "with_association": opt(m.with_association),
        "group_actions": opt(m.group_actions),
        "recently_deleted": opt(m.recently_deleted),
        "other_rows": opt(m.other_rows),
        "from_me": m.from_me,
        "not_from_me": m.not_from_me,
        "direction_null": m.direction_null,
        "direction_unreadable": m.direction_unreadable,
        "not_from_me_handle_zero": opt(m.not_from_me_handle_zero),
        "handle_unmatched": opt(m.handle_unmatched),
        "other_handle_unmatched": opt(m.other_handle_unmatched),
        "by_service": service(&m.by_service),
        "item_kinds": item_kinds,
        "associations": associations,
        "replies": opt(m.replies),
        "reply_originator_not_found": opt(m.reply_originator_not_found),
        "reply_to_guid_set": opt(m.reply_to_guid_set),
        "edit_labels": {
            "not_recorded": l.not_recorded,
            "summary_unreadable": l.summary_unreadable,
            "unsent_parts_and_edit_history": l.unsent_parts_and_edit_history,
            "unsent_parts": l.unsent_parts,
            "edit_history": l.edit_history,
            "date_edited_no_body": l.date_edited_no_body,
            "date_edited_only": l.date_edited_only,
            "no_edit_recorded": l.no_edit_recorded,
            "undetermined": l.undetermined,
        },
        "ec_single_entry_parts": opt(m.ec_single_entry_parts),
        "summaries": summaries,
        "scheduled": opt(m.scheduled),
        "filter_action": hist(&m.filter_action),
        "payload_data_present": opt(m.payload_data_present),
        "cache_has_attachments_set": opt(m.cache_has_attachments_set),
        "date_retracted_set": opt(m.date_retracted_set),
        "bodies": {
            "text_by_attributed": Value::Object(cross),
            "decoded_ok": b.decoded_ok,
            "decoded_degraded": b.decoded_degraded,
            "decode_failed": b.decode_failed,
            "not_decoded": b.not_decoded,
            "text_equals_decoded": b.text_equals_decoded,
            "text_differs_from_decoded": b.text_differs_from_decoded,
            "more_than_one_part": b.more_than_one_part,
            "with_mention": b.with_mention,
            "with_link_key": b.with_link_key,
            "with_audio_transcription": b.with_audio_transcription,
        },
    })
}

fn dates(d: &DateCounts) -> Value {
    json!({
        "message_date": units(&d.message_date),
        "chat_message_date": units_opt(&d.chat_message_date),
        "chat_last_read": units_opt(&d.chat_last_read),
        "recoverable_delete_date": units_opt(&d.recoverable_delete_date),
        "edit_entry_date": units_opt(&d.edit_entry_date),
        "earliest_message": opt(d.earliest_message),
        "latest_message": opt(d.latest_message),
        "excluded_scheduled": opt(d.excluded_scheduled),
        "excluded_not_classified": d.excluded_not_classified,
        "excluded_before_floor": d.excluded_before_floor,
        "excluded_after_checked_at": d.excluded_after_checked_at,
    })
}

fn path_class(c: PathClass) -> String {
    snake(&format!("{c:?}"))
}

fn not_attempted(r: NotAttemptedReason) -> String {
    snake(&format!("{r:?}"))
}

fn attachments(a: &AttachmentCounts) -> Value {
    let mut by_class = Map::new();
    for (c, n) in &a.by_class {
        by_class.insert(path_class(*c), Value::from(*n));
    }
    let mut na = Map::new();
    for (r, n) in &a.lookup.not_attempted {
        na.insert(not_attempted(*r), Value::from(*n));
    }
    let p = &a.probes;
    json!({
        "rows": opt(a.rows),
        "linked": opt(a.linked),
        "not_linked": opt(a.not_linked),
        "filename_null": opt(a.filename_null),
        "filename_too_long": opt(a.filename_too_long),
        "by_class": Value::Object(by_class),
        "lookup": {
            "found": a.lookup.found,
            "not_listed_at_sourced_key": a.lookup.not_listed_at_sourced_key,
            "not_found_at_assumed_key": a.lookup.not_found_at_assumed_key,
            "ambiguous": a.lookup.ambiguous,
            "invalid_entry": a.lookup.invalid_entry,
            "not_attempted": Value::Object(na),
        },
        "probes": {
            "present": p.present, "missing": p.missing, "not_regular": p.not_regular,
            "unreadable": p.unreadable, "not_a_file": p.not_a_file,
            "invalid_entry": p.invalid_entry, "no_key": p.no_key,
            "no_protection_class": p.no_protection_class,
            "class_key_unavailable": p.class_key_unavailable,
            "key_does_not_unwrap": p.key_does_not_unwrap, "other": p.other,
            "not_probed": p.not_probed,
        },
        "declared_total_bytes": opt(a.declared_total_bytes),
        "present_blob_bytes": a.present_blob_bytes,
        "stickers": opt(a.stickers),
        "hidden": opt(a.hidden),
        "transfer_state": hist(&a.transfer_state),
        "with_genmoji_description": opt(a.with_genmoji_description),
    })
}

fn links(l: &LinkCounts) -> Value {
    json!({
        "chat_message_rows": l.chat_message_rows,
        "chat_handle_rows": l.chat_handle_rows,
        "message_attachment_rows": opt(l.message_attachment_rows),
        "recoverable_rows": opt(l.recoverable_rows),
        "orphan_chat_message_chat": opt(l.orphan_chat_message_chat),
        "orphan_chat_message_message": opt(l.orphan_chat_message_message),
        "orphan_chat_handle_chat": opt(l.orphan_chat_handle_chat),
        "orphan_chat_handle_handle": opt(l.orphan_chat_handle_handle),
        "orphan_message_attachment_message": opt(l.orphan_message_attachment_message),
        "orphan_message_attachment_attachment": opt(l.orphan_message_attachment_attachment),
        "orphan_recoverable_chat": opt(l.orphan_recoverable_chat),
        "orphan_recoverable_message": opt(l.orphan_recoverable_message),
        "messages_in_no_chat": opt(l.messages_in_no_chat),
        "messages_in_no_chat_recently_deleted": opt(l.messages_in_no_chat_recently_deleted),
        "messages_in_several_chats": opt(l.messages_in_several_chats),
    })
}

fn recently_deleted(r: &RecentlyDeletedCounts) -> Value {
    json!({
        "chat_recoverable_message_join": opt(r.chat_recoverable_message_join),
        "recoverable_message_part": opt(r.recoverable_message_part),
        "deleted_messages": opt(r.deleted_messages),
        "sync_deleted_messages": opt(r.sync_deleted_messages),
        "sync_deleted_chats": opt(r.sync_deleted_chats),
        "sync_deleted_attachments": opt(r.sync_deleted_attachments),
        "unsynced_removed_recoverable_messages": opt(r.unsynced_removed_recoverable_messages),
    })
}

fn failures(f: &FailureCounts) -> Value {
    let mut ts = Map::new();
    for k in TsErrorKind::ALL {
        ts.insert(k.name().to_owned(), Value::from(f.typedstream_count(k)));
    }
    let v = &f.values;
    let mut by_col = Map::new();
    for (c, n) in &v.oversized_by_column {
        by_col.insert(format!("{c:?}"), Value::from(*n));
    }
    json!({
        "typedstream": Value::Object(ts),
        "summary_too_large": f.summary_too_large,
        "summary_plist_invalid": f.summary_plist_invalid,
        "summary_plist_budget": f.summary_plist_budget,
        "summary_not_dictionary": f.summary_not_dictionary,
        "summary_shape": f.summary_shape,
        "summary_unknown_keys": f.summary_unknown_keys,
        "values": {
            "real_in_integer_column": v.real_in_integer_column,
            "invalid_utf8_text": v.invalid_utf8_text,
            "blob_in_text_column": v.blob_in_text_column,
            "text_in_blob_column": v.text_in_blob_column,
            "type_mismatch": v.type_mismatch,
            "bool_out_of_range": v.bool_out_of_range,
            "plist_invalid": v.plist_invalid,
            "plist_over_budget": v.plist_over_budget,
            "plist_not_dictionary": v.plist_not_dictionary,
            "value_oversized": v.value_oversized,
            "oversized_by_column": Value::Object(by_col),
        },
        "date_ambiguous": f.date_ambiguous,
        "date_negative": f.date_negative,
        "assoc_guid_unparsed": f.assoc_guid_unparsed,
        "unknown_attribute_key": f.unknown_attribute_key,
        "unknown_attribute_class": f.unknown_attribute_class,
        "unreadable_attribute_value": f.unreadable_attribute_value,
        "orphan_join_row": f.orphan_join_row,
        "attachment_ambiguous": f.attachment_ambiguous,
        "attachment_invalid_entry": f.attachment_invalid_entry,
        "attachment_unusual_name": f.attachment_unusual_name,
        "attachment_blob_missing": f.attachment_blob_missing,
        "attachment_blob_unreadable": f.attachment_blob_unreadable,
        "attachment_key_problem": f.attachment_key_problem,
        "aggregate_over_budget": f.aggregate_over_budget,
    })
}

/// The projection compared with `report.json`.
pub fn project(r: &ExtractionReport) -> Value {
    json!({
        "status": status(&r.status),
        "schema": schema(&r.schema),
        "database": database(r),
        "handles": r.handles.as_ref().map_or(Value::Null, handles),
        "chats": r.chats.as_ref().map_or(Value::Null, chats),
        "messages": r.messages.as_ref().map_or(Value::Null, messages),
        "dates": r.dates.as_ref().map_or(Value::Null, dates),
        "attachments": r.attachments.as_ref().map_or(Value::Null, attachments),
        "links": r.links.as_ref().map_or(Value::Null, links),
        "recently_deleted": r.recently_deleted.as_ref().map_or(Value::Null, recently_deleted),
        "failures": r.failures.as_ref().map_or(Value::Null, failures),
    })
}

/// Every leaf where `got` and `want` differ, as `path: got != want`.
pub fn diff(path: &str, got: &Value, want: &Value, out: &mut Vec<String>) {
    match (got, want) {
        (Value::Object(g), Value::Object(w)) => {
            let mut keys: Vec<&String> = g.keys().chain(w.keys()).collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                diff(
                    &format!("{path}.{k}"),
                    g.get(k).unwrap_or(&Value::Null),
                    w.get(k).unwrap_or(&Value::Null),
                    out,
                );
            }
        }
        (Value::Array(g), Value::Array(w)) if g.len() == w.len() => {
            for (i, (a, b)) in g.iter().zip(w).enumerate() {
                diff(&format!("{path}[{i}]"), a, b, out);
            }
        }
        _ if got != want => out.push(format!("{path}: got {got} != want {want}")),
        _ => {}
    }
}
