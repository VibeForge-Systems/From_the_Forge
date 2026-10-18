//! Keybag TLV parsing (§6.1, D18).
//!
//! Written from the algorithm description in `docs/supported-formats.md`; it
//! is not a translation of any reference implementation. The policy is to fail
//! closed only on what decryption actually uses; anything else unexpected
//! becomes a [`KeybagNote`].
//!
//! Each record is `tag[4] ‖ len[4, big-endian] ‖ value[len]`. The first `UUID`
//! is the header UUID; every later `UUID` starts a class record. Tags before
//! the second `UUID` belong to the header, tags after it to the current class,
//! except header-only tags (`SALT`, `ITER`, `DPIC`, `DPSL` and the like): those
//! go to the header wherever they appear, with a
//! [`KeybagNote::MisplacedHeaderTag`]. That the header tags always precede the
//! first class record is **UNVERIFIED**; the MIT oracle accepts them anywhere.
#![deny(clippy::arithmetic_side_effects)]

use crate::error::{BackupError, MalformedDetail, UnsupportedReason};
use crate::limits::{LimitKind, Limits};
use crate::plists::BackupFile;

/// Something unexpected in a keybag that does not stop decryption.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeybagNote {
    /// Bytes after the last record, too few for a record header.
    TrailingBytes(u32),
    /// Tags the parser does not know, or known tags in the wrong scope.
    UnknownTags(u32),
    /// A tag repeated within one scope; the last value was used.
    DuplicateTag([u8; 4]),
    /// `TYPE & 0x3FFF_FFFF` is not 1 (backup). Local backups being TYPE 1 is
    /// **UNVERIFIED**.
    UnexpectedType(u32),
    /// A `CLAS` value above 0xF.
    UnusualClass(u32),
    /// A value with an unexpected length that decryption does not use, or a
    /// `SALT`/`DPSL` length other than the 20 bytes seen so far.
    UnusualLength([u8; 4]),
    /// A class record whose `CLAS`, `WRAP` or `KTYP` has a bad length, or
    /// that has no `CLAS`; it is unusable.
    UnusableClassRecord,
    /// A class whose `WPKY` is not 40 bytes. Fatal only if the class is used.
    BadWrappedKeyLength(u32),
    /// A class without `KTYP`; treated as 0 (AES).
    MissingKeyType(u32),
    /// A header-only tag (for example `DPIC` or `SALT`) after a class record
    /// had started. It was used as a header value.
    MisplacedHeaderTag([u8; 4]),
}

/// The key-derivation parameters.
#[derive(Clone)]
pub(crate) struct KdfParams {
    pub salt: Vec<u8>,
    pub iter: u32,
    pub dpsl: Vec<u8>,
    pub dpic: u32,
}

/// Why a class key cannot be produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClassStatus {
    /// `WRAP & 2` and a 40-byte `WPKY` with `KTYP` 0.
    Usable,
    /// `WRAP` bit 2 unset: wrapped with the device key only.
    DeviceOnly,
    /// `WPKY` is not 40 bytes.
    BadWrappedKeyLength,
    /// `KTYP` other than 0.
    KeyType(u32),
    /// No `WPKY`.
    NoWrappedKey,
}

/// One class record.
#[derive(Clone)]
pub(crate) struct ClassRecord {
    pub clas: u32,
    pub wpky: Vec<u8>,
    pub status: ClassStatus,
}

/// A parsed keybag.
#[derive(Clone)]
pub(crate) struct Keybag {
    pub kdf: KdfParams,
    pub classes: Vec<ClassRecord>,
    pub notes: Vec<KeybagNote>,
}

impl std::fmt::Debug for Keybag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Keybag")
            .field("classes", &self.classes.len())
            .field("notes", &self.notes)
            .finish_non_exhaustive()
    }
}

const HEADER_TAGS: &[&[u8; 4]] = &[
    b"VERS", b"TYPE", b"UUID", b"HMCK", b"WRAP", b"SALT", b"ITER", b"DPWT", b"DPIC", b"DPSL",
];
const CLASS_TAGS: &[&[u8; 4]] = &[b"UUID", b"CLAS", b"WRAP", b"WPKY", b"KTYP", b"PBKY"];

/// Tags that only make sense in the header: in [`HEADER_TAGS`] but not in
/// [`CLASS_TAGS`].
fn header_only(tag: &[u8; 4]) -> bool {
    HEADER_TAGS.contains(&tag) && !CLASS_TAGS.contains(&tag)
}

/// Tag/value pairs of one scope, in order, with last-wins lookup.
#[derive(Default)]
struct Scope<'a> {
    tags: Vec<([u8; 4], &'a [u8])>,
}

impl<'a> Scope<'a> {
    fn insert(&mut self, tag: [u8; 4], value: &'a [u8], notes: &mut Vec<KeybagNote>) {
        if let Some(slot) = self.tags.iter_mut().find(|(t, _)| *t == tag) {
            notes.push(KeybagNote::DuplicateTag(tag));
            slot.1 = value;
        } else {
            self.tags.push((tag, value));
        }
    }

    fn get(&self, tag: &[u8; 4]) -> Option<&'a [u8]> {
        self.tags.iter().find(|(t, _)| t == tag).map(|(_, v)| *v)
    }
}

fn be_u32(v: &[u8]) -> Option<u32> {
    let arr: [u8; 4] = v.try_into().ok()?;
    Some(u32::from_be_bytes(arr))
}

fn malformed(detail: MalformedDetail) -> BackupError {
    BackupError::malformed(BackupFile::Keybag, detail)
}

impl Keybag {
    /// Parse and check a `BackupKeyBag`. Iteration caps are checked here,
    /// before any derivation can start.
    pub(crate) fn parse(bytes: &[u8], limits: &Limits) -> Result<Keybag, BackupError> {
        let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if len > limits.keybag_bytes {
            return Err(BackupError::limit(
                LimitKind::KeybagBytes,
                len,
                limits.keybag_bytes,
            ));
        }
        let mut notes = Vec::new();
        let mut header = Scope::default();
        let mut classes: Vec<Scope<'_>> = Vec::new();
        let mut seen_header_uuid = false;
        let mut unknown: u32 = 0;
        let class_cap = usize::try_from(limits.keybag_classes).unwrap_or(usize::MAX);

        let mut pos: usize = 0;
        loop {
            let Some(rest) = bytes.get(pos..) else {
                return Err(BackupError::Internal("keybag cursor out of range"));
            };
            if rest.is_empty() {
                break;
            }
            if rest.len() < 8 {
                notes.push(KeybagNote::TrailingBytes(
                    u32::try_from(rest.len()).unwrap_or(u32::MAX),
                ));
                break;
            }
            let tag: [u8; 4] = rest
                .get(..4)
                .and_then(|t| t.try_into().ok())
                .ok_or(BackupError::Internal("keybag tag"))?;
            let declared = rest
                .get(4..8)
                .and_then(be_u32)
                .ok_or(BackupError::Internal("keybag length"))?;
            let declared =
                usize::try_from(declared).map_err(|_| malformed(MalformedDetail::Truncated))?;
            let end = declared
                .checked_add(8)
                .ok_or_else(|| malformed(MalformedDetail::Truncated))?;
            let value = rest
                .get(8..end)
                .ok_or_else(|| malformed(MalformedDetail::Truncated))?;
            pos = pos
                .checked_add(end)
                .ok_or_else(|| malformed(MalformedDetail::Truncated))?;

            if &tag == b"UUID" {
                if !seen_header_uuid {
                    seen_header_uuid = true;
                    header.insert(tag, value, &mut notes);
                } else {
                    if classes.len() >= class_cap {
                        return Err(BackupError::limit(
                            LimitKind::KeybagClasses,
                            u64::try_from(classes.len())
                                .unwrap_or(u64::MAX)
                                .saturating_add(1),
                            u64::from(limits.keybag_classes),
                        ));
                    }
                    let mut scope = Scope::default();
                    scope.insert(tag, value, &mut notes);
                    classes.push(scope);
                }
                continue;
            }
            match classes.last_mut() {
                None => {
                    if HEADER_TAGS.contains(&&tag) {
                        header.insert(tag, value, &mut notes);
                    } else {
                        unknown = unknown.saturating_add(1);
                    }
                }
                Some(scope) => {
                    if CLASS_TAGS.contains(&&tag) {
                        scope.insert(tag, value, &mut notes);
                    } else if header_only(&tag) {
                        notes.push(KeybagNote::MisplacedHeaderTag(tag));
                        header.insert(tag, value, &mut notes);
                    } else {
                        unknown = unknown.saturating_add(1);
                    }
                }
            }
        }
        if unknown > 0 {
            notes.push(KeybagNote::UnknownTags(unknown));
        }

        // Header values decryption does not use: notes only.
        if let Some(t) = header.get(b"TYPE") {
            match be_u32(t) {
                Some(t) => {
                    let masked = t & 0x3FFF_FFFF;
                    if masked != 1 {
                        notes.push(KeybagNote::UnexpectedType(masked));
                    }
                }
                None => notes.push(KeybagNote::UnusualLength(*b"TYPE")),
            }
        }
        for tag in [b"VERS", b"DPWT"] {
            if let Some(v) = header.get(tag) {
                if v.len() != 4 {
                    notes.push(KeybagNote::UnusualLength(*tag));
                }
            }
        }
        if let Some(v) = header.get(b"UUID") {
            if v.len() != 16 {
                notes.push(KeybagNote::UnusualLength(*b"UUID"));
            }
        }

        // Class records.
        let mut records: Vec<ClassRecord> = Vec::with_capacity(classes.len());
        for scope in &classes {
            if let Some(v) = scope.get(b"UUID") {
                if v.len() != 16 {
                    notes.push(KeybagNote::UnusualLength(*b"UUID"));
                }
            }
            let clas = match scope.get(b"CLAS").map(be_u32) {
                Some(Some(c)) => c,
                _ => {
                    notes.push(KeybagNote::UnusableClassRecord);
                    continue;
                }
            };
            if clas > 0xF {
                notes.push(KeybagNote::UnusualClass(clas));
            }
            if records.iter().any(|r| r.clas == clas) {
                return Err(malformed(MalformedDetail::DuplicateClass(clas)));
            }
            let wrap = match scope.get(b"WRAP").map(be_u32) {
                Some(Some(w)) => Some(w),
                Some(None) => {
                    notes.push(KeybagNote::UnusableClassRecord);
                    records.push(ClassRecord {
                        clas,
                        wpky: Vec::new(),
                        status: ClassStatus::DeviceOnly,
                    });
                    continue;
                }
                None => None,
            };
            let ktyp = match scope.get(b"KTYP").map(be_u32) {
                Some(Some(k)) => k,
                Some(None) => {
                    notes.push(KeybagNote::UnusableClassRecord);
                    records.push(ClassRecord {
                        clas,
                        wpky: Vec::new(),
                        status: ClassStatus::KeyType(u32::MAX),
                    });
                    continue;
                }
                None => {
                    // Absent KTYP is treated as 0 (AES); that KTYP is always
                    // present is UNVERIFIED.
                    notes.push(KeybagNote::MissingKeyType(clas));
                    0
                }
            };
            let wpky = scope.get(b"WPKY");
            let status = if wrap.unwrap_or(0) & 2 == 0 {
                ClassStatus::DeviceOnly
            } else if ktyp != 0 {
                ClassStatus::KeyType(ktyp)
            } else {
                match wpky {
                    None => ClassStatus::NoWrappedKey,
                    Some(w) if w.len() == 40 => ClassStatus::Usable,
                    Some(_) => {
                        notes.push(KeybagNote::BadWrappedKeyLength(clas));
                        ClassStatus::BadWrappedKeyLength
                    }
                }
            };
            if let Some(p) = scope.get(b"PBKY") {
                if p.is_empty() {
                    notes.push(KeybagNote::UnusualLength(*b"PBKY"));
                }
            }
            records.push(ClassRecord {
                clas,
                wpky: wpky.map(<[u8]>::to_vec).unwrap_or_default(),
                status,
            });
        }

        // KDF parameters: required, with lengths checked and caps applied
        // before any derivation.
        let (Some(dpsl), Some(dpic)) = (header.get(b"DPSL"), header.get(b"DPIC")) else {
            return Err(BackupError::Unsupported(UnsupportedReason::SingleStageKdf));
        };
        let salt = header
            .get(b"SALT")
            .ok_or_else(|| malformed(MalformedDetail::Missing))?;
        let iter = header
            .get(b"ITER")
            .ok_or_else(|| malformed(MalformedDetail::Missing))?;
        for (tag, v) in [(*b"SALT", salt), (*b"DPSL", dpsl)] {
            if v.is_empty() || v.len() > 64 {
                return Err(malformed(MalformedDetail::BadLength { tag }));
            }
            if v.len() != 20 {
                notes.push(KeybagNote::UnusualLength(tag));
            }
        }
        let dpic =
            be_u32(dpic).ok_or_else(|| malformed(MalformedDetail::BadLength { tag: *b"DPIC" }))?;
        let iter =
            be_u32(iter).ok_or_else(|| malformed(MalformedDetail::BadLength { tag: *b"ITER" }))?;
        if dpic == 0 || dpic > limits.max_dpic {
            return Err(BackupError::limit(
                LimitKind::Dpic,
                u64::from(dpic),
                u64::from(limits.max_dpic),
            ));
        }
        if iter == 0 || iter > limits.max_iter {
            return Err(BackupError::limit(
                LimitKind::Iter,
                u64::from(iter),
                u64::from(limits.max_iter),
            ));
        }

        // At least one class must be unwrappable with the password: the same
        // rule `unlock` uses, so a keybag that would accept any password is
        // refused before the derivation runs.
        if !records.iter().any(|r| r.status == ClassStatus::Usable) {
            return Err(BackupError::Unsupported(
                UnsupportedReason::NoPassphraseClasses,
            ));
        }

        Ok(Keybag {
            kdf: KdfParams {
                salt: salt.to_vec(),
                iter,
                dpsl: dpsl.to_vec(),
                dpic,
            },
            classes: records,
            notes,
        })
    }

    /// The record for a class, if there is one.
    pub(crate) fn class(&self, clas: u32) -> Option<&ClassRecord> {
        self.classes.iter().find(|c| c.clas == clas)
    }
}

/// Test-only keybag builder, shared with `keys.rs`.
#[cfg(test)]
pub(crate) mod build {
    #![allow(clippy::arithmetic_side_effects)]

    pub fn tlv(out: &mut Vec<u8>, tag: &[u8; 4], value: &[u8]) {
        out.extend_from_slice(tag);
        out.extend_from_slice(&(value.len() as u32).to_be_bytes());
        out.extend_from_slice(value);
    }

    pub fn tlv_u32(out: &mut Vec<u8>, tag: &[u8; 4], v: u32) {
        tlv(out, tag, &v.to_be_bytes());
    }

    /// A header with the given KDF parameters.
    pub fn header(salt: &[u8], iter: u32, dpsl: &[u8], dpic: u32) -> Vec<u8> {
        let mut kb = Vec::new();
        tlv_u32(&mut kb, b"VERS", 4);
        tlv_u32(&mut kb, b"TYPE", 1);
        tlv(&mut kb, b"UUID", &[0x11; 16]);
        tlv(&mut kb, b"HMCK", &[0x22; 40]);
        tlv_u32(&mut kb, b"WRAP", 0);
        tlv(&mut kb, b"SALT", salt);
        tlv_u32(&mut kb, b"ITER", iter);
        tlv_u32(&mut kb, b"DPWT", 1);
        tlv_u32(&mut kb, b"DPIC", dpic);
        tlv(&mut kb, b"DPSL", dpsl);
        kb
    }

    pub fn class(out: &mut Vec<u8>, clas: u32, wrap: u32, ktyp: u32, wpky: &[u8]) {
        tlv(out, b"UUID", &[clas as u8; 16]);
        tlv_u32(out, b"CLAS", clas);
        tlv_u32(out, b"WRAP", wrap);
        tlv_u32(out, b"KTYP", ktyp);
        tlv(out, b"WPKY", wpky);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::arithmetic_side_effects)]
    use super::build::*;
    use super::*;

    fn valid() -> Vec<u8> {
        let mut kb = header(&[1; 20], 10, &[2; 20], 1000);
        for c in 1..=4 {
            class(&mut kb, c, 2, 0, &[c as u8; 40]);
        }
        kb
    }

    fn parse(kb: &[u8]) -> Result<Keybag, BackupError> {
        Keybag::parse(kb, &Limits::default())
    }

    #[test]
    fn a_valid_keybag_parses() {
        let kb = parse(&valid()).expect("valid");
        assert_eq!(kb.classes.len(), 4);
        assert_eq!(kb.kdf.iter, 10);
        assert_eq!(kb.kdf.dpic, 1000);
        assert!(kb.notes.is_empty(), "{:?}", kb.notes);
        assert!(kb.classes.iter().all(|c| c.status == ClassStatus::Usable));
    }

    #[test]
    fn a_header_without_wrap_still_assigns_by_position() {
        let mut kb = Vec::new();
        tlv_u32(&mut kb, b"VERS", 4);
        tlv(&mut kb, b"UUID", &[0; 16]);
        tlv(&mut kb, b"SALT", &[1; 20]);
        tlv_u32(&mut kb, b"ITER", 10);
        tlv_u32(&mut kb, b"DPIC", 10);
        tlv(&mut kb, b"DPSL", &[2; 20]);
        class(&mut kb, 3, 2, 0, &[3; 40]);
        let parsed = parse(&kb).expect("valid");
        assert_eq!(parsed.classes.len(), 1);
        assert_eq!(parsed.class(3).map(|c| c.status), Some(ClassStatus::Usable));
    }

    #[test]
    fn truncation_at_every_offset_never_panics() {
        let kb = valid();
        for cut in 0..kb.len() {
            let prefix = kb.get(..cut).expect("prefix");
            // Any result is fine; a panic is not.
            let _ = parse(prefix);
        }
        // Cutting inside a record's value is fatal.
        let cut = kb.get(..kb.len() - 5).expect("prefix");
        assert!(matches!(
            parse(cut),
            Err(BackupError::Malformed {
                file: BackupFile::Keybag,
                detail: MalformedDetail::Truncated
            })
        ));
    }

    #[test]
    fn a_length_near_u32_max_is_truncated_not_a_panic() {
        let mut kb = valid();
        kb.extend_from_slice(b"WPKY");
        kb.extend_from_slice(&0xFFFF_FFF8u32.to_be_bytes());
        kb.extend_from_slice(&[0; 4]);
        assert!(matches!(
            parse(&kb),
            Err(BackupError::Malformed {
                detail: MalformedDetail::Truncated,
                ..
            })
        ));
    }

    #[test]
    fn short_trailing_bytes_are_a_note() {
        let mut kb = valid();
        kb.extend_from_slice(&[0; 7]);
        let parsed = parse(&kb).expect("valid");
        assert_eq!(parsed.notes, vec![KeybagNote::TrailingBytes(7)]);
    }

    #[test]
    fn duplicate_tags_are_last_wins_with_a_note() {
        let mut kb = header(&[1; 20], 10, &[2; 20], 1000);
        tlv_u32(&mut kb, b"ITER", 20);
        class(&mut kb, 1, 2, 0, &[1; 40]);
        let parsed = parse(&kb).expect("valid");
        assert_eq!(parsed.kdf.iter, 20);
        assert!(parsed.notes.contains(&KeybagNote::DuplicateTag(*b"ITER")));
    }

    #[test]
    fn duplicate_clas_is_malformed() {
        let mut kb = valid();
        class(&mut kb, 3, 2, 0, &[9; 40]);
        assert!(matches!(
            parse(&kb),
            Err(BackupError::Malformed {
                detail: MalformedDetail::DuplicateClass(3),
                ..
            })
        ));
    }

    #[test]
    fn a_short_wpky_on_an_unused_class_is_a_note() {
        let mut kb = valid();
        class(&mut kb, 11, 2, 0, &[9; 39]);
        let parsed = parse(&kb).expect("valid");
        assert!(parsed.notes.contains(&KeybagNote::BadWrappedKeyLength(11)));
        assert_eq!(
            parsed.class(11).map(|c| c.status),
            Some(ClassStatus::BadWrappedKeyLength)
        );
    }

    #[test]
    fn type_and_clas_oddities_are_notes() {
        let mut kb = Vec::new();
        tlv_u32(&mut kb, b"TYPE", 0x4000_0001); // flag bits masked off: 1
        tlv(&mut kb, b"UUID", &[0; 16]);
        tlv(&mut kb, b"SALT", &[1; 20]);
        tlv_u32(&mut kb, b"ITER", 10);
        tlv_u32(&mut kb, b"DPIC", 10);
        tlv(&mut kb, b"DPSL", &[2; 20]);
        class(&mut kb, 1, 2, 0, &[1; 40]);
        assert!(parse(&kb).expect("valid").notes.is_empty());

        let mut kb2 = header(&[1; 20], 10, &[2; 20], 1000);
        // Replace TYPE 1 with TYPE 2.
        let pos = kb2.windows(4).position(|w| w == b"TYPE").expect("TYPE");
        kb2[pos + 8..pos + 12].copy_from_slice(&2u32.to_be_bytes());
        class(&mut kb2, 1, 2, 0, &[1; 40]);
        class(&mut kb2, 0x10, 2, 0, &[1; 40]);
        let notes = parse(&kb2).expect("valid").notes;
        assert!(notes.contains(&KeybagNote::UnexpectedType(2)));
        assert!(notes.contains(&KeybagNote::UnusualClass(0x10)));
    }

    #[test]
    fn refusals() {
        // Missing DPSL: single-stage KDF.
        let mut kb = Vec::new();
        tlv(&mut kb, b"UUID", &[0; 16]);
        tlv(&mut kb, b"SALT", &[1; 20]);
        tlv_u32(&mut kb, b"ITER", 10);
        tlv_u32(&mut kb, b"DPIC", 10);
        class(&mut kb, 1, 2, 0, &[1; 40]);
        assert!(matches!(
            parse(&kb),
            Err(BackupError::Unsupported(UnsupportedReason::SingleStageKdf))
        ));

        // All WRAP = 1: no passphrase classes.
        let mut kb = header(&[1; 20], 10, &[2; 20], 1000);
        for c in 1..=3 {
            class(&mut kb, c, 1, 0, &[1; 40]);
        }
        assert!(matches!(
            parse(&kb),
            Err(BackupError::Unsupported(
                UnsupportedReason::NoPassphraseClasses
            ))
        ));

        // KTYP 1 is recorded on the class (fatal only when used).
        let mut kb = valid();
        class(&mut kb, 12, 2, 1, &[1; 40]);
        assert_eq!(
            parse(&kb).expect("valid").class(12).map(|c| c.status),
            Some(ClassStatus::KeyType(1))
        );

        // 65 classes.
        let mut kb = header(&[1; 20], 10, &[2; 20], 1000);
        for c in 1..=65 {
            class(&mut kb, c, 2, 0, &[1; 40]);
        }
        assert!(matches!(
            parse(&kb),
            Err(BackupError::LimitExceeded {
                what: LimitKind::KeybagClasses,
                ..
            })
        ));

        // 64 KiB + 1.
        let mut kb = valid();
        kb.resize(64 * 1024 + 1, 0);
        assert!(matches!(
            parse(&kb),
            Err(BackupError::LimitExceeded {
                what: LimitKind::KeybagBytes,
                value: 65_537,
                cap: 65_536
            })
        ));
    }

    #[test]
    fn header_tags_after_a_class_record_go_to_the_header() {
        // DPIC and DPSL after the class records: still a two-stage KDF.
        let mut kb = Vec::new();
        tlv_u32(&mut kb, b"VERS", 4);
        tlv(&mut kb, b"UUID", &[0; 16]);
        tlv(&mut kb, b"SALT", &[1; 20]);
        tlv_u32(&mut kb, b"ITER", 10);
        class(&mut kb, 1, 2, 0, &[1; 40]);
        class(&mut kb, 2, 2, 0, &[2; 40]);
        tlv_u32(&mut kb, b"DPIC", 1000);
        tlv(&mut kb, b"DPSL", &[2; 20]);
        let parsed = parse(&kb).expect("two-stage parameters found");
        assert_eq!(parsed.kdf.dpic, 1000);
        assert_eq!(parsed.kdf.dpsl, vec![2; 20]);
        assert!(parsed
            .notes
            .contains(&KeybagNote::MisplacedHeaderTag(*b"DPIC")));
        assert!(parsed
            .notes
            .contains(&KeybagNote::MisplacedHeaderTag(*b"DPSL")));
        assert!(!parsed
            .notes
            .iter()
            .any(|n| matches!(n, KeybagNote::UnknownTags(_))));
        // The class record before them is unaffected.
        assert_eq!(parsed.class(2).map(|c| c.status), Some(ClassStatus::Usable));

        // SALT and ITER late as well.
        let mut kb = Vec::new();
        tlv(&mut kb, b"UUID", &[0; 16]);
        tlv_u32(&mut kb, b"DPIC", 1000);
        tlv(&mut kb, b"DPSL", &[2; 20]);
        class(&mut kb, 1, 2, 0, &[1; 40]);
        tlv(&mut kb, b"SALT", &[1; 20]);
        tlv_u32(&mut kb, b"ITER", 10);
        let parsed = parse(&kb).expect("SALT and ITER found");
        assert_eq!(parsed.kdf.iter, 10);
    }

    #[test]
    fn no_usable_class_is_refused_before_derivation() {
        // The only passphrase-wrapped class has KTYP 1.
        let mut kb = header(&[1; 20], 10, &[2; 20], 1000);
        class(&mut kb, 1, 1, 0, &[1; 40]);
        class(&mut kb, 2, 2, 1, &[2; 40]);
        assert!(matches!(
            parse(&kb),
            Err(BackupError::Unsupported(
                UnsupportedReason::NoPassphraseClasses
            ))
        ));

        // The only passphrase-wrapped record has no CLAS.
        let mut kb = header(&[1; 20], 10, &[2; 20], 1000);
        class(&mut kb, 1, 1, 0, &[1; 40]);
        tlv(&mut kb, b"UUID", &[9; 16]);
        tlv_u32(&mut kb, b"WRAP", 2);
        tlv_u32(&mut kb, b"KTYP", 0);
        tlv(&mut kb, b"WPKY", &[9; 40]);
        assert!(matches!(
            parse(&kb),
            Err(BackupError::Unsupported(
                UnsupportedReason::NoPassphraseClasses
            ))
        ));
    }

    #[test]
    fn iteration_caps_apply_before_any_derivation() {
        for (dpic, iter, kind) in [
            (0, 10, LimitKind::Dpic),
            (20_000_001, 10, LimitKind::Dpic),
            (0xFFFF_FFFF, 10, LimitKind::Dpic),
            (1000, 0, LimitKind::Iter),
            (1000, 1_000_001, LimitKind::Iter),
        ] {
            let mut kb = header(&[1; 20], iter, &[2; 20], dpic);
            class(&mut kb, 1, 2, 0, &[1; 40]);
            match parse(&kb) {
                Err(BackupError::LimitExceeded { what, .. }) => assert_eq!(what, kind),
                other => panic!("dpic {dpic} iter {iter}: {other:?}"),
            }
        }
        // The caps themselves are accepted.
        let mut kb = header(&[1; 20], 1_000_000, &[2; 20], 20_000_000);
        class(&mut kb, 1, 2, 0, &[1; 40]);
        assert!(parse(&kb).is_ok());
    }

    #[test]
    fn bad_kdf_lengths_are_fatal_and_unusual_ones_are_notes() {
        let mut kb = header(&[1; 20], 10, &[2; 65], 1000);
        class(&mut kb, 1, 2, 0, &[1; 40]);
        assert!(matches!(
            parse(&kb),
            Err(BackupError::Malformed {
                detail: MalformedDetail::BadLength {
                    tag: [b'D', b'P', b'S', b'L']
                },
                ..
            })
        ));
        let mut kb = header(&[1; 16], 10, &[2; 20], 1000);
        class(&mut kb, 1, 2, 0, &[1; 40]);
        assert!(parse(&kb)
            .expect("valid")
            .notes
            .contains(&KeybagNote::UnusualLength(*b"SALT")));
    }
}
