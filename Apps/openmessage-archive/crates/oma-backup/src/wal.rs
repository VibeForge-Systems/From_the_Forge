//! Facts about a staged database's header and its `-wal`, read with plain
//! arithmetic from the layouts in the SQLite file-format document
//! (sqlite.org/fileformat2.html, "The Database Header" and "The WAL File
//! Format"). Nothing here runs SQLite.
//!
//! These facts describe what a staged `-wal` contains. Whether SQLite then
//! applied those frames is inferred, never observed, so callers say the
//! `-wal` was *staged*, not *applied*.

use std::io::{self, Read};

/// The 16-byte magic at the start of every SQLite database file.
const SQLITE_MAGIC: &[u8; 16] = b"SQLite format 3\0";
/// WAL magic, checksums little-endian.
const WAL_MAGIC_LE: u32 = 0x377f_0682;
/// WAL magic, checksums big-endian.
const WAL_MAGIC_BE: u32 = 0x377f_0683;
/// The only WAL format version.
const WAL_VERSION: u32 = 3_007_000;
const WAL_HEADER_LEN: usize = 32;
const FRAME_HEADER_LEN: usize = 24;

/// Facts from a database's 100-byte header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct HeaderFacts {
    /// The file starts with `SQLite format 3\0`.
    pub magic_valid: bool,
    /// The page size (bytes 16–17; the value 1 means 65,536), when it is a
    /// power of two from 512 to 65,536.
    pub page_size: Option<u32>,
    /// The file-format write version (byte 18): 1 legacy, 2 WAL.
    pub write_version: Option<u8>,
    /// The file-format read version (byte 19): 1 legacy, 2 WAL.
    pub read_version: Option<u8>,
}

impl HeaderFacts {
    /// Bytes 18 and 19 are both 2: the database was last used in WAL mode.
    pub fn says_wal(&self) -> bool {
        self.magic_valid && self.write_version == Some(2) && self.read_version == Some(2)
    }
}

/// Facts about a staged `-wal`, from its header and frame checksums.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct WalFacts {
    /// The header starts with one of the two WAL magic numbers.
    pub magic_valid: bool,
    /// The magic, format version, page size and header checksum are all
    /// valid. When `false`, SQLite ignores every frame.
    pub header_valid: bool,
    /// The WAL's page size equals the database header's.
    pub page_size_matches: bool,
    /// Frames in the valid prefix up to and including its last commit frame.
    /// A valid frame has the header's salts, a non-zero page number and an
    /// intact cumulative checksum.
    pub valid_committed_frames: u64,
    /// Valid frames after the last commit frame of the valid prefix. SQLite
    /// does not use them.
    pub uncommitted_valid_frames: u64,
    /// The first complete frame that fails validation, and every complete
    /// frame after it; with an invalid header, every complete frame (when
    /// the page size is readable).
    pub frames_after_first_invalid: u64,
    /// Bytes after the last complete frame.
    pub trailing_bytes: u64,
}

fn be32(b: &[u8], at: usize) -> u32 {
    let mut out = [0u8; 4];
    if let Some(s) = b.get(at..at.saturating_add(4)) {
        out.copy_from_slice(s);
    }
    u32::from_be_bytes(out)
}

/// A page size as the header stores it, validated.
fn valid_page_size(n: u32) -> Option<u32> {
    (n.is_power_of_two() && (512..=65_536).contains(&n)).then_some(n)
}

/// Read header facts from the first bytes of a database file. A file
/// shorter than the header gives `magic_valid: false` and no values.
pub(crate) fn header_facts(mut r: impl Read) -> io::Result<HeaderFacts> {
    let mut h = [0u8; 100];
    let mut filled = 0usize;
    while filled < h.len() {
        let Some(rest) = h.get_mut(filled..) else {
            break;
        };
        match r.read(rest) {
            Ok(0) => break,
            Ok(n) => filled = filled.saturating_add(n),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    if filled < h.len() || !h.starts_with(SQLITE_MAGIC) {
        return Ok(HeaderFacts {
            magic_valid: false,
            page_size: None,
            write_version: None,
            read_version: None,
        });
    }
    let raw = u16::from_be_bytes([h[16], h[17]]);
    let page_size = if raw == 1 {
        Some(65_536)
    } else {
        valid_page_size(u32::from(raw))
    };
    Ok(HeaderFacts {
        magic_valid: true,
        page_size,
        write_version: Some(h[18]),
        read_version: Some(h[19]),
    })
}

/// The WAL checksum: `data` as pairs of 32-bit words in the WAL's byte
/// order, `s0 += x0 + s1; s1 += x1 + s0`, modulo 2^32. `data.len()` is a
/// multiple of 8 for every caller.
fn checksum(big_endian: bool, seed: (u32, u32), data: &[u8]) -> (u32, u32) {
    let (mut s0, mut s1) = seed;
    for pair in data.chunks_exact(8) {
        let (a, b) = pair.split_at(4);
        let word = |w: &[u8]| {
            let mut x = [0u8; 4];
            x.copy_from_slice(w);
            if big_endian {
                u32::from_be_bytes(x)
            } else {
                u32::from_le_bytes(x)
            }
        };
        s0 = s0.wrapping_add(word(a)).wrapping_add(s1);
        s1 = s1.wrapping_add(word(b)).wrapping_add(s0);
    }
    (s0, s1)
}

/// Fill `buf` from `r`; returns how many bytes were read (less than
/// `buf.len()` only at end of file).
fn fill(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0usize;
    while filled < buf.len() {
        let Some(rest) = buf.get_mut(filled..) else {
            break;
        };
        match r.read(rest) {
            Ok(0) => break,
            Ok(n) => filled = filled.saturating_add(n),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

fn inc(n: &mut u64) {
    *n = n.saturating_add(1);
}

/// Stream a `-wal` and validate its header and frames. `db_page_size` is
/// the main database header's page size, when known.
pub(crate) fn wal_facts(mut r: impl Read, db_page_size: Option<u32>) -> io::Result<WalFacts> {
    let mut facts = WalFacts::default();
    let mut header = [0u8; WAL_HEADER_LEN];
    let got = fill(&mut r, &mut header)?;
    if got < WAL_HEADER_LEN {
        facts.trailing_bytes = u64::try_from(got).unwrap_or(u64::MAX);
        return Ok(facts);
    }
    let magic = be32(&header, 0);
    let big_endian = match magic {
        WAL_MAGIC_BE => Some(true),
        WAL_MAGIC_LE => Some(false),
        _ => None,
    };
    facts.magic_valid = big_endian.is_some();
    let page_size = valid_page_size(be32(&header, 8));
    facts.page_size_matches = page_size.is_some() && page_size == db_page_size;
    let Some(page_size) = page_size else {
        // Without a page size the frames cannot be found; count nothing.
        return Ok(facts);
    };
    let header_sum = big_endian.map(|be| checksum(be, (0, 0), header.get(..24).unwrap_or(&[])));
    facts.header_valid = big_endian.is_some()
        && be32(&header, 4) == WAL_VERSION
        && header_sum == Some((be32(&header, 24), be32(&header, 28)));
    let salts = header.get(16..24).unwrap_or(&[]).to_vec();

    let page = usize::try_from(page_size).map_err(|_| io::Error::other("page size"))?;
    let frame_len = page.saturating_add(FRAME_HEADER_LEN);
    let mut frame = vec![0u8; frame_len];
    let mut sum = header_sum.unwrap_or((0, 0));
    let mut valid = facts.header_valid;
    let mut valid_frames: u64 = 0;
    loop {
        let got = fill(&mut r, &mut frame)?;
        if got < frame_len {
            facts.trailing_bytes = u64::try_from(got).unwrap_or(u64::MAX);
            break;
        }
        if !valid {
            inc(&mut facts.frames_after_first_invalid);
            continue;
        }
        let be = big_endian.unwrap_or(true);
        let (fh, data) = frame.split_at(FRAME_HEADER_LEN);
        let page_no = be32(fh, 0);
        let commit_size = be32(fh, 4);
        let next = checksum(be, checksum(be, sum, fh.get(..8).unwrap_or(&[])), data);
        let ok = page_no != 0
            && fh.get(8..16) == Some(salts.as_slice())
            && next == (be32(fh, 16), be32(fh, 20));
        if !ok {
            valid = false;
            inc(&mut facts.frames_after_first_invalid);
            continue;
        }
        sum = next;
        inc(&mut valid_frames);
        if commit_size != 0 {
            facts.valid_committed_frames = valid_frames;
        }
    }
    facts.uncommitted_valid_frames = valid_frames.saturating_sub(facts.valid_committed_frames);
    Ok(facts)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: u32 = 512;

    /// A hand-built WAL: `frames` is (page number, commit size, fill byte).
    fn build(big_endian: bool, frames: &[(u32, u32, u8)]) -> Vec<u8> {
        let mut out = Vec::new();
        let magic = if big_endian {
            WAL_MAGIC_BE
        } else {
            WAL_MAGIC_LE
        };
        out.extend_from_slice(&magic.to_be_bytes());
        out.extend_from_slice(&WAL_VERSION.to_be_bytes());
        out.extend_from_slice(&PAGE.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes()); // checkpoint sequence
        out.extend_from_slice(&0x1122_3344u32.to_be_bytes()); // salt-1
        out.extend_from_slice(&0x5566_7788u32.to_be_bytes()); // salt-2
        let mut sum = checksum(big_endian, (0, 0), &out);
        out.extend_from_slice(&sum.0.to_be_bytes());
        out.extend_from_slice(&sum.1.to_be_bytes());
        for (pgno, commit, fill) in frames {
            let mut fh = Vec::new();
            fh.extend_from_slice(&pgno.to_be_bytes());
            fh.extend_from_slice(&commit.to_be_bytes());
            let data = vec![*fill; PAGE as usize];
            sum = checksum(big_endian, checksum(big_endian, sum, &fh), &data);
            fh.extend_from_slice(&0x1122_3344u32.to_be_bytes());
            fh.extend_from_slice(&0x5566_7788u32.to_be_bytes());
            fh.extend_from_slice(&sum.0.to_be_bytes());
            fh.extend_from_slice(&sum.1.to_be_bytes());
            out.extend_from_slice(&fh);
            out.extend_from_slice(&data);
        }
        out
    }

    fn frame_offset(i: usize) -> usize {
        WAL_HEADER_LEN + i * (FRAME_HEADER_LEN + PAGE as usize)
    }

    #[test]
    fn checksum_known_answer() {
        // Two words per pair: s0 = 1 + 0 = 1, s1 = 2 + 1 = 3; then
        // s0 = 1 + 3 + 3 = 7, s1 = 3 + 4 + 7 = 14.
        let data = [0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 4];
        assert_eq!(checksum(true, (0, 0), &data), (7, 14));
        let le = [1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 4, 0, 0, 0];
        assert_eq!(checksum(false, (0, 0), &le), (7, 14));
        // Wrapping arithmetic: 1 + 0xffffffff + 1 and 1 + 0xffffffff + 1.
        assert_eq!(checksum(true, (1, 1), &[0xff; 8]), (1, 1));
    }

    #[test]
    fn hand_built_wal_files() {
        for be in [true, false] {
            // Two transactions: frames 1-2 (commit at 2) and 3 (commit), then
            // one uncommitted frame.
            let wal = build(be, &[(1, 0, 1), (2, 2, 2), (3, 3, 3), (4, 0, 4)]);
            let f = wal_facts(wal.as_slice(), Some(PAGE)).expect("facts");
            assert!(f.magic_valid && f.header_valid && f.page_size_matches);
            assert_eq!(f.valid_committed_frames, 3);
            assert_eq!(f.uncommitted_valid_frames, 1);
            assert_eq!(f.frames_after_first_invalid, 0);
            assert_eq!(f.trailing_bytes, 0);

            // A flipped byte in the third frame's page invalidates it and
            // everything after.
            let mut bad = wal.clone();
            bad[frame_offset(2) + FRAME_HEADER_LEN + 7] ^= 1;
            let f = wal_facts(bad.as_slice(), Some(PAGE)).expect("facts");
            assert_eq!(f.valid_committed_frames, 2);
            assert_eq!(f.uncommitted_valid_frames, 0);
            assert_eq!(f.frames_after_first_invalid, 2);

            // A wrong salt in the second frame.
            let mut salt = wal.clone();
            salt[frame_offset(1) + 8] ^= 0x80;
            let f = wal_facts(salt.as_slice(), Some(PAGE)).expect("facts");
            assert_eq!(f.valid_committed_frames, 0);
            assert_eq!(f.uncommitted_valid_frames, 1);
            assert_eq!(f.frames_after_first_invalid, 3);

            // A truncated last frame.
            let cut = &wal[..wal.len() - 10];
            let f = wal_facts(cut, Some(PAGE)).expect("facts");
            assert_eq!(f.valid_committed_frames, 3);
            assert_eq!(f.uncommitted_valid_frames, 0);
            assert_eq!(
                f.trailing_bytes,
                (FRAME_HEADER_LEN + PAGE as usize - 10) as u64
            );

            // A page-size mismatch with the database is reported, not fatal.
            let f = wal_facts(wal.as_slice(), Some(4096)).expect("facts");
            assert!(!f.page_size_matches);
            assert_eq!(f.valid_committed_frames, 3);
        }
    }

    #[test]
    fn bad_headers() {
        let wal = build(true, &[(1, 1, 1), (2, 2, 2)]);
        // Header checksum broken: every frame counts as after the first invalid.
        let mut bad = wal.clone();
        bad[12] ^= 1; // checkpoint sequence, covered by the header checksum
        let f = wal_facts(bad.as_slice(), Some(PAGE)).expect("facts");
        assert!(f.magic_valid && !f.header_valid);
        assert_eq!(f.valid_committed_frames, 0);
        assert_eq!(f.frames_after_first_invalid, 2);
        // Wrong magic.
        let mut bad = wal.clone();
        bad[3] = 0x84;
        let f = wal_facts(bad.as_slice(), Some(PAGE)).expect("facts");
        assert!(!f.magic_valid && !f.header_valid);
        assert_eq!(f.frames_after_first_invalid, 2);
        // A page size that is not a power of two: nothing can be counted.
        let mut bad = wal.clone();
        bad[8..12].copy_from_slice(&1000u32.to_be_bytes());
        let f = wal_facts(bad.as_slice(), Some(PAGE)).expect("facts");
        assert!(!f.header_valid && !f.page_size_matches);
        assert_eq!(f.frames_after_first_invalid, 0);
        // Shorter than a header.
        let f = wal_facts(&wal[..10], Some(PAGE)).expect("facts");
        assert!(!f.magic_valid);
        assert_eq!(f.trailing_bytes, 10);
        // A page number of zero is invalid.
        let zero = build(true, &[(0, 1, 1)]);
        let f = wal_facts(zero.as_slice(), Some(PAGE)).expect("facts");
        assert_eq!(f.valid_committed_frames, 0);
        assert_eq!(f.frames_after_first_invalid, 1);
    }

    #[test]
    fn database_headers() {
        let mut h = vec![0u8; 100];
        h[..16].copy_from_slice(SQLITE_MAGIC);
        h[16..18].copy_from_slice(&4096u16.to_be_bytes());
        h[18] = 2;
        h[19] = 2;
        let f = header_facts(h.as_slice()).expect("facts");
        assert!(f.magic_valid && f.says_wal());
        assert_eq!(f.page_size, Some(4096));
        h[16..18].copy_from_slice(&1u16.to_be_bytes());
        h[18] = 1;
        let f = header_facts(h.as_slice()).expect("facts");
        assert_eq!(f.page_size, Some(65_536));
        assert!(!f.says_wal());
        h[16..18].copy_from_slice(&1000u16.to_be_bytes());
        assert_eq!(header_facts(h.as_slice()).expect("facts").page_size, None);
        assert!(!header_facts(&h[..50]).expect("facts").magic_valid);
        h[0] = b'X';
        assert!(!header_facts(h.as_slice()).expect("facts").magic_valid);
    }
}
