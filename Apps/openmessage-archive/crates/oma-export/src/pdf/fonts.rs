//! The PDF fonts and their pins (M6 spec §5.6, §5.7; owner Q2).
//!
//! The PDF backend draws only with the fonts bundled under
//! `src-tauri/resources/fonts/` (never a system font). Each file is pinned
//! here by its exact length and SHA-256, computed from the upstream
//! download recorded in `docs/dependencies.md` ("PDF fonts"). The main
//! process reads the files, and [`verify`] checks every one against its
//! pin **before** any byte of it reaches the export-render helper: a
//! [`VerifiedFonts`] can be built only by [`verify`], and
//! `RenderSession::send_fonts` takes nothing else. The helper checks the
//! pins again when it receives the slots.
//!
//! A slot is a font's index in [`PINS`]; the helper registers slot `i`
//! as [`PINS`]`[i]` and refuses a set whose family names differ.

use std::sync::Arc;

use sha2::{Digest, Sha256};

/// What a bundled font is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Face {
    /// Latin, Greek, Cyrillic and general punctuation (the body face).
    Sans,
    Arabic,
    Hebrew,
    Devanagari,
    Thai,
    /// Symbols and dingbats (Noto Sans Symbols 2).
    Symbols,
    /// Mathematical symbols, including the reserved marker's brackets
    /// U+27E8 and U+27E9, which no other bundled font has.
    Math,
    /// Noto Sans CJK, Simplified Chinese region subset.
    CjkSc,
    /// Traditional Chinese region subset.
    CjkTc,
    /// Japanese region subset.
    CjkJp,
    /// Korean region subset.
    CjkKr,
    /// Noto Color Emoji (the COLRv1 build).
    Emoji,
}

/// One pinned font file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FontPin {
    pub face: Face,
    /// The file's name in the fonts resource folder (a constant of ours).
    pub file: &'static str,
    /// The family name the font's `name` table must declare.
    pub family: &'static str,
    /// The exact length in bytes.
    pub bytes: u64,
    /// The SHA-256 of the file, lowercase hex.
    pub sha256: &'static str,
}

/// Every bundled font, in slot order. Changing a file means a new
/// download recorded in `docs/dependencies.md` and a new pin here; the
/// shell's `exports/pdf_fonts.rs` tests check the committed files against
/// these.
pub const PINS: [FontPin; 12] = [
    FontPin {
        face: Face::Sans,
        file: "NotoSans-Regular.ttf",
        family: "Noto Sans",
        bytes: 431_364,
        sha256: "f3961a9cde016d41a4879aecda1474d3a36d6bf54fa0e4643de029cc2248b0e8",
    },
    FontPin {
        face: Face::Arabic,
        file: "NotoSansArabic-Regular.ttf",
        family: "Noto Sans Arabic",
        bytes: 142_140,
        sha256: "bd86ca02f087d7f3c3788ba458fb6b73744c7639ed276b8d870dba6def6c40d0",
    },
    FontPin {
        face: Face::Hebrew,
        file: "NotoSansHebrew-Regular.ttf",
        family: "Noto Sans Hebrew",
        bytes: 16_836,
        sha256: "04272f5600d0ec816d31d0df73b23aa8d3501ea359ebe820da31c11ffcf00853",
    },
    FontPin {
        face: Face::Devanagari,
        file: "NotoSansDevanagari-Regular.ttf",
        family: "Noto Sans Devanagari",
        bytes: 184_228,
        sha256: "9c7d935139ea6a1e6ad9dbac4f6d27ece1e04bca8123c8888d00a0f9df4724cd",
    },
    FontPin {
        face: Face::Thai,
        file: "NotoSansThai-Regular.ttf",
        family: "Noto Sans Thai",
        bytes: 20_960,
        sha256: "d4303fe9c63ebb72759ca8b6d2040c8ae81689f7d08d7b91c656154382b49313",
    },
    FontPin {
        face: Face::Symbols,
        file: "NotoSansSymbols2-Regular.ttf",
        family: "Noto Sans Symbols 2",
        bytes: 671_568,
        sha256: "c4a0a80f0041ce4be81e2478faad22776d23edb98ae3f0d19bd37044820ecf9d",
    },
    FontPin {
        face: Face::Math,
        file: "NotoSansMath-Regular.ttf",
        family: "Noto Sans Math",
        bytes: 657_440,
        sha256: "b127e84699212b6b2ef50aff58e0ebebeec04ffe6db1b9eb9e209c8c3d97b4aa",
    },
    FontPin {
        face: Face::CjkSc,
        file: "NotoSansSC-Regular.otf",
        family: "Noto Sans SC",
        bytes: 8_331_336,
        sha256: "faa6c9df652116dde789d351359f3d7e5d2285a2b2a1f04a2d7244df706d5ea9",
    },
    FontPin {
        face: Face::CjkTc,
        file: "NotoSansTC-Regular.otf",
        family: "Noto Sans TC",
        bytes: 5_683_368,
        sha256: "5bab0cb3c1cf89dde07c4a95a4054b195afbcfe784d69d75c340780712237537",
    },
    FontPin {
        face: Face::CjkJp,
        file: "NotoSansJP-Regular.otf",
        family: "Noto Sans JP",
        bytes: 4_533_028,
        sha256: "dff723ba59d57d136764a04b9b2d03205544f7cd785a711442d6d2d085ac5073",
    },
    FontPin {
        face: Face::CjkKr,
        file: "NotoSansKR-Regular.otf",
        family: "Noto Sans KR",
        bytes: 4_644_748,
        sha256: "69975a0ac8472717870aefeab0a4d52739308d90856b9955313b2ad5e0148d68",
    },
    FontPin {
        face: Face::Emoji,
        file: "Noto-COLRv1.ttf",
        family: "Noto Color Emoji",
        bytes: 5_038_236,
        sha256: "b8e25ea68db82f9e4d0aee921f4420be2be39887bd5c893a2ad98710531f9d0c",
    },
];

/// The pin of `face` (every face has one).
pub fn pin(face: Face) -> Option<FontPin> {
    PINS.iter().copied().find(|p| p.face == face)
}

/// The slot of `face` (its index in [`PINS`]).
pub fn slot(face: Face) -> usize {
    PINS.iter().position(|p| p.face == face).unwrap_or(0)
}

/// Why a font set was refused. Kinds only: no path, no bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontRefusal {
    /// Not exactly one file per pin.
    WrongCount,
    /// The slot's length differs from its pin.
    WrongLength { slot: usize },
    /// The slot's SHA-256 differs from its pin.
    WrongDigest { slot: usize },
}

/// The lowercase hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(64);
    for b in digest.iter() {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Check one font file against the pin of `slot`.
pub fn check_slot(slot: usize, bytes: &[u8]) -> Result<(), FontRefusal> {
    let pin = PINS.get(slot).ok_or(FontRefusal::WrongCount)?;
    if u64::try_from(bytes.len()).ok() != Some(pin.bytes) {
        return Err(FontRefusal::WrongLength { slot });
    }
    if sha256_hex(bytes) != pin.sha256 {
        return Err(FontRefusal::WrongDigest { slot });
    }
    Ok(())
}

/// The bundled fonts, every one checked against its pin.
#[derive(Clone, PartialEq, Eq)]
pub struct VerifiedFonts {
    slots: Vec<Arc<Vec<u8>>>,
}

impl std::fmt::Debug for VerifiedFonts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerifiedFonts")
            .field("slots", &self.slots.len())
            .finish()
    }
}

impl VerifiedFonts {
    /// The slots, in [`PINS`] order.
    pub fn slots(&self) -> &[Arc<Vec<u8>>] {
        &self.slots
    }

    /// The bytes of `face`.
    pub fn face(&self, face: Face) -> Option<&Arc<Vec<u8>>> {
        self.slots.get(slot(face))
    }
}

/// Check `files` (one per pin, in slot order) against [`PINS`].
pub fn verify(files: Vec<Vec<u8>>) -> Result<VerifiedFonts, FontRefusal> {
    if files.len() != PINS.len() {
        return Err(FontRefusal::WrongCount);
    }
    for (slot, bytes) in files.iter().enumerate() {
        check_slot(slot, bytes)?;
    }
    Ok(VerifiedFonts {
        slots: files.into_iter().map(Arc::new).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_are_unique_well_formed_and_within_the_budget() {
        let mut total = 0;
        for (i, p) in PINS.iter().enumerate() {
            assert_eq!(slot(p.face), i);
            assert_eq!(pin(p.face), Some(*p));
            assert_eq!(p.sha256.len(), 64);
            assert!(p
                .sha256
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
            assert!(p.file.ends_with(".ttf") || p.file.ends_with(".otf"));
            total += p.bytes;
        }
        // The owner's budget is about 32 MB raw (Q2).
        assert!(total < 32_000_000, "{total}");
        let files: std::collections::BTreeSet<_> =
            PINS.iter().map(|p| p.file.to_ascii_lowercase()).collect();
        assert_eq!(files.len(), PINS.len());
        // Every slot fits the helper's resource limits.
        assert!(PINS.len() <= usize::from(crate::helper::frame::MAX_RESOURCE_SLOTS));
        assert!(total <= crate::helper::frame::MAX_RESOURCE_BYTES);
    }

    #[test]
    fn a_set_that_does_not_match_its_pins_is_refused() {
        assert_eq!(verify(Vec::new()), Err(FontRefusal::WrongCount));
        let wrong: Vec<Vec<u8>> = PINS.iter().map(|_| b"x".to_vec()).collect();
        assert_eq!(verify(wrong), Err(FontRefusal::WrongLength { slot: 0 }));
        let len = usize::try_from(PINS[0].bytes).expect("len");
        assert_eq!(
            check_slot(0, &vec![0u8; len]),
            Err(FontRefusal::WrongDigest { slot: 0 })
        );
        assert_eq!(check_slot(PINS.len(), b""), Err(FontRefusal::WrongCount));
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
