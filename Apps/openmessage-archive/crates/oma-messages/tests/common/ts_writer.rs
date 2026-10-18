//! A test-only, clean-room typedstream writer (M3 §19.2).
//!
//! It is the Rust twin of the writer in
//! `scripts/fixtures/make_synthetic_typedstream.py`, written from the same
//! format facts (see `scripts/fixtures/PROVENANCE.md`). Tests use it for the
//! round-trip property and for hostile inputs too large to commit. Agreement
//! between this writer and the decoder shows internal consistency only.
#![allow(dead_code)]

use std::collections::HashMap;

pub const NEW: u8 = 0x84;
pub const NIL: u8 = 0x85;
pub const END: u8 = 0x86;
const REF_BIAS: i64 = 110;

pub type Chain = &'static [(&'static str, i64)];
pub const NSSTRING: Chain = &[("NSString", 1), ("NSObject", 0)];
pub const NSMSTRING: Chain = &[("NSMutableString", 1), ("NSString", 1), ("NSObject", 0)];
pub const NSDICT: Chain = &[("NSDictionary", 0), ("NSObject", 0)];
pub const NSNUMBER: Chain = &[("NSNumber", 0), ("NSValue", 0), ("NSObject", 0)];
pub const NSMAS: Chain = &[
    ("NSMutableAttributedString", 0),
    ("NSAttributedString", 0),
    ("NSObject", 0),
];
pub const UNKNOWN: Chain = &[("SYNTHUnknownValue", 0), ("NSObject", 0)];
pub const BULK: Chain = &[("SYNTHBulk", 0), ("NSObject", 0)];

pub const PART: &str = "__kIMMessagePartAttributeName";
pub const GUID: &str = "__kIMFileTransferGUIDAttributeName";
pub const MENTION: &str = "__kIMMentionConfirmedMention";
pub const BOLD: &str = "__kIMTextBoldAttributeName";
pub const EFFECT: &str = "__kIMTextEffectAttributeName";
pub const LINK: &str = "__kIMLinkAttributeName";

/// An attribute value.
#[derive(Debug, Clone, PartialEq)]
pub enum AttrVal {
    Int(i64),
    Str(String),
    Unknown,
}

pub type Attrs = Vec<(&'static str, AttrVal)>;

pub struct W {
    pub b: Vec<u8>,
    strings: HashMap<Vec<u8>, i64>,
    pub objs: i64,
    classes: HashMap<&'static str, i64>,
}

impl Default for W {
    fn default() -> Self {
        Self::new()
    }
}

impl W {
    pub fn new() -> Self {
        let mut w = W {
            b: Vec::new(),
            strings: HashMap::new(),
            objs: 0,
            classes: HashMap::new(),
        };
        w.b.extend_from_slice(&[4, 11]);
        w.b.extend_from_slice(b"streamtyped");
        w.int(1000);
        w
    }

    pub fn int(&mut self, v: i64) {
        if (-110..=127).contains(&v) {
            self.b.push(v as i8 as u8);
        } else if let Ok(x) = i16::try_from(v) {
            self.b.push(0x81);
            self.b.extend_from_slice(&x.to_le_bytes());
        } else if let Ok(x) = i32::try_from(v) {
            self.b.push(0x82);
            self.b.extend_from_slice(&x.to_le_bytes());
        } else {
            self.b.push(0x87);
            self.b.extend_from_slice(&v.to_le_bytes());
        }
    }

    pub fn reference(&mut self, idx: i64) {
        self.int(idx - REF_BIAS);
    }

    pub fn shared(&mut self, s: &[u8]) {
        if let Some(&i) = self.strings.get(s) {
            self.reference(i);
        } else {
            self.strings.insert(s.to_vec(), self.strings.len() as i64);
            self.b.push(NEW);
            self.int(s.len() as i64);
            self.b.extend_from_slice(s);
        }
    }

    pub fn cls(&mut self, chain: &[(&'static str, i64)]) {
        let Some(((name, ver), rest)) = chain.split_first() else {
            self.b.push(NIL);
            return;
        };
        if let Some(&i) = self.classes.get(name) {
            self.reference(i);
            return;
        }
        self.b.push(NEW);
        self.classes.insert(name, self.objs);
        self.objs += 1;
        self.shared(name.as_bytes());
        self.int(*ver);
        self.cls(rest);
    }

    pub fn begin_obj(&mut self, chain: &[(&'static str, i64)]) -> i64 {
        self.b.push(NEW);
        let idx = self.objs;
        self.objs += 1;
        self.cls(chain);
        idx
    }

    pub fn end_obj(&mut self) {
        self.b.push(END);
    }

    pub fn types(&mut self, t: &[u8]) {
        self.shared(t);
    }

    pub fn plus(&mut self, raw: &[u8]) {
        self.types(b"+");
        self.int(raw.len() as i64);
        self.b.extend_from_slice(raw);
    }

    pub fn nsstring_with(&mut self, raw: &[u8], chain: &[(&'static str, i64)]) -> i64 {
        self.types(b"@");
        let idx = self.begin_obj(chain);
        self.plus(raw);
        self.end_obj();
        idx
    }

    pub fn nsstring(&mut self, raw: &[u8]) -> i64 {
        self.nsstring_with(raw, NSSTRING)
    }

    pub fn nsnumber(&mut self, v: i64) {
        self.types(b"@");
        self.begin_obj(NSNUMBER);
        self.types(b"*");
        self.b.push(NEW);
        self.objs += 1;
        self.shared(b"i");
        self.types(b"i");
        self.int(v);
        self.end_obj();
    }

    pub fn unknown(&mut self) {
        self.types(b"@");
        self.begin_obj(UNKNOWN);
        self.types(b"i");
        self.int(0);
        self.end_obj();
    }

    pub fn value(&mut self, v: &AttrVal) {
        match v {
            AttrVal::Int(n) => self.nsnumber(*n),
            AttrVal::Str(s) => {
                self.nsstring(s.as_bytes());
            }
            AttrVal::Unknown => self.unknown(),
        }
    }

    pub fn dict(&mut self, items: &[(&'static str, AttrVal)]) {
        self.types(b"@");
        self.begin_obj(NSDICT);
        self.types(b"i");
        self.int(items.len() as i64);
        for (k, v) in items {
            self.nsstring(k.as_bytes());
            self.value(v);
        }
        self.end_obj();
    }

    /// Begin an attributed string holding `text`; runs follow.
    pub fn begin_attributed(&mut self, text: &[u8]) {
        self.types(b"@");
        self.begin_obj(NSMAS);
        self.nsstring_with(text, NSMSTRING);
    }

    pub fn run(&mut self, idx: i64, len: i64) {
        self.types(b"iI");
        self.int(idx);
        self.int(len);
    }
}

pub fn utf16_len(s: &str) -> u64 {
    s.encode_utf16().count() as u64
}

/// An attributed string from segments; equal attribute lists share one
/// dictionary, numbered in order of first use. Returns the blob and the
/// dictionary number of each segment.
pub fn attributed(segs: &[(String, Attrs)]) -> (Vec<u8>, Vec<usize>) {
    let text: String = segs.iter().map(|(t, _)| t.as_str()).collect();
    let mut w = W::new();
    w.begin_attributed(text.as_bytes());
    let mut seen: Vec<&Attrs> = Vec::new();
    let mut numbers = Vec::new();
    for (t, attrs) in segs {
        let pos = seen.iter().position(|a| *a == attrs);
        let n = pos.unwrap_or(seen.len()) + 1;
        w.run(n as i64, utf16_len(t) as i64);
        if pos.is_none() {
            seen.push(attrs);
            w.dict(attrs);
        }
        numbers.push(n);
    }
    w.end_obj();
    (w.b, numbers)
}
