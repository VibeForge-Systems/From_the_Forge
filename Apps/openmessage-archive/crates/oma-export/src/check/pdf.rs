//! The PDF output check (M6 spec §5.7, C1.17), run by the main process on
//! every PDF the export-render helper returns, before any byte of it is
//! staged.
//!
//! It is a validator, not a reader: it accepts only the shape a PDF writer
//! like ours produces and refuses anything it cannot vouch for.
//!
//! 1. The file starts with `%PDF-<digit>.<digit>` and ends with `%%EOF`
//!    (trailing white space allowed).
//! 2. The whole file is tokenised. Literal and hexadecimal strings and
//!    comments are skipped, so text *inside* a string is never a name.
//!    Every **name object** is decoded (`#xx` escapes included, so
//!    `/J#53` is `/JS`) and compared with [`FORBIDDEN_NAMES`]: the spec's
//!    list (`/JavaScript`, `/JS`, `/OpenAction`, `/AA`, `/Launch`, `/URI`,
//!    `/SubmitForm`, `/ImportData`, `/GoToR`, `/GoToE`, `/EmbeddedFile`,
//!    `/EmbeddedFiles`, `/AcroForm`, `/XFA`, `/RichMedia`) plus
//!    `/Encrypt`, because an encrypted file's strings and streams cannot
//!    be checked.
//! 3. Every stream needs a direct `/Length` whose data is followed by
//!    `endstream`, so a stream cannot hide objects from this check that a
//!    reader would see. Its filter must be none, `FlateDecode` or (for an
//!    image) `DCTDecode`; any other filter or chain is refused.
//! 4. Every `FlateDecode` stream is inflated with `miniz_oxide` (already
//!    in the tree through `png`; the spec left `zip`'s or krilla's open,
//!    settled here), bounded per stream ([`MAX_INFLATED_STREAM`]) and per
//!    file ([`MAX_INFLATED_TOTAL`]). Streams that hold text-like syntax
//!    (content streams, CMaps, forms) are tokenised too, with the same name
//!    check; an inline image (`ID`) or a nested `stream` is refused.
//!    Binary streams (images, font programs, ICC profiles, XMP metadata)
//!    are inflated but not tokenised: a name-shaped byte run inside them
//!    is not a name object, and would make legitimate files fail at random.
//! 5. Compressed object and cross-reference streams (`/Type /ObjStm`,
//!    `/Type /XRef`) are refused: every object must be visible to step 2.
//!    krilla 0.8.2 with its default settings writes neither (observed in
//!    slice 6j1's tests, `tests/pdf_layout.rs`).
//! 6. A font descriptor's `/CIDSet` stream (a bitset krilla writes for PDF
//!    1.7) and its `/FontFile2` (krilla writes no `/Length1`) carry no
//!    type of their own: a first pass collects the objects that a
//!    `/CIDSet`, `/FontFile`, `/FontFile2` or `/FontFile3` key references,
//!    and their streams are inflated but not tokenised (slice 6j1). Every
//!    other stream is checked as above.
//!
//! Residual risk (spec §11.1): a parser differential between this check
//! and a reader, or an action reached through a name not on the list.

use std::borrow::Cow;
use std::collections::BTreeSet;

use miniz_oxide::inflate::{decompress_to_vec_zlib_with_limit, TINFLStatus};

/// Name objects a PDF from the helper may never contain.
pub const FORBIDDEN_NAMES: [&[u8]; 16] = [
    b"JavaScript",
    b"JS",
    b"OpenAction",
    b"AA",
    b"Launch",
    b"URI",
    b"SubmitForm",
    b"ImportData",
    b"GoToR",
    b"GoToE",
    b"EmbeddedFile",
    b"EmbeddedFiles",
    b"AcroForm",
    b"XFA",
    b"RichMedia",
    b"Encrypt",
];

/// The most bytes one stream may inflate to.
pub const MAX_INFLATED_STREAM: usize = 64 << 20;
/// The most bytes all of a file's streams may inflate to.
pub const MAX_INFLATED_TOTAL: usize = 512 << 20;
/// The deepest nesting of dictionaries and arrays accepted.
pub const MAX_DEPTH: usize = 64;

/// Why a PDF was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PdfRefusal {
    /// No `%PDF-x.y` header at the start.
    NoHeader,
    /// No `%%EOF` at the end.
    NoEof,
    /// The syntax could not be followed (an unterminated string, an
    /// unbalanced dictionary, nesting over [`MAX_DEPTH`], …).
    Malformed,
    /// A stream without a direct `/Length`, or whose data is not followed
    /// by `endstream`.
    StreamLength,
    /// A stream filter other than none, `FlateDecode` or an image's
    /// `DCTDecode`.
    UnsupportedFilter,
    /// A `FlateDecode` stream did not inflate.
    Inflate,
    /// A stream inflated past [`MAX_INFLATED_STREAM`] or the file past
    /// [`MAX_INFLATED_TOTAL`].
    InflateLimit,
    /// A name on [`FORBIDDEN_NAMES`].
    ForbiddenName,
    /// An object stream or a cross-reference stream.
    CompressedObjects,
    /// An inline image in a content stream.
    InlineImage,
}

fn is_ws(c: u8) -> bool {
    matches!(c, 0 | 9 | 10 | 12 | 13 | 32)
}

fn is_delim(c: u8) -> bool {
    matches!(
        c,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

fn is_regular(c: u8) -> bool {
    !is_ws(c) && !is_delim(c)
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok<'a> {
    Name(Cow<'a, [u8]>),
    Int(i64),
    Real,
    Str,
    DictOpen,
    DictClose,
    ArrOpen,
    ArrClose,
    Brace,
    Keyword(&'a [u8]),
}

struct Lexer<'a> {
    b: &'a [u8],
    pos: usize,
    /// Objects a font descriptor names as its `/CIDSet` (a bitset) or
    /// `/FontFile`, `/FontFile2`, `/FontFile3` (a font program).
    binary_refs: BTreeSet<i64>,
}

impl<'a> Lexer<'a> {
    fn new(b: &'a [u8]) -> Lexer<'a> {
        Lexer {
            b,
            pos: 0,
            binary_refs: BTreeSet::new(),
        }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let c = self.peek()?;
        self.pos += 1;
        Some(c)
    }

    fn skip_space(&mut self) {
        while let Some(c) = self.peek() {
            if is_ws(c) {
                self.pos += 1;
            } else if c == b'%' {
                while let Some(c) = self.peek() {
                    if c == b'\r' || c == b'\n' {
                        break;
                    }
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    fn literal_string(&mut self) -> Result<(), PdfRefusal> {
        let mut depth = 1u32;
        loop {
            match self.bump().ok_or(PdfRefusal::Malformed)? {
                b'\\' => {
                    self.bump().ok_or(PdfRefusal::Malformed)?;
                }
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(());
                    }
                }
                _ => {}
            }
        }
    }

    fn hex_string(&mut self) -> Result<(), PdfRefusal> {
        loop {
            let c = self.bump().ok_or(PdfRefusal::Malformed)?;
            if c == b'>' {
                return Ok(());
            }
            if hex(c).is_none() && !is_ws(c) {
                return Err(PdfRefusal::Malformed);
            }
        }
    }

    fn regular_run(&mut self) -> &'a [u8] {
        let start = self.pos;
        while self.peek().is_some_and(is_regular) {
            self.pos += 1;
        }
        self.b.get(start..self.pos).unwrap_or_default()
    }

    /// The next token; names on [`FORBIDDEN_NAMES`] are refused here, so
    /// every name anywhere in the file is checked.
    fn next(&mut self) -> Result<Option<Tok<'a>>, PdfRefusal> {
        self.skip_space();
        let Some(c) = self.bump() else {
            return Ok(None);
        };
        let t = match c {
            b'(' => {
                self.literal_string()?;
                Tok::Str
            }
            b'<' if self.peek() == Some(b'<') => {
                self.pos += 1;
                Tok::DictOpen
            }
            b'<' => {
                self.hex_string()?;
                Tok::Str
            }
            b'>' if self.peek() == Some(b'>') => {
                self.pos += 1;
                Tok::DictClose
            }
            b'[' => Tok::ArrOpen,
            b']' => Tok::ArrClose,
            b'{' | b'}' => Tok::Brace,
            b'/' => {
                let raw = self.regular_run();
                let name = decode_name(raw);
                if FORBIDDEN_NAMES.contains(&name.as_ref()) {
                    return Err(PdfRefusal::ForbiddenName);
                }
                Tok::Name(name)
            }
            b')' | b'>' => return Err(PdfRefusal::Malformed),
            _ => {
                self.pos -= 1;
                let run = self.regular_run();
                number(run).unwrap_or(Tok::Keyword(run))
            }
        };
        Ok(Some(t))
    }
}

/// A name's bytes with `#xx` escapes decoded (an invalid escape stays as
/// written).
fn decode_name(raw: &[u8]) -> Cow<'_, [u8]> {
    if !raw.contains(&b'#') {
        return Cow::Borrowed(raw);
    }
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while let Some(&c) = raw.get(i) {
        if c == b'#' {
            let pair = (
                raw.get(i + 1).copied().and_then(hex),
                raw.get(i + 2).copied().and_then(hex),
            );
            if let (Some(h), Some(l)) = pair {
                out.push(h << 4 | l);
                i += 3;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    Cow::Owned(out)
}

/// A regular run as a number token, if it is one.
fn number(run: &[u8]) -> Option<Tok<'static>> {
    let digits = run.strip_prefix(b"-").or_else(|| run.strip_prefix(b"+"));
    let body = digits.unwrap_or(run);
    if body.is_empty() {
        return None;
    }
    if body.iter().all(u8::is_ascii_digit) {
        let s = std::str::from_utf8(run).ok()?;
        return Some(s.parse::<i64>().map_or(Tok::Real, Tok::Int));
    }
    let dots = body.iter().filter(|c| **c == b'.').count();
    let rest_digits = body.iter().all(|c| c.is_ascii_digit() || *c == b'.');
    (dots == 1 && rest_digits && body.len() > 1).then_some(Tok::Real)
}

/// What a stream's dictionary says (only the keys the check needs).
#[derive(Debug, Default)]
struct StreamFacts {
    length: Option<i64>,
    length_indirect: bool,
    filters: Option<Vec<Vec<u8>>>,
    filter_unusual: bool,
    kind: Option<Vec<u8>>,
    subtype: Option<Vec<u8>>,
    font_lengths: bool,
    icc_n: bool,
}

enum Val {
    Name(Vec<u8>),
    Int(i64),
    /// An indirect reference, with its object number.
    Ref(i64),
    Names(Vec<Vec<u8>>),
    Other,
}

fn parse_value(lx: &mut Lexer<'_>, depth: usize) -> Result<Val, PdfRefusal> {
    let t = lx.next()?.ok_or(PdfRefusal::Malformed)?;
    value_from(lx, t, depth)
}

fn value_from<'a>(lx: &mut Lexer<'a>, t: Tok<'a>, depth: usize) -> Result<Val, PdfRefusal> {
    Ok(match t {
        Tok::Name(n) => Val::Name(n.into_owned()),
        Tok::Int(i) => {
            // `n g R` is an indirect reference.
            let at = lx.pos;
            match (lx.next()?, lx.next()?) {
                (Some(Tok::Int(_)), Some(Tok::Keyword(b"R"))) => Val::Ref(i),
                _ => {
                    lx.pos = at;
                    Val::Int(i)
                }
            }
        }
        Tok::Real | Tok::Str => Val::Other,
        Tok::Keyword(b"true" | b"false" | b"null") => Val::Other,
        Tok::DictOpen => {
            parse_dict(lx, depth + 1)?;
            Val::Other
        }
        Tok::ArrOpen => Val::Names(parse_array(lx, depth + 1)?),
        _ => return Err(PdfRefusal::Malformed),
    })
}

/// The rest of an array; returns the names among its elements.
fn parse_array(lx: &mut Lexer<'_>, depth: usize) -> Result<Vec<Vec<u8>>, PdfRefusal> {
    if depth > MAX_DEPTH {
        return Err(PdfRefusal::Malformed);
    }
    let mut names = Vec::new();
    loop {
        let t = lx.next()?.ok_or(PdfRefusal::Malformed)?;
        if t == Tok::ArrClose {
            return Ok(names);
        }
        if let Val::Name(n) = value_from(lx, t, depth)? {
            names.push(n);
        }
    }
}

/// The rest of a dictionary, with the stream facts of its keys.
fn parse_dict(lx: &mut Lexer<'_>, depth: usize) -> Result<StreamFacts, PdfRefusal> {
    if depth > MAX_DEPTH {
        return Err(PdfRefusal::Malformed);
    }
    let mut f = StreamFacts::default();
    loop {
        let key = match lx.next()?.ok_or(PdfRefusal::Malformed)? {
            Tok::DictClose => return Ok(f),
            Tok::Name(k) => k.into_owned(),
            _ => return Err(PdfRefusal::Malformed),
        };
        let v = parse_value(lx, depth)?;
        match (key.as_slice(), v) {
            (b"Length", Val::Int(n)) => f.length = Some(n),
            (b"Length", _) => f.length_indirect = true,
            (b"Filter", Val::Name(n)) => f.filters = Some(vec![n]),
            (b"Filter", Val::Names(v)) => f.filters = Some(v),
            (b"Filter", _) => f.filter_unusual = true,
            (b"Type", Val::Name(n)) => f.kind = Some(n),
            (b"Subtype", Val::Name(n)) => f.subtype = Some(n),
            (b"Length1" | b"Length2" | b"Length3", _) => f.font_lengths = true,
            (b"N", Val::Int(_) | Val::Ref(_)) => f.icc_n = true,
            (b"CIDSet" | b"FontFile" | b"FontFile2" | b"FontFile3", Val::Ref(n)) => {
                lx.binary_refs.insert(n);
            }
            _ => {}
        }
    }
}

impl StreamFacts {
    /// Streams whose bytes are binary data, not PDF syntax.
    fn binary(&self) -> bool {
        let sub = self.subtype.as_deref();
        let kind = self.kind.as_deref();
        sub == Some(b"Image")
            || self.font_lengths
            || matches!(
                sub,
                Some(b"Type1C" | b"CIDFontType0C" | b"OpenType" | b"XML")
            )
            || kind == Some(b"Metadata")
            || (self.icc_n && kind.is_none() && sub.is_none())
    }
}

enum Filter {
    None,
    Flate,
    Dct,
}

fn filter(f: &StreamFacts) -> Result<Filter, PdfRefusal> {
    if f.filter_unusual {
        return Err(PdfRefusal::UnsupportedFilter);
    }
    match f.filters.as_deref() {
        None | Some([]) => Ok(Filter::None),
        Some([one]) if one.as_slice() == b"FlateDecode" => Ok(Filter::Flate),
        Some([one]) if one.as_slice() == b"DCTDecode" && f.subtype.as_deref() == Some(b"Image") => {
            Ok(Filter::Dct)
        }
        _ => Err(PdfRefusal::UnsupportedFilter),
    }
}

/// Tokenise a stream's (inflated) syntax: the name check, no inline image
/// and no nested stream.
fn scan_syntax(data: &[u8]) -> Result<(), PdfRefusal> {
    let mut lx = Lexer::new(data);
    while let Some(t) = lx.next()? {
        match t {
            Tok::Keyword(b"ID" | b"BI") => return Err(PdfRefusal::InlineImage),
            Tok::Keyword(b"stream" | b"endstream") => return Err(PdfRefusal::Malformed),
            _ => {}
        }
    }
    Ok(())
}

/// The stream whose `stream` keyword the lexer has just read.
fn stream(
    lx: &mut Lexer<'_>,
    f: &StreamFacts,
    budget: &mut usize,
    binary_object: bool,
    scan: bool,
) -> Result<(), PdfRefusal> {
    if matches!(f.kind.as_deref(), Some(b"ObjStm" | b"XRef")) {
        return Err(PdfRefusal::CompressedObjects);
    }
    // `stream` is followed by CRLF or LF.
    match (lx.peek(), lx.b.get(lx.pos + 1).copied()) {
        (Some(b'\r'), Some(b'\n')) => lx.pos += 2,
        (Some(b'\n'), _) => lx.pos += 1,
        _ => return Err(PdfRefusal::StreamLength),
    }
    let len = match (f.length, f.length_indirect) {
        (Some(n), false) => usize::try_from(n).map_err(|_| PdfRefusal::StreamLength)?,
        _ => return Err(PdfRefusal::StreamLength),
    };
    let start = lx.pos;
    let end = start.checked_add(len).ok_or(PdfRefusal::StreamLength)?;
    let data = lx.b.get(start..end).ok_or(PdfRefusal::StreamLength)?;
    lx.pos = end;
    match (lx.peek(), lx.b.get(lx.pos + 1).copied()) {
        (Some(b'\r'), Some(b'\n')) => lx.pos += 2,
        (Some(b'\r' | b'\n'), _) => lx.pos += 1,
        _ => {}
    }
    let rest = lx.b.get(lx.pos..).unwrap_or_default();
    let ok_end = rest.starts_with(b"endstream")
        && rest.get(b"endstream".len()).is_none_or(|c| !is_regular(*c));
    if !ok_end {
        return Err(PdfRefusal::StreamLength);
    }
    lx.pos += b"endstream".len();
    let filter = filter(f)?;
    if !scan {
        return Ok(());
    }
    let binary = f.binary() || binary_object;
    match filter {
        Filter::None => {
            if !binary {
                scan_syntax(data)?;
            }
        }
        Filter::Dct => {}
        Filter::Flate => {
            let limit = MAX_INFLATED_STREAM.min(*budget);
            let inflated = decompress_to_vec_zlib_with_limit(data, limit).map_err(|e| {
                if e.status == TINFLStatus::HasMoreOutput {
                    PdfRefusal::InflateLimit
                } else {
                    PdfRefusal::Inflate
                }
            })?;
            *budget = budget.saturating_sub(inflated.len());
            if !binary {
                scan_syntax(&inflated)?;
            }
        }
    }
    Ok(())
}

/// Check a PDF from the export-render helper.
pub fn check(bytes: &[u8]) -> Result<(), PdfRefusal> {
    let header = bytes.get(..8).ok_or(PdfRefusal::NoHeader)?;
    let well_formed_header = header.starts_with(b"%PDF-")
        && header.get(5).is_some_and(u8::is_ascii_digit)
        && header.get(6) == Some(&b'.')
        && header.get(7).is_some_and(u8::is_ascii_digit);
    if !well_formed_header {
        return Err(PdfRefusal::NoHeader);
    }
    let trimmed_end = bytes.iter().rposition(|c| !is_ws(*c)).map_or(0, |i| i + 1);
    if !bytes
        .get(..trimmed_end)
        .is_some_and(|b| b.ends_with(b"%%EOF"))
    {
        return Err(PdfRefusal::NoEof);
    }
    // The first pass follows the syntax and collects the font
    // descriptors' binary objects without reading any stream's data; the second checks every
    // stream, treating those objects' streams as binary.
    let binary = pass(bytes, None)?;
    pass(bytes, Some(&binary)).map(|_| ())
}

/// One pass over the file's objects; `known` is the first pass's binary
/// objects (`None`: the first pass, which reads no stream's data).
fn pass(bytes: &[u8], known: Option<&BTreeSet<i64>>) -> Result<BTreeSet<i64>, PdfRefusal> {
    let mut lx = Lexer::new(bytes);
    let mut budget = MAX_INFLATED_TOTAL;
    let mut last_dict: Option<StreamFacts> = None;
    // The last two integers before an `obj` keyword: the object's number.
    let mut ints: [Option<i64>; 2] = [None, None];
    let mut object: Option<i64> = None;
    while let Some(t) = lx.next()? {
        match t {
            Tok::DictOpen => {
                last_dict = Some(parse_dict(&mut lx, 1)?);
                ints = [None, None];
                continue;
            }
            Tok::Int(n) => {
                ints = [ints[1], Some(n)];
                last_dict = None;
                continue;
            }
            Tok::Keyword(b"obj") => {
                object = match ints {
                    [Some(n), Some(_)] => Some(n),
                    _ => return Err(PdfRefusal::Malformed),
                };
            }
            Tok::Keyword(b"endobj") => object = None,
            Tok::Keyword(b"stream") => {
                let f = last_dict.take().ok_or(PdfRefusal::StreamLength)?;
                let binary_object = match (known, object) {
                    (Some(k), Some(o)) => k.contains(&o),
                    _ => false,
                };
                stream(&mut lx, &f, &mut budget, binary_object, known.is_some())?;
            }
            Tok::Keyword(b"endstream") => return Err(PdfRefusal::StreamLength),
            Tok::ArrOpen => {
                parse_array(&mut lx, 1)?;
            }
            Tok::DictClose | Tok::ArrClose | Tok::Brace => return Err(PdfRefusal::Malformed),
            _ => {}
        }
        ints = [None, None];
        last_dict = None;
    }
    Ok(lx.binary_refs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_decode_their_escapes() {
        assert_eq!(decode_name(b"J#53").as_ref(), b"JS");
        assert_eq!(decode_name(b"Java#53cript").as_ref(), b"JavaScript");
        assert_eq!(decode_name(b"A#4").as_ref(), b"A#4");
        assert_eq!(decode_name(b"Plain").as_ref(), b"Plain");
    }

    #[test]
    fn numbers_and_keywords_are_told_apart() {
        assert_eq!(number(b"12"), Some(Tok::Int(12)));
        assert_eq!(number(b"-3"), Some(Tok::Int(-3)));
        assert_eq!(number(b"4.5"), Some(Tok::Real));
        assert_eq!(number(b".5"), Some(Tok::Real));
        assert_eq!(number(b"obj"), None);
        assert_eq!(number(b"-"), None);
        assert_eq!(number(b"1.2.3"), None);
        assert_eq!(number(b"99999999999999999999"), Some(Tok::Real));
    }
}
