//! The PDF output check (M6 spec §5.7, C1.17), run by the main process on
//! every PDF the export-render helper returns, before any byte of it is
//! staged.
//!
//! It is a validator, not a reader: it accepts only the shape krilla 0.8.2
//! writes for this app and refuses anything it cannot vouch for. A helper
//! that hostile text compromised controls every byte, so the check must
//! see every object a lenient reader (pdf.js, Acrobat) would load.
//!
//! 1. The file starts with `%PDF-<digit>.<digit>` and ends with `%%EOF`
//!    (trailing white space allowed).
//! 2. The whole file is tokenised. Literal and hexadecimal strings and
//!    comments are skipped, so text *inside* a string is never a name.
//!    Every **name object** is decoded (`#xx` escapes included, so
//!    `/J#53` is `/JS`) and compared with [`FORBIDDEN_NAMES`]: the spec's
//!    list (`/JavaScript`, `/JS`, `/OpenAction`, `/AA`, `/Launch`, `/URI`,
//!    `/SubmitForm`, `/ImportData`, `/GoToR`, `/GoToE`, `/EmbeddedFile`,
//!    `/EmbeddedFiles`, `/AcroForm`, `/XFA`, `/RichMedia`), `/Encrypt`
//!    (an encrypted file's strings and streams cannot be checked), and the
//!    names of every other way to attach a file, start an action or load
//!    external media: `/Annots` (krilla writes no annotation for this
//!    app; the outline uses `/Dest`), `/EF`, `/FileAttachment`,
//!    `/Filespec`, `/FS`, `/Thread`, `/Rendition`, `/Movie`, `/Sound`,
//!    `/Screen`, `/3D`, `/3DD`, `/OnInstantiate`, `/URL` and `/XRefStm`.
//!    The key `/A` (an action) is refused in every dictionary
//!    ([`FORBIDDEN_KEYS`]), so with `/OpenAction`, `/AA` and `/Annots`
//!    refused no action of any kind can be reached.
//! 3. Every stream needs a direct `/Length` whose data is followed by
//!    `endstream`, so a stream cannot hide objects from this check. Its
//!    filter must be none, `FlateDecode` or (for an image XObject)
//!    `DCTDecode`; any other filter or chain is refused, and so are
//!    `/DecodeParms`, `/DP` and the external-file keys `/F`, `/FFilter`
//!    and `/FDecodeParms` (krilla writes none of them), so the bytes the
//!    check tokenises are the bytes a reader decodes.
//! 4. Every `FlateDecode` stream is inflated with `miniz_oxide` (already
//!    in the tree through `png`), bounded per stream
//!    ([`MAX_INFLATED_STREAM`]) and per file ([`MAX_INFLATED_TOTAL`]).
//!    Every stream is tokenised with the same name check (an inline image
//!    `ID` or a nested `stream` is refused) **unless** it is one of the
//!    binary streams krilla writes: an image XObject (`/Type /XObject
//!    /Subtype /Image`, its soft mask included), a font program or
//!    `/CIDSet` (an object a `/FontFile`, `/FontFile2`, `/FontFile3` or
//!    `/CIDSet` key references, with no `/Type`), an ICC profile (an
//!    object an `[/ICCBased n 0 R]` array references, with `/N` and no
//!    `/Type` or `/Subtype`), or the XMP packet (an object a `/Metadata`
//!    key references, `/Type /Metadata /Subtype /XML`). The references
//!    are collected by a first pass. Nothing else is exempt: a stream that
//!    merely carries `/N` or `/Length1` is tokenised.
//! 5. Object streams and cross-reference streams are refused by shape,
//!    whatever their `/Type`: a stream dictionary with `/First`, `/W` or
//!    `/Index`, or `/Type /ObjStm` or `/Type /XRef`. krilla 0.8.2 with
//!    this app's settings writes neither (slice 6j1's tests).
//! 6. The cross-reference table is read: exactly one `xref` table, one
//!    `trailer` (with no `/Prev` and a `/Root`) and one `startxref`, which
//!    must point at that `xref` keyword. Every in-use entry must point
//!    exactly at an `N G obj` header the tokenising pass saw at the top
//!    level (never inside stream data) with the same number and
//!    generation, and every indirect reference in the file must name an
//!    in-use entry, so no reader is sent into repair mode to look for an
//!    object the check did not see.
//!
//! Residual risk (spec §11.1): a parser differential between this check
//! and a reader that this list does not foresee.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use miniz_oxide::inflate::{decompress_to_vec_zlib_with_limit, TINFLStatus};

/// Name objects a PDF from the helper may never contain.
pub const FORBIDDEN_NAMES: [&[u8]; 31] = [
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
    b"Annots",
    b"EF",
    b"FileAttachment",
    b"Filespec",
    b"FS",
    b"Thread",
    b"Rendition",
    b"Movie",
    b"Sound",
    b"Screen",
    b"3D",
    b"3DD",
    b"OnInstantiate",
    b"URL",
    b"XRefStm",
];

/// Dictionary keys a PDF from the helper may never use (`/A`: an action).
pub const FORBIDDEN_KEYS: [&[u8]; 1] = [b"A"];

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
    /// `DCTDecode`, or `/DecodeParms` or an external-file key.
    UnsupportedFilter,
    /// A `FlateDecode` stream did not inflate.
    Inflate,
    /// A stream inflated past [`MAX_INFLATED_STREAM`] or the file past
    /// [`MAX_INFLATED_TOTAL`].
    InflateLimit,
    /// A name on [`FORBIDDEN_NAMES`] or a key on [`FORBIDDEN_KEYS`].
    ForbiddenName,
    /// An object stream or a cross-reference stream (by shape or type).
    CompressedObjects,
    /// An inline image in a content stream.
    InlineImage,
    /// The cross-reference table, trailer or `startxref` is missing, not
    /// alone, or does not match the objects the check saw: an entry that
    /// does not point at a top-level `N G obj` header, a reference to an
    /// object the table does not list, or a `/Prev` chain.
    CrossReference,
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

/// Why a first-pass reference makes an object's stream binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// `/FontFile`, `/FontFile2`, `/FontFile3` or `/CIDSet`.
    Font,
    /// The second element of `[/ICCBased n 0 R]`.
    Icc,
    /// `/Metadata`.
    Metadata,
}

struct Lexer<'a> {
    b: &'a [u8],
    pos: usize,
    /// Where the last token returned by `next` started.
    tok_start: usize,
    /// Objects whose streams may be binary, and why.
    roles: BTreeMap<i64, Role>,
    /// Two roles for one object.
    role_conflict: bool,
    /// Every object an indirect reference names.
    refs: BTreeSet<i64>,
}

impl<'a> Lexer<'a> {
    fn new(b: &'a [u8]) -> Lexer<'a> {
        Lexer {
            b,
            pos: 0,
            tok_start: 0,
            roles: BTreeMap::new(),
            role_conflict: false,
            refs: BTreeSet::new(),
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

    fn role(&mut self, object: i64, role: Role) {
        if *self.roles.entry(object).or_insert(role) != role {
            self.role_conflict = true;
        }
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
        self.tok_start = self.pos;
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

/// What a dictionary says (only the keys the check needs; most of them
/// matter only when the dictionary is a stream's or the trailer).
#[derive(Debug, Default)]
struct DictFacts {
    length: Option<i64>,
    length_indirect: bool,
    filters: Option<Vec<Vec<u8>>>,
    filter_unusual: bool,
    kind: Option<Vec<u8>>,
    subtype: Option<Vec<u8>>,
    icc_n: bool,
    /// `/First`, `/W` or `/Index`: an object or cross-reference stream's
    /// shape.
    compressed_shape: bool,
    /// `/DecodeParms`, `/DP`, `/F`, `/FFilter` or `/FDecodeParms`.
    decode_parms: bool,
    /// `/Prev` (in the trailer: an earlier cross-reference section).
    prev: bool,
    /// `/Root`'s object.
    root: Option<i64>,
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
                (Some(Tok::Int(_)), Some(Tok::Keyword(b"R"))) => {
                    lx.refs.insert(i);
                    Val::Ref(i)
                }
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

/// The rest of an array; returns the names among its elements, and
/// records the profile of an `[/ICCBased n 0 R]` colour space.
fn parse_array(lx: &mut Lexer<'_>, depth: usize) -> Result<Vec<Vec<u8>>, PdfRefusal> {
    if depth > MAX_DEPTH {
        return Err(PdfRefusal::Malformed);
    }
    let mut names = Vec::new();
    let mut index = 0usize;
    let mut icc = false;
    loop {
        let t = lx.next()?.ok_or(PdfRefusal::Malformed)?;
        if t == Tok::ArrClose {
            return Ok(names);
        }
        match value_from(lx, t, depth)? {
            Val::Name(n) => {
                icc = index == 0 && n.as_slice() == b"ICCBased";
                names.push(n);
            }
            Val::Ref(n) if icc && index == 1 => lx.role(n, Role::Icc),
            _ => {}
        }
        index += 1;
    }
}

/// The rest of a dictionary, with the facts of its keys.
fn parse_dict(lx: &mut Lexer<'_>, depth: usize) -> Result<DictFacts, PdfRefusal> {
    if depth > MAX_DEPTH {
        return Err(PdfRefusal::Malformed);
    }
    let mut f = DictFacts::default();
    loop {
        let key = match lx.next()?.ok_or(PdfRefusal::Malformed)? {
            Tok::DictClose => return Ok(f),
            Tok::Name(k) => k.into_owned(),
            _ => return Err(PdfRefusal::Malformed),
        };
        if FORBIDDEN_KEYS.contains(&key.as_slice()) {
            return Err(PdfRefusal::ForbiddenName);
        }
        let v = parse_value(lx, depth)?;
        match (key.as_slice(), v) {
            (b"Length", Val::Int(n)) => f.length = Some(n),
            (b"Length", _) => f.length_indirect = true,
            (b"Filter", Val::Name(n)) => f.filters = Some(vec![n]),
            (b"Filter", Val::Names(v)) => f.filters = Some(v),
            (b"Filter", _) => f.filter_unusual = true,
            (b"Type", Val::Name(n)) => f.kind = Some(n),
            (b"Subtype", Val::Name(n)) => f.subtype = Some(n),
            (b"N", Val::Int(_) | Val::Ref(_)) => f.icc_n = true,
            (b"First" | b"W" | b"Index", _) => f.compressed_shape = true,
            (b"DecodeParms" | b"DP" | b"F" | b"FFilter" | b"FDecodeParms", _) => {
                f.decode_parms = true;
            }
            (b"Prev", _) => f.prev = true,
            (b"Root", Val::Ref(n)) => f.root = Some(n),
            (b"CIDSet" | b"FontFile" | b"FontFile2" | b"FontFile3", Val::Ref(n)) => {
                lx.role(n, Role::Font);
            }
            (b"Metadata", Val::Ref(n)) => lx.role(n, Role::Metadata),
            _ => {}
        }
    }
}

impl DictFacts {
    fn is_image(&self) -> bool {
        self.kind.as_deref() == Some(b"XObject") && self.subtype.as_deref() == Some(b"Image")
    }

    /// Streams whose bytes are binary data krilla writes, not PDF syntax:
    /// image XObjects, and the objects the first pass found referenced as
    /// a font program, an ICC profile or the XMP packet, each only in the
    /// shape krilla gives it.
    fn binary(&self, role: Option<Role>) -> bool {
        let sub = self.subtype.as_deref();
        let kind = self.kind.as_deref();
        self.is_image()
            || match role {
                Some(Role::Font) => {
                    kind.is_none()
                        && matches!(sub, None | Some(b"Type1C" | b"CIDFontType0C" | b"OpenType"))
                }
                Some(Role::Icc) => kind.is_none() && sub.is_none() && self.icc_n,
                Some(Role::Metadata) => kind == Some(b"Metadata") && sub == Some(b"XML"),
                None => false,
            }
    }
}

enum Filter {
    None,
    Flate,
    Dct,
}

fn filter(f: &DictFacts) -> Result<Filter, PdfRefusal> {
    if f.filter_unusual || f.decode_parms {
        return Err(PdfRefusal::UnsupportedFilter);
    }
    match f.filters.as_deref() {
        None | Some([]) => Ok(Filter::None),
        Some([one]) if one.as_slice() == b"FlateDecode" => Ok(Filter::Flate),
        Some([one]) if one.as_slice() == b"DCTDecode" && f.is_image() => Ok(Filter::Dct),
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

/// The stream whose `stream` keyword the lexer has just read. `role` is
/// `None` in the first pass, which reads no stream's data.
fn stream(
    lx: &mut Lexer<'_>,
    f: &DictFacts,
    budget: &mut usize,
    role: Option<Option<Role>>,
) -> Result<(), PdfRefusal> {
    if f.compressed_shape || matches!(f.kind.as_deref(), Some(b"ObjStm" | b"XRef")) {
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
    let Some(role) = role else {
        return Ok(());
    };
    let binary = f.binary(role);
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
    // The first pass follows the syntax without reading any stream's
    // data: it collects the objects, the binary streams' roles, every
    // reference and the cross-reference structure, which is checked
    // before the second pass inflates and tokenises the streams.
    let seen = pass(bytes, None)?;
    if seen.role_conflict {
        return Err(PdfRefusal::Malformed);
    }
    cross_reference(bytes, &seen)?;
    pass(bytes, Some(&seen.roles)).map(|_| ())
}

/// What the first pass saw.
#[derive(Default)]
struct Seen {
    roles: BTreeMap<i64, Role>,
    role_conflict: bool,
    refs: BTreeSet<i64>,
    /// Top-level `N G obj` headers: offset → (number, generation).
    headers: BTreeMap<usize, (i64, i64)>,
    /// Offsets of top-level `xref` keywords.
    xrefs: Vec<usize>,
    trailers: Vec<DictFacts>,
    startxrefs: Vec<i64>,
}

/// One pass over the file's objects; `known` is the first pass's roles
/// (`None`: the first pass, which reads no stream's data).
fn pass(bytes: &[u8], known: Option<&BTreeMap<i64, Role>>) -> Result<Seen, PdfRefusal> {
    let mut lx = Lexer::new(bytes);
    let mut seen = Seen::default();
    let mut budget = MAX_INFLATED_TOTAL;
    let mut last_dict: Option<DictFacts> = None;
    // The last two integers (and where the first began): an object's
    // number and generation before `obj`, or a reference before `R`.
    let mut ints: [Option<(i64, usize)>; 2] = [None, None];
    let mut object: Option<i64> = None;
    let mut want_trailer = false;
    let mut want_startxref = false;
    while let Some(t) = lx.next()? {
        let at = lx.tok_start;
        if (want_trailer && t != Tok::DictOpen) || (want_startxref && !matches!(t, Tok::Int(_))) {
            return Err(PdfRefusal::CrossReference);
        }
        match t {
            Tok::DictOpen => {
                let f = parse_dict(&mut lx, 1)?;
                ints = [None, None];
                if want_trailer {
                    want_trailer = false;
                    seen.trailers.push(f);
                    last_dict = None;
                } else {
                    last_dict = Some(f);
                }
                continue;
            }
            Tok::Int(n) => {
                if want_startxref {
                    want_startxref = false;
                    seen.startxrefs.push(n);
                } else {
                    ints = [ints[1], Some((n, at))];
                    last_dict = None;
                    continue;
                }
            }
            Tok::Keyword(b"obj") => {
                let (n, start, g) = match ints {
                    [Some((n, start)), Some((g, _))] => (n, start, g),
                    _ => return Err(PdfRefusal::Malformed),
                };
                seen.headers.insert(start, (n, g));
                object = Some(n);
            }
            Tok::Keyword(b"R") => match ints {
                [Some((n, _)), Some(_)] => {
                    lx.refs.insert(n);
                }
                _ => return Err(PdfRefusal::Malformed),
            },
            Tok::Keyword(b"endobj") => object = None,
            Tok::Keyword(b"xref") => seen.xrefs.push(at),
            Tok::Keyword(b"trailer") => want_trailer = true,
            Tok::Keyword(b"startxref") => want_startxref = true,
            Tok::Keyword(b"stream") => {
                let f = last_dict.take().ok_or(PdfRefusal::StreamLength)?;
                let role = known.map(|k| object.and_then(|o| k.get(&o).copied()));
                stream(&mut lx, &f, &mut budget, role)?;
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
    if want_trailer || want_startxref {
        return Err(PdfRefusal::CrossReference);
    }
    seen.roles = lx.roles;
    seen.role_conflict = lx.role_conflict;
    seen.refs = lx.refs;
    Ok(seen)
}

/// An unsigned decimal run at `p`, and the position after it.
fn digits(bytes: &[u8], p: usize, max: usize) -> Option<(u64, usize)> {
    let run = bytes
        .get(p..)?
        .iter()
        .take(max + 1)
        .take_while(|c| c.is_ascii_digit())
        .count();
    if run == 0 || run > max {
        return None;
    }
    let s = std::str::from_utf8(bytes.get(p..p + run)?).ok()?;
    Some((s.parse().ok()?, p + run))
}

/// Step 6: one classic cross-reference table at `startxref`, one trailer
/// without `/Prev`, every in-use entry at a top-level header of the same
/// object, and every reference to an in-use object.
fn cross_reference(bytes: &[u8], seen: &Seen) -> Result<(), PdfRefusal> {
    let bad = PdfRefusal::CrossReference;
    let ([xref], [trailer], [startxref]) = (
        seen.xrefs.as_slice(),
        seen.trailers.as_slice(),
        seen.startxrefs.as_slice(),
    ) else {
        return Err(bad);
    };
    if usize::try_from(*startxref).ok() != Some(*xref) || trailer.prev {
        return Err(bad);
    }
    let root = trailer.root.ok_or(bad)?;
    let mut p = xref + b"xref".len();
    let mut in_use = BTreeSet::new();
    loop {
        while bytes.get(p).copied().is_some_and(is_ws) {
            p += 1;
        }
        if bytes.get(p..).is_some_and(|r| r.starts_with(b"trailer")) {
            break;
        }
        // A subsection: `first count` on its own line.
        let (first, q) = digits(bytes, p, 10).ok_or(bad)?;
        if bytes.get(q) != Some(&b' ') {
            return Err(bad);
        }
        let (count, mut q) = digits(bytes, q + 1, 10).ok_or(bad)?;
        while bytes.get(q).is_some_and(|c| *c == b' ') {
            q += 1;
        }
        match (bytes.get(q), bytes.get(q + 1)) {
            (Some(b'\r'), Some(b'\n')) => q += 2,
            (Some(b'\r' | b'\n'), _) => q += 1,
            _ => return Err(bad),
        }
        p = q;
        for i in 0..count {
            // Exactly 20 bytes: `oooooooooo ggggg n` and a two-byte end.
            let e = bytes.get(p..p + 20).ok_or(bad)?;
            let (offset, _) = digits(e, 0, 10).filter(|(_, at)| *at == 10).ok_or(bad)?;
            let (gen, _) = digits(e, 11, 5).filter(|(_, at)| *at == 16).ok_or(bad)?;
            let ends = matches!(e.get(18..20), Some(b" \r" | b" \n" | b"\r\n"));
            if e.get(10) != Some(&b' ') || e.get(16) != Some(&b' ') || !ends {
                return Err(bad);
            }
            let number = first.checked_add(i).and_then(|n| i64::try_from(n).ok());
            match e.get(17) {
                Some(b'n') => {
                    let number = number.ok_or(bad)?;
                    let gen = i64::try_from(gen).map_err(|_| bad)?;
                    let at = usize::try_from(offset).map_err(|_| bad)?;
                    if seen.headers.get(&at) != Some(&(number, gen)) {
                        return Err(bad);
                    }
                    in_use.insert(number);
                }
                Some(b'f') => {}
                _ => return Err(bad),
            }
            p += 20;
        }
    }
    if !in_use.contains(&root) || !seen.refs.is_subset(&in_use) {
        return Err(bad);
    }
    Ok(())
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
