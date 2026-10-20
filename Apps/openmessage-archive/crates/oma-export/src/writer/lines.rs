//! An item's lines in words, shared by the TXT and PDF writers (M6 spec
//! §5.4, §5.6; slice 6j2).
//!
//! TXT (slice 6e2) and PDF say the same things about an item: its body
//! (segments, the transcription, alternative and earlier text), its
//! details (subject, account, event, reaction target, reply), its
//! attachments, reactions, value history and stored versions. Slice 6j2
//! moved those sentences out of `txt.rs` into the functions here, generic
//! over an [`Emit`]ter, so the two formats cannot drift apart: the TXT
//! goldens did not move.
//!
//! **Recorded values never mix with our words as plain strings.** A line
//! is a list of [`Part`]s: `Part::Ours` for our own words (`words.rs`
//! sentences, numbers, codes) and `Part::Value` for a recorded value that
//! the emitter has already marked with its header policy
//! ([`Emit::value`]: TXT also escapes `[` and `]`; PDF builds a
//! `pdf::doc::PdfText` with `TextBuilder::header`). Recorded text that may
//! hold line breaks goes through [`Emit::text`] as [`Run`]s, which each
//! emitter splits and marks with its body policy.

use crate::codes::{AttachmentAccessCode, CardStateCode, ChipSenderCode, SegmentKindCode};
use crate::error::ExportError;
use crate::ids::Ordinal;
use crate::record::*;

use super::{is, phrase, placeholder, Words};

/// A piece of a recorded text: text to mark, or a placeholder of ours.
pub(crate) enum Run<'a> {
    Text(&'a str),
    Placeholder(String),
}

/// One piece of a line: our words, or a recorded value already marked.
pub(crate) enum Part<V> {
    Ours(String),
    Value(V),
}

/// `Part::Ours`.
pub(crate) fn ours<V>(s: impl Into<String>) -> Part<V> {
    Part::Ours(s.into())
}

/// Where a writer puts an item's lines.
pub(crate) trait Emit {
    /// A recorded value, marked for this format.
    type V: Clone;
    fn words(&self) -> &Words;
    /// Mark a recorded header value (an address, a name, a tag).
    fn value(&mut self, raw: &str) -> Self::V;
    /// The marked address of handle `h`, when it is known.
    fn handle_value(&self, h: Ordinal) -> Option<Self::V>;
    /// A label line (TXT: four spaces).
    fn label(&mut self, parts: Vec<Part<Self::V>>) -> Result<(), ExportError>;
    /// A line under a label (TXT: six spaces).
    fn content(&mut self, parts: Vec<Part<Self::V>>) -> Result<(), ExportError>;
    /// A label line, then recorded text split at its line breaks.
    fn text(&mut self, label: Vec<Part<Self::V>>, runs: &[Run<'_>]) -> Result<(), ExportError>;
    /// After an attachment's lines (PDF: its preview). Default: nothing.
    fn after_attachment(&mut self, a: &AttachmentRecord) -> Result<(), ExportError> {
        let _ = a;
        Ok(())
    }
}

fn say<E: Emit>(e: &E, table: &'static str, c: crate::codes::Code) -> String {
    e.words().say(table, c)
}

/// The marked value of `v`, or `None` when nothing is recorded.
pub(crate) fn value_of<E: Emit>(e: &mut E, v: &TextValue) -> Option<E::V> {
    v.text.as_ref().map(|t| e.value(t.as_str()))
}

/// The marked value of `v`, or the words of why it is missing.
fn value_or_missing<E: Emit>(e: &mut E, v: &TextValue) -> Part<E::V> {
    match value_of(e, v) {
        Some(x) => Part::Value(x),
        None => Part::Ours(
            v.missing
                .map(|m| say(e, "valueState", m))
                .unwrap_or_default(),
        ),
    }
}

fn pieces<E: Emit>(
    e: &mut E,
    label: Vec<Part<E::V>>,
    ps: &[PieceRecord],
) -> Result<(), ExportError> {
    let owned: Vec<(bool, String)> = ps
        .iter()
        .map(|p| match &p.text {
            Some(t) if is(p.kind, SegmentKindCode::Text) => (true, t.as_str().to_owned()),
            _ => (false, placeholder(phrase("oneTimeCodeMarker"))),
        })
        .collect();
    let runs: Vec<Run<'_>> = owned
        .iter()
        .map(|(text, s)| {
            if *text {
                Run::Text(s)
            } else {
                Run::Placeholder(s.clone())
            }
        })
        .collect();
    e.text(label, &runs)
}

/// The body: the text and the transcription, the alternative text and
/// the earlier text.
pub(crate) fn body<E: Emit>(e: &mut E, it: &ItemRecord) -> Result<(), ExportError> {
    match &it.body {
        BodyRecord::Text {
            segments, lossy, ..
        } => {
            let mut main: Vec<Run<'_>> = Vec::new();
            let mut spoken: Vec<Run<'_>> = Vec::new();
            for seg in segments {
                let k = seg.kind;
                let run = if is(k, SegmentKindCode::Text) {
                    match &seg.text {
                        Some(t) => Run::Text(t.as_str()),
                        None => continue,
                    }
                } else if is(k, SegmentKindCode::Withheld) {
                    Run::Placeholder(placeholder(phrase("oneTimeCodeMarker")))
                } else if is(k, SegmentKindCode::Attachment) {
                    match seg.attachment {
                        Some(n) => Run::Placeholder(placeholder(&format!("Attachment {n}"))),
                        None => Run::Placeholder(placeholder(&say(e, "segmentKind", k))),
                    }
                } else {
                    Run::Placeholder(placeholder(&say(e, "segmentKind", k)))
                };
                if seg.transcription {
                    spoken.push(run);
                } else {
                    main.push(run);
                }
            }
            if !main.is_empty() {
                e.text(vec![ours("Text:")], &main)?;
            }
            if !spoken.is_empty() {
                e.text(vec![ours(format!("{}:", phrase("transcription")))], &spoken)?;
            }
            if *lossy {
                e.label(vec![ours(
                    "Some bytes of this text were not valid text and show as U+FFFD",
                )])?;
            }
        }
        BodyRecord::NoText { reason } => {
            let s = say(e, "noTextReason", *reason);
            e.label(vec![ours(s)])?;
        }
    }
    if it.body_facts.sources_differ == Some(true) {
        e.label(vec![ours(phrase("sourcesDiffer"))])?;
    }
    match &it.alternative_text {
        AltTextRecord::None => {}
        AltTextRecord::Text { source, text } => {
            let label = format!("{}:", say(e, "altTextSource", *source));
            match &text.text {
                Some(t) => {
                    let t = t.as_str().to_owned();
                    e.text(vec![ours(label)], &[Run::Text(&t)])?;
                }
                None => e.label(vec![ours(label)])?,
            }
        }
        AltTextRecord::Withheld { why, .. } => {
            let s = say(e, "altTextWithheld", *why);
            e.label(vec![ours(s)])?;
        }
    }
    match &it.earlier_text {
        EarlierRecord::None | EarlierRecord::Available => {}
        EarlierRecord::Withheld { reason } => {
            let s = say(e, "withheldReason", *reason);
            e.label(vec![ours(s)])?;
        }
        EarlierRecord::Revealed {
            reason, pieces: p, ..
        } => {
            let label = format!("{}:", say(e, "revealedReason", *reason));
            pieces(e, vec![ours(label)], p)?;
        }
    }
    Ok(())
}

/// `a, b, c` as parts.
fn comma_parts<V>(items: Vec<Part<V>>) -> Vec<Part<V>> {
    let mut out = Vec::with_capacity(items.len() * 2);
    for (i, p) in items.into_iter().enumerate() {
        if i > 0 {
            out.push(Part::Ours(", ".to_owned()));
        }
        out.push(p);
    }
    out
}

/// Subject, account, event, reaction target, reply and reply counts.
pub(crate) fn details<E: Emit>(e: &mut E, it: &ItemRecord) -> Result<(), ExportError> {
    if let Some(s) = value_of(e, &it.subject) {
        e.label(vec![ours("Subject: "), Part::Value(s)])?;
    }
    if let Some(s) = value_of(e, &it.account) {
        e.label(vec![
            ours(format!("{}: ", phrase("account"))),
            Part::Value(s),
        ])?;
    }
    if let Some(ev) = &it.event {
        let what = say(e, "itemKind", ev.kind);
        e.label(vec![ours(format!("Event: {what}"))])?;
        if !ev.handles.is_empty() {
            let list: Vec<Part<E::V>> = ev
                .handles
                .iter()
                .map(|h| match e.handle_value(*h) {
                    Some(v) => Part::Value(v),
                    None => Part::Ours(h.to_string()),
                })
                .collect();
            let mut line = vec![ours("Addresses recorded with it: ")];
            line.extend(comma_parts(list));
            e.label(line)?;
        }
        if let Some(t) = ev.title.as_ref().and_then(|t| value_of(e, t)) {
            e.label(vec![ours("Name recorded with it: "), Part::Value(t)])?;
        }
    }
    if let Some(a) = &it.association {
        let mut line = vec![ours("Reaction")];
        if let Some(k) = a.kind {
            line.push(ours(format!(": {}", say(e, "assocKind", k))));
        }
        if let Some(v) = a.emoji.as_ref().and_then(|x| value_of(e, x)) {
            line.push(ours(" "));
            line.push(Part::Value(v));
        }
        match a.target {
            Some(MessageRefRecord::InExport { n }) => line.push(ours(format!(", on item {n}"))),
            Some(MessageRefRecord::NotInExport) => {
                line.push(ours(format!(", on a message {}", phrase("notInExport"))))
            }
            None => {}
        }
        line.push(ours(format!(
            " ({})",
            say(e, "assocVisibility", a.visibility)
        )));
        e.label(line)?;
    }
    if let Some(r) = &it.reply {
        let s = match r.originator {
            Some(MessageRefRecord::InExport { n }) => {
                let elsewhere = it.context.reply_in_conversation == Some(false);
                if elsewhere {
                    format!("Reply to item {n}, in another conversation of this export")
                } else {
                    format!("Reply to item {n}")
                }
            }
            Some(MessageRefRecord::NotInExport) => {
                format!("Reply to a message {}", phrase("notInExport"))
            }
            None => say(e, "replyResolution", r.resolution),
        };
        e.label(vec![ours(s)])?;
    }
    if let Determinable::Determined { value } = &it.reply_count {
        if value.in_export > 0 || value.elsewhere > 0 {
            e.label(vec![ours(format!(
                "Replies: {} in this export, {} {}",
                value.in_export,
                value.elsewhere,
                phrase("notInExport")
            ))])?;
        }
    }
    Ok(())
}

/// Each attachment: its name, outcome, kinds, access, card state,
/// description and value history.
pub(crate) fn attachments<E: Emit>(e: &mut E, it: &ItemRecord) -> Result<(), ExportError> {
    for a in &it.attachments {
        let name = match &a.name.text {
            Some(n) if n.as_str().is_empty() => ours("an empty name recorded"),
            Some(n) => Part::Value(e.value(n.as_str())),
            None => {
                let why = a
                    .name
                    .missing
                    .map(|m| say(e, "valueState", m))
                    .unwrap_or_default();
                ours(format!("no name recorded ({why})"))
            }
        };
        e.label(vec![ours(format!("Attachment {}: ", a.n)), name])?;
        let mut outcome = vec![ours(say(e, "attachmentOutcome", a.file.outcome))];
        if let Some(p) = &a.file.path {
            outcome.push(ours(": "));
            outcome.push(Part::Value(e.value(p.as_str())));
        }
        e.content(outcome)?;
        if let Some(k) = a.file.sniffed {
            let s = say(e, "sniffedKind", k);
            e.content(vec![ours(format!("Content: {s}"))])?;
        }
        if let Some(t) = value_of(e, &a.mime_type) {
            e.content(vec![ours("Declared type: "), Part::Value(t)])?;
        }
        if !is(a.access, AttachmentAccessCode::Included) {
            let s = say(e, "attachmentAccess", a.access);
            e.content(vec![ours(s)])?;
        }
        if !is(a.card.state, CardStateCode::File) {
            let mut s = say(e, "cardState", a.card.state);
            if let (Some(l), Some(c)) = (&a.card.unavailable_lookup, &a.card.unavailable_copy) {
                s = s
                    .replace("{lookup}", l.as_str())
                    .replace("{copy}", c.as_str());
            }
            e.content(vec![ours(s)])?;
        }
        if let Some(d) = value_of(e, &a.emoji_description) {
            e.content(vec![ours("Description recorded with it: "), Part::Value(d)])?;
        }
        for h in &a.history {
            value_history(e, h, true)?;
        }
        e.after_attachment(a)?;
    }
    Ok(())
}

/// The derived reaction chips, or how many were not included.
pub(crate) fn reactions<E: Emit>(e: &mut E, it: &ItemRecord) -> Result<(), ExportError> {
    match &it.reactions {
        Included::NotIncluded { counts, .. } => {
            if counts.rows > 0 {
                e.label(vec![ours(format!(
                    "Reactions not included in this export: {} recorded",
                    counts.rows
                ))])?;
            }
        }
        Included::Included { value } => {
            for c in &value.chips {
                let mut line = vec![ours(if c.includes_rows_outside_scope {
                    format!("Reaction ({}): ", phrase("chipsOutsideScope"))
                } else {
                    format!("Reaction ({}): ", phrase("derived"))
                })];
                line.push(ours(say(e, "reactionKind", c.kind)));
                if let Some(v) = c.emoji.as_ref().and_then(|x| value_of(e, x)) {
                    line.push(ours(" "));
                    line.push(Part::Value(v));
                }
                line.push(ours(" by "));
                let handle = if is(c.sender.kind, ChipSenderCode::Handle) {
                    c.sender.handle.and_then(|h| e.handle_value(h))
                } else {
                    None
                };
                line.push(match handle {
                    Some(v) => Part::Value(v),
                    None => ours(say(e, "chipSender", c.sender.kind)),
                });
                line.push(ours(format!(", {}", say(e, "chipState", c.state))));
                e.label(line)?;
            }
        }
    }
    Ok(())
}

/// One other recorded value of a column (`nested`: under an attachment).
pub(crate) fn value_history<E: Emit>(
    e: &mut E,
    h: &ValueHistoryRecord,
    nested: bool,
) -> Result<(), ExportError> {
    let line = match h {
        ValueHistoryRecord::Value { column, value, .. } => {
            let col = say(e, "historyColumn", *column);
            vec![
                ours(format!("Other recorded value of {col}: ")),
                value_or_missing(e, value),
            ]
        }
        ValueHistoryRecord::Withheld { column, reason } => {
            let col = say(e, "historyColumn", *column);
            vec![ours(format!(
                "Other recorded value of {col}: {}",
                say(e, "withheldReason", *reason)
            ))]
        }
    };
    if nested {
        e.content(line)
    } else {
        e.label(line)
    }
}

/// How many stored versions were not included, and the value history.
pub(crate) fn history<E: Emit>(e: &mut E, it: &ItemRecord) -> Result<(), ExportError> {
    if let Included::NotIncluded { counts, .. } = &it.history {
        if counts.captured_versions > 1 || counts.phone_entries > 0 {
            e.label(vec![ours(format!(
                "Stored versions not included in this export: {} captured, {} phone edit entries",
                counts.captured_versions, counts.phone_entries
            ))])?;
        }
    }
    for h in &it.value_history {
        value_history(e, h, false)?;
    }
    Ok(())
}

/// The other recorded value of a conversation's name.
pub(crate) fn name_history_value<E: Emit>(e: &mut E, h: &ValueHistoryRecord) -> Part<E::V> {
    match h {
        ValueHistoryRecord::Value { value, .. } => value_or_missing(e, value),
        ValueHistoryRecord::Withheld { reason, .. } => ours(say(e, "withheldReason", *reason)),
    }
}

/// One stored body version and its phone edit entries.
pub(crate) fn version<E: Emit>(e: &mut E, v: &VersionRecord) -> Result<(), ExportError> {
    let label = format!(
        "Stored version{}, kept by imports {} to {}:",
        if v.current { " (current)" } else { "" },
        v.first_session,
        v.last_session
    );
    match &v.text {
        VersionTextRecord::Text { pieces: p } => pieces(e, vec![ours(label)], p)?,
        VersionTextRecord::Withheld { reason } => {
            let s = say(e, "withheldReason", *reason);
            e.label(vec![ours(format!("{label} {s}"))])?;
        }
        VersionTextRecord::NoText { reason, .. } => {
            let s = say(e, "noTextReason", *reason);
            e.label(vec![ours(format!("{label} {s}"))])?;
        }
    }
    for x in &v.edits {
        let at = x.at.raw.as_ref().map_or("not recorded", |r| r.as_str());
        let label = format!(
            "Phone edit entry, part {}, {} {at}:",
            x.part.as_str(),
            phrase("phoneEditTime")
        );
        match &x.text {
            EditEntryTextRecord::Text { pieces: p } => pieces(e, vec![ours(label)], p)?,
            EditEntryTextRecord::Withheld { reason } => {
                let s = say(e, "withheldReason", *reason);
                e.label(vec![ours(format!("{label} {s}"))])?;
            }
            EditEntryTextRecord::NoBody => e.label(vec![ours(format!(
                "{label} No text recorded for this version"
            ))])?,
            EditEntryTextRecord::Failed { .. } => e.label(vec![ours(format!(
                "{label} Edit text could not be decoded"
            ))])?,
            EditEntryTextRecord::NotMatched => e.label(vec![ours(format!(
                "{label} Edit text could not be matched"
            ))])?,
        }
    }
    Ok(())
}
