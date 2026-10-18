//! PBKDF2 (RFC 8018 §5.2) on HMAC, with cancellation and progress (D4).
//!
//! The `pbkdf2` crate cannot be interrupted, and the first stage alone can run
//! for seconds. These two functions are written from RFC 8018 and held to the
//! RFC 6070 and RFC 7914 vectors and to a differential loop against the
//! `pbkdf2` crate (a dev-dependency only).
//!
//! The HMAC is keyed once and its state cloned for every iteration, so the
//! password is not re-keyed each time; `U` and `T` are zeroized on every exit.
#![deny(clippy::arithmetic_side_effects)]

use hmac::digest::{FixedOutput, Output, Update};
use hmac::{Hmac, KeyInit};
use sha1::Sha1;
use sha2::Sha256;
use zeroize::Zeroize;

use crate::cancel::{JobCtx, Phase};
use crate::error::BackupError;

/// Shares one progress bar across both derivation stages: iteration counts
/// are reported as `base + done` out of `total`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct KdfProgress {
    pub base: u64,
    pub total: u64,
}

/// The token is checked every 4,096 iterations.
const CANCEL_MASK: u64 = (1 << 12) - 1;
/// Progress is reported every 65,536 iterations.
pub(crate) const PROGRESS_EVERY: u64 = 1 << 16;
const PROGRESS_MASK: u64 = PROGRESS_EVERY - 1;

/// PBKDF2-HMAC-SHA256.
pub(crate) fn pbkdf2_hmac_sha256(
    password: &[u8],
    salt: &[u8],
    rounds: u32,
    out: &mut [u8],
    ctx: &mut JobCtx<'_>,
    prog: KdfProgress,
) -> Result<(), BackupError> {
    pbkdf2_generic::<Hmac<Sha256>>(password, salt, rounds, out, ctx, prog)
}

/// PBKDF2-HMAC-SHA1.
pub(crate) fn pbkdf2_hmac_sha1(
    password: &[u8],
    salt: &[u8],
    rounds: u32,
    out: &mut [u8],
    ctx: &mut JobCtx<'_>,
    prog: KdfProgress,
) -> Result<(), BackupError> {
    pbkdf2_generic::<Hmac<Sha1>>(password, salt, rounds, out, ctx, prog)
}

fn pbkdf2_generic<P>(
    password: &[u8],
    salt: &[u8],
    rounds: u32,
    out: &mut [u8],
    ctx: &mut JobCtx<'_>,
    prog: KdfProgress,
) -> Result<(), BackupError>
where
    P: KeyInit + Update + FixedOutput + Clone,
{
    if rounds == 0 {
        return Err(BackupError::Internal("PBKDF2 with zero rounds"));
    }
    ctx.check()?;
    let prf = P::new_from_slice(password).map_err(|_| BackupError::Internal("HMAC key"))?;
    let mut u = Output::<P>::default();
    let mut t = Output::<P>::default();
    let result = derive(&prf, salt, rounds, out, ctx, prog, &mut u, &mut t);
    u.as_mut_slice().zeroize();
    t.as_mut_slice().zeroize();
    if result.is_err() {
        out.zeroize();
    }
    result
}

#[allow(clippy::too_many_arguments)] // private; the buffers are passed in so the caller can wipe them
fn derive<P>(
    prf: &P,
    salt: &[u8],
    rounds: u32,
    out: &mut [u8],
    ctx: &mut JobCtx<'_>,
    prog: KdfProgress,
    u: &mut Output<P>,
    t: &mut Output<P>,
) -> Result<(), BackupError>
where
    P: Update + FixedOutput + Clone,
{
    let block_len = u.len();
    let mut done = prog.base;
    for (index, chunk) in out.chunks_mut(block_len).enumerate() {
        let block_number = u32::try_from(index)
            .ok()
            .and_then(|i| i.checked_add(1))
            .ok_or(BackupError::Internal("PBKDF2 output too long"))?;

        // U1 = PRF(P, S ‖ INT(i)); T = U1.
        let mut first = prf.clone();
        first.update(salt);
        first.update(&block_number.to_be_bytes());
        first.finalize_into(u);
        t.copy_from_slice(u.as_slice());
        tick(&mut done, ctx, prog)?;

        // Uj = PRF(P, Uj-1); T ^= Uj.
        for _ in 1..rounds {
            let mut next = prf.clone();
            next.update(u.as_slice());
            next.finalize_into(u);
            for (a, b) in t.iter_mut().zip(u.iter()) {
                *a ^= *b;
            }
            tick(&mut done, ctx, prog)?;
        }

        let take = t
            .get(..chunk.len())
            .ok_or(BackupError::Internal("PBKDF2 block length"))?;
        chunk.copy_from_slice(take);
    }
    // A final report, unless the last periodic report already landed on the total.
    if done == prog.total && done & PROGRESS_MASK != 0 {
        ctx.report(Phase::DerivingKey, done, Some(prog.total));
    }
    Ok(())
}

#[inline]
fn tick(done: &mut u64, ctx: &mut JobCtx<'_>, prog: KdfProgress) -> Result<(), BackupError> {
    *done = done.saturating_add(1);
    if *done & CANCEL_MASK == 0 {
        ctx.check()?;
    }
    if *done & PROGRESS_MASK == 0 {
        ctx.report(Phase::DerivingKey, *done, Some(prog.total));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::arithmetic_side_effects)]
    use super::*;
    use crate::cancel::{CancelToken, Progress};

    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
            .collect()
    }

    fn sha1(p: &[u8], s: &[u8], c: u32, len: usize) -> Vec<u8> {
        let token = CancelToken::new();
        let mut sink = |_: Progress| {};
        let mut ctx = JobCtx::new(&token, &mut sink);
        let mut out = vec![0; len];
        let total = u64::from(c) * len.div_ceil(20) as u64;
        pbkdf2_hmac_sha1(p, s, c, &mut out, &mut ctx, KdfProgress { base: 0, total })
            .expect("derive");
        out
    }

    fn sha256(p: &[u8], s: &[u8], c: u32, len: usize) -> Vec<u8> {
        let token = CancelToken::new();
        let mut sink = |_: Progress| {};
        let mut ctx = JobCtx::new(&token, &mut sink);
        let mut out = vec![0; len];
        let total = u64::from(c) * len.div_ceil(32) as u64;
        pbkdf2_hmac_sha256(p, s, c, &mut out, &mut ctx, KdfProgress { base: 0, total })
            .expect("derive");
        out
    }

    // RFC 6070 §2, transcribed from the RFC text.
    #[test]
    fn rfc6070_sha1() {
        assert_eq!(
            sha1(b"password", b"salt", 1, 20),
            hex("0c 60 c8 0f 96 1f 0e 71 f3 a9 b5 24 af 60 12 06 2f e0 37 a6")
        );
        assert_eq!(
            sha1(b"password", b"salt", 2, 20),
            hex("ea 6c 01 4d c7 2d 6f 8c cd 1e d9 2a ce 1d 41 f0 d8 de 89 57")
        );
        assert_eq!(
            sha1(b"password", b"salt", 4096, 20),
            hex("4b 00 79 01 b7 65 48 9a be ad 49 d9 26 f7 21 d0 65 a4 29 c1")
        );
        assert_eq!(
            sha1(
                b"passwordPASSWORDpassword",
                b"saltSALTsaltSALTsaltSALTsaltSALTsalt",
                4096,
                25
            ),
            hex("3d 2e ec 4f e4 1c 84 9b 80 c8 d8 36 62 c0 e4 4a 8b 29 1a 96 4c f2 f0 70 38")
        );
        assert_eq!(
            sha1(b"pass\0word", b"sa\0lt", 4096, 16),
            hex("56 fa 6a a7 55 48 09 9d cc 37 d7 f0 34 25 e0 c3")
        );
    }

    #[test]
    #[ignore = "16,777,216 iterations; run with --ignored"]
    fn rfc6070_sha1_16m() {
        assert_eq!(
            sha1(b"password", b"salt", 16_777_216, 20),
            hex("ee fe 3d 61 cd 4d a4 e4 e9 94 5b 3d 6b a2 15 8c 26 34 e9 84")
        );
    }

    // RFC 7914 §11, transcribed from the RFC text.
    #[test]
    fn rfc7914_sha256() {
        assert_eq!(
            sha256(b"passwd", b"salt", 1, 64),
            hex("55 ac 04 6e 56 e3 08 9f ec 16 91 c2 25 44 b6 05
                 f9 41 85 21 6d de 04 65 e6 8b 9d 57 c2 0d ac bc
                 49 ca 9c cc f1 79 b6 45 99 16 64 b3 9d 77 ef 31
                 7c 71 b8 45 b1 e3 0b d5 09 11 20 41 d3 a1 97 83")
        );
        assert_eq!(
            sha256(b"Password", b"NaCl", 80_000, 64),
            hex("4d dc d8 f6 0b 98 be 21 83 0c ee 5e f2 27 01 f9
                 64 1a 44 18 d0 4c 04 14 ae ff 08 87 6b 34 ab 56
                 a1 d4 25 a1 22 58 33 54 9a db 84 1b 51 c9 b3 17
                 6a 27 2b de bb a1 d0 78 47 8f 62 b3 97 f3 3c 8d")
        );
    }

    /// A fixed-seed byte generator (xorshift64*), so no RNG crate is needed.
    struct Gen(u64);
    impl Gen {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
        fn bytes(&mut self, n: usize) -> Vec<u8> {
            (0..n).map(|_| self.next() as u8).collect()
        }
    }

    #[test]
    fn differential_against_the_pbkdf2_crate() {
        let mut g = Gen(0x0123_4567_89AB_CDEF);
        for case in 0..200 {
            let plen = g.below(130) as usize;
            let slen = g.below(65) as usize;
            let rounds = 1 + g.below(50) as u32;
            let olen = 1 + g.below(64) as usize;
            let p = g.bytes(plen);
            let s = g.bytes(slen);
            let mut want = vec![0; olen];
            if case % 2 == 0 {
                pbkdf2::pbkdf2_hmac::<Sha256>(&p, &s, rounds, &mut want);
                assert_eq!(sha256(&p, &s, rounds, olen), want, "sha256 case {case}");
            } else {
                pbkdf2::pbkdf2_hmac::<Sha1>(&p, &s, rounds, &mut want);
                assert_eq!(sha1(&p, &s, rounds, olen), want, "sha1 case {case}");
            }
        }
    }

    #[test]
    fn cancellation_stops_at_the_next_checkpoint_and_progress_is_bounded() {
        let token = CancelToken::new();
        let mut reports: Vec<Progress> = Vec::new();
        let t2 = token.clone();
        let mut sink = |p: Progress| {
            reports.push(p);
            t2.cancel();
        };
        let mut ctx = JobCtx::new(&token, &mut sink);
        let mut out = [0u8; 32];
        let total = 1_000_000;
        let r = pbkdf2_hmac_sha256(
            b"pw",
            b"salt",
            1_000_000,
            &mut out,
            &mut ctx,
            KdfProgress { base: 0, total },
        );
        assert!(matches!(r, Err(BackupError::Cancelled)));
        assert_eq!(reports.len(), 1, "cancelled before a second report");
        assert_eq!(out, [0u8; 32], "output is wiped on cancellation");
    }

    #[test]
    fn progress_is_monotonic_ends_at_total_and_is_rate_limited() {
        let token = CancelToken::new();
        let mut reports: Vec<u64> = Vec::new();
        let mut sink = |p: Progress| {
            assert_eq!(p.phase, Phase::DerivingKey);
            reports.push(p.done);
        };
        let mut ctx = JobCtx::new(&token, &mut sink);
        // Two stages sharing one bar, like the real derivation.
        let (dpic, iter) = (200_000u32, 70_000u32);
        let total = u64::from(dpic) + 2 * u64::from(iter);
        let mut k1 = [0u8; 32];
        pbkdf2_hmac_sha256(
            b"pw",
            b"dpsl",
            dpic,
            &mut k1,
            &mut ctx,
            KdfProgress { base: 0, total },
        )
        .expect("stage 1");
        let mut k2 = [0u8; 32];
        pbkdf2_hmac_sha1(
            &k1,
            b"salt",
            iter,
            &mut k2,
            &mut ctx,
            KdfProgress {
                base: u64::from(dpic),
                total,
            },
        )
        .expect("stage 2");
        assert!(reports.windows(2).all(|w| w[0] < w[1]), "monotonic");
        assert_eq!(reports.last().copied(), Some(total));
        assert!(reports.len() as u64 <= total.div_ceil(PROGRESS_EVERY));
        // Matches the crate for the same two stages.
        let want1 = pbkdf2::pbkdf2_hmac_array::<Sha256, 32>(b"pw", b"dpsl", dpic);
        let want2 = pbkdf2::pbkdf2_hmac_array::<Sha1, 32>(&want1, b"salt", iter);
        assert_eq!(k2, want2);
    }

    /// The §6.2 acceptance benchmark: within 15 % of `pbkdf2::pbkdf2_hmac`
    /// for 1M SHA-256 rounds. Manual: `cargo test --release -p oma-backup
    /// --lib kdf::tests::benchmark -- --ignored --nocapture`.
    #[test]
    #[ignore = "timing benchmark; run manually in release"]
    fn benchmark_against_the_crate() {
        use std::time::Instant;
        let rounds = 1_000_000;
        let mut best_ours = f64::MAX;
        let mut best_crate = f64::MAX;
        for _ in 0..3 {
            let t = Instant::now();
            let ours = sha256(b"benchmark-password", b"benchmark-salt-20-by", rounds, 32);
            best_ours = best_ours.min(t.elapsed().as_secs_f64());
            let t = Instant::now();
            let theirs = pbkdf2::pbkdf2_hmac_array::<Sha256, 32>(
                b"benchmark-password",
                b"benchmark-salt-20-by",
                rounds,
            );
            best_crate = best_crate.min(t.elapsed().as_secs_f64());
            assert_eq!(ours, theirs.to_vec());
        }
        let ratio = best_ours / best_crate;
        println!("PBKDF2-HMAC-SHA256 1M rounds: ours {best_ours:.3}s, pbkdf2 crate {best_crate:.3}s, ratio {ratio:.3}");
        assert!(ratio <= 1.15, "ratio {ratio:.3}");
    }
}
