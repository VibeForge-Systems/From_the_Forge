//! Cocoa timestamps, classified per value (M3 §8.1, D27).
//!
//! sms.db stores dates as an offset from the Cocoa epoch, 2001-01-01T00:00:00Z
//! (S15), in seconds in older databases and in nanoseconds in newer ones
//! (supported-formats.md; S8). One database can mix both: snapshot C stores
//! `attachment.created_date` in seconds next to `message.date` in
//! nanoseconds (S3, observed). So the unit is decided **per value**, by two
//! unit windows, and a value outside both is [`TimeInterp::Ambiguous`] with
//! no normalised value.
//!
//! The windows decide the unit, not whether a date is plausible.
//!
//! [`SummaryTime`] reads the `d` of an edit entry in `message_summary_info`
//! (§8.2). That rule is **UNVERIFIED** throughout: no sourced row has `ec`.
#![deny(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::unreachable,
    clippy::unimplemented,
    clippy::string_slice
)]

/// The Cocoa epoch, 2001-01-01T00:00:00Z, in Unix seconds (S15).
pub const COCOA_EPOCH_UNIX_S: i64 = 978_307_200;

/// The first raw value read as nanoseconds (supported-formats.md; S8). As
/// nanoseconds it is 2001-01-01T00:16:40Z, so the nanosecond window starts
/// there.
pub const NS_THRESHOLD: i64 = 1_000_000_000_000;

/// 2100-01-01T00:00:00Z in Cocoa seconds: the end of the seconds window.
pub const SECONDS_MAX: i64 = 3_124_137_600;

/// 2100-01-01T00:00:00Z in Cocoa nanoseconds: the end of the nanosecond
/// window.
pub const NS_MAX: i64 = 3_124_137_600_000_000_000;

const NS_PER_S: i64 = 1_000_000_000;

use std::time::{SystemTime, UNIX_EPOCH};

/// A raw Cocoa timestamp and how it was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CocoaTime {
    /// The raw value, always kept.
    pub raw: i64,
    /// How the raw value was classified.
    pub interp: TimeInterp,
}

/// How one raw timestamp was classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TimeInterp {
    /// `raw == 0`. Whether that means "unset" is a per-column fact
    /// ([`crate::KnownColumn::zero_means_unset`]); otherwise it claims
    /// nothing.
    Zero,
    /// `raw < 0`.
    Negative,
    /// `1 ..< SECONDS_MAX`: seconds since the Cocoa epoch.
    Seconds {
        /// The instant in Unix nanoseconds.
        unix_ns: i64,
    },
    /// `NS_THRESHOLD ..< NS_MAX`: nanoseconds since the Cocoa epoch.
    Nanoseconds {
        /// The instant in Unix nanoseconds.
        unix_ns: i64,
    },
    /// Outside both unit windows, or not convertible: no normalised value.
    Ambiguous,
}

impl CocoaTime {
    /// Classify one raw value.
    ///
    /// | Raw value | Result |
    /// |---|---|
    /// | 0 | `Zero` |
    /// | < 0 | `Negative` |
    /// | 1 ..< `SECONDS_MAX` | `Seconds` |
    /// | `SECONDS_MAX` ..< `NS_THRESHOLD` | `Ambiguous` |
    /// | `NS_THRESHOLD` ..< `NS_MAX` | `Nanoseconds` |
    /// | ≥ `NS_MAX` | `Ambiguous` |
    ///
    /// Conversion uses checked arithmetic; an overflow is `Ambiguous`.
    pub fn classify(raw: i64) -> Self {
        let interp = if raw == 0 {
            TimeInterp::Zero
        } else if raw < 0 {
            TimeInterp::Negative
        } else if raw < SECONDS_MAX {
            raw.checked_add(COCOA_EPOCH_UNIX_S)
                .and_then(|s| s.checked_mul(NS_PER_S))
                .map_or(TimeInterp::Ambiguous, |unix_ns| TimeInterp::Seconds {
                    unix_ns,
                })
        } else if (NS_THRESHOLD..NS_MAX).contains(&raw) {
            COCOA_EPOCH_UNIX_S
                .checked_mul(NS_PER_S)
                .and_then(|epoch| raw.checked_add(epoch))
                .map_or(TimeInterp::Ambiguous, |unix_ns| TimeInterp::Nanoseconds {
                    unix_ns,
                })
        } else {
            TimeInterp::Ambiguous
        };
        CocoaTime { raw, interp }
    }

    /// The instant in Unix nanoseconds, when the value is in a unit window.
    pub fn unix_ns(&self) -> Option<i64> {
        match self.interp {
            TimeInterp::Seconds { unix_ns } | TimeInterp::Nanoseconds { unix_ns } => Some(unix_ns),
            _ => None,
        }
    }
}

/// The raw form of an edit entry's `d` (§8.2).
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum SummaryTimeRaw {
    /// A plist integer, classified by the per-value rule of
    /// [`CocoaTime::classify`].
    Integer(i64),
    /// A plist real, read as Cocoa seconds with a fraction (S4). It must be
    /// finite and inside the seconds window; it is never reclassified as
    /// nanoseconds.
    Real(f64),
    /// A plist date: Cocoa seconds by definition of the format.
    Date,
}

/// An edit entry's `d`, and how it was read. The unit rule is
/// **UNVERIFIED**.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SummaryTime {
    /// The raw value.
    pub raw: SummaryTimeRaw,
    /// How it was classified. `Nanoseconds` occurs only for integers.
    pub interp: TimeInterp,
}

impl SummaryTime {
    /// An integer `d`: the per-value rule of §8.1.
    pub fn from_integer(raw: i64) -> Self {
        SummaryTime {
            raw: SummaryTimeRaw::Integer(raw),
            interp: CocoaTime::classify(raw).interp,
        }
    }

    /// A real `d`: finite, Cocoa seconds with a fraction, inside
    /// `0 < r < SECONDS_MAX`; otherwise `Ambiguous` (or `Zero` /
    /// `Negative`).
    pub fn from_real(r: f64) -> Self {
        let interp = if !r.is_finite() {
            TimeInterp::Ambiguous
        } else if r == 0.0 {
            TimeInterp::Zero
        } else if r < 0.0 {
            TimeInterp::Negative
        } else {
            real_seconds_to_unix_ns(r).map_or(TimeInterp::Ambiguous, |unix_ns| {
                TimeInterp::Seconds { unix_ns }
            })
        };
        SummaryTime {
            raw: SummaryTimeRaw::Real(r),
            interp,
        }
    }

    /// A plist date, already converted to a `SystemTime` by the plist
    /// reader. It is in seconds by definition; outside the seconds window
    /// it is `Ambiguous`.
    pub fn from_date(t: SystemTime) -> Self {
        let unix_ns = match t.duration_since(UNIX_EPOCH) {
            Ok(d) => i64::try_from(d.as_nanos()).ok(),
            Err(e) => i64::try_from(e.duration().as_nanos())
                .ok()
                .and_then(i64::checked_neg),
        };
        let cocoa_ns = unix_ns.and_then(|u| {
            COCOA_EPOCH_UNIX_S
                .checked_mul(NS_PER_S)
                .and_then(|epoch| u.checked_sub(epoch))
        });
        let window_ns = SECONDS_MAX.checked_mul(NS_PER_S);
        let interp = match (unix_ns, cocoa_ns, window_ns) {
            (_, Some(0), _) => TimeInterp::Zero,
            (_, Some(c), _) if c < 0 => TimeInterp::Negative,
            (Some(unix_ns), Some(c), Some(w)) if c < w => TimeInterp::Seconds { unix_ns },
            _ => TimeInterp::Ambiguous,
        };
        SummaryTime {
            raw: SummaryTimeRaw::Date,
            interp,
        }
    }

    /// The instant in Unix nanoseconds, when the value is in a unit window.
    pub fn unix_ns(&self) -> Option<i64> {
        match self.interp {
            TimeInterp::Seconds { unix_ns } | TimeInterp::Nanoseconds { unix_ns } => Some(unix_ns),
            _ => None,
        }
    }
}

/// `0 < r < SECONDS_MAX` Cocoa seconds as Unix nanoseconds, rounded.
fn real_seconds_to_unix_ns(r: f64) -> Option<i64> {
    // Both bounds are exact in f64 (they are below 2^53).
    #[allow(clippy::cast_precision_loss)]
    let (max, epoch) = (SECONDS_MAX as f64, COCOA_EPOCH_UNIX_S as f64);
    if !(r > 0.0 && r < max) {
        return None;
    }
    // (r + epoch) < 4.2e9 s, so the product is below 4.2e18 < i64::MAX: the
    // conversion cannot truncate or wrap, and the value is positive.
    let ns = ((r + epoch) * 1e9).round();
    #[allow(clippy::cast_possible_truncation)]
    Some(ns as i64)
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    const EPOCH_NS: i64 = COCOA_EPOCH_UNIX_S * NS_PER_S;

    fn interp(raw: i64) -> TimeInterp {
        CocoaTime::classify(raw).interp
    }

    #[test]
    fn the_unit_windows() {
        assert_eq!(interp(0), TimeInterp::Zero);
        assert_eq!(interp(-1), TimeInterp::Negative);
        assert_eq!(interp(i64::MIN), TimeInterp::Negative);
        assert_eq!(
            interp(1),
            TimeInterp::Seconds {
                unix_ns: EPOCH_NS + NS_PER_S
            }
        );
        assert_eq!(
            interp(SECONDS_MAX - 1),
            TimeInterp::Seconds {
                unix_ns: (COCOA_EPOCH_UNIX_S + SECONDS_MAX - 1) * NS_PER_S
            }
        );
        assert_eq!(interp(SECONDS_MAX), TimeInterp::Ambiguous);
        assert_eq!(interp(NS_THRESHOLD - 1), TimeInterp::Ambiguous);
        assert_eq!(
            interp(NS_THRESHOLD),
            TimeInterp::Nanoseconds {
                unix_ns: EPOCH_NS + NS_THRESHOLD
            }
        );
        assert_eq!(
            interp(NS_MAX - 1),
            TimeInterp::Nanoseconds {
                unix_ns: EPOCH_NS + NS_MAX - 1
            }
        );
        assert_eq!(interp(NS_MAX), TimeInterp::Ambiguous);
        assert_eq!(interp(i64::MAX), TimeInterp::Ambiguous);
    }

    /// The constants against independent arithmetic on calendar facts.
    #[test]
    fn the_constants() {
        // 2001-01-01 is 31 years after 1970-01-01, with 8 leap days.
        assert_eq!(COCOA_EPOCH_UNIX_S, (31 * 365 + 8) * 86_400);
        // 2001-01-01 to 2100-01-01: 99 years with 24 leap days.
        assert_eq!(SECONDS_MAX, (99 * 365 + 24) * 86_400);
        assert_eq!(NS_MAX, SECONDS_MAX * NS_PER_S);
        // NS_THRESHOLD as nanoseconds is 1,000 s after the epoch: 00:16:40.
        assert_eq!(NS_THRESHOLD / NS_PER_S, 16 * 60 + 40);
        const { assert!(SECONDS_MAX < NS_THRESHOLD) };
    }

    #[test]
    fn unix_ns_only_inside_a_window() {
        assert_eq!(CocoaTime::classify(0).unix_ns(), None);
        assert_eq!(CocoaTime::classify(-5).unix_ns(), None);
        assert_eq!(CocoaTime::classify(SECONDS_MAX).unix_ns(), None);
        assert_eq!(CocoaTime::classify(1).unix_ns(), Some(EPOCH_NS + NS_PER_S));
        let t = CocoaTime::classify(NS_THRESHOLD);
        assert_eq!(t.raw, NS_THRESHOLD);
        assert_eq!(t.unix_ns(), Some(EPOCH_NS + NS_THRESHOLD));
    }

    /// M3 §4.3 / §19.1 "lints": the parser lints are live in this module.
    /// `#[expect]` fails the clippy run (unfulfilled expectation, denied by
    /// `-D warnings`) if `string_slice` stops firing on `str` range
    /// indexing. In clippy 0.1.97, `indexing_slicing` fires on byte slices
    /// but not on `str` ranges, so `string_slice` is the lint that covers
    /// them.
    #[test]
    fn the_string_slice_lint_fires() {
        let s = "ab";
        #[expect(clippy::string_slice, reason = "proves the lint fires (§19.1)")]
        let tail = &s[1..];
        assert_eq!(tail, "b");
    }

    /// §19.1 `time`: the summary `d` as an integer, a real and a plist date.
    #[test]
    fn summary_times() {
        let i = SummaryTime::from_integer(NS_THRESHOLD);
        assert_eq!(i.raw, SummaryTimeRaw::Integer(NS_THRESHOLD));
        assert_eq!(i.unix_ns(), Some(EPOCH_NS + NS_THRESHOLD));
        assert_eq!(SummaryTime::from_integer(0).interp, TimeInterp::Zero);
        assert_eq!(SummaryTime::from_integer(-1).interp, TimeInterp::Negative);

        for (r, want) in [
            (f64::NAN, TimeInterp::Ambiguous),
            (f64::INFINITY, TimeInterp::Ambiguous),
            (f64::NEG_INFINITY, TimeInterp::Ambiguous),
            (0.0, TimeInterp::Zero),
            (-0.0, TimeInterp::Zero),
            (-1.5, TimeInterp::Negative),
            (1e12, TimeInterp::Ambiguous),
            (SECONDS_MAX as f64, TimeInterp::Ambiguous),
        ] {
            assert_eq!(SummaryTime::from_real(r).interp, want, "{r}");
        }
        let frac = SummaryTime::from_real(1.25);
        assert_eq!(frac.unix_ns(), Some(EPOCH_NS + 1_250_000_000));
        let near = SummaryTime::from_real(700_000_000.5);
        assert_eq!(
            near.unix_ns(),
            Some((COCOA_EPOCH_UNIX_S + 700_000_000) * NS_PER_S + 500_000_000)
        );

        let at = |unix_s: i64| {
            let t = if unix_s >= 0 {
                UNIX_EPOCH + std::time::Duration::from_secs(unix_s.unsigned_abs())
            } else {
                UNIX_EPOCH - std::time::Duration::from_secs(unix_s.unsigned_abs())
            };
            SummaryTime::from_date(t)
        };
        let d = at(COCOA_EPOCH_UNIX_S + 10);
        assert_eq!(d.raw, SummaryTimeRaw::Date);
        assert_eq!(d.unix_ns(), Some(EPOCH_NS + 10 * NS_PER_S));
        assert_eq!(at(COCOA_EPOCH_UNIX_S).interp, TimeInterp::Zero);
        assert_eq!(at(0).interp, TimeInterp::Negative);
        assert_eq!(at(-5).interp, TimeInterp::Negative);
        assert_eq!(
            at(COCOA_EPOCH_UNIX_S + SECONDS_MAX).interp,
            TimeInterp::Ambiguous
        );
    }
}
