//! Mutable positive cache policy over immutable authenticated headers/pages.
use super::*;

/// Positive-only bounded cache. Namespace is the authenticated source manifest;
/// missing projections are deliberately retried because search is asynchronous.
#[derive(Clone, Debug)]
pub(crate) struct FulltextCache(Arc<Mutex<Cache>>);
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FulltextCacheStats {
    pub entries: usize,
    /// Cache-owned decoded capacities plus conservative entry overhead; not
    /// process RSS or Arc pages retained independently by active cursors.
    pub retained_bytes: usize,
    pub max_entries: usize,
    pub max_bytes: usize,
}
#[derive(Debug)]
pub(super) struct Cache {
    pages: LruMap<(Digest, Digest), (Arc<Page>, usize)>,
    headers: BTreeMap<Digest, FulltextProjection>,
    bytes: usize,
    max_entries: usize,
    max_bytes: usize,
}
impl Cache {
    pub(super) const HEADER_BYTES: usize =
        std::mem::size_of::<FulltextProjection>() + std::mem::size_of::<Digest>() + 64;
    fn evict(&mut self) {
        while self.pages.len() + self.headers.len() > self.max_entries
            || self.bytes > self.max_bytes
        {
            if let Some((_, (_, bytes))) = self.pages.pop_lru() {
                self.bytes -= bytes;
            } else if let Some(key) = self.headers.keys().next().copied() {
                self.headers.remove(&key);
                self.bytes -= Self::HEADER_BYTES;
            } else {
                break;
            }
        }
    }
}
impl FulltextCache {
    pub(crate) fn stats(&self) -> FulltextCacheStats {
        let c = self.0.lock().unwrap();
        FulltextCacheStats {
            entries: c.pages.len() + c.headers.len(),
            retained_bytes: c.bytes,
            max_entries: c.max_entries,
            max_bytes: c.max_bytes,
        }
    }
    pub(crate) fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self(Arc::new(Mutex::new(Cache {
            pages: LruMap::default(),
            headers: BTreeMap::new(),
            bytes: 0,
            max_entries,
            max_bytes,
        })))
    }
    pub(crate) fn header(&self, source: Digest) -> Option<FulltextProjection> {
        self.0.lock().unwrap().headers.get(&source).cloned()
    }
    pub(crate) fn insert_header_at(&self, key: Digest, p: FulltextProjection) {
        let mut c = self.0.lock().unwrap();
        if c.max_entries == 0 || Cache::HEADER_BYTES > c.max_bytes {
            return;
        }
        if c.headers.insert(key, p).is_none() {
            c.bytes += Cache::HEADER_BYTES;
        }
        c.evict();
    }
    pub(crate) fn get(&self, source: Digest, hash: Digest) -> Option<Arc<Page>> {
        let mut c = self.0.lock().unwrap();
        let key = (source, hash);
        c.pages.get(&key).map(|(p, _)| Arc::clone(p))
    }
    pub(crate) fn insert(
        &self,
        source: Digest,
        hash: Digest,
        page: Arc<Page>,
        _encoded_bytes: usize,
    ) {
        let mut c = self.0.lock().unwrap();
        // Decoded allocations plus conservative per-entry tree/LRU/Arc overhead.
        // Cursor-owned Arc references can outlive cache eviction and are charged
        // to the caller's bounded traversal, not falsely claimed as freed RSS.
        let bytes = page.retained_bytes().saturating_add(192);
        if c.max_entries == 0 || bytes > c.max_bytes {
            return;
        }
        let key = (source, hash);
        if let Some((_, old)) = c.pages.insert(key, (page, bytes)) {
            c.bytes -= old;
        }
        c.bytes += bytes;
        c.evict();
    }
}
