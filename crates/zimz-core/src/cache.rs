// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Regis Boudinot

use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

use lru::LruCache;

use crate::cluster::ClusterData;
use crate::dirent::Dirent;

struct ClusterInner {
    map: LruCache<u32, Arc<ClusterData>>,
    bytes: usize,
    budget: usize,
}

/// LRU of decoded clusters bounded by a byte budget.
pub(crate) struct ClusterCache {
    inner: Mutex<ClusterInner>,
}

impl ClusterCache {
    pub fn new(budget: usize) -> Self {
        Self {
            inner: Mutex::new(ClusterInner {
                map: LruCache::unbounded(),
                bytes: 0,
                budget,
            }),
        }
    }

    pub fn get(&self, index: u32) -> Option<Arc<ClusterData>> {
        self.inner.lock().ok()?.map.get(&index).cloned()
    }

    pub fn insert(&self, index: u32, data: Arc<ClusterData>) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        let fp = data.memory_footprint();
        if let Some(old) = inner.map.push(index, data) {
            inner.bytes = inner.bytes.saturating_sub(old.1.memory_footprint());
        }
        inner.bytes += fp;
        while inner.bytes > inner.budget && inner.map.len() > 1 {
            if let Some((_, evicted)) = inner.map.pop_lru() {
                inner.bytes = inner.bytes.saturating_sub(evicted.memory_footprint());
            } else {
                break;
            }
        }
    }

    pub fn stats(&self) -> (usize, usize) {
        self.inner.lock().map_or((0, 0), |i| (i.map.len(), i.bytes))
    }
}

/// LRU of parsed directory entries.
pub(crate) struct DirentCache {
    inner: Mutex<LruCache<u32, Arc<Dirent>>>,
}

impl DirentCache {
    pub fn new(entries: usize) -> Self {
        let cap = NonZeroUsize::new(entries.max(1)).expect("non-zero");
        Self {
            inner: Mutex::new(LruCache::new(cap)),
        }
    }

    pub fn get(&self, index: u32) -> Option<Arc<Dirent>> {
        self.inner.lock().ok()?.get(&index).cloned()
    }

    pub fn insert(&self, index: u32, d: Arc<Dirent>) {
        if let Ok(mut m) = self.inner.lock() {
            m.put(index, d);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::{ClusterReadConfig, read_cluster};
    use crate::dirent::DirentKind;
    use crate::source::MemorySource;

    fn cluster(index: u32, blobs: usize) -> Arc<ClusterData> {
        let mut c = vec![1u8];
        let width = 4;
        let base = ((blobs + 1) * width) as u32;
        for i in 0..=blobs {
            c.extend_from_slice(&(base + i as u32).to_le_bytes());
        }
        c.extend_from_slice(&vec![0u8; blobs]);
        let src = MemorySource::new(c);
        Arc::new(
            read_cluster(
                &src,
                index,
                0,
                &ClusterReadConfig {
                    max_decompressed: 1 << 20,
                    max_blobs: 1000,
                },
            )
            .unwrap(),
        )
    }

    #[test]
    fn cluster_cache_evicts_by_bytes_and_keeps_one() {
        let footprint = cluster(0, 10).memory_footprint();
        let cache = ClusterCache::new(footprint * 2 + 1);
        cache.insert(0, cluster(0, 10));
        cache.insert(1, cluster(1, 10));
        assert_eq!(cache.stats(), (2, footprint * 2));
        assert!(
            cache.get(0).is_some(),
            "touch 0 so 1 becomes least recently used"
        );
        cache.insert(2, cluster(2, 10));
        assert_eq!(cache.stats().0, 2);
        assert!(cache.get(1).is_none(), "1 was evicted");
        assert!(cache.get(0).is_some() && cache.get(2).is_some());
        let tiny = ClusterCache::new(1);
        tiny.insert(7, cluster(7, 50));
        assert_eq!(tiny.stats().0, 1, "always keeps the last inserted cluster");
        tiny.insert(7, cluster(7, 50));
        assert_eq!(
            tiny.stats().0,
            1,
            "re-inserting the same key does not double count"
        );
        assert_eq!(tiny.get(8).map(|c| c.index()), None);
    }

    #[test]
    fn dirent_cache_is_lru() {
        let mk = |i: u32| {
            Arc::new(Dirent {
                index: i,
                namespace: b'C',
                mime: 0,
                path: i.to_string(),
                title: String::new(),
                kind: DirentKind::LinkTarget,
            })
        };
        let cache = DirentCache::new(2);
        cache.insert(1, mk(1));
        cache.insert(2, mk(2));
        assert!(cache.get(1).is_some());
        cache.insert(3, mk(3));
        assert!(cache.get(2).is_none() && cache.get(1).is_some() && cache.get(3).is_some());
        let zero = DirentCache::new(0);
        zero.insert(1, mk(1));
        assert!(zero.get(1).is_some(), "capacity 0 is treated as 1");
    }
}
