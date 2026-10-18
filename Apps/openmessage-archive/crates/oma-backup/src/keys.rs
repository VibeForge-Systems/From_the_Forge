//! Passwords, key derivation and key unwrapping (§6.3).
//!
//! Written from RFC 3394, RFC 8018 and the algorithm prose in
//! `docs/supported-formats.md`.

use std::fmt;

use aes_kw::KeyInit;
use aes_kw::KwAes256;
use zeroize::Zeroizing;

use crate::cancel::{JobCtx, Phase};
use crate::error::{BackupError, FileUnavailable, MalformedDetail, UnsupportedReason};
use crate::kdf::{pbkdf2_hmac_sha1, pbkdf2_hmac_sha256, KdfProgress};
use crate::keybag::{ClassStatus, Keybag};
use crate::limits::{LimitKind, Limits};
use crate::plists::BackupFile;

/// A backup password. No `Clone`, no `Serialize`; zeroized on drop; `Debug`
/// prints `Password(<redacted>)`.
pub struct Password(Zeroizing<String>);

impl Password {
    /// Wrap a password. Longer than `limits.password_bytes` (UTF-8) is
    /// refused with `LimitExceeded { PasswordBytes }`; the error carries the
    /// cap, never the value or its length.
    pub fn new(s: Zeroizing<String>, limits: &Limits) -> Result<Self, BackupError> {
        if s.len() > limits.password_bytes {
            let cap = u64::try_from(limits.password_bytes).unwrap_or(u64::MAX);
            return Err(BackupError::limit(
                LimitKind::PasswordBytes,
                cap.saturating_add(1),
                cap,
            ));
        }
        Ok(Password(s))
    }

    /// `true` for the empty password.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The UTF-8 bytes exactly as entered: no trimming, no normalisation.
    fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

impl fmt::Debug for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Password(<redacted>)")
    }
}

/// Unwrapped class keys, zeroized on drop.
pub(crate) struct ClassKeys {
    keys: Vec<(u32, Zeroizing<[u8; 32]>)>,
}

impl fmt::Debug for ClassKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ClassKeys(<{} keys>)", self.keys.len())
    }
}

impl ClassKeys {
    fn get(&self, clas: u32) -> Option<&[u8; 32]> {
        self.keys
            .iter()
            .find(|(c, _)| *c == clas)
            .map(|(_, k)| &**k)
    }

    /// `true` when the class key is available.
    pub(crate) fn has(&self, clas: u32) -> bool {
        self.get(clas).is_some()
    }

    /// The key for a class, or the reason it is unavailable.
    pub(crate) fn class_key(&self, kb: &Keybag, clas: u32) -> Result<&[u8; 32], BackupError> {
        if let Some(k) = self.get(clas) {
            return Ok(k);
        }
        match kb.class(clas).map(|c| c.status) {
            Some(ClassStatus::KeyType(k)) => {
                Err(BackupError::Unsupported(UnsupportedReason::KeyType(k)))
            }
            Some(ClassStatus::BadWrappedKeyLength) => Err(BackupError::malformed(
                BackupFile::Keybag,
                MalformedDetail::BadLength { tag: *b"WPKY" },
            )),
            _ => Err(BackupError::FileUnavailable(
                FileUnavailable::ClassKeyUnavailable,
            )),
        }
    }
}

/// Derive the passphrase key and unwrap every usable class key.
///
/// `password` is moved in and dropped (zeroized) right after the first stage
/// reads it. The intermediate keys are zeroized before this returns.
pub(crate) fn unlock(
    kb: &Keybag,
    password: Password,
    ctx: &mut JobCtx<'_>,
) -> Result<ClassKeys, BackupError> {
    // Without a usable class no unwrap would run, so any password would be
    // accepted. `Keybag::parse` already refuses this; check again here so the
    // derivation never runs for nothing.
    if !kb.classes.iter().any(|c| c.status == ClassStatus::Usable) {
        return Err(BackupError::Unsupported(
            UnsupportedReason::NoPassphraseClasses,
        ));
    }
    let dpic = kb.kdf.dpic;
    let iter = kb.kdf.iter;
    let total = u64::from(iter)
        .checked_mul(2)
        .and_then(|i| i.checked_add(u64::from(dpic)))
        .ok_or(BackupError::Internal("iteration total"))?;
    ctx.report(Phase::DerivingKey, 0, Some(total));

    let mut k1 = Zeroizing::new([0u8; 32]);
    pbkdf2_hmac_sha256(
        password.as_bytes(),
        &kb.kdf.dpsl,
        dpic,
        &mut k1[..],
        ctx,
        KdfProgress { base: 0, total },
    )?;
    drop(password);

    let mut passphrase_key = Zeroizing::new([0u8; 32]);
    pbkdf2_hmac_sha1(
        &k1[..],
        &kb.kdf.salt,
        iter,
        &mut passphrase_key[..],
        ctx,
        KdfProgress {
            base: u64::from(dpic),
            total,
        },
    )?;
    drop(k1);

    let kw = KwAes256::new_from_slice(&passphrase_key[..])
        .map_err(|_| BackupError::Internal("key-wrap key length"))?;
    let usable: Vec<_> = kb
        .classes
        .iter()
        .filter(|c| c.status == ClassStatus::Usable)
        .collect();
    let count = u64::try_from(usable.len()).unwrap_or(u64::MAX);
    let mut keys = Vec::with_capacity(usable.len());
    for (done, class) in (0u64..).zip(usable) {
        ctx.check()?;
        ctx.report(Phase::UnwrappingKeys, done, Some(count));
        let mut out = Zeroizing::new([0u8; 32]);
        if kw.unwrap_key(&class.wpky, &mut out[..]).is_err() {
            // The integrity check is the only signal of a wrong password.
            return Err(if keys.is_empty() {
                BackupError::WrongPassword
            } else {
                BackupError::KeybagDamaged
            });
        }
        keys.push((class.clas, out));
    }
    ctx.report(Phase::UnwrappingKeys, count, Some(count));
    Ok(ClassKeys { keys })
}

/// Unwrap the `ManifestKey`: a little-endian u32 class, then a 40-byte
/// RFC 3394 wrapped key.
pub(crate) fn unwrap_manifest_key(
    keys: &ClassKeys,
    kb: &Keybag,
    manifest_key: &[u8],
) -> Result<Zeroizing<[u8; 32]>, BackupError> {
    let bad_length = || {
        BackupError::malformed(
            BackupFile::ManifestKey,
            MalformedDetail::BadLength { tag: *b"MKEY" },
        )
    };
    if manifest_key.len() != 44 {
        return Err(bad_length());
    }
    let class_bytes: [u8; 4] = manifest_key
        .get(..4)
        .and_then(|b| b.try_into().ok())
        .ok_or_else(bad_length)?;
    let wrapped = manifest_key.get(4..).ok_or_else(bad_length)?;
    let clas = u32::from_le_bytes(class_bytes);
    let class_key = keys.class_key(kb, clas)?;
    unwrap_with(class_key, wrapped).ok_or_else(|| {
        BackupError::malformed(
            BackupFile::ManifestKey,
            MalformedDetail::Inconsistent("manifest key does not unwrap"),
        )
    })
}

/// Unwrap a file key (the 40 bytes after the EncryptionKey's 4-byte prefix)
/// with the key of `protection_class`.
pub(crate) fn unwrap_file_key(
    keys: &ClassKeys,
    kb: &Keybag,
    protection_class: u32,
    wrapped: &[u8],
) -> Result<Zeroizing<[u8; 32]>, BackupError> {
    if wrapped.len() != 40 {
        return Err(BackupError::malformed(
            BackupFile::Blob,
            MalformedDetail::BadLength { tag: *b"EKEY" },
        ));
    }
    let class_key = keys.class_key(kb, protection_class)?;
    unwrap_with(class_key, wrapped).ok_or_else(|| {
        BackupError::malformed(
            BackupFile::Blob,
            MalformedDetail::Inconsistent("file key does not unwrap"),
        )
    })
}

fn unwrap_with(kek: &[u8; 32], wrapped: &[u8]) -> Option<Zeroizing<[u8; 32]>> {
    let kw = KwAes256::new_from_slice(kek).ok()?;
    let mut out = Zeroizing::new([0u8; 32]);
    kw.unwrap_key(wrapped, &mut out[..]).ok()?;
    Some(out)
}

#[cfg(test)]
pub(crate) mod testkeys {
    //! Builds real keybags (wrapping with `aes-kw`, deriving with the
    //! `pbkdf2` crate) for unit tests.
    use super::*;
    use crate::keybag::build;

    pub const DPIC: u32 = 100;
    pub const ITER: u32 = 3;

    pub fn passphrase_key(password: &str) -> [u8; 32] {
        let k1 = pbkdf2::pbkdf2_hmac_array::<sha2::Sha256, 32>(password.as_bytes(), &[2; 20], DPIC);
        pbkdf2::pbkdf2_hmac_array::<sha1::Sha1, 32>(&k1, &[1; 20], ITER)
    }

    pub fn wrap(kek: &[u8; 32], key: &[u8; 32]) -> Vec<u8> {
        let kw = KwAes256::new_from_slice(kek).expect("kek");
        let mut out = [0u8; 40];
        kw.wrap_key(key, &mut out).expect("wrap").to_vec()
    }

    pub fn class_key(clas: u32) -> [u8; 32] {
        [0x40u8.wrapping_add(clas as u8); 32]
    }

    /// A keybag whose classes 1..=4 are wrapped with `password`; the class
    /// listed in `corrupt` gets a wrapped key that fails its integrity check.
    pub fn keybag(password: &str, corrupt: Option<u32>) -> Vec<u8> {
        let pk = passphrase_key(password);
        let mut kb = build::header(&[1; 20], ITER, &[2; 20], DPIC);
        for c in 1..=4u32 {
            let mut w = wrap(&pk, &class_key(c));
            if corrupt == Some(c) {
                if let Some(b) = w.get_mut(10) {
                    *b ^= 1;
                }
            }
            build::class(&mut kb, c, 2, 0, &w);
        }
        kb
    }
}

#[cfg(test)]
mod tests {
    use super::testkeys::*;
    use super::*;
    use crate::cancel::{CancelToken, Progress};

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
            .collect()
    }

    fn run_unlock(kb: &[u8], password: &str) -> Result<(Keybag, ClassKeys), BackupError> {
        let kb = Keybag::parse(kb, &Limits::default())?;
        let token = CancelToken::new();
        let mut sink = |_: Progress| {};
        let mut ctx = JobCtx::new(&token, &mut sink);
        let pw = Password::new(Zeroizing::new(password.to_owned()), &Limits::default())?;
        let keys = unlock(&kb, pw, &mut ctx)?;
        Ok((kb, keys))
    }

    // RFC 3394 §4.6, transcribed from the RFC text.
    #[test]
    fn rfc3394_256_bit_key_with_256_bit_kek() {
        let kek: [u8; 32] = hex("000102030405060708090A0B0C0D0E0F101112131415161718191A1B1C1D1E1F")
            .try_into()
            .expect("32");
        let wrapped =
            hex("28C9F404C4B810F4CBCCB35CFB87F8263F5786E2D80ED326CBC7F0E71A99F43BFB988B9B7A02DD21");
        let key = unwrap_with(&kek, &wrapped).expect("unwrap");
        assert_eq!(
            key.to_vec(),
            hex("00112233445566778899AABBCCDDEEFF000102030405060708090A0B0C0D0E0F")
        );

        let mut flipped = wrapped.clone();
        flipped[0] ^= 1;
        assert!(unwrap_with(&kek, &flipped).is_none());
        // aes-kw zeroes the output buffer when the integrity check fails.
        let kw = KwAes256::new_from_slice(&kek).expect("kek");
        let mut out = [0xAAu8; 32];
        assert!(kw.unwrap_key(&flipped, &mut out).is_err());
        assert_eq!(out, [0u8; 32]);
    }

    #[test]
    fn the_right_password_unwraps_every_class() {
        let (_, keys) = run_unlock(&keybag("pw", None), "pw").expect("unlock");
        for c in 1..=4 {
            assert!(keys.has(c));
            assert_eq!(keys.get(c), Some(&class_key(c)));
        }
    }

    #[test]
    fn a_wrong_password_and_a_damaged_keybag_are_told_apart() {
        assert!(matches!(
            run_unlock(&keybag("pw", None), "other"),
            Err(BackupError::WrongPassword)
        ));
        assert!(matches!(
            run_unlock(&keybag("pw", Some(3)), "pw"),
            Err(BackupError::KeybagDamaged)
        ));
        // A damaged first class cannot be told from a wrong password.
        assert!(matches!(
            run_unlock(&keybag("pw", Some(1)), "pw"),
            Err(BackupError::WrongPassword)
        ));
    }

    #[test]
    fn unlock_refuses_a_keybag_without_usable_classes() {
        let mut kb = Keybag::parse(&keybag("pw", None), &Limits::default()).expect("parse");
        for c in &mut kb.classes {
            c.status = ClassStatus::KeyType(1);
        }
        let token = CancelToken::new();
        let mut sink = |_: Progress| {};
        let mut ctx = JobCtx::new(&token, &mut sink);
        let pw = Password::new(Zeroizing::new("anything".into()), &Limits::default()).expect("pw");
        assert!(matches!(
            unlock(&kb, pw, &mut ctx),
            Err(BackupError::Unsupported(
                UnsupportedReason::NoPassphraseClasses
            ))
        ));
    }

    #[test]
    fn manifest_key_layout_is_checked() {
        let (kb, keys) = run_unlock(&keybag("pw", None), "pw").expect("unlock");
        let mkey = [0x5Au8; 32];
        let mut mk = 4u32.to_le_bytes().to_vec();
        mk.extend(wrap(&class_key(4), &mkey));
        assert_eq!(*unwrap_manifest_key(&keys, &kb, &mk).expect("mkey"), mkey);

        for bad in [43usize, 45] {
            let mut m = mk.clone();
            m.resize(bad, 0);
            assert!(matches!(
                unwrap_manifest_key(&keys, &kb, &m),
                Err(BackupError::Malformed {
                    file: BackupFile::ManifestKey,
                    detail: MalformedDetail::BadLength { .. }
                })
            ));
        }
        let mut missing = 9u32.to_le_bytes().to_vec();
        missing.extend(wrap(&class_key(4), &mkey));
        assert!(matches!(
            unwrap_manifest_key(&keys, &kb, &missing),
            Err(BackupError::FileUnavailable(
                FileUnavailable::ClassKeyUnavailable
            ))
        ));
        let mut wrong_class = 3u32.to_le_bytes().to_vec();
        wrong_class.extend(wrap(&class_key(4), &mkey));
        assert!(matches!(
            unwrap_manifest_key(&keys, &kb, &wrong_class),
            Err(BackupError::Malformed {
                file: BackupFile::ManifestKey,
                ..
            })
        ));
    }

    #[test]
    fn a_short_wpky_on_the_manifest_class_is_bad_length_and_ktyp1_is_unsupported() {
        let pk = passphrase_key("pw");
        let mut kb = crate::keybag::build::header(&[1; 20], ITER, &[2; 20], DPIC);
        crate::keybag::build::class(&mut kb, 1, 2, 0, &wrap(&pk, &class_key(1)));
        crate::keybag::build::class(&mut kb, 4, 2, 0, &[0; 39]);
        crate::keybag::build::class(&mut kb, 5, 2, 1, &wrap(&pk, &class_key(5)));
        let (kb, keys) = run_unlock(&kb, "pw").expect("unlock");
        let mut mk = 4u32.to_le_bytes().to_vec();
        mk.extend([0u8; 40]);
        assert!(matches!(
            unwrap_manifest_key(&keys, &kb, &mk),
            Err(BackupError::Malformed {
                file: BackupFile::Keybag,
                detail: MalformedDetail::BadLength {
                    tag: [b'W', b'P', b'K', b'Y']
                }
            })
        ));
        let mut mk5 = 5u32.to_le_bytes().to_vec();
        mk5.extend([0u8; 40]);
        assert!(matches!(
            unwrap_manifest_key(&keys, &kb, &mk5),
            Err(BackupError::Unsupported(UnsupportedReason::KeyType(1)))
        ));
    }

    #[test]
    fn file_keys_that_do_not_unwrap_are_malformed_not_a_password_error() {
        let (kb, keys) = run_unlock(&keybag("pw", None), "pw").expect("unlock");
        let fk = [7u8; 32];
        let w = wrap(&class_key(3), &fk);
        assert_eq!(*unwrap_file_key(&keys, &kb, 3, &w).expect("file key"), fk);
        assert!(matches!(
            unwrap_file_key(&keys, &kb, 2, &w),
            Err(BackupError::Malformed {
                file: BackupFile::Blob,
                ..
            })
        ));
    }

    #[test]
    fn long_passwords_are_refused_without_echoing_them() {
        let limits = Limits::default();
        let ok = "x".repeat(1024);
        assert!(Password::new(Zeroizing::new(ok), &limits).is_ok());
        let sentinel = format!("{}SENTINEL", "y".repeat(1020));
        let err = Password::new(Zeroizing::new(sentinel), &limits).expect_err("too long");
        let shown = format!("{err:?} {err}");
        assert!(!shown.contains("SENTINEL") && !shown.contains("yyyy"));
        assert!(!shown.contains("1028"), "the length is not recorded");
        assert!(matches!(
            err,
            BackupError::LimitExceeded {
                what: LimitKind::PasswordBytes,
                cap: 1024,
                ..
            }
        ));
        let pw = Password::new(Zeroizing::new("hunter2".into()), &limits).expect("ok");
        assert_eq!(format!("{pw:?}"), "Password(<redacted>)");
    }
}
