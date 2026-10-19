//! Time in exports (M6 spec §4.6, D128, D145).
//!
//! Rust links no time-zone database (D104 keeps one zone source, the
//! webview's ICU). The webview builds a **zone offset table** for a
//! bounded window ([`ZoneTableInput`]); Rust validates it
//! ([`ZoneTable::new`]) and does everything else here with its own civil
//! arithmetic:
//!
//! - **The window** is the export's exact dated span clamped to
//!   [`CLAMP_FROM_MS`] (1900-01-01T00:00Z) … now + [`CLAMP_AHEAD_MS`]. The
//!   table must cover the clamped span.
//! - **Outliers.** An instant outside the clamped window is written in UTC
//!   with `Z` and the qualifier "[UTC: outside the zone table's range]",
//!   and counted ([`Localized::OutsideWindow`]).
//! - **Staleness is an error.** An instant inside the clamped window that
//!   the table does not cover stops the export with
//!   [`ExportError::ZoneTableStale`] before anything is written. Zones are
//!   never mixed silently.
//! - **Local-day bounds** ([`ZoneTable::day_start`]) are computed here,
//!   from the same table, following DST gaps (the day starts at the first
//!   instant whose local time is at or after midnight) and repeats (the
//!   first midnight).
//! - **Formatting** ([`format_iso`], [`format_human`]) never shows more
//!   precision than the unit has: seconds never show a fraction, and
//!   nanoseconds show one only in JSON's `utc`. Every local timestamp
//!   carries its offset.
//!
//! The civil-date arithmetic counts days from 1970-01-01 in the proleptic
//! Gregorian calendar over whole 400-year eras (146,097 days each), so it
//! is exact for every year from -9999 to 9999, including leap years,
//! dates before 1970 and year 9999.

use crate::error::{DateRangeRefusal, ExportError, ZoneTableRefusal};

/// The earliest instant the zone window may cover: 1900-01-01T00:00Z, in
/// Unix milliseconds.
pub const CLAMP_FROM_MS: i64 = -2_208_988_800_000;
/// How far past "now" the window may reach: 366 days, so "now + 1 year"
/// is covered in a leap year too.
pub const CLAMP_AHEAD_MS: i64 = 366 * 86_400_000;
/// The most transitions a table may hold.
pub const MAX_TRANSITIONS: usize = 4_096;
/// The largest offset magnitude, in seconds (±18 hours).
pub const MAX_OFFSET_SECONDS: i32 = 18 * 3_600;
/// The longest zone name.
pub const MAX_NAME_BYTES: usize = 64;

const NS_PER_MS: i64 = 1_000_000;
const NS_PER_S: i64 = 1_000_000_000;
const S_PER_DAY: i64 = 86_400;

// ---------------------------------------------------------------------------
// Civil arithmetic
// ---------------------------------------------------------------------------

/// A proleptic Gregorian date.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CivilDate {
    pub year: i64,
    pub month: u8,
    pub day: u8,
}

/// `true` for a leap year.
pub fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// The days in `month` of `year`.
pub fn days_in_month(year: i64, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        _ => 0,
    }
}

impl CivilDate {
    /// A valid date, or `None` (month 13, February 30, …). Years outside
    /// -9999..=9999 are refused.
    pub fn new(year: i64, month: u8, day: u8) -> Option<CivilDate> {
        let ok = (-9_999..=9_999).contains(&year)
            && (1..=12).contains(&month)
            && day >= 1
            && day <= days_in_month(year, month);
        ok.then_some(CivilDate { year, month, day })
    }

    /// Days since 1970-01-01 (negative before it).
    pub fn days_from_epoch(self) -> i64 {
        // Shift the year to start in March, so the leap day is the last
        // day of the shifted year.
        let m = i64::from(self.month);
        let y = if m <= 2 { self.year - 1 } else { self.year };
        let era = y.div_euclid(400);
        let year_of_era = y.rem_euclid(400);
        let month_from_march = (m + 9) % 12;
        let day_of_year = (153 * month_from_march + 2) / 5 + i64::from(self.day) - 1;
        let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
        // 719,468 days from 0000-03-01 to 1970-01-01.
        era * 146_097 + day_of_era - 719_468
    }

    /// The date `days` after 1970-01-01.
    pub fn from_days(days: i64) -> CivilDate {
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let day_of_era = z.rem_euclid(146_097);
        let year_of_era =
            (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let month_from_march = (5 * day_of_year + 2) / 153;
        let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
        let month = if month_from_march < 10 {
            month_from_march + 3
        } else {
            month_from_march - 9
        };
        let year = year_of_era + era * 400 + i64::from(month <= 2);
        CivilDate {
            year,
            // Both are in range by construction (1..=12, 1..=31).
            month: u8::try_from(month).unwrap_or(1),
            day: u8::try_from(day).unwrap_or(1),
        }
    }
}

/// A local date and time of day, to the second.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CivilTime {
    pub date: CivilDate,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl CivilTime {
    /// The civil time `secs` seconds after 1970-01-01T00:00:00 (of the
    /// same clock).
    pub fn from_seconds(secs: i64) -> CivilTime {
        let days = secs.div_euclid(S_PER_DAY);
        let rem = secs.rem_euclid(S_PER_DAY);
        CivilTime {
            date: CivilDate::from_days(days),
            hour: u8::try_from(rem / 3_600).unwrap_or(0),
            minute: u8::try_from(rem % 3_600 / 60).unwrap_or(0),
            second: u8::try_from(rem % 60).unwrap_or(0),
        }
    }
}

// ---------------------------------------------------------------------------
// The zone table
// ---------------------------------------------------------------------------

/// The zone table as the webview sends it (spec §4.6): the zone's name, the
/// window it covers and its offsets, each `[utcMs, offsetSeconds]` in force
/// from that instant on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoneTableInput {
    pub name: String,
    pub window_utc_ms: (i64, i64),
    pub transitions: Vec<(i64, i32)>,
}

/// A validated zone table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoneTable {
    name: String,
    window: (i64, i64),
    transitions: Vec<(i64, i32)>,
    clamp: (i64, i64),
}

/// An instant placed in the export's zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Localized {
    /// Inside the window: the offset in force, in seconds.
    Local { offset_seconds: i32 },
    /// Outside the clamped window: written in UTC and counted.
    OutsideWindow,
}

/// The clamped window for a dated span (Unix ms) at `now_ms`.
pub fn clamp_window(now_ms: i64) -> (i64, i64) {
    (CLAMP_FROM_MS, now_ms.saturating_add(CLAMP_AHEAD_MS))
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_BYTES
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'+' | b'-' | b'/'))
}

impl ZoneTable {
    /// Validate `input` for an export whose exact dated span is `span_ms`
    /// (`None` without a dated row), at `now_ms` (spec §4.6):
    ///
    /// - a name matching `^[A-Za-z0-9_+\-/]{1,64}$`;
    /// - 1 to 4,096 transitions, strictly increasing, offsets within
    ///   ±18 h, the first at or before the window's start;
    /// - a window that covers the span clamped to the clamp window.
    pub fn new(
        input: ZoneTableInput,
        span_ms: Option<(i64, i64)>,
        now_ms: i64,
    ) -> Result<ZoneTable, ExportError> {
        let refuse = |r| Err(ExportError::ZoneTable(r));
        if !valid_name(&input.name) {
            return refuse(ZoneTableRefusal::BadName);
        }
        let n = input.transitions.len();
        if n == 0 || n > MAX_TRANSITIONS {
            return refuse(ZoneTableRefusal::TransitionCount);
        }
        if input.transitions.windows(2).any(|w| match w {
            [a, b] => a.0 >= b.0,
            _ => false,
        }) {
            return refuse(ZoneTableRefusal::NotIncreasing);
        }
        if input
            .transitions
            .iter()
            .any(|&(_, o)| o.abs() > MAX_OFFSET_SECONDS)
        {
            return refuse(ZoneTableRefusal::OffsetOutOfRange);
        }
        let (from, to) = input.window_utc_ms;
        if from >= to {
            return refuse(ZoneTableRefusal::BadWindow);
        }
        if input.transitions.first().is_some_and(|t| t.0 > from) {
            return refuse(ZoneTableRefusal::StartNotCovered);
        }
        let clamp = clamp_window(now_ms);
        if let Some((lo, hi)) = span_ms {
            let lo = lo.clamp(clamp.0, clamp.1);
            let hi = hi.clamp(clamp.0, clamp.1);
            if lo < from || hi > to {
                return refuse(ZoneTableRefusal::SpanNotCovered);
            }
        }
        Ok(ZoneTable {
            name: input.name,
            window: (from, to),
            transitions: input.transitions,
            clamp,
        })
    }

    /// The zone's name (for the manifest; never logged).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The window the table covers, in Unix ms.
    pub fn window_ms(&self) -> (i64, i64) {
        self.window
    }

    /// The transitions inside the window, for the manifest (§7.1).
    pub fn transitions(&self) -> &[(i64, i32)] {
        &self.transitions
    }

    /// The offset in force at `utc_ms`, assuming it is in the window.
    fn offset_at_ms(&self, utc_ms: i64) -> i32 {
        let i = self.transitions.partition_point(|t| t.0 <= utc_ms);
        i.checked_sub(1)
            .and_then(|i| self.transitions.get(i))
            .map_or(0, |t| t.1)
    }

    /// Place the instant `utc_ns` (spec §4.6): its offset, or outside the
    /// clamped window; `ZoneTableStale` when it is inside the clamped
    /// window but not the table's.
    pub fn localize(&self, utc_ns: i64) -> Result<Localized, ExportError> {
        let ms = utc_ns.div_euclid(NS_PER_MS);
        if ms < self.clamp.0 || ms > self.clamp.1 {
            return Ok(Localized::OutsideWindow);
        }
        if ms < self.window.0 || ms > self.window.1 {
            return Err(ExportError::ZoneTableStale);
        }
        Ok(Localized::Local {
            offset_seconds: self.offset_at_ms(ms),
        })
    }

    /// The first instant (Unix ns) whose local time is at or after local
    /// midnight of `date`: local midnight itself, the first of two in a
    /// repeat, or the end of a gap that swallowed it.
    pub fn day_start(&self, date: CivilDate) -> Result<i64, ExportError> {
        let local = date.days_from_epoch() * S_PER_DAY;
        let outside = || ExportError::DateRange(DateRangeRefusal::OutsideWindow);
        let lo_ms = (local - i64::from(MAX_OFFSET_SECONDS)) * 1_000;
        let start = self.transitions.partition_point(|t| t.0 <= lo_ms);
        let start = start.saturating_sub(1);
        let mut found: Option<i64> = None;
        for (i, &(t_ms, off)) in self.transitions.iter().enumerate().skip(start) {
            let seg_start = t_ms.div_euclid(1_000);
            let seg_end = self
                .transitions
                .get(i + 1)
                .map_or(i64::MAX, |n| n.0.div_euclid(1_000));
            let u = (local - i64::from(off)).max(seg_start);
            if u < seg_end {
                found = Some(u);
                break;
            }
        }
        let u = found.ok_or_else(outside)?;
        let ms = u * 1_000;
        if ms < self.window.0 || ms > self.window.1 {
            return Err(outside());
        }
        Ok(u * NS_PER_S)
    }

    /// The half-open instants `[start of from, start of until)` of a local
    /// date range (Flow C): local midnight to local midnight, in this
    /// table's zone.
    pub fn day_range(&self, from: CivilDate, until: CivilDate) -> Result<(i64, i64), ExportError> {
        if until <= from {
            return Err(ExportError::DateRange(DateRangeRefusal::Empty));
        }
        Ok((self.day_start(from)?, self.day_start(until)?))
    }
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

/// How a human-readable time is written (the plan's options).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HumanStyle {
    /// `true`: `14:02:11`; `false`: `02:02:11 PM`.
    pub h23: bool,
    pub seconds: bool,
}

/// What a timestamp's precision allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Precision {
    Seconds,
    Nanoseconds,
}

fn offset_text(offset_seconds: i32, colon: bool) -> String {
    let sign = if offset_seconds < 0 { '-' } else { '+' };
    let abs = offset_seconds.unsigned_abs();
    let (h, m, s) = (abs / 3_600, abs % 3_600 / 60, abs % 60);
    let sep = if colon { ":" } else { "" };
    if s == 0 {
        format!("{sign}{h:02}{sep}{m:02}")
    } else {
        // A historical offset with seconds (local mean time) keeps them.
        format!("{sign}{h:02}{sep}{m:02}{sep}{s:02}")
    }
}

fn year_text(year: i64) -> String {
    if (0..=9_999).contains(&year) {
        format!("{year:04}")
    } else if year < 0 {
        format!("-{:04}", year.unsigned_abs())
    } else {
        format!("+{year}")
    }
}

/// `utc_ns` split into whole seconds and nanoseconds (floored).
fn split(utc_ns: i64) -> (i64, i64) {
    (utc_ns.div_euclid(NS_PER_S), utc_ns.rem_euclid(NS_PER_S))
}

/// ISO 8601: `2024-03-10T13:02:11Z` (UTC, `offset_seconds = None`) or
/// `2024-03-10T14:02:11+01:00`. A fraction is written only for
/// [`Precision::Nanoseconds`] in UTC (JSON's `utc`), trailing zeros kept
/// to nine digits so every value has one shape.
pub fn format_iso(utc_ns: i64, offset_seconds: Option<i32>, precision: Precision) -> String {
    let (secs, nanos) = split(utc_ns);
    let local = secs + i64::from(offset_seconds.unwrap_or(0));
    let t = CivilTime::from_seconds(local);
    let mut out = format!(
        "{}-{:02}-{:02}T{:02}:{:02}:{:02}",
        year_text(t.date.year),
        t.date.month,
        t.date.day,
        t.hour,
        t.minute,
        t.second
    );
    match offset_seconds {
        None => {
            if precision == Precision::Nanoseconds {
                out.push_str(&format!(".{nanos:09}"));
            }
            out.push('Z');
        }
        Some(o) => out.push_str(&offset_text(o, true)),
    }
    out
}

/// Human-readable: `2024-03-10 14:02:11 +01:00`, or `02:02:11 PM +01:00`
/// with the 12-hour clock, seconds optional, never a fraction. In UTC
/// (`None`) the offset reads `Z`.
pub fn format_human(utc_ns: i64, offset_seconds: Option<i32>, style: HumanStyle) -> String {
    let (secs, _) = split(utc_ns);
    let t = CivilTime::from_seconds(secs + i64::from(offset_seconds.unwrap_or(0)));
    let date = format!(
        "{}-{:02}-{:02}",
        year_text(t.date.year),
        t.date.month,
        t.date.day
    );
    let (hour, suffix) = if style.h23 {
        (t.hour, "")
    } else {
        let h = match t.hour % 12 {
            0 => 12,
            h => h,
        };
        (h, if t.hour < 12 { " AM" } else { " PM" })
    };
    let clock = if style.seconds {
        format!("{hour:02}:{:02}:{:02}", t.minute, t.second)
    } else {
        format!("{hour:02}:{:02}", t.minute)
    };
    let zone = offset_seconds.map_or_else(|| "Z".to_owned(), |o| offset_text(o, true));
    format!("{date} {clock}{suffix} {zone}")
}

/// The local date an instant falls on (for day headings), or the UTC date
/// outside the window.
pub fn local_date(utc_ns: i64, at: Localized) -> CivilDate {
    let (secs, _) = split(utc_ns);
    let off = match at {
        Localized::Local { offset_seconds } => i64::from(offset_seconds),
        Localized::OutsideWindow => 0,
    };
    CivilTime::from_seconds(secs + off).date
}

#[cfg(test)]
mod tests {
    use super::*;

    const H23: HumanStyle = HumanStyle {
        h23: true,
        seconds: true,
    };

    fn d(y: i64, m: u8, day: u8) -> CivilDate {
        CivilDate::new(y, m, day).expect("date")
    }

    fn ms(date: CivilDate, h: i64, m: i64, s: i64) -> i64 {
        (date.days_from_epoch() * S_PER_DAY + h * 3_600 + m * 60 + s) * 1_000
    }

    /// A Berlin-like zone for 2024: +01:00, +02:00 from 2024-03-31T01:00Z,
    /// +01:00 from 2024-10-27T01:00Z.
    fn berlin() -> ZoneTable {
        ZoneTable::new(
            ZoneTableInput {
                name: "Europe/Berlin".into(),
                window_utc_ms: (ms(d(2024, 1, 1), 0, 0, 0), ms(d(2025, 1, 1), 0, 0, 0)),
                transitions: vec![
                    (ms(d(2023, 10, 29), 1, 0, 0), 3_600),
                    (ms(d(2024, 3, 31), 1, 0, 0), 7_200),
                    (ms(d(2024, 10, 27), 1, 0, 0), 3_600),
                ],
            },
            Some((ms(d(2024, 2, 1), 0, 0, 0), ms(d(2024, 12, 1), 0, 0, 0))),
            ms(d(2024, 12, 2), 0, 0, 0),
        )
        .expect("table")
    }

    #[test]
    fn civil_vectors() {
        let cases: [((i64, u8, u8), i64); 10] = [
            ((1970, 1, 1), 0),
            ((1969, 12, 31), -1),
            ((2000, 2, 29), 11_016),
            ((2000, 3, 1), 11_017),
            ((1900, 1, 1), -25_567),
            ((1900, 3, 1), -25_508),
            ((2007, 1, 1), 13_514),
            ((2024, 3, 10), 19_792),
            ((9999, 12, 31), 2_932_896),
            ((1, 1, 1), -719_162),
        ];
        for ((y, m, day), days) in cases {
            let c = d(y, m, day);
            assert_eq!(c.days_from_epoch(), days, "{c:?}");
            assert_eq!(CivilDate::from_days(days), c, "{days}");
        }
        assert!(CivilDate::new(1900, 2, 29).is_none());
        assert!(CivilDate::new(2000, 2, 29).is_some());
        assert!(CivilDate::new(2023, 13, 1).is_none());
        for days in -800_000..-799_000 {
            assert_eq!(CivilDate::from_days(days).days_from_epoch(), days);
        }
    }

    #[test]
    fn formats_carry_their_offset_and_precision() {
        let ns = ms(d(2024, 3, 10), 13, 2, 11) * NS_PER_MS + 123_456_789;
        assert_eq!(
            format_iso(ns, None, Precision::Nanoseconds),
            "2024-03-10T13:02:11.123456789Z"
        );
        assert_eq!(
            format_iso(ns, None, Precision::Seconds),
            "2024-03-10T13:02:11Z"
        );
        assert_eq!(
            format_iso(ns, Some(3_600), Precision::Nanoseconds),
            "2024-03-10T14:02:11+01:00"
        );
        assert_eq!(
            format_human(ns, Some(3_600), H23),
            "2024-03-10 14:02:11 +01:00"
        );
        let h12 = HumanStyle {
            h23: false,
            seconds: true,
        };
        assert_eq!(
            format_human(ns, Some(3_600), h12),
            "2024-03-10 02:02:11 PM +01:00"
        );
        let midnight = ms(d(2024, 3, 10), 0, 30, 0) * NS_PER_MS;
        assert_eq!(
            format_human(midnight, None, h12),
            "2024-03-10 12:30:00 AM Z"
        );
        let short = HumanStyle {
            h23: true,
            seconds: false,
        };
        assert_eq!(
            format_human(ns, Some(-34_200), short),
            "2024-03-10 03:32 -09:30"
        );
        // Pre-1970 floors toward the past; a seconds offset is kept.
        assert_eq!(
            format_iso(-1, None, Precision::Nanoseconds),
            "1969-12-31T23:59:59.999999999Z"
        );
        assert_eq!(
            format_human(-1, Some(-1_172), H23),
            "1969-12-31 23:40:27 -00:19:32"
        );
    }

    #[test]
    fn validation_refuses_bad_tables() {
        let ok = ZoneTableInput {
            name: "UTC".into(),
            window_utc_ms: (0, 1_000),
            transitions: vec![(0, 0)],
        };
        assert!(ZoneTable::new(ok.clone(), Some((10, 20)), 0).is_ok());
        let refused = |i: ZoneTableInput, span: Option<(i64, i64)>| match ZoneTable::new(i, span, 0)
        {
            Err(ExportError::ZoneTable(r)) => r,
            other => panic!("{other:?}"),
        };
        for name in ["", "Europe/Berlin; rm", "a b", &"x".repeat(65)] {
            let i = ZoneTableInput {
                name: name.into(),
                ..ok.clone()
            };
            assert_eq!(refused(i, None), ZoneTableRefusal::BadName);
        }
        let i = ZoneTableInput {
            transitions: vec![],
            ..ok.clone()
        };
        assert_eq!(refused(i, None), ZoneTableRefusal::TransitionCount);
        let i = ZoneTableInput {
            transitions: (0..=4_096).map(|n| (n, 0)).collect(),
            ..ok.clone()
        };
        assert_eq!(refused(i, None), ZoneTableRefusal::TransitionCount);
        let i = ZoneTableInput {
            transitions: vec![(0, 0), (0, 3_600)],
            ..ok.clone()
        };
        assert_eq!(refused(i, None), ZoneTableRefusal::NotIncreasing);
        let i = ZoneTableInput {
            transitions: vec![(0, 18 * 3_600 + 1)],
            ..ok.clone()
        };
        assert_eq!(refused(i, None), ZoneTableRefusal::OffsetOutOfRange);
        let i = ZoneTableInput {
            window_utc_ms: (5, 5),
            ..ok.clone()
        };
        assert_eq!(refused(i, None), ZoneTableRefusal::BadWindow);
        let i = ZoneTableInput {
            transitions: vec![(1, 0)],
            ..ok.clone()
        };
        assert_eq!(refused(i, None), ZoneTableRefusal::StartNotCovered);
        assert_eq!(
            refused(ok, Some((10, 2_000))),
            ZoneTableRefusal::SpanNotCovered
        );
    }

    #[test]
    fn the_window_is_clamped_and_outliers_go_to_utc() {
        let now = ms(d(2026, 10, 19), 0, 0, 0);
        let (lo, hi) = clamp_window(now);
        let table = ZoneTable::new(
            ZoneTableInput {
                name: "Etc/GMT-1".into(),
                window_utc_ms: (lo, hi),
                transitions: vec![(lo, 3_600)],
            },
            // A span from year 1800 to 9999 is clamped to the window.
            Some((i64::MIN / 2, i64::MAX / 2)),
            now,
        )
        .expect("clamped span is covered");
        // Pre-1900 and far-future instants are outliers, not errors.
        let pre = ms(d(1899, 12, 31), 23, 0, 0) * NS_PER_MS;
        assert_eq!(table.localize(pre).expect("ok"), Localized::OutsideWindow);
        let far = ms(d(2200, 1, 1), 0, 0, 0) * NS_PER_MS;
        assert_eq!(table.localize(far).expect("ok"), Localized::OutsideWindow);
        assert_eq!(
            table.localize(0).expect("ok"),
            Localized::Local {
                offset_seconds: 3_600
            }
        );
    }

    #[test]
    fn an_instant_the_table_does_not_cover_is_stale() {
        let t = berlin();
        let outside_table = ms(d(2025, 6, 1), 0, 0, 0) * NS_PER_MS;
        assert!(matches!(
            t.localize(outside_table),
            Err(ExportError::ZoneTableStale)
        ));
        let at = |h: i64| ms(d(2024, 3, 31), h, 0, 0) * NS_PER_MS;
        assert_eq!(
            t.localize(at(0)).expect("ok"),
            Localized::Local {
                offset_seconds: 3_600
            }
        );
        assert_eq!(
            t.localize(at(1)).expect("ok"),
            Localized::Local {
                offset_seconds: 7_200
            }
        );
        assert_eq!(
            t.localize(at(1) - 1).expect("ok"),
            Localized::Local {
                offset_seconds: 3_600
            }
        );
    }

    #[test]
    fn local_day_bounds_follow_dst() {
        let t = berlin();
        // A spring-forward day is 23 hours long, a fall-back day 25.
        let (a, b) = t.day_range(d(2024, 3, 31), d(2024, 4, 1)).expect("range");
        assert_eq!(b - a, 23 * 3_600 * NS_PER_S);
        assert_eq!(a, ms(d(2024, 3, 30), 23, 0, 0) * NS_PER_MS);
        let (a, b) = t
            .day_range(d(2024, 10, 27), d(2024, 10, 28))
            .expect("range");
        assert_eq!(b - a, 25 * 3_600 * NS_PER_S);
        assert!(matches!(
            t.day_range(d(2024, 5, 2), d(2024, 5, 1)),
            Err(ExportError::DateRange(DateRangeRefusal::Empty))
        ));
        assert!(matches!(
            t.day_range(d(2024, 5, 1), d(2026, 5, 1)),
            Err(ExportError::DateRange(DateRangeRefusal::OutsideWindow))
        ));
    }

    #[test]
    fn a_gap_at_midnight_starts_the_day_at_the_gap_end_and_a_repeat_at_the_first() {
        // A zone that springs forward at local midnight (00:00 -> 01:00,
        // -03:00 -> -02:00) and falls back at 01:00 local (01:00 -> 00:00,
        // -02:00 -> -03:00), so that midnight happens twice.
        let gap_utc = ms(d(2018, 11, 4), 3, 0, 0); // local 00:00 at -03:00
        let back_utc = ms(d(2019, 2, 17), 3, 0, 0); // local 01:00 at -02:00
        let t = ZoneTable::new(
            ZoneTableInput {
                name: "America/Sao_Paulo".into(),
                window_utc_ms: (ms(d(2018, 1, 1), 0, 0, 0), ms(d(2019, 12, 31), 0, 0, 0)),
                transitions: vec![
                    (ms(d(2017, 1, 1), 0, 0, 0), -10_800),
                    (gap_utc, -7_200),
                    (back_utc, -10_800),
                ],
            },
            None,
            ms(d(2019, 12, 1), 0, 0, 0),
        )
        .expect("table");
        // Midnight of 2018-11-04 does not exist: the day starts at the gap's
        // end, 01:00 local; the day before is 24 hours long.
        assert_eq!(
            t.day_start(d(2018, 11, 4)).expect("start"),
            gap_utc * NS_PER_MS
        );
        assert_eq!(
            t.day_start(d(2018, 11, 3)).expect("start"),
            ms(d(2018, 11, 3), 3, 0, 0) * NS_PER_MS
        );
        // Midnight of 2019-02-17 happens twice: the first one, at -02:00;
        // that day is 25 hours long.
        assert_eq!(
            t.day_start(d(2019, 2, 17)).expect("start"),
            ms(d(2019, 2, 17), 2, 0, 0) * NS_PER_MS
        );
        let (a, b) = t.day_range(d(2019, 2, 17), d(2019, 2, 18)).expect("range");
        assert_eq!(b - a, 25 * 3_600 * NS_PER_S);
    }
}
