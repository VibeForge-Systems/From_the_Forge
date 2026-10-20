//! The helper's fonts: the pinned set, registered with parley (fontique)
//! with **no system fonts**, and the same bytes as krilla fonts.
//!
//! The slots arrive as `Resource` frames. They are checked against the
//! pins again here ([`crate::pdf::fonts::verify`]; the main process
//! checked them before sending), and each must register exactly its
//! pin's family, so a set in the wrong order, or a font that is not what
//! its pin says, is refused rather than silently drawn with.
//!
//! **Font stacks** ([`StackKind`]) decide which face draws a run; parley
//! then falls back cluster by cluster along the stack:
//!
//! - the default: Noto Sans, Arabic, Hebrew, Devanagari, Thai, the CJK
//!   faces (the export's Han face first, then the other Chinese face,
//!   Japanese, Korean), Symbols 2, Math (the marker's `⟨` `⟩`), and Noto
//!   Color Emoji last, so a character with both a text and an emoji form
//!   is drawn as text unless it asks otherwise;
//! - a CJK run holding kana is set Japanese first, one holding hangul
//!   Korean first (Han unification: Japanese text must not take Chinese
//!   forms), any other the export's Han face first;
//! - a character followed by VS16 (U+FE0F) is set emoji first (with the
//!   rest of its keycap or ZWJ sequence); one followed by VS15 (U+FE0E)
//!   with the emoji font left out.

use std::collections::BTreeMap;
use std::sync::Arc;

use parley::fontique::{Blob, Collection, CollectionOptions};
use parley::{FontContext, FontData, FontFamilyName};

use crate::pdf::doc::HanFace;
use crate::pdf::fonts::{self, Face, PINS};

/// Which stack a range of text is set in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StackKind {
    Default,
    Japanese,
    Korean,
    /// A Han run that is neither Japanese nor Korean: the export's Han
    /// face first.
    Chinese,
    EmojiFirst,
    TextOnly,
}

impl StackKind {
    pub const ALL: [StackKind; 6] = [
        StackKind::Default,
        StackKind::Japanese,
        StackKind::Korean,
        StackKind::Chinese,
        StackKind::EmojiFirst,
        StackKind::TextOnly,
    ];
}

/// The faces of a stack, in order.
pub fn stack_faces(kind: StackKind, han: HanFace) -> Vec<Face> {
    let (first_han, second_han) = match han {
        HanFace::Simplified => (Face::CjkSc, Face::CjkTc),
        HanFace::Traditional => (Face::CjkTc, Face::CjkSc),
    };
    let base = [
        Face::Sans,
        Face::Arabic,
        Face::Hebrew,
        Face::Devanagari,
        Face::Thai,
        first_han,
        second_han,
        Face::CjkJp,
        Face::CjkKr,
        Face::Symbols,
        Face::Math,
        Face::Emoji,
    ];
    let first = match kind {
        StackKind::Default | StackKind::TextOnly => None,
        StackKind::Japanese => Some(Face::CjkJp),
        StackKind::Korean => Some(Face::CjkKr),
        StackKind::Chinese => Some(first_han),
        StackKind::EmojiFirst => Some(Face::Emoji),
    };
    let mut out: Vec<Face> = first.into_iter().collect();
    out.extend(base.into_iter().filter(|f| Some(*f) != first));
    if kind == StackKind::TextOnly {
        out.retain(|f| *f != Face::Emoji);
    }
    out
}

/// The registered fonts.
pub struct Library {
    pub(crate) cx: FontContext,
    /// Each slot's bytes, shared with krilla.
    bytes: Vec<Arc<Vec<u8>>>,
    /// fontique blob id → slot.
    blobs: BTreeMap<u64, usize>,
    /// The family-name stacks, by kind.
    stacks: BTreeMap<StackKind, Vec<FontFamilyName<'static>>>,
    /// krilla fonts, made on first use, by slot.
    krilla: BTreeMap<usize, krilla::text::Font>,
}

impl std::fmt::Debug for Library {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Library")
            .field("slots", &self.bytes.len())
            .finish_non_exhaustive()
    }
}

/// Why the slots were refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibraryRefusal {
    /// The slots do not match the pins.
    Pins,
    /// A slot did not register exactly its pin's family.
    Family,
}

impl Library {
    /// Check `slots` against the pins and register them.
    pub fn new(slots: Vec<Vec<u8>>, han: HanFace) -> Result<Library, LibraryRefusal> {
        let verified = fonts::verify(slots).map_err(|_| LibraryRefusal::Pins)?;
        let mut cx = FontContext {
            collection: Collection::new(CollectionOptions {
                shared: false,
                system_fonts: false,
            }),
            source_cache: Default::default(),
        };
        let mut blobs = BTreeMap::new();
        let mut bytes = Vec::new();
        for (slot, (data, pin)) in verified.slots().iter().zip(PINS).enumerate() {
            let shared: Arc<dyn AsRef<[u8]> + Send + Sync> = data.clone();
            let blob = Blob::new(shared);
            blobs.insert(blob.id(), slot);
            let families = cx.collection.register_fonts(blob, None);
            let [(family, faces)] = families.as_slice() else {
                return Err(LibraryRefusal::Family);
            };
            if faces.len() != 1 || cx.collection.family_name(*family) != Some(pin.family) {
                return Err(LibraryRefusal::Family);
            }
            bytes.push(Arc::clone(data));
        }
        let stacks = StackKind::ALL
            .into_iter()
            .map(|k| {
                let names = stack_faces(k, han)
                    .into_iter()
                    .filter_map(fonts::pin)
                    .map(|p| FontFamilyName::named(p.family))
                    .collect();
                (k, names)
            })
            .collect();
        Ok(Library {
            cx,
            bytes,
            blobs,
            stacks,
            krilla: BTreeMap::new(),
        })
    }

    /// The family names of `kind`'s stack.
    pub fn stack(&self, kind: StackKind) -> &[FontFamilyName<'static>] {
        self.stacks.get(&kind).map(Vec::as_slice).unwrap_or(&[])
    }

    /// The slot a shaped run's font came from (`None`: not one of ours,
    /// which cannot happen with system fonts off).
    pub fn slot_of(&self, font: &FontData) -> Option<usize> {
        self.blobs.get(&font.data.id()).copied()
    }

    /// The krilla font of `slot`.
    pub fn krilla_font(&mut self, slot: usize) -> Option<krilla::text::Font> {
        if let Some(f) = self.krilla.get(&slot) {
            return Some(f.clone());
        }
        let data: krilla::Data = Arc::clone(self.bytes.get(slot)?).into();
        let font = krilla::text::Font::new(data, 0)?;
        self.krilla.insert(slot, font.clone());
        Some(font)
    }
}
