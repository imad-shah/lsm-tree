pub(crate) struct Engine {
    memtable: crate::memtable::MemTable,
    sstables: Vec<crate::sstable::SsTable>,
}

#[derive(Debug)]
pub(crate) enum FlushError {
    Write(crate::sstable::TableWriteError),
    Read(crate::sstable::TableReadError),
}

impl Engine {
    pub(crate) fn new() -> Self {
        Self {
            memtable: crate::memtable::MemTable::new(),
            sstables: vec![],
        }
    }

    pub(crate) fn put(&mut self, key: Vec<u8>, value: Vec<u8>) {
        self.memtable.put(key, value);
    }

    pub(crate) fn get(&self, key: &[u8]) -> Option<&[u8]> {
        match self.memtable.get(key) {
            Some(crate::entry::Entry::Value(bytes)) => return Some(bytes.as_slice()),
            Some(crate::entry::Entry::Tombstone) => return None,
            None => {}
        };

        for sst in self.sstables.iter().rev() {
            match sst.get(key) {
                Some(crate::entry::Entry::Value(bytes)) => return Some(bytes.as_slice()),
                Some(crate::entry::Entry::Tombstone) => return None,
                None => {}
            };
        }
        None
    }

    pub(crate) fn remove(&mut self, key: &[u8]) {
        self.memtable.remove(key);
    }

    pub(crate) fn flush(&mut self, path: &std::path::Path) -> Result<(), FlushError> {
        crate::sstable::write_new(path, &self.memtable).map_err(FlushError::Write)?;
        let table = crate::sstable::SsTable::open(path).map_err(FlushError::Read)?;
        self.sstables.push(table);
        self.memtable = crate::memtable::MemTable::new();
        Ok(())
    }
}

#[cfg(test)]
mod test {
    #[test]
    fn integration_test() {
        let mut eng = crate::engine::Engine::new();
        let dir = tempfile::tempdir().unwrap();
        let fp1 = dir.path().join("first.sst");
        let fp2 = dir.path().join("second.sst");

        let key = b"apple".as_slice();
        let value = b"red".as_slice();
        let value_2 = b"green".as_slice();

        eng.put(key.to_vec(), value.to_vec());
        assert_eq!(eng.get(key), Some(value));
        eng.flush(&fp1).unwrap();
        assert_eq!(eng.get(key), Some(value));

        eng.remove(key);
        assert_eq!(eng.get(key), None);

        eng.flush(&fp2).unwrap();
        assert_eq!(eng.get(key), None);

        eng.put(key.to_vec(), value_2.to_vec());
        assert_eq!(eng.get(key), Some(value_2));
    }

    #[test]
    fn get_missing_key_returns_none() {
        let mut eng = crate::engine::Engine::new();
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(eng.get(b"ghost"), None);

        eng.put(b"apple".to_vec(), b"red".to_vec());
        eng.flush(&dir.path().join("1.sst")).unwrap();
        assert_eq!(eng.get(b"ghost"), None);
    }

    #[test]
    fn newest_sstable_wins_across_value_tombstone_value() {
        let mut eng = crate::engine::Engine::new();
        let dir = tempfile::tempdir().unwrap();

        eng.put(b"apple".to_vec(), b"red".to_vec());
        eng.flush(&dir.path().join("1.sst")).unwrap();

        eng.remove(b"apple");
        eng.flush(&dir.path().join("2.sst")).unwrap();
        assert_eq!(eng.get(b"apple"), None);

        eng.put(b"apple".to_vec(), b"green".to_vec());
        eng.flush(&dir.path().join("3.sst")).unwrap();
        assert_eq!(eng.get(b"apple"), Some(b"green".as_slice()));
    }

    #[test]
    fn tombstone_does_not_affect_other_keys() {
        let mut eng = crate::engine::Engine::new();
        let dir = tempfile::tempdir().unwrap();

        eng.put(b"apple".to_vec(), b"red".to_vec());
        eng.put(b"banana".to_vec(), b"yellow".to_vec());
        eng.flush(&dir.path().join("1.sst")).unwrap();

        eng.remove(b"apple");
        eng.put(b"cherry".to_vec(), b"dark red".to_vec());
        eng.flush(&dir.path().join("2.sst")).unwrap();

        assert_eq!(eng.get(b"apple"), None);
        assert_eq!(eng.get(b"banana"), Some(b"yellow".as_slice()));
        assert_eq!(eng.get(b"cherry"), Some(b"dark red".as_slice()));
    }

    #[test]
    fn empty_value_is_not_a_delete() {
        let mut eng = crate::engine::Engine::new();
        let dir = tempfile::tempdir().unwrap();

        eng.put(b"apple".to_vec(), vec![]);
        assert_eq!(eng.get(b"apple"), Some(b"".as_slice()));
        eng.flush(&dir.path().join("1.sst")).unwrap();
        assert_eq!(eng.get(b"apple"), Some(b"".as_slice()));
    }

    #[test]
    fn failed_flush_keeps_memtable() {
        let mut eng = crate::engine::Engine::new();
        let dir = tempfile::tempdir().unwrap();
        let fp = dir.path().join("1.sst");

        eng.put(b"apple".to_vec(), b"red".to_vec());
        eng.flush(&fp).unwrap();

        eng.remove(b"apple");
        eng.put(b"banana".to_vec(), b"yellow".to_vec());
        let err = eng.flush(&fp).unwrap_err();
        assert!(
            matches!(&err, crate::engine::FlushError::Write(crate::sstable::TableWriteError::Io(e))
                if e.kind() == std::io::ErrorKind::AlreadyExists),
            "got {err:?}"
        );

        assert_eq!(eng.get(b"apple"), None);
        assert_eq!(eng.get(b"banana"), Some(b"yellow".as_slice()));
    }
    #[test]
    fn untouched_key_in_oldest_table_is_found() {
        let mut eng = crate::engine::Engine::new();
        let dir = tempfile::tempdir().unwrap();

        eng.put(b"apple".to_vec(), b"red".to_vec());
        eng.put(b"banana".to_vec(), b"yellow".to_vec());
        eng.flush(&dir.path().join("1.sst")).unwrap();

        eng.remove(b"apple");
        eng.put(b"cherry".to_vec(), b"dark red".to_vec());
        eng.flush(&dir.path().join("2.sst")).unwrap();

        eng.put(b"apple".to_vec(), b"green".to_vec());
        eng.put(b"durian".to_vec(), b"spiky".to_vec());
        eng.flush(&dir.path().join("3.sst")).unwrap();

        assert_eq!(eng.get(b"banana"), Some(b"yellow".as_slice()));
        assert_eq!(eng.get(b"cherry"), Some(b"dark red".as_slice()));
        assert_eq!(eng.get(b"apple"), Some(b"green".as_slice()));
    }

    #[test]
    fn missing_key_across_several_tables_returns_none() {
        let mut eng = crate::engine::Engine::new();
        let dir = tempfile::tempdir().unwrap();

        eng.put(b"apple".to_vec(), b"red".to_vec());
        eng.put(b"cherry".to_vec(), b"dark red".to_vec());
        eng.flush(&dir.path().join("1.sst")).unwrap();

        eng.put(b"banana".to_vec(), b"yellow".to_vec());
        eng.remove(b"durian");
        eng.flush(&dir.path().join("2.sst")).unwrap();

        eng.put(b"elderberry".to_vec(), b"purple".to_vec());

        for key in [b"".as_slice(), b"aardvark", b"blueberry", b"date", b"zzz"] {
            assert_eq!(eng.get(key), None, "key {key:?} should be absent");
        }
    }

    #[test]
    fn empty_and_arbitrary_byte_values_keep_newest_write() {
        let mut eng = crate::engine::Engine::new();
        let dir = tempfile::tempdir().unwrap();

        eng.put(b"apple".to_vec(), b"red".to_vec());
        eng.flush(&dir.path().join("1.sst")).unwrap();

        eng.put(b"apple".to_vec(), vec![]);
        eng.flush(&dir.path().join("2.sst")).unwrap();
        assert_eq!(eng.get(b"apple"), Some(b"".as_slice()));

        eng.put(b"apple".to_vec(), vec![0x00, 0xff, 0x80]);
        eng.flush(&dir.path().join("3.sst")).unwrap();
        assert_eq!(eng.get(b"apple"), Some([0x00, 0xff, 0x80].as_slice()));

        eng.put(b"apple".to_vec(), vec![]);
        assert_eq!(eng.get(b"apple"), Some(b"".as_slice()));
    }

    #[test]
    fn empty_flush_succeeds_and_preserves_reads() {
        let mut eng = crate::engine::Engine::new();
        let dir = tempfile::tempdir().unwrap();

        eng.flush(&dir.path().join("1.sst")).unwrap();
        assert_eq!(eng.get(b"apple"), None);

        eng.put(b"apple".to_vec(), b"red".to_vec());
        eng.flush(&dir.path().join("2.sst")).unwrap();

        eng.flush(&dir.path().join("3.sst")).unwrap();
        assert_eq!(eng.get(b"apple"), Some(b"red".as_slice()));
    }

    #[test]
    fn failed_file_creation_keeps_memtable() {
        let mut eng = crate::engine::Engine::new();
        let dir = tempfile::tempdir().unwrap();

        eng.put(b"apple".to_vec(), b"red".to_vec());
        eng.remove(b"banana");
        let err = eng
            .flush(&dir.path().join("missing_dir").join("1.sst"))
            .unwrap_err();
        assert!(
            matches!(&err, crate::engine::FlushError::Write(crate::sstable::TableWriteError::Io(e))
                if e.kind() == std::io::ErrorKind::NotFound),
            "got {err:?}"
        );

        assert_eq!(eng.get(b"apple"), Some(b"red".as_slice()));
        assert_eq!(eng.get(b"banana"), None);

        eng.flush(&dir.path().join("1.sst")).unwrap();
        assert_eq!(eng.get(b"apple"), Some(b"red".as_slice()));
    }
}
