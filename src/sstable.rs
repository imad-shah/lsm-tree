//! Writes a [`MemTable`](crate::memtable::MemTable) to disk as an SSTable (Sorted String
//! Table) file, and reads it back with [`SsTable::open`].
//!
//! The file is just records (see [`record`](crate::record)) back to back, in
//! ascending key order. Deleted keys are written as tombstone records so they
//! keep shadowing older SSTables. A file holding three entries, the last one a
//! tombstone, looks like this:
//!
//! ```text
//! offset: 0                   19                     40                 55
//!         ┌───────────────────┬──────────────────────┬──────────────────┐
//!         │ apple → green     │ banana → yellow      │ cherry → tb      │
//!         │ 9 + 5 + 5 = 19 B  │ 9 + 6 + 6 = 21 B     │ 9 + 6 + 0 = 15 B │
//!         └───────────────────┴──────────────────────┴──────────────────┘
//! ```
//! note:
//! atm, there is no index or footer, so finding a key means reading records from
//! the start of the file (WIP)

use std::io::Write;

#[derive(Debug)]
pub(crate) enum TableWriteError {
    /// A key or value was longer than `u32::MAX` bytes, so its length doesn't fit in
    /// the record header. Carries the original conversion error.
    Encode(std::num::TryFromIntError),
    /// The file couldn't be created or written. Check `.kind()` on the inner error:
    /// `ErrorKind::AlreadyExists` means a file was already at that path.
    Io(std::io::Error),
}

#[derive(Debug)]
pub(crate) enum TableReadError {
    /// The file couldn't be read
    Io(std::io::Error),
    /// A record in the file is malformed (truncated, unknown kind, bad tombstone, etc)
    Decode(crate::record::DecodeError),
    /// A key is not strictly greater than the one before it
    InvalidKeyOrder,
}

/// An SSTable loaded fully into memory, records in ascending key order.
pub(crate) struct SsTable {
    records: Vec<crate::record::Record>,
}

impl SsTable {
    /// Reads and decodes every record in the file at `path`.
    ///
    /// # Errors
    ///
    /// - [`TableReadError::Io`] if the file can't be read
    /// - [`TableReadError::Decode`] if any record is malformed
    /// - [`TableReadError::InvalidKeyOrder`] if keys aren't strictly increasing
    pub(crate) fn open(path: &std::path::Path) -> Result<Self, TableReadError> {
        let file_contents = std::fs::read(path).map_err(TableReadError::Io)?;
        let decoded = crate::record::decode_all(&file_contents).map_err(TableReadError::Decode)?;

        for i in 1..decoded.len() {
            if decoded[i].key <= decoded[i - 1].key {
                return Err(TableReadError::InvalidKeyOrder);
            }
        }

        Ok(SsTable { records: decoded })
    }

    /// Looks up `key` in this table.
    ///
    /// - `Some(Entry::Value(_))`: the key is live in this table
    /// - `Some(Entry::Tombstone)`: the key was deleted; callers must stop
    ///   searching older tables
    /// - `None`: this table has no record for the key
    pub(crate) fn get(&self, key: &[u8]) -> Option<&crate::entry::Entry> {
        self.records
            .binary_search_by(|r| r.key.as_slice().cmp(key))
            .ok()
            .map(|i| &self.records[i].value)
    }

    /// Iterates over every entry in ascending key order
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&[u8], &crate::entry::Entry)> {
        self.records.iter().map(|r| (r.key.as_slice(), &r.value))
    }
}

/// Writes every entry of `table`, tombstones included, into a new file at
/// `path`.
///
/// ```ignore
/// let mut table = MemTable::new();
/// table.put(b"cherry".to_vec(), b"red".to_vec());
/// table.put(b"apple".to_vec(), b"green".to_vec());
/// table.remove(b"banana");
///
/// write_new(Path::new("data/000001.sst"), &table)?;
/// // New file contains: [apple -> green][banana -> tombstone][cherry -> red]
/// ```
///
/// # Errors
///
/// - [`TableWriteError::Io`] with kind `AlreadyExists` if a file is already at
///   `path`
/// - [`TableWriteError::Io`] for any other failure to open or write the file
///   (missing directory, no permission, disk full, etc)
/// - [`TableWriteError::Encode`] if a key or value is longer than `u32::MAX` bytes
pub(crate) fn write_new(
    path: &std::path::Path,
    table: &crate::memtable::MemTable,
) -> Result<(), TableWriteError> {
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(TableWriteError::Io)?;

    for (key, value) in table.iter() {
        let encoded = crate::record::encode(key, value).map_err(TableWriteError::Encode)?;
        f.write_all(encoded.as_slice())
            .map_err(TableWriteError::Io)?;
    }
    Ok(())
}

#[cfg(test)]
mod test {
    use crate::memtable::MemTable;

    use super::*;
    use crate::entry::Entry;
    use crate::record::{DecodeError, Record};
    use std::fs;
    use std::io::ErrorKind;
    use tempfile::tempdir;

    // helper so expected values are readable
    fn val(bytes: &[u8]) -> Entry {
        Entry::Value(bytes.to_vec())
    }

    fn encode_all(records: &[(&[u8], Entry)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (k, v) in records {
            out.extend(crate::record::encode(k, v).unwrap());
        }
        out
    }

    #[test]
    fn several_entries_sorted_and_consumed() {
        let dir = tempdir().unwrap();
        let fp = dir.path().join("sstable.sst");

        // insert k-v pairs into the memtable out of order,
        // proving it sorts it using the b tree
        let mut table = crate::memtable::MemTable::new();
        table.put(b"cherry".to_vec(), b"red".to_vec());
        table.put(b"apple".to_vec(), b"green".to_vec());
        table.put(b"banana".to_vec(), b"yellow".to_vec());
        table.remove(b"blueberry");

        // write all of these into the file
        write_new(&fp, &table).unwrap();

        let bytes = fs::read(&fp).unwrap();
        let decoded = crate::record::decode_all(&bytes).unwrap();

        assert_eq!(
            decoded,
            vec![
                Record {
                    key: b"apple".to_vec(),
                    value: val(b"green"),
                },
                Record {
                    key: b"banana".to_vec(),
                    value: val(b"yellow"),
                },
                Record {
                    key: b"blueberry".to_vec(),
                    value: Entry::Tombstone,
                },
                Record {
                    key: b"cherry".to_vec(),
                    value: val(b"red"),
                },
            ]
        );
    }

    #[test]
    fn file_size_matches_module_doc_example() {
        let dir = tempdir().unwrap();
        let fp = dir.path().join("doc.sst");

        let mut table = MemTable::new();
        table.put(b"apple".to_vec(), b"green".to_vec());
        table.put(b"banana".to_vec(), b"yellow".to_vec());
        table.remove(b"cherry");
        write_new(&fp, &table).unwrap();

        assert_eq!(fs::read(&fp).unwrap().len(), 55);
    }

    #[test]
    fn existing_destination_fails_without_overwriting() {
        let dir = tempdir().unwrap();
        let fp = dir.path().join("existing.bin");

        let original_contents = b"already contains contents";
        fs::write(&fp, original_contents).unwrap();

        let mut table = crate::memtable::MemTable::new();
        table.put(b"apple".to_vec(), b"banana".to_vec());

        let err = write_new(&fp, &table).unwrap_err();
        match err {
            TableWriteError::Io(io_err) => assert_eq!(io_err.kind(), ErrorKind::AlreadyExists),
            other => panic!("expected TableWriteError::Io(AlreadyExists), got {other:?}"),
        }
        let on_disk = fs::read(&fp).unwrap();
        assert_eq!(on_disk, original_contents);
    }

    #[test]
    fn memtable_preserved_after_write() {
        let dir = tempdir().unwrap();
        let fp = dir.path().join("preserved.bin");

        let mut table = crate::memtable::MemTable::new();
        table.put(b"apple".to_vec(), b"red".to_vec());
        table.put(b"grape".to_vec(), b"purple".to_vec());
        table.remove(b"kiwi");

        write_new(&fp, &table).unwrap();
        let remaining_entries: Vec<(&[u8], &Entry)> = table.iter().collect();
        let (red, purple) = (val(b"red"), val(b"purple"));
        assert_eq!(
            remaining_entries,
            vec![
                (b"apple".as_slice(), &red),
                (b"grape".as_slice(), &purple),
                (b"kiwi".as_slice(), &Entry::Tombstone),
            ]
        );
    }

    #[test]
    fn empty_table_writes_empty_file_that_opens() {
        let dir = tempdir().unwrap();
        let fp = dir.path().join("empty.sst");

        write_new(&fp, &MemTable::new()).unwrap();
        assert!(fs::read(&fp).unwrap().is_empty());

        let sstable = SsTable::open(&fp).unwrap();
        assert_eq!(sstable.get(b"anything"), None);
    }

    #[test]
    fn round_trip_test_open_and_get() {
        let mut table = MemTable::new();
        table.put(b"banana".to_vec(), b"yellow".to_vec());
        table.put(b"apple".to_vec(), b"red".to_vec());
        table.put(b"grape".to_vec(), b"purple".to_vec());

        let dir = tempdir().unwrap();
        let fp = dir.path().join("sstable.sst");
        write_new(&fp, &table).unwrap();

        let sstable = SsTable::open(&fp).unwrap();
        assert_eq!(sstable.get(b"apple"), Some(&val(b"red")));
        assert_eq!(sstable.get(b"banana"), Some(&val(b"yellow")));
        assert_eq!(sstable.get(b"grape"), Some(&val(b"purple")));
        assert_eq!(sstable.get(b"0000"), None);
        assert_eq!(sstable.get(b"zzz"), None);
        assert_eq!(sstable.get(b"blueberry"), None);
    }

    #[test]
    fn tombstones_round_trip_through_file() {
        let mut table = MemTable::new();
        table.put(b"apple".to_vec(), b"red".to_vec());
        table.remove(b"banana");
        table.put(b"cherry".to_vec(), b"".to_vec());

        let dir = tempdir().unwrap();
        let fp = dir.path().join("tomb.sst");
        write_new(&fp, &table).unwrap();

        let sstable = SsTable::open(&fp).unwrap();
        assert_eq!(sstable.get(b"apple"), Some(&val(b"red")));
        assert_eq!(sstable.get(b"banana"), Some(&Entry::Tombstone));
        assert_eq!(sstable.get(b"cherry"), Some(&val(b"")));
        assert_eq!(sstable.get(b"durian"), None);
    }

    #[test]
    fn open_missing_file_is_io_error() {
        let dir = tempdir().unwrap();
        let err = SsTable::open(&dir.path().join("nope.sst")).err().unwrap();
        assert!(
            matches!(&err, TableReadError::Io(e) if e.kind() == ErrorKind::NotFound),
            "got {err:?}"
        );
    }

    #[test]
    fn open_rejects_out_of_order_keys() {
        let dir = tempdir().unwrap();
        let fp = dir.path().join("unordered.sst");
        fs::write(
            &fp,
            encode_all(&[(b"banana", val(b"yellow")), (b"apple", val(b"red"))]),
        )
        .unwrap();

        let err = SsTable::open(&fp).err().unwrap();
        assert!(
            matches!(err, TableReadError::InvalidKeyOrder),
            "got {err:?}"
        );
    }

    #[test]
    fn open_rejects_duplicate_keys() {
        let dir = tempdir().unwrap();
        let fp = dir.path().join("dup.sst");
        fs::write(
            &fp,
            encode_all(&[(b"apple", val(b"red")), (b"apple", Entry::Tombstone)]),
        )
        .unwrap();

        let err = SsTable::open(&fp).err().unwrap();
        assert!(
            matches!(err, TableReadError::InvalidKeyOrder),
            "got {err:?}"
        );
    }

    #[test]
    fn open_rejects_unknown_kind() {
        let dir = tempdir().unwrap();
        let fp = dir.path().join("badkind.sst");
        let mut bytes = encode_all(&[(b"apple", val(b"red"))]);
        bytes[0] = 0x05;
        fs::write(&fp, bytes).unwrap();

        let err = SsTable::open(&fp).err().unwrap();
        assert!(
            matches!(err, TableReadError::Decode(DecodeError::UnknownKind(0x05))),
            "got {err:?}"
        );
    }

    #[test]
    fn open_rejects_truncated_file() {
        let dir = tempdir().unwrap();
        let fp = dir.path().join("truncated.sst");
        let mut bytes = encode_all(&[(b"apple", val(b"red")), (b"banana", Entry::Tombstone)]);
        bytes.pop();
        fs::write(&fp, bytes).unwrap();

        let err = SsTable::open(&fp).err().unwrap();
        assert!(
            matches!(err, TableReadError::Decode(DecodeError::TruncatedPayload)),
            "got {err:?}"
        );
    }
    #[test]
    fn get_finds_first_middle_and_last_keys() {
        let mut table = MemTable::new();
        table.put(b"apple".to_vec(), b"red".to_vec());
        table.put(b"banana".to_vec(), b"yellow".to_vec());
        table.put(b"cherry".to_vec(), b"dark red".to_vec());
        table.put(b"durian".to_vec(), b"spiky".to_vec());
        table.put(b"elderberry".to_vec(), b"purple".to_vec());
        table.put(b"fig".to_vec(), b"brown".to_vec());
        table.put(b"grape".to_vec(), b"green".to_vec());

        let dir = tempdir().unwrap();
        let fp = dir.path().join("search.sst");
        write_new(&fp, &table).unwrap();
        let sstable = SsTable::open(&fp).unwrap();

        assert_eq!(sstable.get(b"apple"), Some(&val(b"red")));
        assert_eq!(sstable.get(b"grape"), Some(&val(b"green")));
        assert_eq!(sstable.get(b"durian"), Some(&val(b"spiky")));

        for (key, entry) in table.iter() {
            assert_eq!(sstable.get(key), Some(entry), "key {key:?} should be found");
        }
    }

    #[test]
    fn get_missing_keys_before_between_and_after() {
        let mut table = MemTable::new();
        table.put(b"b".to_vec(), b"1".to_vec());
        table.put(b"d".to_vec(), b"2".to_vec());
        table.put(b"f".to_vec(), b"3".to_vec());
        table.put(b"h".to_vec(), b"4".to_vec());
        table.put(b"j".to_vec(), b"5".to_vec());

        let dir = tempdir().unwrap();
        let fp = dir.path().join("gaps.sst");
        write_new(&fp, &table).unwrap();
        let sstable = SsTable::open(&fp).unwrap();

        assert_eq!(sstable.get(b""), None);
        assert_eq!(sstable.get(b"a"), None);
        for key in [b"c", b"e", b"g", b"i"] {
            assert_eq!(sstable.get(key), None, "key {key:?} should be absent");
        }
        assert_eq!(sstable.get(b"bb"), None);
        assert_eq!(sstable.get(b"d\x00"), None);
        assert_eq!(sstable.get(b"k"), None);
        assert_eq!(sstable.get(&[0xff]), None);
    }

    #[test]
    fn get_distinguishes_tombstone_empty_value_and_absence() {
        let mut table = MemTable::new();
        table.remove(b"apple");
        table.put(b"banana".to_vec(), b"yellow".to_vec());
        table.put(b"cherry".to_vec(), b"".to_vec());
        table.put(b"durian".to_vec(), b"spiky".to_vec());
        table.remove(b"elderberry");

        let dir = tempdir().unwrap();
        let fp = dir.path().join("three_way.sst");
        write_new(&fp, &table).unwrap();
        let sstable = SsTable::open(&fp).unwrap();

        assert_eq!(sstable.get(b"apple"), Some(&Entry::Tombstone));
        assert_eq!(sstable.get(b"elderberry"), Some(&Entry::Tombstone));
        assert_eq!(sstable.get(b"cherry"), Some(&val(b"")));
        assert_ne!(sstable.get(b"cherry"), Some(&Entry::Tombstone));
        assert_eq!(sstable.get(b"banana"), Some(&val(b"yellow")));
        assert_eq!(sstable.get(b"durian"), Some(&val(b"spiky")));
        assert_eq!(sstable.get(b"blueberry"), None);
        assert_eq!(sstable.get(b"fig"), None);
    }

    #[test]
    fn empty_and_non_utf8_keys_and_values_round_trip() {
        let mut table = MemTable::new();
        table.put(b"".to_vec(), b"empty key".to_vec());
        table.put(vec![0x00], vec![0xff, 0x00]);
        table.put(b"apple".to_vec(), b"".to_vec());
        table.put(vec![0x80, 0x81], vec![0xc3, 0x28]);
        table.put(vec![0xff], vec![]);

        let dir = tempdir().unwrap();
        let fp = dir.path().join("bytes.sst");
        write_new(&fp, &table).unwrap();
        let sstable = SsTable::open(&fp).unwrap();

        assert_eq!(sstable.get(b""), Some(&val(b"empty key")));
        assert_eq!(sstable.get(&[0x00]), Some(&val(&[0xff, 0x00])));
        assert_eq!(sstable.get(b"apple"), Some(&val(b"")));
        assert_eq!(sstable.get(&[0x80, 0x81]), Some(&val(&[0xc3, 0x28])));
        assert_eq!(sstable.get(&[0xff]), Some(&val(b"")));
        assert_eq!(sstable.get(&[0x80]), None);
    }

    #[test]
    fn sstable_independent_of_source_memtable() {
        let mut table = MemTable::new();
        table.put(b"apple".to_vec(), b"red".to_vec());
        table.put(b"banana".to_vec(), b"yellow".to_vec());

        let dir = tempdir().unwrap();
        let fp = dir.path().join("independent.sst");
        write_new(&fp, &table).unwrap();
        let sstable = SsTable::open(&fp).unwrap();

        table.put(b"apple".to_vec(), b"green".to_vec());
        table.remove(b"banana");
        table.put(b"cherry".to_vec(), b"dark red".to_vec());
        drop(table);

        assert_eq!(sstable.get(b"apple"), Some(&val(b"red")));
        assert_eq!(sstable.get(b"banana"), Some(&val(b"yellow")));
        assert_eq!(sstable.get(b"cherry"), None);
    }

    #[test]
    fn open_rejects_order_violation_after_valid_prefix() {
        let dir = tempdir().unwrap();
        let descending: &[(&[u8], Entry)] = &[
            (b"apple", val(b"red")),
            (b"banana", val(b"yellow")),
            (b"cherry", val(b"dark red")),
            (b"blueberry", val(b"blue")),
            (b"durian", val(b"spiky")),
        ];
        let duplicate: &[(&[u8], Entry)] = &[
            (b"apple", val(b"red")),
            (b"banana", val(b"yellow")),
            (b"cherry", val(b"dark red")),
            (b"cherry", Entry::Tombstone),
            (b"durian", val(b"spiky")),
        ];

        for (name, records) in [("descending", descending), ("duplicate", duplicate)] {
            let fp = dir.path().join(format!("{name}.sst"));
            fs::write(&fp, encode_all(records)).unwrap();

            let err = SsTable::open(&fp).err().unwrap();
            assert!(
                matches!(err, TableReadError::InvalidKeyOrder),
                "{name}: got {err:?}"
            );
        }
    }

    #[test]
    fn open_rejects_truncated_header_after_valid_record() {
        let dir = tempdir().unwrap();
        let fp = dir.path().join("truncated_header.sst");
        let mut bytes = encode_all(&[(b"apple", val(b"red"))]);
        bytes.extend_from_slice(&[0x00, 0x05, 0x00, 0x00]);
        fs::write(&fp, bytes).unwrap();

        let err = SsTable::open(&fp).err().unwrap();
        assert!(
            matches!(err, TableReadError::Decode(DecodeError::TruncatedHeader)),
            "got {err:?}"
        );
    }
}
