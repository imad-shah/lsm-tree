use std::collections::BTreeMap;

#[derive(Default)]
pub struct MemTable {
    store: BTreeMap<Vec<u8>, Vec<u8>>,
}

impl MemTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, key: &[u8]) -> Option<&[u8]> {
        self.store.get(key).map(|bytes| bytes.as_slice())
    }

    pub fn put(&mut self, key: Vec<u8>, value: Vec<u8>) {
        self.store.insert(key, value);
    }

    pub fn remove(&mut self, key: &[u8]) {
        self.store.remove(key);
    }

    pub fn iter(&self) -> impl Iterator<Item = (&[u8], &[u8])> {
        self.store.iter().map(|(k, v)| (k.as_slice(), v.as_slice()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_key_get_returns_none() {
        let table = MemTable::new();
        assert_eq!(table.get(b"abc"), None);
    }

    #[test]
    fn inserted_bytes_ok_retrieve() {
        let mut table = MemTable::new();
        table.put(b"abc".to_vec(), b"def".to_vec());
        assert_eq!(table.get(b"abc"), Some(b"def".as_slice()));
    }

    #[test]
    fn reinsertion_overrides() {
        let mut table = MemTable::new();
        table.put(b"abc".to_vec(), b"def".to_vec());
        table.put(b"abc".to_vec(), b"hij".to_vec());
        assert_eq!(table.get(b"abc"), Some(b"hij".as_slice()));
    }

    #[test]
    fn empty_value_present_not_absent() {
        let mut table = MemTable::new();
        table.put(b"abc".to_vec(), b"".to_vec());
        assert_eq!(table.get(b"abc"), Some(b"".as_slice()));
    }

    #[test]
    fn remove_deletes_key() {
        let mut table = MemTable::new();
        table.put(b"abc".to_vec(), b"def".to_vec());
        assert_eq!(table.get(b"abc"), Some(b"def".as_slice()));
        table.remove(b"abc");
        assert_eq!(table.get(b"abc"), None);
    }

    #[test]
    fn remove_absentkey_no_error() {
        let mut table = MemTable::new();
        table.remove(b"abc");
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
        assert_eq!(table.get(b""), Some(b"def".as_slice()));
        table.remove(b"");
        assert_eq!(table.get(b""), None);
    }

    #[test]
    fn test_non_utf8_keys() {
        let mut table = MemTable::new();
        table.put(vec![255], vec![255]);
        assert_eq!(table.get(vec![255].as_slice()), Some(vec![255].as_slice()));
    }

    #[test]
    fn removing_absent_key_does_not_break_existing_key() {
        let mut table = MemTable::new();
        table.put(b"abc".to_vec(), b"def".to_vec());
        table.remove(b"");
        assert_eq!(table.get(b"abc"), Some(b"def".as_slice()));
    }

    #[test]
    fn empty_table_yields_no_entries() {
        let table = MemTable::new();
        {
            let v: Vec<_> = table.iter().collect();
            assert!(v.is_empty());
        }
    }

    #[test]
    fn removed_key_does_not_appear_iter() {
        let mut table = MemTable::new();

        table.put(b"abc".to_vec(), b"def".to_vec());
        {
            let v: Vec<_> = table.iter().collect();
            assert_eq!(v[0], (b"abc".as_slice(), b"def".as_slice()));
        }

        table.remove(b"abc");
        {
            let v: Vec<_> = table.iter().collect();
            assert!(v.is_empty());
        }
    }

    #[test]
    fn iter_is_sorted_and_reflects_replacement() {
        let mut table = MemTable::new();
        table.put(vec![20], vec![1]);
        table.put(vec![10], vec![3]);
        table.put(vec![10, 5], vec![2]);
        table.put(vec![20], vec![4]);
        {
            let v: Vec<_> = table.iter().collect();
            let expected: &[(&[u8], &[u8])] = &[(&[10], &[3]), (&[10, 5], &[2]), (&[20], &[4])];
            assert_eq!(v, expected);
        }
    }
}
