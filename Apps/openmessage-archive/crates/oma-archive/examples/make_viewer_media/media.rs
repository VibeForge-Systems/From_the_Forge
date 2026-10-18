//! The viewer media catalogue (M5 spec §14.1): every file of
//! `tests/fixtures/viewer/media/`, built in memory.
//!
//! Shared by the `make_viewer_media` example, which writes the files, and
//! by `tests/viewer_fixtures.rs`, which rebuilds them and compares them
//! byte for byte with the committed copies (so the files stay what this
//! code says they are).
//!
//! - **Encoded images** come from the `image` crate's own encoders (PNG,
//!   JPEG, GIF, WebP). The JPEG gets a hand-written EXIF block
//!   (orientation 6 and a fake GPS block) spliced in after its APP0.
//! - **Hostile PNGs** are written by hand: a decompression bomb, a 16-bit
//!   RGBA image over the decoded-bytes cap, and an IHDR that claims
//!   60000 × 60000. Their compressed streams come from [`zlib_zeros`], a
//!   minimal deflate writer for runs of zero bytes (one dynamic-Huffman
//!   block; length 258 at distance 1 costs two bits).
//! - **Containers** (MP4, QuickTime, M4A, WAV, CAF, HEIC) are minimal box
//!   and chunk trees written by hand, valid and hostile.
//! - **Hostile files**: an SVG with a script, HTML named `.jpg`, a GIF
//!   followed by HTML, and a few plain cards.
//!
//! Every value is synthetic. No playable audio or video is included.
#![allow(dead_code)]

use image::codecs::gif::{GifEncoder, Repeat};
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::codecs::webp::WebPEncoder;
use image::{Delay, ExtendedColorType, Frame, ImageEncoder, Rgba, RgbaImage};

/// One file of the catalogue.
pub struct MediaFile {
    /// The file name under `media/` (lowercase, unique).
    pub name: &'static str,
    /// What the file is for (written into the catalogue README).
    pub purpose: &'static str,
    pub bytes: Vec<u8>,
}

/// The decoded-bytes cap the bomb fixtures exceed (spec §5.5): 128 MiB.
pub const DECODED_CAP: u64 = 128 << 20;

/// The decompression bomb's dimensions: grey 8-bit, inside the 16384 px
/// limit, but 144,000,000 decoded bytes (over [`DECODED_CAP`], and under
/// the decoder's 160 MiB `max_alloc`, so only the cap refuses it).
pub const BOMB_SIDE: u32 = 12_000;
/// The 16-bit RGBA image's side: 4097² pixels × 8 bytes is over the cap,
/// though 4 bytes per pixel would be under it.
pub const RGBA16_SIDE: u32 = 4_097;
/// The oversized IHDR's claimed side.
pub const HUGE_SIDE: u32 = 60_000;

/// Every file, in name order.
pub fn all() -> Vec<MediaFile> {
    let mut v = vec![
        MediaFile {
            name: "png-rgb-6x4.png",
            purpose: "A tiny opaque PNG (RGB8, 6×4)",
            bytes: png_rgb(),
        },
        MediaFile {
            name: "png-alpha-4x4.png",
            purpose: "A tiny PNG with alpha (RGBA8, 4×4): re-encoded as PNG",
            bytes: png_alpha(),
        },
        MediaFile {
            name: "jpeg-exif6-gps-8x6.jpg",
            purpose: "A tiny JPEG (8×6) with EXIF orientation 6 and a fake GPS block",
            bytes: jpeg_exif(),
        },
        MediaFile {
            name: "jpeg-truncated.jpg",
            purpose: "A 96×64 JPEG cut to its first third",
            bytes: jpeg_truncated(),
        },
        MediaFile {
            name: "gif-2frames-4x4.gif",
            purpose: "A GIF with two frames (first frame only, \"Animated\")",
            bytes: gif_two_frames(),
        },
        MediaFile {
            name: "webp-lossless-4x4.webp",
            purpose: "A tiny lossless WebP",
            bytes: webp(),
        },
        MediaFile {
            name: "png-bomb-12000.png",
            purpose: "Decompression bomb: grey 12000×12000, 144,000,000 decoded bytes from ~140 KB",
            bytes: png_zero_image(BOMB_SIDE, BOMB_SIDE, 0, 8, 1),
        },
        MediaFile {
            name: "png-rgba16-4097.png",
            purpose: "RGBA 16-bit 4097×4097: over the 128 MiB decoded cap only at 8 bytes per pixel",
            bytes: png_zero_image(RGBA16_SIDE, RGBA16_SIDE, 6, 16, 8),
        },
        MediaFile {
            name: "png-ihdr-60000.png",
            purpose: "IHDR claims 60000×60000 RGB8; the data holds one row",
            bytes: png_huge_ihdr(),
        },
        MediaFile {
            name: "mp4-minimal.mp4",
            purpose: "Minimal ISO-BMFF (`isom`): ftyp, moov (mvhd, trak with a self-contained dref), mdat",
            bytes: mp4(b"isom", &[b"isom", b"iso2", b"mp41"], b"vide", None),
        },
        MediaFile {
            name: "mov-minimal.mov",
            purpose: "Minimal QuickTime (`qt  `), as the MP4",
            bytes: mp4(b"qt  ", &[b"qt  "], b"vide", None),
        },
        MediaFile {
            name: "m4a-minimal.m4a",
            purpose: "Minimal M4A (`M4A `), a sound track",
            bytes: mp4(b"M4A ", &[b"M4A ", b"mp42", b"isom"], b"soun", None),
        },
        MediaFile {
            name: "wav-minimal.wav",
            purpose: "Minimal RIFF WAVE: fmt (PCM, mono, 8 kHz, 16-bit) then 16 bytes of data",
            bytes: wav(16, None),
        },
        MediaFile {
            name: "caf-header.caf",
            purpose: "A Core Audio Format header (desc, data): an iOS audio message's container",
            bytes: caf(),
        },
        MediaFile {
            name: "heic-header.heic",
            purpose: "A HEIF header only (ftyp `heic`, meta/hdlr `pict`); never decoded",
            bytes: heic(),
        },
        MediaFile {
            name: "mov-refmovie-rmra.mov",
            purpose: "Hostile: a reference movie (moov/rmra/rmda/rdrf) pointing at http://127.0.0.1:9",
            bytes: mov_reference_movie(),
        },
        MediaFile {
            name: "mov-dref-external.mov",
            purpose: "Hostile: a dref `url ` entry without the self-contained flag, naming http://127.0.0.1:9",
            bytes: mp4(
                b"qt  ",
                &[b"qt  "],
                b"vide",
                Some(b"http://127.0.0.1:9/SYNTH-EXTERNAL.mov"),
            ),
        },
        MediaFile {
            name: "mp4-box-past-eof.mp4",
            purpose: "Hostile: the moov box claims 4096 bytes; the file ends first",
            bytes: mp4_box_past_eof(),
        },
        MediaFile {
            name: "mp4-box-64bit-overflow.mp4",
            purpose: "Hostile: an mdat with size 1 and a 64-bit size of u64::MAX",
            bytes: mp4_64bit_overflow(),
        },
        MediaFile {
            name: "wav-chunk-past-eof.wav",
            purpose: "Hostile: the data chunk claims 0x7ffffff0 bytes; 16 follow",
            bytes: wav(16, Some(0x7fff_fff0)),
        },
        MediaFile {
            name: "svg-script.svg",
            purpose: "Hostile: an SVG with a script element (a card; XML is never sniffed as an image)",
            bytes: SVG_SCRIPT.as_bytes().to_vec(),
        },
        MediaFile {
            name: "html-named-jpg.jpg",
            purpose: "Hostile: HTML with a script and an onerror handler, named .jpg",
            bytes: HTML_PAGE.as_bytes().to_vec(),
        },
        MediaFile {
            name: "gif-html-polyglot.gif",
            purpose: "Hostile: a valid one-frame GIF followed by HTML (served only re-encoded)",
            bytes: gif_html_polyglot(),
        },
        MediaFile {
            name: "pdf-minimal.pdf",
            purpose: "A PDF header and trailer (a card)",
            bytes: PDF.as_bytes().to_vec(),
        },
        MediaFile {
            name: "vcard-synthetic.vcf",
            purpose: "A synthetic vCard (a card)",
            bytes: VCARD.as_bytes().to_vec(),
        },
        MediaFile {
            name: "text-plain.txt",
            purpose: "Plain text (a card)",
            bytes: b"SYNTH-TEXT-ATTACHMENT plain text\n".to_vec(),
        },
        MediaFile {
            name: "unknown-bytes.bin",
            purpose: "Bytes that match no signature (\"Unrecognised content\")",
            bytes: (0u8..64).map(|i| i.wrapping_mul(37).wrapping_add(11)).collect(),
        },
        MediaFile {
            name: "one-byte.bin",
            purpose: "A one-byte file",
            bytes: vec![0x89],
        },
        MediaFile {
            name: "empty.bin",
            purpose: "An empty file",
            bytes: Vec::new(),
        },
    ];
    v.sort_by_key(|f| f.name);
    v
}

// ---------------------------------------------------------------------------
// Encoded images (the `image` crate's encoders)
// ---------------------------------------------------------------------------

/// A deterministic RGB pattern of `w × h`.
fn pattern_rgb(w: u32, h: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            v.push(((x * 255) / w.max(1)) as u8);
            v.push(((y * 255) / h.max(1)) as u8);
            v.push((((x ^ y) * 37) & 0xff) as u8);
        }
    }
    v
}

fn png_rgb() -> Vec<u8> {
    let mut out = Vec::new();
    PngEncoder::new(&mut out)
        .write_image(&pattern_rgb(6, 4), 6, 4, ExtendedColorType::Rgb8)
        .expect("png");
    out
}

fn png_alpha() -> Vec<u8> {
    let mut px = Vec::new();
    for y in 0u8..4 {
        for x in 0u8..4 {
            px.extend_from_slice(&[x * 60, y * 60, 200, 255 - (x + y) * 30]);
        }
    }
    let mut out = Vec::new();
    PngEncoder::new(&mut out)
        .write_image(&px, 4, 4, ExtendedColorType::Rgba8)
        .expect("png");
    out
}

fn jpeg(w: u32, h: u32, quality: u8) -> Vec<u8> {
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, quality)
        .encode(&pattern_rgb(w, h), w, h, ExtendedColorType::Rgb8)
        .expect("jpeg");
    out
}

/// A little-endian TIFF structure: IFD0 with Orientation = 6 and a GPS IFD
/// pointer; the GPS IFD with fake latitude and longitude (12°34'56" N,
/// 65°43'21" E).
fn exif_tiff() -> Vec<u8> {
    fn entry(t: &mut Vec<u8>, tag: u16, typ: u16, count: u32, value: [u8; 4]) {
        t.extend_from_slice(&tag.to_le_bytes());
        t.extend_from_slice(&typ.to_le_bytes());
        t.extend_from_slice(&count.to_le_bytes());
        t.extend_from_slice(&value);
    }
    const IFD0: u32 = 8;
    const GPS: u32 = IFD0 + 2 + 2 * 12 + 4; // 38
    const LAT: u32 = GPS + 2 + 4 * 12 + 4; // 92
    const LON: u32 = LAT + 24; // 116
    let mut t = Vec::new();
    t.extend_from_slice(b"II");
    t.extend_from_slice(&42u16.to_le_bytes());
    t.extend_from_slice(&IFD0.to_le_bytes());
    // IFD0
    t.extend_from_slice(&2u16.to_le_bytes());
    entry(&mut t, 0x0112, 3, 1, [6, 0, 0, 0]); // Orientation, SHORT
    entry(&mut t, 0x8825, 4, 1, GPS.to_le_bytes()); // GPSInfo, LONG
    t.extend_from_slice(&0u32.to_le_bytes());
    assert_eq!(t.len() as u32, GPS);
    // GPS IFD
    t.extend_from_slice(&4u16.to_le_bytes());
    entry(&mut t, 1, 2, 2, *b"N\0\0\0"); // GPSLatitudeRef, ASCII
    entry(&mut t, 2, 5, 3, LAT.to_le_bytes()); // GPSLatitude, RATIONAL x3
    entry(&mut t, 3, 2, 2, *b"E\0\0\0"); // GPSLongitudeRef
    entry(&mut t, 4, 5, 3, LON.to_le_bytes()); // GPSLongitude
    t.extend_from_slice(&0u32.to_le_bytes());
    assert_eq!(t.len() as u32, LAT);
    for v in [12u32, 34, 56, 65, 43, 21] {
        t.extend_from_slice(&v.to_le_bytes());
        t.extend_from_slice(&1u32.to_le_bytes());
    }
    assert_eq!(t.len() as u32, LON + 24);
    t
}

/// The 8×6 JPEG with an APP1 `Exif` segment after its APP0 (JFIF).
fn jpeg_exif() -> Vec<u8> {
    let plain = jpeg(8, 6, 85);
    assert_eq!(&plain[..4], &[0xff, 0xd8, 0xff, 0xe0], "SOI then APP0");
    let app0_len = u16::from_be_bytes([plain[4], plain[5]]) as usize;
    let split = 4 + app0_len;
    let mut payload = b"Exif\0\0".to_vec();
    payload.extend_from_slice(&exif_tiff());
    let mut out = plain[..split].to_vec();
    out.extend_from_slice(&[0xff, 0xe1]);
    out.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(&payload);
    out.extend_from_slice(&plain[split..]);
    out
}

fn jpeg_truncated() -> Vec<u8> {
    let full = jpeg(96, 64, 90);
    full[..full.len() / 3].to_vec()
}

fn gif_frames(frames: &[[u8; 4]], side: u32) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = GifEncoder::new(&mut out);
        enc.set_repeat(Repeat::Infinite).expect("repeat");
        let frames = frames.iter().map(|c| {
            Frame::from_parts(
                RgbaImage::from_pixel(side, side, Rgba(*c)),
                0,
                0,
                Delay::from_numer_denom_ms(100, 1),
            )
        });
        enc.encode_frames(frames).expect("gif");
    }
    out
}

fn gif_two_frames() -> Vec<u8> {
    gif_frames(&[[200, 30, 30, 255], [30, 30, 200, 255]], 4)
}

fn gif_html_polyglot() -> Vec<u8> {
    let mut v = gif_frames(&[[30, 160, 30, 255]], 2);
    v.extend_from_slice(HTML_PAGE.as_bytes());
    v
}

fn webp() -> Vec<u8> {
    let mut px = Vec::new();
    for y in 0u8..4 {
        for x in 0u8..4 {
            px.extend_from_slice(&[x * 50, 100, y * 50, 255]);
        }
    }
    let mut out = Vec::new();
    WebPEncoder::new_lossless(&mut out)
        .encode(&px, 4, 4, ExtendedColorType::Rgba8)
        .expect("webp");
    out
}

// ---------------------------------------------------------------------------
// Hand-written PNGs
// ---------------------------------------------------------------------------

/// CRC-32 (ISO 3309, the PNG chunk checksum).
pub fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (n, slot) in table.iter_mut().enumerate() {
        let mut c = n as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xedb8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
        *slot = c;
    }
    let mut c = 0xffff_ffffu32;
    for &b in data {
        c = table[((c ^ u32::from(b)) & 0xff) as usize] ^ (c >> 8);
    }
    c ^ 0xffff_ffff
}

fn chunk(out: &mut Vec<u8>, typ: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut body = typ.to_vec();
    body.extend_from_slice(data);
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32(&body).to_be_bytes());
}

fn ihdr(w: u32, h: u32, color_type: u8, depth: u8) -> Vec<u8> {
    let mut d = Vec::new();
    d.extend_from_slice(&w.to_be_bytes());
    d.extend_from_slice(&h.to_be_bytes());
    d.extend_from_slice(&[depth, color_type, 0, 0, 0]);
    d
}

/// A PNG of `w × h` all-zero pixels (filter type 0 on every row, so the
/// whole decompressed stream is zeros). `bytes_per_pixel` must match the
/// colour type and depth.
pub fn png_zero_image(w: u32, h: u32, color_type: u8, depth: u8, bytes_per_pixel: u64) -> Vec<u8> {
    let raw = u64::from(h) * (1 + u64::from(w) * bytes_per_pixel);
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut out, b"IHDR", &ihdr(w, h, color_type, depth));
    chunk(&mut out, b"IDAT", &zlib_zeros(raw));
    chunk(&mut out, b"IEND", &[]);
    out
}

/// IHDR 60000 × 60000 RGB8 with the zeros of its first row only.
fn png_huge_ihdr() -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut out, b"IHDR", &ihdr(HUGE_SIDE, HUGE_SIDE, 2, 8));
    chunk(&mut out, b"IDAT", &zlib_zeros(1 + u64::from(HUGE_SIDE) * 3));
    chunk(&mut out, b"IEND", &[]);
    out
}

/// LSB-first bit writer (RFC 1951 §3.1.1).
struct Bits {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl Bits {
    fn new() -> Bits {
        Bits {
            out: Vec::new(),
            acc: 0,
            n: 0,
        }
    }

    /// `len` bits of `v`, least significant first (header fields, extra
    /// bits).
    fn put(&mut self, v: u32, len: u32) {
        self.acc |= u64::from(v) << self.n;
        self.n += len;
        while self.n >= 8 {
            self.out.push((self.acc & 0xff) as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    /// A Huffman code of `len` bits, most significant bit first.
    fn code(&mut self, code: u32, len: u32) {
        for i in (0..len).rev() {
            self.put((code >> i) & 1, 1);
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push((self.acc & 0xff) as u8);
        }
        self.out
    }
}

/// A zlib stream (RFC 1950) whose deflate data (RFC 1951) decompresses to
/// `n` zero bytes, `n >= 1`: one final dynamic-Huffman block whose
/// literal/length code has three symbols (285 → `0`, literal 0 → `10`,
/// end of block 256 → `11`) and whose distance code has two (both one
/// bit; code 0 is distance 1). It writes a literal 0, then length 258 at
/// distance 1 (two bits each) as often as fits, then literal zeros.
pub fn zlib_zeros(n: u64) -> Vec<u8> {
    assert!(n >= 1);
    let mut b = Bits::new();
    b.put(1, 1); // BFINAL
    b.put(2, 2); // BTYPE = dynamic
    b.put(286 - 257, 5); // HLIT: symbols 0..=285
    b.put(2 - 1, 5); // HDIST: distance codes 0 and 1
    b.put(18 - 4, 4); // HCLEN: 18 code-length code lengths
                      // Code-length code: 18 → `0`, 1 → `10`, 2 → `11`.
    const ORDER: [usize; 18] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1];
    for sym in ORDER {
        let len = match sym {
            18 => 1,
            1 | 2 => 2,
            _ => 0,
        };
        b.put(len, 3);
    }
    let cl = |b: &mut Bits, sym: u32| match sym {
        18 => b.code(0b0, 1),
        1 => b.code(0b10, 2),
        2 => b.code(0b11, 2),
        _ => unreachable!(),
    };
    let zeros = |b: &mut Bits, mut run: u32| {
        while run > 0 {
            let r = run.min(138);
            assert!(r >= 11);
            cl(b, 18);
            b.put(r - 11, 7);
            run -= r;
        }
    };
    cl(&mut b, 2); // literal 0: length 2
    zeros(&mut b, 117); // 1..=117
    zeros(&mut b, 138); // 118..=255
    cl(&mut b, 2); // 256: length 2
    zeros(&mut b, 28); // 257..=284
    cl(&mut b, 1); // 285: length 1
    cl(&mut b, 1); // distance 0: length 1
    cl(&mut b, 1); // distance 1: length 1
                   // Data.
    b.code(0b10, 2); // literal 0
    let rest = n - 1;
    for _ in 0..rest / 258 {
        b.code(0b0, 1); // length 258
        b.code(0b0, 1); // distance 1
    }
    for _ in 0..rest % 258 {
        b.code(0b10, 2);
    }
    b.code(0b11, 2); // end of block
    let mut out = vec![0x78, 0x01];
    out.extend_from_slice(&b.finish());
    // Adler-32 of n zeros: a = 1, b = n mod 65521.
    let adler = (((n % 65_521) as u32) << 16) | 1;
    out.extend_from_slice(&adler.to_be_bytes());
    out
}

// ---------------------------------------------------------------------------
// Containers
// ---------------------------------------------------------------------------

fn bx(typ: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut v = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
    v.extend_from_slice(typ);
    v.extend_from_slice(payload);
    v
}

fn fullbox(typ: &[u8; 4], version: u8, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut p = vec![version];
    p.extend_from_slice(&flags.to_be_bytes()[1..]);
    p.extend_from_slice(payload);
    bx(typ, &p)
}

fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.concat()
}

fn ftyp(major: &[u8; 4], compat: &[&[u8; 4]]) -> Vec<u8> {
    let mut p = major.to_vec();
    p.extend_from_slice(&0x200u32.to_be_bytes());
    for c in compat {
        p.extend_from_slice(*c);
    }
    bx(b"ftyp", &p)
}

/// The unity matrix of `mvhd` and `tkhd`.
fn matrix() -> Vec<u8> {
    let mut m = Vec::new();
    for v in [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000] {
        m.extend_from_slice(&v.to_be_bytes());
    }
    m
}

fn mvhd() -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&[0; 8]); // creation, modification
    p.extend_from_slice(&1000u32.to_be_bytes()); // timescale
    p.extend_from_slice(&0u32.to_be_bytes()); // duration
    p.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // rate 1.0
    p.extend_from_slice(&0x0100u16.to_be_bytes()); // volume 1.0
    p.extend_from_slice(&[0; 10]);
    p.extend_from_slice(&matrix());
    p.extend_from_slice(&[0; 24]);
    p.extend_from_slice(&2u32.to_be_bytes()); // next track id
    fullbox(b"mvhd", 0, 0, &p)
}

fn tkhd() -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&[0; 8]);
    p.extend_from_slice(&1u32.to_be_bytes()); // track id
    p.extend_from_slice(&[0; 4]);
    p.extend_from_slice(&0u32.to_be_bytes()); // duration
    p.extend_from_slice(&[0; 8]);
    p.extend_from_slice(&[0; 4]); // layer, alternate group
    p.extend_from_slice(&[0; 4]); // volume, reserved
    p.extend_from_slice(&matrix());
    p.extend_from_slice(&[0; 8]); // width, height
    fullbox(b"tkhd", 0, 3, &p)
}

fn hdlr(handler: &[u8; 4]) -> Vec<u8> {
    let mut p = vec![0; 4];
    p.extend_from_slice(handler);
    p.extend_from_slice(&[0; 12]);
    p.extend_from_slice(b"SYNTH\0");
    fullbox(b"hdlr", 0, 0, &p)
}

/// `dref` with one `url ` entry: self-contained (flag 1, no location) when
/// `external` is `None`, else flag 0 and the location.
fn dref(external: Option<&[u8]>) -> Vec<u8> {
    let entry = match external {
        None => fullbox(b"url ", 0, 1, &[]),
        Some(loc) => {
            let mut l = loc.to_vec();
            l.push(0);
            fullbox(b"url ", 0, 0, &l)
        }
    };
    let mut p = 1u32.to_be_bytes().to_vec();
    p.extend_from_slice(&entry);
    fullbox(b"dref", 0, 0, &p)
}

fn stbl() -> Vec<u8> {
    let zero = 0u32.to_be_bytes();
    bx(
        b"stbl",
        &cat(&[
            fullbox(b"stsd", 0, 0, &zero),
            fullbox(b"stts", 0, 0, &zero),
            fullbox(b"stsc", 0, 0, &zero),
            fullbox(b"stsz", 0, 0, &[0; 8]),
            fullbox(b"stco", 0, 0, &zero),
        ]),
    )
}

fn moov(handler: &[u8; 4], external: Option<&[u8]>) -> Vec<u8> {
    let mut mdhd = Vec::new();
    mdhd.extend_from_slice(&[0; 8]);
    mdhd.extend_from_slice(&1000u32.to_be_bytes());
    mdhd.extend_from_slice(&0u32.to_be_bytes());
    mdhd.extend_from_slice(&0x55c4u16.to_be_bytes()); // language "und"
    mdhd.extend_from_slice(&[0; 2]);
    let minf = bx(b"minf", &cat(&[bx(b"dinf", &dref(external)), stbl()]));
    let mdia = bx(
        b"mdia",
        &cat(&[fullbox(b"mdhd", 0, 0, &mdhd), hdlr(handler), minf]),
    );
    let trak = bx(b"trak", &cat(&[tkhd(), mdia]));
    bx(b"moov", &cat(&[mvhd(), trak]))
}

fn mp4(
    major: &[u8; 4],
    compat: &[&[u8; 4]],
    handler: &[u8; 4],
    external: Option<&[u8]>,
) -> Vec<u8> {
    cat(&[
        ftyp(major, compat),
        moov(handler, external),
        bx(b"mdat", b"SYNTHMDT"),
    ])
}

fn mov_reference_movie() -> Vec<u8> {
    let url = b"http://127.0.0.1:9/SYNTH-REF.mov\0";
    let mut rdrf = Vec::new();
    rdrf.extend_from_slice(b"url ");
    rdrf.extend_from_slice(&(url.len() as u32).to_be_bytes());
    rdrf.extend_from_slice(url);
    let rmda = bx(b"rmda", &fullbox(b"rdrf", 0, 0, &rdrf));
    let rmra = bx(b"rmra", &rmda);
    cat(&[ftyp(b"qt  ", &[b"qt  "]), bx(b"moov", &rmra)])
}

fn mp4_box_past_eof() -> Vec<u8> {
    let mut moov = moov(b"vide", None);
    moov[..4].copy_from_slice(&4096u32.to_be_bytes());
    cat(&[ftyp(b"isom", &[b"isom", b"iso2", b"mp41"]), moov])
}

fn mp4_64bit_overflow() -> Vec<u8> {
    let mut big = 1u32.to_be_bytes().to_vec();
    big.extend_from_slice(b"mdat");
    big.extend_from_slice(&u64::MAX.to_be_bytes());
    big.extend_from_slice(b"SYNTHMDT");
    cat(&[
        ftyp(b"isom", &[b"isom", b"iso2", b"mp41"]),
        moov(b"vide", None),
        big,
    ])
}

/// RIFF WAVE: `fmt ` (PCM, mono, 8000 Hz, 16-bit) and `data` holding
/// `data_len` zero bytes; `claimed` overrides the data chunk's size.
fn wav(data_len: u32, claimed: Option<u32>) -> Vec<u8> {
    let mut fmt = Vec::new();
    fmt.extend_from_slice(&1u16.to_le_bytes()); // PCM
    fmt.extend_from_slice(&1u16.to_le_bytes()); // channels
    fmt.extend_from_slice(&8000u32.to_le_bytes());
    fmt.extend_from_slice(&16000u32.to_le_bytes()); // byte rate
    fmt.extend_from_slice(&2u16.to_le_bytes()); // block align
    fmt.extend_from_slice(&16u16.to_le_bytes()); // bits
    let mut body = b"WAVE".to_vec();
    body.extend_from_slice(b"fmt ");
    body.extend_from_slice(&(fmt.len() as u32).to_le_bytes());
    body.extend_from_slice(&fmt);
    body.extend_from_slice(b"data");
    body.extend_from_slice(&claimed.unwrap_or(data_len).to_le_bytes());
    body.extend_from_slice(&vec![0; data_len as usize]);
    let mut v = b"RIFF".to_vec();
    v.extend_from_slice(&(body.len() as u32).to_le_bytes());
    v.extend_from_slice(&body);
    v
}

/// CAF (big-endian): the file header, `desc` (8000 Hz linear PCM, mono,
/// 16-bit) and `data` (edit count, then 16 zero bytes).
fn caf() -> Vec<u8> {
    let mut v = b"caff".to_vec();
    v.extend_from_slice(&1u16.to_be_bytes());
    v.extend_from_slice(&0u16.to_be_bytes());
    let mut desc = 8000f64.to_be_bytes().to_vec();
    desc.extend_from_slice(b"lpcm");
    for x in [0u32, 2, 1, 1, 16] {
        desc.extend_from_slice(&x.to_be_bytes());
    }
    v.extend_from_slice(b"desc");
    v.extend_from_slice(&(desc.len() as i64).to_be_bytes());
    v.extend_from_slice(&desc);
    let mut data = 0u32.to_be_bytes().to_vec();
    data.extend_from_slice(&[0; 16]);
    v.extend_from_slice(b"data");
    v.extend_from_slice(&(data.len() as i64).to_be_bytes());
    v.extend_from_slice(&data);
    v
}

fn heic() -> Vec<u8> {
    cat(&[
        ftyp(b"heic", &[b"mif1", b"heic"]),
        fullbox(b"meta", 0, 0, &hdlr(b"pict")),
    ])
}

// ---------------------------------------------------------------------------
// Text files
// ---------------------------------------------------------------------------

const SVG_SCRIPT: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"10\" height=\"10\">\
<script>alert('SYNTH-SVG-SCRIPT')</script>\
<rect width=\"10\" height=\"10\" fill=\"#808080\"/></svg>\n";

const HTML_PAGE: &str = "<!DOCTYPE html>\n<html><head><title>SYNTH</title></head><body>\
<img src=\"x\" onerror=\"alert('SYNTH-ONERROR')\">\
<script>alert('SYNTH-HTML-SCRIPT')</script></body></html>\n";

const PDF: &str = "%PDF-1.4\n%SYNTH\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n\
2 0 obj << /Type /Pages /Kids [] /Count 0 >> endobj\n\
trailer << /Root 1 0 R >>\n%%EOF\n";

const VCARD: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:SYNTH-PERSON-VCARD\r\n\
EMAIL:synthetic-vcard@example.invalid\r\nEND:VCARD\r\n";
