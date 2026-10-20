//! A strict parser of the TXT line grammar (M6 spec §5.4, `writer/txt.rs`)
//! for the Rust tests: our own check of the fixed grammar, written from the
//! grammar's statement rather than from the writer. The independent Python
//! oracle is `scripts/oracles/txt_grammar.py`.
#![allow(dead_code)]

/// One parsed item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub n: u64,
    /// `None` for `[Undated]` and `[No usable time]`.
    pub time: Option<String>,
    /// `[Undated]`: in the undated section.
    pub undated: bool,
    pub sender: Option<String>,
    pub tags: Vec<String>,
    /// Label lines (without their indent), each with its content lines
    /// (without theirs).
    pub labels: Vec<(String, Vec<String>)>,
}

impl Item {
    /// The content lines under the first label equal to `label`.
    pub fn block(&self, label: &str) -> Option<&[String]> {
        self.labels
            .iter()
            .find(|(l, _)| l == label)
            .map(|(_, c)| c.as_slice())
    }
}

/// One parsed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    /// `Some(n)` for `Conversation n: …`, `None` for the no-conversation
    /// file.
    pub conversation: Option<u64>,
    pub name_line: String,
    pub participants: Option<u64>,
    /// The conversation's other recorded names, as written.
    pub other_names: Vec<String>,
    pub zone: String,
    pub items: Vec<Item>,
    pub days: Vec<String>,
    pub undated_section: bool,
    pub footer_items: u64,
    pub footer_folded: u64,
    /// Lines after the footer's counts (legend, About).
    pub tail: Vec<String>,
}

const FIRST: &str =
    "OpenMessage Archive export \u{2014} plaintext copy. See the end of this file for details.";

const PLACEHOLDERS: &[&str] = &[
    "one-time code hidden",
    "earlier text not included",
    "Inline emoji image (not shown)",
    "Attachment placeholder not matched to a file",
];

/// Every `⟨` opens one of our markers.
fn markers_ok(line: &str) -> Result<(), String> {
    let mut rest = line;
    while let Some(i) = rest.find('\u{27E8}') {
        let after = &rest[i + '\u{27E8}'.len_utf8()..];
        let end = after
            .find('\u{27E9}')
            .ok_or_else(|| format!("unclosed marker: {line:?}"))?;
        let inner = &after[..end];
        let ok = (inner.starts_with("U+")
            && (4..=6).contains(&(inner.len() - 2))
            && inner[2..]
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_lowercase()))
            || (inner.starts_with('+')
                && inner.ends_with(" combining marks")
                && inner[1..inner.len() - " combining marks".len()]
                    .chars()
                    .all(|c| c.is_ascii_digit()))
            || inner
                .strip_prefix("Attachment ")
                .is_some_and(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit()))
            || PLACEHOLDERS.contains(&inner);
        if !ok {
            return Err(format!("unknown marker {inner:?}"));
        }
        rest = &after[end + '\u{27E9}'.len_utf8()..];
    }
    Ok(())
}

/// Characters that may never appear raw: C0 other than TAB, C1, DEL, the
/// line and paragraph separators. TAB only in recorded text.
fn bytes_ok(line: &str, tab_ok: bool) -> Result<(), String> {
    for c in line.chars() {
        let bad = (c < ' ' && !(c == '\t' && tab_ok))
            || ('\u{7F}'..='\u{9F}').contains(&c)
            || c == '\u{2028}'
            || c == '\u{2029}';
        if bad {
            return Err(format!("raw U+{:04X} in {line:?}", u32::from(c)));
        }
    }
    Ok(())
}

/// A character D121 would drop (`hidden::classify`, itself checked
/// against the shared vectors): never raw on a header line, nor in
/// recorded text with the markers on.
pub fn has_hidden(s: &str) -> bool {
    oma_export::hidden::classify(s)
        .iter()
        .any(|p| p.class.is_hidden())
}

fn time_ok(t: &str) -> bool {
    // YYYY-MM-DD HH:MM[:SS][ AM| PM] (+HH:MM[:SS]|Z)
    let parts: Vec<&str> = t.split(' ').collect();
    let (date, clock, rest) = match parts.as_slice() {
        [d, c, z] => (*d, *c, vec![*z]),
        [d, c, ap, z] if *ap == "AM" || *ap == "PM" => (*d, *c, vec![*z]),
        _ => return false,
    };
    let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    let d: Vec<&str> = date.rsplitn(3, '-').collect();
    let date_ok = d.len() == 3 && d.iter().all(|x| digits(x.trim_start_matches(['+', '-'])));
    let c: Vec<&str> = clock.split(':').collect();
    let clock_ok = (c.len() == 2 || c.len() == 3) && c.iter().all(|x| x.len() == 2 && digits(x));
    let z = rest[0];
    let zone_ok = z == "Z"
        || ((z.starts_with('+') || z.starts_with('-'))
            && z[1..].split(':').all(|x| x.len() == 2 && digits(x)));
    date_ok && clock_ok && zone_ok
}

fn item_line(line: &str) -> Result<Item, String> {
    let rest = line.strip_prefix('[').ok_or("item line")?;
    let close = rest.find(']').ok_or("item time")?;
    let time = &rest[..close];
    let (time, undated) = if time == "Undated" {
        (None, true)
    } else if time == "No usable time" {
        (None, false)
    } else if time_ok(time) {
        (Some(time.to_owned()), false)
    } else {
        return Err(format!("bad time {time:?}"));
    };
    let rest = rest[close + 1..]
        .strip_prefix(" [item ")
        .ok_or_else(|| format!("no item number: {line:?}"))?;
    let close = rest.find(']').ok_or("item number")?;
    let n: u64 = rest[..close].parse().map_err(|_| "item number digits")?;
    let mut rest = &rest[close + 1..];
    let mut sender = None;
    if let Some(r) = rest.strip_prefix(' ') {
        if !r.starts_with('[') {
            let end = r.find(" [").unwrap_or(r.len());
            let s = &r[..end];
            if s.is_empty() || s.contains('[') || s.contains(']') {
                return Err(format!("bad sender: {line:?}"));
            }
            sender = Some(s.to_owned());
            rest = &r[end..];
        }
    }
    let mut tags = Vec::new();
    while !rest.is_empty() {
        let r = rest
            .strip_prefix(" [")
            .ok_or_else(|| format!("bad tag start: {line:?}"))?;
        let close = r.find(']').ok_or("unclosed tag")?;
        let t = &r[..close];
        if t.is_empty() || t.contains('[') {
            return Err(format!("bad tag: {line:?}"));
        }
        tags.push(t.to_owned());
        rest = &r[close + 1..];
    }
    Ok(Item {
        n,
        time,
        undated,
        sender,
        tags,
        labels: Vec::new(),
    })
}

/// Parse one file written with line ending `nl` and markers `markers`.
pub fn parse(text: &str, nl: &str, markers: bool) -> Result<File, String> {
    if !text.ends_with(nl) {
        return Err("no final line end".into());
    }
    if nl == "\n" && text.contains('\r') {
        return Err("raw CR in an LF file".into());
    }
    let body = &text[..text.len() - nl.len()];
    let lines: Vec<&str> = body.split(nl).collect();
    let mut i = 0;
    let mut next = || -> Option<&str> {
        let l = lines.get(i).copied();
        i += 1;
        l
    };
    if next() != Some(FIRST) {
        return Err("first line".into());
    }
    let name_line = next().ok_or("name line")?.to_owned();
    let conversation = if let Some(r) = name_line.strip_prefix("Conversation ") {
        let (n, _) = r.split_once(": ").ok_or("conversation line")?;
        Some(n.parse::<u64>().map_err(|_| "conversation number")?)
    } else if name_line == "No conversation: messages recorded in no conversation" {
        None
    } else {
        return Err(format!("name line {name_line:?}"));
    };
    let mut participants = None;
    let mut zone_line = next().ok_or("zone line")?;
    if conversation.is_some() {
        let p = zone_line
            .strip_prefix("Addresses recorded in this conversation (includes people who left): ")
            .ok_or("participants line")?;
        participants = Some(p.parse().map_err(|_| "participants")?);
        zone_line = next().ok_or("zone line")?;
    }
    let mut other_names = Vec::new();
    while let Some(n) = zone_line.strip_prefix("Other recorded name of this conversation: ") {
        bytes_ok(n, false)?;
        markers_ok(n)?;
        if has_hidden(n) {
            return Err(format!("hidden character in a header: {n:?}"));
        }
        other_names.push(n.to_owned());
        zone_line = next().ok_or("zone line")?;
    }
    let zone = zone_line
        .strip_prefix("Times: ")
        .and_then(|z| z.strip_suffix(" (offsets shown on every line)"))
        .ok_or("zone line")?
        .to_owned();
    for l in [name_line.as_str(), zone_line] {
        bytes_ok(l, false)?;
        markers_ok(l)?;
        if has_hidden(l) {
            return Err(format!("hidden character in a header: {l:?}"));
        }
    }
    let mut f = File {
        conversation,
        name_line,
        participants,
        other_names,
        zone,
        items: Vec::new(),
        days: Vec::new(),
        undated_section: false,
        footer_items: 0,
        footer_folded: 0,
        tail: Vec::new(),
    };
    let mut in_text = false;
    loop {
        let Some(l) = next() else {
            return Err("no footer".into());
        };
        if l.is_empty() {
            in_text = false;
            continue;
        }
        if l == "==== End of conversation file ====" {
            break;
        }
        if l == "==== Undated ====" {
            f.undated_section = true;
            continue;
        }
        if let Some(d) = l
            .strip_prefix("==== ")
            .and_then(|r| r.strip_suffix(" ===="))
        {
            if f.undated_section {
                return Err("a day after the undated section".into());
            }
            f.days.push(d.to_owned());
            continue;
        }
        if let Some(c) = l.strip_prefix("      ") {
            bytes_ok(c, true)?;
            markers_ok(c)?;
            if markers && has_hidden(c) {
                return Err(format!("hidden character with markers on: {c:?}"));
            }
            let item = f.items.last_mut().ok_or("text before an item")?;
            let label = item.labels.last_mut().ok_or("text before a label")?;
            label.1.push(c.to_owned());
            in_text = true;
            continue;
        }
        let _ = in_text;
        if let Some(c) = l.strip_prefix("    ") {
            if c.starts_with(' ') {
                return Err(format!("odd indent: {l:?}"));
            }
            bytes_ok(c, false)?;
            markers_ok(c)?;
            if has_hidden(c) {
                return Err(format!("hidden character on a label line: {c:?}"));
            }
            let item = f.items.last_mut().ok_or("label before an item")?;
            item.labels.push((c.to_owned(), Vec::new()));
            continue;
        }
        bytes_ok(l, false)?;
        markers_ok(l)?;
        if has_hidden(l) {
            return Err(format!("hidden character on an item line: {l:?}"));
        }
        let item = item_line(l)?;
        match (&item.time, item.undated, f.undated_section) {
            (Some(_), _, true) => return Err("a dated item in the undated section".into()),
            (Some(_), _, false) if f.days.is_empty() => {
                return Err("a dated item before any day line".into())
            }
            (None, true, false) => return Err("an undated item outside the undated section".into()),
            (None, false, true) => {
                return Err("an item without a usable time in the undated section".into())
            }
            _ => {}
        }
        f.items.push(item);
    }
    let items = next()
        .and_then(|l| l.strip_prefix("Items: "))
        .ok_or("footer items")?;
    f.footer_items = items.parse().map_err(|_| "footer items")?;
    let folded = next()
        .and_then(|l| l.strip_prefix("Reactions shown on the messages they react to: "))
        .ok_or("footer folded")?;
    f.footer_folded = folded.parse().map_err(|_| "footer folded")?;
    while let Some(l) = next() {
        bytes_ok(l, false)?;
        f.tail.push(l.to_owned());
    }
    if f.footer_items != f.items.len() as u64 {
        return Err("footer count differs from the items".into());
    }
    Ok(f)
}

/// Undo the markers of a recorded line: `⟨U+XXXX⟩` back to the character.
pub fn unmark(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('\u{27E8}') {
        out.push_str(&rest[..i]);
        let after = &rest[i + '\u{27E8}'.len_utf8()..];
        let end = after.find('\u{27E9}').unwrap_or(after.len());
        let inner = &after[..end];
        match inner
            .strip_prefix("U+")
            .and_then(|h| u32::from_str_radix(h, 16).ok())
            .and_then(char::from_u32)
        {
            Some(c) => out.push(c),
            None => {
                out.push('\u{27E8}');
                out.push_str(inner);
                out.push('\u{27E9}');
            }
        }
        rest = after.get(end + '\u{27E9}'.len_utf8()..).unwrap_or("");
    }
    out.push_str(rest);
    out
}
