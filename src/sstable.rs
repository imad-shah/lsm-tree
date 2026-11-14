use std::io::Write;

#[derive(Debug)]
pub(crate) enum TableWriteError {
    Encode(std::num::TryFromIntError),
    Io(std::io::Error),
}

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
}
