//! `handle`: one (address, service) row. S3's DDL declares
//! `UNIQUE(id, service)`, so one address can have several rows.

use crate::codes::ServiceClass;
use crate::col::Col;
use crate::db::{FromRow, RowReader};
use crate::error::{MessagesError, Stage};
use crate::progress::MessagesPhase;
use crate::redact::{Redacted, SqlText};
use crate::schema::known::handle as c;
use crate::schema::KnownTable;

use super::shape_debug;

/// One `handle` row.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct HandleRecord {
    /// `ROWID`.
    pub rowid: i64,
    /// `id`: the address (a phone number or an email address).
    pub id: Col<Redacted<SqlText>>,
    /// `service`, raw.
    pub service: Col<Redacted<SqlText>>,
    /// The class of `service`, when it is readable text.
    pub service_class: Option<ServiceClass>,
    /// `country`.
    pub country: Col<Redacted<SqlText>>,
    /// `uncanonicalized_id`.
    pub uncanonicalized_id: Col<Redacted<SqlText>>,
    /// `person_centric_id`.
    pub person_centric_id: Col<Redacted<SqlText>>,
}

shape_debug!(HandleRecord { id, service, country, uncanonicalized_id, person_centric_id } kinds { service_class });

/// The class of a readable service column.
pub(crate) fn service_class(col: &Col<Redacted<SqlText>>) -> Option<ServiceClass> {
    col.value()
        .map(|s| ServiceClass::classify(s.expose().as_bytes()))
}

impl FromRow for HandleRecord {
    const TABLE: KnownTable = KnownTable::Handle;
    const PHASE: MessagesPhase = MessagesPhase::ReadingHandles;
    const STAGE: Stage = Stage::Handles;

    fn from_row(rowid: i64, r: &mut RowReader<'_, '_>) -> Result<Self, MessagesError> {
        let service = r.text(c::SERVICE)?;
        Ok(HandleRecord {
            rowid,
            id: r.text(c::ID)?,
            service_class: service_class(&service),
            service,
            country: r.text(c::COUNTRY)?,
            uncanonicalized_id: r.text(c::UNCANONICALIZED_ID)?,
            person_centric_id: r.text(c::PERSON_CENTRIC_ID)?,
        })
    }
}
