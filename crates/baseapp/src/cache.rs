//! Small byte- and entry-bounded LRU for encoded media held by the UI.
use std::{cell::Cell, collections::HashMap};

struct Entry<T> {
    value: T,
    bytes: usize,
    used: Cell<u64>,
}

pub struct Cache<T> {
    entries: HashMap<String, Entry<T>>,
    bytes: usize,
    capacity: usize,
    max_entries: usize,
    clock: Cell<u64>,
    weight: fn(&T) -> usize,
}

impl<T> Cache<T> {
    pub fn new(capacity: usize, max_entries: usize, weight: fn(&T) -> usize) -> Self {
        Self {
            entries: HashMap::new(),
            bytes: 0,
            capacity,
            max_entries,
            clock: Cell::new(0),
            weight,
        }
    }
    fn tick(&self) -> u64 {
        let next = self.clock.get().wrapping_add(1);
        self.clock.set(next);
        next
    }
    pub fn get(&self, key: &str) -> Option<&T> {
        let entry = self.entries.get(key)?;
        entry.used.set(self.tick());
        Some(&entry.value)
    }
    pub fn insert(&mut self, key: String, value: T) {
        if let Some(previous) = self.entries.remove(&key) {
            self.bytes -= previous.bytes;
        }
        let bytes = (self.weight)(&value);
        if bytes > self.capacity || self.max_entries == 0 {
            return;
        }
        while self.bytes + bytes > self.capacity || self.entries.len() >= self.max_entries {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, value)| value.used.get())
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            self.bytes -= self.entries.remove(&oldest).unwrap().bytes;
        }
        let used = Cell::new(self.tick());
        self.entries.insert(key, Entry { value, bytes, used });
        self.bytes += bytes;
    }
    pub fn bytes(&self) -> usize {
        self.bytes
    }
    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }
    pub fn retain(&mut self, keep: impl Fn(&str) -> bool) {
        self.entries.retain(|key, entry| {
            if keep(key) {
                true
            } else {
                self.bytes -= entry.bytes;
                false
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cache_evicts_the_least_recently_used_bytes() {
        let mut cache = Cache::new(6, 10, Vec::len);
        cache.insert("a".into(), vec![1; 3]);
        cache.insert("b".into(), vec![2; 3]);
        assert!(cache.get("a").is_some());
        cache.insert("c".into(), vec![3; 3]);
        assert!(cache.get("b").is_none());
        assert!(cache.get("a").is_some());
        assert_eq!(cache.bytes(), 6);
    }
    #[test]
    fn cache_counts_replacements_and_rejects_oversized_files() {
        let mut cache = Cache::new(6, 10, Vec::len);
        cache.insert("a".into(), vec![1; 3]);
        cache.insert("a".into(), vec![2; 2]);
        assert_eq!(cache.bytes(), 2);
        cache.insert("large".into(), vec![0; 7]);
        assert!(cache.get("large").is_none());
        assert_eq!(cache.bytes(), 2);
    }
    #[test]
    fn cache_bounds_empty_entries_and_preserves_pending_uploads_on_clear() {
        let mut cache = Cache::new(8, 2, Vec::len);
        cache.insert("remote".into(), vec![]);
        cache.insert("local:1".into(), vec![1; 2]);
        cache.insert("another".into(), vec![]);
        assert!(cache.get("remote").is_none());
        cache.retain(|key| key.starts_with("local:"));
        assert_eq!(cache.bytes(), 2);
        assert!(cache.get("local:1").is_some());
        cache.clear();
        assert_eq!(cache.bytes(), 0);
        assert!(cache.get("local:1").is_none());
    }
}
