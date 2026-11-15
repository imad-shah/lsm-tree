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
        if let Some(v) = self.memtable.get(key) {
            return Some(v);
        }

        for sst in self.sstables.iter().rev() {
            if let Some(v) = sst.get(key) {
                return Some(v);
            }
        }
        None
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
        let value_3 = b"blue".as_slice();

        eng.put(key.to_vec(), value.to_vec());
        eng.flush(&fp1).unwrap();
        assert_eq!(eng.get(key), Some(value));

        eng.put(key.to_vec(), value_2.to_vec());
        assert_eq!(eng.get(key), Some(value_2));
        eng.flush(&fp2).unwrap();
        assert_eq!(eng.get(key), Some(value_2));

        eng.put(key.to_vec(), value_3.to_vec());
        assert_eq!(eng.get(key), Some(value_3));
    }
}
