//! The `Range` header of a `stream` request (M5 spec §6.4 rule 3).
//!
//! The parse is strict: exactly one `bytes=a-b`, `bytes=a-` or `bytes=-n`,
//! ASCII digits only, values that fit in `u64`, no spaces, no list. Anything
//! else is a refusal, never a best guess: wry keeps only the last of
//! repeated request headers, so a header that is not one well-formed spec
//! may hide another (`web_context.rs:159-167`).
//!
//! [`plan`] turns a spec and the file's length into the bytes to send:
//! clamped to one chunk, `416` for `a > b`, `bytes=-0` and a start at or
//! past the end, and a suffix longer than the file served from byte 0
//! (RFC 9110 §14.1.2).

/// One byte-range spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeSpec {
    /// `bytes=a-b`
    FromTo(u64, u64),
    /// `bytes=a-`
    From(u64),
    /// `bytes=-n`: the last `n` bytes.
    Suffix(u64),
}

/// What to send for a range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangePlan {
    /// `206` with bytes `start..=end` of the file.
    Partial { start: u64, end: u64 },
    /// `416` with `Content-Range: bytes */len`.
    Unsatisfiable,
}

/// ASCII digits only, at least one, no sign, fitting in `u64`.
fn number(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<u64>().ok()
}

/// Parse one `Range` header value. `None` is a refusal.
pub fn parse(value: &str) -> Option<RangeSpec> {
    let spec = value.strip_prefix("bytes=")?;
    let (a, b) = spec.split_once('-')?;
    match (a.is_empty(), b.is_empty()) {
        (true, true) => None,
        (true, false) => number(b).map(RangeSpec::Suffix),
        (false, true) => number(a).map(RangeSpec::From),
        (false, false) => Some(RangeSpec::FromTo(number(a)?, number(b)?)),
    }
}

/// The bytes to send for `spec` of a file of `len` bytes, at most `chunk`
/// of them (`chunk` ≥ 1).
pub fn plan(spec: RangeSpec, len: u64, chunk: u64) -> RangePlan {
    let chunk = chunk.max(1);
    if len == 0 {
        return RangePlan::Unsatisfiable;
    }
    let last = len - 1;
    let (start, end) = match spec {
        RangeSpec::FromTo(a, b) if a > b || a > last => return RangePlan::Unsatisfiable,
        RangeSpec::FromTo(a, b) => (a, b.min(last)),
        RangeSpec::From(a) if a > last => return RangePlan::Unsatisfiable,
        RangeSpec::From(a) => (a, last),
        RangeSpec::Suffix(0) => return RangePlan::Unsatisfiable,
        RangeSpec::Suffix(n) => (len.saturating_sub(n), last),
    };
    let end = end.min(start.saturating_add(chunk - 1));
    RangePlan::Partial { start, end }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB2: u64 = 2 << 20;

    #[test]
    fn accepted_forms() {
        assert_eq!(parse("bytes=0-0"), Some(RangeSpec::FromTo(0, 0)));
        assert_eq!(parse("bytes=0-"), Some(RangeSpec::From(0)));
        assert_eq!(parse("bytes=-1"), Some(RangeSpec::Suffix(1)));
        assert_eq!(
            parse("bytes=18446744073709551615-"),
            Some(RangeSpec::From(u64::MAX))
        );
    }

    #[test]
    fn refused_forms() {
        for bad in [
            "",
            "bytes=",
            "bytes=-",
            "bytes=0-18446744073709551616",
            "bytes=18446744073709551616-",
            "bytes= 0-1",
            "bytes=0 -1",
            "bytes=0- 1",
            "bytes=0-1 ",
            " bytes=0-1",
            "bytes =0-1",
            "Bytes=0-1",
            "items=0-1",
            "bytes=0-1,2-3",
            "bytes=0-1, 2-3",
            "bytes=+0-1",
            "bytes=-+1",
            "bytes=0x1-2",
            "bytes=1-2-3",
            "bytes=\u{0661}-2",
            "bytes=0-1\t",
        ] {
            assert_eq!(parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn plans_and_unsatisfiable_ranges() {
        let len = 10;
        assert_eq!(
            plan(RangeSpec::FromTo(0, 0), len, MIB2),
            RangePlan::Partial { start: 0, end: 0 }
        );
        assert_eq!(
            plan(RangeSpec::From(0), len, MIB2),
            RangePlan::Partial { start: 0, end: 9 }
        );
        assert_eq!(
            plan(RangeSpec::Suffix(1), len, MIB2),
            RangePlan::Partial { start: 9, end: 9 }
        );
        // A suffix past the start is the whole file (RFC 9110 §14.1.2).
        assert_eq!(
            plan(RangeSpec::Suffix(1000), len, MIB2),
            RangePlan::Partial { start: 0, end: 9 }
        );
        // An end past EOF is clamped.
        assert_eq!(
            plan(RangeSpec::FromTo(5, 1000), len, MIB2),
            RangePlan::Partial { start: 5, end: 9 }
        );
        for unsat in [
            RangeSpec::Suffix(0),
            RangeSpec::FromTo(5, 2),
            RangeSpec::FromTo(10, 20),
            RangeSpec::From(10),
            RangeSpec::From(u64::MAX),
        ] {
            assert_eq!(
                plan(unsat, len, MIB2),
                RangePlan::Unsatisfiable,
                "{unsat:?}"
            );
        }
        assert_eq!(
            plan(RangeSpec::From(0), 0, MIB2),
            RangePlan::Unsatisfiable,
            "an empty file has no range"
        );
    }

    #[test]
    fn chunks_are_clamped_to_two_mib() {
        let len = 10 << 20;
        assert_eq!(
            plan(RangeSpec::From(0), len, MIB2),
            RangePlan::Partial {
                start: 0,
                end: MIB2 - 1
            }
        );
        assert_eq!(
            plan(RangeSpec::FromTo(1, len), len, MIB2),
            RangePlan::Partial {
                start: 1,
                end: MIB2
            }
        );
        // A long suffix is served from byte 0 in chunks.
        assert_eq!(
            plan(RangeSpec::Suffix(u64::MAX), len, MIB2),
            RangePlan::Partial {
                start: 0,
                end: MIB2 - 1
            }
        );
    }

    #[test]
    fn a_three_gib_length_near_its_end() {
        let len: u64 = 3 << 30;
        assert_eq!(
            plan(RangeSpec::From(len - 100), len, MIB2),
            RangePlan::Partial {
                start: len - 100,
                end: len - 1
            }
        );
        assert_eq!(
            plan(RangeSpec::FromTo(len - MIB2 - 10, u64::MAX), len, MIB2),
            RangePlan::Partial {
                start: len - MIB2 - 10,
                end: len - 11
            }
        );
        assert_eq!(
            plan(RangeSpec::Suffix(5), len, MIB2),
            RangePlan::Partial {
                start: len - 5,
                end: len - 1
            }
        );
        assert_eq!(
            plan(RangeSpec::From(len), len, MIB2),
            RangePlan::Unsatisfiable
        );
        // Near u64::MAX nothing overflows.
        assert_eq!(
            plan(RangeSpec::From(u64::MAX - 1), u64::MAX, MIB2),
            RangePlan::Partial {
                start: u64::MAX - 1,
                end: u64::MAX - 1
            }
        );
    }
}
