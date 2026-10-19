//! Image previews for HTML (M6 spec §5.5, §5.7, §6.5, D113, D132).
//!
//! The HTML writer never decodes an image and never reads an original
//! file. It asks a [`PreviewSource`] for an attachment's **renditions**:
//! the decode helper's re-encoded, metadata-free `thumb` (at most 640 px)
//! and, when "Larger previews" is on, `display` (at most 2048 px). The
//! source behind the export job is the decode-helper pool (slice 6h, the
//! `RenditionPair` request); tests give their own.
//!
//! **Every rendition is checked before it is embedded or written**
//! ([`check_rendition`], §5.7): a JPEG starts with SOI, ends with EOI and
//! has a frame header before its scan; a PNG has the signature, a valid
//! `IHDR` first and `IEND` last; the dimensions are within the
//! rendition's limit, and the byte length within the 16 MiB frame bound.
//! A rendition that fails is refused (`HelperOutputRejected`): the export
//! fails rather than embed bytes the checks cannot vouch for. A thumbnail
//! is embedded as a `data:` URI ([`data_uri`]), so it works however a
//! browser treats `file:` origins.
//!
//! Nothing here logs; `Debug` prints sizes only.

use crate::error::ExportError;
use crate::naming::ImageExt;
use crate::record::AttachmentRecord;

/// The longest edge of a thumbnail (§6.5).
pub const THUMB_MAX_PX: u32 = 640;
/// The longest edge of a display rendition (§6.5).
pub const DISPLAY_MAX_PX: u32 = 2048;
/// The most bytes one rendition may have: the helper protocol's frame
/// bound (§5.7).
pub const MAX_RENDITION_BYTES: usize = 16 << 20;

/// Which rendition a buffer claims to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RenditionSize {
    Thumb,
    Display,
}

impl RenditionSize {
    pub fn max_px(self) -> u32 {
        match self {
            RenditionSize::Thumb => THUMB_MAX_PX,
            RenditionSize::Display => DISPLAY_MAX_PX,
        }
    }
}

/// A rendition's bytes as a source returned them, not yet checked.
#[derive(Clone, PartialEq, Eq)]
pub struct RenditionBytes(pub Vec<u8>);

impl std::fmt::Debug for RenditionBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RenditionBytes({} bytes)", self.0.len())
    }
}

/// What a source has for one attachment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Preview {
    /// Not an image the export previews (a document, audio, a video, an
    /// unknown type): no preview card.
    None,
    /// An image whose preview could not be made (the helper failed or
    /// timed out, the input is over its limit, HEIC without the feature):
    /// the card says "Preview unavailable", and it is counted.
    Unavailable,
    /// The renditions, unchecked. `display` only when it was asked for.
    Ready {
        thumb: RenditionBytes,
        display: Option<RenditionBytes>,
    },
}

/// Where the HTML writer's previews come from (spec §6.5).
pub trait PreviewSource: Send {
    /// The renditions of attachment record `a`'s file. The writer asks
    /// only when previews are on, once per placement, and only for an
    /// attachment whose access is `included`; `display` asks for the
    /// larger rendition as well (bundles with "Larger previews" on).
    fn renditions(&mut self, a: &AttachmentRecord, display: bool) -> Result<Preview, ExportError>;
}

/// A rendition that passed [`check_rendition`].
#[derive(Clone, PartialEq, Eq)]
pub struct CheckedRendition {
    ext: ImageExt,
    width: u32,
    height: u32,
    bytes: Vec<u8>,
}

impl std::fmt::Debug for CheckedRendition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CheckedRendition")
            .field("ext", &self.ext)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

impl CheckedRendition {
    pub fn ext(&self) -> ImageExt {
        self.ext
    }
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    /// The media type of the `data:` URI.
    pub fn mime(&self) -> &'static str {
        match self.ext {
            ImageExt::Jpeg => "image/jpeg",
            ImageExt::Png => "image/png",
        }
    }
}

fn rejected() -> ExportError {
    ExportError::HelperOutputRejected
}

fn be16(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at + 2)?;
    Some(u32::from(u16::from_be_bytes([*s.first()?, *s.get(1)?])))
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at + 4)?;
    Some(u32::from_be_bytes([
        *s.first()?,
        *s.get(1)?,
        *s.get(2)?,
        *s.get(3)?,
    ]))
}

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
const PNG_IEND: [u8; 12] = [0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82];

/// The dimensions of a PNG with the signature, a valid `IHDR` as its
/// first chunk and `IEND` as its last.
fn png_size(b: &[u8]) -> Option<(u32, u32)> {
    if b.get(..8)? != PNG_SIGNATURE || be32(b, 8)? != 13 || b.get(12..16)? != b"IHDR" {
        return None;
    }
    let (w, h) = (be32(b, 16)?, be32(b, 20)?);
    let depth = *b.get(24)?;
    let colour = *b.get(25)?;
    let valid_depth = match colour {
        0 => matches!(depth, 1 | 2 | 4 | 8 | 16),
        3 => matches!(depth, 1 | 2 | 4 | 8),
        2 | 4 | 6 => matches!(depth, 8 | 16),
        _ => false,
    };
    let (compression, filter, interlace) = (*b.get(26)?, *b.get(27)?, *b.get(28)?);
    let tail = b.get(b.len().checked_sub(12)?..)?;
    let ok = valid_depth
        && compression == 0
        && filter == 0
        && interlace <= 1
        && b.len() >= 8 + 25 + 12
        && tail == PNG_IEND;
    ok.then_some((w, h))
}

/// The dimensions of a JPEG that starts with SOI, ends with EOI and has a
/// frame header (SOF0–SOF15 but DHT, JPG and DAC) before its first scan.
fn jpeg_size(b: &[u8]) -> Option<(u32, u32)> {
    if b.get(..2)? != [0xFF, 0xD8] || b.get(b.len().checked_sub(2)?..)? != [0xFF, 0xD9] {
        return None;
    }
    let mut at = 2usize;
    loop {
        if *b.get(at)? != 0xFF {
            return None;
        }
        // Fill bytes before a marker.
        while *b.get(at + 1)? == 0xFF {
            at += 1;
        }
        let marker = *b.get(at + 1)?;
        at += 2;
        if matches!(marker, 0x01 | 0xD0..=0xD7) {
            continue;
        }
        if marker == 0xD8 || marker == 0xD9 || marker == 0xDA || marker == 0x00 {
            // A second SOI, an early EOI or a scan before any frame header.
            return None;
        }
        let len = usize::try_from(be16(b, at)?).ok()?;
        if len < 2 {
            return None;
        }
        let sof = matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
        if sof {
            let h = be16(b, at + 3)?;
            let w = be16(b, at + 5)?;
            return Some((w, h));
        }
        at = at.checked_add(len)?;
    }
}

/// Check a rendition (spec §5.7) as the `size` it claims to be.
pub fn check_rendition(
    r: RenditionBytes,
    size: RenditionSize,
) -> Result<CheckedRendition, ExportError> {
    let b = r.0;
    if b.len() > MAX_RENDITION_BYTES {
        return Err(rejected());
    }
    let (ext, (width, height)) = if let Some(d) = png_size(&b) {
        (ImageExt::Png, d)
    } else if let Some(d) = jpeg_size(&b) {
        (ImageExt::Jpeg, d)
    } else {
        return Err(rejected());
    };
    let max = size.max_px();
    if width == 0 || height == 0 || width > max || height > max {
        return Err(rejected());
    }
    Ok(CheckedRendition {
        ext,
        width,
        height,
        bytes: b,
    })
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn b64_char(v: u32) -> char {
    B64.get((v & 63) as usize).map_or('A', |&c| char::from(c))
}

/// Standard base64 with padding (RFC 4648 §4).
pub fn base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let a = u32::from(chunk.first().copied().unwrap_or(0));
        let b = chunk.get(1).copied().map(u32::from);
        let c = chunk.get(2).copied().map(u32::from);
        let n = (a << 16) | (b.unwrap_or(0) << 8) | c.unwrap_or(0);
        out.push(b64_char(n >> 18));
        out.push(b64_char(n >> 12));
        out.push(if b.is_some() { b64_char(n >> 6) } else { '=' });
        out.push(if c.is_some() { b64_char(n) } else { '=' });
    }
    out
}

/// The length of [`base64`]'s output for `n` bytes.
pub fn base64_len(n: usize) -> usize {
    n.div_ceil(3).saturating_mul(4)
}

/// `data:<mime>;base64,<…>` for a checked rendition.
pub fn data_uri(r: &CheckedRendition) -> String {
    format!("data:{};base64,{}", r.mime(), base64(r.bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal valid PNG header of `w` × `h` (8-bit RGB), an empty IDAT
    /// and IEND. Structure only: checksums are not checked here.
    pub(crate) fn png(w: u32, h: u32) -> Vec<u8> {
        let mut b = PNG_SIGNATURE.to_vec();
        b.extend_from_slice(&13u32.to_be_bytes());
        b.extend_from_slice(b"IHDR");
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&h.to_be_bytes());
        b.extend_from_slice(&[8, 2, 0, 0, 0]);
        b.extend_from_slice(&[0, 0, 0, 0]);
        b.extend_from_slice(&[0, 0, 0, 0]);
        b.extend_from_slice(b"IDAT");
        b.extend_from_slice(&[0, 0, 0, 0]);
        b.extend_from_slice(&PNG_IEND);
        b
    }

    /// A minimal JPEG marker skeleton of `w` × `h`.
    fn jpeg(w: u16, h: u16) -> Vec<u8> {
        let mut b = vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 4, 0, 0];
        b.extend_from_slice(&[0xFF, 0xC0, 0, 11, 8]);
        b.extend_from_slice(&h.to_be_bytes());
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&[1, 1, 0x11, 0]);
        b.extend_from_slice(&[0xFF, 0xDA, 0, 2, 0x12, 0x34, 0xFF, 0xD9]);
        b
    }

    fn check(b: Vec<u8>, s: RenditionSize) -> Result<CheckedRendition, ExportError> {
        check_rendition(RenditionBytes(b), s)
    }

    #[test]
    fn valid_renditions_pass_with_their_dimensions() {
        let p = check(png(640, 480), RenditionSize::Thumb).expect("png");
        assert_eq!(
            (p.ext(), p.width(), p.height(), p.mime()),
            (ImageExt::Png, 640, 480, "image/png")
        );
        let j = check(jpeg(300, 2048), RenditionSize::Display).expect("jpeg");
        assert_eq!(
            (j.ext(), j.width(), j.height()),
            (ImageExt::Jpeg, 300, 2048)
        );
    }

    #[test]
    fn bad_renditions_are_refused() {
        let refused = |b: Vec<u8>, s| matches!(check(b, s), Err(ExportError::HelperOutputRejected));
        // Over the thumbnail's limit, zero-sized, wrong signature.
        assert!(refused(png(641, 10), RenditionSize::Thumb));
        assert!(refused(png(0, 10), RenditionSize::Thumb));
        assert!(refused(jpeg(2049, 1), RenditionSize::Display));
        let mut sig = png(10, 10);
        sig[1] = b'Q';
        assert!(refused(sig, RenditionSize::Thumb));
        // A PNG without IEND, or with a bad bit depth.
        let mut cut = png(10, 10);
        cut.truncate(cut.len() - 12);
        assert!(refused(cut, RenditionSize::Thumb));
        let mut depth = png(10, 10);
        depth[24] = 3;
        assert!(refused(depth, RenditionSize::Thumb));
        // A JPEG without EOI, a scan before the frame header, an SVG, HTML.
        let mut no_eoi = jpeg(10, 10);
        no_eoi.truncate(no_eoi.len() - 2);
        assert!(refused(no_eoi, RenditionSize::Thumb));
        let early_scan = vec![0xFF, 0xD8, 0xFF, 0xDA, 0, 2, 0xFF, 0xD9];
        assert!(refused(early_scan, RenditionSize::Thumb));
        assert!(refused(b"<svg onload=x>".to_vec(), RenditionSize::Thumb));
        assert!(refused(Vec::new(), RenditionSize::Thumb));
        // Over the frame bound.
        let mut big = png(10, 10);
        big.resize(MAX_RENDITION_BYTES + 1, 0);
        assert!(refused(big, RenditionSize::Display));
    }

    #[test]
    fn base64_matches_rfc_4648_vectors() {
        for (i, o) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(i.as_bytes()), o);
            assert_eq!(base64_len(i.len()), o.len());
        }
        assert_eq!(base64(&[0xFB, 0xFF]), "+/8=");
    }
}
