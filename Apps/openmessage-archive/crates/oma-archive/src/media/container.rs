//! The structural check before streaming (M5 spec §5.3).
//!
//! It runs after `verify_blob`, over the verified file read through
//! `open_range`, and decides whether the original bytes may be handed to the
//! web engine's media stack. It reads box and chunk **headers** only, never
//! the media data, so its cost does not grow with the file's size. It is
//! bounded to [`MAX_BOXES`] boxes or chunks, a nesting depth of
//! [`MAX_DEPTH`], and a deadline (2 s in [`crate::ArchiveReader::check_container`]),
//! and it stops on the cancel token.
//!
//! - **ISO-BMFF and QuickTime** (MP4, M4V, MOV, M4A): the box tree is
//!   walked through the container boxes ([`CONTAINER_BOXES`]). Every box
//!   must fit inside its parent and the file; a 64-bit size (`size == 1`)
//!   is read and checked for overflow; `size == 0` ("to the end") is
//!   accepted at the top level only. **Reference-movie atoms** (`rmra`,
//!   `rmda`, `rdrf`, `rmdr`) are refused wherever they appear, and so is any
//!   `dref` entry (`url `, `urn `, `alis`, `rsrc`) without the
//!   self-contained flag (0x000001). A file without a top-level `moov` is
//!   refused. The atom names and the flag's meaning follow Apple's
//!   QuickTime File Format Specification as summarised in the M5 spec; that
//!   document was not re-read for this slice, so the exact semantics are
//!   **UNVERIFIED** (recorded in `docs/supported-formats.md`).
//! - **WAV (RIFF)**: the chunks are walked; every chunk must fit inside the
//!   RIFF size and the file, and a `fmt ` chunk must come before `data`.
//! - **MP3, Ogg and WebM**: no structural check
//!   ([`ContainerVerdict::NotChecked`]); the sniff checked the magic. The
//!   card says "structure not checked for this format".
//!
//! **This is not a safety guarantee.** A file that passes can still exploit
//! a flaw in the engine's demuxers and codecs; the check refuses the known
//! ways a container reaches outside itself (reference movies, external data
//! references) and malformed sizes. The same walk is stated in the
//! catalogue's model (`scripts/fixtures/make_synthetic_viewer.py`); reason
//! names match it.

use std::io;
use std::time::Instant;

use super::sniff::SniffedKind;
use crate::progress::CancelToken;

/// The most boxes (ISO-BMFF) or chunks (RIFF) one check reads.
pub const MAX_BOXES: u32 = 10_000;
/// The deepest box nesting accepted; the top level is depth 1.
pub const MAX_DEPTH: u32 = 16;

/// The boxes whose payload is a list of boxes and is walked.
pub const CONTAINER_BOXES: [&[u8; 4]; 11] = [
    b"moov", b"trak", b"mdia", b"minf", b"dinf", b"stbl", b"edts", b"udta", b"mvex", b"rmra",
    b"rmda",
];
/// Reference-movie atoms: a movie that points at other movies.
const REFERENCE_ATOMS: [&[u8; 4]; 4] = [b"rmra", b"rmda", b"rdrf", b"rmdr"];
/// `dref` entry types that may name data outside the file.
const DREF_ENTRIES: [&[u8; 4]; 4] = [b"url ", b"urn ", b"alis", b"rsrc"];
/// The `dref` entry flag "the data is in this file".
const SELF_CONTAINED: u32 = 0x00_0001;

/// Random access to the bytes being checked: a verified store file
/// ([`crate::RangeReader`]) or, in tests, memory.
pub trait ReadAt {
    /// The length of the source.
    fn size(&self) -> u64;
    /// Fill `buf` from `offset`. A read past the end is an error.
    fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()>;
}

impl ReadAt for &[u8] {
    fn size(&self) -> u64 {
        u64::try_from(self.len()).unwrap_or(u64::MAX)
    }

    fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let eof = || io::Error::from(io::ErrorKind::UnexpectedEof);
        let start = usize::try_from(offset).map_err(|_| eof())?;
        let end = start.checked_add(buf.len()).ok_or_else(eof)?;
        buf.copy_from_slice(self.get(start..end).ok_or_else(eof)?);
        Ok(())
    }
}

/// The structural check's verdict (§5.3). Cached per blob for the session
/// by the shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ContainerVerdict {
    /// The structure passed. Not a safety guarantee.
    Ok,
    /// MP3, Ogg or WebM: magic only, "structure not checked for this
    /// format".
    NotChecked,
    /// "This file can't be played in the app: its structure failed a
    /// safety check".
    Refused(ContainerRefusal),
}

/// Why the structural check refused a file (the model's reason names in
/// brackets).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ContainerRefusal {
    /// Nesting deeper than [`MAX_DEPTH`] (`tooDeep`).
    TooDeep,
    /// More than [`MAX_BOXES`] boxes or chunks (`tooManyBoxes`).
    TooManyBoxes,
    /// A box header or box runs past its parent or the file (`boxPastEnd`).
    BoxPastEnd,
    /// A 64-bit box size overflows the offset (`sizeOverflow`).
    SizeOverflow,
    /// `size == 0` below the top level (`sizeZeroNested`).
    SizeZeroNested,
    /// A box smaller than its own header (`boxTooSmall`).
    BoxTooSmall,
    /// A reference-movie atom (`referenceMovie`).
    ReferenceMovie,
    /// A `dref` entry without the self-contained flag
    /// (`externalDataReference`).
    ExternalDataReference,
    /// No top-level `moov` (`noMoov`).
    NoMoov,
    /// A RIFF chunk runs past the RIFF size or the file (`chunkPastEnd`).
    ChunkPastEnd,
    /// `data` before `fmt ` (`dataBeforeFmt`).
    DataBeforeFmt,
    /// No `fmt ` chunk (`noFmt`).
    NoFmt,
    /// The check ran past its deadline.
    TimedOut,
    /// The sniffed kind is not a playable container.
    NotPlayable,
}

/// Why a check stopped without a verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CheckStop {
    Cancelled,
    Io(io::ErrorKind),
}

enum Stop {
    Refused(ContainerRefusal),
    Check(CheckStop),
}

impl From<ContainerRefusal> for Stop {
    fn from(r: ContainerRefusal) -> Stop {
        Stop::Refused(r)
    }
}

struct Walk<'a, R: ReadAt + ?Sized> {
    src: &'a mut R,
    boxes: u32,
    cancel: Option<&'a CancelToken>,
    deadline: Instant,
}

impl<R: ReadAt + ?Sized> Walk<'_, R> {
    /// Count one box or chunk, and check the cancel token and deadline.
    fn tick(&mut self) -> Result<(), Stop> {
        self.boxes = self.boxes.saturating_add(1);
        if self.boxes > MAX_BOXES {
            return Err(ContainerRefusal::TooManyBoxes.into());
        }
        if self.cancel.is_some_and(CancelToken::is_cancelled) {
            return Err(Stop::Check(CheckStop::Cancelled));
        }
        if Instant::now() >= self.deadline {
            return Err(ContainerRefusal::TimedOut.into());
        }
        Ok(())
    }

    fn read<const N: usize>(&mut self, at: u64) -> Result<[u8; N], Stop> {
        let mut b = [0u8; N];
        self.src
            .read_exact_at(at, &mut b)
            .map_err(|e| Stop::Check(CheckStop::Io(e.kind())))?;
        Ok(b)
    }

    /// Walk the boxes in `[start, end)`; `true` when one of them is `moov`.
    fn boxes(&mut self, start: u64, end: u64, depth: u32, top: bool) -> Result<bool, Stop> {
        if depth > MAX_DEPTH {
            return Err(ContainerRefusal::TooDeep.into());
        }
        let mut pos = start;
        let mut moov = false;
        while pos < end {
            if end - pos < 8 {
                return Err(ContainerRefusal::BoxPastEnd.into());
            }
            self.tick()?;
            let (size32, typ) = split_header(self.read(pos)?);
            let mut hdr = 8u64;
            let size = match size32 {
                1 => {
                    if end - pos < 16 {
                        return Err(ContainerRefusal::BoxPastEnd.into());
                    }
                    let big = u64::from_be_bytes(self.read(pos + 8)?);
                    hdr = 16;
                    if pos.checked_add(big).is_none() {
                        return Err(ContainerRefusal::SizeOverflow.into());
                    }
                    big
                }
                0 if !top => return Err(ContainerRefusal::SizeZeroNested.into()),
                0 => end - pos,
                n => u64::from(n),
            };
            if size < hdr {
                return Err(ContainerRefusal::BoxTooSmall.into());
            }
            let Some(box_end) = pos.checked_add(size) else {
                return Err(ContainerRefusal::SizeOverflow.into());
            };
            if box_end > end {
                return Err(ContainerRefusal::BoxPastEnd.into());
            }
            if REFERENCE_ATOMS.contains(&&typ) {
                return Err(ContainerRefusal::ReferenceMovie.into());
            }
            if CONTAINER_BOXES.contains(&&typ) {
                self.boxes(pos + hdr, box_end, depth + 1, false)?;
            } else if &typ == b"dref" {
                if box_end - (pos + hdr) < 8 {
                    return Err(ContainerRefusal::BoxTooSmall.into());
                }
                // version and flags (4), entry count (4), then the entries.
                self.dref_entries(pos + hdr + 8, box_end, depth + 1)?;
            }
            moov |= &typ == b"moov";
            pos = box_end;
        }
        Ok(moov)
    }

    /// The entries of a `dref` box: each a full box (size, type, version,
    /// flags). Every entry is read before any is judged, as the model does.
    fn dref_entries(&mut self, start: u64, end: u64, depth: u32) -> Result<(), Stop> {
        if depth > MAX_DEPTH {
            return Err(ContainerRefusal::TooDeep.into());
        }
        let mut pos = start;
        let mut external = false;
        while pos < end {
            if end - pos < 12 {
                return Err(ContainerRefusal::BoxPastEnd.into());
            }
            self.tick()?;
            let [a, b, c, d, e, f, g, i, _version, f1, f2, f3]: [u8; 12] = self.read(pos)?;
            let (size, typ) = split_header([a, b, c, d, e, f, g, i]);
            let size = u64::from(size);
            if size < 12 {
                return Err(ContainerRefusal::BoxTooSmall.into());
            }
            if pos + size > end {
                return Err(ContainerRefusal::BoxPastEnd.into());
            }
            let flags = u32::from_be_bytes([0, f1, f2, f3]);
            if DREF_ENTRIES.contains(&&typ) && flags & SELF_CONTAINED == 0 {
                external = true;
            }
            pos += size;
        }
        if external {
            return Err(ContainerRefusal::ExternalDataReference.into());
        }
        Ok(())
    }

    fn riff(&mut self) -> Result<(), Stop> {
        let len = self.src.size();
        if len < 12 {
            return Err(ContainerRefusal::ChunkPastEnd.into());
        }
        let [_, _, _, _, a, b, c, d, _, _, _, _]: [u8; 12] = self.read(0)?;
        let riff = u64::from(u32::from_le_bytes([a, b, c, d]));
        let end = 8 + riff;
        if end > len {
            return Err(ContainerRefusal::ChunkPastEnd.into());
        }
        let mut pos = 12u64;
        let mut fmt = false;
        while pos < end {
            if end - pos < 8 {
                return Err(ContainerRefusal::ChunkPastEnd.into());
            }
            self.tick()?;
            let [i0, i1, i2, i3, a, b, c, d]: [u8; 8] = self.read(pos)?;
            let size = u64::from(u32::from_le_bytes([a, b, c, d]));
            if pos + 8 + size > end {
                return Err(ContainerRefusal::ChunkPastEnd.into());
            }
            match &[i0, i1, i2, i3] {
                b"fmt " => fmt = true,
                b"data" if !fmt => return Err(ContainerRefusal::DataBeforeFmt.into()),
                _ => {}
            }
            pos += 8 + size + (size & 1);
        }
        if !fmt {
            return Err(ContainerRefusal::NoFmt.into());
        }
        Ok(())
    }
}

fn split_header(h: [u8; 8]) -> (u32, [u8; 4]) {
    let [a, b, c, d, e, f, g, i] = h;
    (u32::from_be_bytes([a, b, c, d]), [e, f, g, i])
}

/// Check the structure of `src`, sniffed as `kind` (§5.3), by the deadline.
pub(crate) fn check<R: ReadAt + ?Sized>(
    src: &mut R,
    kind: SniffedKind,
    cancel: Option<&CancelToken>,
    deadline: Instant,
) -> Result<ContainerVerdict, CheckStop> {
    let len = src.size();
    let mut w = Walk {
        src,
        boxes: 0,
        cancel,
        deadline,
    };
    let walked = match kind {
        SniffedKind::Mp4 | SniffedKind::M4v | SniffedKind::QuickTime | SniffedKind::M4a => {
            w.boxes(0, len, 1, true).and_then(|moov| {
                if moov {
                    Ok(())
                } else {
                    Err(ContainerRefusal::NoMoov.into())
                }
            })
        }
        SniffedKind::Wav => w.riff(),
        SniffedKind::Mp3 | SniffedKind::Ogg | SniffedKind::Webm => {
            return Ok(ContainerVerdict::NotChecked)
        }
        _ => return Ok(ContainerVerdict::Refused(ContainerRefusal::NotPlayable)),
    };
    match walked {
        Ok(()) => Ok(ContainerVerdict::Ok),
        Err(Stop::Refused(r)) => Ok(ContainerVerdict::Refused(r)),
        Err(Stop::Check(s)) => Err(s),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn bx(typ: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut v = ((8 + payload.len()) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(typ);
        v.extend_from_slice(payload);
        v
    }

    fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
        parts.concat()
    }

    fn major_ftyp(major: &[u8; 4]) -> Vec<u8> {
        let mut p = major.to_vec();
        p.extend_from_slice(&[0, 0, 0, 0]);
        p.extend_from_slice(b"isom");
        let mut v = ((8 + p.len()) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(b"ftyp");
        v.extend_from_slice(&p);
        v
    }

    fn dref_entry(typ: &[u8; 4], flags: u32, data: &[u8]) -> Vec<u8> {
        let mut p = flags.to_be_bytes().to_vec(); // version 0, flags
        p.extend_from_slice(data);
        bx(typ, &p)
    }

    fn dref(entries: &[Vec<u8>]) -> Vec<u8> {
        let mut p = vec![0, 0, 0, 0];
        p.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        p.extend_from_slice(&entries.concat());
        bx(b"dref", &p)
    }

    fn movie(major: &[u8; 4], dref_box: Vec<u8>) -> Vec<u8> {
        let dinf = bx(b"dinf", &dref_box);
        let minf = bx(b"minf", &cat(&[dinf, bx(b"stbl", b"")]));
        let mdia = bx(b"mdia", &cat(&[bx(b"mdhd", &[0; 24]), minf]));
        let trak = bx(b"trak", &cat(&[bx(b"tkhd", &[0; 84]), mdia]));
        let moov = bx(b"moov", &cat(&[bx(b"mvhd", &[0; 100]), trak]));
        cat(&[major_ftyp(major), moov, bx(b"mdat", b"SYNTHMDT")])
    }

    fn self_contained() -> Vec<u8> {
        dref(&[dref_entry(b"url ", 1, b"")])
    }

    fn run(data: &[u8], kind: SniffedKind) -> ContainerVerdict {
        let mut src: &[u8] = data;
        check(
            &mut src,
            kind,
            None,
            Instant::now() + Duration::from_secs(10),
        )
        .expect("verdict")
    }

    fn refused(data: &[u8], kind: SniffedKind) -> ContainerRefusal {
        match run(data, kind) {
            ContainerVerdict::Refused(r) => r,
            v => panic!("expected a refusal, got {v:?}"),
        }
    }

    fn wav(chunks: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut body = b"WAVE".to_vec();
        for (id, data) in chunks {
            body.extend_from_slice(*id);
            body.extend_from_slice(&(data.len() as u32).to_le_bytes());
            body.extend_from_slice(data);
            if data.len() % 2 == 1 {
                body.push(0);
            }
        }
        let mut v = b"RIFF".to_vec();
        v.extend_from_slice(&(body.len() as u32).to_le_bytes());
        v.extend_from_slice(&body);
        v
    }

    #[test]
    fn valid_mp4_mov_m4a_and_wav_pass() {
        for (major, kind) in [
            (b"isom", SniffedKind::Mp4),
            (b"qt  ", SniffedKind::QuickTime),
            (b"M4A ", SniffedKind::M4a),
            (b"M4V ", SniffedKind::M4v),
        ] {
            assert_eq!(
                run(&movie(major, self_contained()), kind),
                ContainerVerdict::Ok,
                "{kind:?}"
            );
        }
        let ok = wav(&[(b"fmt ", vec![0; 16]), (b"data", vec![0; 5])]);
        assert_eq!(run(&ok, SniffedKind::Wav), ContainerVerdict::Ok);
        // `size == 0` (to the end) at the top level.
        let mut tail = movie(b"isom", self_contained());
        tail.extend_from_slice(&[0, 0, 0, 0]);
        tail.extend_from_slice(b"free");
        tail.extend_from_slice(b"anything");
        assert_eq!(run(&tail, SniffedKind::Mp4), ContainerVerdict::Ok);
    }

    #[test]
    fn magic_only_and_non_playable_kinds() {
        for k in [SniffedKind::Mp3, SniffedKind::Ogg, SniffedKind::Webm] {
            assert_eq!(run(b"", k), ContainerVerdict::NotChecked);
        }
        for k in [SniffedKind::Png, SniffedKind::Heif, SniffedKind::Caf] {
            assert_eq!(
                run(b"", k),
                ContainerVerdict::Refused(ContainerRefusal::NotPlayable)
            );
        }
    }

    #[test]
    fn reference_movies_and_external_data_are_refused() {
        use ContainerRefusal as R;
        let url = b"http://127.0.0.1:9/SYNTH-REF.mov\0";
        for atom in [b"rmra", b"rmda", b"rdrf", b"rmdr"] {
            let moov = bx(b"moov", &bx(atom, url));
            let data = cat(&[major_ftyp(b"qt  "), moov]);
            assert_eq!(refused(&data, SniffedKind::QuickTime), R::ReferenceMovie);
        }
        // rdrf inside rmda inside rmra.
        let nested = bx(b"rmra", &bx(b"rmda", &bx(b"rdrf", url)));
        let data = cat(&[major_ftyp(b"qt  "), bx(b"moov", &nested)]);
        assert_eq!(refused(&data, SniffedKind::QuickTime), R::ReferenceMovie);
        // Every external dref entry type without the flag.
        for typ in [b"url ", b"urn ", b"alis", b"rsrc"] {
            let d = dref(&[dref_entry(typ, 0, url)]);
            assert_eq!(
                refused(&movie(b"qt  ", d), SniffedKind::QuickTime),
                R::ExternalDataReference,
                "{typ:?}"
            );
        }
        // A self-contained entry beside an external one.
        let d = dref(&[dref_entry(b"url ", 1, b""), dref_entry(b"url ", 2, url)]);
        assert_eq!(
            refused(&movie(b"isom", d), SniffedKind::Mp4),
            R::ExternalDataReference
        );
        // An unknown entry type is not judged.
        let d = dref(&[dref_entry(b"cios", 0, b"")]);
        assert_eq!(
            run(&movie(b"isom", d), SniffedKind::Mp4),
            ContainerVerdict::Ok
        );
    }

    #[test]
    fn malformed_sizes_are_refused() {
        use ContainerRefusal as R;
        // A box size past the end of the file.
        let mut past = movie(b"isom", self_contained());
        let at = major_ftyp(b"isom").len();
        past[at..at + 4].copy_from_slice(&4096u32.to_be_bytes());
        assert_eq!(refused(&past, SniffedKind::Mp4), R::BoxPastEnd);
        // A 64-bit size that overflows the offset.
        let mut big = movie(b"isom", self_contained());
        big.extend_from_slice(&1u32.to_be_bytes());
        big.extend_from_slice(b"mdat");
        big.extend_from_slice(&u64::MAX.to_be_bytes());
        big.extend_from_slice(b"SYNTHMDT");
        assert_eq!(refused(&big, SniffedKind::Mp4), R::SizeOverflow);
        // A 64-bit size smaller than its header.
        let mut small = movie(b"isom", self_contained());
        small.extend_from_slice(&1u32.to_be_bytes());
        small.extend_from_slice(b"free");
        small.extend_from_slice(&8u64.to_be_bytes());
        assert_eq!(refused(&small, SniffedKind::Mp4), R::BoxTooSmall);
        // A header cut short.
        let mut cut = movie(b"isom", self_contained());
        cut.extend_from_slice(&[0, 0, 0]);
        assert_eq!(refused(&cut, SniffedKind::Mp4), R::BoxPastEnd);
        // size 0 inside a container.
        let zero = cat(&[
            major_ftyp(b"isom"),
            bx(b"moov", &[0, 0, 0, 0, b'f', b'r', b'e', b'e']),
        ]);
        assert_eq!(refused(&zero, SniffedKind::Mp4), R::SizeZeroNested);
        // A box smaller than its header.
        let tiny = cat(&[
            major_ftyp(b"isom"),
            vec![0, 0, 0, 4, b'f', b'r', b'e', b'e'],
        ]);
        assert_eq!(refused(&tiny, SniffedKind::Mp4), R::BoxTooSmall);
        // No moov.
        let none = cat(&[major_ftyp(b"isom"), bx(b"mdat", b"x")]);
        assert_eq!(refused(&none, SniffedKind::Mp4), R::NoMoov);
        // A dref entry past its box.
        let mut d = dref(&[dref_entry(b"url ", 1, b"")]);
        let n = d.len();
        d[n - 12..n - 8].copy_from_slice(&64u32.to_be_bytes());
        assert_eq!(refused(&movie(b"isom", d), SniffedKind::Mp4), R::BoxPastEnd);
    }

    #[test]
    fn depth_and_box_count_limits() {
        use ContainerRefusal as R;
        // A leaf box at depth 16 passes; a container at depth 16, whose
        // payload would be walked at depth 17, is refused (as in the
        // model, even when that payload is empty).
        let nest = |containers: usize, leaf: bool| {
            let mut inner = if leaf { bx(b"free", b"") } else { Vec::new() };
            for _ in 1..containers {
                inner = bx(b"udta", &inner);
            }
            cat(&[major_ftyp(b"isom"), bx(b"moov", &inner)])
        };
        let deepest = MAX_DEPTH as usize - 1;
        assert_eq!(
            run(&nest(deepest, true), SniffedKind::Mp4),
            ContainerVerdict::Ok
        );
        assert_eq!(
            refused(&nest(deepest + 1, false), SniffedKind::Mp4),
            R::TooDeep
        );
        // 10,000 boxes pass; 10,001 are refused.
        let many = |n: usize| {
            let frees: Vec<u8> = (0..n - 2).flat_map(|_| bx(b"free", b"")).collect();
            cat(&[major_ftyp(b"isom"), bx(b"moov", b""), frees])
        };
        assert_eq!(
            run(&many(MAX_BOXES as usize), SniffedKind::Mp4),
            ContainerVerdict::Ok
        );
        assert_eq!(
            refused(&many(MAX_BOXES as usize + 1), SniffedKind::Mp4),
            R::TooManyBoxes
        );
    }

    #[test]
    fn riff_chunks() {
        use ContainerRefusal as R;
        let past = {
            let mut v = wav(&[(b"fmt ", vec![0; 16])]);
            v.extend_from_slice(b"data");
            v.extend_from_slice(&0x7fff_fff0u32.to_le_bytes());
            v.extend_from_slice(&[0; 16]);
            let riff = (v.len() - 8) as u32;
            v[4..8].copy_from_slice(&riff.to_le_bytes());
            v
        };
        assert_eq!(refused(&past, SniffedKind::Wav), R::ChunkPastEnd);
        let mut riff_long = wav(&[(b"fmt ", vec![0; 16])]);
        riff_long[4..8].copy_from_slice(&1000u32.to_le_bytes());
        assert_eq!(refused(&riff_long, SniffedKind::Wav), R::ChunkPastEnd);
        assert_eq!(
            refused(b"RIFF\x04\0\0\0WAV", SniffedKind::Wav),
            R::ChunkPastEnd
        );
        let before = wav(&[(b"data", vec![0; 4]), (b"fmt ", vec![0; 16])]);
        assert_eq!(refused(&before, SniffedKind::Wav), R::DataBeforeFmt);
        let nofmt = wav(&[(b"LIST", vec![0; 3])]);
        assert_eq!(refused(&nofmt, SniffedKind::Wav), R::NoFmt);
        // Odd-sized chunks are padded.
        let odd = wav(&[
            (b"fmt ", vec![0; 16]),
            (b"LIST", vec![1; 3]),
            (b"data", vec![0; 2]),
        ]);
        assert_eq!(run(&odd, SniffedKind::Wav), ContainerVerdict::Ok);
    }

    /// A 3 GiB source that is never allocated: a movie header followed by
    /// an `mdat` that claims the rest. Only headers are read.
    struct Huge {
        head: Vec<u8>,
        len: u64,
        reads: u32,
    }

    impl ReadAt for Huge {
        fn size(&self) -> u64 {
            self.len
        }
        fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
            self.reads += 1;
            let mut head: &[u8] = &self.head;
            head.read_exact_at(offset, buf)
        }
    }

    #[test]
    fn a_three_gib_file_reads_headers_only() {
        let mut head = movie(b"isom", self_contained());
        head.truncate(head.len() - 16); // drop the small mdat
        let len = 3u64 << 30;
        let rest = len - head.len() as u64;
        head.extend_from_slice(&1u32.to_be_bytes());
        head.extend_from_slice(b"mdat");
        head.extend_from_slice(&rest.to_be_bytes());
        let mut src = Huge {
            head,
            len,
            reads: 0,
        };
        let v = check(
            &mut src,
            SniffedKind::Mp4,
            None,
            Instant::now() + Duration::from_secs(10),
        )
        .expect("verdict");
        assert_eq!(v, ContainerVerdict::Ok);
        assert!(src.reads < 40, "{}", src.reads);
        // The same mdat one byte longer than the file.
        let n = src.head.len();
        src.head[n - 8..].copy_from_slice(&(rest + 1).to_be_bytes());
        let v = check(
            &mut src,
            SniffedKind::Mp4,
            None,
            Instant::now() + Duration::from_secs(10),
        )
        .expect("verdict");
        assert_eq!(v, ContainerVerdict::Refused(ContainerRefusal::BoxPastEnd));
    }

    #[test]
    fn cancel_deadline_and_read_errors() {
        let data = movie(b"isom", self_contained());
        let cancel = CancelToken::new();
        cancel.cancel();
        let mut src: &[u8] = &data;
        let e = check(
            &mut src,
            SniffedKind::Mp4,
            Some(&cancel),
            Instant::now() + Duration::from_secs(10),
        );
        assert_eq!(e, Err(CheckStop::Cancelled));
        let mut src: &[u8] = &data;
        let v = check(&mut src, SniffedKind::Mp4, None, Instant::now());
        assert_eq!(v, Ok(ContainerVerdict::Refused(ContainerRefusal::TimedOut)));
        // A source shorter than it claims.
        let mut short = Huge {
            head: data[..20].to_vec(),
            len: data.len() as u64,
            reads: 0,
        };
        let e = check(
            &mut short,
            SniffedKind::Mp4,
            None,
            Instant::now() + Duration::from_secs(10),
        );
        assert_eq!(e, Err(CheckStop::Io(io::ErrorKind::UnexpectedEof)));
    }
}
