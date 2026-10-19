//! The bounded render corpus (M5 spec §13.1, slice 5d).
//!
//! `media::render::render` runs on every file of the viewer media
//! catalogue (the hostile ones included) at the app's limits, and on 200
//! seeded mutations of each valid image at a tightened decoded-bytes cap
//! (8 MiB, so a mutated header cannot ask a debug build for a 128 MiB
//! decode). Every call must return a rendition or one of the helper's
//! closed error codes, within a per-input wall-time cap: a panic fails
//! the test. A rendition is always a well-formed image of at most the
//! rendition's size. Mutated PNGs have their chunk CRCs repaired, so the
//! mutations reach the decoder instead of stopping at the CRC check.
#![allow(clippy::disallowed_methods)]

#[path = "../examples/make_viewer_media/media.rs"]
mod media;

use std::time::{Duration, Instant};

use oma_archive::media::render::{render, RenderLimits, Rendition};
use oma_archive::{RenderError, SniffedKind};

/// The per-input wall-time cap. Generous for an unoptimised build of the
/// decoders on a slow runner; inputs are tiny or refused from the header.
const PER_INPUT: Duration = Duration::from_secs(20);
const MUTATIONS: u32 = 200;
/// The valid images the mutations start from.
const SEEDS: &[&str] = &[
    "png-rgb-6x4.png",
    "png-alpha-4x4.png",
    "jpeg-exif6-gps-8x6.jpg",
    "gif-2frames-4x4.gif",
    "gif-html-polyglot.gif",
    "webp-lossless-4x4.webp",
];

/// xorshift64*: deterministic, seeded per input.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// One seeded mutation of `input`: 1 to 4 edits among byte flips,
/// interesting values, 32-bit size-like overwrites, truncation and a
/// duplicated run.
fn mutate(input: &[u8], rng: &mut Rng) -> Vec<u8> {
    let mut v = input.to_vec();
    for _ in 0..1 + rng.below(4) {
        if v.is_empty() {
            break;
        }
        let at = rng.below(v.len());
        match rng.below(6) {
            0 => v[at] ^= 1 << rng.below(8),
            1 => v[at] = [0x00, 0xFF, 0x7F, 0x80, 0x01][rng.below(5)],
            2 => {
                let word: u32 =
                    [0xFFFF_FFFF, 0x7FFF_FFFF, 0x0001_0000, 0x0000_4000, 0][rng.below(5)];
                for (i, b) in word.to_be_bytes().into_iter().enumerate() {
                    if let Some(x) = v.get_mut(at + i) {
                        *x = b;
                    }
                }
            }
            3 => v.truncate(at.max(1)),
            4 => {
                let end = (at + 1 + rng.below(16)).min(v.len());
                let run = v[at..end].to_vec();
                v.splice(at..at, run);
            }
            _ => v[at] = rng.next() as u8,
        }
    }
    v
}

/// Recompute every PNG chunk's CRC after a mutation.
fn fix_png_crcs(v: &mut [u8]) {
    let mut pos = 8;
    while pos + 12 <= v.len() {
        let len = u32::from_be_bytes([v[pos], v[pos + 1], v[pos + 2], v[pos + 3]]) as usize;
        let Some(end) = (pos + 8).checked_add(len) else {
            return;
        };
        if end + 4 > v.len() {
            return;
        }
        let crc = media::crc32(&v[pos + 4..end]);
        v[end..end + 4].copy_from_slice(&crc.to_be_bytes());
        pos = end + 4;
    }
}

fn run(name: &str, bytes: &[u8], limits: &RenderLimits, outcomes: &mut Vec<String>) {
    for rendition in [Rendition::Thumb, Rendition::Display] {
        let start = Instant::now();
        let got = render(bytes, rendition, limits);
        let took = start.elapsed();
        assert!(took < PER_INPUT, "{name}: {took:?}");
        match got {
            Ok(r) => {
                let edge = rendition.long_edge();
                assert!(r.width >= 1 && r.height >= 1, "{name}");
                assert!(r.width <= edge && r.height <= edge, "{name}");
                let back = image::load_from_memory(r.bytes.expose()).expect("re-decodes");
                assert_eq!((back.width(), back.height()), (r.width, r.height), "{name}");
                outcomes.push("ok".to_owned());
            }
            Err(e) => {
                assert!(RenderError::ALL.contains(&e), "{name}: {e:?}");
                assert_eq!(RenderError::from_code(e.code()), Some(e));
                outcomes.push(format!("{e:?}"));
            }
        }
    }
}

#[test]
fn every_catalogue_file_renders_or_fails_with_a_code() {
    let mut outcomes = Vec::new();
    for f in media::all() {
        run(f.name, &f.bytes, &RenderLimits::default(), &mut outcomes);
    }
    for want in ["ok", "NotAnImage", "DecodedTooLarge", "DimensionsTooLarge"] {
        assert!(outcomes.iter().any(|o| o == want), "{want}: {outcomes:?}");
    }
}

#[test]
fn seeded_mutations_render_or_fail_with_a_code() {
    let all = media::all();
    let limits = RenderLimits::with_decoded_cap(8 << 20);
    let mut outcomes = Vec::new();
    for (n, seed) in SEEDS.iter().enumerate() {
        let file = all.iter().find(|f| f.name == *seed).expect("seed file");
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ (n as u64 + 1));
        let png =
            oma_archive::media::sniff(&file.bytes, file.bytes.len() as u64) == SniffedKind::Png;
        for i in 0..MUTATIONS {
            let mut m = mutate(&file.bytes, &mut rng);
            if png && i % 2 == 0 {
                fix_png_crcs(&mut m);
            }
            run(&format!("{seed} #{i}"), &m, &limits, &mut outcomes);
        }
    }
    assert_eq!(outcomes.len(), SEEDS.len() * MUTATIONS as usize * 2);
    // The corpus reaches both the decoders and their refusals.
    assert!(outcomes.iter().any(|o| o == "ok"));
    assert!(outcomes.iter().any(|o| o == "Malformed"));
}

/// The cost of one large photo (M5 §5.5, §11): a 4032 × 3024 noise JPEG
/// (a worst case for the decoder), made into both renditions. Run by hand,
/// under the release profile:
/// `cargo test --release -p oma-archive --test media_corpus -- --ignored --nocapture`.
#[test]
#[ignore = "performance: run by hand under --release"]
fn perf_render_a_large_photo() {
    use image::codecs::jpeg::JpegEncoder;
    let (w, h) = (4032u32, 3024u32);
    let mut state = 0x1234_5678_9ABC_DEF0u64;
    let noise = image::RgbImage::from_fn(w, h, |_, _| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let b = state.to_le_bytes();
        image::Rgb([b[0], b[1], b[2]])
    });
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, 90)
        .encode_image(&noise)
        .expect("encode");
    for rendition in [Rendition::Thumb, Rendition::Display] {
        let start = Instant::now();
        let r = render(&jpeg, rendition, &RenderLimits::default()).expect("render");
        eprintln!(
            "RECORD render {rendition:?}: input {} bytes, output {}x{} {} bytes, {:?}",
            jpeg.len(),
            r.width,
            r.height,
            r.bytes.expose().len(),
            start.elapsed()
        );
    }
}
