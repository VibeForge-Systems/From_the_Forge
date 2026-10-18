//! AES-256-CBC with strict PKCS#7: streaming decryption and the in-place
//! probe (§6.4, D11).
//!
//! The output size comes from the padding, never from `MBFile.Size`, and the
//! output is never truncated. Plaintext buffers are zeroized.
#![deny(clippy::arithmetic_side_effects)]

use std::io::{self, Read, Seek, SeekFrom, Write};

use aes::cipher::array::Array;
use aes::cipher::block_padding::{Padding, Pkcs7};
use aes::cipher::{BlockModeDecrypt, KeyIvInit};
use aes::Aes256;
use zeroize::{Zeroize, Zeroizing};

use crate::cancel::{JobCtx, Phase};
use crate::error::{BackupError, IoOp, MalformedDetail};
use crate::plists::BackupFile;

type Decryptor = cbc::Decryptor<Aes256>;
type Block = aes::Block;

/// Read size for streaming: 1 MiB.
pub(crate) const CHUNK: usize = 1 << 20;
const BLOCK: u64 = 16;

/// The ciphertext source, its length, and the file it belongs to (named in
/// errors).
pub(crate) struct CbcInput<'r, R: Read> {
    pub src: &'r mut R,
    pub src_len: u64,
    pub file: BackupFile,
}

/// Decrypt `input` into `dst`. `src_len` must be a non-zero multiple of 16;
/// the caller handles a zero length. Returns the plaintext length.
pub(crate) fn decrypt_cbc_pkcs7_stream<R: Read, W: Write>(
    key: &[u8; 32],
    iv: &[u8; 16],
    input: CbcInput<'_, R>,
    dst: &mut W,
    ctx: &mut JobCtx<'_>,
    phase: Phase,
) -> Result<u64, BackupError> {
    let file = input.file;
    let total = input.src_len;
    if total == 0 {
        return Err(BackupError::malformed(file, MalformedDetail::Empty));
    }
    if !total.is_multiple_of(BLOCK) {
        return Err(BackupError::malformed(
            file,
            MalformedDetail::NotBlockAligned,
        ));
    }
    let mut dec = Decryptor::new(&(*key).into(), &(*iv).into());
    let mut buf = Zeroizing::new(vec![0u8; CHUNK]);
    let mut remaining = total;
    let mut written: u64 = 0;
    let mut src = input.src.take(total);
    ctx.report(phase, 0, Some(total));

    while remaining > 0 {
        ctx.check()?;
        let want = usize::try_from(remaining.min(CHUNK as u64))
            .map_err(|_| BackupError::Internal("chunk size"))?;
        let chunk = buf
            .get_mut(..want)
            .ok_or(BackupError::Internal("chunk buffer"))?;
        read_full(&mut src, chunk, file)?;
        remaining = remaining
            .checked_sub(want as u64)
            .ok_or(BackupError::Internal("remaining"))?;

        let (blocks, rest) = Array::slice_as_chunks_mut(chunk);
        if !rest.is_empty() {
            return Err(BackupError::Internal("unaligned chunk"));
        }
        if remaining > 0 {
            dec.decrypt_blocks(blocks);
            write_all(dst, chunk)?;
            written = written.saturating_add(want as u64);
        } else {
            let (body, last) = match blocks.split_last_mut() {
                Some((last, body)) => (body, last),
                None => return Err(BackupError::Internal("empty final chunk")),
            };
            dec.decrypt_blocks(body);
            dec.decrypt_block(last);
            let tail = Pkcs7::unpad(last)
                .map_err(|_| BackupError::malformed(file, MalformedDetail::Padding))?;
            let body_bytes = Array::slice_as_flattened(body);
            write_all(dst, body_bytes)?;
            write_all(dst, tail)?;
            written = written
                .saturating_add(body_bytes.len() as u64)
                .saturating_add(tail.len() as u64);
            last.as_mut_slice().zeroize();
        }
        ctx.report(phase, total.saturating_sub(remaining), Some(total));
    }
    dst.flush()
        .map_err(|e| BackupError::staging_io(IoOp::Write, &e))?;
    Ok(written)
}

fn read_full<R: Read>(src: &mut R, buf: &mut [u8], file: BackupFile) -> Result<(), BackupError> {
    match src.read_exact(buf) {
        Ok(()) => Ok(()),
        // The file is shorter than its metadata said: it changed or is damaged.
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
            Err(BackupError::malformed(file, MalformedDetail::Truncated))
        }
        Err(e) => Err(BackupError::source_io(IoOp::Read, &e)),
    }
}

fn write_all<W: Write>(dst: &mut W, bytes: &[u8]) -> Result<(), BackupError> {
    dst.write_all(bytes)
        .map_err(|e| BackupError::staging_io(IoOp::Write, &e))
}

/// The result of [`probe_first_and_last_block`].
pub(crate) struct Probe {
    /// The first plaintext block, for the SQLite header check.
    pub first_block: Zeroizing<[u8; 16]>,
    /// Whether the last block carries strict PKCS#7 padding.
    pub padding_ok: bool,
}

/// D11: decrypt only the first block (zero IV) and the last block (IV = the
/// previous ciphertext block, or zero if there is only one), in memory, and
/// check strict PKCS#7 on the last block. Writes nothing.
pub(crate) fn probe_first_and_last_block<R: Read + Seek>(
    key: &[u8; 32],
    src: &mut R,
    src_len: u64,
) -> Result<Probe, BackupError> {
    let file = BackupFile::Blob;
    if src_len == 0 {
        return Err(BackupError::malformed(file, MalformedDetail::Empty));
    }
    if !src_len.is_multiple_of(BLOCK) {
        return Err(BackupError::malformed(
            file,
            MalformedDetail::NotBlockAligned,
        ));
    }
    let zero_iv = [0u8; 16];

    let mut first = Block::default();
    seek(src, 0)?;
    read_full(src, first.as_mut_slice(), file)?;
    let mut first_plain = first;
    Decryptor::new(&(*key).into(), &zero_iv.into()).decrypt_block(&mut first_plain);

    let (iv, mut last) = if src_len == BLOCK {
        (Block::from(zero_iv), first)
    } else {
        let start = src_len
            .checked_sub(BLOCK.saturating_mul(2))
            .ok_or(BackupError::Internal("probe offset"))?;
        seek(src, start)?;
        let mut prev = Block::default();
        let mut last = Block::default();
        read_full(src, prev.as_mut_slice(), file)?;
        read_full(src, last.as_mut_slice(), file)?;
        (prev, last)
    };
    Decryptor::new(&(*key).into(), &iv).decrypt_block(&mut last);
    let padding_ok = Pkcs7::unpad(&last).is_ok();
    last.as_mut_slice().zeroize();

    let mut out = Zeroizing::new([0u8; 16]);
    out.copy_from_slice(first_plain.as_slice());
    first_plain.as_mut_slice().zeroize();
    Ok(Probe {
        first_block: out,
        padding_ok,
    })
}

fn seek<R: Seek>(src: &mut R, pos: u64) -> Result<(), BackupError> {
    src.seek(SeekFrom::Start(pos))
        .map(|_| ())
        .map_err(|e| BackupError::source_io(IoOp::Seek, &e))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::arithmetic_side_effects)]
    use super::*;
    use crate::cancel::{CancelToken, Progress};
    use crate::error::IoSide;
    use aes::cipher::BlockModeEncrypt;
    use std::io::Cursor;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
            .collect()
    }

    fn encrypt(key: &[u8; 32], iv: &[u8; 16], pt: &[u8]) -> Vec<u8> {
        let mut buf = pt.to_vec();
        buf.resize(pt.len() + 16, 0);
        cbc::Encryptor::<Aes256>::new(&(*key).into(), &(*iv).into())
            .encrypt_padded::<Pkcs7>(&mut buf, pt.len())
            .expect("pad")
            .to_vec()
    }

    fn decrypt(key: &[u8; 32], iv: &[u8; 16], ct: &[u8]) -> Result<Vec<u8>, BackupError> {
        let token = CancelToken::new();
        let mut sink = |_: Progress| {};
        let mut ctx = JobCtx::new(&token, &mut sink);
        let mut src = Cursor::new(ct);
        let mut out = Vec::new();
        let n = decrypt_cbc_pkcs7_stream(
            key,
            iv,
            CbcInput {
                src: &mut src,
                src_len: ct.len() as u64,
                file: BackupFile::Blob,
            },
            &mut out,
            &mut ctx,
            Phase::Extracting,
        )?;
        assert_eq!(n, out.len() as u64);
        Ok(out)
    }

    // NIST SP 800-38A Appendix F.2.5 (encrypt) and F.2.6 (decrypt),
    // CBC-AES256, transcribed from the publication. Our function requires a
    // PKCS#7 final block, so one full padding block is appended; its
    // ciphertext was computed with an independent implementation.
    #[test]
    fn nist_sp800_38a_f25_f26() {
        let key: [u8; 32] = hex("603deb1015ca71be2b73aef0857d77811f352c073b6108d72d9810a30914dff4")
            .try_into()
            .expect("32");
        let iv: [u8; 16] = hex("000102030405060708090a0b0c0d0e0f")
            .try_into()
            .expect("16");
        let pt = hex(concat!(
            "6bc1bee22e409f96e93d7e117393172a",
            "ae2d8a571e03ac9c9eb76fac45af8e51",
            "30c81c46a35ce411e5fbc1191a0a52ef",
            "f69f2445df4f9b17ad2b417be66c3710"
        ));
        let ct = hex(concat!(
            "f58c4c04d6e5f1ba779eabfb5f7bfbd6",
            "9cfc4e967edb808d679f777bc6702c7d",
            "39f23369a9d9bacfa530e26304231461",
            "b2eb05e2c39be9fcda6c19078c6a9d1b"
        ));
        let mut full = ct.clone();
        full.extend(hex("3f461796d6b0d6b2e0c2a72b4d80e644"));
        // F.2.5: our encryptor (test-only) reproduces the NIST ciphertext.
        assert_eq!(encrypt(&key, &iv, &pt), full);
        // F.2.6: decryption recovers the NIST plaintext.
        assert_eq!(decrypt(&key, &iv, &full).expect("decrypt"), pt);
    }

    #[test]
    fn chunk_boundaries() {
        let key = [7u8; 32];
        let iv = [0u8; 16];
        for len in [0usize, 1, 15, 16, 17, CHUNK - 1, CHUNK, CHUNK + 1] {
            let pt: Vec<u8> = (0..len).map(|i| (i * 31 % 251) as u8).collect();
            let ct = encrypt(&key, &iv, &pt);
            assert_eq!(decrypt(&key, &iv, &ct).expect("decrypt"), pt, "len {len}");
        }
    }

    /// Encrypt raw blocks without padding, to build bad padding.
    fn encrypt_raw(key: &[u8; 32], blocks: &[u8]) -> Vec<u8> {
        let mut buf = blocks.to_vec();
        let (b, _) = Array::slice_as_chunks_mut(&mut buf);
        cbc::Encryptor::<Aes256>::new(&(*key).into(), &[0u8; 16].into()).encrypt_blocks(b);
        buf
    }

    #[test]
    fn bad_padding_and_lengths() {
        let key = [9u8; 32];
        let mut n0 = vec![0x41u8; 16];
        n0[15] = 0;
        let mut n17 = vec![0x41u8; 32];
        n17[31] = 17;
        let mut mixed = vec![0x41u8; 16];
        mixed[12..].copy_from_slice(&[4, 3, 4, 4]);
        for plain in [n0, n17, mixed] {
            let ct = encrypt_raw(&key, &plain);
            assert!(matches!(
                decrypt(&key, &[0; 16], &ct),
                Err(BackupError::Malformed {
                    detail: MalformedDetail::Padding,
                    ..
                })
            ));
        }
        assert!(matches!(
            decrypt(&key, &[0; 16], &[0u8; 17]),
            Err(BackupError::Malformed {
                detail: MalformedDetail::NotBlockAligned,
                ..
            })
        ));
        assert!(matches!(
            decrypt(&key, &[0; 16], &[]),
            Err(BackupError::Malformed {
                detail: MalformedDetail::Empty,
                ..
            })
        ));
    }

    #[test]
    fn probe_one_and_many_blocks() {
        let key = [3u8; 32];
        let header = b"SQLite format 3\0";
        for len in [0usize, 5, 16, 100, 4096] {
            let mut pt = header.to_vec();
            pt.resize(16 + len, 0x33);
            let ct = encrypt(&key, &[0; 16], &pt);
            let p = probe_first_and_last_block(&key, &mut Cursor::new(&ct), ct.len() as u64)
                .expect("probe");
            assert_eq!(&*p.first_block, header);
            assert!(p.padding_ok, "len {len}");
        }
        // A single block: header-less short plaintext.
        let ct = encrypt(&key, &[0; 16], b"abc");
        let p = probe_first_and_last_block(&key, &mut Cursor::new(&ct), 16).expect("probe");
        assert!(p.padding_ok);
        assert_ne!(&*p.first_block, header);

        // Bad padding: flip the last ciphertext byte.
        let mut pt = header.to_vec();
        pt.resize(200, 1);
        let mut ct = encrypt(&key, &[0; 16], &pt);
        let last = ct.len() - 1;
        ct[last] ^= 0xFF;
        let p = probe_first_and_last_block(&key, &mut Cursor::new(&ct), ct.len() as u64)
            .expect("probe");
        assert_eq!(&*p.first_block, header);
        assert!(!p.padding_ok);

        // Not SQLite.
        let ct = encrypt(&key, &[0; 16], &[0x55; 64]);
        let p = probe_first_and_last_block(&key, &mut Cursor::new(&ct), ct.len() as u64)
            .expect("probe");
        assert_ne!(&*p.first_block, header);
        assert!(matches!(
            probe_first_and_last_block(&key, &mut Cursor::new(&ct), 0),
            Err(BackupError::Malformed {
                detail: MalformedDetail::Empty,
                ..
            })
        ));
    }

    struct FullDisk;
    impl Write for FullDisk {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::StorageFull, "full"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn staging_write_failures_are_staging_io() {
        let key = [1u8; 32];
        let ct = encrypt(&key, &[0; 16], &[1u8; 100]);
        let token = CancelToken::new();
        let mut sink = |_: Progress| {};
        let mut ctx = JobCtx::new(&token, &mut sink);
        let err = decrypt_cbc_pkcs7_stream(
            &key,
            &[0; 16],
            CbcInput {
                src: &mut Cursor::new(&ct),
                src_len: ct.len() as u64,
                file: BackupFile::Blob,
            },
            &mut FullDisk,
            &mut ctx,
            Phase::Extracting,
        )
        .expect_err("full");
        assert!(matches!(
            err,
            BackupError::Io {
                side: IoSide::Staging,
                kind: io::ErrorKind::StorageFull,
                ..
            }
        ));
    }

    #[test]
    fn a_short_source_is_truncated() {
        let key = [1u8; 32];
        let ct = encrypt(&key, &[0; 16], &[1u8; 100]);
        let token = CancelToken::new();
        let mut sink = |_: Progress| {};
        let mut ctx = JobCtx::new(&token, &mut sink);
        let err = decrypt_cbc_pkcs7_stream(
            &key,
            &[0; 16],
            CbcInput {
                src: &mut Cursor::new(&ct),
                src_len: ct.len() as u64 + 16,
                file: BackupFile::ManifestDb,
            },
            &mut Vec::new(),
            &mut ctx,
            Phase::DecryptingManifest,
        )
        .expect_err("short");
        assert!(matches!(
            err,
            BackupError::Malformed {
                file: BackupFile::ManifestDb,
                detail: MalformedDetail::Truncated
            }
        ));
    }
}
