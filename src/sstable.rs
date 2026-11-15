//! Writes a [`MemTable`](crate::MemTable) to disk as an SSTable (Sorted String
//! Table) file.
//!
//! A file holding three entries looks like this:
//!
//! ```text
//! offset: 0                   18                     38                 55
//!         ┌───────────────────┬──────────────────────┬──────────────────┐
//!         │ apple → green     │ banana → yellow      │ cherry → red     │
//!         │ 8 + 5 + 5 = 18 B  │ 8 + 6 + 6 = 20 B     │ 8 + 6 + 3 = 17 B │
//!         └───────────────────┴──────────────────────┴──────────────────┘
//! ```
//!
//! note:
//! atm, there is no index or footer, so finding a key means reading records from
//! the start of the file
//!
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
    Io(std::io::Error),
    Decode(crate::record::DecodeError),
    InvalidKeyOrder,
}

pub(crate) struct SsTable {
    records: Vec<crate::record::Record>,
}

impl SsTable {
    pub(crate) fn open(path: &std::path::Path) -> Result<Self, TableReadError> {
        let file_contents = std::fs::read(&path).map_err(TableReadError::Io)?;
        let mut raw_file_contents: &[u8] = file_contents.as_slice();

        let mut decoded: Vec<crate::record::Record> = Vec::new();

        while let Some((record, size)) =
            crate::record::decode(raw_file_contents).map_err(TableReadError::Decode)?
        {
            if let Some(last) = decoded.last()
                && record.key <= last.key
            {
                return Err(TableReadError::InvalidKeyOrder);
            }
            decoded.push(record);
            raw_file_contents = &raw_file_contents[size..];
        }

        Ok(SsTable { records: decoded })
    }

    pub(crate) fn get(&self, key: &[u8]) -> Option<&[u8]> {
        for rec in &self.records {
            if rec.key == key {
                return Some(rec.value.as_slice());
            } else if rec.key.as_slice() > key {
                break;
            }
        }
        None
    }
}

/// Writes every key-value pair of `table`, into a new file at
/// `path`.
///
/// ```ignore
/// let mut table = MemTable::new();
/// table.put(b"cherry".to_vec(), b"red".to_vec());
/// table.put(b"apple".to_vec(), b"green".to_vec());
///
/// write_new(Path::new("data/000001.sst"), &table)?;
/// // New file contains: [apple -> green][cherry -> red]
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
    use crate::MemTable;

    use super::*;
    use std::fs;
    use std::io::ErrorKind;
    use tempfile::tempdir;

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

        // write all of these into the file
        write_new(&fp, &table).unwrap();

        let bytes = fs::read(&fp).unwrap();
        let mut remaining: &[u8] = &bytes;
        let mut decoded = Vec::new();

        // keep pulling out (record, size)'s
        // push records into the decoded vec
        // move the slice forward by jumping `size` distance
        while let Some((record, size)) = crate::record::decode(remaining).unwrap() {
            decoded.push(record);
            remaining = &remaining[size..];
        }

        assert_eq!(
            decoded,
            vec![
                crate::record::Record {
                    key: b"apple".to_vec(),
                    value: b"green".to_vec(),
                },
                crate::record::Record {
                    key: b"banana".to_vec(),
                    value: b"yellow".to_vec(),
                },
                crate::record::Record {
                    key: b"cherry".to_vec(),
                    value: b"red".to_vec(),
                },
            ]
        );

        assert!(remaining.is_empty());
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

        write_new(&fp, &table).unwrap();
        let remaining_entries: Vec<(&[u8], &[u8])> = table.iter().collect();
        assert_eq!(
            remaining_entries,
            vec![
                (b"apple".as_slice(), b"red".as_slice()),
                (b"grape".as_slice(), b"purple".as_slice()),
            ]
        );
    }

    // fn empty_table_test()
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
        assert_eq!(sstable.get(b"apple"), Some(b"red".as_slice()));
        assert_eq!(sstable.get(b"banana"), Some(b"yellow".as_slice()));
        assert_eq!(sstable.get(b"grape"), Some(b"purple".as_slice()));
        assert_eq!(sstable.get(b"0000"), None);
    }
}
