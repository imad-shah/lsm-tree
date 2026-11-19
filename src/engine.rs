//! LSM-tree storage engine coordinating an in-memory [`MemTable`] and on-disk
//! [`SsTable`] files with MVCC snapshot reads and full compaction.
//!
//! Writes receive monotonically increasing sequence numbers (`1, 2, ...`) and
//! land in the active [`MemTable`]. [`Engine::flush`] writes the active memtable
//! to a new [`SsTable`] on disk, while [`Engine::compact_all`] merges all
//! on-disk tables and garbage-collects versions shadowed at or below
//! `oldest_live`.

use crate::entry::Entry;
use crate::keys::SeqNum;
use crate::memtable::MemTable;
use crate::merge::{MergeIterator, Source};
use crate::sstable::{self, SsTable, TableReadError, TableWriteError};
use crate::visible::Visible;
use std::fs;
use std::io;
use std::mem;
use std::path::{Path, PathBuf};

/// An open on-disk [`SsTable`] paired with its filesystem path.
pub(crate) struct TableHandle {
    path: PathBuf,
    table: SsTable,
}

/// Errors returned by [`Engine::compact_all`].
#[derive(Debug)]
pub(crate) enum CompactionError {
    /// Writing the compacted output table failed.
    Write(TableWriteError),
    /// Opening the newly written output table failed.
    Read(TableReadError),
}

/// Result of [`Engine::compact_all`], recording any old SSTable files that
/// could not be deleted after the replacement table was installed.
#[derive(Debug)]
pub(crate) struct CompactionOutcome {
    cleanup_failures: Vec<(PathBuf, io::Error)>,
}

/// Coordinates the active [`MemTable`], flushed [`SsTable`]s, and sequence
/// number assignment.
pub(crate) struct Engine {
    memtable: MemTable,
    sstables: Vec<TableHandle>,
    seq: SeqNum,
}

/// Errors returned by [`Engine::flush`].
#[derive(Debug)]
pub(crate) enum FlushError {
    /// Writing the new SSTable file failed.
    Write(TableWriteError),
    /// Opening the newly flushed SSTable file failed.
    Read(TableReadError),
}

impl Engine {
    /// Creates an empty [`Engine`] at sequence number `0`.
    pub(crate) fn new() -> Self {
        Self {
            memtable: MemTable::new(),
            sstables: vec![],
            seq: 0,
        }
    }

    /// Inserts or updates `key` with `value` at the next sequence number.
    pub(crate) fn put(&mut self, key: Vec<u8>, value: Vec<u8>) {
        self.seq += 1;
        self.memtable.put(key, self.seq, value);
    }

    /// Looks up the latest live value for `key`.
    ///
    /// Searches the active [`MemTable`] first, then flushed [`SsTable`]s from
    /// newest to oldest, stopping at the first value or [`Entry::Tombstone`].
    pub(crate) fn get(&self, key: &[u8]) -> Option<&[u8]> {
        self.get_at(key, self.seq)
    }

    /// Looks up the live value for `key` visible at snapshot `read_seq`.
    pub(crate) fn get_at(&self, key: &[u8], read_seq: SeqNum) -> Option<&[u8]> {
        match self.memtable.get(key, read_seq) {
            Some(Entry::Value(bytes)) => return Some(bytes.as_slice()),
            Some(Entry::Tombstone) => return None,
            None => {}
        };

        for sst in self.sstables.iter().rev() {
            match sst.table.get(key, read_seq) {
                Some(Entry::Value(bytes)) => return Some(bytes.as_slice()),
                Some(Entry::Tombstone) => return None,
                None => {}
            };
        }
        None
    }

    /// Deletes `key` by writing an [`Entry::Tombstone`] at the next sequence number.
    pub(crate) fn remove(&mut self, key: &[u8]) {
        self.seq += 1;
        self.memtable.remove(key, self.seq);
    }

    /// Flushes the active [`MemTable`] to a new SSTable at `path` and resets
    /// the active memtable.
    ///
    /// # Errors
    ///
    /// - [`FlushError::Write`] if creating or writing `path` fails
    /// - [`FlushError::Read`] if reopening the written SSTable fails
    pub(crate) fn flush(&mut self, path: &Path) -> Result<(), FlushError> {
        sstable::write_new(path, self.memtable.iter()).map_err(FlushError::Write)?;
        let table = SsTable::open(path).map_err(FlushError::Read)?;
        self.sstables.push(TableHandle {
            path: path.to_path_buf(),
            table,
        });
        self.memtable = MemTable::new();
        Ok(())
    }

    /// Iterates over all live `(key, value)` pairs visible at the latest sequence number.
    pub(crate) fn scan(&self) -> impl Iterator<Item = (&[u8], &[u8])> + '_ {
        Visible::new(self.merged_entries(), self.seq)
    }

    /// Iterates over all live `(key, value)` pairs visible at snapshot `read_seq`.
    pub(crate) fn scan_at(&self, read_seq: SeqNum) -> impl Iterator<Item = (&[u8], &[u8])> + '_ {
        Visible::new(self.merged_entries(), read_seq)
    }

    /// Merges all on-disk [`SsTable`]s into a single compacted table at `output_path`
    /// and removes the old SSTable files.
    ///
    /// Retention rules per user key (newest to oldest):
    /// - Keeps every version with `seq > oldest_live`.
    /// - Keeps the newest version with `seq <= oldest_live` if it is a live value,
    ///   or drops it if it is a tombstone.
    /// - Drops all older versions.
    ///
    /// # Errors
    ///
    /// - [`CompactionError::Write`] if creating or writing `output_path` fails
    /// - [`CompactionError::Read`] if reopening the compacted SSTable fails
    pub(crate) fn compact_all(
        &mut self,
        output_path: &Path,
        oldest_live: SeqNum,
    ) -> Result<CompactionOutcome, CompactionError> {
        if self.sstables.is_empty() {
            return Ok(CompactionOutcome {
                cleanup_failures: vec![],
            });
        }
        let mut res = CompactionOutcome {
            cleanup_failures: vec![],
        };
        let mut last_kept_below_seq: Option<Vec<u8>> = None;
        let gc_iter = self.merged_sstables().filter(move |(ikey, entry)| {
            if ikey.seq > oldest_live {
                return true;
            }

            if last_kept_below_seq.as_deref() == Some(ikey.user_key.as_slice()) {
                return false;
            }

            last_kept_below_seq = Some(ikey.user_key.clone());

            !matches!(entry, Entry::Tombstone)
        });
        sstable::write_new(output_path, gc_iter).map_err(CompactionError::Write)?;
        let replacement = SsTable::open(output_path).map_err(CompactionError::Read)?;
        let replacement_handle = TableHandle {
            path: output_path.to_path_buf(),
            table: replacement,
        };
        let old_handles = mem::replace(&mut self.sstables, vec![replacement_handle]);

        for sstable in old_handles {
            match fs::remove_file(&sstable.path) {
                Ok(()) => {}
                Err(e) => {
                    res.cleanup_failures.push((sstable.path.clone(), e));
                }
            }
        }

        Ok(res)
    }
    /// Merges the active [`MemTable`] and all on-disk [`SsTable`]s in [`InternalKey`](crate::keys::InternalKey) order.
    fn merged_entries(&self) -> MergeIterator<'_> {
        let mut sources: Vec<Source> = Vec::new();
        sources.push(Box::new(self.memtable.iter()));

        for sst in self.sstables.iter().rev() {
            sources.push(Box::new(sst.table.iter()));
        }
        MergeIterator::new(sources)
    }

    /// Merges only the on-disk [`SsTable`]s in [`InternalKey`](crate::keys::InternalKey) order.
    fn merged_sstables(&self) -> MergeIterator<'_> {
        let mut sources: Vec<Source> = Vec::new();
        for sst in self.sstables.iter().rev() {
            sources.push(Box::new(sst.table.iter()));
        }
        MergeIterator::new(sources)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::keys::InternalKey;
    use std::io::ErrorKind;
    use tempfile::tempdir;

    #[test]
    fn seq_test() {
        let mut eng = Engine::new();

        eng.put(b"apple".to_vec(), b"red".to_vec());
        eng.put(b"apple".to_vec(), b"green".to_vec());

        assert_eq!(eng.get(b"apple".as_slice()), Some(b"green".as_slice()));
        assert_eq!(eng.get_at(b"apple".as_slice(), 1), Some(b"red".as_slice()));
    }

    #[test]
    fn one_table_no_drop_sucs() {
        let mut eng = Engine::new();
        let dir = tempdir().unwrap();
        let fp1 = dir.path().join("first.sst");
        let fp2 = dir.path().join("compacted.sst");

        eng.put(b"apple".to_vec(), b"red".to_vec());
        eng.put(b"banana".to_vec(), b"yellow".to_vec());
        eng.flush(&fp1).unwrap();
        let outcome = eng.compact_all(&fp2, 2).unwrap();

        assert!(outcome.cleanup_failures.is_empty());
        assert_eq!(eng.sstables.len(), 1);
        let got: Vec<(&[u8], SeqNum, &Entry)> = eng.sstables[0]
            .table
            .iter()
            .map(|(k, e)| (k.user_key.as_slice(), k.seq, e))
            .collect();
        assert_eq!(
            got,
            vec![
                (b"apple".as_slice(), 1, &Entry::Value(b"red".to_vec())),
                (b"banana".as_slice(), 2, &Entry::Value(b"yellow".to_vec())),
            ]
        );
    }

    #[test]
    fn full_mvcc_test() {
        // apple@3 -> red
        // apple@7 -> green
        // apple@9 -> tombstone
        // compact all with time 8, should hold 1 above (apple@9) and 1 below (apple@7)
        //
        // compact all no time given, should hold nothing because (apple@9) is most recent and
        // deleted apple
        let mut eng = Engine::new();
        let dir = tempdir().unwrap();
        let fp1 = dir.path().join("first.sst");
        let fp2 = dir.path().join("second.sst");
        let fp3 = dir.path().join("third.sst");
        let fp4 = dir.path().join("compacted.sst");

        eng.put(b"foo".to_vec(), b"bar".to_vec()); // 1
        eng.put(b"foo".to_vec(), b"bar".to_vec()); // 2
        eng.put(b"apple".to_vec(), b"red".to_vec()); // apple@3 -> red
        eng.flush(&fp1).unwrap(); // flushed {apple@3 -> red} to sstable1

        eng.put(b"foo".to_vec(), b"bar".to_vec()); // 4
        eng.put(b"foo".to_vec(), b"bar".to_vec()); // 5
        eng.put(b"foo".to_vec(), b"bar".to_vec()); // 6
        eng.put(b"apple".to_vec(), b"green".to_vec()); // apple@7 -> green
        eng.flush(&fp2).unwrap(); // flushed {apple@7 -> green} to sstable2

        eng.put(b"foo".to_vec(), b"bar".to_vec()); // 8
        eng.remove(b"apple"); // apple@9 -> tombstone
        eng.flush(&fp3).unwrap(); // flushed {apple@9 -> tomb} to sstable3

        // memtable is empty, 3 sstables with different values
        let oldest_alive = 8;
        let outcome = eng.compact_all(&fp4, oldest_alive).unwrap();

        // 1. verify the inmemory representation
        assert!(outcome.cleanup_failures.is_empty());
        assert_eq!(eng.sstables.len(), 1);
        let got: Vec<(&[u8], SeqNum, &Entry)> = eng.sstables[0]
            .table
            .iter()
            .filter(|(k, _)| k.user_key == b"apple")
            .map(|(k, e)| (k.user_key.as_slice(), k.seq, e))
            .collect();
        assert_eq!(
            got,
            vec![
                (b"apple".as_slice(), 9, &Entry::Tombstone),
                (b"apple".as_slice(), 7, &Entry::Value(b"green".to_vec())),
            ]
        );

        // 2. verify the actual file got wrote to
        let table = SsTable::open(&fp4).unwrap();
        let got: Vec<(&[u8], SeqNum, &Entry)> = table
            .iter()
            .filter(|(k, _)| k.user_key == b"apple")
            .map(|(k, e)| (k.user_key.as_slice(), k.seq, e))
            .collect();
        assert_eq!(
            got,
            vec![
                (b"apple".as_slice(), 9, &Entry::Tombstone),
                (b"apple".as_slice(), 7, &Entry::Value(b"green".to_vec())),
            ]
        );
    }

    #[test]
    fn integration_test() {
        let mut eng = Engine::new();
        let dir = tempdir().unwrap();
        let fp1 = dir.path().join("first.sst");
        let fp2 = dir.path().join("second.sst");

        eng.put(b"apple".to_vec(), b"red".to_vec());
        assert_eq!(eng.get(b"apple"), Some(b"red".as_slice()));
        eng.flush(&fp1).unwrap();
        assert_eq!(eng.get(b"apple"), Some(b"red".as_slice()));

        eng.remove(b"apple");
        assert_eq!(eng.get(b"apple"), None);

        eng.flush(&fp2).unwrap();
        assert_eq!(eng.get(b"apple"), None);

        eng.put(b"apple".to_vec(), b"green".to_vec());
        assert_eq!(eng.get(b"apple"), Some(b"green".as_slice()));
    }

    #[test]
    fn integration_with_scan() {
        let mut eng = Engine::new();
        let dir = tempdir().unwrap();
        let fp1 = dir.path().join("first.sst");
        let fp2 = dir.path().join("second.sst");

        eng.put(b"apple".to_vec(), b"red".to_vec());
        eng.put(b"banana".to_vec(), b"yellow".to_vec());
        eng.put(b"pear".to_vec(), b"green".to_vec());
        eng.flush(&fp1).unwrap();

        eng.remove(b"apple");
        eng.put(b"cherry".to_vec(), b"red".to_vec());
        eng.flush(&fp2).unwrap();

        eng.put(b"banana".to_vec(), b"blue".to_vec());
        // current state:
        // memtable { (banana, 6): blue }
        // sstable2 { (apple, 4): tomb, (cherry, 5): red }
        // sstable1 { (apple, 1): red, (banana, 2): yellow, (pear, 3): green }
        let res: Vec<_> = eng.scan().collect();
        assert_eq!(
            res,
            vec![
                (b"banana".as_slice(), b"blue".as_slice()),
                (b"cherry".as_slice(), b"red".as_slice()),
                (b"pear".as_slice(), b"green".as_slice()),
            ]
        );
    }

    #[test]
    fn snapshot_reads_with_get_at_and_scan_at() {
        let mut eng = Engine::new();
        let dir = tempdir().unwrap();
        let fp1 = dir.path().join("first.sst");
        let fp2 = dir.path().join("second.sst");

        eng.put(b"apple".to_vec(), b"red".to_vec()); // seq 1
        eng.put(b"banana".to_vec(), b"yellow".to_vec()); // seq 2
        eng.flush(&fp1).unwrap();

        eng.remove(b"apple"); // seq 3
        eng.put(b"cherry".to_vec(), b"dark red".to_vec()); // seq 4
        eng.flush(&fp2).unwrap();

        eng.put(b"apple".to_vec(), b"green".to_vec()); // seq 5
        eng.put(b"banana".to_vec(), b"brown".to_vec()); // seq 6

        assert_eq!(eng.get_at(b"apple", 0), None);
        assert_eq!(eng.get_at(b"apple", 1), Some(b"red".as_slice()));
        assert_eq!(eng.get_at(b"apple", 2), Some(b"red".as_slice()));
        assert_eq!(eng.get_at(b"apple", 3), None);
        assert_eq!(eng.get_at(b"apple", 4), None);
        assert_eq!(eng.get_at(b"apple", 5), Some(b"green".as_slice()));

        assert_eq!(eng.get_at(b"banana", 2), Some(b"yellow".as_slice()));
        assert_eq!(eng.get_at(b"banana", 6), Some(b"brown".as_slice()));
        assert_eq!(eng.get_at(b"cherry", 3), None);
        assert_eq!(eng.get_at(b"cherry", 4), Some(b"dark red".as_slice()));

        let snap_2: Vec<_> = eng.scan_at(2).collect();
        assert_eq!(
            snap_2,
            vec![
                (b"apple".as_slice(), b"red".as_slice()),
                (b"banana".as_slice(), b"yellow".as_slice()),
            ]
        );

        let snap_4: Vec<_> = eng.scan_at(4).collect();
        assert_eq!(
            snap_4,
            vec![
                (b"banana".as_slice(), b"yellow".as_slice()),
                (b"cherry".as_slice(), b"dark red".as_slice()),
            ]
        );
    }

    #[test]
    fn integration_test_with_compact_all_drops_obsolete_versions_and_tombstones() {
        let mut eng = Engine::new();
        let dir = tempdir().unwrap();
        let fp1 = dir.path().join("first.sst");
        let fp2 = dir.path().join("second.sst");
        let fp3 = dir.path().join("compacted.sst");

        eng.put(b"apple".to_vec(), b"red".to_vec()); // seq 1
        eng.put(b"banana".to_vec(), b"yellow".to_vec()); // seq 2
        eng.put(b"pear".to_vec(), b"green".to_vec()); // seq 3
        eng.flush(&fp1).unwrap();

        eng.remove(b"apple"); // seq 4
        eng.put(b"cherry".to_vec(), b"red".to_vec()); // seq 5
        eng.flush(&fp2).unwrap();

        eng.put(b"banana".to_vec(), b"blue".to_vec()); // seq 6 (still in memtable)

        // With oldest_live = 5, all SSTable records have seq <= 5:
        // - apple's newest SSTable record is tombstone (seq 4 <= 5), so apple is dropped completely
        // - banana (seq 2), cherry (seq 5), and pear (seq 3) survive
        let outcome = eng.compact_all(&fp3, 5).unwrap();
        assert!(outcome.cleanup_failures.is_empty());
        assert_eq!(eng.sstables.len(), 1);
        assert_eq!(eng.sstables[0].path, fp3);

        assert!(fp3.exists());
        assert!(!fp1.exists());
        assert!(!fp2.exists());

        let replacement = &eng.sstables[0].table;
        let stored: Vec<_> = replacement.iter().collect();
        assert_eq!(
            stored,
            vec![
                (
                    &InternalKey {
                        user_key: b"banana".to_vec(),
                        seq: 2
                    },
                    &Entry::Value(b"yellow".to_vec()),
                ),
                (
                    &InternalKey {
                        user_key: b"cherry".to_vec(),
                        seq: 5
                    },
                    &Entry::Value(b"red".to_vec()),
                ),
                (
                    &InternalKey {
                        user_key: b"pear".to_vec(),
                        seq: 3
                    },
                    &Entry::Value(b"green".to_vec()),
                ),
            ]
        );

        // Memtable entry (banana, 6) still shadows compacted (banana, 2)
        assert_eq!(eng.get(b"banana"), Some(b"blue".as_slice()));
        assert_eq!(eng.get_at(b"banana", 5), Some(b"yellow".as_slice()));
        assert_eq!(eng.get(b"apple"), None);
    }

    #[test]
    fn compact_all_preserves_versions_above_oldest_live_for_active_snapshots() {
        let mut eng = Engine::new();
        let dir = tempdir().unwrap();
        let fp1 = dir.path().join("first.sst");
        let fp2 = dir.path().join("second.sst");
        let fp3 = dir.path().join("compacted.sst");

        eng.put(b"apple".to_vec(), b"v1".to_vec()); // seq 1
        eng.flush(&fp1).unwrap();

        eng.put(b"apple".to_vec(), b"v2".to_vec()); // seq 2
        eng.remove(b"apple"); // seq 3
        eng.flush(&fp2).unwrap();

        // Snapshot at seq 1 is still active (oldest_live = 1):
        // - (apple, 3) tombstone and (apple, 2) v2 have seq > 1 -> kept
        // - (apple, 1) v1 is first version <= 1 -> kept
        let outcome = eng.compact_all(&fp3, 1).unwrap();
        assert!(outcome.cleanup_failures.is_empty());

        assert_eq!(eng.get_at(b"apple", 1), Some(b"v1".as_slice()));
        assert_eq!(eng.get_at(b"apple", 2), Some(b"v2".as_slice()));
        assert_eq!(eng.get_at(b"apple", 3), None);
    }

    #[test]
    fn get_missing_key_returns_none() {
        let mut eng = Engine::new();
        let dir = tempdir().unwrap();
        assert_eq!(eng.get(b"ghost"), None);

        eng.put(b"apple".to_vec(), b"red".to_vec());
        eng.flush(&dir.path().join("1.sst")).unwrap();
        assert_eq!(eng.get(b"ghost"), None);
    }

    #[test]
    fn newest_sstable_wins_across_value_tombstone_value() {
        let mut eng = Engine::new();
        let dir = tempdir().unwrap();

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
        let mut eng = Engine::new();
        let dir = tempdir().unwrap();

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
        let mut eng = Engine::new();
        let dir = tempdir().unwrap();

        eng.put(b"apple".to_vec(), vec![]);
        assert_eq!(eng.get(b"apple"), Some(b"".as_slice()));
        eng.flush(&dir.path().join("1.sst")).unwrap();
        assert_eq!(eng.get(b"apple"), Some(b"".as_slice()));
    }

    #[test]
    fn failed_flush_keeps_memtable() {
        let mut eng = Engine::new();
        let dir = tempdir().unwrap();
        let fp = dir.path().join("1.sst");

        eng.put(b"apple".to_vec(), b"red".to_vec());
        eng.flush(&fp).unwrap();

        eng.remove(b"apple");
        eng.put(b"banana".to_vec(), b"yellow".to_vec());
        let err = eng.flush(&fp).unwrap_err();
        assert!(
            matches!(&err, FlushError::Write(TableWriteError::Io(e))
                if e.kind() == ErrorKind::AlreadyExists),
            "got {err:?}"
        );

        assert_eq!(eng.get(b"apple"), None);
        assert_eq!(eng.get(b"banana"), Some(b"yellow".as_slice()));
    }

    #[test]
    fn untouched_key_in_oldest_table_is_found() {
        let mut eng = Engine::new();
        let dir = tempdir().unwrap();

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
        let mut eng = Engine::new();
        let dir = tempdir().unwrap();

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
        let mut eng = Engine::new();
        let dir = tempdir().unwrap();

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
        let mut eng = Engine::new();
        let dir = tempdir().unwrap();

        eng.flush(&dir.path().join("1.sst")).unwrap();
        assert_eq!(eng.get(b"apple"), None);

        eng.put(b"apple".to_vec(), b"red".to_vec());
        eng.flush(&dir.path().join("2.sst")).unwrap();

        eng.flush(&dir.path().join("3.sst")).unwrap();
        assert_eq!(eng.get(b"apple"), Some(b"red".as_slice()));
    }

    #[test]
    fn failed_file_creation_keeps_memtable() {
        let mut eng = Engine::new();
        let dir = tempdir().unwrap();

        eng.put(b"apple".to_vec(), b"red".to_vec());
        eng.remove(b"banana");
        let err = eng
            .flush(&dir.path().join("missing_dir").join("1.sst"))
            .unwrap_err();
        assert!(
            matches!(&err, FlushError::Write(TableWriteError::Io(e))
                if e.kind() == ErrorKind::NotFound),
            "got {err:?}"
        );

        assert_eq!(eng.get(b"apple"), Some(b"red".as_slice()));
        assert_eq!(eng.get(b"banana"), None);

        eng.flush(&dir.path().join("1.sst")).unwrap();
        assert_eq!(eng.get(b"apple"), Some(b"red".as_slice()));
    }
}
