use std::collections::BTreeMap;

/// In-memory, sorted write buffer.
///
/// Every key maps to an [`Entry`](crate::entry::Entry): either a live value or a
/// [`Tombstone`](crate::entry::Entry::Tombstone). The tombstone records that the key was
/// deleted. Tombstones are kept so that when the memtable is
/// flushed, the delete shadows older values for the same key in older SSTables
#[derive(Default)]
pub(crate) struct MemTable {
    store: BTreeMap<Vec<u8>, crate::entry::Entry>,
}

impl MemTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the entry stored for `key`.
    ///
    /// - `Some(Entry::Value(_))`: the key has a live value
    /// - `Some(Entry::Tombstone)`: the key was deleted in this memtable
    /// - `None`: this memtable knows nothing about the key (an older SSTable might)
    pub(crate) fn get(&self, key: &[u8]) -> Option<&crate::entry::Entry> {
        self.store.get(key)
    }

    pub(crate) fn put(&mut self, key: Vec<u8>, value: Vec<u8>) {
        self.store.insert(key, crate::entry::Entry::Value(value));
    }

    /// Marks `key` as deleted by storing a tombstone
    ///
    /// This records a tombstone even if the key was never written to this
    /// memtable, because the key may still be live in an older SSTable
    pub(crate) fn remove(&mut self, key: &[u8]) {
        self.store
            .insert(key.to_vec(), crate::entry::Entry::Tombstone);
    }

    /// Iterates over every entry in ascending key order
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&[u8], &crate::entry::Entry)> {
        self.store.iter().map(|(k, v)| (k.as_slice(), v))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::Entry;

    // helper so expected values are readable
    fn val(bytes: &[u8]) -> Entry {
        Entry::Value(bytes.to_vec())
    }

    #[test]
    fn missing_key_get_returns_none() {
        let table = MemTable::new();
        assert_eq!(table.get(b"abc"), None);
    }

    #[test]
    fn inserted_bytes_ok_retrieve() {
        let mut table = MemTable::new();
        table.put(b"abc".to_vec(), b"def".to_vec());
        assert_eq!(table.get(b"abc"), Some(&val(b"def")));
    }

    #[test]
    fn reinsertion_overrides() {
        let mut table = MemTable::new();
        table.put(b"abc".to_vec(), b"def".to_vec());
        table.put(b"abc".to_vec(), b"hij".to_vec());
        assert_eq!(table.get(b"abc"), Some(&val(b"hij")));
    }

    #[test]
    fn empty_value_present_not_absent() {
        let mut table = MemTable::new();
        table.put(b"abc".to_vec(), b"".to_vec());
        assert_eq!(table.get(b"abc"), Some(&val(b"")));
        assert_ne!(table.get(b"abc"), Some(&Entry::Tombstone));
    }

    #[test]
    fn remove_replaces_value_with_tombstone() {
        let mut table = MemTable::new();
        table.put(b"abc".to_vec(), b"def".to_vec());
        assert_eq!(table.get(b"abc"), Some(&val(b"def")));
        table.remove(b"abc");
        assert_eq!(table.get(b"abc"), Some(&Entry::Tombstone));
    }

    #[test]
    fn remove_absent_key_records_tombstone() {
        let mut table = MemTable::new();
        table.remove(b"abc");
        assert_eq!(table.get(b"abc"), Some(&Entry::Tombstone));
    }

    #[test]
    fn put_after_remove_overrides_tombstone() {
        let mut table = MemTable::new();
        table.put(b"abc".to_vec(), b"def".to_vec());
        table.remove(b"abc");
        table.put(b"abc".to_vec(), b"xyz".to_vec());
        assert_eq!(table.get(b"abc"), Some(&val(b"xyz")));
    }

    #[test]
    fn get_empty_key_returns_none() {
        let mut table = MemTable::new();
        table.put(b"abc".to_vec(), b"def".to_vec());
        assert_eq!(table.get(b""), None);
    }

    #[test]
    fn empty_key_can_be_inserted_retrieved_removed() {
        let mut table = MemTable::new();
        table.put(b"".to_vec(), b"def".to_vec());
        assert_eq!(table.get(b""), Some(&val(b"def")));
        table.remove(b"");
        assert_eq!(table.get(b""), Some(&Entry::Tombstone));
    }

    #[test]
    fn non_utf8_keys() {
        let mut table = MemTable::new();
        table.put(vec![255], vec![255]);
        assert_eq!(table.get(&[255]), Some(&val(&[255])));
    }

    #[test]
    fn removing_absent_key_does_not_break_existing_key() {
        let mut table = MemTable::new();
        table.put(b"abc".to_vec(), b"def".to_vec());
        table.remove(b"");
        assert_eq!(table.get(b"abc"), Some(&val(b"def")));
        assert_eq!(table.get(b""), Some(&Entry::Tombstone));
    }

    #[test]
    fn empty_table_yields_no_entries() {
        let table = MemTable::new();
        let v: Vec<_> = table.iter().collect();
        assert!(v.is_empty());
    }

    #[test]
    fn removed_key_appears_in_iter_as_tombstone() {
        let mut table = MemTable::new();

        table.put(b"abc".to_vec(), b"def".to_vec());
        let v: Vec<_> = table.iter().collect();
        assert_eq!(v, vec![(b"abc".as_slice(), &val(b"def"))]);

        table.remove(b"abc");
        let v: Vec<_> = table.iter().collect();
        assert_eq!(v, vec![(b"abc".as_slice(), &Entry::Tombstone)]);
    }

    #[test]
    fn iter_is_sorted_and_reflects_replacement() {
        let mut table = MemTable::new();
        table.put(vec![20], vec![1]);
        table.put(vec![10], vec![3]);
        table.put(vec![10, 5], vec![2]);
        table.put(vec![20], vec![4]);
        table.remove(&[15]);
        let v: Vec<_> = table.iter().collect();
        let (three, two, four) = (val(&[3]), val(&[2]), val(&[4]));
        let expected: Vec<(&[u8], &Entry)> = vec![
            (&[10], &three),
            (&[10, 5], &two),
            (&[15], &Entry::Tombstone),
            (&[20], &four),
        ];
        assert_eq!(v, expected);
    }
}
