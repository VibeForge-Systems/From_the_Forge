//! The PDF output check (M6 spec §5.7, §14.2 "Helper output checks"): a
//! planted action or embedding name is refused wherever it is a name
//! object (top level, nested, escaped, inside an inflated stream), the
//! same text inside a string or a binary stream is accepted, and every
//! shape the check cannot follow is refused.

use miniz_oxide::deflate::compress_to_vec_zlib;
use oma_export::check::pdf::{check, PdfRefusal, FORBIDDEN_NAMES, MAX_DEPTH};
use oma_export::check::{check_output, CheckRefusal, OutputKind};

/// A PDF from raw object bodies (the check does not read the xref, so
/// its offsets are not computed).
fn pdf(objects: &[Vec<u8>], trailer: &str) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    for (i, body) in objects.iter().enumerate() {
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    out.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \n");
    out.extend_from_slice(format!("trailer\n{trailer}\nstartxref\n9\n%%EOF\n").as_bytes());
    out
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
        let nested = format!("<< /A [ << /S /{name} >> ] >>").into_bytes();
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
    // Binary streams are inflated but not tokenised.
    let binary = text.as_bytes();
    for dict in [
        "/Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceRGB",
        "/Length1 100",
        "/Subtype /CIDFontType0C",
        "/Subtype /OpenType",
        "/N 3",
        "/Type /Metadata /Subtype /XML",
    ] {
        assert_eq!(check(&with(flate(dict, binary))), Ok(()), "{dict}");
        assert_eq!(check(&with(stream(dict, binary))), Ok(()), "{dict}");
    }
    let jpeg = stream(
        "/Type /XObject /Subtype /Image /Filter /DCTDecode",
        b"\xFF\xD8/JS /JavaScript\xFF\xD9",
    );
    assert_eq!(check(&with(jpeg)), Ok(()));
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
    for dict in ["/Type /ObjStm /N 1 /First 4", "/Type /XRef /W [1 2 1]"] {
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
        b"<< /A 1 >> >>",
        b"<< /A <4G> >>",
        b"<< 5 /A >>",
        b")",
        b"<< /A",
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
