//! A budgeted property-list reader (D17).
//!
//! `plist::Value::from_reader` is never called on backup data. plist 1.10.1's
//! binary reader rejects only references that are already on the stack, not a
//! DAG in which one object is referenced many times, and building a `Value`
//! deep-copies every reference: a few hundred bytes can expand without bound.
//!
//! This reader iterates `plist::stream::Reader` events (XML or binary,
//! detected from the content) and enforces three budgets while streaming:
//!
//! - events: at most `4 × len + 1024`;
//! - cumulative string and data bytes: at most `2 × len`;
//! - nesting depth: at most `ReadOpts::max_depth`.
//!
//! Subtrees under skipped top-level keys are consumed and counted against the
//! budgets, but never allocated.

use std::fmt;
use std::io::{self, Cursor, Read};
use std::path::Path;

use plist::stream::{Event, Reader};

use crate::guard::{open_source_file, OpenFailure};
use crate::limits::LimitKind;

/// A parsed plist value. A crate-local tree, so that nothing is allocated
/// beyond what the budgets allow.
#[derive(Clone, PartialEq)]
pub enum PValue {
    /// A dictionary, in file order. Duplicate keys are kept; lookups take the
    /// last one.
    Dict(Vec<(String, PValue)>),
    /// An array.
    Array(Vec<PValue>),
    /// A boolean.
    Bool(bool),
    /// Raw data.
    Data(Vec<u8>),
    /// A date, as parsed (so `to_xml_format` is safe on it).
    Date(plist::Date),
    /// An integer.
    Integer(plist::Integer),
    /// A real number.
    Real(f64),
    /// A string.
    String(String),
    /// An NSKeyedArchiver UID (binary form only).
    Uid(u64),
}

impl PValue {
    /// The value under `key` if this is a dictionary (the last one when the
    /// key is repeated).
    pub fn get(&self, key: &str) -> Option<&PValue> {
        match self {
            PValue::Dict(entries) => entries.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The entries, if this is a dictionary.
    pub fn as_dict(&self) -> Option<&[(String, PValue)]> {
        match self {
            PValue::Dict(entries) => Some(entries),
            _ => None,
        }
    }

    /// A short type name for notes and errors.
    pub fn type_name(&self) -> &'static str {
        match self {
            PValue::Dict(_) => "dictionary",
            PValue::Array(_) => "array",
            PValue::Bool(_) => "boolean",
            PValue::Data(_) => "data",
            PValue::Date(_) => "date",
            PValue::Integer(_) => "integer",
            PValue::Real(_) => "real",
            PValue::String(_) => "string",
            PValue::Uid(_) => "uid",
        }
    }
}

/// Prints the shape only, never content: a `PValue` may hold device names or
/// key material.
impl fmt::Debug for PValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PValue::Dict(e) => write!(f, "Dict(<{} entries>)", e.len()),
            PValue::Array(a) => write!(f, "Array(<{} items>)", a.len()),
            PValue::Data(d) => write!(f, "Data(<{} bytes>)", d.len()),
            PValue::String(s) => write!(f, "String(<{} bytes>)", s.len()),
            other => write!(f, "{}", other.type_name()),
        }
    }
}

/// Options for one read.
#[derive(Debug, Clone, Copy)]
pub struct ReadOpts<'a> {
    /// Keys of the root dictionary whose values are consumed but not kept.
    pub skip: &'a [&'a str],
    /// Maximum nesting depth.
    pub max_depth: u32,
    /// Optional cap on the length of every array, and the limit it reports.
    pub array_cap: Option<(u32, LimitKind)>,
}

impl<'a> ReadOpts<'a> {
    /// No skipped keys, no array cap.
    pub fn new(max_depth: u32) -> Self {
        ReadOpts {
            skip: &[],
            max_depth,
            array_cap: None,
        }
    }
}

/// Why a plist could not be read.
#[derive(Debug)]
pub enum PlistReadError {
    /// No file at that path.
    Missing,
    /// The path is a link, directory, FIFO or device.
    NotRegular,
    /// The file is larger than its cap.
    TooLarge {
        /// Bytes found (at least `cap + 1`).
        len: u64,
    },
    /// The file could not be read.
    Io(io::Error),
    /// Not a valid property list.
    Syntax,
    /// A budget was exceeded: `PlistEvents`, `PlistBytes`, `PlistDepth`, or
    /// the array cap's kind.
    Limit {
        /// Which budget.
        what: LimitKind,
        /// The count reached.
        value: u64,
        /// The budget.
        cap: u64,
    },
}

/// Read and parse the plist at `path`, which must be at most `cap_bytes`.
/// Returns the value and the number of bytes read.
pub fn read_bounded(
    path: &Path,
    cap_bytes: u64,
    opts: ReadOpts<'_>,
) -> Result<(PValue, u64), PlistReadError> {
    let (file, len) = open_source_file(path).map_err(|e| match e {
        OpenFailure::Missing => PlistReadError::Missing,
        OpenFailure::NotRegular => PlistReadError::NotRegular,
        OpenFailure::Io(e) => PlistReadError::Io(e),
    })?;
    if len > cap_bytes {
        return Err(PlistReadError::TooLarge { len });
    }
    let mut bytes = Vec::new();
    // Read at most cap + 1 bytes, so a file that grows during the read is
    // still caught.
    file.take(cap_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(PlistReadError::Io)?;
    let read = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if read > cap_bytes {
        return Err(PlistReadError::TooLarge { len: read });
    }
    let value = parse_bounded(&bytes, opts)?;
    Ok((value, read))
}

/// Parse an in-memory plist under the same budgets.
pub fn parse_bounded(bytes: &[u8], opts: ReadOpts<'_>) -> Result<PValue, PlistReadError> {
    parse_with_stats(bytes, opts).map(|(v, _)| v)
}

/// Counters from one parse; `nodes` is the number of `PValue`s allocated.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ParseStats {
    pub events: u64,
    pub bytes: u64,
    pub nodes: u64,
}

enum Frame {
    Dict {
        entries: Vec<(String, PValue)>,
        pending_key: Option<String>,
    },
    Array(Vec<PValue>),
}

pub(crate) fn parse_with_stats(
    bytes: &[u8],
    opts: ReadOpts<'_>,
) -> Result<(PValue, ParseStats), PlistReadError> {
    let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    let event_cap = len.saturating_mul(4).saturating_add(1024);
    let byte_cap = len.saturating_mul(2);
    let max_depth = usize::try_from(opts.max_depth).unwrap_or(usize::MAX);

    let mut stats = ParseStats::default();
    let mut stack: Vec<Frame> = Vec::new();
    let mut root: Option<PValue> = None;
    // Depth of a subtree being skipped (0 = not skipping).
    let mut skip_depth: usize = 0;
    // The next value belongs to a skipped key.
    let mut skip_next = false;

    for event in Reader::new(Cursor::new(bytes)) {
        stats.events = stats.events.saturating_add(1);
        if stats.events > event_cap {
            return Err(limit(LimitKind::PlistEvents, stats.events, event_cap));
        }
        let event = event.map_err(|_| PlistReadError::Syntax)?;

        let counted = match &event {
            Event::String(s) => s.len(),
            Event::Data(d) => d.len(),
            _ => 0,
        };
        stats.bytes = stats
            .bytes
            .saturating_add(u64::try_from(counted).unwrap_or(u64::MAX));
        if stats.bytes > byte_cap {
            return Err(limit(LimitKind::PlistBytes, stats.bytes, byte_cap));
        }

        if skip_depth > 0 {
            match event {
                Event::StartArray(_) | Event::StartDictionary(_) => {
                    skip_depth = skip_depth.saturating_add(1);
                    let depth = stack.len().saturating_add(skip_depth);
                    if depth > max_depth {
                        return Err(limit(
                            LimitKind::PlistDepth,
                            as_u64(depth),
                            u64::from(opts.max_depth),
                        ));
                    }
                }
                Event::EndCollection => {
                    skip_depth = skip_depth.saturating_sub(1);
                }
                _ => {}
            }
            continue;
        }

        // A dictionary waiting for a key takes only a string.
        let awaiting_key = matches!(
            stack.last(),
            Some(Frame::Dict {
                pending_key: None,
                ..
            })
        );
        if awaiting_key {
            match event {
                Event::String(key) => {
                    if stack.len() == 1 && opts.skip.contains(&key.as_ref()) {
                        skip_next = true;
                    }
                    if let Some(Frame::Dict { pending_key, .. }) = stack.last_mut() {
                        *pending_key = Some(key.into_owned());
                    }
                    continue;
                }
                Event::EndCollection => {}
                _ => return Err(PlistReadError::Syntax),
            }
        }

        match event {
            Event::StartArray(_) | Event::StartDictionary(_) => {
                let depth = stack.len().saturating_add(1);
                if depth > max_depth {
                    return Err(limit(
                        LimitKind::PlistDepth,
                        as_u64(depth),
                        u64::from(opts.max_depth),
                    ));
                }
                if skip_next {
                    skip_next = false;
                    take_pending_key(&mut stack);
                    skip_depth = 1;
                    continue;
                }
                stack.push(match event {
                    Event::StartArray(_) => Frame::Array(Vec::new()),
                    _ => Frame::Dict {
                        entries: Vec::new(),
                        pending_key: None,
                    },
                });
            }
            Event::EndCollection => {
                let frame = stack.pop().ok_or(PlistReadError::Syntax)?;
                let value = match frame {
                    Frame::Array(items) => PValue::Array(items),
                    Frame::Dict {
                        entries,
                        pending_key,
                    } => {
                        if pending_key.is_some() {
                            return Err(PlistReadError::Syntax);
                        }
                        PValue::Dict(entries)
                    }
                };
                attach(&mut stack, &mut root, value, &opts, &mut stats)?;
            }
            scalar => {
                let value = match scalar {
                    Event::Boolean(b) => PValue::Bool(b),
                    Event::Data(d) => PValue::Data(d.into_owned()),
                    Event::Date(d) => PValue::Date(d),
                    Event::Integer(i) => PValue::Integer(i),
                    Event::Real(r) => PValue::Real(r),
                    Event::String(s) => PValue::String(s.into_owned()),
                    Event::Uid(u) => PValue::Uid(u.get()),
                    _ => return Err(PlistReadError::Syntax),
                };
                if skip_next {
                    skip_next = false;
                    take_pending_key(&mut stack);
                    continue;
                }
                attach(&mut stack, &mut root, value, &opts, &mut stats)?;
            }
        }
    }

    if !stack.is_empty() || skip_depth > 0 {
        return Err(PlistReadError::Syntax);
    }
    root.map(|r| (r, stats)).ok_or(PlistReadError::Syntax)
}

fn limit(what: LimitKind, value: u64, cap: u64) -> PlistReadError {
    PlistReadError::Limit { what, value, cap }
}

fn as_u64(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

fn take_pending_key(stack: &mut [Frame]) {
    if let Some(Frame::Dict { pending_key, .. }) = stack.last_mut() {
        *pending_key = None;
    }
}

fn attach(
    stack: &mut [Frame],
    root: &mut Option<PValue>,
    value: PValue,
    opts: &ReadOpts<'_>,
    stats: &mut ParseStats,
) -> Result<(), PlistReadError> {
    stats.nodes = stats.nodes.saturating_add(1);
    match stack.last_mut() {
        None => {
            if root.is_some() {
                return Err(PlistReadError::Syntax);
            }
            *root = Some(value);
        }
        Some(Frame::Array(items)) => {
            if let Some((cap, kind)) = opts.array_cap {
                if items.len() >= usize::try_from(cap).unwrap_or(usize::MAX) {
                    return Err(limit(
                        kind,
                        as_u64(items.len()).saturating_add(1),
                        u64::from(cap),
                    ));
                }
            }
            items.push(value);
        }
        Some(Frame::Dict {
            entries,
            pending_key,
        }) => {
            let key = pending_key.take().ok_or(PlistReadError::Syntax)?;
            entries.push((key, value));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use plist::Value;

    fn binary(v: &Value) -> Vec<u8> {
        let mut out = Vec::new();
        v.to_writer_binary(&mut out).expect("write binary");
        out
    }

    fn xml(v: &Value) -> Vec<u8> {
        let mut out = Vec::new();
        v.to_writer_xml(&mut out).expect("write xml");
        out
    }

    /// A binary plist whose objects are written once and referenced many
    /// times. `plist`'s writer does not share references, so the object table
    /// is written by hand: object 0 is the leaf, object `i` is an array of two
    /// references to object `i - 1`, and the root is the last object.
    fn nested_pair_bomb(levels: u8) -> Vec<u8> {
        let mut out = b"bplist00".to_vec();
        let mut offsets = Vec::new();
        offsets.push(out.len());
        out.push(0x10); // integer, 1 byte
        out.push(7);
        for i in 1..=levels {
            offsets.push(out.len());
            out.push(0xA2); // array of 2
            out.push(i - 1);
            out.push(i - 1);
        }
        let table = out.len();
        for o in &offsets {
            out.push(u8::try_from(*o).expect("small"));
        }
        out.extend_from_slice(&[0; 6]);
        out.push(1); // offset size
        out.push(1); // ref size
        out.extend_from_slice(&(offsets.len() as u64).to_be_bytes());
        out.extend_from_slice(&u64::from(levels).to_be_bytes());
        out.extend_from_slice(&(table as u64).to_be_bytes());
        out
    }

    /// One `size`-byte data object referenced `refs` times from the root array.
    fn shared_data_bomb(size: usize, refs: u16) -> Vec<u8> {
        let mut out = b"bplist00".to_vec();
        let data_off = out.len();
        out.push(0x4F); // data, length follows as an int
        out.push(0x12); // 4-byte int
        out.extend_from_slice(&u32::try_from(size).expect("size").to_be_bytes());
        out.extend(std::iter::repeat_n(0u8, size));
        let arr_off = out.len();
        out.push(0xAF); // array, length follows
        out.push(0x11); // 2-byte int
        out.extend_from_slice(&refs.to_be_bytes());
        out.extend(std::iter::repeat_n(0u8, usize::from(refs))); // refs to object 0
        let table = out.len();
        out.extend_from_slice(&(data_off as u32).to_be_bytes());
        out.extend_from_slice(&(arr_off as u32).to_be_bytes());
        out.extend_from_slice(&[0; 6]);
        out.push(4); // offset size
        out.push(1); // ref size
        out.extend_from_slice(&2u64.to_be_bytes());
        out.extend_from_slice(&1u64.to_be_bytes());
        out.extend_from_slice(&(table as u64).to_be_bytes());
        out
    }

    #[test]
    fn pair_bomb_beyond_depth_cap_hits_depth() {
        let bomb = nested_pair_bomb(60);
        assert!(bomb.len() < 1024);
        let err = parse_bounded(&bomb, ReadOpts::new(32)).expect_err("bomb");
        assert!(matches!(
            err,
            PlistReadError::Limit {
                what: LimitKind::PlistDepth,
                ..
            }
        ));
    }

    #[test]
    fn pair_bomb_within_depth_cap_hits_events() {
        let bomb = nested_pair_bomb(25);
        let started = std::time::Instant::now();
        let err = parse_bounded(&bomb, ReadOpts::new(32)).expect_err("bomb");
        assert!(matches!(
            err,
            PlistReadError::Limit {
                what: LimitKind::PlistEvents,
                ..
            }
        ));
        // 2^25 events would take far longer; the budget stops it at ~1.5k.
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn shared_data_bomb_hits_bytes() {
        let bomb = shared_data_bomb(64 * 1024, 10_000);
        let err = parse_bounded(&bomb, ReadOpts::new(32)).expect_err("bomb");
        assert!(matches!(
            err,
            PlistReadError::Limit {
                what: LimitKind::PlistBytes,
                ..
            }
        ));
    }

    #[test]
    fn xml_and_binary_give_equal_trees() {
        let mut d = plist::Dictionary::new();
        d.insert("IsEncrypted".into(), Value::Boolean(true));
        d.insert("Name".into(), Value::String("Synthetic".into()));
        d.insert("Blob".into(), Value::Data(vec![1, 2, 3]));
        d.insert(
            "List".into(),
            Value::Array(vec![Value::Integer(5.into()), Value::Real(1.5)]),
        );
        let v = Value::Dictionary(d);
        let a = parse_bounded(&binary(&v), ReadOpts::new(32)).expect("binary");
        let b = parse_bounded(&xml(&v), ReadOpts::new(32)).expect("xml");
        assert_eq!(a, b);
        assert_eq!(a.get("IsEncrypted"), Some(&PValue::Bool(true)));
        assert_eq!(a.get("Name"), Some(&PValue::String("Synthetic".into())));
    }

    #[test]
    fn skipped_subtrees_are_not_allocated() {
        let mut big = Vec::new();
        for i in 0..100i64 {
            big.push(Value::Integer(i.into()));
        }
        let mut d = plist::Dictionary::new();
        d.insert("Applications".into(), Value::Array(big));
        d.insert("Keep".into(), Value::Boolean(true));
        let bytes = binary(&Value::Dictionary(d));
        let (full, full_stats) = parse_with_stats(&bytes, ReadOpts::new(32)).expect("full");
        assert!(full.get("Applications").is_some());
        let opts = ReadOpts {
            skip: &["Applications"],
            ..ReadOpts::new(32)
        };
        let (skipped, stats) = parse_with_stats(&bytes, opts).expect("skipped");
        assert!(skipped.get("Applications").is_none());
        assert_eq!(skipped.get("Keep"), Some(&PValue::Bool(true)));
        assert_eq!(stats.nodes, 2, "only the root and Keep are allocated");
        assert!(full_stats.nodes > 100);
        assert_eq!(
            stats.events, full_stats.events,
            "skipped events still count"
        );
    }

    #[test]
    fn array_cap_is_enforced_while_streaming() {
        let v = Value::Array((0..10i64).map(|i| Value::Integer(i.into())).collect());
        let opts = ReadOpts {
            array_cap: Some((5, LimitKind::MbFileObjects)),
            ..ReadOpts::new(32)
        };
        let err = parse_bounded(&binary(&v), opts).expect_err("cap");
        assert!(matches!(
            err,
            PlistReadError::Limit {
                what: LimitKind::MbFileObjects,
                value: 6,
                cap: 5
            }
        ));
    }

    #[test]
    fn garbage_and_truncation_are_syntax_errors() {
        assert!(matches!(
            parse_bounded(b"", ReadOpts::new(32)),
            Err(PlistReadError::Syntax)
        ));
        let bytes = xml(&Value::Dictionary(plist::Dictionary::new()));
        let cut = bytes.get(..bytes.len() / 2).expect("half");
        assert!(parse_bounded(cut, ReadOpts::new(32)).is_err());
        let bin = binary(&Value::Boolean(true));
        for n in 0..bin.len() {
            let _ = parse_bounded(bin.get(..n).expect("prefix"), ReadOpts::new(32));
        }
    }

    #[test]
    fn file_caps_are_checked_before_parsing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let p = tmp.path().join("x.plist");
        std::fs::write(&p, xml(&Value::Boolean(true))).expect("write");
        let (v, n) = read_bounded(&p, 1 << 20, ReadOpts::new(32)).expect("read");
        assert_eq!(v, PValue::Bool(true));
        assert!(n > 0);
        assert!(matches!(
            read_bounded(&p, 10, ReadOpts::new(32)),
            Err(PlistReadError::TooLarge { .. })
        ));
        assert!(matches!(
            read_bounded(&tmp.path().join("none"), 10, ReadOpts::new(32)),
            Err(PlistReadError::Missing)
        ));
    }

    #[test]
    fn debug_prints_shape_only() {
        let v = PValue::String("Synthetic Test Phone".into());
        assert!(!format!("{v:?}").contains("Synthetic"));
        let d = PValue::Data(vec![0xAB; 4]);
        assert!(!format!("{d:?}").contains("171"));
    }
}
