//! A strict reader of the HTML writer's fixed grammar (M6 spec §5.5,
//! §14.2 "HTML: every hostile-fixture page is parsed with a strict
//! tokenizer in Rust"), written from the spec, not from `html.rs`:
//!
//! - the document is `<!doctype html>`, then elements with double-quoted
//!   attributes only, `meta` and `img` void, `style` raw text;
//! - a text node holds no `<`, `>`, `"` or `'`, and `&` only as `&amp;`,
//!   `&lt;`, `&gt;`, `&quot;` or `&#x27;`;
//! - [`check`] applies the template's rules: the head in its fixed order
//!   with the CSP meta first after the charset, the one stylesheet and its
//!   hash, the allowed elements and attributes (no script, event, style
//!   attribute, base, form, frame, object, embed, link, audio or video),
//!   links and image sources of our grammar, one `h1`, one `h2` per
//!   section, an `h3` labelling every `article`, `nav` after `main`,
//!   backward-only item links, unique ids, the `data:` budget, and markers
//!   as elements (no control or noncharacter in any text node, no hidden
//!   character either when markers are on).
#![allow(dead_code)]

use std::collections::BTreeSet;

use oma_export::hidden::{classify, CharClass};

#[derive(Debug, Clone)]
pub enum Node {
    El(Element),
    Text(String),
}

#[derive(Debug, Clone)]
pub struct Element {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Node>,
}

impl Element {
    pub fn attr(&self, k: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.as_str())
    }

    pub fn class(&self) -> Vec<&str> {
        self.attr("class")
            .map(|c| c.split(' ').collect())
            .unwrap_or_default()
    }

    pub fn has_class(&self, c: &str) -> bool {
        self.class().contains(&c)
    }

    pub fn elements(&self) -> impl Iterator<Item = &Element> {
        self.children.iter().filter_map(|n| match n {
            Node::El(e) => Some(e),
            Node::Text(_) => None,
        })
    }

    /// Every element below this one, in document order (this one first).
    pub fn walk(&self) -> Vec<&Element> {
        let mut out = vec![self];
        for c in self.elements() {
            out.extend(c.walk());
        }
        out
    }

    /// The text below this element as a reader gets it back: entities
    /// decoded, a marker element turned back into its character, a
    /// placeholder as U+FFFC.
    pub fn text(&self) -> String {
        let mut s = String::new();
        for c in &self.children {
            match c {
                Node::Text(t) => s.push_str(t),
                Node::El(e) if e.name == "span" && e.has_class("mark") => {
                    s.push(unmark(&e.text()).unwrap_or('\u{FFFD}'));
                }
                Node::El(e) if e.name == "span" && e.has_class("ph") => s.push('\u{FFFC}'),
                Node::El(e) => s.push_str(&e.text()),
            }
        }
        s
    }

    /// The plain text below this element (markers and placeholders as
    /// their words).
    pub fn plain(&self) -> String {
        let mut s = String::new();
        for c in &self.children {
            match c {
                Node::Text(t) => s.push_str(t),
                Node::El(e) => s.push_str(&e.plain()),
            }
        }
        s
    }
}

/// `U+XXXX` back into its character.
pub fn unmark(s: &str) -> Option<char> {
    let hex = s.strip_prefix("U+")?;
    if hex.len() < 4 || hex.len() > 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    if hex.bytes().any(|b| b.is_ascii_lowercase()) {
        return None;
    }
    char::from_u32(u32::from_str_radix(hex, 16).ok()?)
}

const VOID: &[&str] = &["meta", "img"];

struct P<'a> {
    s: &'a str,
    at: usize,
}

impl P<'_> {
    fn rest(&self) -> &str {
        self.s.get(self.at..).unwrap_or("")
    }

    fn err(&self, what: &str) -> String {
        format!("{what} at byte {}", self.at)
    }

    fn eat(&mut self, lit: &str) -> bool {
        if self.rest().starts_with(lit) {
            self.at += lit.len();
            true
        } else {
            false
        }
    }

    fn name(&mut self) -> Result<String, String> {
        let n: String = self
            .rest()
            .chars()
            .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
            .collect();
        if n.is_empty() {
            return Err(self.err("a name"));
        }
        self.at += n.len();
        Ok(n)
    }
}

/// Entities decoded; anything else after `&`, or a raw `<`, `>`, `"` or
/// `'`, is refused.
fn decode(raw: &str, attr: bool) -> Result<String, String> {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(c) = rest.chars().next() {
        if c == '&' {
            let mut hit = false;
            for (e, v) in [
                ("&amp;", '&'),
                ("&lt;", '<'),
                ("&gt;", '>'),
                ("&quot;", '"'),
                ("&#x27;", '\''),
            ] {
                if let Some(r) = rest.strip_prefix(e) {
                    out.push(v);
                    rest = r;
                    hit = true;
                    break;
                }
            }
            if !hit {
                return Err("a raw & or another entity".to_owned());
            }
            continue;
        }
        if matches!(c, '<' | '>' | '"' | '\'') {
            return Err(format!(
                "a raw {c:?} in {}",
                if attr { "an attribute" } else { "text" }
            ));
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    Ok(out)
}

/// Parse a page of the fixed grammar into its `html` element.
pub fn parse(src: &str) -> Result<Element, String> {
    let mut p = P { s: src, at: 0 };
    if !p.eat("<!doctype html>\n") {
        return Err("no doctype".to_owned());
    }
    let mut stack: Vec<Element> = vec![Element {
        name: "#document".to_owned(),
        attrs: Vec::new(),
        children: Vec::new(),
    }];
    while p.at < src.len() {
        if p.eat("</") {
            let n = p.name()?;
            if !p.eat(">") {
                return Err(p.err("an end tag's >"));
            }
            let el = stack
                .pop()
                .ok_or_else(|| p.err("an end tag without a start"))?;
            if el.name != n || stack.is_empty() {
                return Err(p.err(&format!("</{n}> closes <{}>", el.name)));
            }
            stack
                .last_mut()
                .ok_or_else(|| p.err("stack"))?
                .children
                .push(Node::El(el));
        } else if p.eat("<") {
            let name = p.name()?;
            let mut attrs = Vec::new();
            loop {
                if p.eat(">") {
                    break;
                }
                if !p.eat(" ") {
                    return Err(p.err("a space before an attribute"));
                }
                let k = p.name()?;
                if !p.eat("=\"") {
                    return Err(p.err("a double-quoted value"));
                }
                let end = p
                    .rest()
                    .find('"')
                    .ok_or_else(|| p.err("an unterminated value"))?;
                let raw = &p.rest()[..end];
                let v = decode(raw, true).map_err(|e| p.err(&e))?;
                p.at += end + 1;
                if attrs.iter().any(|(n, _): &(String, String)| *n == k) {
                    return Err(p.err("a repeated attribute"));
                }
                attrs.push((k, v));
            }
            let mut el = Element {
                name: name.clone(),
                attrs,
                children: Vec::new(),
            };
            if VOID.contains(&name.as_str()) {
                stack
                    .last_mut()
                    .ok_or_else(|| p.err("stack"))?
                    .children
                    .push(Node::El(el));
            } else if name == "style" {
                let end = p
                    .rest()
                    .find("</style>")
                    .ok_or_else(|| p.err("an unterminated style"))?;
                el.children.push(Node::Text(p.rest()[..end].to_owned()));
                p.at += end + "</style>".len();
                stack
                    .last_mut()
                    .ok_or_else(|| p.err("stack"))?
                    .children
                    .push(Node::El(el));
            } else {
                stack.push(el);
            }
        } else {
            let end = p.rest().find('<').unwrap_or(p.rest().len());
            let raw = &p.rest()[..end];
            let t = decode(raw, false).map_err(|e| p.err(&e))?;
            p.at += end;
            let top = stack.last_mut().ok_or_else(|| p.err("stack"))?;
            if top.name == "#document" {
                if !t.trim().is_empty() {
                    return Err(p.err("text outside html"));
                }
                continue;
            }
            top.children.push(Node::Text(t));
        }
    }
    let mut doc = stack.pop().ok_or("stack")?;
    if !stack.is_empty() {
        return Err("unclosed elements".to_owned());
    }
    let html = match doc.children.pop() {
        Some(Node::El(e)) if e.name == "html" && doc.children.is_empty() => e,
        _ => return Err("not one html element".to_owned()),
    };
    Ok(html)
}

/// What [`check`] found on a page.
#[derive(Debug, Default)]
pub struct Page {
    pub title: String,
    /// `(n, article)` in document order.
    pub articles: Vec<(u64, Element)>,
    pub data_bytes: u64,
    pub item_links: u64,
    /// Every href, in document order.
    pub hrefs: Vec<String>,
    pub h1: String,
    pub images: u64,
}

/// What the page is expected to follow.
#[derive(Debug, Clone, Copy)]
pub struct Expect<'a> {
    pub csp: &'a str,
    pub style: &'a str,
    pub style_sha256_b64: &'a str,
    pub markers: bool,
    pub data_budget: u64,
    /// `index.html` (no days, one skip link).
    pub index: bool,
}

fn allowed(el: &Element) -> Result<(), String> {
    let n = el.name.as_str();
    let classes = |ok: &[&str]| -> Result<(), String> {
        for c in el.class() {
            if !ok.contains(&c) {
                return Err(format!("<{n} class={c}>"));
            }
        }
        Ok(())
    };
    let attrs_ok = |ok: &[&str]| -> Result<(), String> {
        for (k, _) in &el.attrs {
            if !ok.contains(&k.as_str()) {
                return Err(format!("<{n} {k}=>"));
            }
        }
        Ok(())
    };
    match n {
        "html" => {
            attrs_ok(&["lang"])?;
            if el.attr("lang") != Some("en") {
                return Err("html lang".to_owned());
            }
        }
        "meta" => attrs_ok(&["charset", "http-equiv", "content", "name"])?,
        "head" | "title" | "style" | "body" | "header" | "h1" | "figure" | "figcaption"
        | "details" | "summary" | "li" => attrs_ok(&[])?,
        "a" => {
            attrs_ok(&["href", "class"])?;
            classes(&["skip"])?;
            if el.attr("href").is_none() {
                return Err("a without href".to_owned());
            }
        }
        "main" => attrs_ok(&["id"])?,
        "nav" => attrs_ok(&["id", "aria-label"])?,
        "footer" => attrs_ok(&["id"])?,
        "section" => attrs_ok(&["aria-labelledby"])?,
        "h2" | "h3" => attrs_ok(&["id"])?,
        "article" => {
            attrs_ok(&["id", "aria-labelledby", "class"])?;
            classes(&["me"])?;
        }
        "span" => {
            attrs_ok(&["class"])?;
            classes(&["who", "t", "mark", "ph", "label"])?;
        }
        "bdi" => {
            attrs_ok(&["dir", "class"])?;
            classes(&["b", "i", "u", "s", "mention", "url"])?;
            if el.attr("dir") != Some("auto") {
                return Err("bdi without dir=auto".to_owned());
            }
        }
        "time" => {
            attrs_ok(&["datetime"])?;
            let ok = el.attr("datetime").is_none_or(global_date);
            if !ok {
                return Err("time datetime".to_owned());
            }
        }
        "img" => {
            attrs_ok(&["alt", "width", "height", "src"])?;
            if el.attr("alt") != Some("") {
                return Err("img alt".to_owned());
            }
            for k in ["width", "height"] {
                let ok = el
                    .attr(k)
                    .is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()));
                if !ok {
                    return Err(format!("img {k}"));
                }
            }
        }
        "p" => {
            attrs_ok(&["class"])?;
            classes(&[
                "notice",
                "q",
                "note",
                "sub",
                "body",
                "seg",
                "transcription",
                "card",
                "parts",
                "label",
            ])?;
        }
        "ul" => {
            attrs_ok(&["class"])?;
            classes(&["chips", "legend", "about", "convs"])?;
        }
        "ol" => {
            attrs_ok(&["class"])?;
            classes(&["versions"])?;
        }
        "div" => {
            attrs_ok(&["class"])?;
            classes(&["att"])?;
        }
        other => return Err(format!("element <{other}> is not in the template")),
    }
    Ok(())
}

/// HTML's "valid global date and time string" in UTC:
/// `YYYY-MM-DDTHH:MM:SS[.f{1,3}]Z`.
fn global_date(d: &str) -> bool {
    let b = d.as_bytes();
    let shape = b"dddd-dd-ddTdd:dd:dd";
    if b.len() < shape.len() + 1 {
        return false;
    }
    for (i, want) in shape.iter().enumerate() {
        let ok = match want {
            b'd' => b[i].is_ascii_digit(),
            c => b[i] == *c,
        };
        if !ok {
            return false;
        }
    }
    match &b[shape.len()..] {
        b"Z" => true,
        [b'.', rest @ ..] => {
            let Some(n) = rest.len().checked_sub(1) else {
                return false;
            };
            rest.last() == Some(&b'Z')
                && (1..=3).contains(&n)
                && rest[..n].iter().all(u8::is_ascii_digit)
        }
        _ => false,
    }
}

fn href_ok(h: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let page = |s: &str| -> bool {
        let Some(stem) = s.strip_suffix(".html") else {
            return false;
        };
        let (base, part) = match stem.rsplit_once('-') {
            Some((b, p)) if digits(p) && b != "no" => (b, Some(p)),
            _ => (stem, None),
        };
        let _ = part;
        base == "index" && part.is_none()
            || base == "no-conversation"
            || base
                .strip_prefix('c')
                .is_some_and(|n| n.len() >= 4 && digits(n))
    };
    if let Some(f) = h.strip_prefix('#') {
        return !f.is_empty()
            && f.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'+');
    }
    if let Some(p) = h.strip_prefix("previews/p") {
        return p
            .strip_suffix(".jpg")
            .or_else(|| p.strip_suffix(".png"))
            .is_some_and(digits);
    }
    page(h)
}

fn text_nodes<'a>(el: &'a Element, out: &mut Vec<(&'a Element, &'a str)>) {
    for c in &el.children {
        match c {
            Node::Text(t) => out.push((el, t)),
            Node::El(e) if e.name != "style" => text_nodes(e, out),
            Node::El(_) => {}
        }
    }
}

/// Apply the template's rules to a parsed page.
pub fn check(html: &Element, e: &Expect<'_>) -> Result<Page, String> {
    use sha2::Digest;
    let mut page = Page::default();
    let kids: Vec<&Element> = html.elements().collect();
    let (head, body) = match kids.as_slice() {
        [h, b] if h.name == "head" && b.name == "body" => (*h, *b),
        _ => return Err("html is not head + body".to_owned()),
    };
    // The head, in its fixed order: the CSP meta first after the charset.
    let h: Vec<&Element> = head.elements().collect();
    let shape: Vec<String> = h
        .iter()
        .map(|x| {
            let k = x
                .attr("charset")
                .map(|v| format!("charset={v}"))
                .or_else(|| x.attr("http-equiv").map(|v| format!("http-equiv={v}")))
                .or_else(|| x.attr("name").map(|v| format!("name={v}")))
                .unwrap_or_default();
            format!("{}[{k}]", x.name)
        })
        .collect();
    let want = [
        "meta[charset=utf-8]",
        "meta[http-equiv=Content-Security-Policy]",
        "meta[name=referrer]",
        "meta[name=viewport]",
        "meta[name=color-scheme]",
        "title[]",
        "style[]",
    ];
    if shape != want {
        return Err(format!("head: {shape:?}"));
    }
    if h[1].attr("content") != Some(e.csp) {
        return Err("the CSP differs".to_owned());
    }
    if h[2].attr("content") != Some("no-referrer") {
        return Err("referrer".to_owned());
    }
    page.title = h[5].plain();
    let style = h[6].plain();
    if style != e.style {
        return Err("the stylesheet differs".to_owned());
    }
    let digest = sha2::Sha256::digest(style.as_bytes());
    if oma_export::preview::base64(&digest) != e.style_sha256_b64
        || !e.csp.contains(&format!("'sha256-{}'", e.style_sha256_b64))
    {
        return Err("the style hash".to_owned());
    }
    // The body's landmarks, in order.
    let order: Vec<String> = body
        .elements()
        .map(|x| match x.attr("id") {
            Some(id) => format!("{}#{id}", x.name),
            None => x.name.clone(),
        })
        .collect();
    let want: Vec<&str> = if e.index {
        vec!["a", "header", "main#main", "footer#about"]
    } else {
        vec!["a", "a", "header", "main#main", "nav#days", "footer#about"]
    };
    if order != want {
        return Err(format!("body landmarks: {order:?}"));
    }
    for t in body.children.iter() {
        if let Node::Text(t) = t {
            if !t.trim().is_empty() {
                return Err("text directly in body".to_owned());
            }
        }
    }
    let all = html.walk();
    // Every element and attribute is the template's.
    for el in &all {
        allowed(el)?;
        for (k, _) in &el.attrs {
            if k.starts_with("on") || k == "style" {
                return Err(format!("attribute {k}"));
            }
        }
    }
    // One h1, in the header.
    let h1s: Vec<&&Element> = all.iter().filter(|x| x.name == "h1").collect();
    if h1s.len() != 1 {
        return Err(format!("{} h1", h1s.len()));
    }
    page.h1 = h1s[0].text();
    // Ids are unique.
    let mut ids = BTreeSet::new();
    for el in &all {
        if let Some(id) = el.attr("id") {
            if !ids.insert(id.to_owned()) {
                return Err(format!("id {id} twice"));
            }
        }
    }
    // Sections: labelled by their one h2; no h2 elsewhere.
    let mut h2_in_sections = 0;
    for s in all.iter().filter(|x| x.name == "section") {
        let h2: Vec<&Element> = s.elements().filter(|x| x.name == "h2").collect();
        match h2.as_slice() {
            [one] if one.attr("id") == s.attr("aria-labelledby") && one.attr("id").is_some() => {}
            _ => return Err("a section without its one h2".to_owned()),
        }
        if s.elements().next().map(|x| x.name.as_str()) != Some("h2") {
            return Err("a section not headed by its h2".to_owned());
        }
        h2_in_sections += 1;
    }
    if all.iter().filter(|x| x.name == "h2").count() != h2_in_sections {
        return Err("an h2 outside a section".to_owned());
    }
    // Articles: m<n>, labelled by their h3.
    for a in all.iter().filter(|x| x.name == "article") {
        let id = a.attr("id").unwrap_or("");
        let n: u64 = id
            .strip_prefix('m')
            .and_then(|n| n.parse().ok())
            .ok_or_else(|| format!("article id {id}"))?;
        let h3 = a.elements().next().ok_or("an empty article")?;
        if h3.name != "h3"
            || h3.attr("id") != Some(&format!("m{n}-h"))
            || a.attr("aria-labelledby") != Some(&format!("m{n}-h"))
        {
            return Err(format!("article m{n} is not labelled by its h3"));
        }
        page.articles.push((n, (*a).clone()));
    }
    // Links: our grammar; fragments exist; item links point backward.
    let mut seen_ids: BTreeSet<String> = BTreeSet::new();
    for el in &all {
        if let Some(h) = el.attr("href") {
            if !href_ok(h) {
                return Err(format!("href outside the grammar: {h}"));
            }
            if let Some(f) = h.strip_prefix('#') {
                if !ids.contains(f) {
                    return Err(format!("a link to a missing #{f}"));
                }
                if let Some(n) = f.strip_prefix('m') {
                    if n.bytes().all(|b| b.is_ascii_digit()) {
                        if !seen_ids.contains(f) {
                            return Err(format!("a forward item link #{f}"));
                        }
                        page.item_links += 1;
                    }
                }
            }
            page.hrefs.push(h.to_owned());
        }
        if let Some(id) = el.attr("id") {
            seen_ids.insert(id.to_owned());
        }
        if el.name == "img" {
            let src = el.attr("src").unwrap_or("");
            let b64 = src
                .strip_prefix("data:image/png;base64,")
                .map(|b| (b, &b"\x89PNG"[..]))
                .or_else(|| {
                    src.strip_prefix("data:image/jpeg;base64,")
                        .map(|b| (b, &b"\xFF\xD8"[..]))
                });
            let Some((b, sig)) = b64 else {
                return Err("an img src outside data:image/png|jpeg".to_owned());
            };
            let ok = b
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"+/=".contains(&c));
            if !ok || !b.starts_with(&oma_export::preview::base64(sig)[..4]) {
                return Err("an img src that is not the rendition's base64".to_owned());
            }
            page.data_bytes += src.len() as u64;
            page.images += 1;
        }
    }
    if page.data_bytes > e.data_budget {
        return Err(format!("data: {} over the budget", page.data_bytes));
    }
    // Text nodes: never a control; never a hidden character with markers
    // on; markers hold a code point only.
    let mut texts = Vec::new();
    text_nodes(html, &mut texts);
    for (el, t) in texts {
        if el.name == "span" && el.has_class("mark") && unmark(t).is_none() {
            return Err("a marker element that is not U+XXXX".to_owned());
        }
        if t.chars().any(|c| {
            let cp = u32::from(c);
            (0xFDD0..=0xFDEF).contains(&cp) || (cp & 0xFFFE) == 0xFFFE
        }) {
            return Err("a raw noncharacter in text".to_owned());
        }
        for p in classify(t) {
            let bad = p.class.is_control() || (e.markers && p.class.is_hidden());
            if bad || p.class == CharClass::Cr {
                return Err(format!("a raw {} character in text", p.class.name()));
            }
        }
    }
    Ok(page)
}
