//! Identity and rank (spec §6.2, §8.7, D56, D81, D83).
//!
//! **Tagged encoding.** Every key, fingerprint and observed key encodes each
//! field as a tag byte followed by its payload:
//!
//! | Tag | Field | Payload |
//! |---|---|---|
//! | 0 | Absent | none |
//! | 1 | Null | none |
//! | 2 | Unreadable | the kind (one byte, the `value_issue` kind code) |
//! | 3 | Oversized | the byte count, u64 big-endian |
//! | 4 | Value | bytes: their length (u64 big-endian) then the bytes; integers: i64 big-endian |
//!
//! So NULL, `''` and an absent column never encode the same (C2.10). A
//! column holds either bytes or integers, so the two value forms are never
//! compared with each other. The encodings are pinned byte for byte by the
//! tests below.
//!
//! **Keys.** A message, chat or attachment is keyed by its exact GUID bytes
//! and a handle by the tagged (`id`, `service`), when the identifying field
//! is a readable, non-empty value **and** that key occurs once in this
//! source. Duplicates are found by sorting 128-bit truncated SHA-256
//! digests of the key's tagged encoding; a truncation collision can only
//! make a unique key look duplicated, which demotes a row to unkeyed and
//! never merges two rows (D83).
//!
//! **Rank.** 41 bytes compared with memcmp: the order class (1 dated, 0
//! undated), the order key biased by 2^63 (big-endian, zero when undated),
//! then the staged content digest.

use sha2::{Digest, Sha256};

/// One field in the tagged encoding.
#[derive(Clone, Copy)]
pub(crate) enum Field<'a> {
    Absent,
    Null,
    /// The unreadable kind's `value_issue` code (16 and up).
    Unreadable(u8),
    Oversized(u64),
    Bytes(&'a [u8]),
    Int(i64),
}

pub(crate) const TAG_ABSENT: u8 = 0;
pub(crate) const TAG_NULL: u8 = 1;
pub(crate) const TAG_UNREADABLE: u8 = 2;
pub(crate) const TAG_OVERSIZED: u8 = 3;
pub(crate) const TAG_VALUE: u8 = 4;

/// Append `f`'s tagged encoding to `out`.
pub(crate) fn encode(out: &mut Vec<u8>, f: Field<'_>) {
    match f {
        Field::Absent => out.push(TAG_ABSENT),
        Field::Null => out.push(TAG_NULL),
        Field::Unreadable(k) => {
            out.push(TAG_UNREADABLE);
            out.push(k);
        }
        Field::Oversized(n) => {
            out.push(TAG_OVERSIZED);
            out.extend_from_slice(&n.to_be_bytes());
        }
        Field::Bytes(b) => {
            out.push(TAG_VALUE);
            let len = u64::try_from(b.len()).unwrap_or(u64::MAX);
            out.extend_from_slice(&len.to_be_bytes());
            out.extend_from_slice(b);
        }
        Field::Int(i) => {
            out.push(TAG_VALUE);
            out.extend_from_slice(&i.to_be_bytes());
        }
    }
}

/// The tagged encoding of a GUID key (one Value field).
pub(crate) fn guid_key_encoding(guid: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(guid.len().saturating_add(9));
    encode(&mut out, Field::Bytes(guid));
    out
}

/// A handle's natural key: the tagged (`id`, `service`).
pub(crate) fn handle_natural_key(id: &[u8], service: Field<'_>) -> Vec<u8> {
    let mut out = Vec::with_capacity(id.len().saturating_add(32));
    encode(&mut out, Field::Bytes(id));
    encode(&mut out, service);
    out
}

/// The 128-bit truncated SHA-256 of a key's tagged encoding, for the
/// duplicate scan and `import_dup_key`.
pub(crate) fn key_digest(encoding: &[u8]) -> [u8; 16] {
    let full = Sha256::digest(encoding);
    let mut out = [0u8; 16];
    out.copy_from_slice(full.get(..16).unwrap_or(&[0u8; 16]));
    out
}

/// The domain prefix of an unkeyed fingerprint.
pub(crate) const FINGERPRINT_DOMAIN: &[u8] = b"oma-unkeyed-fp-v1";

/// SHA-256 over a fingerprint's encoded fields (`bind::Cell::fp_encode`),
/// prefixed with the domain and the entity code.
pub(crate) fn fingerprint(entity: u8, encoded: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(FINGERPRINT_DOMAIN);
    h.update([entity]);
    h.update(encoded);
    h.finalize().into()
}

/// The length of a rank.
pub const RANK_LEN: usize = 41;

/// A session's rank (spec §8.7). Ordered as bytes (memcmp): undated below
/// dated, then by date, then by content digest. Its `Debug` prints only the
/// order class: the digest is an identifier of the source content.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Rank([u8; RANK_LEN]);

impl Rank {
    /// The rank of a session with this order key (Unix ns) and digest.
    pub(crate) fn new(order_key: Option<i64>, digest: &[u8; 32]) -> Rank {
        let mut b = Vec::with_capacity(RANK_LEN);
        match order_key {
            Some(k) => {
                b.push(1);
                // The bias maps i64 order onto u64 byte order.
                let biased = u64::from_be_bytes(k.to_be_bytes()) ^ (1u64 << 63);
                b.extend_from_slice(&biased.to_be_bytes());
            }
            None => b.extend_from_slice(&[0u8; 9]),
        }
        b.extend_from_slice(digest);
        Rank(<[u8; RANK_LEN]>::try_from(b.as_slice()).unwrap_or([0u8; RANK_LEN]))
    }

    /// A stored rank, if it has the right length.
    pub(crate) fn from_slice(s: &[u8]) -> Option<Rank> {
        <[u8; RANK_LEN]>::try_from(s).ok().map(Rank)
    }

    /// The 41 bytes, as stored.
    pub(crate) fn as_bytes(&self) -> &[u8; RANK_LEN] {
        &self.0
    }

    /// Whether the session had a usable backup date.
    pub fn is_dated(&self) -> bool {
        self.0.first() == Some(&1)
    }
}

impl std::fmt::Debug for Rank {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_dated() {
            f.write_str("Rank(dated)")
        } else {
            f.write_str("Rank(undated)")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(f: Field<'_>) -> Vec<u8> {
        let mut v = Vec::new();
        encode(&mut v, f);
        v
    }

    #[test]
    fn tagged_encodings_are_pinned() {
        assert_eq!(enc(Field::Absent), [0]);
        assert_eq!(enc(Field::Null), [1]);
        assert_eq!(enc(Field::Unreadable(17)), [2, 17]);
        assert_eq!(enc(Field::Oversized(258)), [3, 0, 0, 0, 0, 0, 0, 1, 2]);
        assert_eq!(enc(Field::Bytes(b"")), [4, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(
            enc(Field::Bytes(b"ab")),
            [4, 0, 0, 0, 0, 0, 0, 0, 2, b'a', b'b']
        );
        assert_eq!(
            enc(Field::Int(-2)),
            [4, 255, 255, 255, 255, 255, 255, 255, 254]
        );
        assert_eq!(enc(Field::Int(i64::MIN)), [4, 128, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(
            enc(Field::Int(i64::MAX)),
            [4, 127, 255, 255, 255, 255, 255, 255, 255]
        );
        assert_eq!(guid_key_encoding(b"G"), [4, 0, 0, 0, 0, 0, 0, 0, 1, b'G']);
        assert_eq!(
            handle_natural_key(b"a", Field::Null),
            [4, 0, 0, 0, 0, 0, 0, 0, 1, b'a', 1]
        );
        // Pinned digest of one key, so the truncation never changes.
        assert_eq!(
            key_digest(&guid_key_encoding(b"G")),
            key_digest(&[4, 0, 0, 0, 0, 0, 0, 0, 1, b'G'])
        );
        let d = key_digest(b"");
        assert_eq!(
            d,
            [
                0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f,
                0xb9, 0x24
            ]
        );
    }

    #[test]
    fn null_empty_absent_and_unreadable_never_collide() {
        let forms = [
            enc(Field::Absent),
            enc(Field::Null),
            enc(Field::Bytes(b"")),
            enc(Field::Unreadable(16)),
            enc(Field::Oversized(0)),
        ];
        // A column is either bytes or integers, never both, so a byte and
        // an integer value are never compared: `''` and `0` may encode the
        // same (fingerprints prefix every field with its column code).
        assert_eq!(enc(Field::Bytes(b"")), enc(Field::Int(0)));
        for (i, a) in forms.iter().enumerate() {
            for b in forms.iter().skip(i + 1) {
                assert_ne!(a, b);
            }
        }
        // A handle's service state is part of its key.
        let keys = [
            handle_natural_key(b"x", Field::Absent),
            handle_natural_key(b"x", Field::Null),
            handle_natural_key(b"x", Field::Bytes(b"")),
            handle_natural_key(b"x", Field::Bytes(b"SMS")),
        ];
        for (i, a) in keys.iter().enumerate() {
            for b in keys.iter().skip(i + 1) {
                assert_ne!(key_digest(a), key_digest(b));
            }
        }
    }

    #[test]
    fn ranks_order_as_specified() {
        let d0 = [0u8; 32];
        let d1 = [1u8; 32];
        let undated = Rank::new(None, &d1);
        let neg = Rank::new(Some(-5), &d0);
        let zero = Rank::new(Some(0), &d0);
        let pos = Rank::new(Some(1_700_000_000_000_000_000), &d0);
        let pos_d1 = Rank::new(Some(1_700_000_000_000_000_000), &d1);
        let max = Rank::new(Some(i64::MAX), &d0);
        let min = Rank::new(Some(i64::MIN), &d0);
        let ordered = [undated, min, neg, zero, pos, pos_d1, max];
        for w in ordered.windows(2) {
            assert!(w[0] < w[1], "{:?} {:?}", w[0].as_bytes(), w[1].as_bytes());
        }
        assert_eq!(undated.as_bytes()[..9], [0u8; 9]);
        assert_eq!(zero.as_bytes()[..9], [1, 128, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(&undated.as_bytes()[9..], &d1);
        assert_eq!(Rank::from_slice(pos.as_bytes()), Some(pos));
        assert_eq!(Rank::from_slice(&[0u8; 40]), None);
        assert_eq!(format!("{pos:?} {undated:?}"), "Rank(dated) Rank(undated)");
        // The catalogue's v1 rank (tests/fixtures/imports, imp-dev1-v1-plain).
        let r = Rank::new(Some(1_769_940_000_000_000_000), &d0);
        assert_eq!(
            r.as_bytes()[..9],
            [0x01, 0x98, 0x90, 0x17, 0x0d, 0x6d, 0xc1, 0x40, 0x00]
        );
    }

    #[test]
    fn fingerprints_cover_domain_and_entity() {
        assert_ne!(fingerprint(1, b"x"), fingerprint(2, b"x"));
        assert_ne!(fingerprint(1, b"x"), fingerprint(1, b"y"));
        assert_eq!(fingerprint(3, b"abc"), fingerprint(3, b"abc"));
    }
}
