//! Type strings (M3 §10.4), parsed as a grammar from Apple's type-encoding
//! table (T3).
//!
//! | Types | Element |
//! |---|---|
//! | `c i s l q` | signed integer |
//! | `C I S L Q` | unsigned integer |
//! | `f d` | float, double |
//! | `B` | one raw byte |
//! | `+` | unshared bytes, length-prefixed |
//! | `*` | `char *`: nil, a new pointer, or a reference |
//! | `:` `%` | shared string |
//! | `#` | class |
//! | `@` | object |
//! | `[Nc]` `[NC]` | N raw bytes |
//! | `{name=…}` | members written flat; the name is skipped |
//!
//! Arrays of anything but `c`/`C`, unions, bitfields, pointers and every
//! other code are `UnsupportedType`. A struct also yields a zero-width
//! [`Elem::Struct`] marker, so that an empty struct `{}` still costs a node
//! when read; struct nesting counts toward `max_depth`.
#![deny(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::unreachable,
    clippy::unimplemented,
    clippy::string_slice
)]

use super::TsErrorKind;

/// One element of a parsed type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Elem {
    Signed,
    Unsigned,
    Float,
    Double,
    Bool,
    Unshared,
    CStr,
    Shared,
    Class,
    Object,
    /// `[Nc]` / `[NC]`: N raw bytes.
    Bytes(u64),
    /// The start of a struct: no value, but one node.
    Struct,
}

/// A parsed type string: elements appended to an arena, plus the deepest
/// struct nesting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Parsed {
    pub(crate) struct_depth: u16,
}

/// Parse `src` (a whole type string), appending its elements to `out`.
/// Struct nesting deeper than `max_depth` is `TooDeep`.
pub(crate) fn parse(
    src: &[u8],
    max_depth: u16,
    out: &mut Vec<Elem>,
) -> Result<Parsed, TsErrorKind> {
    if src.is_empty() {
        return Err(TsErrorKind::UnsupportedType);
    }
    let mut p = Parser {
        src,
        pos: 0,
        max_depth,
        deepest: 0,
    };
    p.seq(0, out)?;
    if p.pos != src.len() {
        return Err(TsErrorKind::UnsupportedType);
    }
    Ok(Parsed {
        struct_depth: p.deepest,
    })
}

struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
    max_depth: u16,
    deepest: u16,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn bump(&mut self) {
        self.pos = self.pos.saturating_add(1);
    }

    /// Elements until the end of input or a closing `}`.
    fn seq(&mut self, depth: u16, out: &mut Vec<Elem>) -> Result<(), TsErrorKind> {
        while let Some(c) = self.peek() {
            if c == b'}' {
                return Ok(());
            }
            self.one(depth, out)?;
        }
        Ok(())
    }

    fn one(&mut self, depth: u16, out: &mut Vec<Elem>) -> Result<(), TsErrorKind> {
        let c = self.peek().ok_or(TsErrorKind::UnsupportedType)?;
        self.bump();
        let e = match c {
            b'c' | b'i' | b's' | b'l' | b'q' => Elem::Signed,
            b'C' | b'I' | b'S' | b'L' | b'Q' => Elem::Unsigned,
            b'f' => Elem::Float,
            b'd' => Elem::Double,
            b'B' => Elem::Bool,
            b'+' => Elem::Unshared,
            b'*' => Elem::CStr,
            b':' | b'%' => Elem::Shared,
            b'#' => Elem::Class,
            b'@' => Elem::Object,
            b'[' => self.array()?,
            b'{' => return self.structure(depth, out),
            _ => return Err(TsErrorKind::UnsupportedType),
        };
        out.push(e);
        Ok(())
    }

    /// After `[`: decimal N, then `c` or `C`, then `]`.
    fn array(&mut self) -> Result<Elem, TsErrorKind> {
        let mut n: u64 = 0;
        let mut digits = 0u32;
        while let Some(c) = self.peek().filter(u8::is_ascii_digit) {
            n = n
                .checked_mul(10)
                .and_then(|v| v.checked_add(u64::from(c.saturating_sub(b'0'))))
                .ok_or(TsErrorKind::UnsupportedType)?;
            digits = digits.saturating_add(1);
            self.bump();
        }
        if digits == 0 {
            return Err(TsErrorKind::UnsupportedType);
        }
        match self.peek() {
            Some(b'c' | b'C') => self.bump(),
            _ => return Err(TsErrorKind::UnsupportedType),
        }
        if self.peek() != Some(b']') {
            return Err(TsErrorKind::UnsupportedType);
        }
        self.bump();
        Ok(Elem::Bytes(n))
    }

    /// After `{`: a name up to `=` (members follow) or `}` (no members).
    fn structure(&mut self, depth: u16, out: &mut Vec<Elem>) -> Result<(), TsErrorKind> {
        let depth = depth.checked_add(1).ok_or(TsErrorKind::TooDeep)?;
        if depth > self.max_depth {
            return Err(TsErrorKind::TooDeep);
        }
        if depth > self.deepest {
            self.deepest = depth;
        }
        out.push(Elem::Struct);
        loop {
            match self.peek() {
                None | Some(b'{') => return Err(TsErrorKind::UnsupportedType),
                Some(b'}') => {
                    self.bump();
                    return Ok(());
                }
                Some(b'=') => {
                    self.bump();
                    break;
                }
                Some(_) => self.bump(),
            }
        }
        self.seq(depth, out)?;
        if self.peek() != Some(b'}') {
            return Err(TsErrorKind::UnsupportedType);
        }
        self.bump();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<(Vec<Elem>, u16), TsErrorKind> {
        let mut out = Vec::new();
        parse(s.as_bytes(), 64, &mut out).map(|x| (out, x.struct_depth))
    }

    #[test]
    fn grammar() {
        use Elem::*;
        assert_eq!(p("iI"), Ok((vec![Signed, Unsigned], 0)));
        assert_eq!(
            p("@+*#:%"),
            Ok((vec![Object, Unshared, CStr, Class, Shared, Shared], 0))
        );
        assert_eq!(p("fdB"), Ok((vec![Float, Double, Bool], 0)));
        assert_eq!(p("[5c][0C]"), Ok((vec![Bytes(5), Bytes(0)], 0)));
        // The struct name contains S, a type letter; it is skipped.
        assert_eq!(
            p("{_NSRange=QQ}"),
            Ok((vec![Struct, Unsigned, Unsigned], 1))
        );
        assert_eq!(p("{}"), Ok((vec![Struct], 1)));
        assert_eq!(
            p("{a={b=i}c}"),
            Ok((vec![Struct, Struct, Signed, Signed], 2))
        );
        for bad in [
            "",
            "[4i]",
            "[c]",
            "[5c",
            "[99999999999999999999c]",
            "(u=ic)",
            "^i",
            "b4",
            "v",
            "?",
            "{a=i",
            "{a",
            "i}",
            "{a{b}=i}",
            "r*",
        ] {
            assert_eq!(
                p(bad).map(|_| ()),
                Err(TsErrorKind::UnsupportedType),
                "{bad}"
            );
        }
    }

    #[test]
    fn struct_nesting_is_bounded() {
        let ok = format!("{}i{}", "{a=".repeat(64), "}".repeat(64));
        assert_eq!(p(&ok).map(|x| x.1), Ok(64));
        let deep = format!("{}i{}", "{a=".repeat(65), "}".repeat(65));
        assert_eq!(p(&deep).map(|_| ()), Err(TsErrorKind::TooDeep));
    }
}
