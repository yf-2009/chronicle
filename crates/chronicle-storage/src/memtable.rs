//! In-memory sorted table backing the "hot" write path.
//!
//! Implementation note: the reference design for Chronicle's MemTable calls
//! for a skip list, which is the classic choice (RocksDB, LevelDB) because it
//! gives lock-free-friendly concurrent access. We use a `BTreeMap` guarded by
//! a single `RwLock` instead -- also a real, production-used design (RocksDB
//! ships a `SkipList` memtable *and* a `HashLinkList`/vector variant; the
//! interface is pluggable specifically because no single structure is
//! strictly better). We chose the simpler structure deliberately: this
//! project is built and verified through remote CI rather than a local
//! compiler (see docs/adr/0006-remote-ci-verification.md), so we favored the
//! implementation we could reason about correctly by inspection over one
//! with more opportunities for subtle unsafe-pointer bugs. See
//! docs/adr/0002-lsm-tree-storage-engine.md for the full tradeoff.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::RwLock;

/// A single slot in the memtable: `None` represents a tombstone (delete).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemValue {
    pub value: Option<Vec<u8>>,
    pub seq: u64,
}

pub struct MemTable {
    entries: RwLock<BTreeMap<Vec<u8>, MemValue>>,
    size_bytes: AtomicUsize,
}

impl Default for MemTable {
    fn default() -> Self {
        Self::new()
    }
}

impl MemTable {
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(BTreeMap::new()),
            size_bytes: AtomicUsize::new(0),
        }
    }

    fn entry_cost(key: &[u8], value: Option<&Vec<u8>>) -> usize {
        key.len() + value.map(|v| v.len()).unwrap_or(0) + 16 // rough overhead per entry
    }

    pub fn put(&self, key: Vec<u8>, value: Vec<u8>, seq: u64) {
        let cost = Self::entry_cost(&key, Some(&value));
        let mut guard = self.entries.write().unwrap();
        let prev = guard.insert(key.clone(), MemValue { value: Some(value), seq });
        drop(guard);
        let prev_cost = prev
            .as_ref()
            .map(|p| Self::entry_cost(&key, p.value.as_ref()))
            .unwrap_or(0);
        self.adjust_size(cost, prev_cost);
    }

    pub fn delete(&self, key: Vec<u8>, seq: u64) {
        let cost = Self::entry_cost(&key, None);
        let mut guard = self.entries.write().unwrap();
        let prev = guard.insert(key.clone(), MemValue { value: None, seq });
        drop(guard);
        let prev_cost = prev
            .as_ref()
            .map(|p| Self::entry_cost(&key, p.value.as_ref()))
            .unwrap_or(0);
        self.adjust_size(cost, prev_cost);
    }

    fn adjust_size(&self, added: usize, removed: usize) {
        if added >= removed {
            self.size_bytes.fetch_add(added - removed, Ordering::Relaxed);
        } else {
            self.size_bytes.fetch_sub(removed - added, Ordering::Relaxed);
        }
    }

    pub fn get(&self, key: &[u8]) -> Option<MemValue> {
        self.entries.read().unwrap().get(key).cloned()
    }

    pub fn size_bytes(&self) -> usize {
        self.size_bytes.load(Ordering::Relaxed)
    }

    pub fn len(&self) -> usize {
        self.entries.read().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Snapshot the current contents in sorted key order. Used when flushing
    /// to an SSTable.
    pub fn snapshot(&self) -> Vec<(Vec<u8>, MemValue)> {
        self.entries
            .read()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_then_get() {
        let mt = MemTable::new();
        mt.put(b"k1".to_vec(), b"v1".to_vec(), 1);
        let got = mt.get(b"k1").unwrap();
        assert_eq!(got.value, Some(b"v1".to_vec()));
        assert_eq!(got.seq, 1);
    }

    #[test]
    fn newer_seq_overwrites() {
        let mt = MemTable::new();
        mt.put(b"k1".to_vec(), b"v1".to_vec(), 1);
        mt.put(b"k1".to_vec(), b"v2".to_vec(), 2);
        let got = mt.get(b"k1").unwrap();
        assert_eq!(got.value, Some(b"v2".to_vec()));
        assert_eq!(got.seq, 2);
        assert_eq!(mt.len(), 1);
    }

    #[test]
    fn delete_creates_tombstone() {
        let mt = MemTable::new();
        mt.put(b"k1".to_vec(), b"v1".to_vec(), 1);
        mt.delete(b"k1".to_vec(), 2);
        let got = mt.get(b"k1").unwrap();
        assert_eq!(got.value, None);
        assert_eq!(got.seq, 2);
    }

    #[test]
    fn missing_key_is_none() {
        let mt = MemTable::new();
        assert!(mt.get(b"missing").is_none());
    }

    #[test]
    fn size_bytes_tracks_growth_and_overwrite() {
        let mt = MemTable::new();
        mt.put(b"k1".to_vec(), b"v1".to_vec(), 1);
        let s1 = mt.size_bytes();
        assert!(s1 > 0);
        mt.put(b"k1".to_vec(), b"v1_longer_value".to_vec(), 2);
        let s2 = mt.size_bytes();
        assert!(s2 > s1);
    }

    #[test]
    fn snapshot_is_sorted() {
        let mt = MemTable::new();
        mt.put(b"c".to_vec(), b"3".to_vec(), 1);
        mt.put(b"a".to_vec(), b"1".to_vec(), 2);
        mt.put(b"b".to_vec(), b"2".to_vec(), 3);
        let snap = mt.snapshot();
        let keys: Vec<_> = snap.iter().map(|(k, _)| k.clone()).collect();
        assert_eq!(keys, vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()]);
    }
}
