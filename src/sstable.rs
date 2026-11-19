//! Writes a [`MemTable`](crate::memtable::Memtable) to disk as an SSTable (Sorted String
//! Table) file, and reads it back with [`SsTable::open`].
//!
//! The file is just records (see [`record`]) back to back, in
//! ascending [`InternalKey`] order (`user_key` ascending,
//! `seq` descending). Deleted keys are written as tombstone records so they
//! keep shadowing older SSTables. A file holding three entries, the last one a
//! tombstone, looks like this:
//!
//! ```text
//! offset: 0                   27                     56                 79
//!         ┌───────────────────┬──────────────────────┬──────────────────┐
//!         │ apple -> green    │ banana -> yellow     │ cherry -> tb     │
//!         │ 17 + 5 + 5 = 27 B │ 17 + 6 + 6 = 29 B    │ 17 + 6 + 0 = 23B │
//!         └───────────────────┴──────────────────────┴──────────────────┘
//! ```
//! note:
//! atm, there is no index or footer, so finding a key means reading records from
//! the start of the file (WIP)

use crate::entry::Entry;
use crate::keys::{InternalKey, SeqNum};
use crate::record::{self, DecodeError, Record};
use std::cmp::Ordering;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::num::TryFromIntError;
use std::path::Path;

#[derive(Debug)]
pub(crate) enum TableWriteError {
    /// A key or value was longer than `u32::MAX` bytes, so its length doesn't fit in
    /// the record header. Carries the original conversion error.
    Encode(TryFromIntError),
    /// The file couldn't be created or written. Check `.kind()` on the inner error:
    /// `ErrorKind::AlreadyExists` means a file was already at that path.
    Io(io::Error),
}

#[derive(Debug)]
pub(crate) enum TableReadError {
    /// The file couldn't be read
    Io(io::Error),
    /// A record in the file is malformed (truncated, unknown kind, bad tombstone, etc)
    Decode(DecodeError),
    /// A key is not strictly greater than the one before it
    InvalidKeyOrder,
}

/// An SSTable loaded fully into memory, records in ascending [`InternalKey`] order.
pub(crate) struct SsTable {
    records: Vec<Record>,
}

impl SsTable {
    /// Reads and decodes every record in the file at `path`.
    ///
    /// # Errors
    ///
    /// - [`TableReadError::Io`] if the file can't be read
    /// - [`TableReadError::Decode`] if any record is malformed
    /// - [`TableReadError::InvalidKeyOrder`] if internal keys aren't strictly increasing
    pub(crate) fn open(path: &Path) -> Result<Self, TableReadError> {
        let file_contents = fs::read(path).map_err(TableReadError::Io)?;
        let decoded = record::decode_all(&file_contents).map_err(TableReadError::Decode)?;

        for i in 1..decoded.len() {
            if decoded[i].key <= decoded[i - 1].key {
                return Err(TableReadError::InvalidKeyOrder);
            }
        }

        Ok(SsTable { records: decoded })
    }

    /// Looks up `key` at snapshot `read_seq` in this table.
    ///
    /// - `Some(Entry::Value(_))`: the key is live at `read_seq` in this table
    /// - `Some(Entry::Tombstone)`: the key was deleted; callers must stop
    ///   searching older tables
    /// - `None`: this table has no record for `key` with `seq <= read_seq`
    pub(crate) fn get(&self, key: &[u8], read_seq: SeqNum) -> Option<&Entry> {
        let i = self
            .records
            .partition_point(|r| match r.key.user_key.as_slice().cmp(key) {
                Ordering::Less => true,
                Ordering::Equal => r.key.seq > read_seq,
                Ordering::Greater => false,
            });

        self.records
            .get(i)
            .filter(|r| r.key.user_key == key)
            .map(|r| &r.value)
    }

    /// Iterates over every entry in ascending [`InternalKey`] order
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&InternalKey, &Entry)> {
        self.records.iter().map(|r| (&r.key, &r.value))
    }
}

/// Writes every entry of `table`, tombstones included, into a new file at
/// `path`.
///
/// ```ignore
/// let mut table = crate::memtable::MemTable::new();
/// table.put(b"cherry".to_vec(), 1, b"red".to_vec());
/// table.put(b"apple".to_vec(), 2, b"green".to_vec());
/// table.remove(b"banana", 3);
///
/// write_new(Path::new("data/000001.sst"), table.iter())?;
/// // New file contains: [(apple, 2) -> green][(banana, 3) -> tombstone][(cherry, 1) -> red]
/// ```
///
/// # Errors
///
/// - [`TableWriteError::Io`] with kind `AlreadyExists` if a file is already at
///   `path`
/// - [`TableWriteError::Io`] for any other failure to open or write the file
///   (missing directory, no permission, disk full, etc)
/// - [`TableWriteError::Encode`] if a key or value is longer than `u32::MAX` bytes
pub(crate) fn write_new<'a>(
    path: &Path,
    entries: impl Iterator<Item = (&'a InternalKey, &'a Entry)>,
) -> Result<(), TableWriteError> {
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(TableWriteError::Io)?;

    for (key, value) in entries {
        let encoded = record::encode(key, value).map_err(TableWriteError::Encode)?;
        f.write_all(encoded.as_slice())
            .map_err(TableWriteError::Io)?;
    }
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::memtable::MemTable;
    use std::io::ErrorKind;
    use tempfile::tempdir;

    fn val(bytes: &[u8]) -> Entry {
        Entry::Value(bytes.to_vec())
    }

    fn ikey(bytes: &[u8], seq: SeqNum) -> InternalKey {
        InternalKey {
            user_key: bytes.to_vec(),
            seq,
        }
    }

    fn encode_all(records: &[(&[u8], SeqNum, Entry)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (k, seq, v) in records {
            out.extend(record::encode(&ikey(k, *seq), v).unwrap());
        }
        out
    }

    #[test]
    fn several_entries_sorted_and_consumed() {
        let dir = tempdir().unwrap();
        let fp = dir.path().join("sstable.sst");

        let mut table = MemTable::new();
        table.put(b"cherry".to_vec(), 1, b"red".to_vec());
        table.put(b"apple".to_vec(), 2, b"green".to_vec());
        table.put(b"banana".to_vec(), 3, b"yellow".to_vec());
        table.remove(b"blueberry", 4);

        write_new(&fp, table.iter()).unwrap();

        let bytes = fs::read(&fp).unwrap();
        let decoded = record::decode_all(&bytes).unwrap();

        assert_eq!(
            decoded,
            vec![
                Record {
                    key: ikey(b"apple", 2),
                    value: val(b"green"),
                },
                Record {
                    key: ikey(b"banana", 3),
                    value: val(b"yellow"),
                },
                Record {
                    key: ikey(b"blueberry", 4),
                    value: Entry::Tombstone,
                },
                Record {
                    key: ikey(b"cherry", 1),
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
        table.put(b"apple".to_vec(), 1, b"green".to_vec());
        table.put(b"banana".to_vec(), 2, b"yellow".to_vec());
        table.remove(b"cherry", 3);
        write_new(&fp, table.iter()).unwrap();

        // 27 + 29 + 23 = 79 bytes
        assert_eq!(fs::read(&fp).unwrap().len(), 79);
    }

    #[test]
    fn existing_destination_fails_without_overwriting() {
        let dir = tempdir().unwrap();
        let fp = dir.path().join("existing.bin");

        let original_contents = b"already contains contents";
        fs::write(&fp, original_contents).unwrap();

        let mut table = MemTable::new();
        table.put(b"apple".to_vec(), 1, b"banana".to_vec());

        let err = write_new(&fp, table.iter()).unwrap_err();
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

        let mut table = MemTable::new();
        table.put(b"apple".to_vec(), 1, b"red".to_vec());
        table.put(b"grape".to_vec(), 2, b"purple".to_vec());
        table.remove(b"kiwi", 3);

        write_new(&fp, table.iter()).unwrap();
        let remaining_entries: Vec<(&InternalKey, &Entry)> = table.iter().collect();
        let (k_apple, red) = (ikey(b"apple", 1), val(b"red"));
        let (k_grape, purple) = (ikey(b"grape", 2), val(b"purple"));
        let k_kiwi = ikey(b"kiwi", 3);
        assert_eq!(
            remaining_entries,
            vec![
                (&k_apple, &red),
                (&k_grape, &purple),
                (&k_kiwi, &Entry::Tombstone),
            ]
        );
    }

    #[test]
    fn empty_table_writes_empty_file_that_opens() {
        let dir = tempdir().unwrap();
        let fp = dir.path().join("empty.sst");

        write_new(&fp, MemTable::new().iter()).unwrap();
        assert!(fs::read(&fp).unwrap().is_empty());

        let sstable = SsTable::open(&fp).unwrap();
        assert_eq!(sstable.get(b"anything", 1), None);
        assert_eq!(sstable.iter().count(), 0);
    }

    #[test]
    fn round_trip_test_open_and_get() {
        let mut table = MemTable::new();
        table.put(b"banana".to_vec(), 1, b"yellow".to_vec());
        table.put(b"apple".to_vec(), 2, b"red".to_vec());
        table.put(b"grape".to_vec(), 3, b"purple".to_vec());

        let dir = tempdir().unwrap();
        let fp = dir.path().join("sstable.sst");
        write_new(&fp, table.iter()).unwrap();

        let sstable = SsTable::open(&fp).unwrap();
        assert_eq!(sstable.get(b"apple", 3), Some(&val(b"red")));
        assert_eq!(sstable.get(b"banana", 3), Some(&val(b"yellow")));
        assert_eq!(sstable.get(b"grape", 3), Some(&val(b"purple")));
        assert_eq!(sstable.get(b"0000", 3), None);
        assert_eq!(sstable.get(b"zzz", 3), None);
        assert_eq!(sstable.get(b"blueberry", 3), None);
    }

    #[test]
    fn get_respects_snapshot_read_seq_across_versions() {
        let mut table = MemTable::new();
        table.put(b"apple".to_vec(), 2, b"v2".to_vec());
        table.put(b"apple".to_vec(), 5, b"v5".to_vec());
        table.remove(b"apple", 8);
        table.put(b"banana".to_vec(), 4, b"yellow".to_vec());

        let dir = tempdir().unwrap();
        let fp = dir.path().join("mvcc.sst");
        write_new(&fp, table.iter()).unwrap();

        let sstable = SsTable::open(&fp).unwrap();
        assert_eq!(sstable.get(b"apple", 1), None);
        assert_eq!(sstable.get(b"apple", 2), Some(&val(b"v2")));
        assert_eq!(sstable.get(b"apple", 4), Some(&val(b"v2")));
        assert_eq!(sstable.get(b"apple", 5), Some(&val(b"v5")));
        assert_eq!(sstable.get(b"apple", 7), Some(&val(b"v5")));
        assert_eq!(sstable.get(b"apple", 8), Some(&Entry::Tombstone));
        assert_eq!(sstable.get(b"apple", 99), Some(&Entry::Tombstone));
    }

    #[test]
    fn tombstones_round_trip_through_file() {
        let mut table = MemTable::new();
        table.put(b"apple".to_vec(), 1, b"red".to_vec());
        table.remove(b"banana", 2);
        table.put(b"cherry".to_vec(), 3, b"".to_vec());

        let dir = tempdir().unwrap();
        let fp = dir.path().join("tomb.sst");
        write_new(&fp, table.iter()).unwrap();

        let sstable = SsTable::open(&fp).unwrap();
        assert_eq!(sstable.get(b"apple", 3), Some(&val(b"red")));
        assert_eq!(sstable.get(b"banana", 3), Some(&Entry::Tombstone));
        assert_eq!(sstable.get(b"cherry", 3), Some(&val(b"")));
        assert_eq!(sstable.get(b"durian", 3), None);
    }

    #[test]
    fn iter_yields_internal_keys_and_entries_in_order() {
        let mut table = MemTable::new();
        table.put(b"banana".to_vec(), 1, b"yellow".to_vec());
        table.put(b"apple".to_vec(), 2, b"v2".to_vec());
        table.put(b"apple".to_vec(), 5, b"v5".to_vec());

        let dir = tempdir().unwrap();
        let fp = dir.path().join("iter.sst");
        write_new(&fp, table.iter()).unwrap();

        let sstable = SsTable::open(&fp).unwrap();
        let items: Vec<_> = sstable.iter().collect();
        let (k_a5, v5) = (ikey(b"apple", 5), val(b"v5"));
        let (k_a2, v2) = (ikey(b"apple", 2), val(b"v2"));
        let (k_b1, vy) = (ikey(b"banana", 1), val(b"yellow"));
        assert_eq!(items, vec![(&k_a5, &v5), (&k_a2, &v2), (&k_b1, &vy)]);
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
            encode_all(&[(b"banana", 1, val(b"yellow")), (b"apple", 2, val(b"red"))]),
        )
        .unwrap();

        let err = SsTable::open(&fp).err().unwrap();
        assert!(
            matches!(err, TableReadError::InvalidKeyOrder),
            "got {err:?}"
        );
    }

    #[test]
    fn open_rejects_duplicate_internal_keys_and_ascending_seq() {
        let dir = tempdir().unwrap();

        let dup_fp = dir.path().join("dup.sst");
        fs::write(
            &dup_fp,
            encode_all(&[(b"apple", 5, val(b"red")), (b"apple", 5, Entry::Tombstone)]),
        )
        .unwrap();
        assert!(matches!(
            SsTable::open(&dup_fp).err().unwrap(),
            TableReadError::InvalidKeyOrder
        ));

        let asc_seq_fp = dir.path().join("asc_seq.sst");
        fs::write(
            &asc_seq_fp,
            encode_all(&[(b"apple", 2, val(b"older")), (b"apple", 5, val(b"newer"))]),
        )
        .unwrap();
        assert!(matches!(
            SsTable::open(&asc_seq_fp).err().unwrap(),
            TableReadError::InvalidKeyOrder
        ));
    }

    #[test]
    fn open_rejects_unknown_kind() {
        let dir = tempdir().unwrap();
        let fp = dir.path().join("badkind.sst");
        let mut bytes = encode_all(&[(b"apple", 1, val(b"red"))]);
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
        let mut bytes = encode_all(&[(b"apple", 1, val(b"red")), (b"banana", 2, Entry::Tombstone)]);
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
        table.put(b"apple".to_vec(), 1, b"red".to_vec());
        table.put(b"banana".to_vec(), 2, b"yellow".to_vec());
        table.put(b"cherry".to_vec(), 3, b"dark red".to_vec());
        table.put(b"durian".to_vec(), 4, b"spiky".to_vec());
        table.put(b"elderberry".to_vec(), 5, b"purple".to_vec());
        table.put(b"fig".to_vec(), 6, b"brown".to_vec());
        table.put(b"grape".to_vec(), 7, b"green".to_vec());

        let dir = tempdir().unwrap();
        let fp = dir.path().join("search.sst");
        write_new(&fp, table.iter()).unwrap();
        let sstable = SsTable::open(&fp).unwrap();

        assert_eq!(sstable.get(b"apple", 7), Some(&val(b"red")));
        assert_eq!(sstable.get(b"grape", 7), Some(&val(b"green")));
        assert_eq!(sstable.get(b"durian", 7), Some(&val(b"spiky")));

        for (key, entry) in table.iter() {
            assert_eq!(
                sstable.get(&key.user_key, key.seq),
                Some(entry),
                "key {key:?} should be found"
            );
        }
    }

    #[test]
    fn get_missing_keys_before_between_and_after() {
        let mut table = MemTable::new();
        table.put(b"b".to_vec(), 1, b"1".to_vec());
        table.put(b"d".to_vec(), 2, b"2".to_vec());
        table.put(b"f".to_vec(), 3, b"3".to_vec());
        table.put(b"h".to_vec(), 4, b"4".to_vec());
        table.put(b"j".to_vec(), 5, b"5".to_vec());

        let dir = tempdir().unwrap();
        let fp = dir.path().join("gaps.sst");
        write_new(&fp, table.iter()).unwrap();
        let sstable = SsTable::open(&fp).unwrap();

        assert_eq!(sstable.get(b"", 5), None);
        assert_eq!(sstable.get(b"a", 5), None);
        for key in [b"c", b"e", b"g", b"i"] {
            assert_eq!(sstable.get(key, 5), None, "key {key:?} should be absent");
        }
        assert_eq!(sstable.get(b"bb", 5), None);
        assert_eq!(sstable.get(b"d\x00", 5), None);
        assert_eq!(sstable.get(b"k", 5), None);
        assert_eq!(sstable.get(&[0xff], 5), None);
    }

    #[test]
    fn get_distinguishes_tombstone_empty_value_and_absence() {
        let mut table = MemTable::new();
        table.remove(b"apple", 1);
        table.put(b"banana".to_vec(), 2, b"yellow".to_vec());
        table.put(b"cherry".to_vec(), 3, b"".to_vec());
        table.put(b"durian".to_vec(), 4, b"spiky".to_vec());
        table.remove(b"elderberry", 5);

        let dir = tempdir().unwrap();
        let fp = dir.path().join("three_way.sst");
        write_new(&fp, table.iter()).unwrap();
        let sstable = SsTable::open(&fp).unwrap();

        assert_eq!(sstable.get(b"apple", 5), Some(&Entry::Tombstone));
        assert_eq!(sstable.get(b"elderberry", 5), Some(&Entry::Tombstone));
        assert_eq!(sstable.get(b"cherry", 5), Some(&val(b"")));
        assert_ne!(sstable.get(b"cherry", 5), Some(&Entry::Tombstone));
        assert_eq!(sstable.get(b"banana", 5), Some(&val(b"yellow")));
        assert_eq!(sstable.get(b"durian", 5), Some(&val(b"spiky")));
        assert_eq!(sstable.get(b"blueberry", 5), None);
        assert_eq!(sstable.get(b"fig", 5), None);
    }

    #[test]
    fn empty_and_non_utf8_keys_and_values_round_trip() {
        let mut table = MemTable::new();
        table.put(b"".to_vec(), 1, b"empty key".to_vec());
        table.put(vec![0x00], 2, vec![0xff, 0x00]);
        table.put(b"apple".to_vec(), 3, b"".to_vec());
        table.put(vec![0x80, 0x81], 4, vec![0xc3, 0x28]);
        table.put(vec![0xff], 5, vec![]);

        let dir = tempdir().unwrap();
        let fp = dir.path().join("bytes.sst");
        write_new(&fp, table.iter()).unwrap();
        let sstable = SsTable::open(&fp).unwrap();

        assert_eq!(sstable.get(b"", 5), Some(&val(b"empty key")));
        assert_eq!(sstable.get(&[0x00], 5), Some(&val(&[0xff, 0x00])));
        assert_eq!(sstable.get(b"apple", 5), Some(&val(b"")));
        assert_eq!(sstable.get(&[0x80, 0x81], 5), Some(&val(&[0xc3, 0x28])));
        assert_eq!(sstable.get(&[0xff], 5), Some(&val(b"")));
        assert_eq!(sstable.get(&[0x80], 5), None);
    }

    #[test]
    fn sstable_independent_of_source_memtable() {
        let mut table = MemTable::new();
        table.put(b"apple".to_vec(), 1, b"red".to_vec());
        table.put(b"banana".to_vec(), 2, b"yellow".to_vec());

        let dir = tempdir().unwrap();
        let fp = dir.path().join("independent.sst");
        write_new(&fp, table.iter()).unwrap();
        let sstable = SsTable::open(&fp).unwrap();

        table.put(b"apple".to_vec(), 3, b"green".to_vec());
        table.remove(b"banana", 4);
        table.put(b"cherry".to_vec(), 5, b"dark red".to_vec());
        drop(table);

        assert_eq!(sstable.get(b"apple", 5), Some(&val(b"red")));
        assert_eq!(sstable.get(b"banana", 5), Some(&val(b"yellow")));
        assert_eq!(sstable.get(b"cherry", 5), None);
    }

    #[test]
    fn open_rejects_order_violation_after_valid_prefix() {
        let dir = tempdir().unwrap();
        let descending: &[(&[u8], SeqNum, Entry)] = &[
            (b"apple", 1, val(b"red")),
            (b"banana", 2, val(b"yellow")),
            (b"cherry", 3, val(b"dark red")),
            (b"blueberry", 4, val(b"blue")),
            (b"durian", 5, val(b"spiky")),
        ];
        let duplicate: &[(&[u8], SeqNum, Entry)] = &[
            (b"apple", 1, val(b"red")),
            (b"banana", 2, val(b"yellow")),
            (b"cherry", 3, val(b"dark red")),
            (b"cherry", 3, Entry::Tombstone),
            (b"durian", 4, val(b"spiky")),
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
        let mut bytes = encode_all(&[(b"apple", 1, val(b"red"))]);
        bytes.extend_from_slice(&[0x00, 0x05, 0x00, 0x00]);
        fs::write(&fp, bytes).unwrap();

        let err = SsTable::open(&fp).err().unwrap();
        assert!(
            matches!(err, TableReadError::Decode(DecodeError::TruncatedHeader)),
            "got {err:?}"
        );
    }
}
