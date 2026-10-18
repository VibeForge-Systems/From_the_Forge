//! Scan, integrity and probe edge cases on databases built in the test
//! (M3 §6.2, §19.1 "schema"). Every name and value is synthetic.
#![allow(clippy::disallowed_types, clippy::disallowed_methods)]
mod common;

use common::*;
use oma_messages::{
    DamageReason, IncompleteReason, KnownColumn, KnownTable, ProbeOptions, TextEncoding,
    UnsupportedReason,
};
use rusqlite::Connection;

/// The minimal schema with one required table's DDL replaced.
fn minimal_with(t: KnownTable, ddl: &str) -> String {
    KnownTable::ALL
        .iter()
        .filter(|k| k.required())
        .map(|k| {
            if *k == t {
                format!("{ddl};\n")
            } else {
                create_table(*k, true)
            }
        })
        .collect()
}

fn column(t: KnownTable, name: &str) -> KnownColumn {
    t.known_columns()
        .find(|c| c.name() == name)
        .expect("known column")
}

fn table_caps(caps: &oma_messages::Capabilities, t: KnownTable) -> oma_messages::TableCaps {
    caps.table(t).expect("table entry").clone()
}

#[test]
fn every_known_table_and_column() {
    let (kind, caps) = probe_db(&build(&full_schema()));
    assert_eq!(kind, Kind::Supported);
    let caps = caps.expect("caps");
    assert_eq!(caps.tables.len(), KnownTable::ALL.len());
    for t in KnownTable::ALL {
        let tc = table_caps(&caps, t);
        assert!(tc.present, "{t:?}");
        let known = u32::try_from(t.known_columns().count()).expect("small");
        assert_eq!(tc.present_columns.len(), known, "{t:?}");
        assert_eq!(
            (
                tc.unknown_columns,
                tc.generated_columns,
                tc.wrong_case_columns
            ),
            (0, 0, 0),
            "{t:?}"
        );
    }
    assert_eq!(
        table_caps(&caps, KnownTable::RecoverableMessagePart).rows,
        Some(1)
    );
    assert_eq!(table_caps(&caps, KnownTable::Message).rows, Some(0));
    assert_eq!(caps.client_version, Some(17001));
    assert!(caps.client_version_parsed);
    assert_eq!(caps.encoding, Some(TextEncoding::Utf8));
    let f = caps.features;
    assert!(
        f.attributed_body
            && f.tapbacks
            && f.emoji_tapbacks
            && f.replies
            && f.edits_and_unsends
            && f.recently_deleted
            && f.scheduled
            && f.filtering
            && f.attachments
            && f.genmoji
    );
    // AUTOINCREMENT creates sqlite_sequence: internal, not unknown.
    assert_eq!(caps.unknown_tables, 0);
    assert_eq!(caps.internal_objects, 1);
}

#[test]
fn the_required_minimum_alone() {
    let (kind, caps) = probe_db(&build(&minimal_schema()));
    assert_eq!(kind, Kind::Supported);
    let caps = caps.expect("caps");
    assert_eq!(caps.features, oma_messages::Features::default());
    assert!(!table_caps(&caps, KnownTable::Attachment).present);
    assert_eq!(table_caps(&caps, KnownTable::Attachment).rows, None);
    assert_eq!(caps.client_version, None);
    assert!(!caps.client_version_parsed);
    assert!(caps.has_column(column(KnownTable::Message, "guid")));
    assert!(!caps.has_column(column(KnownTable::Message, "text")));
}

#[test]
fn missing_required_tables_are_listed_in_order() {
    let ddl: String = [KnownTable::Handle, KnownTable::ChatHandleJoin]
        .iter()
        .map(|t| create_table(*t, true))
        .collect();
    let (kind, _) = probe_db(&build(&ddl));
    assert_eq!(
        kind,
        Kind::Unsupported(vec![
            UnsupportedReason::MissingTable(KnownTable::Message),
            UnsupportedReason::MissingTable(KnownTable::Chat),
            UnsupportedReason::MissingTable(KnownTable::ChatMessageJoin),
        ])
    );
}

#[test]
fn scrambled_column_order_reads_by_name() {
    let ddl = minimal_with(
        KnownTable::Handle,
        "CREATE TABLE handle (service TEXT, extra_synthetic INTEGER, id TEXT, ROWID INTEGER PRIMARY KEY, country TEXT)",
    );
    let mut sql = ddl;
    sql.push_str("INSERT INTO handle (ROWID, id, service, country) VALUES (7, 'synthetic-handle-1@example.invalid', 'SMS', 'zz');");
    let fx = Fixture::with_sms(&build(&sql));
    let seen = with_probe(&fx, None, &ProbeOptions::default(), |p, ctx| {
        let oma_messages::Probe::Supported(db) = p.expect("probe") else {
            panic!("not supported");
        };
        let tc = db
            .capabilities()
            .table(KnownTable::Handle)
            .expect("handle")
            .clone();
        assert_eq!(tc.unknown_columns, 1);
        let mut rows = Vec::new();
        db.for_each_handle::<()>(oma_messages::Cursor::Start, ctx, |h| {
            rows.push((
                h.rowid,
                h.id.value().map(|v| v.expose().as_bytes().to_vec()),
                h.service.value().map(|v| v.expose().as_bytes().to_vec()),
                h.service_class,
                h.country.value().map(|v| v.expose().as_bytes().to_vec()),
                h.uncanonicalized_id.is_recorded(),
            ));
            std::ops::ControlFlow::Continue(())
        })
        .expect("pass");
        rows
    })
    .expect("connected");
    assert_eq!(
        seen,
        vec![(
            7,
            Some(b"synthetic-handle-1@example.invalid".to_vec()),
            Some(b"SMS".to_vec()),
            Some(oma_messages::ServiceClass::Sms),
            Some(b"zz".to_vec()),
            false
        )]
    );
}

#[test]
fn rowid_shapes() {
    let cases = [
        // (DDL for handle, expected)
        (
            "CREATE TABLE handle (ROWID integer primary key, id TEXT)",
            None,
        ),
        (
            "CREATE TABLE handle (ROWID INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT)",
            None,
        ),
        (
            "CREATE TABLE handle (ROWID TEXT PRIMARY KEY, id TEXT)",
            Some(()),
        ),
        ("CREATE TABLE handle (ROWID INTEGER, id TEXT)", Some(())),
        (
            "CREATE TABLE handle (ROWID INTEGER PRIMARY KEY DESC, id TEXT)",
            Some(()),
        ),
        (
            "CREATE TABLE handle (ROWID INTEGER, id TEXT, PRIMARY KEY (ROWID, id))",
            Some(()),
        ),
        (
            "CREATE TABLE handle (ROWID INT PRIMARY KEY, id TEXT)",
            Some(()),
        ),
        (
            "CREATE TABLE handle (ROWID INTEGER PRIMARY KEY, id TEXT) WITHOUT ROWID",
            Some(()),
        ),
    ];
    for (ddl, bad) in cases {
        let (kind, _) = probe_db(&build(&minimal_with(KnownTable::Handle, ddl)));
        let want = match bad {
            None => Kind::Supported,
            Some(()) => Kind::Unsupported(vec![UnsupportedReason::RowidShape(KnownTable::Handle)]),
        };
        assert_eq!(kind, want, "{ddl}");
    }
}

#[test]
fn join_table_shapes() {
    for (ddl, ok) in [
        ("CREATE TABLE chat_handle_join (chat_id INTEGER, handle_id INTEGER, UNIQUE(chat_id, handle_id))", true),
        ("CREATE TABLE chat_handle_join (chat_id INTEGER, handle_id INTEGER, PRIMARY KEY (chat_id, handle_id)) WITHOUT ROWID", false),
        ("CREATE TABLE chat_handle_join (chat_id INTEGER, handle_id INTEGER, rowid INTEGER)", false),
        ("CREATE TABLE chat_handle_join (chat_id INTEGER, handle_id INTEGER, \"OID\" TEXT)", false),
        ("CREATE TABLE chat_handle_join (chat_id INTEGER, handle_id INTEGER, _rowid_ TEXT)", false),
    ] {
        let (kind, _) = probe_db(&build(&minimal_with(KnownTable::ChatHandleJoin, ddl)));
        let want = if ok {
            Kind::Supported
        } else {
            Kind::Unsupported(vec![UnsupportedReason::RowidShape(KnownTable::ChatHandleJoin)])
        };
        assert_eq!(kind, want, "{ddl}");
    }
}

#[test]
fn a_view_or_index_in_a_known_tables_place() {
    let mut sql = minimal_with(KnownTable::Message, "CREATE TABLE message_real (ROWID INTEGER PRIMARY KEY, guid TEXT, date INTEGER, is_from_me INTEGER)");
    sql.push_str("CREATE VIEW message AS SELECT * FROM message_real;");
    let (kind, caps) = probe_db(&build(&sql));
    assert_eq!(
        kind,
        Kind::Unsupported(vec![UnsupportedReason::NotOrdinaryTable(
            KnownTable::Message
        )])
    );
    let caps = caps.expect("caps");
    assert_eq!((caps.views, caps.unknown_tables), (1, 1));

    // An optional table's name used by an index.
    let mut sql = minimal_schema();
    sql.push_str("CREATE INDEX attachment ON handle(id);");
    let (kind, caps) = probe_db(&build(&sql));
    assert_eq!(
        kind,
        Kind::Unsupported(vec![UnsupportedReason::NotOrdinaryTable(
            KnownTable::Attachment
        )])
    );
    assert_eq!(caps.expect("caps").indexes, 1);
}

#[test]
fn virtual_tables_are_refused_before_anything_runs() {
    let mut sql = full_schema();
    sql.push_str("CREATE VIRTUAL TABLE synthetic_search USING fts5(body);");
    let (kind, caps) = probe_db(&build(&sql));
    assert_eq!(
        kind,
        Kind::Unsupported(vec![UnsupportedReason::VirtualTablePresent])
    );
    let caps = caps.expect("caps");
    // Only the scan ran: no table entries, no probe.
    assert!(caps.tables.is_empty());
    assert_eq!(caps.encoding, Some(TextEncoding::Utf8));
    // fts5's shadow tables are ordinary tables outside the known list.
    assert!(caps.unknown_tables >= 4, "{}", caps.unknown_tables);

    // A virtual table whose module does not exist: if any of its code ran
    // (xConnect), SQLite would fail with "no such module". The scan only
    // reads sqlite_schema, so the result is the same refusal, not an error.
    let main = build_with(|c: &Connection| {
        c.execute_batch(&full_schema()).expect("schema");
        c.execute_batch("CREATE VIRTUAL TABLE message_search USING fts5(body);")
            .expect("vtab");
        c.execute_batch(
            "PRAGMA writable_schema = ON;
             UPDATE sqlite_schema SET sql = 'CREATE VIRTUAL TABLE message_search USING synthetic_nosuchmodule(body)'
               WHERE name = 'message_search';
             PRAGMA writable_schema = OFF;",
        )
        .expect("rewrite");
    });
    let (kind, _) = probe_db(&main);
    assert_eq!(
        kind,
        Kind::Unsupported(vec![UnsupportedReason::VirtualTablePresent])
    );
}

/// A virtual table disguised in `sqlite_schema` (D44): the stored SQL hides
/// `VIRTUAL` behind a comment or padding, the rootpage is non-zero, the name
/// is a known join table or a reserved `sqlite_` name. SQLite's loader
/// tokenises the SQL and ignores the rootpage, so each is a working virtual
/// table. Each must be refused before `quick_check` and before any read of
/// the table; the module does not exist, so any `xConnect` would fail with
/// "no such module" instead of this refusal.
#[test]
fn disguised_virtual_tables_are_refused_before_anything_runs() {
    let padded = format!(
        "CREATE{}VIRTUAL TABLE {{name}} USING synthetic_nosuchmodule(a)",
        " ".repeat(40)
    );
    let cases: [(&str, String); 5] = [
        (
            "message_search",
            "CREATE/**/VIRTUAL TABLE {name} USING synthetic_nosuchmodule(a)".to_owned(),
        ),
        ("message_search", padded),
        (
            "message_search",
            "CREATE -- synthetic\nVIRTUAL TABLE {name} USING synthetic_nosuchmodule(a)".to_owned(),
        ),
        (
            "chat_message_join",
            "CREATE/**/VIRTUAL TABLE {name} USING synthetic_nosuchmodule(chat_id, message_id)"
                .to_owned(),
        ),
        (
            "sqlite_synthetic",
            "CREATE VIRTUAL TABLE {name} USING synthetic_nosuchmodule(a)".to_owned(),
        ),
    ];
    for (name, template) in cases {
        let disguised = template.replace("{name}", name);
        let main = build_with(|c: &Connection| {
            let schema: String = full_schema()
                .split_inclusive(';')
                .filter(|stmt| {
                    name != "chat_message_join" || !stmt.contains("\"chat_message_join\" (")
                })
                .collect();
            c.execute_batch(&schema).expect("schema");
            let placeholder = if name.starts_with("sqlite_") {
                "synthetic_placeholder"
            } else {
                name
            };
            c.execute_batch(&format!(
                "CREATE VIRTUAL TABLE {placeholder} USING fts5(a);"
            ))
            .expect("vtab");
            c.execute_batch("PRAGMA writable_schema = ON;")
                .expect("writable");
            c.execute(
                "UPDATE sqlite_schema SET name = ?1, tbl_name = ?1, sql = ?2, rootpage = 2 \
                 WHERE name = ?3",
                rusqlite::params![name, disguised, placeholder],
            )
            .expect("rewrite");
            c.execute_batch("PRAGMA writable_schema = OFF;")
                .expect("not writable");
        });
        let (kind, caps) = probe_db(&main);
        assert_eq!(
            kind,
            Kind::Unsupported(vec![UnsupportedReason::VirtualTablePresent]),
            "{name}: {disguised:?}"
        );
        assert!(caps.expect("caps").tables.is_empty(), "{name}");
    }
}

#[test]
fn generated_and_wrong_case_columns_are_absent() {
    let ddl = minimal_with(
        KnownTable::Handle,
        "CREATE TABLE handle (ROWID INTEGER PRIMARY KEY, id TEXT, Service TEXT, country TEXT GENERATED ALWAYS AS ('zz') VIRTUAL, person_centric_id TEXT GENERATED ALWAYS AS (id) STORED)",
    );
    let (kind, caps) = probe_db(&build(&ddl));
    assert_eq!(kind, Kind::Supported);
    let caps = caps.expect("caps");
    let tc = table_caps(&caps, KnownTable::Handle);
    assert_eq!(tc.generated_columns, 2);
    assert_eq!(tc.wrong_case_columns, 1);
    assert_eq!(tc.unknown_columns, 0);
    for name in ["service", "country", "person_centric_id"] {
        assert!(!caps.has_column(column(KnownTable::Handle, name)), "{name}");
    }

    // A required column whose case is wrong makes the database unsupported,
    // although SQLite's own lookup would match it.
    let ddl = minimal_with(
        KnownTable::Message,
        "CREATE TABLE message (ROWID INTEGER PRIMARY KEY, GUID TEXT, date INTEGER, is_from_me INTEGER)",
    );
    let (kind, _) = probe_db(&build(&ddl));
    assert_eq!(
        kind,
        Kind::Unsupported(vec![UnsupportedReason::MissingRequiredColumn(column(
            KnownTable::Message,
            "guid"
        ))])
    );
}

#[test]
fn names_from_the_database_never_reach_debug_output() {
    const SENTINEL: &str = "SYNTH-NAME-SENTINEL";
    let mut sql = minimal_with(
        KnownTable::Handle,
        &format!(
            "CREATE TABLE handle (ROWID INTEGER PRIMARY KEY, id TEXT, \"{SENTINEL}-col\" TEXT)"
        ),
    );
    sql.push_str(&format!(
        "CREATE TABLE \"{SENTINEL}-table\" (a);
         CREATE INDEX \"{SENTINEL}-index\" ON handle(id);
         CREATE VIEW \"{SENTINEL}-view\" AS SELECT 1;
         CREATE TRIGGER \"{SENTINEL}-trigger\" AFTER INSERT ON handle BEGIN SELECT 1; END;"
    ));
    let fx = Fixture::with_sms(&build(&sql));
    let shown = with_probe(&fx, None, &ProbeOptions::default(), |p, _| format!("{p:?}"))
        .expect("connected");
    assert!(!shown.contains("SYNTH"), "{shown}");
    let (kind, caps) = probe_db(&build(&sql));
    assert_eq!(kind, Kind::Supported);
    let caps = caps.expect("caps");
    assert_eq!(
        (caps.unknown_tables, caps.indexes, caps.views, caps.triggers),
        (1, 1, 1, 1)
    );
    assert_eq!(table_caps(&caps, KnownTable::Handle).unknown_columns, 1);
}

#[test]
fn schema_sql_over_the_limit_is_schema_exceeds_limits() {
    let cols: Vec<String> = (0..700)
        .map(|i| format!("synthetic_wide_column_{i:04} TEXT"))
        .collect();
    let mut sql = minimal_schema();
    sql.push_str(&format!(
        "CREATE TABLE synthetic_wide ({});",
        cols.join(", ")
    ));
    assert!(sql.len() > 16 * 1024);
    let (kind, _) = probe_db(&build(&sql));
    assert_eq!(kind, Kind::SchemaExceedsLimits);
}

#[test]
fn a_trigger_over_the_function_argument_limit() {
    // SMS_DB_LIMITS allows 8 function arguments, and the limit also applies
    // when trigger SQL is parsed with the schema.
    let mut sql = minimal_schema();
    sql.push_str(
        "CREATE TRIGGER synthetic_t AFTER INSERT ON handle BEGIN SELECT char(1,2,3,4,5,6,7,8,9); END;",
    );
    let (kind, _) = probe_db(&build(&sql));
    assert_eq!(kind, Kind::SchemaExceedsLimits);

    let mut sql = minimal_schema();
    sql.push_str(
        "CREATE TRIGGER synthetic_t AFTER INSERT ON handle BEGIN SELECT char(1,2,3,4,5,6,7,8); END;",
    );
    let (kind, caps) = probe_db(&build(&sql));
    assert_eq!(kind, Kind::Supported);
    assert_eq!(caps.expect("caps").triggers, 1);
}

#[test]
fn stat_tables_and_indexes_are_counted() {
    let mut sql = minimal_schema();
    sql.push_str(
        "CREATE TABLE synthetic_u (a TEXT UNIQUE);
         CREATE INDEX synthetic_i ON handle(id);
         INSERT INTO handle (id) VALUES ('synthetic-handle-1@example.invalid');
         ANALYZE;",
    );
    let (kind, caps) = probe_db(&build(&sql));
    assert_eq!(kind, Kind::Supported);
    let caps = caps.expect("caps");
    assert!(caps.stat_tables >= 1, "{}", caps.stat_tables);
    // The UNIQUE constraint's sqlite_autoindex_* is not counted.
    assert_eq!(caps.indexes, 1);
    assert_eq!(caps.unknown_tables, 1);
}

#[test]
fn a_utf16_database() {
    let main = build_with(|c: &Connection| {
        c.execute_batch("PRAGMA encoding = 'UTF-16le';")
            .expect("encoding");
        c.execute_batch(&minimal_schema()).expect("schema");
        c.execute(
            "INSERT INTO handle (id) VALUES (?1)",
            ["synthetic-hándle@example.invalid"],
        )
        .expect("insert");
    });
    let fx = Fixture::with_sms(&main);
    let (enc, ids) = with_probe(&fx, None, &ProbeOptions::default(), |p, ctx| {
        let oma_messages::Probe::Supported(db) = p.expect("probe") else {
            panic!("not supported");
        };
        let mut ids = Vec::new();
        db.for_each_handle::<()>(oma_messages::Cursor::Start, ctx, |h| {
            ids.push(h.id.value().map(|v| v.expose().as_bytes().to_vec()));
            std::ops::ControlFlow::Continue(())
        })
        .expect("pass");
        (db.capabilities().encoding, ids)
    })
    .expect("connected");
    assert_eq!(enc, Some(TextEncoding::Utf16le));
    // SQLite converts to UTF-8 on the way out.
    assert_eq!(
        ids,
        vec![Some("synthetic-hándle@example.invalid".as_bytes().to_vec())]
    );
}

#[test]
fn client_version_is_strict_decimal() {
    for (value, want) in [
        ("'9005'", Some(9005)),
        ("'9005 '", None),
        ("'x'", None),
        ("9005", None), // stored as an integer, not as text
    ] {
        let mut sql = minimal_schema();
        sql.push_str(&format!(
            "CREATE TABLE _SqliteDatabaseProperties (key TEXT, value);
             INSERT INTO _SqliteDatabaseProperties VALUES ('_ClientVersion', {value});"
        ));
        let (kind, caps) = probe_db(&build(&sql));
        assert_eq!(kind, Kind::Supported);
        let caps = caps.expect("caps");
        assert_eq!(caps.client_version, want, "{value}");
        assert_eq!(caps.client_version_parsed, want.is_some());
    }
    // Two rows for the key: ambiguous.
    let mut sql = minimal_schema();
    sql.push_str(
        "CREATE TABLE _SqliteDatabaseProperties (key TEXT, value TEXT);
         INSERT INTO _SqliteDatabaseProperties VALUES ('_ClientVersion', '1'), ('_ClientVersion', '2');",
    );
    let (_, caps) = probe_db(&build(&sql));
    assert_eq!(caps.expect("caps").client_version, None);
}

/// A database with enough pages for `quick_check` to need more than one
/// progress-handler tick.
fn several_pages() -> String {
    let mut sql = minimal_schema();
    sql.push_str(
        "CREATE INDEX synthetic_handle_id ON handle(id);
         WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 3000)
         INSERT INTO handle (id) SELECT 'synthetic-handle-' || i || '@example.invalid' FROM n;",
    );
    sql
}

#[test]
fn integrity_damage_and_budget() {
    // Over budget: the smallest budget is one progress-handler tick.
    let main = build(&several_pages());
    let (kind, _) = probe_db_with(&main, &ProbeOptions::default().with_integrity_budget(0, 0));
    assert_eq!(
        kind,
        Kind::Incomplete(IncompleteReason::IntegrityCheckOverBudget)
    );
    let (kind, _) = probe_db(&main);
    assert_eq!(kind, Kind::Supported);

    // Damage: overwrite the cell area of the last page (a leaf of the
    // handle table or its index; the schema page is untouched).
    let mut bad = main.clone();
    let page = 4096;
    assert_eq!(bad.len() % page, 0);
    let start = bad.len() - page / 2;
    for b in &mut bad[start..] {
        *b = 0xA5;
    }
    let (kind, _) = probe_db(&bad);
    assert_eq!(kind, Kind::Damaged(DamageReason::IntegrityProblems));
}
