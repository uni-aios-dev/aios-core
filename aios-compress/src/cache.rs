//! LRU cache for decompression results

use aios_core::error::Result;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

/// LRU cache entry
#[derive(Clone, Debug)]
struct CacheEntry {
    data: Arc<Vec<u8>>,
    access_count: u64,
}

/// Compression cache with LRU eviction
pub struct CompressionCache {
    /// Cache storage
    cache: RefCell<HashMap<String, CacheEntry>>,
    /// Maximum entries
    max_entries: usize,
    /// Global access counter for LRU
    access_counter: RefCell<u64>,
}

impl CompressionCache {
    /// Create cache with max entries
    pub fn new(max_entries: usize) -> Self {
        CompressionCache {
            cache: RefCell::new(HashMap::new()),
            max_entries,
            access_counter: RefCell::new(0),
        }
    }

    /// Get entry from cache
    pub fn get(&self, key: &str) -> Option<Arc<Vec<u8>>> {
        let mut cache = self.cache.borrow_mut();
        if let Some(entry) = cache.get_mut(key) {
            let mut counter = self.access_counter.borrow_mut();
            *counter += 1;
            entry.access_count = *counter;
            return Some(entry.data.clone());
        }
        None
    }

    /// Insert entry into cache
    pub fn insert(&self, key: String, data: Arc<Vec<u8>>) -> Result<()> {
        let mut cache = self.cache.borrow_mut();

        if cache.len() >= self.max_entries && !cache.contains_key(&key) {
            // Evict LRU entry
            if let Some(lru_key) = cache
                .iter()
                .min_by_key(|(_, v)| v.access_count)
                .map(|(k, _)| k.clone())
            {
                cache.remove(&lru_key);
            }
        }

        let mut counter = self.access_counter.borrow_mut();
        *counter += 1;

        cache.insert(
            key,
            CacheEntry {
                data,
                access_count: *counter,
            },
        );

        Ok(())
    }

    /// Clear cache
    pub fn clear(&self) {
        self.cache.borrow_mut().clear();
    }

    /// Get cache statistics
    pub fn stats(&self) -> (usize, usize) {
        let cache = self.cache.borrow();
        (cache.len(), self.max_entries)
    }

    /// Check if key exists
    pub fn contains(&self, key: &str) -> bool {
        self.cache.borrow().contains_key(key)
    }

    /// Get cache size in bytes
    pub fn size_bytes(&self) -> usize {
        self.cache.borrow().values().map(|e| e.data.len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_insert_get() {
        let cache = CompressionCache::new(10);
        let data = Arc::new(vec![1, 2, 3, 4, 5]);

        cache.insert("key1".to_string(), data.clone()).unwrap();
        let retrieved = cache.get("key1");

        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap(), data);
    }

    #[test]
    fn test_cache_lru_eviction() {
        let cache = CompressionCache::new(3);

        cache.insert("key1".to_string(), Arc::new(vec![1])).unwrap();
        cache.insert("key2".to_string(), Arc::new(vec![2])).unwrap();
        cache.insert("key3".to_string(), Arc::new(vec![3])).unwrap();

        // Access key1 to make it more recent
        let _ = cache.get("key1");

        // Add key4, should evict key2 (least recently used)
        cache.insert("key4".to_string(), Arc::new(vec![4])).unwrap();

        assert!(cache.contains("key1"));
        assert!(!cache.contains("key2"));
        assert!(cache.contains("key3"));
        assert!(cache.contains("key4"));
    }

    #[test]
    fn test_cache_clear() {
        let cache = CompressionCache::new(10);
        cache.insert("key1".to_string(), Arc::new(vec![1])).unwrap();
        cache.insert("key2".to_string(), Arc::new(vec![2])).unwrap();

        let (size, _) = cache.stats();
        assert_eq!(size, 2);

        cache.clear();
        let (size, _) = cache.stats();
        assert_eq!(size, 0);
    }

    #[test]
    fn test_cache_stats() {
        let cache = CompressionCache::new(10);
        cache
            .insert("key1".to_string(), Arc::new(vec![1, 2, 3]))
            .unwrap();
        cache
            .insert("key2".to_string(), Arc::new(vec![4, 5]))
            .unwrap();

        let (entries, max) = cache.stats();
        assert_eq!(entries, 2);
        assert_eq!(max, 10);

        let size = cache.size_bytes();
        assert_eq!(size, 5); // 3 + 2 bytes
    }

    #[test]
    fn test_cache_size_bytes() {
        let cache = CompressionCache::new(10);
        cache
            .insert("key1".to_string(), Arc::new(vec![0; 100]))
            .unwrap();
        cache
            .insert("key2".to_string(), Arc::new(vec![0; 50]))
            .unwrap();

        assert_eq!(cache.size_bytes(), 150);
    }
}
