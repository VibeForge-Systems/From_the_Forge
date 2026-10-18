//! A test-only binary property-list writer, written from the format's
//! public description ("bplist00", an object table, an offset table and a
//! 32-byte trailer). Objects may be referenced many times, so it can build
//! the shared-reference "bombs" a tree writer cannot.
#![allow(dead_code)]

/// One object; arrays and dictionaries refer to others by index.
#[derive(Debug, Clone)]
pub enum Obj {
    Int(i64),
    Real(f64),
    /// Seconds since 2001-01-01T00:00:00Z.
    Date(f64),
    Bool(bool),
    Data(Vec<u8>),
    Ascii(String),
    Array(Vec<usize>),
    Dict(Vec<(usize, usize)>),
}

#[derive(Default)]
pub struct Bp {
    objs: Vec<Obj>,
}

impl Bp {
    pub fn new() -> Self {
        Bp::default()
    }

    pub fn add(&mut self, o: Obj) -> usize {
        self.objs.push(o);
        self.objs.len() - 1
    }

    pub fn str(&mut self, s: &str) -> usize {
        self.add(Obj::Ascii(s.to_owned()))
    }

    pub fn int(&mut self, v: i64) -> usize {
        self.add(Obj::Int(v))
    }

    /// A dictionary from (key, value-ref) pairs.
    pub fn dict(&mut self, entries: &[(&str, usize)]) -> usize {
        let pairs = entries.iter().map(|(k, v)| (self.str(k), *v)).collect();
        self.add(Obj::Dict(pairs))
    }

    /// Serialise with `root` as the top object.
    pub fn finish(&self, root: usize) -> Vec<u8> {
        let n = self.objs.len();
        let ref_size: usize = if n <= 0xff {
            1
        } else if n <= 0xffff {
            2
        } else {
            4
        };
        let mut out = b"bplist00".to_vec();
        let mut offsets = Vec::new();
        for o in &self.objs {
            offsets.push(out.len());
            write_obj(&mut out, o, ref_size);
        }
        let table = out.len();
        let max = *offsets.iter().max().unwrap_or(&0);
        let off_size: usize = if max <= 0xff {
            1
        } else if max <= 0xffff {
            2
        } else if max <= 0xffff_ffff {
            4
        } else {
            8
        };
        for off in offsets {
            put_be(&mut out, off as u64, off_size);
        }
        out.extend_from_slice(&[0; 6]);
        out.push(off_size as u8);
        out.push(ref_size as u8);
        out.extend_from_slice(&(n as u64).to_be_bytes());
        out.extend_from_slice(&(root as u64).to_be_bytes());
        out.extend_from_slice(&(table as u64).to_be_bytes());
        out
    }
}

fn put_be(out: &mut Vec<u8>, v: u64, size: usize) {
    out.extend_from_slice(&v.to_be_bytes()[8 - size..]);
}

/// A marker with a count: inline below 15, else 0xF and an int object.
fn marker(out: &mut Vec<u8>, high: u8, count: usize) {
    if count < 15 {
        out.push(high | count as u8);
    } else {
        out.push(high | 0x0f);
        write_int(out, count as i64);
    }
}

fn write_int(out: &mut Vec<u8>, v: i64) {
    if (0..=0xff).contains(&v) {
        out.push(0x10);
        out.push(v as u8);
    } else if (0..=0xffff).contains(&v) {
        out.push(0x11);
        out.extend_from_slice(&(v as u16).to_be_bytes());
    } else if (0..=0xffff_ffff).contains(&v) {
        out.push(0x12);
        out.extend_from_slice(&(v as u32).to_be_bytes());
    } else {
        out.push(0x13);
        out.extend_from_slice(&v.to_be_bytes());
    }
}

fn write_obj(out: &mut Vec<u8>, o: &Obj, ref_size: usize) {
    match o {
        Obj::Int(v) => write_int(out, *v),
        Obj::Real(r) => {
            out.push(0x23);
            out.extend_from_slice(&r.to_be_bytes());
        }
        Obj::Date(r) => {
            out.push(0x33);
            out.extend_from_slice(&r.to_be_bytes());
        }
        Obj::Bool(b) => out.push(if *b { 0x09 } else { 0x08 }),
        Obj::Data(d) => {
            marker(out, 0x40, d.len());
            out.extend_from_slice(d);
        }
        Obj::Ascii(s) => {
            assert!(s.is_ascii());
            marker(out, 0x50, s.len());
            out.extend_from_slice(s.as_bytes());
        }
        Obj::Array(items) => {
            marker(out, 0xa0, items.len());
            for r in items {
                put_be(out, *r as u64, ref_size);
            }
        }
        Obj::Dict(pairs) => {
            marker(out, 0xd0, pairs.len());
            for (k, _) in pairs {
                put_be(out, *k as u64, ref_size);
            }
            for (_, v) in pairs {
                put_be(out, *v as u64, ref_size);
            }
        }
    }
}
