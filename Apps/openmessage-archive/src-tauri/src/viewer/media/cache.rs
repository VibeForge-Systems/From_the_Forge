//! The rendition cache (M5 spec §5.8, D102): memory only, 96 MiB of
//! encoded renditions keyed by `(blob, rendition)`, evicted least recently
//! used, plus the failure cache that keeps a hostile image from being
//! decoded more than once per session. Both die with the viewer session.
//! Nothing here is ever written to disk.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use oma_archive::{BlobId, RenderedFormat, Rendition};

/// The cache's byte bound.
pub const CACHE_BYTES: usize = 96 << 20;

/// One cached rendition: what the protocol answers an LRU hit with.
#[derive(Clone)]
pub struct CachedImage {
    pub format: RenderedFormat,
    pub bytes: Arc<Vec<u8>>,
}

impl std::fmt::Debug for CachedImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedImage")
            .field("format", &self.format)
            .finish_non_exhaustive()
    }
}

pub type Key = (BlobId, Rendition);

/// The LRU and the failure cache.
#[derive(Default)]
pub struct RenditionCache {
    entries: HashMap<Key, (CachedImage, u64)>,
    order: BTreeMap<u64, Key>,
    tick: u64,
    bytes: usize,
    limit: usize,
    failed: HashSet<Key>,
}

impl RenditionCache {
    pub fn new(limit: usize) -> RenditionCache {
        RenditionCache {
            limit,
            ..RenditionCache::default()
        }
    }

    /// A hit, which becomes the most recently used entry.
    pub fn get(&mut self, key: Key) -> Option<CachedImage> {
        self.tick += 1;
        let tick = self.tick;
        let (img, used) = self.entries.get_mut(&key)?;
        self.order.remove(used);
        *used = tick;
        self.order.insert(tick, key);
        Some(img.clone())
    }

    /// Keep `img`, evicting the least recently used entries past the bound.
    /// A rendition larger than the whole bound is not kept.
    pub fn insert(&mut self, key: Key, img: CachedImage) {
        let size = img.bytes.len();
        if size > self.limit {
            return;
        }
        self.tick += 1;
        if let Some((old, used)) = self.entries.insert(key, (img, self.tick)) {
            self.order.remove(&used);
            self.bytes -= old.bytes.len();
        }
        self.order.insert(self.tick, key);
        self.bytes += size;
        while self.bytes > self.limit {
            let Some((_, k)) = self.order.pop_first() else {
                break;
            };
            if let Some((old, _)) = self.entries.remove(&k) {
                self.bytes -= old.bytes.len();
            }
        }
    }

    pub fn fail(&mut self, key: Key) {
        self.failed.insert(key);
    }

    pub fn failed(&self, key: Key) -> bool {
        self.failed.contains(&key)
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Forget everything (session close; the privacy screen in slice 5j).
    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.bytes = 0;
        self.failed.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(n: usize) -> CachedImage {
        CachedImage {
            format: RenderedFormat::Jpeg,
            bytes: Arc::new(vec![0; n]),
        }
    }

    #[test]
    fn evicts_least_recently_used_within_the_byte_bound() {
        let mut c = RenditionCache::new(100);
        let k = |i| (BlobId::new(i), Rendition::Thumb);
        c.insert(k(1), img(40));
        c.insert(k(2), img(40));
        assert!(c.get(k(1)).is_some(), "touch 1");
        c.insert(k(3), img(40));
        assert!(c.get(k(2)).is_none(), "2 was least recently used");
        assert!(c.get(k(1)).is_some() && c.get(k(3)).is_some());
        assert_eq!(c.bytes(), 80);
        c.insert(k(4), img(101));
        assert!(c.get(k(4)).is_none(), "larger than the bound");
        c.insert(k(1), img(10));
        assert_eq!(c.bytes(), 50, "a replacement is counted once");
        c.fail(k(9));
        assert!(c.failed(k(9)) && !c.failed(k(1)));
        c.clear();
        assert!(c.is_empty() && !c.failed(k(9)) && c.bytes() == 0);
    }
}
