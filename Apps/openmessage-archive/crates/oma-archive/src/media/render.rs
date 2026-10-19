//! Image renditions (M5 spec §5.5, D98, D113): verified bytes in, an image
//! the app produced out.
//!
//! [`render`] is a **pure function**: it takes the bytes that
//! `read_blob_verified` read to the end through the hashing reader, and
//! returns a re-encoded rendition. It opens nothing, logs nothing and keeps
//! nothing. The shell calls it **only inside the decode helper process**
//! (`--oma-decode-helper`, slice 5f), never in the main process, so a
//! decoder panic, an allocation failure or a memory-safety bug in a
//! decoder's `unsafe` SIMD code ends one helper, not the app (the release
//! profile aborts on panic). The helper is not an OS sandbox.
//!
//! **Limits, in order, before any pixel is decoded:**
//!
//! 1. Input over [`MAX_INPUT_BYTES`] (64 MiB) is refused ("Too large to
//!    preview").
//! 2. Only the sniffed PNG, JPEG, GIF and WebP are decoded (and HEIF in a
//!    build with the `heic` feature); the stored type decides nothing.
//! 3. A side over [`MAX_SIDE`] (16384 px) is refused, from the header.
//! 4. `width × height × bytes per pixel` of the decoder's output colour type
//!    over [`MAX_DECODED_BYTES`] (128 MiB) is refused, from the header: 32 MP
//!    for RGBA8, about 44 MP for RGB8, 16 MP for RGBA16.
//! 5. The decoder runs under `image::Limits` with the same side limit and
//!    an allocation limit of [`DECODER_ALLOC`] (160 MiB), headroom for its
//!    own buffers.
//!
//! **Downscale, then orient.** The decoded image is resized to the
//! rendition's long edge first (640 px for `thumb`, 2048 px for `display`;
//! never enlarged), and the EXIF orientation the decoder reports is applied
//! to the small image, so the orientation copy is at most 2048 × 2048 × 4
//! bytes (16 MiB).
//!
//! **Output** is a JPEG at quality 85, or a PNG when the decoder's colour
//! type has alpha. Encoding writes no metadata, so EXIF, GPS and every
//! other block of the original are dropped. A GIF yields its first frame,
//! with `animated` set when the file has more than one; an animated WebP
//! (the `VP8X` animation flag) yields what the decoder gives for it and the
//! same flag. There is no colour management.
//!
//! **Decoding is not integrity.** A JPEG truncated to a third decodes
//! "successfully"; only the hash, checked before this function runs, says
//! the bytes are the ones imported.
//!
//! **HEIC (owner decision Q2).** With the non-default `heic` feature, HEIF
//! is decoded by the pure-Rust `heic-rs` (no `unsafe`, no C). Official and
//! CI release builds never enable it: HEVC is pooled-patent technology
//! (docs/privacy.md). That decoder applies the file's `clap`, `irot` and
//! `imir` at full size, so for HEIF both the decoded image and its
//! transformed copy are each held to half of [`MAX_DECODED_BYTES`]. Without
//! the feature HEIF is [`RenderError::NotAnImage`] (a file card).

use std::io::Cursor;

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::error::{ImageError, LimitErrorKind};
use image::imageops::FilterType;
use image::metadata::Orientation;
use image::{
    DynamicImage, ExtendedColorType, ImageDecoder, ImageEncoder, ImageFormat, ImageReader,
};
use oma_messages::Redacted;

use super::sniff::{sniff, SniffedKind};

/// The largest input decoded: 64 MiB.
pub const MAX_INPUT_BYTES: u64 = 64 << 20;
/// The largest decoded image: 128 MiB of output pixels.
pub const MAX_DECODED_BYTES: u64 = 128 << 20;
/// The longest side decoded, in pixels.
pub const MAX_SIDE: u32 = 16_384;
/// The decoder's own allocation limit (`image::Limits::max_alloc`).
pub const DECODER_ALLOC: u64 = 160 << 20;
/// The JPEG quality of an opaque rendition.
pub const JPEG_QUALITY: u8 = 85;

/// Which rendition to make.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Rendition {
    /// 640 px on the longest edge.
    Thumb,
    /// 2048 px on the longest edge.
    Display,
}

impl Rendition {
    /// The longest edge of the output; smaller images keep their size.
    pub const fn long_edge(self) -> u32 {
        match self {
            Rendition::Thumb => 640,
            Rendition::Display => 2048,
        }
    }
}

/// The bounds one render runs under. [`RenderLimits::default`] is the
/// app's (§5.5); tests may tighten them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct RenderLimits {
    pub max_input_bytes: u64,
    pub max_decoded_bytes: u64,
    pub max_side: u32,
    pub decoder_alloc: u64,
}

impl Default for RenderLimits {
    fn default() -> RenderLimits {
        RenderLimits {
            max_input_bytes: MAX_INPUT_BYTES,
            max_decoded_bytes: MAX_DECODED_BYTES,
            max_side: MAX_SIDE,
            decoder_alloc: DECODER_ALLOC,
        }
    }
}

impl RenderLimits {
    /// The defaults with a lower decoded-bytes cap (and a decoder
    /// allocation limit to match).
    pub fn with_decoded_cap(bytes: u64) -> RenderLimits {
        RenderLimits {
            max_decoded_bytes: bytes,
            decoder_alloc: bytes.saturating_add(bytes / 4),
            ..RenderLimits::default()
        }
    }
}

/// The encoding of a rendition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RenderedFormat {
    Jpeg,
    Png,
}

impl RenderedFormat {
    /// Its `Content-Type`.
    pub const fn mime(self) -> &'static str {
        match self {
            RenderedFormat::Jpeg => "image/jpeg",
            RenderedFormat::Png => "image/png",
        }
    }
}

/// A rendition the app produced. `Debug` prints the encoded length only.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Rendered {
    pub format: RenderedFormat,
    /// The rendition's size.
    pub width: u32,
    pub height: u32,
    /// The original's size as shown (after orientation): "original W×H".
    pub source_width: u32,
    pub source_height: u32,
    /// The decoder's colour type had alpha (so the output is a PNG).
    pub has_alpha: bool,
    /// More than one frame (a GIF, or a WebP with the animation flag): the
    /// first frame only, with an "Animated" badge.
    pub animated: bool,
    /// The encoded rendition.
    pub bytes: Redacted<Vec<u8>>,
}

/// Why no rendition was made. Each is a closed code the decode helper
/// returns as its status ([`RenderError::code`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RenderError {
    /// Over [`RenderLimits::max_input_bytes`]: "Too large to preview".
    InputTooLarge,
    /// The bytes are not a format this build decodes.
    NotAnImage,
    /// A side over [`RenderLimits::max_side`].
    DimensionsTooLarge,
    /// The decoded image would be over [`RenderLimits::max_decoded_bytes`],
    /// or the decoder ran into its allocation limit.
    DecodedTooLarge,
    /// The decoder refused the bytes.
    Malformed,
    /// Re-encoding failed.
    Encode,
}

impl RenderError {
    /// Every error, in code order.
    pub const ALL: [RenderError; 6] = [
        RenderError::InputTooLarge,
        RenderError::NotAnImage,
        RenderError::DimensionsTooLarge,
        RenderError::DecodedTooLarge,
        RenderError::Malformed,
        RenderError::Encode,
    ];

    /// The helper protocol's status code (0 is success).
    pub const fn code(self) -> u8 {
        match self {
            RenderError::InputTooLarge => 1,
            RenderError::NotAnImage => 2,
            RenderError::DimensionsTooLarge => 3,
            RenderError::DecodedTooLarge => 4,
            RenderError::Malformed => 5,
            RenderError::Encode => 6,
        }
    }

    /// The error with this status code.
    pub fn from_code(code: u8) -> Option<RenderError> {
        RenderError::ALL.into_iter().find(|e| e.code() == code)
    }
}

fn decode_error(e: &ImageError) -> RenderError {
    match e {
        ImageError::Limits(l) => match l.kind() {
            LimitErrorKind::DimensionError => RenderError::DimensionsTooLarge,
            _ => RenderError::DecodedTooLarge,
        },
        _ => RenderError::Malformed,
    }
}

/// Make `rendition` of `bytes` under `limits` (see the module docs).
pub fn render(
    bytes: &[u8],
    rendition: Rendition,
    limits: &RenderLimits,
) -> Result<Rendered, RenderError> {
    let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if len > limits.max_input_bytes {
        return Err(RenderError::InputTooLarge);
    }
    let (format, animated) = match sniff(bytes, len) {
        SniffedKind::Png => (ImageFormat::Png, false),
        SniffedKind::Jpeg => (ImageFormat::Jpeg, false),
        SniffedKind::Gif => (ImageFormat::Gif, gif_frames(bytes) > 1),
        SniffedKind::Webp => (ImageFormat::WebP, webp_animated(bytes)),
        #[cfg(feature = "heic")]
        SniffedKind::Heif => return heif::render(bytes, rendition, limits),
        _ => return Err(RenderError::NotAnImage),
    };
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut l = image::Limits::default();
    l.max_image_width = Some(limits.max_side);
    l.max_image_height = Some(limits.max_side);
    l.max_alloc = Some(limits.decoder_alloc);
    reader.limits(l);
    let mut decoder = reader.into_decoder().map_err(|e| decode_error(&e))?;
    let (w, h) = decoder.dimensions();
    if w == 0 || h == 0 {
        return Err(RenderError::Malformed);
    }
    if w > limits.max_side || h > limits.max_side {
        return Err(RenderError::DimensionsTooLarge);
    }
    let color = decoder.color_type();
    let decoded = u64::from(w)
        .saturating_mul(u64::from(h))
        .saturating_mul(u64::from(color.bytes_per_pixel()));
    if decoded > limits.max_decoded_bytes || decoder.total_bytes() > limits.max_decoded_bytes {
        return Err(RenderError::DecodedTooLarge);
    }
    // An orientation that cannot be read is no orientation.
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let img = DynamicImage::from_decoder(decoder).map_err(|e| decode_error(&e))?;
    finish(img, orientation, color.has_alpha(), animated, rendition)
}

/// Downscale, orient and encode.
fn finish(
    img: DynamicImage,
    orientation: Orientation,
    has_alpha: bool,
    animated: bool,
    rendition: Rendition,
) -> Result<Rendered, RenderError> {
    let (w, h) = (img.width(), img.height());
    let edge = rendition.long_edge();
    // `resize` keeps the aspect ratio inside an edge × edge box; the
    // full-size image is dropped before the orientation copy is made.
    let mut small = if w.max(h) > edge {
        img.resize(edge, edge, FilterType::Triangle)
    } else {
        img
    };
    small.apply_orientation(orientation);
    let swaps = matches!(
        orientation,
        Orientation::Rotate90
            | Orientation::Rotate270
            | Orientation::Rotate90FlipH
            | Orientation::Rotate270FlipH
    );
    let (source_width, source_height) = if swaps { (h, w) } else { (w, h) };
    let (width, height) = (small.width(), small.height());
    let mut out = Vec::new();
    let format = if has_alpha {
        let rgba = small.into_rgba8();
        PngEncoder::new(&mut out)
            .write_image(rgba.as_raw(), width, height, ExtendedColorType::Rgba8)
            .map_err(|_| RenderError::Encode)?;
        RenderedFormat::Png
    } else {
        let rgb = small.into_rgb8();
        JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY)
            .encode_image(&rgb)
            .map_err(|_| RenderError::Encode)?;
        RenderedFormat::Jpeg
    };
    Ok(Rendered {
        format,
        width,
        height,
        source_width,
        source_height,
        has_alpha,
        animated,
        bytes: Redacted::new(out),
    })
}

/// The number of image descriptors in a GIF, counted up to 2 by walking
/// its blocks (header, logical screen, colour tables, extensions and
/// image data). Nothing is decoded; a malformed stream counts what it
/// reached.
fn gif_frames(b: &[u8]) -> u32 {
    fn table(flags: u8) -> usize {
        if flags & 0x80 != 0 {
            3usize << ((flags & 0x07) + 1)
        } else {
            0
        }
    }
    fn skip_sub_blocks(b: &[u8], mut pos: usize) -> Option<usize> {
        loop {
            let n = usize::from(*b.get(pos)?);
            pos = pos.checked_add(1)?;
            if n == 0 {
                return Some(pos);
            }
            pos = pos.checked_add(n)?;
        }
    }
    let Some(&screen) = b.get(10) else {
        return 0;
    };
    let mut pos = 13 + table(screen);
    let mut frames = 0;
    while frames < 2 {
        match b.get(pos) {
            Some(0x21) => {
                // Extension introducer, label, then sub-blocks.
                match skip_sub_blocks(b, pos + 2) {
                    Some(p) => pos = p,
                    None => break,
                }
            }
            Some(0x2C) => {
                frames += 1;
                let Some(&local) = b.get(pos + 9) else {
                    break;
                };
                // Descriptor (10), local table, LZW minimum code size (1).
                match skip_sub_blocks(b, pos + 10 + table(local) + 1) {
                    Some(p) => pos = p,
                    None => break,
                }
            }
            _ => break,
        }
    }
    frames
}

/// The `VP8X` animation flag of an extended WebP.
fn webp_animated(b: &[u8]) -> bool {
    b.get(12..16) == Some(b"VP8X") && b.get(20).is_some_and(|f| f & 0x02 != 0)
}

#[cfg(feature = "heic")]
mod heif {
    use image::{DynamicImage, RgbImage, RgbaImage};

    use super::{finish, Orientation, RenderError, RenderLimits, Rendered, Rendition};

    /// HEIF through `heic-rs` (owner decision Q2; the `heic` feature only).
    pub(super) fn render(
        bytes: &[u8],
        rendition: Rendition,
        limits: &RenderLimits,
    ) -> Result<Rendered, RenderError> {
        let info = heic_rs::probe(bytes).map_err(|_| RenderError::Malformed)?;
        let sides = [info.width, info.height, info.coded_width, info.coded_height];
        if sides.contains(&0) {
            return Err(RenderError::Malformed);
        }
        if sides.iter().any(|s| *s > limits.max_side) {
            return Err(RenderError::DimensionsTooLarge);
        }
        let alpha = info.has_alpha;
        let bpp: u64 = if alpha { 4 } else { 3 };
        // The decoded image and its transformed copy are both full size.
        let per_copy = limits.max_decoded_bytes / 2;
        let coded = u64::from(info.coded_width) * u64::from(info.coded_height) * bpp;
        let shown = u64::from(info.width) * u64::from(info.height) * bpp;
        if coded > per_copy || shown > per_copy {
            return Err(RenderError::DecodedTooLarge);
        }
        let layout = if alpha {
            heic_rs::PixelLayout::Rgba8
        } else {
            heic_rs::PixelLayout::Rgb8
        };
        let options = heic_rs::DecodeOptions::default()
            .with_layout(layout)
            .with_max_pixels(Some(per_copy / bpp))
            .with_transforms(true)
            .with_threads(Some(1));
        let img = heic_rs::decode(bytes, &options).map_err(|e| match e {
            heic_rs::Error::PixelLimit { .. } => RenderError::DecodedTooLarge,
            _ => RenderError::Malformed,
        })?;
        let dynamic = if alpha {
            RgbaImage::from_raw(img.width, img.height, img.data).map(DynamicImage::ImageRgba8)
        } else {
            RgbImage::from_raw(img.width, img.height, img.data).map(DynamicImage::ImageRgb8)
        }
        .ok_or(RenderError::Malformed)?;
        finish(dynamic, Orientation::NoTransforms, alpha, false, rendition)
    }
}

#[cfg(test)]
mod tests {
    use image::{Rgb, RgbImage, Rgba, RgbaImage};

    use super::*;

    fn jpeg(img: &RgbImage) -> Vec<u8> {
        let mut out = Vec::new();
        JpegEncoder::new_with_quality(&mut out, 95)
            .encode_image(img)
            .expect("encode");
        out
    }

    fn png_rgba(img: &RgbaImage) -> Vec<u8> {
        let mut out = Vec::new();
        PngEncoder::new(&mut out)
            .write_image(
                img.as_raw(),
                img.width(),
                img.height(),
                ExtendedColorType::Rgba8,
            )
            .expect("encode");
        out
    }

    /// A big-endian EXIF APP1 segment with one Orientation entry, spliced
    /// in after SOI.
    fn with_orientation(jpeg: &[u8], o: u16) -> Vec<u8> {
        let mut tiff = b"MM\x00\x2a\x00\x00\x00\x08".to_vec();
        tiff.extend_from_slice(&1u16.to_be_bytes());
        tiff.extend_from_slice(&0x0112u16.to_be_bytes());
        tiff.extend_from_slice(&3u16.to_be_bytes());
        tiff.extend_from_slice(&1u32.to_be_bytes());
        tiff.extend_from_slice(&o.to_be_bytes());
        tiff.extend_from_slice(&[0, 0]);
        tiff.extend_from_slice(&0u32.to_be_bytes());
        let mut payload = b"Exif\0\0".to_vec();
        payload.extend_from_slice(&tiff);
        let mut out = jpeg[..2].to_vec();
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(&payload);
        out.extend_from_slice(&jpeg[2..]);
        out
    }

    fn decode(r: &Rendered) -> DynamicImage {
        image::load_from_memory(r.bytes.expose()).expect("the rendition decodes")
    }

    #[test]
    fn exif_orientations_are_applied_after_downscaling() {
        // 800 × 400, red in the top-left quarter.
        let img = RgbImage::from_fn(800, 400, |x, y| {
            if x < 400 && y < 200 {
                Rgb([255, 0, 0])
            } else {
                Rgb([0, 0, 255])
            }
        });
        let base = jpeg(&img);
        for o in 1..=8u16 {
            let r = render(
                &with_orientation(&base, o),
                Rendition::Thumb,
                &RenderLimits::default(),
            )
            .expect("render");
            let swapped = o >= 5;
            let (w, h) = if swapped { (320, 640) } else { (640, 320) };
            assert_eq!((r.width, r.height), (w, h), "orientation {o}");
            let (sw, sh) = if swapped { (400, 800) } else { (800, 400) };
            assert_eq!(
                (r.source_width, r.source_height),
                (sw, sh),
                "orientation {o}"
            );
            assert_eq!(r.format, RenderedFormat::Jpeg);
            // Where the red quarter lands.
            let out = decode(&r).to_rgb8();
            let red_at = |x: u32, y: u32| {
                let p = out.get_pixel(x, y);
                p[0] > 200 && p[2] < 60
            };
            let (x0, y0, x1, y1) = (10, 10, w - 10, h - 10);
            let corner = [
                red_at(x0, y0),
                red_at(x1, y0),
                red_at(x0, y1),
                red_at(x1, y1),
            ];
            // Where the stored top-left corner is shown, for EXIF 1..8:
            // corners are [top-left, top-right, bottom-left, bottom-right].
            let want = match o {
                1 | 5 => [true, false, false, false],
                2 | 6 => [false, true, false, false],
                4 | 8 => [false, false, true, false],
                _ => [false, false, false, true],
            };
            assert_eq!(corner, want, "orientation {o}");
            // No EXIF survives.
            assert!(
                !r.bytes.expose().windows(6).any(|w| w == b"Exif\0\0"),
                "orientation {o}"
            );
        }
    }

    #[test]
    fn alpha_gives_png_and_opaque_gives_jpeg_without_enlarging() {
        let rgba = RgbaImage::from_pixel(5, 3, Rgba([1, 2, 3, 100]));
        let r = render(
            &png_rgba(&rgba),
            Rendition::Display,
            &RenderLimits::default(),
        )
        .expect("render");
        assert_eq!(r.format, RenderedFormat::Png);
        assert!(r.has_alpha && !r.animated);
        assert_eq!((r.width, r.height), (5, 3));
        assert!(r.bytes.expose().starts_with(b"\x89PNG"));
        let rgb = RgbImage::from_pixel(3000, 1000, Rgb([9, 9, 9]));
        let r = render(&jpeg(&rgb), Rendition::Display, &RenderLimits::default()).expect("jpeg");
        assert_eq!(r.format, RenderedFormat::Jpeg);
        assert_eq!((r.width, r.height), (2048, 683));
        assert_eq!((r.source_width, r.source_height), (3000, 1000));
        assert!(r.bytes.expose().starts_with(b"\xff\xd8\xff"));
        // Debug prints a length, never bytes.
        assert!(format!("{r:?}").contains("redacted"));
    }

    #[test]
    fn limits_refuse_before_decoding() {
        let rgb = RgbImage::from_pixel(64, 64, Rgb([1, 1, 1]));
        let data = jpeg(&rgb);
        let tight = RenderLimits {
            max_input_bytes: data.len() as u64 - 1,
            ..RenderLimits::default()
        };
        assert_eq!(
            render(&data, Rendition::Thumb, &tight),
            Err(RenderError::InputTooLarge)
        );
        let side = RenderLimits {
            max_side: 63,
            ..RenderLimits::default()
        };
        assert_eq!(
            render(&data, Rendition::Thumb, &side),
            Err(RenderError::DimensionsTooLarge)
        );
        // 64 × 64 × 3 = 12,288 bytes.
        assert_eq!(
            render(
                &data,
                Rendition::Thumb,
                &RenderLimits::with_decoded_cap(12_287)
            ),
            Err(RenderError::DecodedTooLarge)
        );
        assert!(render(
            &data,
            Rendition::Thumb,
            &RenderLimits::with_decoded_cap(12_288)
        )
        .is_ok());
    }

    #[test]
    fn other_kinds_are_not_images() {
        for bytes in [
            &b""[..],
            b"\x00",
            b"<html><img src=x onerror=alert(1)>",
            b"<?xml version=\"1.0\"?><svg><script>x</script></svg>",
            b"BM\x00\x00\x00\x00",
            b"%PDF-1.4",
        ] {
            assert_eq!(
                render(bytes, Rendition::Thumb, &RenderLimits::default()),
                Err(RenderError::NotAnImage),
                "{bytes:?}"
            );
        }
        // A PNG signature with nothing after it.
        assert_eq!(
            render(
                b"\x89PNG\r\n\x1a\n",
                Rendition::Thumb,
                &RenderLimits::default()
            ),
            Err(RenderError::Malformed)
        );
    }

    #[test]
    fn gif_frames_and_webp_animation_flag() {
        // Header, screen 1×1 with a 2-entry table, one image, trailer.
        let one: Vec<u8> = [
            &b"GIF89a\x01\x00\x01\x00\x80\x00\x00"[..],
            &[0, 0, 0, 255, 255, 255],
            &[0x21, 0xF9, 4, 0, 0, 0, 0, 0],
            &[0x2C, 0, 0, 0, 0, 1, 0, 1, 0, 0],
            &[2, 2, 0x44, 0x01, 0],
            &[0x3B],
        ]
        .concat();
        assert_eq!(gif_frames(&one), 1);
        let two: Vec<u8> = [
            &one[..one.len() - 1],
            &[0x2C, 0, 0, 0, 0, 1, 0, 1, 0, 0],
            &[2, 2, 0x44, 0x01, 0],
            &[0x3B],
        ]
        .concat();
        assert_eq!(gif_frames(&two), 2);
        assert_eq!(gif_frames(b"GIF89a"), 0);
        assert_eq!(gif_frames(&one[..20]), 0);
        let r = render(&two, Rendition::Thumb, &RenderLimits::default()).expect("gif");
        assert!(r.animated);
        assert_eq!((r.width, r.height), (1, 1));
        let r = render(&one, Rendition::Thumb, &RenderLimits::default()).expect("gif");
        assert!(!r.animated);
        let mut vp8x = b"RIFF\0\0\0\0WEBPVP8X\x0a\0\0\0".to_vec();
        vp8x.push(0x02);
        assert!(webp_animated(&vp8x));
        vp8x[20] = 0x10;
        assert!(!webp_animated(&vp8x));
    }

    #[test]
    fn error_codes_round_trip() {
        for e in RenderError::ALL {
            assert_ne!(e.code(), 0);
            assert_eq!(RenderError::from_code(e.code()), Some(e));
        }
        assert_eq!(RenderError::from_code(0), None);
        assert_eq!(RenderError::from_code(99), None);
    }

    #[cfg(not(feature = "heic"))]
    #[test]
    fn heif_is_a_file_card_without_the_feature() {
        let mut heif = 24u32.to_be_bytes().to_vec();
        heif.extend_from_slice(b"ftypheic\0\0\0\0mif1heic");
        assert_eq!(
            render(&heif, Rendition::Thumb, &RenderLimits::default()),
            Err(RenderError::NotAnImage)
        );
    }

    #[cfg(feature = "heic")]
    #[test]
    fn heif_without_an_image_is_malformed() {
        let mut heif = 24u32.to_be_bytes().to_vec();
        heif.extend_from_slice(b"ftypheic\0\0\0\0mif1heic");
        assert_eq!(
            render(&heif, Rendition::Thumb, &RenderLimits::default()),
            Err(RenderError::Malformed)
        );
    }
}
