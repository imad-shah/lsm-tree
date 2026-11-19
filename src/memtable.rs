use std::collections::BTreeMap;

/// In-memory, sorted write buffer.
///
/// Every key maps to an [`Entry`](crate::entry::Entry): either a live value or a
/// [`Tombstone`](crate::entry::Entry::Tombstone). The tombstone records that the key was
/// deleted. Tombstones are kept so that when the memtable is
/// flushed, the delete shadows older values for the same key in older SSTables
#[derive(Default)]
pub(crate) struct MemTable {
    store: BTreeMap<crate::keys::InternalKey, crate::entry::Entry>,
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
    pub(crate) fn get(
        &self,
        key: &[u8],
        read_seq: crate::keys::SeqNum,
    ) -> Option<&crate::entry::Entry> {
        self.store
            .range(
                crate::keys::InternalKey {
                    user_key: key.to_vec(),
                    seq: read_seq,
                }..,
            )
            .next()
            .and_then(|(k, v)| (k.user_key == key).then_some(v))
    }

    pub(crate) fn put(&mut self, key: Vec<u8>, seq: crate::keys::SeqNum, value: Vec<u8>) {
        let key = crate::keys::InternalKey { user_key: key, seq };
        let value = crate::entry::Entry::Value(value);
        self.store.insert(key, value);
    }

    /// Marks `key` as deleted by storing a tombstone
    ///
    /// This records a tombstone even if the key was never written to this
    /// memtable, because the key may still be live in an older SSTable
    pub(crate) fn remove(&mut self, key: &[u8], seq: crate::keys::SeqNum) {
        let key = crate::keys::InternalKey {
            user_key: key.to_vec(),
            seq,
        };
        self.store.insert(key, crate::entry::Entry::Tombstone);
    }

    /// Iterates over every entry in ascending key order
    pub(crate) fn iter(
        &self,
    ) -> impl Iterator<Item = (&crate::keys::InternalKey, &crate::entry::Entry)> {
        self.store.iter()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::Entry;
    use crate::keys::{InternalKey, SeqNum};

    fn val(bytes: &[u8]) -> Entry {
        Entry::Value(bytes.to_vec())
    }

    fn ikey(bytes: &[u8], seq: SeqNum) -> InternalKey {
        InternalKey {
            user_key: bytes.to_vec(),
            seq,
        }
    }

    #[test]
    fn missing_key_get_returns_none() {
        let table = MemTable::new();
        assert_eq!(table.get(b"abc", 1), None);
    }

    #[test]
    fn inserted_bytes_ok_retrieve() {
        let mut table = MemTable::new();
        table.put(b"abc".to_vec(), 1, b"def".to_vec());
        assert_eq!(table.get(b"abc", 1), Some(&val(b"def")));
    }

    #[test]
    fn newer_seq_shadows_older_value() {
        let mut table = MemTable::new();
        table.put(b"abc".to_vec(), 1, b"def".to_vec());
        table.put(b"abc".to_vec(), 2, b"hij".to_vec());

        assert_eq!(table.get(b"abc", 2), Some(&val(b"hij")));
        assert_eq!(table.get(b"abc", 1), Some(&val(b"def")));
    }

    #[test]
    fn empty_value_present_not_absent() {
        let mut table = MemTable::new();
        table.put(b"abc".to_vec(), 1, b"".to_vec());
        assert_eq!(table.get(b"abc", 1), Some(&val(b"")));
        assert_ne!(table.get(b"abc", 1), Some(&Entry::Tombstone));
    }

    #[test]
    fn remove_shadows_value_with_tombstone() {
        let mut table = MemTable::new();
        table.put(b"abc".to_vec(), 1, b"def".to_vec());
        assert_eq!(table.get(b"abc", 1), Some(&val(b"def")));

        table.remove(b"abc", 2);
        assert_eq!(table.get(b"abc", 2), Some(&Entry::Tombstone));
        assert_eq!(table.get(b"abc", 1), Some(&val(b"def")));
    }

    #[test]
    fn remove_absent_key_records_tombstone() {
        let mut table = MemTable::new();
        table.remove(b"abc", 1);
        assert_eq!(table.get(b"abc", 1), Some(&Entry::Tombstone));
    }

    #[test]
    fn put_after_remove_shadows_tombstone() {
        let mut table = MemTable::new();
        table.put(b"abc".to_vec(), 1, b"def".to_vec());
        table.remove(b"abc", 2);
        table.put(b"abc".to_vec(), 3, b"xyz".to_vec());

        assert_eq!(table.get(b"abc", 3), Some(&val(b"xyz")));
        assert_eq!(table.get(b"abc", 2), Some(&Entry::Tombstone));
        assert_eq!(table.get(b"abc", 1), Some(&val(b"def")));
    }

    #[test]
    fn get_empty_key_returns_none() {
        let mut table = MemTable::new();
        table.put(b"abc".to_vec(), 1, b"def".to_vec());
        assert_eq!(table.get(b"", 1), None);
    }

    #[test]
    fn empty_key_can_be_inserted_retrieved_removed() {
        let mut table = MemTable::new();
        table.put(b"".to_vec(), 1, b"def".to_vec());
        assert_eq!(table.get(b"", 1), Some(&val(b"def")));

        table.remove(b"", 2);
        assert_eq!(table.get(b"", 2), Some(&Entry::Tombstone));
    }

    #[test]
    fn non_utf8_keys() {
        let mut table = MemTable::new();
        table.put(vec![255], 1, vec![255]);
        assert_eq!(table.get(&[255], 1), Some(&val(&[255])));
    }

    #[test]
    fn removing_absent_key_does_not_break_existing_key() {
        let mut table = MemTable::new();
        table.put(b"abc".to_vec(), 1, b"def".to_vec());
        table.remove(b"", 2);

        assert_eq!(table.get(b"abc", 2), Some(&val(b"def")));
        assert_eq!(table.get(b"", 2), Some(&Entry::Tombstone));
    }

    #[test]
    fn empty_table_yields_no_entries() {
        let table = MemTable::new();
        let v: Vec<_> = table.iter().collect();
        assert!(v.is_empty());
    }

    #[test]
    fn removed_key_appears_in_iter_with_tombstone_first() {
        let mut table = MemTable::new();

        table.put(b"abc".to_vec(), 1, b"def".to_vec());
        let k1 = ikey(b"abc", 1);
        let v1 = val(b"def");
        assert_eq!(table.iter().collect::<Vec<_>>(), vec![(&k1, &v1)]);

        table.remove(b"abc", 2);
        let k2 = ikey(b"abc", 2);
        assert_eq!(
            table.iter().collect::<Vec<_>>(),
            vec![(&k2, &Entry::Tombstone), (&k1, &v1)]
        );
    }

    #[test]
    fn iter_is_sorted_by_user_key_asc_then_seq_desc() {
        let mut table = MemTable::new();
        table.put(vec![20], 1, vec![1]);
        table.put(vec![10], 2, vec![3]);
        table.put(vec![10, 5], 3, vec![2]);
        table.put(vec![20], 4, vec![4]);
        table.remove(&[15], 5);

        let v: Vec<_> = table.iter().collect();

        let (k_10, v_10) = (ikey(&[10], 2), val(&[3]));
        let (k_10_5, v_10_5) = (ikey(&[10, 5], 3), val(&[2]));
        let k_15 = ikey(&[15], 5);
        let (k_20_new, v_20_new) = (ikey(&[20], 4), val(&[4]));
        let (k_20_old, v_20_old) = (ikey(&[20], 1), val(&[1]));

        let expected: Vec<(&InternalKey, &Entry)> = vec![
            (&k_10, &v_10),
            (&k_10_5, &v_10_5),
            (&k_15, &Entry::Tombstone),
            (&k_20_new, &v_20_new),
            (&k_20_old, &v_20_old),
        ];
        assert_eq!(v, expected);
    }
}
