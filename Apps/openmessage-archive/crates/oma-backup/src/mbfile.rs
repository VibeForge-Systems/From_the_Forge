//! MBFile records: a hand-written NSKeyedArchiver walker over [`PValue`]
//! (§7.5).
//!
//! The archive's root object is found through `$top.root` (never by assuming
//! `$objects[1]`). Every UID is bounds-checked, and references are followed
//! at most four levels deep. Both the binary `Uid` form and the XML
//! `{ "CF$UID": n }` form are accepted.
#![deny(clippy::arithmetic_side_effects)]

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use zeroize::Zeroizing;

use crate::bplist::{parse_bounded, PValue, PlistReadError, ReadOpts};
use crate::error::{BackupError, MalformedDetail};
use crate::limits::{LimitKind, Limits};
use crate::plists::BackupFile;

/// References followed from a field before giving up.
const MAX_REFERENCE_HOPS: usize = 4;

/// The decoded fields of one MBFile record. Missing or out-of-range values
/// are `None`; nothing is guessed.
#[derive(Clone, Default)]
pub struct MbFile {
    /// `Size`, when a non-negative integer.
    pub size: Option<u64>,
    /// `Mode`, when it fits a u32.
    pub mode: Option<u32>,
    /// `ProtectionClass`, when it fits a u32.
    pub protection_class: Option<u32>,
    /// The 40 bytes after the EncryptionKey's 4-byte prefix.
    pub(crate) encryption_key: Option<Zeroizing<Vec<u8>>>,
    /// An EncryptionKey is present.
    pub has_encryption_key: bool,
    /// A `Target` (symlink target) is present. Its value is never read.
    pub target_present: bool,
    /// `LastModified`, in seconds since the Unix epoch. Never formatted in M2.
    pub last_modified: Option<i64>,
    /// `Birth`, in seconds since the Unix epoch. Never formatted in M2.
    pub birth: Option<i64>,
}

/// The key prints as `<present>`.
impl fmt::Debug for MbFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MbFile")
            .field("size", &self.size)
            .field("mode", &self.mode)
            .field("protection_class", &self.protection_class)
            .field(
                "encryption_key",
                &self.encryption_key.as_ref().map(|_| "<present>"),
            )
            .field("target_present", &self.target_present)
            .field("last_modified", &self.last_modified.is_some())
            .field("birth", &self.birth.is_some())
            .finish()
    }
}

fn malformed(detail: MalformedDetail) -> BackupError {
    BackupError::malformed(BackupFile::Blob, detail)
}

/// The UID a value refers to: binary `Uid(n)` or XML `{ "CF$UID": n }`.
fn uid_of(v: &PValue) -> Option<u64> {
    match v {
        PValue::Uid(u) => Some(*u),
        PValue::Dict(entries) if entries.len() == 1 => match v.get("CF$UID") {
            Some(PValue::Integer(i)) => i.as_unsigned(),
            _ => None,
        },
        _ => None,
    }
}

/// Follow references from `v`. `Ok(None)` is NSKeyedArchiver's `$null`
/// (UID 0).
fn resolve<'a>(v: &'a PValue, objects: &'a [PValue]) -> Result<Option<&'a PValue>, BackupError> {
    let mut current = v;
    for _ in 0..=MAX_REFERENCE_HOPS {
        let Some(uid) = uid_of(current) else {
            return Ok(Some(current));
        };
        if uid == 0 {
            return Ok(None);
        }
        let index = usize::try_from(uid).map_err(|_| malformed(MalformedDetail::UidOutOfRange))?;
        current = objects
            .get(index)
            .ok_or_else(|| malformed(MalformedDetail::UidOutOfRange))?;
    }
    Err(malformed(MalformedDetail::NestingTooDeep))
}

fn field<'a>(
    root: &'a PValue,
    key: &str,
    objects: &'a [PValue],
) -> Result<Option<&'a PValue>, BackupError> {
    match root.get(key) {
        None => Ok(None),
        Some(v) => resolve(v, objects),
    }
}

fn unsigned(v: Option<&PValue>) -> Option<u64> {
    match v {
        Some(PValue::Integer(i)) => i.as_unsigned(),
        _ => None,
    }
}

fn timestamp(v: Option<&PValue>) -> Option<i64> {
    match v {
        Some(PValue::Integer(i)) => i.as_signed(),
        Some(PValue::Date(d)) => {
            let t: SystemTime = (*d).into();
            match t.duration_since(UNIX_EPOCH) {
                Ok(after) => i64::try_from(after.as_secs()).ok(),
                Err(before) => i64::try_from(before.duration().as_secs())
                    .ok()
                    .and_then(i64::checked_neg),
            }
        }
        _ => None,
    }
}

impl MbFile {
    /// Decode one `file` blob from `Manifest.db`, under `limits.mbfile_bytes`
    /// and the plist budgets, with at most `limits.mbfile_objects` objects.
    pub fn decode(bytes: &[u8], limits: &Limits) -> Result<MbFile, BackupError> {
        let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if len > limits.mbfile_bytes {
            return Err(BackupError::limit(
                LimitKind::MbFileBytes,
                len,
                limits.mbfile_bytes,
            ));
        }
        let opts = ReadOpts {
            skip: &[],
            max_depth: limits.plist_depth,
            array_cap: Some((limits.mbfile_objects, LimitKind::MbFileObjects)),
        };
        let archive = parse_bounded(bytes, opts).map_err(|e| match e {
            PlistReadError::Limit { what, value, cap } => BackupError::limit(what, value, cap),
            _ => malformed(MalformedDetail::PlistSyntax),
        })?;
        let objects = match archive.get("$objects") {
            Some(PValue::Array(items)) => items.as_slice(),
            _ => return Err(malformed(MalformedDetail::UnexpectedType("$objects"))),
        };
        let top = archive
            .get("$top")
            .ok_or_else(|| malformed(MalformedDetail::UnexpectedType("$top")))?;
        let root_ref = top
            .get("root")
            .ok_or_else(|| malformed(MalformedDetail::UnexpectedType("$top.root")))?;
        if uid_of(root_ref).is_none() {
            return Err(malformed(MalformedDetail::UnexpectedType("$top.root")));
        }
        let root = match resolve(root_ref, objects)? {
            Some(r @ PValue::Dict(_)) => r,
            _ => return Err(malformed(MalformedDetail::UnexpectedType("root object"))),
        };

        let mut out = MbFile {
            size: unsigned(field(root, "Size", objects)?),
            mode: unsigned(field(root, "Mode", objects)?).and_then(|m| u32::try_from(m).ok()),
            protection_class: unsigned(field(root, "ProtectionClass", objects)?)
                .and_then(|p| u32::try_from(p).ok()),
            target_present: field(root, "Target", objects)?.is_some(),
            last_modified: timestamp(field(root, "LastModified", objects)?),
            birth: timestamp(field(root, "Birth", objects)?),
            ..MbFile::default()
        };

        // EncryptionKey: a UID to a dictionary with NS.data, a UID to raw
        // data, or inline data. Only the first shape is confirmed by the
        // reference implementation; the other two are UNVERIFIED.
        if let Some(key) = field(root, "EncryptionKey", objects)? {
            let data = match key {
                PValue::Data(d) => d,
                PValue::Dict(_) => match key.get("NS.data") {
                    Some(inner) => match resolve(inner, objects)? {
                        Some(PValue::Data(d)) => d,
                        _ => return Err(malformed(MalformedDetail::UnexpectedType("NS.data"))),
                    },
                    None => return Err(malformed(MalformedDetail::UnexpectedType("NS.data"))),
                },
                _ => return Err(malformed(MalformedDetail::UnexpectedType("EncryptionKey"))),
            };
            if data.len() != 44 {
                return Err(malformed(MalformedDetail::BadLength { tag: *b"EKEY" }));
            }
            let wrapped = data
                .get(4..)
                .ok_or_else(|| malformed(MalformedDetail::BadLength { tag: *b"EKEY" }))?;
            out.encryption_key = Some(Zeroizing::new(wrapped.to_vec()));
            out.has_encryption_key = true;
        }
        Ok(out)
    }

    /// The wrapped file key, when present.
    pub(crate) fn wrapped_key(&self) -> Option<&[u8]> {
        self.encryption_key.as_ref().map(|k| k.as_slice())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::arithmetic_side_effects)]
    use super::*;
    use plist::{Dictionary, Uid, Value};

    enum KeyShape {
        NsData,
        UidData,
        Inline,
        None,
    }

    fn archive(fields: Vec<(&str, Value)>, key: KeyShape, key_len: usize) -> Value {
        let mut objects = vec![Value::String("$null".into())];
        let mut root = Dictionary::new();
        for (k, v) in fields {
            root.insert(k.into(), v);
        }
        objects.push(Value::Boolean(false)); // placeholder for the root, index 1
        let mut key_bytes = 3u32.to_le_bytes().to_vec();
        key_bytes.resize(key_len, 0xEE);
        match key {
            KeyShape::NsData => {
                let mut d = Dictionary::new();
                d.insert("NS.data".into(), Value::Data(key_bytes));
                d.insert("$class".into(), Value::Uid(Uid::new(0)));
                objects.push(Value::Dictionary(d));
                root.insert(
                    "EncryptionKey".into(),
                    Value::Uid(Uid::new(objects.len() as u64 - 1)),
                );
            }
            KeyShape::UidData => {
                objects.push(Value::Data(key_bytes));
                root.insert(
                    "EncryptionKey".into(),
                    Value::Uid(Uid::new(objects.len() as u64 - 1)),
                );
            }
            KeyShape::Inline => {
                root.insert("EncryptionKey".into(), Value::Data(key_bytes));
            }
            KeyShape::None => {}
        }
        objects[1] = Value::Dictionary(root);
        let mut top = Dictionary::new();
        top.insert("root".into(), Value::Uid(Uid::new(1)));
        let mut a = Dictionary::new();
        a.insert("$version".into(), Value::Integer(100_000.into()));
        a.insert("$archiver".into(), Value::String("NSKeyedArchiver".into()));
        a.insert("$top".into(), Value::Dictionary(top));
        a.insert("$objects".into(), Value::Array(objects));
        Value::Dictionary(a)
    }

    fn binary(v: &Value) -> Vec<u8> {
        let mut out = Vec::new();
        v.to_writer_binary(&mut out).expect("binary");
        out
    }

    fn basic() -> Vec<(&'static str, Value)> {
        vec![
            ("Size", Value::Integer(1234.into())),
            ("Mode", Value::Integer(0o100644.into())),
            ("ProtectionClass", Value::Integer(3.into())),
            ("LastModified", Value::Integer(1_767_323_045i64.into())),
            ("Birth", Value::Integer(1_767_323_045i64.into())),
        ]
    }

    #[test]
    fn binary_and_xml_forms_decode_alike() {
        let v = archive(basic(), KeyShape::NsData, 44);
        let b = MbFile::decode(&binary(&v), &Limits::default()).expect("binary");
        assert_eq!(b.size, Some(1234));
        assert_eq!(b.mode, Some(0o100644));
        assert_eq!(b.protection_class, Some(3));
        assert_eq!(b.last_modified, Some(1_767_323_045));
        assert!(b.has_encryption_key);
        assert_eq!(b.wrapped_key().map(<[u8]>::len), Some(40));

        // The XML form writes UIDs as { CF$UID: n } dictionaries.
        let xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>$top</key><dict><key>root</key><dict><key>CF$UID</key><integer>1</integer></dict></dict>
<key>$objects</key><array>
  <string>$null</string>
  <dict>
    <key>Size</key><integer>1234</integer>
    <key>ProtectionClass</key><integer>3</integer>
    <key>EncryptionKey</key><dict><key>CF$UID</key><integer>2</integer></dict>
    <key>LastModified</key><date>2026-01-02T03:04:05Z</date>
  </dict>
  <dict><key>NS.data</key><data>{}</data></dict>
</array></dict></plist>"#,
            "AwAAAO7u7u7u7u7u7u7u7u7u7u7u7u7u7u7u7u7u7u7u7u7u7u7u7u7u7u4="
        );
        let x = MbFile::decode(xml.as_bytes(), &Limits::default()).expect("xml");
        assert_eq!(x.size, Some(1234));
        assert_eq!(x.protection_class, Some(3));
        assert!(x.has_encryption_key);
        assert_eq!(x.last_modified, Some(1_767_323_045));
    }

    #[test]
    fn all_three_key_shapes() {
        for shape in [KeyShape::NsData, KeyShape::UidData, KeyShape::Inline] {
            let v = archive(basic(), shape, 44);
            let m = MbFile::decode(&binary(&v), &Limits::default()).expect("decode");
            assert_eq!(m.wrapped_key().map(|k| k.to_vec()), Some(vec![0xEE; 40]));
        }
        let none = MbFile::decode(
            &binary(&archive(basic(), KeyShape::None, 0)),
            &Limits::default(),
        )
        .expect("decode");
        assert!(!none.has_encryption_key);
    }

    #[test]
    fn a_43_byte_key_is_bad_length() {
        let v = archive(basic(), KeyShape::NsData, 43);
        assert!(matches!(
            MbFile::decode(&binary(&v), &Limits::default()),
            Err(BackupError::Malformed {
                detail: MalformedDetail::BadLength { .. },
                ..
            })
        ));
    }

    #[test]
    fn uids_are_bounds_checked_and_depth_limited() {
        let mut v = archive(basic(), KeyShape::None, 0);
        // Point $top.root outside $objects.
        if let Value::Dictionary(a) = &mut v {
            if let Some(Value::Dictionary(top)) = a.get_mut("$top") {
                top.insert("root".into(), Value::Uid(Uid::new(99)));
            }
        }
        assert!(matches!(
            MbFile::decode(&binary(&v), &Limits::default()),
            Err(BackupError::Malformed {
                detail: MalformedDetail::UidOutOfRange,
                ..
            })
        ));
        // A self-referencing UID.
        let mut v = archive(basic(), KeyShape::None, 0);
        if let Value::Dictionary(a) = &mut v {
            if let Some(Value::Array(objs)) = a.get_mut("$objects") {
                objs.push(Value::Uid(Uid::new(2)));
            }
            if let Some(Value::Dictionary(top)) = a.get_mut("$top") {
                top.insert("root".into(), Value::Uid(Uid::new(2)));
            }
        }
        assert!(matches!(
            MbFile::decode(&binary(&v), &Limits::default()),
            Err(BackupError::Malformed {
                detail: MalformedDetail::NestingTooDeep,
                ..
            })
        ));
    }

    #[test]
    fn missing_fields_are_none_and_odd_values_are_not_guessed() {
        let v = archive(
            vec![
                ("Mode", Value::Integer(0o100644.into())),
                (
                    "Birth",
                    Value::Date(std::time::SystemTime::UNIX_EPOCH.into()),
                ),
            ],
            KeyShape::None,
            0,
        );
        let m = MbFile::decode(&binary(&v), &Limits::default()).expect("decode");
        assert_eq!(m.size, None);
        assert_eq!(m.protection_class, None);
        assert_eq!(m.birth, Some(0));

        let v = archive(
            vec![
                ("Size", Value::Integer(u64::MAX.into())),
                ("ProtectionClass", Value::Integer((-1i64).into())),
                ("Mode", Value::Integer((u64::from(u32::MAX) + 1).into())),
            ],
            KeyShape::None,
            0,
        );
        let m = MbFile::decode(&binary(&v), &Limits::default()).expect("decode");
        assert_eq!(m.size, Some(u64::MAX));
        assert_eq!(m.protection_class, None);
        assert_eq!(m.mode, None);
    }

    #[test]
    fn object_count_and_size_caps() {
        let mut v = archive(basic(), KeyShape::None, 0);
        if let Value::Dictionary(a) = &mut v {
            if let Some(Value::Array(objs)) = a.get_mut("$objects") {
                while objs.len() < 257 {
                    objs.push(Value::Integer((objs.len() as i64).into()));
                }
            }
        }
        assert!(matches!(
            MbFile::decode(&binary(&v), &Limits::default()),
            Err(BackupError::LimitExceeded {
                what: LimitKind::MbFileObjects,
                ..
            })
        ));
        let big = vec![0u8; 100 * 1024 + 1];
        assert!(matches!(
            MbFile::decode(&big, &Limits::default()),
            Err(BackupError::LimitExceeded {
                what: LimitKind::MbFileBytes,
                ..
            })
        ));
    }

    #[test]
    fn debug_never_prints_key_bytes() {
        let v = archive(basic(), KeyShape::Inline, 44);
        let m = MbFile::decode(&binary(&v), &Limits::default()).expect("decode");
        let d = format!("{m:?}");
        assert!(d.contains("<present>"));
        assert!(!d.contains("238"), "{d}");
    }
}
