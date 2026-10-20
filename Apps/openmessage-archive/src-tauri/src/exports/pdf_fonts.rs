//! The PDF fonts on the main process's side (M6 spec §5.7; owner Q2;
//! slice 6j1).
//!
//! The fonts ship under `resources/fonts/` (with `OFL.txt`). [`load`]
//! reads each file the pins in `oma_export::pdf::fonts::PINS` name, from
//! a folder Rust chose (the app's resource folder; never a path from the
//! webview), refuses a file whose length differs from its pin before
//! reading it, and checks every file's SHA-256 with
//! `oma_export::pdf::fonts::verify`. Only the [`VerifiedFonts`] it returns
//! can be sent to the export-render helper (`RenderSession::send_fonts`),
//! so no unchecked byte reaches the helper.
//!
//! Errors carry a slot number and a kind only: never a path. The export
//! job calls this (slice 6j2) with the resource folder `start_export`
//! resolved through Tauri (`bundle.resources` copies the fonts there).

use std::io::Read;
use std::path::Path;

use oma_export::pdf::fonts::{self, FontRefusal, VerifiedFonts, PINS};

/// Why the fonts could not be loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontLoadError {
    /// The slot's file is missing, not a regular file, or unreadable.
    Unreadable { slot: usize },
    /// A file does not match its pin.
    Refused(FontRefusal),
}

/// Read and check every pinned font in `dir`.
pub fn load(dir: &Path) -> Result<VerifiedFonts, FontLoadError> {
    let mut files = Vec::with_capacity(PINS.len());
    for (slot, pin) in PINS.iter().enumerate() {
        let unreadable = FontLoadError::Unreadable { slot };
        // `pin.file` is a constant of ours, not an outside string.
        let path = dir.join(pin.file);
        let meta = std::fs::metadata(&path).map_err(|_| unreadable)?;
        if !meta.is_file() {
            return Err(unreadable);
        }
        if meta.len() != pin.bytes {
            return Err(FontLoadError::Refused(FontRefusal::WrongLength { slot }));
        }
        let file = std::fs::File::open(&path).map_err(|_| unreadable)?;
        let cap = usize::try_from(pin.bytes).map_err(|_| unreadable)?;
        let mut bytes = Vec::with_capacity(cap);
        file.take(pin.bytes + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| unreadable)?;
        files.push(bytes);
    }
    fonts::verify(files).map_err(FontLoadError::Refused)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn committed() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("resources")
            .join("fonts")
    }

    #[test]
    fn the_committed_fonts_load_and_match_their_pins() {
        let fonts = load(&committed()).expect("the committed fonts match their pins");
        assert_eq!(fonts.slots().len(), PINS.len());
        assert!(committed().join("OFL.txt").is_file());
        // Nothing else ships in the folder without a pin.
        let mut names: Vec<String> = std::fs::read_dir(committed())
            .expect("dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        let mut expected: Vec<String> = PINS.iter().map(|p| p.file.to_owned()).collect();
        expected.push("OFL.txt".into());
        expected.sort();
        assert_eq!(names, expected);
    }

    #[test]
    fn a_missing_resized_or_altered_font_is_refused_before_the_helper() {
        let dir = tempfile::tempdir().expect("tempdir");
        for p in PINS {
            std::fs::copy(committed().join(p.file), dir.path().join(p.file)).expect("copy");
        }
        assert!(load(dir.path()).is_ok());
        // One byte changed: same length, wrong digest.
        let victim = dir.path().join(PINS[5].file);
        let mut b = std::fs::read(&victim).expect("read");
        b[1000] ^= 0x55;
        std::fs::write(&victim, &b).expect("write");
        assert_eq!(
            load(dir.path()).err(),
            Some(FontLoadError::Refused(FontRefusal::WrongDigest { slot: 5 }))
        );
        // A byte longer: refused from its length, before it is read.
        b[1000] ^= 0x55;
        b.push(0);
        std::fs::write(&victim, &b).expect("write");
        assert_eq!(
            load(dir.path()).err(),
            Some(FontLoadError::Refused(FontRefusal::WrongLength { slot: 5 }))
        );
        // Missing, or a folder in its place.
        std::fs::remove_file(&victim).expect("remove");
        assert_eq!(
            load(dir.path()).err(),
            Some(FontLoadError::Unreadable { slot: 5 })
        );
        std::fs::create_dir(&victim).expect("mkdir");
        assert_eq!(
            load(dir.path()).err(),
            Some(FontLoadError::Unreadable { slot: 5 })
        );
    }
}
