//! Media tokens (M5 spec §6.3): one stable, random 128-bit token per
//! attachment surfaced in a viewer session.
//!
//! - **What a token protects:** URLs never carry a row id, only files the
//!   session surfaced can be served, and URLs from a closed session fail.
//! - **What it does not:** a compromised page can still read any message
//!   through IPC. The token is defence in depth, not a boundary.
//! - **One table per session**, so tokens die with it (`close_viewer`, an
//!   archive switch, a reopen after a reload, idle hide, exit).
//! - **At most [`TOKEN_LIMIT`] entries**, evicting the one least recently
//!   minted or requested, so a mounted image (requested recently) stays.
//!   An evicted token is re-minted, as a new value, on the next page.
//!
//! The table never logs a token, an id or a kind.

use std::collections::{BTreeMap, HashMap};

use oma_archive::{AttachmentId, BlobId, SniffedKind};

/// The most tokens a session keeps.
pub const TOKEN_LIMIT: usize = 50_000;

/// A token's 16 random bytes.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct TokenBytes([u8; 16]);

impl std::fmt::Debug for TokenBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TokenBytes(..)")
    }
}

const HEX: &[u8; 16] = b"0123456789abcdef";

impl TokenBytes {
    /// A fresh random token, from the OS (`getrandom`).
    pub fn random() -> Option<TokenBytes> {
        let mut b = [0u8; 16];
        getrandom::fill(&mut b).ok()?;
        Some(TokenBytes(b))
    }

    #[cfg(test)]
    pub fn from_bytes(b: [u8; 16]) -> TokenBytes {
        TokenBytes(b)
    }

    /// 32 lowercase hex characters: the URL form.
    pub fn hex(&self) -> String {
        let mut s = String::with_capacity(32);
        for b in self.0 {
            s.push(char::from(HEX[usize::from(b >> 4)]));
            s.push(char::from(HEX[usize::from(b & 0x0f)]));
        }
        s
    }

    /// Exactly 32 lowercase hex characters, or `None`.
    pub fn parse(s: &str) -> Option<TokenBytes> {
        let s = s.as_bytes();
        if s.len() != 32 {
            return None;
        }
        let nibble = |c: u8| match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            _ => None,
        };
        let mut out = [0u8; 16];
        for (i, pair) in s.chunks_exact(2).enumerate() {
            out[i] = (nibble(pair[0])? << 4) | nibble(pair[1])?;
        }
        Some(TokenBytes(out))
    }
}

/// What `inspect_attachment` found for the attachment's stored file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inspected {
    pub blob: BlobId,
    pub kind: SniffedKind,
}

/// One token's entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenEntry {
    pub attachment: AttachmentId,
    /// The stored file, as the page listed it.
    pub blob: BlobId,
    /// Set by `inspect_attachment`; until then no rendition is served.
    pub inspected: Option<Inspected>,
    used: u64,
}

/// The session's token table.
#[derive(Default)]
pub struct TokenTable {
    by_token: HashMap<TokenBytes, TokenEntry>,
    by_attachment: HashMap<AttachmentId, TokenBytes>,
    order: BTreeMap<u64, TokenBytes>,
    tick: u64,
    limit: usize,
}

impl TokenTable {
    pub fn new(limit: usize) -> TokenTable {
        TokenTable {
            limit: limit.max(1),
            ..TokenTable::default()
        }
    }

    fn touch(&mut self, token: TokenBytes) {
        self.tick += 1;
        let tick = self.tick;
        if let Some(e) = self.by_token.get_mut(&token) {
            self.order.remove(&e.used);
            e.used = tick;
            self.order.insert(tick, token);
        }
    }

    /// The attachment's token, minted on first use. `None` only when the
    /// OS gave no random bytes.
    pub fn mint(&mut self, attachment: AttachmentId, blob: BlobId) -> Option<TokenBytes> {
        if let Some(&t) = self.by_attachment.get(&attachment) {
            if let Some(e) = self.by_token.get_mut(&t) {
                if e.blob != blob {
                    // The page now lists another file (a later import).
                    e.blob = blob;
                    e.inspected = None;
                }
            }
            self.touch(t);
            return Some(t);
        }
        let token = loop {
            let t = TokenBytes::random()?;
            if !self.by_token.contains_key(&t) {
                break t;
            }
        };
        self.tick += 1;
        self.by_token.insert(
            token,
            TokenEntry {
                attachment,
                blob,
                inspected: None,
                used: self.tick,
            },
        );
        self.by_attachment.insert(attachment, token);
        self.order.insert(self.tick, token);
        while self.by_token.len() > self.limit {
            let Some((_, old)) = self.order.pop_first() else {
                break;
            };
            if let Some(e) = self.by_token.remove(&old) {
                self.by_attachment.remove(&e.attachment);
            }
        }
        Some(token)
    }

    /// The entry for a requested token, which becomes the most recently
    /// used.
    pub fn lookup(&mut self, token: TokenBytes) -> Option<TokenEntry> {
        let e = *self.by_token.get(&token)?;
        self.touch(token);
        Some(e)
    }

    /// Record what the attachment's file is.
    pub fn set_inspected(&mut self, attachment: AttachmentId, found: Inspected) {
        if let Some(t) = self.by_attachment.get(&attachment) {
            if let Some(e) = self.by_token.get_mut(t) {
                if e.blob == found.blob {
                    e.inspected = Some(found);
                }
            }
        }
    }

    pub fn len(&self) -> usize {
        self.by_token.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_token.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips_and_the_grammar_is_strict() {
        let t = TokenBytes::from_bytes([0xab; 16]);
        assert_eq!(t.hex(), "ab".repeat(16));
        assert_eq!(TokenBytes::parse(&t.hex()), Some(t));
        for bad in [
            "a".repeat(31),
            "a".repeat(33),
            "A".repeat(32),
            "g".repeat(32),
            format!("{}é", "a".repeat(30)),
            String::new(),
        ] {
            assert_eq!(TokenBytes::parse(&bad), None, "{bad:?}");
        }
        let a = TokenBytes::random().expect("random");
        let b = TokenBytes::random().expect("random");
        assert_ne!(a, b);
        assert!(!format!("{a:?}").contains(&a.hex()));
    }

    #[test]
    fn tokens_are_stable_per_attachment_and_evicted_least_recently_used() {
        let mut t = TokenTable::new(3);
        let att = |i| AttachmentId::new(i);
        let blob = |i| BlobId::new(i);
        let one = t.mint(att(1), blob(1)).expect("mint");
        assert_eq!(t.mint(att(1), blob(1)), Some(one), "stable");
        let two = t.mint(att(2), blob(2)).expect("mint");
        let three = t.mint(att(3), blob(3)).expect("mint");
        // Requesting 1 keeps it; 2 is now the least recently used.
        assert!(t.lookup(one).is_some());
        let four = t.mint(att(4), blob(4)).expect("mint");
        assert_eq!(t.len(), 3);
        assert!(t.lookup(two).is_none(), "evicted");
        assert!(t.lookup(one).is_some() && t.lookup(three).is_some());
        assert!(t.lookup(four).is_some());
        // An evicted attachment gets a new token.
        let again = t.mint(att(2), blob(2)).expect("mint");
        assert_ne!(again, two);
        // Inspection is recorded for the listed file only.
        let found = Inspected {
            blob: blob(4),
            kind: SniffedKind::Png,
        };
        t.set_inspected(att(4), found);
        assert_eq!(t.lookup(four).and_then(|e| e.inspected), Some(found));
        t.mint(att(4), blob(5));
        assert_eq!(
            t.lookup(four).and_then(|e| e.inspected),
            None,
            "file changed"
        );
    }
}
