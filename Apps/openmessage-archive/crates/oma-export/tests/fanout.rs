//! The writer trait and the fan-out over real records (M6 spec §4.3,
//! §14.2 "Manifest: the counts equal the writer reports"; slice 6e1).
//!
//! The view catalogue's main entry is imported and mapped whole; its
//! records drive two writers (TXT and CSV stand-ins that write one line
//! per record) through the fan-out, into a staged bundle that is then
//! given its provenance and published.
#![allow(clippy::disallowed_methods)]

mod common;
mod files;

use std::collections::BTreeMap;

use common::*;
use files::*;
use oma_archive::{CancelToken, Reveal};
use oma_export::manifest::write_provenance;
use oma_export::map::ExportZone;
use oma_export::plan::{ExportOptions, ExportPlan, Format, ScopePlan};
use oma_export::record::ItemRecord;
use oma_export::stage::{Replace, Staging};
use oma_export::writer::{AboutDraft, ConversationFile, ExportWriter, FanOut};
use oma_export::{ExportError, IdentifierMode, LimitKind, TargetName};

fn walked() -> Walked {
    let doc = docs()
        .into_iter()
        .find(|d| d.name == "view-main-plain")
        .expect("entry");
    let i = import(&doc);
    with_snapshot(&i, |s| {
        walk_archive(
            s,
            Reveal::default(),
            options(false, IdentifierMode::Ordinals),
        )
    })
}

fn two_format_plan() -> ExportPlan {
    let formats = [Format::Txt, Format::Csv];
    let scope = ScopePlan::Archive;
    let mut o = ExportOptions::defaults(&scope, &formats);
    o.attachments = oma_export::plan::AttachmentMode::None;
    ExportPlan::new(scope, formats.to_vec(), o).expect("plan")
}

fn writers() -> Vec<Box<dyn ExportWriter>> {
    vec![
        Box::new(LinesWriter::new(Format::Txt)),
        Box::new(LinesWriter::new(Format::Csv)),
    ]
}

/// The items and folded rows of each conversation file, in walk order.
fn files_of(w: &Walked) -> Vec<(Option<u64>, Vec<&ItemRecord>)> {
    let mut order: Vec<Option<u64>> = Vec::new();
    let mut by: BTreeMap<Option<u64>, Vec<&ItemRecord>> = BTreeMap::new();
    for it in w.items.iter().chain(&w.folded) {
        let c = it.context.conversation;
        if !order.contains(&c) {
            order.push(c);
        }
        by.entry(c).or_default().push(it);
    }
    order
        .into_iter()
        .map(|c| (c, by.remove(&c).unwrap_or_default()))
        .collect()
}

#[test]
fn every_writer_gets_every_record_and_the_reports_agree() {
    let w = walked();
    assert!(
        !w.items.is_empty() && !w.folded.is_empty(),
        "the entry has folded reactions"
    );
    let plan = two_format_plan();
    let zone = ExportZone::Utc;
    let d = dest();
    let mut staging = Staging::bundle(&d.parent).expect("staging");
    let mut fan = FanOut::new(writers(), &plan).expect("fan-out");
    let cancel = CancelToken::new();
    fan.begin(&ctx(&plan, &zone), &mut staging).expect("begin");
    let groups = files_of(&w);
    for (conv, items) in &groups {
        let file = match conv {
            Some(n) => ConversationFile::Conversation(
                w.conversations.iter().find(|c| c.n == *n).expect("record"),
            ),
            None => ConversationFile::NoConversation,
        };
        fan.begin_conversation(file, &mut staging)
            .expect("conversation");
        for it in items {
            fan.item(it, &mut staging, &cancel).expect("item");
        }
        let t = fan.end_conversation(&mut staging).expect("end");
        assert_eq!(t.items + t.folded_rows, items.len() as u64);
    }
    let totals = fan.totals();
    assert_eq!(totals.items, w.items.len() as u64);
    assert_eq!(totals.folded_rows, w.folded.len() as u64);
    assert_eq!(totals.conversation_files, groups.len() as u64);
    let body = body(&plan, w.conversations.len() as u64);
    let reports = fan
        .finish(&AboutDraft { body: &body }, &mut staging)
        .expect("finish");
    assert_eq!(
        reports.iter().map(|r| r.format).collect::<Vec<_>>(),
        [Format::Txt, Format::Csv]
    );
    write_provenance(&mut staging, &plan, &body).expect("provenance");
    staging
        .publish(&TargetName::new("Fan").expect("name"), Replace::Never)
        .expect("publish");
    let bundle = d.parent.join("Fan");
    let sums = verify_sums(&bundle);
    // Both formats wrote the same lines for the same records.
    let txt: Vec<&String> = sums.keys().filter(|k| k.starts_with("txt/")).collect();
    assert_eq!(txt.len(), groups.len());
    for t in txt {
        let csv = t.replacen("txt/", "csv/", 1).replace(".txt", ".csv");
        assert_eq!(sums.get(t), sums.get(&csv), "{t}");
    }
}

#[test]
fn the_order_is_enforced_and_a_cancel_stops_before_the_next_record() {
    let w = walked();
    let it = &w.items[0];
    let plan = two_format_plan();
    let zone = ExportZone::Utc;
    let d = dest();
    let mut staging = Staging::bundle(&d.parent).expect("staging");
    let cancel = CancelToken::new();
    // The writers must be the plan's formats, exactly.
    let one: Vec<Box<dyn ExportWriter>> = vec![Box::new(LinesWriter::new(Format::Txt))];
    assert!(matches!(
        FanOut::new(one, &plan),
        Err(ExportError::Limit(LimitKind::RowOrder))
    ));
    let mut fan = FanOut::new(writers(), &plan).expect("fan-out");
    let order = |r: Result<(), ExportError>| {
        assert!(
            matches!(r, Err(ExportError::Limit(LimitKind::RowOrder))),
            "{r:?}"
        );
    };
    order(fan.begin_conversation(ConversationFile::NoConversation, &mut staging));
    fan.begin(&ctx(&plan, &zone), &mut staging).expect("begin");
    order(fan.begin(&ctx(&plan, &zone), &mut staging));
    order(fan.item(it, &mut staging, &cancel));
    order(fan.end_conversation(&mut staging).map(|_| ()));
    fan.begin_conversation(ConversationFile::NoConversation, &mut staging)
        .expect("conversation");
    order(fan.begin_conversation(ConversationFile::NoConversation, &mut staging));
    fan.item(it, &mut staging, &cancel).expect("item");
    cancel.cancel();
    assert!(matches!(
        fan.item(it, &mut staging, &cancel),
        Err(ExportError::Cancelled)
    ));
    assert_eq!(fan.totals().items + fan.totals().folded_rows, 1);
    let body = body(&plan, 0);
    assert!(matches!(
        fan.finish(&AboutDraft { body: &body }, &mut staging),
        Err(ExportError::Limit(LimitKind::RowOrder))
    ));
    assert!(staging.remove());
}

#[test]
fn a_writer_whose_report_disagrees_or_leaves_a_file_open_is_refused() {
    let w = walked();
    let it = w
        .items
        .iter()
        .find(|i| i.context.folded_into.is_none())
        .expect("item");
    let plan = plan(false);
    let zone = ExportZone::Utc;
    for (lie, leak, want) in [
        (true, false, LimitKind::RowCountMismatch),
        (false, true, LimitKind::FileStillOpen),
    ] {
        let d = dest();
        let mut staging = Staging::bundle(&d.parent).expect("staging");
        let mut wr = LinesWriter::new(Format::Txt);
        wr.lie = lie;
        wr.leak = leak;
        let mut fan = FanOut::new(vec![Box::new(wr)], &plan).expect("fan-out");
        fan.begin(&ctx(&plan, &zone), &mut staging).expect("begin");
        fan.begin_conversation(ConversationFile::NoConversation, &mut staging)
            .expect("conversation");
        fan.item(it, &mut staging, &CancelToken::new())
            .expect("item");
        fan.end_conversation(&mut staging).expect("end");
        let body = body(&plan, 0);
        let r = fan.finish(&AboutDraft { body: &body }, &mut staging);
        assert!(
            matches!(&r, Err(ExportError::Limit(k)) if *k == want),
            "{r:?}"
        );
        drop(staging);
        // The leaked handle stays open: Windows keeps a folder whose file
        // is open, so the tree is checked off Windows only.
        if !(leak && cfg!(windows)) {
            assert!(tree(&d.parent).is_empty());
        }
    }
}
