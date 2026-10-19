//! Content sniffing (M5 spec §5.2): what an attachment's bytes are,
//! whatever its stored type says.
//!
//! - **The stored type is a label only.** `mime_type`, `uti` and
//!   `transfer_name` decide nothing here; the golden archive's "images" are
//!   ASCII text.
//! - **The table** is a clean-room statement of the facts in the WHATWG MIME
//!   Sniffing Standard's image table (§6.1: GIF87a/GIF89a, `RIFF….WEBPVP`,
//!   PNG, `FF D8 FF`, BMP, ICO) and audio and video table (§6.2: WebM, WAV,
//!   AIFF, Ogg, `ID3` MP3), plus ISO-BMFF `ftyp` brands, because the WHATWG
//!   MP4 rule accepts only the `mp4` brands: `qt  ` (QuickTime), `M4A `,
//!   `M4V ` and the HEIF brands `heic`, `heix`, `mif1` and `msf1`. The major
//!   brand is tried first, then each compatible brand in order. Then CAF,
//!   PDF, XML (never sniffed as an image, so SVG is never served), HTML (a
//!   tag after optional whitespace, case-insensitive, followed by a space or
//!   `>`), vCard, and UTF-8 text without binary control bytes. Everything
//!   else, including an empty file, is [`SniffedKind::Unknown`].
//! - **MP3 without an `ID3` tag** is not recognised: the WHATWG frame-header
//!   walk is not implemented, so such a file is "Unrecognised content".
//! - **It reads at most [`SNIFF_WINDOW`] bytes.** Those bytes go only to
//!   this function.
//! - **The sniff does not decide which demuxer runs in the web engine**
//!   (GStreamer typefinds on its own; UNVERIFIED, M5 Appendix B). It decides
//!   what this app offers: a rendition, the Play card or a file card.
//!
//! The same table is stated in `scripts/fixtures/make_synthetic_viewer.py`
//! (the catalogue's model); `tests/viewer_media.rs` compares the two on every
//! catalogue attachment.

/// The most bytes the sniffer reads: the first 4 KiB of the file.
pub const SNIFF_WINDOW: usize = 4096;

/// What the bytes are (§5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SniffedKind {
    Gif,
    Webp,
    Png,
    Jpeg,
    /// Recognised, served as a file card.
    Bmp,
    /// Recognised, served as a file card.
    Ico,
    /// ISO-BMFF with the `qt  ` brand.
    QuickTime,
    /// ISO-BMFF with the `M4A ` brand.
    M4a,
    /// ISO-BMFF with the `M4V ` brand.
    M4v,
    /// ISO-BMFF with a HEIF brand (`heic`, `heix`, `mif1`, `msf1`).
    Heif,
    /// ISO-BMFF with an `mp4*` brand.
    Mp4,
    /// An `ftyp` box with no brand this table knows.
    IsoBmffOther,
    Webm,
    Wav,
    Aiff,
    Ogg,
    /// An `ID3`-tagged MP3.
    Mp3,
    /// Core Audio Format (an iOS audio message's container).
    Caf,
    Pdf,
    /// XML, SVG included: never an image.
    Xml,
    Html,
    Vcard,
    /// UTF-8 text without binary control bytes.
    Text,
    /// Nothing above, or an empty file: "Unrecognised content".
    Unknown,
}

/// What the viewer offers for a kind (§5.1, §5.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MediaClass {
    /// A rendition the app re-encodes (`thumb`, `display`).
    Image,
    /// The Play card: the original bytes, streamed after the hash and
    /// structural checks. `structure_checked` is false for MP3, Ogg and
    /// WebM, whose check is magic only ("structure not checked for this
    /// format").
    Playable { structure_checked: bool },
    /// A file card that names the type. Not a failure.
    File,
    /// "Unrecognised content". Not a failure.
    Unrecognised,
}

impl SniffedKind {
    /// Every kind, in declaration order.
    pub const ALL: [SniffedKind; 24] = [
        SniffedKind::Gif,
        SniffedKind::Webp,
        SniffedKind::Png,
        SniffedKind::Jpeg,
        SniffedKind::Bmp,
        SniffedKind::Ico,
        SniffedKind::QuickTime,
        SniffedKind::M4a,
        SniffedKind::M4v,
        SniffedKind::Heif,
        SniffedKind::Mp4,
        SniffedKind::IsoBmffOther,
        SniffedKind::Webm,
        SniffedKind::Wav,
        SniffedKind::Aiff,
        SniffedKind::Ogg,
        SniffedKind::Mp3,
        SniffedKind::Caf,
        SniffedKind::Pdf,
        SniffedKind::Xml,
        SniffedKind::Html,
        SniffedKind::Vcard,
        SniffedKind::Text,
        SniffedKind::Unknown,
    ];

    /// What the viewer offers for this kind. HEIF is an image only in a
    /// build with the `heic` feature (owner decision Q2; off by default and
    /// in every official build), and a file card otherwise.
    pub const fn class(self) -> MediaClass {
        match self {
            SniffedKind::Gif | SniffedKind::Webp | SniffedKind::Png | SniffedKind::Jpeg => {
                MediaClass::Image
            }
            SniffedKind::Heif if cfg!(feature = "heic") => MediaClass::Image,
            SniffedKind::Mp4
            | SniffedKind::M4v
            | SniffedKind::QuickTime
            | SniffedKind::M4a
            | SniffedKind::Wav => MediaClass::Playable {
                structure_checked: true,
            },
            SniffedKind::Mp3 | SniffedKind::Ogg | SniffedKind::Webm => MediaClass::Playable {
                structure_checked: false,
            },
            SniffedKind::Unknown => MediaClass::Unrecognised,
            _ => MediaClass::File,
        }
    }

    /// The media type the app states for this kind: the `Content-Type` of
    /// a stream, and the "content: Y" of a type disagreement.
    pub const fn mime(self) -> &'static str {
        match self {
            SniffedKind::Gif => "image/gif",
            SniffedKind::Webp => "image/webp",
            SniffedKind::Png => "image/png",
            SniffedKind::Jpeg => "image/jpeg",
            SniffedKind::Bmp => "image/bmp",
            SniffedKind::Ico => "image/x-icon",
            SniffedKind::QuickTime => "video/quicktime",
            SniffedKind::M4a => "audio/mp4",
            SniffedKind::M4v => "video/x-m4v",
            SniffedKind::Heif => "image/heif",
            SniffedKind::Mp4 => "video/mp4",
            SniffedKind::Webm => "video/webm",
            SniffedKind::Wav => "audio/wav",
            SniffedKind::Aiff => "audio/aiff",
            SniffedKind::Ogg => "application/ogg",
            SniffedKind::Mp3 => "audio/mpeg",
            SniffedKind::Caf => "audio/x-caf",
            SniffedKind::Pdf => "application/pdf",
            SniffedKind::Xml => "text/xml",
            SniffedKind::Html => "text/html",
            SniffedKind::Vcard => "text/vcard",
            SniffedKind::Text => "text/plain",
            SniffedKind::IsoBmffOther | SniffedKind::Unknown => "application/octet-stream",
        }
    }
}

/// The bytes that make a text window binary: C0 controls other than TAB,
/// LF, FF, CR and ESC.
fn is_binary(b: u8) -> bool {
    matches!(b, 0x00..=0x08 | 0x0B | 0x0E..=0x1A | 0x1C..=0x1F)
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    let s: [u8; 4] = b.get(at..at.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_be_bytes(s))
}

/// The major brand and the compatible brands of a leading `ftyp` box, or
/// `None` when the window does not start with a well-formed one (size at
/// least 16, a multiple of 4, inside the window).
fn brands(b: &[u8]) -> Option<(&[u8], Vec<&[u8]>)> {
    if b.len() < 12 || b.get(4..8)? != b"ftyp" {
        return None;
    }
    let size = usize::try_from(be32(b, 0)?).ok()?;
    if size < 16 || size % 4 != 0 || size > b.len() {
        return None;
    }
    let major = b.get(8..12)?;
    let compat = b.get(16..size)?.chunks_exact(4).collect();
    Some((major, compat))
}

fn brand_kind(brand: &[u8]) -> Option<SniffedKind> {
    match brand {
        b"qt  " => Some(SniffedKind::QuickTime),
        b"M4A " => Some(SniffedKind::M4a),
        b"M4V " => Some(SniffedKind::M4v),
        b"heic" | b"heix" | b"mif1" | b"msf1" => Some(SniffedKind::Heif),
        _ if brand.starts_with(b"mp4") => Some(SniffedKind::Mp4),
        _ => None,
    }
}

/// The HTML tags that start an HTML document (WHATWG §7.1's list, plus
/// `<svg`, which the model treats the same way when it is not XML).
const HTML_TAGS: &[&[u8]] = &[
    b"<!doctype html",
    b"<html",
    b"<head",
    b"<script",
    b"<iframe",
    b"<h1",
    b"<div",
    b"<font",
    b"<table",
    b"<a",
    b"<style",
    b"<title",
    b"<b",
    b"<body",
    b"<br",
    b"<p",
    b"<!--",
    b"<svg",
];

/// Sniff `head`, the first bytes of a file of `file_len` bytes. At most
/// [`SNIFF_WINDOW`] bytes of `head` are looked at.
///
/// `file_len` matters only for text: a window cut inside a UTF-8 sequence
/// of a longer file is still text.
pub fn sniff(head: &[u8], file_len: u64) -> SniffedKind {
    let b = head.get(..SNIFF_WINDOW).unwrap_or(head);
    let starts = |m: &[u8]| b.starts_with(m);
    if starts(b"GIF87a") || starts(b"GIF89a") {
        return SniffedKind::Gif;
    }
    if starts(b"RIFF") && b.get(8..14) == Some(b"WEBPVP") {
        return SniffedKind::Webp;
    }
    if starts(b"\x89PNG\r\n\x1a\n") {
        return SniffedKind::Png;
    }
    if starts(b"\xff\xd8\xff") {
        return SniffedKind::Jpeg;
    }
    if starts(b"BM") {
        return SniffedKind::Bmp;
    }
    if starts(b"\x00\x00\x01\x00") || starts(b"\x00\x00\x02\x00") {
        return SniffedKind::Ico;
    }
    if let Some((major, compat)) = brands(b) {
        return std::iter::once(major)
            .chain(compat)
            .find_map(brand_kind)
            .unwrap_or(SniffedKind::IsoBmffOther);
    }
    if starts(b"\x1a\x45\xdf\xa3")
        && b.get(4..b.len().min(64))
            .is_some_and(|w| w.windows(4).any(|x| x == b"webm"))
    {
        return SniffedKind::Webm;
    }
    if starts(b"RIFF") && b.get(8..12) == Some(b"WAVE") {
        return SniffedKind::Wav;
    }
    if starts(b"FORM") && b.get(8..12) == Some(b"AIFF") {
        return SniffedKind::Aiff;
    }
    if starts(b"OggS\x00") {
        return SniffedKind::Ogg;
    }
    if starts(b"ID3") {
        return SniffedKind::Mp3;
    }
    if starts(b"caff") {
        return SniffedKind::Caf;
    }
    if starts(b"%PDF-") {
        return SniffedKind::Pdf;
    }
    let lead = b
        .iter()
        .position(|c| !matches!(c, b'\t' | b'\n' | 0x0C | b'\r' | b' '))
        .unwrap_or(b.len());
    let low: Vec<u8> = b
        .get(lead..)
        .unwrap_or_default()
        .iter()
        .map(u8::to_ascii_lowercase)
        .collect();
    if low.starts_with(b"<?xml") {
        return SniffedKind::Xml;
    }
    for tag in HTML_TAGS {
        if low.starts_with(tag) && matches!(low.get(tag.len()), Some(b' ' | b'>')) {
            return SniffedKind::Html;
        }
    }
    if low.starts_with(b"begin:vcard") {
        return SniffedKind::Vcard;
    }
    if !b.is_empty() && !b.iter().copied().any(is_binary) {
        let cut = file_len > u64::try_from(b.len()).unwrap_or(u64::MAX);
        match std::str::from_utf8(b) {
            Ok(_) => return SniffedKind::Text,
            // A sequence cut by the window's end, in a longer file.
            Err(e) if cut && e.error_len().is_none() => return SniffedKind::Text,
            Err(_) => {}
        }
    }
    SniffedKind::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(b: &[u8]) -> SniffedKind {
        sniff(b, b.len() as u64)
    }

    fn ftyp(major: &[u8; 4], compat: &[&[u8; 4]]) -> Vec<u8> {
        let size = 16 + 4 * compat.len();
        let mut v = (size as u32).to_be_bytes().to_vec();
        v.extend_from_slice(b"ftyp");
        v.extend_from_slice(major);
        v.extend_from_slice(&[0, 0, 0, 0]);
        for c in compat {
            v.extend_from_slice(*c);
        }
        v
    }

    #[test]
    fn every_magic_number_in_the_tables() {
        use SniffedKind as K;
        let cases: &[(&[u8], SniffedKind)] = &[
            (b"GIF87a\x01\x00", K::Gif),
            (b"GIF89a\x01\x00", K::Gif),
            (b"RIFF\0\0\0\0WEBPVP8 ", K::Webp),
            (b"RIFF\0\0\0\0WEBPVP8L", K::Webp),
            (b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR", K::Png),
            (b"\xff\xd8\xff\xe0", K::Jpeg),
            (b"BM\0\0", K::Bmp),
            (b"\x00\x00\x01\x00\x01\x00", K::Ico),
            (b"\x00\x00\x02\x00\x01\x00", K::Ico),
            (
                b"\x1a\x45\xdf\xa3\x01\x00\x00\x00\x00\x00\x00\x1fB\x82\x84webm",
                K::Webm,
            ),
            (b"RIFF\x24\0\0\0WAVEfmt ", K::Wav),
            (b"FORM\0\0\0\x04AIFF", K::Aiff),
            (b"OggS\x00\x02", K::Ogg),
            (b"ID3\x04\x00", K::Mp3),
            (b"caff\x00\x01\x00\x00", K::Caf),
            (b"%PDF-1.4\n", K::Pdf),
            (b"<?xml version=\"1.0\"?><svg/>", K::Xml),
            (b"  \n<!DOCTYPE HTML><p>", K::Html),
            (b"<html>", K::Html),
            (b"<SCRIPT >x", K::Html),
            (b"<svg onload=x>", K::Html),
            (b"<!-- c -->", K::Html),
            (b"BEGIN:VCARD\r\nVERSION:3.0\r\n", K::Vcard),
            (b"plain words\n", K::Text),
            (b"caf\xc3\xa9 \x1b[0m", K::Text),
            (b"\x00\x01\x02", K::Unknown),
            (b"\xff\xfe\xfd", K::Unknown),
            // A tag with nothing after it, or another tag, is not HTML.
            (b"<html", K::Text),
            (b"<a", K::Text),
            (b"<abbr>", K::Text),
        ];
        for (bytes, want) in cases {
            assert_eq!(s(bytes), *want, "{bytes:?}");
        }
    }

    #[test]
    fn every_ftyp_brand() {
        use SniffedKind as K;
        for (major, compat, want) in [
            (b"qt  ", vec![], K::QuickTime),
            (b"M4A ", vec![], K::M4a),
            (b"M4V ", vec![], K::M4v),
            (b"heic", vec![], K::Heif),
            (b"heix", vec![], K::Heif),
            (b"mif1", vec![], K::Heif),
            (b"msf1", vec![], K::Heif),
            (b"mp41", vec![], K::Mp4),
            (b"mp42", vec![], K::Mp4),
            (b"isom", vec![b"iso2", b"mp41"], K::Mp4),
            (b"isom", vec![b"M4A ", b"mp42"], K::M4a),
            (b"isom", vec![b"mif1", b"heic"], K::Heif),
            (b"3gp4", vec![b"isom"], K::IsoBmffOther),
            (b"avif", vec![], K::IsoBmffOther),
        ] {
            let compat: Vec<&[u8; 4]> = compat.into_iter().collect();
            assert_eq!(s(&ftyp(major, &compat)), want, "{major:?}");
        }
        // A malformed ftyp box: size not a multiple of 4, too small, or
        // past the window.
        let mut odd = ftyp(b"qt  ", &[]);
        odd[3] = 17;
        assert_eq!(s(&odd), K::Unknown);
        let mut small = ftyp(b"qt  ", &[]);
        small[3] = 12;
        assert_eq!(s(&small), K::Unknown);
        let mut long = ftyp(b"qt  ", &[]);
        long[3] = 20;
        assert_eq!(s(&long), K::Unknown);
    }

    #[test]
    fn truncated_headers_empty_and_one_byte_files() {
        use SniffedKind as K;
        assert_eq!(s(b""), K::Unknown);
        assert_eq!(s(b"\x00"), K::Unknown);
        assert_eq!(s(b"G"), K::Text);
        assert_eq!(s(b"GIF8"), K::Text);
        assert_eq!(s(b"\x89PNG"), K::Unknown);
        assert_eq!(s(b"\xff\xd8"), K::Unknown);
        assert_eq!(s(b"RIFF\0\0\0\0WEB"), K::Unknown);
        assert_eq!(s(b"\0\0\0\x10ftyp"), K::Unknown);
        assert_eq!(s(b"\x1a\x45\xdf\xa3"), K::Unknown);
    }

    #[test]
    fn polyglots_and_svg() {
        use SniffedKind as K;
        // A GIF header followed by HTML is a GIF: only ever re-encoded.
        let mut poly = b"GIF89a\x01\x00\x01\x00\x00\x00\x00".to_vec();
        poly.extend_from_slice(b"<html><script>alert(1)</script>");
        assert_eq!(s(&poly), K::Gif);
        assert_eq!(K::Gif.class(), MediaClass::Image);
        // HTML (whatever its name) is a card.
        assert_eq!(s(b"<html><img src=x onerror=alert(1)>"), K::Html);
        assert_eq!(K::Html.class(), MediaClass::File);
        // SVG with a script is XML: a card, never an image.
        let svg = b"<?xml version=\"1.0\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\"><script>x</script></svg>";
        assert_eq!(s(svg), K::Xml);
        assert_eq!(K::Xml.class(), MediaClass::File);
    }

    #[test]
    fn only_the_window_is_read_and_a_cut_sequence_is_still_text() {
        let mut long = vec![b'a'; SNIFF_WINDOW - 1];
        long.extend_from_slice("é".as_bytes()); // cut by the window
        long.extend_from_slice(b"\x00\x00");
        assert_eq!(sniff(&long, long.len() as u64), SniffedKind::Text);
        // The same window as a whole file is not text.
        let window = &long[..SNIFF_WINDOW];
        assert_eq!(sniff(window, SNIFF_WINDOW as u64), SniffedKind::Unknown);
        // Bytes past the window never change the verdict.
        let mut tail = vec![b'a'; SNIFF_WINDOW];
        tail.push(0);
        assert_eq!(sniff(&tail, tail.len() as u64), SniffedKind::Text);
    }

    #[test]
    fn classes_and_types() {
        use SniffedKind as K;
        for k in K::ALL {
            let class = k.class();
            match k {
                K::Gif | K::Webp | K::Png | K::Jpeg => assert_eq!(class, MediaClass::Image),
                K::Heif => assert_eq!(class == MediaClass::Image, cfg!(feature = "heic")),
                K::Mp4 | K::M4v | K::QuickTime | K::M4a | K::Wav => assert_eq!(
                    class,
                    MediaClass::Playable {
                        structure_checked: true
                    }
                ),
                K::Mp3 | K::Ogg | K::Webm => assert_eq!(
                    class,
                    MediaClass::Playable {
                        structure_checked: false
                    }
                ),
                K::Unknown => assert_eq!(class, MediaClass::Unrecognised),
                _ => assert_eq!(class, MediaClass::File, "{k:?}"),
            }
            assert!(k.mime().contains('/'));
        }
    }
}
