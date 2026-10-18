//! A seeded mutation harness (M3 §19.2 item 3, D42): no new dependency, no
//! shrinking, reproducible from the seed.
#![allow(dead_code)]

/// xorshift64* (Vigna). Deterministic for a seed.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.max(1))
    }

    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A value in `0..n` (`n > 0`).
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }

    pub fn chance(&mut self, one_in: usize) -> bool {
        self.below(one_in) == 0
    }
}

/// Bytes that mean something to a typedstream reader.
const INTERESTING: [u8; 12] = [
    0x00, 0x01, 0x7f, 0x80, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x92,
];

/// Apply 1–8 mutations: bit flip, byte set, truncate, insert, duplicate a
/// span, or inflate a length.
pub fn mutate(rng: &mut Rng, input: &[u8]) -> Vec<u8> {
    let mut b = input.to_vec();
    let n = 1 + rng.below(8);
    for _ in 0..n {
        let len = b.len();
        match rng.below(6) {
            0 if len > 0 => {
                let p = rng.below(len);
                b[p] ^= 1 << rng.below(8);
            }
            1 if len > 0 => {
                let p = rng.below(len);
                b[p] = if rng.chance(2) {
                    INTERESTING[rng.below(INTERESTING.len())]
                } else {
                    rng.next() as u8
                };
            }
            2 if len > 0 => b.truncate(rng.below(len)),
            3 => {
                let p = rng.below(len + 1);
                let k = 1 + rng.below(8);
                let ins: Vec<u8> = (0..k).map(|_| rng.next() as u8).collect();
                b.splice(p..p, ins);
            }
            4 if len > 1 => {
                let a = rng.below(len);
                let l = 1 + rng.below((len - a).min(64));
                let span = b[a..a + l].to_vec();
                let at = rng.below(len + 1);
                b.splice(at..at, span);
            }
            5 if len > 0 => {
                let p = rng.below(len);
                let big: u32 = match rng.below(3) {
                    0 => u32::MAX,
                    1 => 1 << 31,
                    _ => rng.next() as u32,
                };
                let mut ins = vec![0x82];
                ins.extend_from_slice(&big.to_le_bytes());
                b.splice(p..p + 1, ins);
            }
            _ => {}
        }
    }
    b
}

/// The iteration count: `OMA_TS_MUTATIONS`, else `default`.
pub fn iterations(default: usize) -> usize {
    std::env::var("OMA_TS_MUTATIONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
