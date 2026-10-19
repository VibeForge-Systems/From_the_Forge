//! The decode helper process for real (M5 spec §5.5, §13.6): the app's
//! own binary, built by Cargo, started with `--oma-decode-helper` and fed
//! verified bytes over its pipes. Synthetic images from the viewer
//! catalogue's media directory only.

use std::ffi::OsString;
use std::path::PathBuf;

use oma_archive::{RenderError, RenderLimits, RenderedFormat, Rendition};
use openmessage_archive_lib::viewer::media::helper::{
    HelperCommand, HelperHost, RenderBackend, RenderOutcome, HELPER_FLAG,
};

fn helper() -> HelperHost {
    HelperHost::new(Some(HelperCommand::new(
        PathBuf::from(env!("CARGO_BIN_EXE_openmessage-archive")),
        vec![OsString::from(HELPER_FLAG)],
    )))
}

fn media(name: &str) -> Vec<u8> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("tests")
        .join("fixtures")
        .join("viewer")
        .join("media")
        .join(name);
    std::fs::read(p).expect("fixture")
}

fn image(o: RenderOutcome) -> openmessage_archive_lib::viewer::media::helper::HelperImage {
    match o {
        RenderOutcome::Image(i) => i,
        other => panic!("expected an image, got {other:?}"),
    }
}

#[test]
fn images_round_trip_through_one_long_lived_helper() {
    let mut h = helper();
    let limits = RenderLimits::default();
    let rgb = image(h.render(&media("png-rgb-6x4.png"), Rendition::Thumb, &limits));
    assert_eq!(rgb.format, RenderedFormat::Jpeg);
    assert_eq!((rgb.width, rgb.height), (6, 4));
    assert_eq!((rgb.source_width, rgb.source_height), (6, 4));
    assert!(!rgb.has_alpha);
    assert!(
        rgb.bytes.starts_with(&[0xFF, 0xD8, 0xFF]),
        "a JPEG the app made"
    );
    let alpha = image(h.render(&media("png-alpha-4x4.png"), Rendition::Display, &limits));
    assert_eq!(alpha.format, RenderedFormat::Png);
    assert!(alpha.has_alpha);
    assert!(alpha.bytes.starts_with(b"\x89PNG"));
    // EXIF orientation 6 turns 8×6 into 6×8; the metadata is dropped.
    let exif = image(h.render(&media("jpeg-exif6-gps-8x6.jpg"), Rendition::Thumb, &limits));
    assert_eq!((exif.source_width, exif.source_height), (6, 8));
    assert!(!exif.bytes.windows(4).any(|w| w == b"Exif"));
    let gif = image(h.render(&media("gif-2frames-4x4.gif"), Rendition::Thumb, &limits));
    assert!(gif.animated);
    // Refusals are closed codes, not crashes, and the helper lives on.
    assert_eq!(
        h.render(&media("png-ihdr-60000.png"), Rendition::Thumb, &limits),
        RenderOutcome::Refused(RenderError::DimensionsTooLarge)
    );
    assert_eq!(
        h.render(&media("svg-script.svg"), Rendition::Thumb, &limits),
        RenderOutcome::Refused(RenderError::NotAnImage)
    );
    assert_eq!(
        h.render(b"", Rendition::Thumb, &limits),
        RenderOutcome::Refused(RenderError::NotAnImage)
    );
    assert_eq!(h.spawned(), 1, "one process served every request");
}

/// A helper that dies mid-request is one failed preview; the next request
/// starts a new helper. (The crash code exists in debug builds only.)
#[cfg(debug_assertions)]
#[test]
fn a_helper_killed_mid_request_is_restarted() {
    use openmessage_archive_lib::viewer::media::helper::RENDITION_CRASH_FOR_TEST;
    let mut h = helper();
    let limits = RenderLimits::default();
    let png = media("png-rgb-6x4.png");
    assert!(matches!(
        h.render(&png, Rendition::Thumb, &limits),
        RenderOutcome::Image(_)
    ));
    for round in 1..=3 {
        assert_eq!(
            h.request(&png, RENDITION_CRASH_FOR_TEST, &limits),
            RenderOutcome::Crashed,
            "round {round}"
        );
        assert!(matches!(
            h.render(&png, Rendition::Thumb, &limits),
            RenderOutcome::Image(_)
        ));
    }
    assert_eq!(h.spawned(), 4, "restarted after each crash");
}

/// A helper that does not answer in time is killed and counted as a
/// crash. The test helper here is a process that reads and never answers.
#[cfg(unix)]
#[test]
fn a_hung_helper_is_killed_after_the_timeout() {
    let mut h = HelperHost::new(Some(HelperCommand::new(
        PathBuf::from("/bin/sh"),
        vec![OsString::from("-c"), OsString::from("cat >/dev/null")],
    )))
    .with_timeout(std::time::Duration::from_millis(300));
    assert_eq!(
        h.render(b"bytes", Rendition::Thumb, &RenderLimits::default()),
        RenderOutcome::Crashed
    );
}
