//! The PDF output check (M6 spec §5.7, §14.2 "Helper output checks"): a
//! planted action or embedding name is refused wherever it is a name
//! object (top level, nested, escaped, inside an inflated stream), the
//! same text inside a string or a binary stream krilla writes is
//! accepted, every shape the check cannot follow is refused, and the
//! cross-reference table must point only at objects the check saw.

use std::collections::BTreeMap;

use miniz_oxide::deflate::compress_to_vec_zlib;
use oma_export::check::pdf::{check, PdfRefusal, FORBIDDEN_KEYS, FORBIDDEN_NAMES, MAX_DEPTH};
use oma_export::check::{check_output, CheckRefusal, OutputKind};

/// Moves or adds cross-reference entries (number → offset), given the
/// bytes written before the table.
type XrefEdit<'a> = &'a dyn Fn(&[u8], &mut BTreeMap<usize, usize>);

/// A PDF from raw object bodies (object `i + 1` is `objects[i]`), with a
/// correct cross-reference table; `extra` may add or move entries
/// (number → offset), given the bytes written before the table.
fn assemble(objects: &[Vec<u8>], trailer: &str, extra: XrefEdit<'_>) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut at = BTreeMap::new();
    for (i, body) in objects.iter().enumerate() {
        at.insert(i + 1, out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    extra(&out, &mut at);
    let xref = out.len();
    let size = at.keys().max().copied().unwrap_or(0) + 1;
    out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f\r\n").as_bytes());
    for n in 1..size {
        match at.get(&n) {
            Some(o) => out.extend_from_slice(format!("{o:010} 00000 n\r\n").as_bytes()),
            None => out.extend_from_slice(b"0000000000 65535 f\r\n"),
        }
    }
    out.extend_from_slice(format!("trailer\n{trailer}\nstartxref\n{xref}\n%%EOF\n").as_bytes());
    out
}

fn pdf(objects: &[Vec<u8>], trailer: &str) -> Vec<u8> {
    assemble(objects, trailer, &|_, _| {})
}

fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut v = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    v.extend_from_slice(data);
    v.extend_from_slice(b"\nendstream");
    v
}

fn flate(dict: &str, data: &[u8]) -> Vec<u8> {
    stream(
        &format!("{dict} /Filter /FlateDecode"),
        &compress_to_vec_zlib(data, 6),
    )
}

const TRAILER: &str = "<< /Size 4 /Root 1 0 R >>";

fn base() -> Vec<Vec<u8>> {
    vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595.28 841.89] /Contents 4 0 R >>".to_vec(),
    ]
}

fn with(extra: Vec<u8>) -> Vec<u8> {
    let mut o = base();
    o.push(extra);
    pdf(&o, TRAILER)
}

#[test]
fn a_plain_document_is_accepted() {
    let content = b"BT /F1 9.5 Tf 72 720 Td (Hello) Tj ET\n/Span << /ActualText (x) >> BDC EMC";
    assert_eq!(check(&with(flate("", content))), Ok(()));
    assert_eq!(check(&with(stream("", content))), Ok(()));
    assert_eq!(
        check_output(OutputKind::Pdf, &with(flate("", content))),
        Ok(())
    );
}

#[test]
fn every_forbidden_name_is_refused_as_a_name_object() {
    for name in FORBIDDEN_NAMES {
        let name = std::str::from_utf8(name).expect("ascii");
        // In the catalog.
        let mut o = base();
        o[0] = format!("<< /Type /Catalog /Pages 2 0 R /{name} 5 0 R >>").into_bytes();
        o.push(b"<< >>".to_vec());
        assert_eq!(
            check(&pdf(&o, TRAILER)),
            Err(PdfRefusal::ForbiddenName),
            "{name}"
        );
        // As a value, nested in a dictionary and an array.
        let nested = format!("<< /B [ << /S /{name} >> ] >>").into_bytes();
        assert_eq!(
            check(&with(nested)),
            Err(PdfRefusal::ForbiddenName),
            "{name}"
        );
        // Inside an inflated content stream.
        let content = format!("/{name} gs").into_bytes();
        assert_eq!(
            check(&with(flate("", &content))),
            Err(PdfRefusal::ForbiddenName),
            "{name}"
        );
        // Inside an uncompressed form.
        assert_eq!(
            check(&with(stream("/Type /XObject /Subtype /Form", &content))),
            Err(PdfRefusal::ForbiddenName),
            "{name}"
        );
    }
    // In the trailer.
    let o = base();
    assert_eq!(
        check(&pdf(&o, "<< /Size 4 /Root 1 0 R /Encrypt 9 0 R >>")),
        Err(PdfRefusal::ForbiddenName)
    );
}

#[test]
fn escaped_names_are_decoded_before_the_comparison() {
    for spelled in [
        "/J#53",
        "/#4AS",
        "/Java#53cript",
        "/Open#41ction",
        "/#41#41",
    ] {
        let body = format!("<< /S {spelled} >>").into_bytes();
        assert_eq!(
            check(&with(body)),
            Err(PdfRefusal::ForbiddenName),
            "{spelled}"
        );
    }
    // Near misses are names of their own.
    for spelled in ["/JSX", "/AAA", "/URIs", "/Javascript", "/J#5"] {
        let body = format!("<< /S {spelled} >>").into_bytes();
        assert_eq!(check(&with(body)), Ok(()), "{spelled}");
    }
}

#[test]
fn the_same_text_inside_strings_comments_and_binary_streams_is_accepted() {
    let text = "/JavaScript /JS /OpenAction /Launch /URI /EmbeddedFile /AcroForm";
    let in_strings = format!(
        "<< /Title ({text}) /Nested (a (nested {text}) b \\) {text}) /Hex <4A53 2F4A53> >>"
    )
    .into_bytes();
    assert_eq!(check(&with(in_strings)), Ok(()));
    let mut o = base();
    o.push(format!("% {text}\n<< >>").into_bytes());
    assert_eq!(check(&pdf(&o, TRAILER)), Ok(()));
    let content = format!("BT ({text}) Tj [({text})] TJ ET");
    assert_eq!(check(&with(flate("", content.as_bytes()))), Ok(()));
    // Binary streams krilla writes are inflated but not tokenised: an
    // image XObject, and the objects a font descriptor, an ICCBased colour
    // space or the catalog's /Metadata references, in krilla's shapes.
    let binary = text.as_bytes();
    let image = "/Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceRGB";
    for pack in [flate, stream] {
        assert_eq!(check(&with(pack(image, binary))), Ok(()), "image");
        for (dict, referrer) in [
            ("", "<< /Type /FontDescriptor /FontFile2 4 0 R >>"),
            ("", "<< /Type /FontDescriptor /CIDSet 4 0 R >>"),
            (
                "/Subtype /CIDFontType0C",
                "<< /Type /FontDescriptor /FontFile3 4 0 R >>",
            ),
            (
                "/Subtype /OpenType",
                "<< /Type /FontDescriptor /FontFile3 4 0 R >>",
            ),
            ("/N 3 /Range [0 1 0 1 0 1]", "[/ICCBased 4 0 R]"),
            ("/Type /Metadata /Subtype /XML", "<< /Metadata 4 0 R >>"),
        ] {
            let mut o = base();
            o.push(pack(dict, binary));
            o.push(referrer.as_bytes().to_vec());
            o[0] = b"<< /Type /Catalog /Pages 2 0 R /X 5 0 R >>".to_vec();
            assert_eq!(check(&pdf(&o, TRAILER)), Ok(()), "{dict} by {referrer}");
            // Unreferenced, or in another shape, the same stream is
            // tokenised and its names are found.
            let mut lone = base();
            lone.push(pack(dict, binary));
            assert_eq!(
                check(&pdf(&lone, TRAILER)),
                Err(PdfRefusal::ForbiddenName),
                "{dict} alone"
            );
        }
        let mut wrong = base();
        wrong.push(pack("/Type /XObject /Subtype /Form", binary));
        wrong.push(b"<< /Type /FontDescriptor /FontFile2 4 0 R >>".to_vec());
        wrong[0] = b"<< /Type /Catalog /Pages 2 0 R /X 5 0 R >>".to_vec();
        assert_eq!(
            check(&pdf(&wrong, TRAILER)),
            Err(PdfRefusal::ForbiddenName),
            "a form named as a font program is still tokenised"
        );
    }
    // The old heuristics are gone: /Length1 or a bare /N no longer exempts
    // a stream.
    for dict in ["/Length1 100", "/N 3", "/Subtype /CIDFontType0C"] {
        assert_eq!(
            check(&with(flate(dict, binary))),
            Err(PdfRefusal::ForbiddenName),
            "{dict}"
        );
    }
    let jpeg = stream(
        "/Type /XObject /Subtype /Image /Filter /DCTDecode",
        b"\xFF\xD8/JS /JavaScript\xFF\xD9",
    );
    assert_eq!(check(&with(jpeg)), Ok(()));
    // One object named in two binary roles is refused.
    let mut o = base();
    o.push(stream("", b"x"));
    o.push(b"<< /FontFile2 4 0 R /Metadata 4 0 R >>".to_vec());
    o[0] = b"<< /Type /Catalog /Pages 2 0 R /X 5 0 R >>".to_vec();
    assert_eq!(check(&pdf(&o, TRAILER)), Err(PdfRefusal::Malformed));
}

#[test]
fn header_and_end_are_required() {
    let good = with(stream("", b"BT ET"));
    let mut no_header = good.clone();
    no_header[1] = b'X';
    assert_eq!(check(&no_header), Err(PdfRefusal::NoHeader));
    assert_eq!(check(b"%PDF-"), Err(PdfRefusal::NoHeader));
    assert_eq!(check(b""), Err(PdfRefusal::NoHeader));
    let cut = &good[..good.len() - 7];
    assert_eq!(check(cut), Err(PdfRefusal::NoEof));
    let mut trailing = good.clone();
    trailing.extend_from_slice(b"1 0 obj << /JS 2 0 R >> endobj\n");
    assert_eq!(check(&trailing), Err(PdfRefusal::NoEof));
    let mut spaced = good.clone();
    spaced.extend_from_slice(b"\r\n \n");
    assert_eq!(check(&spaced), Ok(()));
}

#[test]
fn streams_need_a_direct_length_that_ends_at_endstream() {
    let indirect = b"<< /Length 9 0 R >>\nstream\nBT ET\nendstream".to_vec();
    assert_eq!(check(&with(indirect)), Err(PdfRefusal::StreamLength));
    // A length shorter than the data would hide bytes from the check.
    let short = b"<< /Length 2 >>\nstream\nBT /JS ET\nendstream".to_vec();
    assert_eq!(check(&with(short)), Err(PdfRefusal::StreamLength));
    let long = b"<< /Length 900 >>\nstream\nBT ET\nendstream".to_vec();
    assert_eq!(check(&with(long)), Err(PdfRefusal::StreamLength));
    let no_eol = b"<< /Length 5 >>\nstreamBT ET\nendstream".to_vec();
    assert_eq!(check(&with(no_eol)), Err(PdfRefusal::StreamLength));
    let no_dict = b"[1 2]\nstream\nBT ET\nendstream".to_vec();
    assert_eq!(check(&with(no_dict)), Err(PdfRefusal::StreamLength));
    let stray = b"<< >>\nendstream".to_vec();
    assert_eq!(check(&with(stray)), Err(PdfRefusal::StreamLength));
    let crlf = b"<< /Length 5 >>\r\nstream\r\nBT ET\r\nendstream".to_vec();
    assert_eq!(check(&with(crlf)), Ok(()));
}

#[test]
fn only_flate_and_image_dct_filters_are_accepted() {
    for dict in [
        "/Filter /ASCIIHexDecode",
        "/Filter [/FlateDecode /ASCII85Decode]",
        "/Filter /LZWDecode",
        "/Filter /DCTDecode",
        "/Filter 5 0 R",
        "/Filter /Fl",
    ] {
        assert_eq!(
            check(&with(stream(dict, b"BT ET"))),
            Err(PdfRefusal::UnsupportedFilter),
            "{dict}"
        );
    }
    let arr = stream("/Filter [/FlateDecode]", &compress_to_vec_zlib(b"BT ET", 6));
    assert_eq!(check(&with(arr)), Ok(()));
}

#[test]
fn flate_streams_must_inflate_within_their_bound() {
    let broken = stream("/Filter /FlateDecode", b"not zlib at all");
    assert_eq!(check(&with(broken)), Err(PdfRefusal::Inflate));
    let mut z = compress_to_vec_zlib(b"BT ET BT ET BT ET", 6);
    let last = z.len() - 1;
    z[last] ^= 0xFF;
    let bad_sum = stream("/Filter /FlateDecode", &z);
    assert_eq!(check(&with(bad_sum)), Err(PdfRefusal::Inflate));
    // 64 MiB + 1 of spaces inflates past the per-stream bound.
    let bomb = vec![b' '; oma_export::check::pdf::MAX_INFLATED_STREAM + 1];
    let z = compress_to_vec_zlib(&bomb, 1);
    drop(bomb);
    assert!(z.len() < 1 << 20);
    let s = stream("/Filter /FlateDecode", &z);
    assert_eq!(check(&with(s)), Err(PdfRefusal::InflateLimit));
}

#[test]
fn object_streams_inline_images_and_bad_syntax_are_refused() {
    for dict in [
        "/Type /ObjStm /N 1 /First 4",
        "/Type /XRef /W [1 2 1]",
        // By shape, whatever the type.
        "/N 1 /First 4",
        "/Size 6 /W [1 4 2] /Root 1 0 R /Length1 0",
        "/Type /XObject /Subtype /Image /Index [0 1]",
        "/Type /ObjStm",
    ] {
        assert_eq!(
            check(&with(flate(dict, b"1 0 << /JS 1 >>"))),
            Err(PdfRefusal::CompressedObjects),
            "{dict}"
        );
    }
    let inline = b"q BI /W 1 /H 1 /CS /G /BPC 8 ID \x00 EI Q";
    assert_eq!(
        check(&with(flate("", inline))),
        Err(PdfRefusal::InlineImage)
    );
    for body in [
        &b"<< /T (unterminated >>"[..],
        b"<< /B 1 >> >>",
        b"<< /B <4G> >>",
        b"<< 5 /B >>",
        b")",
        b"<< /B",
    ] {
        assert_eq!(check(&with(body.to_vec())), Err(PdfRefusal::Malformed));
    }
    let deep = format!("{}{}", "[".repeat(MAX_DEPTH + 1), "]".repeat(MAX_DEPTH + 1));
    assert_eq!(check(&with(deep.into_bytes())), Err(PdfRefusal::Malformed));
    let ok_depth = format!("{}{}", "[".repeat(MAX_DEPTH), "]".repeat(MAX_DEPTH));
    assert_eq!(check(&with(ok_depth.into_bytes())), Ok(()));
}

#[test]
fn xlsx_output_is_refused_until_its_check_exists() {
    assert_eq!(
        check_output(OutputKind::Xlsx, b"PK\x03\x04"),
        Err(CheckRefusal::Unchecked)
    );
}

/// Review finding: an untyped object stream at the end of a file whose
/// startxref names an untyped cross-reference stream. pdf.js reads both
/// by shape; the check refuses both by shape.
#[test]
fn untyped_object_and_cross_reference_streams_are_refused() {
    let hidden = b"1 0 << /Type /Catalog /Pages 2 0 R /OpenAction << /S /JavaScript /JS (app.alert(1)) >> >>";
    let mut out = b"%PDF-1.7\n".to_vec();
    out.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    out.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>\nendobj\n",
    );
    let z = compress_to_vec_zlib(hidden, 6);
    out.extend_from_slice(
        format!(
            "4 0 obj\n<< /N 1 /First 4 /Filter /FlateDecode /Length {} >>\nstream\n",
            z.len()
        )
        .as_bytes(),
    );
    out.extend_from_slice(&z);
    out.extend_from_slice(b"\nendstream\nendobj\n");
    let at = out.len();
    let z = compress_to_vec_zlib(&[0u8; 14], 6);
    out.extend_from_slice(
        format!(
            "5 0 obj\n<< /Size 6 /W [1 4 2] /Root 1 0 R /Length1 0 /Filter /FlateDecode /Length {} >>\nstream\n",
            z.len()
        )
        .as_bytes(),
    );
    out.extend_from_slice(&z);
    out.extend_from_slice(format!("\nendstream\nendobj\nstartxref\n{at}\n%%EOF\n").as_bytes());
    assert_eq!(check(&out), Err(PdfRefusal::CompressedObjects));
    // Without the object stream, a startxref at an object (a reader would
    // take it for a cross-reference stream) is refused too.
    let mut o = base();
    o.push(stream("", b"BT ET"));
    let mut bytes = pdf(&o, TRAILER);
    let first = bytes
        .windows(7)
        .position(|w| w == b"4 0 obj")
        .expect("obj 4");
    let tail = bytes
        .windows(9)
        .rposition(|w| w == b"startxref")
        .expect("startxref");
    bytes.truncate(tail);
    bytes.extend_from_slice(format!("startxref\n{first}\n%%EOF\n").as_bytes());
    assert_eq!(check(&bytes), Err(PdfRefusal::CrossReference));
}

/// Review finding: an embedded file needs no /Type, and a FileAttachment
/// annotation reaches it through names that were not on the list.
#[test]
fn file_attachments_annotations_and_actions_are_refused() {
    // The annotation.
    let mut o = base();
    o[2] = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Contents 4 0 R /Annots [<< /Type /Annot /Subtype /FileAttachment /Rect [0 0 10 10] /FS << /Type /Filespec /F (invoice.html) /UF (invoice.html) /EF << /F 5 0 R >> >> >>] >>".to_vec();
    o.push(stream("", b"BT ET"));
    o.push(stream("/N 1", b"<html><script>x</script></html>"));
    assert_eq!(check(&pdf(&o, TRAILER)), Err(PdfRefusal::ForbiddenName));
    // The untyped embedded-file stream on its own, unreferenced as a
    // profile, is tokenised (and its HTML is not PDF syntax).
    let mut o = base();
    o.push(stream("/N 1", b"<html><script>x</script></html>"));
    assert_eq!(check(&pdf(&o, TRAILER)), Err(PdfRefusal::Malformed));
    // Each part of the attachment and of other external or active
    // constructs is refused on its own.
    for body in [
        "<< /Subtype /FileAttachment >>",
        "<< /FS 5 0 R >>",
        "<< /Type /Filespec /F (a.exe) >>",
        "<< /EF << /F 5 0 R >> >>",
        "<< /S /Thread /D 1 >>",
        "<< /S /Rendition >>",
        "<< /Subtype /Movie >>",
        "<< /Subtype /Sound >>",
        "<< /Subtype /Screen >>",
        "<< /Subtype /3D /3DD 5 0 R >>",
        "<< /OnInstantiate 5 0 R >>",
        "<< /FS /URL /F (https://example.invalid/) >>",
    ] {
        assert_eq!(
            check(&with(body.as_bytes().to_vec())),
            Err(PdfRefusal::ForbiddenName),
            "{body}"
        );
    }
    // Every action, even a harmless /GoTo, is reached through /A, which
    // is refused as a key; an outline's /Dest is not.
    for key in FORBIDDEN_KEYS {
        let key = std::str::from_utf8(key).expect("ascii");
        let body = format!("<< /Title (x) /{key} << /S /GoTo /D [3 0 R /Fit] >> >>");
        assert_eq!(
            check(&with(body.into_bytes())),
            Err(PdfRefusal::ForbiddenName)
        );
    }
    assert_eq!(
        check(&with(b"<< /Title (x) /Dest [3 0 R /XYZ 0 0 0] >>".to_vec())),
        Ok(())
    );
}

/// Review finding: the xref was never read, so a catalog hidden in an
/// image's raw bytes could be the one a reader loads.
#[test]
fn the_cross_reference_table_must_point_only_at_objects_the_check_saw() {
    let hidden = b"\xFF\xD8 9 0 obj << /Type /Catalog /Pages 2 0 R /OpenAction << /S /JavaScript /JS (app.alert(1)) >> >> endobj \xFF\xD9";
    let mut o = base();
    o.push(stream(
        "/Type /XObject /Subtype /Image /Filter /DCTDecode",
        hidden,
    ));
    let find = |b: &[u8], n: &[u8]| b.windows(n.len()).position(|w| w == n).expect("needle");
    let planted = assemble(&o, "<< /Size 10 /Root 9 0 R >>", &|b, at| {
        at.insert(9, find(b, b"9 0 obj"));
    });
    assert_eq!(check(&planted), Err(PdfRefusal::CrossReference));
    // A reference to an object the table does not list (a reader would
    // search the raw bytes for it) is refused.
    let dangling = pdf(&o, "<< /Size 10 /Root 9 0 R >>");
    assert_eq!(check(&dangling), Err(PdfRefusal::CrossReference));
    let mut o2 = base();
    o2[0] = b"<< /Type /Catalog /Pages 2 0 R /Extra 7 0 R >>".to_vec();
    assert_eq!(check(&pdf(&o2, TRAILER)), Err(PdfRefusal::CrossReference));
    // An entry that points at the wrong object, or at the right number
    // with another generation, is refused.
    let complete = || {
        let mut o = base();
        o.push(stream("", b"BT ET"));
        o
    };
    let swapped = assemble(&complete(), TRAILER, &|_, at| {
        let (one, two) = (at[&1], at[&2]);
        at.insert(1, two);
        at.insert(2, one);
    });
    assert_eq!(check(&swapped), Err(PdfRefusal::CrossReference));
    let mut regen = pdf(&complete(), TRAILER);
    let x = find(&regen, b"xref\n");
    let entry = x + find(&regen[x..], b" 00000 n");
    regen[entry + 5] = b'1';
    assert_eq!(check(&regen), Err(PdfRefusal::CrossReference));
    // /Prev chains, a second table or trailer, a missing or misplaced
    // startxref, and a table in another layout are refused.
    assert_eq!(
        check(&pdf(&complete(), "<< /Size 5 /Root 1 0 R /Prev 9 >>")),
        Err(PdfRefusal::CrossReference)
    );
    assert_eq!(
        check(&pdf(&complete(), "<< /Size 5 >>")),
        Err(PdfRefusal::CrossReference)
    );
    let good = pdf(&complete(), TRAILER);
    assert_eq!(check(&good), Ok(()));
    let x = find(&good, b"xref\n");
    let mut doubled = good[..x].to_vec();
    doubled.extend_from_slice(&good[x..find(&good, b"trailer")]);
    doubled.extend_from_slice(&good[x..]);
    assert_eq!(check(&doubled), Err(PdfRefusal::CrossReference));
    let mut off = good.clone();
    let s = find(&off, b"startxref\n") + b"startxref\n".len();
    off[s] = if off[s] == b'1' { b'2' } else { b'1' };
    assert_eq!(check(&off), Err(PdfRefusal::CrossReference));
    let mut none = good[..find(&good, b"startxref")].to_vec();
    none.extend_from_slice(b"%%EOF\n");
    assert_eq!(check(&none), Err(PdfRefusal::CrossReference));
    let short_entries = String::from_utf8_lossy(&good).replace(" n\r\n", " n\n");
    assert_eq!(
        check(short_entries.as_bytes()),
        Err(PdfRefusal::CrossReference)
    );
}

/// Review finding: predictors and external files make the bytes the
/// check tokenises differ from the bytes a reader decodes.
#[test]
fn decode_parameters_and_external_stream_files_are_refused() {
    for dict in [
        "/DecodeParms << /Predictor 12 /Columns 4 >>",
        "/DP << /Predictor 2 >>",
        "/F (other.bin)",
        "/FFilter /FlateDecode",
        "/FDecodeParms << >>",
    ] {
        assert_eq!(
            check(&with(flate(dict, b"BT ET"))),
            Err(PdfRefusal::UnsupportedFilter),
            "{dict}"
        );
    }
}
