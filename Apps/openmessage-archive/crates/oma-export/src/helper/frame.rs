//! The export-render helper's frames (M6 spec §5.7, D133).
//!
//! Both directions use the same frame:
//!
//! ```text
//! "OMXF" | kind u8 | len u32 LE | payload (len bytes)
//! ```
//!
//! `len` is at most [`MAX_FRAME_BYTES`] (16 MiB) in both directions, and
//! [`read_frame`] checks it **before** it allocates anything: a larger
//! length is [`FrameError::TooLarge`], and the reader stops there (the host
//! kills the helper, `Limit(FrameTooLarge)`; the helper exits). A kind that
//! does not belong to the reading side's direction, or a wrong magic, is
//! [`FrameError::BadFrame`].
//!
//! **Requests** (the main process to the helper), in this order:
//!
//! ```text
//! Begin  Resource*  (Conversation | Item | Image)*  EndVolume  (… EndVolume)*  [End]
//! ```
//!
//! - `Begin`: `version u8 | backend u8 | options` ([`Begin`]);
//! - `Resource`: one piece of a resource (fonts, in slice 6j):
//!   `slot u8 | total u64 | offset u64 | bytes` ([`ResourcePiece`]);
//!   a resource larger than a frame comes in several pieces, in order;
//! - `Conversation`, `Item`: one record, already masked (`oma-archive`
//!   masks before any record exists), as opaque bytes;
//! - `Image`: `file_ordinal u64 | rendition bytes` (a decode-helper
//!   rendition the main process has already checked);
//! - `EndVolume`: finish the current output and send it; the session goes
//!   on with the next volume;
//! - `End`: finish the current output, send it, and exit.
//!
//! **Replies** (the helper to the main process): after `EndVolume` or `End`
//! only, `Chunk*` (the output's bytes, in order) then one `Done`
//! ([`DoneCounts`]); or, at any time, one `Error` ([`HelperErrorCode`]),
//! after which the helper exits.
//!
//! No archive id, name, path or destination ever crosses: the records are
//! the export's own (ordinals by default), and the output is bytes.
//! `Debug` of a [`Frame`] prints its kind and length only.

use std::io::{self, Read, Write};

/// The frame magic.
pub const MAGIC: [u8; 4] = *b"OMXF";
/// The fixed part of a frame: the magic, the kind and the length.
pub const HEADER_BYTES: usize = 4 + 1 + 4;
/// The largest payload of one frame, in both directions (§5.7).
pub const MAX_FRAME_BYTES: u32 = 16 << 20;
/// The protocol version a `Begin` carries.
pub const PROTOCOL_VERSION: u8 = 1;
/// The fixed part of a `Resource` payload.
pub const RESOURCE_HEADER: usize = 1 + 8 + 8;
/// The fixed part of an `Image` payload.
pub const IMAGE_HEADER: usize = 8;
/// The size of a `Done` payload.
pub const DONE_BYTES: usize = 6 * 8;
/// The most resource slots a session may fill.
pub const MAX_RESOURCE_SLOTS: u8 = 16;
/// The most resource bytes, over all slots, one session may send (the
/// owner's font budget is about 32 MB raw, owner Q2).
pub const MAX_RESOURCE_BYTES: u64 = 64 << 20;

/// What a frame is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrameKind {
    Begin,
    Resource,
    Conversation,
    Item,
    Image,
    EndVolume,
    End,
    Chunk,
    Done,
    Error,
}

/// Which side reads: the helper reads requests, the host replies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Request,
    Reply,
}

impl FrameKind {
    pub const ALL: [FrameKind; 10] = [
        FrameKind::Begin,
        FrameKind::Resource,
        FrameKind::Conversation,
        FrameKind::Item,
        FrameKind::Image,
        FrameKind::EndVolume,
        FrameKind::End,
        FrameKind::Chunk,
        FrameKind::Done,
        FrameKind::Error,
    ];

    /// The kind's byte on the wire.
    pub fn code(self) -> u8 {
        match self {
            FrameKind::Begin => 0x01,
            FrameKind::Resource => 0x02,
            FrameKind::Conversation => 0x03,
            FrameKind::Item => 0x04,
            FrameKind::Image => 0x05,
            FrameKind::EndVolume => 0x06,
            FrameKind::End => 0x07,
            FrameKind::Chunk => 0x81,
            FrameKind::Done => 0x82,
            FrameKind::Error => 0x83,
        }
    }

    /// The kind with this byte, if any.
    pub fn from_code(code: u8) -> Option<FrameKind> {
        FrameKind::ALL.into_iter().find(|k| k.code() == code)
    }

    /// The direction the kind travels in.
    pub fn direction(self) -> Direction {
        if self.code() & 0x80 == 0 {
            Direction::Request
        } else {
            Direction::Reply
        }
    }
}

/// One frame read from a pipe.
#[derive(Clone, PartialEq, Eq)]
pub struct Frame {
    pub kind: FrameKind,
    pub payload: Vec<u8>,
}

impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Frame")
            .field("kind", &self.kind)
            .field("len", &self.payload.len())
            .finish()
    }
}

/// Why a frame could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// The pipe closed before the first byte of a frame.
    Closed,
    /// The pipe closed, or failed, inside a frame.
    Truncated,
    /// A wrong magic, an unknown kind, or a kind of the other direction.
    BadFrame,
    /// The length is over [`MAX_FRAME_BYTES`]; nothing was allocated.
    TooLarge,
}

/// Write one frame. A payload over [`MAX_FRAME_BYTES`] is refused
/// (`InvalidInput`) and nothing is written.
pub fn write_frame(w: &mut dyn Write, kind: FrameKind, payload: &[u8]) -> io::Result<()> {
    let len = u32::try_from(payload.len())
        .ok()
        .filter(|l| *l <= MAX_FRAME_BYTES)
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    w.write_all(&raw_header(kind, len))?;
    w.write_all(payload)
}

/// A frame header with an arbitrary length (tests: an oversized frame).
pub fn raw_header(kind: FrameKind, len: u32) -> [u8; HEADER_BYTES] {
    let [m0, m1, m2, m3] = MAGIC;
    let [l0, l1, l2, l3] = len.to_le_bytes();
    [m0, m1, m2, m3, kind.code(), l0, l1, l2, l3]
}

/// Fill `buf`; `Ok(false)` on EOF before the first byte.
fn read_full(r: &mut dyn Read, buf: &mut [u8]) -> io::Result<bool> {
    let mut at = 0;
    while at < buf.len() {
        let Some(rest) = buf.get_mut(at..) else {
            break;
        };
        match r.read(rest) {
            Ok(0) if at == 0 => return Ok(false),
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => at += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

/// Read one frame travelling in `direction`. The length is checked before
/// the payload is allocated.
pub fn read_frame(r: &mut dyn Read, direction: Direction) -> Result<Frame, FrameError> {
    let mut head = [0u8; HEADER_BYTES];
    match read_full(r, &mut head) {
        Ok(true) => {}
        Ok(false) => return Err(FrameError::Closed),
        Err(_) => return Err(FrameError::Truncated),
    }
    let (magic, rest) = head.split_at(4);
    if magic != MAGIC {
        return Err(FrameError::BadFrame);
    }
    let (kind, len) = rest.split_at(1);
    let kind = kind
        .first()
        .and_then(|c| FrameKind::from_code(*c))
        .filter(|k| k.direction() == direction)
        .ok_or(FrameError::BadFrame)?;
    let mut l = [0u8; 4];
    l.copy_from_slice(len);
    let len = u32::from_le_bytes(l);
    if len > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge);
    }
    let len = usize::try_from(len).map_err(|_| FrameError::TooLarge)?;
    let mut payload = vec![0u8; len];
    match read_full(r, &mut payload) {
        Ok(true) => {}
        Ok(false) if len == 0 => {}
        _ => return Err(FrameError::Truncated),
    }
    Ok(Frame { kind, payload })
}

// ---------------------------------------------------------------- payloads

/// The backend a session asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackendKind {
    /// PDF layout and serialisation (`render::pdf`, slice 6j1).
    Pdf,
    /// XLSX writing (deferred; refused).
    Xlsx,
    /// The test backend (debug builds only; refused in release builds).
    Stub,
}

impl BackendKind {
    pub fn code(self) -> u8 {
        match self {
            BackendKind::Pdf => 1,
            BackendKind::Xlsx => 2,
            BackendKind::Stub => 0x7E,
        }
    }

    pub fn from_code(c: u8) -> Option<BackendKind> {
        [BackendKind::Pdf, BackendKind::Xlsx, BackendKind::Stub]
            .into_iter()
            .find(|k| k.code() == c)
    }
}

/// The `Begin` payload.
#[derive(Clone, PartialEq, Eq)]
pub struct Begin {
    pub backend: BackendKind,
    /// Backend-specific options (no content, no path).
    pub options: Vec<u8>,
}

impl std::fmt::Debug for Begin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Begin")
            .field("backend", &self.backend)
            .field("options", &self.options.len())
            .finish()
    }
}

/// Why a `Begin` was refused by the helper's parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeginRefusal {
    Malformed,
    Version,
    UnknownBackend,
}

impl Begin {
    pub fn encode(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(2 + self.options.len());
        v.push(PROTOCOL_VERSION);
        v.push(self.backend.code());
        v.extend_from_slice(&self.options);
        v
    }

    pub fn decode(p: &[u8]) -> Result<Begin, BeginRefusal> {
        let (&version, rest) = p.split_first().ok_or(BeginRefusal::Malformed)?;
        let (&backend, options) = rest.split_first().ok_or(BeginRefusal::Malformed)?;
        if version != PROTOCOL_VERSION {
            return Err(BeginRefusal::Version);
        }
        let backend = BackendKind::from_code(backend).ok_or(BeginRefusal::UnknownBackend)?;
        Ok(Begin {
            backend,
            options: options.to_vec(),
        })
    }
}

fn u64_le(b: &[u8]) -> Option<u64> {
    let mut a = [0u8; 8];
    a.copy_from_slice(b.get(..8)?);
    Some(u64::from_le_bytes(a))
}

/// One piece of a resource.
#[derive(Clone, PartialEq, Eq)]
pub struct ResourcePiece<'a> {
    pub slot: u8,
    pub total: u64,
    pub offset: u64,
    pub bytes: &'a [u8],
}

impl std::fmt::Debug for ResourcePiece<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResourcePiece")
            .field("slot", &self.slot)
            .field("total", &self.total)
            .field("offset", &self.offset)
            .field("len", &self.bytes.len())
            .finish()
    }
}

impl<'a> ResourcePiece<'a> {
    pub fn header(slot: u8, total: u64, offset: u64) -> Vec<u8> {
        let mut h = Vec::with_capacity(RESOURCE_HEADER);
        h.push(slot);
        h.extend_from_slice(&total.to_le_bytes());
        h.extend_from_slice(&offset.to_le_bytes());
        h
    }

    pub fn decode(p: &'a [u8]) -> Option<ResourcePiece<'a>> {
        let (&slot, rest) = p.split_first()?;
        let total = u64_le(rest)?;
        let offset = u64_le(rest.get(8..)?)?;
        Some(ResourcePiece {
            slot,
            total,
            offset,
            bytes: rest.get(16..)?,
        })
    }
}

/// An `Image` payload's parts.
pub fn decode_image(p: &[u8]) -> Option<(u64, &[u8])> {
    Some((u64_le(p)?, p.get(IMAGE_HEADER..)?))
}

/// What a finished output reports (`Done`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DoneCounts {
    /// Pages (PDF) or rows (XLSX).
    pub units: u64,
    /// Characters no bundled font could draw (`.notdef`).
    pub notdef_chars: u64,
    /// Unbroken tokens wrapped at an arbitrary point.
    pub wrapped_tokens: u64,
    /// Combining runs that were limited.
    pub combining_runs: u64,
    /// Preview images drawn (PDF).
    pub figures: u64,
    /// The output's length in bytes: the sum of its chunks.
    pub total_bytes: u64,
}

impl DoneCounts {
    pub fn encode(&self) -> Vec<u8> {
        [
            self.units,
            self.notdef_chars,
            self.wrapped_tokens,
            self.combining_runs,
            self.figures,
            self.total_bytes,
        ]
        .iter()
        .flat_map(|n| n.to_le_bytes())
        .collect()
    }

    pub fn decode(p: &[u8]) -> Option<DoneCounts> {
        if p.len() != DONE_BYTES {
            return None;
        }
        let at = |i: usize| p.get(i * 8..).and_then(u64_le);
        Some(DoneCounts {
            units: at(0)?,
            notdef_chars: at(1)?,
            wrapped_tokens: at(2)?,
            combining_runs: at(3)?,
            figures: at(4)?,
            total_bytes: at(5)?,
        })
    }
}

/// Why the helper refused (an `Error` frame). A closed set: no text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HelperErrorCode {
    /// The backend is not in this build (XLSX, the
    /// stub in release builds).
    Unsupported,
    /// A request out of order, or a payload the helper could not parse.
    BadRequest,
    /// The resources are over [`MAX_RESOURCE_SLOTS`] or
    /// [`MAX_RESOURCE_BYTES`], or a piece does not continue its slot.
    ResourceLimit,
    /// The helper's own writes found no space (XLSX temporary files).
    NoSpace,
    /// The output would be larger than the helper may build.
    OutputLimit,
    /// Anything else the backend could not do.
    Internal,
}

impl HelperErrorCode {
    pub const ALL: [HelperErrorCode; 6] = [
        HelperErrorCode::Unsupported,
        HelperErrorCode::BadRequest,
        HelperErrorCode::ResourceLimit,
        HelperErrorCode::NoSpace,
        HelperErrorCode::OutputLimit,
        HelperErrorCode::Internal,
    ];

    pub fn code(self) -> u8 {
        match self {
            HelperErrorCode::Unsupported => 1,
            HelperErrorCode::BadRequest => 2,
            HelperErrorCode::ResourceLimit => 3,
            HelperErrorCode::NoSpace => 4,
            HelperErrorCode::OutputLimit => 5,
            HelperErrorCode::Internal => 6,
        }
    }

    pub fn from_code(c: u8) -> Option<HelperErrorCode> {
        HelperErrorCode::ALL.into_iter().find(|k| k.code() == c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(kind: FrameKind, payload: &[u8]) -> Result<Frame, FrameError> {
        let mut v = Vec::new();
        write_frame(&mut v, kind, payload).expect("write");
        assert_eq!(v.len(), HEADER_BYTES + payload.len());
        read_frame(&mut v.as_slice(), kind.direction())
    }

    #[test]
    fn frames_round_trip_in_their_own_direction_only() {
        for kind in FrameKind::ALL {
            let f = roundtrip(kind, b"abc").expect("frame");
            assert_eq!(f.kind, kind);
            assert_eq!(f.payload, b"abc");
            let mut v = Vec::new();
            write_frame(&mut v, kind, b"").expect("write");
            let other = match kind.direction() {
                Direction::Request => Direction::Reply,
                Direction::Reply => Direction::Request,
            };
            assert_eq!(
                read_frame(&mut v.as_slice(), other),
                Err(FrameError::BadFrame)
            );
        }
        assert_eq!(roundtrip(FrameKind::Item, b"").expect("empty").payload, b"");
    }

    /// A reader that panics if more than the header is read: the length
    /// must be refused before the payload is touched or allocated.
    struct HeaderOnly(Vec<u8>, usize);

    impl Read for HeaderOnly {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            assert!(self.1 < HEADER_BYTES, "read past the header");
            let n = buf.len().min(self.0.len() - self.1);
            buf[..n].copy_from_slice(&self.0[self.1..self.1 + n]);
            self.1 += n;
            Ok(n)
        }
    }

    #[test]
    fn an_oversized_length_is_refused_before_the_payload_is_read() {
        for len in [MAX_FRAME_BYTES + 1, u32::MAX] {
            let mut r = HeaderOnly(raw_header(FrameKind::Chunk, len).to_vec(), 0);
            assert_eq!(
                read_frame(&mut r, Direction::Reply),
                Err(FrameError::TooLarge)
            );
        }
        let mut v = Vec::new();
        let big = vec![0u8; MAX_FRAME_BYTES as usize + 1];
        let e = write_frame(&mut v, FrameKind::Item, &big).expect_err("refused");
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
        assert!(v.is_empty());
        // Exactly the bound is accepted.
        let at = vec![7u8; MAX_FRAME_BYTES as usize];
        assert_eq!(
            roundtrip(FrameKind::Chunk, &at).expect("max").payload.len(),
            at.len()
        );
    }

    #[test]
    fn garbage_closed_and_truncated_pipes_are_told_apart() {
        assert_eq!(
            read_frame(&mut &b""[..], Direction::Reply),
            Err(FrameError::Closed)
        );
        assert_eq!(
            read_frame(&mut &b"OMX"[..], Direction::Reply),
            Err(FrameError::Truncated)
        );
        assert_eq!(
            read_frame(&mut &b"not a frame at all"[..], Direction::Reply),
            Err(FrameError::BadFrame)
        );
        let mut unknown = raw_header(FrameKind::Chunk, 0);
        unknown[4] = 0x99;
        assert_eq!(
            read_frame(&mut &unknown[..], Direction::Reply),
            Err(FrameError::BadFrame)
        );
        let mut cut = raw_header(FrameKind::Chunk, 10).to_vec();
        cut.extend_from_slice(b"12345");
        assert_eq!(
            read_frame(&mut cut.as_slice(), Direction::Reply),
            Err(FrameError::Truncated)
        );
    }

    #[test]
    fn payloads_round_trip() {
        let b = Begin {
            backend: BackendKind::Stub,
            options: vec![1, 2, 3],
        };
        assert_eq!(Begin::decode(&b.encode()), Ok(b.clone()));
        let mut bad = b.encode();
        bad[0] = 9;
        assert_eq!(Begin::decode(&bad), Err(BeginRefusal::Version));
        bad[0] = PROTOCOL_VERSION;
        bad[1] = 0x55;
        assert_eq!(Begin::decode(&bad), Err(BeginRefusal::UnknownBackend));
        assert_eq!(Begin::decode(&[1]), Err(BeginRefusal::Malformed));

        let mut p = ResourcePiece::header(3, 10, 4);
        p.extend_from_slice(b"abcd");
        let r = ResourcePiece::decode(&p).expect("piece");
        assert_eq!(
            (r.slot, r.total, r.offset, r.bytes),
            (3, 10, 4, &b"abcd"[..])
        );
        assert!(ResourcePiece::decode(&p[..10]).is_none());

        let mut img = 42u64.to_le_bytes().to_vec();
        img.extend_from_slice(b"\x89PNG");
        assert_eq!(decode_image(&img), Some((42, &b"\x89PNG"[..])));
        assert!(decode_image(b"short").is_none());

        let d = DoneCounts {
            units: 1,
            notdef_chars: 2,
            wrapped_tokens: 3,
            combining_runs: 4,
            figures: 6,
            total_bytes: 5,
        };
        assert_eq!(DoneCounts::decode(&d.encode()), Some(d));
        assert!(DoneCounts::decode(&[0; 47]).is_none());

        for c in HelperErrorCode::ALL {
            assert_eq!(HelperErrorCode::from_code(c.code()), Some(c));
        }
        assert!(HelperErrorCode::from_code(0).is_none());
    }

    #[test]
    fn debug_prints_kinds_and_lengths_only() {
        let f = Frame {
            kind: FrameKind::Item,
            payload: b"SENTINEL-CONTENT".to_vec(),
        };
        let s = format!("{f:?}");
        assert!(!s.contains("SENTINEL"), "{s}");
        let p = ResourcePiece {
            slot: 0,
            total: 4,
            offset: 0,
            bytes: b"SENT",
        };
        assert!(!format!("{p:?}").contains("SENT\""));
    }
}
