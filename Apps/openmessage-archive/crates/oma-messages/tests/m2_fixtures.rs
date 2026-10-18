//! M2's synthetic `plain` and `enc` backups carry an sms.db stand-in with no
//! Messages tables (M3 §19.3 `m2_fixtures.rs`). The check must call that
//! unsupported, with no error, and leave no staged file behind.
#![allow(clippy::disallowed_types, clippy::disallowed_methods)]
mod common;

use common::*;
use oma_messages::{KnownTable, ProbeOptions, UnsupportedReason};

fn check(name: &str, password: Option<&str>) {
    let fx = Fixture::m2(name);
    let (kind, unknown) = with_probe(&fx, password, &ProbeOptions::default(), |p, _| {
        match p.expect("probe") {
            oma_messages::Probe::Unsupported(u) => (u.reasons, u.capabilities.unknown_tables),
            other => panic!("{name}: {other:?}"),
        }
    })
    .expect("connected");
    let missing: Vec<UnsupportedReason> = KnownTable::ALL
        .iter()
        .filter(|t| t.required())
        .map(|t| UnsupportedReason::MissingTable(*t))
        .collect();
    assert_eq!(kind, missing, "{name}");
    assert_eq!(unknown, 1, "{name}: the stand-in's marker table");
    assert!(fx.staged_files().is_empty(), "{name}");
}

#[test]
fn plain_is_unsupported() {
    check("plain", None);
}

#[test]
fn enc_is_unsupported() {
    check("enc", Some(PASSWORD));
}
