//! Bytes, the header and integers (M3 §10.1–§10.2).
//!
//! | Head | Meaning |
//! |---|---|
//! | `0x00–0x7F`, `0x92–0xFF` | the head itself: `i8` for signed types, `u8` for unsigned |
//! | `0x81` | 2 bytes, little-endian |
//! | `0x82` | 4 bytes, little-endian |
//! | `0x87` | 8 bytes, little-endian (T1, observed from Apple's encoder) |
//! | `0x83` | a float or double follows (only where the type is `f` or `d`) |
//! | anything else | `UnexpectedTag` |
//!
//! Whether Apple writes unsigned 128–255 as one byte or with `0x81` is
//! UNVERIFIED, so both are accepted. Every read checks the bytes remaining
//! first; nothing here slices without `get`.
#![deny(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::unreachable,
    clippy::unimplemented,
    clippy::string_slice
)]

use super::{TsError, TsErrorKind};

/// Head byte: a new entry follows.
pub(crate) const NEW: u8 = 0x84;
/// Head byte: nil.
pub(crate) const NIL: u8 = 0x85;
/// Head byte: the end of an object.
pub(crate) const END: u8 = 0x86;
/// Head byte: a float or double follows.
const REAL: u8 = 0x83;
const INT16: u8 = 0x81;
const INT32: u8 = 0x82;
const INT64: u8 = 0x87;
/// A reference is the head read as a signed integer, plus this bias (T1).
const REF_BIAS: i64 = 110;

/// Streamer version 4, the signature `streamtyped` (little-endian), and
/// system version 1000 (T1, T2).
pub(crate) const HEADER: [u8; 16] = [
    0x04, 0x0b, b's', b't', b'r', b'e', b'a', b'm', b't', b'y', b'p', b'e', b'd', 0x81, 0xe8, 0x03,
];

/// A half-open byte range in the input. Inputs are at most 16 MiB, so
/// `u32` offsets suffice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Span {
    pub(crate) start: u32,
    pub(crate) end: u32,
}

impl Span {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn len(self) -> u32 {
        self.end.saturating_sub(self.start)
    }
}

/// A cursor over the input.
pub(crate) struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0 }
    }

    pub(crate) fn pos(&self) -> usize {
        self.pos
    }

    pub(crate) fn at_end(&self) -> bool {
        self.pos >= self.data.len()
    }

    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    fn err(&self, kind: TsErrorKind) -> TsError {
        TsError::new(kind, self.pos)
    }

    /// Check the 16-byte header. A strict prefix of it is `Truncated`;
    /// anything else, big-endian `typedstream` included, is
    /// `UnsupportedHeader`.
    pub(crate) fn header(&mut self) -> Result<(), TsError> {
        match self.data.get(..HEADER.len()) {
            Some(h) if h == HEADER => {
                self.pos = HEADER.len();
                Ok(())
            }
            Some(_) => Err(TsError::new(TsErrorKind::UnsupportedHeader, 0)),
            None if HEADER.starts_with(self.data) => {
                Err(TsError::new(TsErrorKind::Truncated, self.data.len()))
            }
            None => Err(TsError::new(TsErrorKind::UnsupportedHeader, 0)),
        }
    }

    pub(crate) fn peek(&self) -> Result<u8, TsError> {
        self.data
            .get(self.pos)
            .copied()
            .ok_or_else(|| self.err(TsErrorKind::Truncated))
    }

    pub(crate) fn byte(&mut self) -> Result<u8, TsError> {
        let b = self.peek()?;
        self.pos = self.pos.saturating_add(1);
        Ok(b)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], TsError> {
        let end = self
            .pos
            .checked_add(N)
            .ok_or_else(|| self.err(TsErrorKind::Truncated))?;
        let bytes = self
            .data
            .get(self.pos..end)
            .ok_or_else(|| self.err(TsErrorKind::Truncated))?;
        let mut out = [0u8; N];
        for (o, b) in out.iter_mut().zip(bytes) {
            *o = *b;
        }
        self.pos = end;
        Ok(out)
    }

    /// Skip `n` bytes and return their span. `n` is checked against the
    /// bytes remaining before anything else happens.
    pub(crate) fn take(&mut self, n: u64) -> Result<Span, TsError> {
        let n = usize::try_from(n).map_err(|_| self.err(TsErrorKind::Truncated))?;
        if n > self.remaining() {
            return Err(self.err(TsErrorKind::Truncated));
        }
        let start = self.pos;
        let end = start.saturating_add(n);
        self.pos = end;
        Ok(Span {
            start: u32::try_from(start).map_err(|_| self.err(TsErrorKind::Budget))?,
            end: u32::try_from(end).map_err(|_| self.err(TsErrorKind::Budget))?,
        })
    }

    /// An integer of a signed type, after its head byte was read.
    pub(crate) fn signed(&mut self, head: u8) -> Result<i64, TsError> {
        match head {
            INT16 => Ok(i64::from(i16::from_le_bytes(self.array()?))),
            INT32 => Ok(i64::from(i32::from_le_bytes(self.array()?))),
            INT64 => Ok(i64::from_le_bytes(self.array()?)),
            h if h <= 0x7f || h >= 0x92 => Ok(i64::from(i8::from_le_bytes([h]))),
            _ => Err(TsError::new(
                TsErrorKind::UnexpectedTag,
                self.pos.saturating_sub(1),
            )),
        }
    }

    /// An integer of an unsigned type, after its head byte was read.
    pub(crate) fn unsigned(&mut self, head: u8) -> Result<u64, TsError> {
        match head {
            INT16 => Ok(u64::from(u16::from_le_bytes(self.array()?))),
            INT32 => Ok(u64::from(u32::from_le_bytes(self.array()?))),
            INT64 => Ok(u64::from_le_bytes(self.array()?)),
            h if h <= 0x7f || h >= 0x92 => Ok(u64::from(h)),
            _ => Err(TsError::new(
                TsErrorKind::UnexpectedTag,
                self.pos.saturating_sub(1),
            )),
        }
    }

    /// A float (`double == false`) or double. `0x83` introduces IEEE bytes;
    /// an integral value may instead use any signed integer form.
    pub(crate) fn real(&mut self, head: u8, double: bool) -> Result<f64, TsError> {
        if head == REAL {
            if double {
                Ok(f64::from_le_bytes(self.array()?))
            } else {
                Ok(f64::from(f32::from_le_bytes(self.array()?)))
            }
        } else {
            // An integral value written as an integer; i64 -> f64 may round
            // above 2^53.
            Ok(self.signed(head)? as f64)
        }
    }

    /// An unsigned length (of `+` bytes or a shared string).
    pub(crate) fn length(&mut self) -> Result<u64, TsError> {
        let head = self.byte()?;
        self.unsigned(head)
    }
}

/// The table index a reference head encodes, or `None` when it is negative
/// or does not fit.
pub(crate) fn ref_index(value: i64) -> Option<u32> {
    value
        .checked_add(REF_BIAS)
        .and_then(|v| u32::try_from(v).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signed(bytes: &[u8]) -> Result<i64, TsErrorKind> {
        let mut r = Reader::new(bytes);
        let h = r.byte().map_err(|e| e.kind())?;
        r.signed(h).map_err(|e| e.kind())
    }

    fn unsigned(bytes: &[u8]) -> Result<u64, TsErrorKind> {
        let mut r = Reader::new(bytes);
        let h = r.byte().map_err(|e| e.kind())?;
        r.unsigned(h).map_err(|e| e.kind())
    }

    #[test]
    fn integer_heads() {
        assert_eq!(signed(&[0x00]), Ok(0));
        assert_eq!(signed(&[0x7f]), Ok(127));
        assert_eq!(signed(&[0x92]), Ok(-110));
        assert_eq!(signed(&[0xff]), Ok(-1));
        assert_eq!(signed(&[0x81, 0x88, 0xff]), Ok(-120));
        assert_eq!(signed(&[0x82, 0, 0, 0x10, 0]), Ok(1 << 20));
        assert_eq!(
            signed(&[0x87, 0x00, 0xe6, 0x8e, 0xe7, 0xfd, 0xff, 0xff, 0xff]),
            Ok(-9_000_000_000)
        );
        assert_eq!(unsigned(&[0xc8]), Ok(200));
        assert_eq!(unsigned(&[0x81, 0xc8, 0x00]), Ok(200));
        assert_eq!(
            unsigned(&[0x87, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]),
            Ok(u64::MAX)
        );
        for h in [0x80u8, 0x83, 0x84, 0x85, 0x86, 0x88, 0x91] {
            assert_eq!(
                signed(&[h, 0, 0, 0, 0, 0, 0, 0, 0]),
                Err(TsErrorKind::UnexpectedTag)
            );
            assert_eq!(
                unsigned(&[h, 0, 0, 0, 0, 0, 0, 0, 0]),
                Err(TsErrorKind::UnexpectedTag)
            );
        }
        assert_eq!(signed(&[0x81, 0x00]), Err(TsErrorKind::Truncated));
        assert_eq!(signed(&[]), Err(TsErrorKind::Truncated));
    }

    #[test]
    fn reals_and_references() {
        let mut r = Reader::new(&[0x83, 0x00, 0x00, 0x60, 0x40]);
        let h = r.byte().unwrap_or(0);
        assert_eq!(r.real(h, false).ok(), Some(3.5));
        let mut r = Reader::new(&[0x81, 0xe8, 0x03]);
        let h = r.byte().unwrap_or(0);
        assert_eq!(r.real(h, true).ok(), Some(1000.0));
        assert_eq!(ref_index(-110), Some(0));
        assert_eq!(ref_index(10), Some(120));
        assert_eq!(ref_index(-111), None);
    }

    #[test]
    fn header_prefix_is_truncated_anything_else_unsupported() {
        assert_eq!(Reader::new(&HEADER).header(), Ok(()));
        let short = HEADER.get(..7).unwrap_or_default();
        assert_eq!(
            Reader::new(short).header().map_err(|e| e.kind()),
            Err(TsErrorKind::Truncated)
        );
        assert_eq!(
            Reader::new(&[]).header().map_err(|e| e.kind()),
            Err(TsErrorKind::Truncated)
        );
        let mut be = HEADER;
        be[2] = b't';
        assert_eq!(
            Reader::new(&be).header().map_err(|e| e.kind()),
            Err(TsErrorKind::UnsupportedHeader)
        );
        assert_eq!(
            Reader::new(&[5]).header().map_err(|e| e.kind()),
            Err(TsErrorKind::UnsupportedHeader)
        );
    }

    #[test]
    fn take_checks_the_remaining_bytes_first() {
        let mut r = Reader::new(&[1, 2, 3]);
        assert_eq!(
            r.take(1u64 << 40).map_err(|e| e.kind()),
            Err(TsErrorKind::Truncated)
        );
        assert_eq!(r.take(4).map_err(|e| e.kind()), Err(TsErrorKind::Truncated));
        assert_eq!(r.take(3).map(Span::len), Ok(3));
        assert!(r.at_end());
    }
}
