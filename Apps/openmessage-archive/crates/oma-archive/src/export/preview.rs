//! The archive generation (M6 spec §3.4, §3.7, D147).
//!
//! A draft records the generation its preview saw; `start_export`
//! compares it and refuses a changed one. Only an import writes archive
//! rows, and every import works under a session row, so the generation is
//! the newest session (its id, state and resume count), the newest
//! completed session, and the highest row id of each table an import adds
//! content to: messages, body versions, attachments and stored files.
//! Rows are never deleted (D57), so every insert moves a highest id, and
//! each is one seek on the table's primary key. The value is opaque: it
//! is only compared.
//!
//! The exact scope counts and the dated span (the rest of `preview.rs` in
//! the spec) are slice 6b2's.

use rusqlite::Connection;

/// What identifies the archive's content at one moment. Equal
/// generations mean no import wrote in between. Opaque: only compared,
/// and `Debug` prints none of it.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ArchiveGeneration {
    newest_completed_session: Option<i64>,
    /// The newest session's id, state code and resume count (a resumed
    /// session writes under the same id).
    newest_session: Option<(i64, i64, i64)>,
    max_message: Option<i64>,
    max_body: Option<i64>,
    max_attachment: Option<i64>,
    max_blob: Option<i64>,
}

impl std::fmt::Debug for ArchiveGeneration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ArchiveGeneration(..)")
    }
}

pub(crate) const SQL_GENERATION: &str = "SELECT \
       (SELECT max(id) FROM import_session WHERE state = 1), \
       (SELECT max(id) FROM message), \
       (SELECT max(id) FROM message_body), \
       (SELECT max(id) FROM attachment), \
       (SELECT max(id) FROM blob)";
pub(crate) const SQL_NEWEST_SESSION: &str =
    "SELECT id, state, resumes FROM import_session ORDER BY id DESC LIMIT 1";

#[cfg_attr(not(feature = "fault-injection"), allow(dead_code))]
pub(crate) fn plan_statements() -> Vec<(&'static str, String)> {
    vec![
        ("export generation", SQL_GENERATION.to_owned()),
        ("export newest session", SQL_NEWEST_SESSION.to_owned()),
    ]
}

/// The generation, read in the caller's transaction.
pub(crate) fn generation(c: &Connection) -> rusqlite::Result<ArchiveGeneration> {
    use rusqlite::OptionalExtension;
    let newest_session = c
        .prepare(SQL_NEWEST_SESSION)?
        .query_row([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .optional()?;
    c.prepare(SQL_GENERATION)?.query_row([], |r| {
        Ok(ArchiveGeneration {
            newest_completed_session: r.get(0)?,
            newest_session,
            max_message: r.get(1)?,
            max_body: r.get(2)?,
            max_attachment: r.get(3)?,
            max_blob: r.get(4)?,
        })
    })
}

#[cfg(test)]
mod tests {
    use crate::schema::codes::SessionState;

    #[test]
    fn the_completed_state_is_the_code_the_statement_names() {
        assert_eq!(SessionState::Completed.code(), 1);
        assert!(super::SQL_GENERATION.contains("state = 1"));
    }
}
