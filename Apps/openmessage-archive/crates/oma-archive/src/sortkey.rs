//! The one `message.sort_key` encoder (M4 spec §6.5, M5 spec §3.3).
//!
//! - A **dated** key is `0x00 ‖ be64(ns XOR 2^63)`: 9 bytes, ordered by
//!   the instant (before the Unix epoch included).
//! - An **undated** key is `0x01 ‖ first_rank ‖ be64(first_src_rowid XOR
//!   2^63)`: the source order of the earliest-ranked session.
//!
//! Every undated key sorts after every dated key, so any read that means
//! "recent", "newest" or "on or after a date" is bounded with
//! `sort_key < X'01'` ([`DATED_END`]). The import, the bulk generator and
//! every viewer seek use this module; sort keys never cross IPC.

/// The first byte of a dated key.
pub(crate) const DATED_TAG: u8 = 0x00;
/// The first byte of an undated key.
pub(crate) const UNDATED_PREFIX: u8 = 0x01;
/// The exclusive upper bound of the dated range (and the inclusive lower
/// bound of the undated one): `X'01'`.
pub(crate) const DATED_END: [u8; 1] = [UNDATED_PREFIX];

/// `v XOR 2^63`, big-endian: signed order as unsigned byte order.
fn bias(v: i64) -> [u8; 8] {
    (u64::from_be_bytes(v.to_be_bytes()) ^ (1u64 << 63)).to_be_bytes()
}

fn unbias(b: [u8; 8]) -> i64 {
    i64::from_be_bytes((u64::from_be_bytes(b) ^ (1u64 << 63)).to_be_bytes())
}

/// The key of a message dated at `ns` (Unix nanoseconds).
pub(crate) fn dated(ns: i64) -> [u8; 9] {
    let mut k = [0u8; 9];
    k[0] = DATED_TAG;
    k[1..].copy_from_slice(&bias(ns));
    k
}

/// The key of an undated message.
pub(crate) fn undated(first_rank: &[u8], first_src_rowid: i64) -> Vec<u8> {
    let mut k = Vec::with_capacity(1 + first_rank.len() + 8);
    k.push(UNDATED_PREFIX);
    k.extend_from_slice(first_rank);
    k.extend_from_slice(&bias(first_src_rowid));
    k
}

/// The key of a message: dated at `utc_ns`, or undated (its first rank and
/// first source rowid).
pub(crate) fn encode(utc_ns: Option<i64>, first_rank: &[u8], first_src_rowid: i64) -> Vec<u8> {
    match utc_ns {
        Some(ns) => dated(ns).to_vec(),
        None => undated(first_rank, first_src_rowid),
    }
}

/// The instant of a dated key; `None` for an undated or malformed key.
pub(crate) fn decode_dated(key: &[u8]) -> Option<i64> {
    match key {
        [DATED_TAG, rest @ ..] => <[u8; 8]>::try_from(rest).ok().map(unbias),
        _ => None,
    }
}

/// `true` when `key` lies in the dated range (`key < X'01'`).
pub(crate) fn is_dated(key: &[u8]) -> bool {
    key < DATED_END.as_slice()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dated_keys_round_trip_including_the_extremes() {
        for ns in [i64::MIN, -1, 0, 1, 1_168_000_000_000_000_000, i64::MAX] {
            let k = dated(ns);
            assert_eq!(k.len(), 9);
            assert_eq!(decode_dated(&k), Some(ns), "{ns}");
            assert!(is_dated(&k));
        }
        assert_eq!(decode_dated(&[]), None);
        assert_eq!(decode_dated(&[0, 1, 2]), None);
        assert_eq!(decode_dated(&undated(&[0; 41], 5)), None);
    }

    #[test]
    fn dated_keys_keep_the_order_of_their_instants() {
        let instants = [i64::MIN, -5, -1, 0, 1, 7, 1 << 40, i64::MAX - 1, i64::MAX];
        for w in instants.windows(2) {
            assert!(dated(w[0]) < dated(w[1]), "{} < {}", w[0], w[1]);
        }
    }

    #[test]
    fn every_undated_key_sorts_after_every_dated_key_and_dated_end() {
        let low = undated(&[0u8; 41], i64::MIN);
        let high = undated(&[0xff; 41], i64::MAX);
        for ns in [i64::MIN, 0, i64::MAX] {
            assert!(dated(ns).as_slice() < DATED_END.as_slice());
            assert!(dated(ns).as_slice() < low.as_slice());
        }
        assert!(DATED_END.as_slice() <= low.as_slice());
        assert!(!is_dated(&low) && !is_dated(&high) && !is_dated(&DATED_END));
        assert!(low < high);
        // Within the undated range, the rank then the rowid decide.
        assert!(undated(&[1u8; 41], 9) < undated(&[2u8; 41], -9));
        assert!(undated(&[1u8; 41], -1) < undated(&[1u8; 41], 0));
    }

    #[test]
    fn encode_chooses_the_form() {
        let rank = [7u8; 41];
        assert_eq!(encode(Some(42), &rank, 3), dated(42).to_vec());
        assert_eq!(encode(None, &rank, 3), undated(&rank, 3));
        assert_eq!(encode(None, &rank, 3).len(), 50);
    }
}
