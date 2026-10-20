//! The PDF backend's layout and serialisation (M6 slice 6j1, spec §5.6,
//! §4.7, §14.2): the pinned fonts, shaping and fallback, bidi, wrapping,
//! `.notdef` counting, header isolates, the marker rule in the text layer,
//! and a whole session through the helper's `serve` loop in-process.
//!
//! The fonts are the committed files under `src-tauri/resources/fonts/`.
//! `write_samples` (ignored) writes the PDFs and expectations that
//! `scripts/oracles/pdf_text.py` checks with pypdf and `pdftotext`.

#![allow(clippy::disallowed_methods)] // write_samples writes the oracle's files

use std::path::{Path, PathBuf};

use oma_export::check::{check_output, OutputKind};
use oma_export::helper::frame::{
    read_frame, write_frame, BackendKind, Begin, Direction, DoneCounts, FrameKind, HelperErrorCode,
    ResourcePiece,
};
use oma_export::hidden::MarkCounts;
use oma_export::pdf::doc::{
    Block, HanFace, HeadingLevel, PageSize, Para, PdfOptions, PdfText, TextBuilder,
};
use oma_export::pdf::fonts::{self, Face, PINS};
use oma_export::render::pdf::compose::{Composer, Extent, MARGIN};
use oma_export::render::pdf::layout::{Paragraph, Typesetter};
use oma_export::render::pdf::library::{Library, LibraryRefusal};
use oma_export::render::serve;

fn font_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("src-tauri")
        .join("resources")
        .join("fonts")
}

fn font_files() -> Vec<Vec<u8>> {
    PINS.iter()
        .map(|p| std::fs::read(font_dir().join(p.file)).expect(p.file))
        .collect()
}

fn typesetter(han: HanFace) -> Typesetter {
    Typesetter::new(Library::new(font_files(), han).expect("library"))
}

const WIDTH: f32 = 400.0;

fn slots(p: &Paragraph) -> Vec<usize> {
    let mut v: Vec<usize> = p
        .lines
        .iter()
        .flat_map(|l| l.runs.iter().map(|r| r.slot))
        .collect();
    v.dedup();
    v
}

fn slot(face: Face) -> usize {
    fonts::slot(face)
}

#[test]
fn the_committed_fonts_match_their_pins_and_register_their_families() {
    let files = font_files();
    for (i, f) in files.iter().enumerate() {
        fonts::check_slot(i, f).unwrap_or_else(|e| panic!("{}: {e:?}", PINS[i].file));
    }
    assert!(Library::new(files.clone(), HanFace::Simplified).is_ok());
    // A set in the wrong order is refused, not drawn with.
    let mut swapped = files;
    swapped.swap(0, 1);
    assert_eq!(
        Library::new(swapped, HanFace::Simplified).err(),
        Some(LibraryRefusal::Pins)
    );
    // OFL.txt ships beside the fonts.
    let ofl = std::fs::read_to_string(font_dir().join("OFL.txt")).expect("OFL.txt");
    assert!(ofl.contains("SIL OPEN FONT LICENSE Version 1.1"));
    for p in PINS {
        assert!(ofl.contains(p.file), "{}", p.file);
    }
}

#[test]
fn scripts_take_their_faces() {
    let mut ts = typesetter(HanFace::Simplified);
    let cases: [(&str, Face); 9] = [
        ("Grüße, Καλημέρα, Привет", Face::Sans),
        ("مرحبا بالعالم", Face::Arabic),
        ("שלום עולם", Face::Hebrew),
        ("हिन्दी क्षत्रिय श्री", Face::Devanagari),
        ("ภาษาไทย", Face::Thai),
        ("简体中文测试", Face::CjkSc),
        ("日本語のテキスト", Face::CjkJp),
        ("한국어 텍스트", Face::CjkKr),
        ("\u{27E8}U+202E\u{27E9}", Face::Math),
    ];
    for (text, face) in cases {
        let p = ts.paragraph(text, WIDTH, 9.5);
        assert!(slots(&p).contains(&slot(face)), "{face:?}: {:?}", slots(&p));
        assert_eq!(p.notdef_chars, 0, "{face:?}");
    }
    // Debug never prints the text.
    let p = ts.paragraph("SENTINEL-PDF-TEXT", WIDTH, 9.5);
    assert!(!format!("{p:?}").contains("SENTINEL"));
    // Han unification: Japanese text never takes the Chinese face, and the
    // Traditional option moves Chinese runs to TC.
    let p = ts.paragraph("漢字です", WIDTH, 9.5);
    assert_eq!(slots(&p), [slot(Face::CjkJp)]);
    let mut tc = typesetter(HanFace::Traditional);
    let p = tc.paragraph("繁體中文", WIDTH, 9.5);
    assert_eq!(slots(&p), [slot(Face::CjkTc)]);
}

#[test]
fn variation_selectors_choose_emoji_or_text_presentation() {
    let mut ts = typesetter(HanFace::Simplified);
    // VS16 asks for the emoji, VS15 for text, in both orders of the
    // characters' coverage (U+2764 and U+2708 are in Symbols 2 and in the
    // emoji font).
    let p = ts.paragraph("\u{2764}\u{FE0F}", WIDTH, 9.5);
    assert_eq!(slots(&p), [slot(Face::Emoji)]);
    let p = ts.paragraph("\u{2764}\u{FE0E}", WIDTH, 9.5);
    assert_ne!(slots(&p), [slot(Face::Emoji)]);
    let p = ts.paragraph("\u{2708}\u{FE0E}", WIDTH, 9.5);
    assert!(!slots(&p).contains(&slot(Face::Emoji)), "{:?}", slots(&p));
    let p = ts.paragraph("\u{2708}\u{FE0F}", WIDTH, 9.5);
    assert_eq!(slots(&p), [slot(Face::Emoji)]);
    // Emoji-only characters, ZWJ sequences, flags and keycaps.
    for e in [
        "\u{1F600}",
        "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}",
        "\u{1F1E9}\u{1F1EA}",
        "1\u{FE0F}\u{20E3}",
        "\u{1F44D}\u{1F3FD}",
    ] {
        let p = ts.paragraph(e, WIDTH, 9.5);
        assert_eq!(slots(&p), [slot(Face::Emoji)], "{e:?}");
        assert_eq!(p.notdef_chars, 0, "{e:?}");
    }
}

#[test]
fn characters_no_font_has_are_counted_as_notdef() {
    let mut ts = typesetter(HanFace::Simplified);
    // Tibetan, Linear B and Cherokee are not in the bundled set.
    let p = ts.paragraph("ok བོད 𐀀𐀁 ᏣᎳᎩ", WIDTH, 9.5);
    assert!(p.notdef_chars >= 8, "{}", p.notdef_chars);
    let p = ts.paragraph("plain text", WIDTH, 9.5);
    assert_eq!(p.notdef_chars, 0);
}

#[test]
fn rtl_paragraphs_are_right_aligned_and_ltr_left_aligned() {
    let mut ts = typesetter(HanFace::Simplified);
    let p = ts.paragraph("שלום עולם and more", WIDTH, 9.5);
    assert!(p.rtl);
    let first = p.lines[0]
        .runs
        .iter()
        .map(|r| r.x)
        .fold(f32::INFINITY, f32::min);
    let end = p.lines[0]
        .runs
        .iter()
        .map(|r| r.x + r.advance)
        .fold(0.0, f32::max);
    assert!(first > WIDTH / 2.0, "starts at {first}");
    assert!((end - WIDTH).abs() < 1.0, "ends at {end}");
    // The first strong character decides: an LTR-first paragraph with
    // Hebrew inside stays left-aligned.
    let p = ts.paragraph("He said שלום then left", WIDTH, 9.5);
    assert!(!p.rtl);
    assert!(p.lines[0].runs[0].x.abs() < 0.5);
}

#[test]
fn long_tokens_wrap_anywhere_and_never_overflow() {
    let mut ts = typesetter(HanFace::Simplified);
    let token = format!("https://example.invalid/{}", "a".repeat(400));
    let p = ts.paragraph(&token, WIDTH, 9.5);
    assert!(p.lines.len() > 3);
    assert!(p.forced_wraps >= 3, "{}", p.forced_wraps);
    for l in &p.lines {
        assert!(l.advance <= WIDTH + 0.5, "{}", l.advance);
    }
    // Thai breaks at its word boundaries (the dictionary segmenter), not
    // by force.
    let thai = "ภาษาไทยไม่มีการเว้นวรรคระหว่างคำและต้องตัดบรรทัดด้วยพจนานุกรม".repeat(4);
    let p = ts.paragraph(&thai, 150.0, 9.5);
    assert!(p.lines.len() > 3);
    assert_eq!(p.forced_wraps, 0);
}

fn compose(
    blocks: &[Block],
    page: PageSize,
) -> (
    Vec<u8>,
    oma_export::render::pdf::compose::VolumeCounts,
    Option<Extent>,
) {
    let mut ts = typesetter(HanFace::Simplified);
    let mut c = Composer::new(page, "Messages export — conversation 1");
    for b in blocks {
        match b {
            Block::Heading { level, text, .. } => {
                c.heading(&mut ts, *level, text).expect("heading")
            }
            Block::Entry { head, body } => c.entry(&mut ts, head, body).expect("entry"),
        }
    }
    c.finish(&mut ts.fonts).expect("finish")
}

fn entry(head: &[&str], body: &str, markers: bool) -> Block {
    let mut n = MarkCounts::default();
    Block::Entry {
        head: head
            .iter()
            .map(|h| TextBuilder::new().header(h).build().0)
            .collect(),
        body: Para::recorded(2, body, markers, &mut n),
    }
}

fn hostile_corpus() -> Vec<Block> {
    let mut blocks = vec![Block::Heading {
        level: HeadingLevel::Conversation,
        text: PdfText::ours("Conversation 1"),
        title: None,
    }];
    blocks.push(Block::Heading {
        level: HeadingLevel::Day,
        text: PdfText::ours("2024-03-10"),
        title: None,
    });
    let long = "x".repeat(5_000);
    let flood = format!("e{}", "\u{301}".repeat(10_000));
    let bidi = "\u{202E}\u{2067}\u{202B}".repeat(500);
    // Characters no bundled font draws (Tibetan, Linear B, Cherokee),
    // many times over: each is a counted `.notdef`.
    let missing = "བོད 𐀀𐀁 ᏣᎳᎩ ".repeat(300);
    for body in [
        long.as_str(),
        flood.as_str(),
        bidi.as_str(),
        missing.as_str(),
        "Mixed: مرحبا 123 שלום ok 中文 日本語のテキスト 한국어 😀 👍🏽",
        "ภาษาไทยไม่มีการเว้นวรรค",
        "",
    ] {
        blocks.push(entry(
            &[
                "[2024-03-10 14:02:11 +01:00]",
                "\u{202E}evil\u{202C}",
                "[Edited: part 1]",
            ],
            body,
            true,
        ));
    }
    for i in 0..400 {
        blocks.push(entry(
            &["[2024-03-10 14:02:11 +01:00]", "Alice"],
            &format!("message {i} with some words to wrap across the line"),
            true,
        ));
    }
    blocks
}

#[test]
fn a_hostile_corpus_paginates_without_clipping() {
    let (bytes, counts, extent) = compose(&hostile_corpus(), PageSize::A4);
    check_output(OutputKind::Pdf, &bytes).expect("passes the output check");
    assert!(counts.pages > 5, "{counts:?}");
    assert_eq!(counts.combining_runs, 1);
    assert!(counts.wrapped_tokens > 20, "{counts:?}");
    assert!(counts.notdef_chars >= 8 * 300, "{counts:?}");
    let e = extent.expect("glyphs");
    let (w, h) = PageSize::A4.points();
    assert!(e.min_x >= MARGIN - 0.5, "{e:?}");
    assert!(e.max_x <= w - MARGIN + 0.5, "{e:?}");
    assert!(
        e.min_y >= MARGIN - 0.5 && e.max_y <= h - MARGIN + 0.5,
        "{e:?}"
    );
    assert!(e.glyphs > 10_000);
    // US Letter too.
    let (bytes, _, extent) = compose(&hostile_corpus()[..8], PageSize::Letter);
    check_output(OutputKind::Pdf, &bytes).expect("letter");
    let e = extent.expect("glyphs");
    assert!(e.max_x <= 612.0 - MARGIN + 0.5, "{e:?}");
}

#[test]
fn header_pieces_are_separate_isolates() {
    // An RTL sender followed by a qualifier: laid out as one bidi
    // paragraph, the qualifier's "[Edited" would be pulled into the RTL
    // run; as separate pieces it stays to the sender's right.
    let mut ts = typesetter(HanFace::Simplified);
    let sender = ts.paragraph("שלום", WIDTH, 9.5);
    let tag = ts.paragraph("[Edited: part 1]", WIDTH, 9.5);
    assert!(sender.rtl && !tag.rtl);
    let blocks = vec![entry(
        &["[10:00]", "שלום", "[Edited: part 1]"],
        "body",
        true,
    )];
    let (bytes, counts, _) = compose(&blocks, PageSize::A4);
    check_output(OutputKind::Pdf, &bytes).expect("ok");
    assert_eq!(counts.pages, 1);
}

/// Every stream of `pdf`, inflated where it is Flate-encoded (krilla's
/// layout: `n 0 obj\n<<…/Length N…>>\nstream\n`, N bytes, `\nendstream`).
fn streams(pdf: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(i) = find(&pdf[at..], b">>\nstream\n").map(|i| i + at) {
        let obj = pdf[..i]
            .windows(5)
            .rposition(|w| w == b" obj\n")
            .expect("obj");
        let dict = String::from_utf8_lossy(&pdf[obj..i]).into_owned();
        let len: usize = dict
            .split("/Length ")
            .nth(1)
            .and_then(|r| r.split(|c: char| !c.is_ascii_digit()).next())
            .and_then(|n| n.parse().ok())
            .expect("length");
        let start = i + b">>\nstream\n".len();
        let raw = &pdf[start..start + len];
        if dict.contains("FlateDecode") {
            out.push(miniz_oxide::inflate::decompress_to_vec_zlib(raw).expect("inflate"));
        } else {
            out.push(raw.to_vec());
        }
        at = start + len;
    }
    out
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

/// The text of the ToUnicode CMaps and of every `ActualText` hex string,
/// as one string of uppercase hex code units.
fn text_layer_hex(pdf: &[u8]) -> String {
    let mut s = String::new();
    for st in streams(pdf) {
        let t = String::from_utf8_lossy(&st).to_uppercase();
        if t.contains("BEGINBFCHAR") || t.contains("BEGINBFRANGE") || t.contains("ACTUALTEXT") {
            s.push_str(&t);
        }
    }
    s
}

#[test]
fn the_text_layer_follows_the_marker_rule() {
    for markers in [true, false] {
        let blocks = vec![entry(&["[10:00]", "Bob"], "pay\u{202E}gnp.exe", markers)];
        let (bytes, _, _) = compose(&blocks, PageSize::A4);
        check_output(OutputKind::Pdf, &bytes).expect("ok");
        let hex = text_layer_hex(&bytes);
        assert!(
            hex.contains("BEGINBFCHAR") || hex.contains("BEGINBFRANGE"),
            "a ToUnicode map"
        );
        // U+202E as a UTF-16 code unit, and U+27E8 (the marker's bracket).
        assert_eq!(
            hex.contains("202E>") || hex.contains("202E "),
            !markers,
            "markers {markers}"
        );
        assert_eq!(hex.contains("27E8"), markers, "markers {markers}");
    }
}

fn request(kind: FrameKind, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    write_frame(&mut v, kind, payload).expect("frame");
    v
}

fn session(options: &PdfOptions, fonts: &[Vec<u8>], blocks: &[(FrameKind, Block)]) -> Vec<u8> {
    let mut input = request(
        FrameKind::Begin,
        &Begin {
            backend: BackendKind::Pdf,
            options: options.encode(),
        }
        .encode(),
    );
    for (slot, bytes) in fonts.iter().enumerate() {
        let piece_max = (oma_export::helper::MAX_FRAME_BYTES as usize) - 17;
        for (i, chunk) in bytes.chunks(piece_max).enumerate() {
            let mut p = ResourcePiece::header(
                u8::try_from(slot).expect("slot"),
                bytes.len() as u64,
                (i * piece_max) as u64,
            );
            p.extend_from_slice(chunk);
            input.extend(request(FrameKind::Resource, &p));
        }
    }
    for (kind, b) in blocks {
        input.extend(request(*kind, &b.encode()));
    }
    input.extend(request(FrameKind::End, b""));
    input
}

#[test]
fn a_whole_session_through_serve_returns_a_checked_pdf() {
    let options = PdfOptions {
        page: PageSize::A4,
        han: HanFace::Simplified,
        title: "Messages export — conversation 1".into(),
    };
    let mut blocks = vec![(
        FrameKind::Conversation,
        Block::Heading {
            level: HeadingLevel::Conversation,
            text: PdfText::ours("Conversation 1"),
            title: None,
        },
    )];
    blocks.push((
        FrameKind::Item,
        Block::Heading {
            level: HeadingLevel::Day,
            text: PdfText::ours("2024-03-10"),
            title: None,
        },
    ));
    blocks.push((
        FrameKind::Item,
        entry(&["[10:00]", "Alice"], "hello བོད", true),
    ));
    let input = session(&options, &font_files(), &blocks);
    let mut out = Vec::new();
    assert_eq!(serve(&mut input.as_slice(), &mut out), 0);
    let mut r = out.as_slice();
    let mut pdf = Vec::new();
    let done = loop {
        let f = read_frame(&mut r, Direction::Reply).expect("reply");
        match f.kind {
            FrameKind::Chunk => pdf.extend_from_slice(&f.payload),
            FrameKind::Done => break DoneCounts::decode(&f.payload).expect("done"),
            k => panic!("unexpected {k:?}"),
        }
    };
    assert_eq!(done.total_bytes, pdf.len() as u64);
    assert_eq!(done.units, 1);
    assert_eq!(done.notdef_chars, 3);
    check_output(OutputKind::Pdf, &pdf).expect("passes the output check");
    assert!(find(&pdf, b"/Outlines").is_some(), "an outline");

    // A conversation heading in an Item frame, an entry in a Conversation
    // frame, a tampered font, and an image that is not one are refused.
    let refused = |input: Vec<u8>, code: HelperErrorCode| {
        let mut out = Vec::new();
        assert_eq!(serve(&mut input.as_slice(), &mut out), 3);
        let mut r = out.as_slice();
        let f = read_frame(&mut r, Direction::Reply).expect("error");
        assert_eq!((f.kind, f.payload), (FrameKind::Error, vec![code.code()]));
    };
    let wrong = vec![(FrameKind::Item, blocks[0].1.clone())];
    refused(
        session(&options, &font_files(), &wrong),
        HelperErrorCode::BadRequest,
    );
    let wrong = vec![(FrameKind::Conversation, blocks[2].1.clone())];
    refused(
        session(&options, &font_files(), &wrong),
        HelperErrorCode::BadRequest,
    );
    let mut tampered = font_files();
    tampered[3][100] ^= 1;
    refused(
        session(&options, &tampered, &blocks),
        HelperErrorCode::BadRequest,
    );
    let mut input = session(&options, &font_files(), &blocks[..1]);
    let end = request(FrameKind::End, b"");
    input.truncate(input.len() - end.len());
    let mut img = 1u64.to_le_bytes().to_vec();
    img.extend_from_slice(b"\x89PNG");
    input.extend(request(FrameKind::Image, &img));
    // Slice 6j2 draws previews: an image that does not decode is refused.
    refused(input, HelperErrorCode::BadRequest);
}

/// Writes sample PDFs and their expectations for
/// `scripts/oracles/pdf_text.py` into `$OMA_PDF_SAMPLES` (an empty
/// directory). See `docs/development.md`.
///
/// Each sample lists strings its text layer must and must never hold, and
/// whether pypdf's extraction is held to them too: pypdf 6.19.0 does not
/// apply `ActualText`, so its text for Arabic, Devanagari and Thai (whose
/// glyphs map to their text only through `ActualText`) and for the
/// markers-off spoof differs; `pdftotext` is held to every sample.
#[test]
#[ignore = "writes the PDF text-layer oracle's samples; run by hand or in CI"]
fn write_samples() {
    let dir = PathBuf::from(std::env::var_os("OMA_PDF_SAMPLES").expect("OMA_PDF_SAMPLES"));
    type Sample<'a> = (&'a str, Vec<Block>, &'a [&'a str], &'a [&'a str], bool);
    let one = |body: &str, markers: bool| {
        vec![
            Block::Heading {
                level: HeadingLevel::Conversation,
                text: PdfText::ours("Conversation 1"),
                title: None,
            },
            entry(&["[2024-03-10 14:02:11 +01:00]", "Alice"], body, markers),
        ]
    };
    let samples: Vec<Sample<'_>> = vec![
        (
            "latin",
            one("Grüße, naïve café; Καλημέρα; Привет мир", true),
            &["Grüße", "Καλημέρα", "Привет"],
            &[],
            true,
        ),
        (
            "arabic",
            one("مرحبا بالعالم", true),
            &["مرحبا", "بالعالم"],
            &[],
            false,
        ),
        (
            "hebrew",
            one("שלום עולם", true),
            &["שלום", "עולם"],
            &[],
            false,
        ),
        (
            "devanagari",
            one("हिन्दी क्षत्रिय श्री", true),
            &["हिन्दी", "क्षत्रिय", "श्री"],
            &[],
            false,
        ),
        (
            "thai",
            one("ภาษาไทยไม่มีการเว้นวรรค", true),
            &["ภาษาไทยไม่มีการเว้นวรรค"],
            &[],
            false,
        ),
        (
            "cjk",
            one("简体中文测试 日本語のテキスト 한국어 텍스트", true),
            &["简体中文测试", "日本語のテキスト", "한국어"],
            &[],
            true,
        ),
        (
            "emoji",
            one("smile 😀 thumbs 👍🏽 keycap 1️⃣", true),
            &["😀", "👍🏽", "smile", "thumbs"],
            &[],
            true,
        ),
        (
            "notdef",
            one("ok བོད 𐀀𐀁 ᏣᎳᎩ", true),
            &["བོད", "𐀀𐀁", "ᏣᎳᎩ"],
            &[],
            false,
        ),
        (
            "markers-on",
            one("pay\u{202E}gnp.exe a\u{200B}b", true),
            &["pay⟨U+202E⟩gnp.exe", "a⟨U+200B⟩b"],
            &["\u{202E}", "\u{200B}"],
            true,
        ),
        (
            "markers-off",
            one("pay\u{202E}gnp.exe", false),
            &["\u{202E}"],
            &["⟨U+202E⟩"],
            false,
        ),
        (
            "hostile",
            hostile_corpus(),
            &[
                "⟨+9984 combining marks⟩",
                "⟨U+202E⟩evil⟨U+202C⟩",
                "message 399 with",
            ],
            &["\u{202E}"],
            true,
        ),
    ];
    let mut expect = String::from("{\n");
    let last = samples.len();
    for (i, (name, blocks, must, never, pypdf)) in samples.into_iter().enumerate() {
        let (bytes, _, _) = compose(&blocks, PageSize::A4);
        check_output(OutputKind::Pdf, &bytes).expect("ok");
        std::fs::write(dir.join(format!("{name}.pdf")), &bytes).expect("write");
        let list = |v: &[&str]| {
            v.iter()
                .map(|s| serde_json::to_string(s).expect("json"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        expect.push_str(&format!(
            "  {}: {{\"must\": [{}], \"never\": [{}], \"pypdf\": {}}}{}\n",
            serde_json::to_string(name).expect("json"),
            list(must),
            list(never),
            pypdf,
            if i + 1 < last { "," } else { "" }
        ));
    }
    expect.push_str("}\n");
    std::fs::write(dir.join("expected.json"), expect).expect("write");
}
