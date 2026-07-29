use std::fmt;

pub const CACHE_LINE_SIZE: usize = 64;

#[repr(C, align(64))]
pub struct CacheAligned<T> {
    value: T,
}

impl<T> CacheAligned<T> {
    pub fn new(value: T) -> Self {
        Self { value }
    }

    pub fn get(&self) -> &T {
        &self.value
    }

    pub fn get_mut(&mut self) -> &mut T {
        &mut self.value
    }

    pub fn into_inner(self) -> T {
        self.value
    }

    pub fn ptr(&self) -> *const T {
        &self.value as *const T
    }

    pub fn offset_bytes(&self) -> usize {
        let base = self as *const Self as usize;
        let field = &self.value as *const T as usize;
        field - base
    }
}

impl<T: Clone> Clone for CacheAligned<T> {
    fn clone(&self) -> Self {
        Self::new(self.value.clone())
    }
}

impl<T: Default> Default for CacheAligned<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T: fmt::Debug> fmt::Debug for CacheAligned<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CacheAligned")
            .field("value", &self.value)
            .finish()
    }
}

pub struct PaddedAtomicU64 {
    value: std::sync::atomic::AtomicU64,
    _pad: [u8; CACHE_LINE_SIZE - 8],
}

impl PaddedAtomicU64 {
    pub fn new(val: u64) -> Self {
        Self {
            value: std::sync::atomic::AtomicU64::new(val),
            _pad: [0; CACHE_LINE_SIZE - 8],
        }
    }

    pub fn load(&self, order: std::sync::atomic::Ordering) -> u64 {
        self.value.load(order)
    }

    pub fn store(&self, val: u64, order: std::sync::atomic::Ordering) {
        self.value.store(val, order);
    }

    pub fn fetch_add(&self, val: u64, order: std::sync::atomic::Ordering) -> u64 {
        self.value.fetch_add(val, order)
    }

    pub fn fetch_max(&self, val: u64, order: std::sync::atomic::Ordering) -> u64 {
        self.value.fetch_max(val, order)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_aligned_size() {
        let a = CacheAligned::new(42u64);
        assert_eq!(std::mem::size_of_val(&a) % CACHE_LINE_SIZE, 0);
    }

    #[test]
    fn test_cache_aligned_access() {
        let mut a = CacheAligned::new(10u32);
        assert_eq!(*a.get(), 10);
        *a.get_mut() = 20;
        assert_eq!(*a.get(), 20);
    }

    #[test]
    fn test_into_inner() {
        let a = CacheAligned::new(String::from("hello"));
        assert_eq!(a.into_inner(), "hello");
    }

    #[test]
    fn test_padded_atomic_u64() {
        let a = PaddedAtomicU64::new(0);
        a.store(42, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(a.load(std::sync::atomic::Ordering::Relaxed), 42);
    }

    #[test]
    fn test_padded_atomic_fetch_add() {
        let a = PaddedAtomicU64::new(10);
        let prev = a.fetch_add(5, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(prev, 10);
        assert_eq!(a.load(std::sync::atomic::Ordering::Relaxed), 15);
    }

    #[test]
    fn test_padded_atomic_fetch_max() {
        let a = PaddedAtomicU64::new(10);
        a.fetch_max(5, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(a.load(std::sync::atomic::Ordering::Relaxed), 10);
        a.fetch_max(20, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(a.load(std::sync::atomic::Ordering::Relaxed), 20);
    }

    #[test]
    fn test_offset_is_within_struct() {
        let a = CacheAligned::new(123u32);
        let offset = a.offset_bytes();
        assert!(offset < std::mem::size_of::<CacheAligned<u32>>());
    }

    #[test]
    fn test_clone() {
        let a = CacheAligned::new(vec![1, 2, 3]);
        let b = a.clone();
        assert_eq!(*a.get(), *b.get());
    }

    #[test]
    fn test_default() {
        let a: CacheAligned<u64> = CacheAligned::default();
        assert_eq!(*a.get(), 0);
    }
}
