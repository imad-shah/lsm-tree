//! Snapshot visibility filter over sorted [`InternalKey`] streams.
//!
//! Wraps a merged stream of versioned entries (such as [`MergeIterator`](crate::merge::MergeIterator))
//! and gives a view at a given `read_seq`:
//! - Ignores writes newer than `read_seq`.
//! - Deduplicates each `user_key` to its newest version at or before `read_seq`.
//! - Suppresses keys whose newest visible version is a [`Tombstone`](Entry::Tombstone)

use crate::entry::Entry;
use crate::keys::{InternalKey, SeqNum};

/// Wraps a sorted stream of `(&InternalKey, &Entry)` pairs and yields the
/// `(user_key, value)` pairs visible at `read_seq`.
///
/// The underlying iterator must yield items in ascending [`InternalKey`] order
/// (`user_key` ascending, `seq` descending).
pub(crate) struct Visible<'a, I> {
    iter: I,
    read_seq: SeqNum,
    last_user_key: Option<&'a [u8]>,
}

impl<'a, I> Visible<'a, I> {
    pub(crate) fn new(iter: I, read_seq: SeqNum) -> Self {
        Self {
            iter,
            read_seq,
            last_user_key: None,
        }
    }
}

impl<'a, I> Iterator for Visible<'a, I>
where
    I: Iterator<Item = (&'a InternalKey, &'a Entry)>,
{
    type Item = (&'a [u8], &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        while let Some((ikey, entry)) = self.iter.next() {
            if ikey.seq > self.read_seq || self.last_user_key == Some(ikey.user_key.as_slice()) {
                continue;
            }

            self.last_user_key = Some(ikey.user_key.as_slice());
            match entry {
                Entry::Value(v) => return Some((ikey.user_key.as_slice(), v.as_slice())),
                Entry::Tombstone => continue,
            }
        }
        None
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::memtable::MemTable;
    use crate::merge::MergeIterator;
    use crate::sstable::{self, SsTable};
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

    fn visible_from_slice<'a>(
        items: &'a [(InternalKey, Entry)],
        read_seq: SeqNum,
    ) -> Vec<(&'a [u8], &'a [u8])> {
        Visible::new(items.iter().map(|(k, v)| (k, v)), read_seq).collect()
    }

    #[test]
    fn empty_input_yields_none() {
        let items: Vec<(InternalKey, Entry)> = vec![];
        let mut vis = Visible::new(items.iter().map(|(k, v)| (k, v)), 10);
        assert_eq!(vis.next(), None);
        assert_eq!(vis.next(), None);
    }

    #[test]
    fn read_seq_zero_hides_positive_seq_writes() {
        let items = vec![
            (ikey(b"apple", 2), val(b"red")),
            (ikey(b"banana", 1), val(b"yellow")),
        ];
        assert!(visible_from_slice(&items, 0).is_empty());
    }

    #[test]
    fn skips_versions_newer_than_read_seq_and_returns_older_visible() {
        let items = vec![
            (ikey(b"apple", 9), Entry::Tombstone),
            (ikey(b"apple", 7), val(b"v7")),
            (ikey(b"apple", 4), val(b"v4")),
            (ikey(b"apple", 1), val(b"v1")),
        ];

        assert_eq!(
            visible_from_slice(&items, 5),
            vec![(b"apple".as_slice(), b"v4".as_slice())]
        );
        assert_eq!(
            visible_from_slice(&items, 7),
            vec![(b"apple".as_slice(), b"v7".as_slice())]
        );
        assert!(visible_from_slice(&items, 9).is_empty());
        assert!(visible_from_slice(&items, 0).is_empty());
    }

    #[test]
    fn tombstone_shadows_older_live_values_at_or_after_delete_seq() {
        let items = vec![
            (ikey(b"apple", 5), Entry::Tombstone),
            (ikey(b"apple", 3), val(b"old")),
            (ikey(b"banana", 2), val(b"yellow")),
        ];

        assert_eq!(
            visible_from_slice(&items, 3),
            vec![
                (b"apple".as_slice(), b"old".as_slice()),
                (b"banana".as_slice(), b"yellow".as_slice()),
            ]
        );
        assert_eq!(
            visible_from_slice(&items, 5),
            vec![(b"banana".as_slice(), b"yellow".as_slice())]
        );
    }

    #[test]
    fn put_after_tombstone_resurrects_key_at_newer_snapshot() {
        let items = vec![
            (ikey(b"apple", 8), val(b"reborn")),
            (ikey(b"apple", 5), Entry::Tombstone),
            (ikey(b"apple", 2), val(b"initial")),
        ];

        assert_eq!(
            visible_from_slice(&items, 2),
            vec![(b"apple".as_slice(), b"initial".as_slice())]
        );
        assert!(visible_from_slice(&items, 6).is_empty());
        assert_eq!(
            visible_from_slice(&items, 8),
            vec![(b"apple".as_slice(), b"reborn".as_slice())]
        );
    }

    #[test]
    fn all_tombstones_yields_empty() {
        let items = vec![
            (ikey(b"a", 3), Entry::Tombstone),
            (ikey(b"b", 2), Entry::Tombstone),
            (ikey(b"c", 1), Entry::Tombstone),
        ];
        assert!(visible_from_slice(&items, 10).is_empty());
    }

    #[test]
    fn preserves_empty_keys_and_empty_values() {
        let items = vec![
            (ikey(b"", 2), val(b"")),
            (ikey(b"", 1), val(b"non-empty")),
            (ikey(b"k", 3), val(b"")),
        ];

        assert_eq!(
            visible_from_slice(&items, 3),
            vec![
                (b"".as_slice(), b"".as_slice()),
                (b"k".as_slice(), b"".as_slice()),
            ]
        );
    }

    #[test]
    fn e2e_with_merge_iterator_memtable_and_sstable() {
        let dir = tempdir().unwrap();
        let sst_path = dir.path().join("000001.sst");

        let mut flushed = MemTable::new();
        flushed.put(b"apple".to_vec(), 1, b"green".to_vec());
        flushed.put(b"banana".to_vec(), 2, b"yellow".to_vec());
        flushed.put(b"durian".to_vec(), 3, b"spiky".to_vec());
        sstable::write_new(&sst_path, flushed.iter()).unwrap();
        let sst = SsTable::open(&sst_path).unwrap();

        let mut active = MemTable::new();
        active.put(b"apple".to_vec(), 4, b"red".to_vec());
        active.remove(b"banana", 5);
        active.put(b"cherry".to_vec(), 6, b"dark red".to_vec());

        let snapshot_3: Vec<_> = Visible::new(
            MergeIterator::new(vec![Box::new(sst.iter()), Box::new(active.iter())]),
            3,
        )
        .collect();
        assert_eq!(
            snapshot_3,
            vec![
                (b"apple".as_slice(), b"green".as_slice()),
                (b"banana".as_slice(), b"yellow".as_slice()),
                (b"durian".as_slice(), b"spiky".as_slice()),
            ]
        );

        let snapshot_6: Vec<_> = Visible::new(
            MergeIterator::new(vec![Box::new(sst.iter()), Box::new(active.iter())]),
            6,
        )
        .collect();
        assert_eq!(
            snapshot_6,
            vec![
                (b"apple".as_slice(), b"red".as_slice()),
                (b"cherry".as_slice(), b"dark red".as_slice()),
                (b"durian".as_slice(), b"spiky".as_slice()),
            ]
        );
    }
}
