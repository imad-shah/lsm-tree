//! K-way merge iterator over sorted [`InternalKey`] streams (such as a
//! [`MemTable`](crate::memtable::MemTable) and one or more
//! [`SsTable`](crate::sstable::SsTable)s).
//!
//! Because every write receives a distinct [`SeqNum`](crate::keys::SeqNum) and
//! [`InternalKey`] orders by `user_key` ascending then `seq` descending,
//! [`MergeIterator`] yields all versions across all sources in global sorted
//! order without deduplicating or dropping older versions, leaving snapshot
//! filtering and compaction deduplication to the caller.

use crate::entry::Entry;
use crate::keys::InternalKey;
use std::iter::Peekable;

/// A boxed, sorted stream of `(&InternalKey, &Entry)` pairs feeding into [`MergeIterator`].
pub(crate) type Source<'a> = Box<dyn Iterator<Item = (&'a InternalKey, &'a Entry)> + 'a>;

/// Merges multiple sorted [`Source`] iterators into a single stream ordered by
/// ascending [`InternalKey`] (`user_key` ascending, `seq` descending).
///
/// Source ordering in `sources` does not affect output order; each step inspects
/// the front of every non-empty source and advances whichever holds the smallest
/// [`InternalKey`].
pub(crate) struct MergeIterator<'a> {
    sources: Vec<Peekable<Source<'a>>>,
}

impl<'a> MergeIterator<'a> {
    /// Creates a [`MergeIterator`] from `sources`.
    ///
    /// Each source must already yield items in ascending [`InternalKey`] order.
    pub(crate) fn new(sources: Vec<Source<'a>>) -> Self {
        Self {
            sources: sources.into_iter().map(|s| s.peekable()).collect(),
        }
    }
}

impl<'a> Iterator for MergeIterator<'a> {
    type Item = (&'a InternalKey, &'a Entry);

    fn next(&mut self) -> Option<Self::Item> {
        let mut winner: Option<(usize, Self::Item)> = None;

        for (i, s) in self.sources.iter_mut().enumerate() {
            if let Some(&contender) = s.peek()
                && winner.is_none_or(|(_, w)| contender.0 < w.0)
            {
                winner = Some((i, contender));
            }
        }

        let (idx, _) = winner?;
        self.sources[idx].next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::SeqNum;
    use crate::memtable::MemTable;
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

    fn vec_source<'a>(items: &'a [(InternalKey, Entry)]) -> Source<'a> {
        Box::new(items.iter().map(|(k, v)| (k, v)))
    }

    #[test]
    fn empty_sources_list_yields_none() {
        let mut merge = MergeIterator::new(vec![]);
        assert_eq!(merge.next(), None);
        // calling next again after exhaustion remains None
        assert_eq!(merge.next(), None);
    }

    #[test]
    fn all_empty_iterators_yield_none() {
        let empty: Vec<(InternalKey, Entry)> = vec![];
        let mut merge = MergeIterator::new(vec![vec_source(&empty), vec_source(&empty)]);
        assert_eq!(merge.next(), None);
    }

    #[test]
    fn single_source_passes_through_unchanged() {
        let items = vec![
            (ikey(b"apple", 3), val(b"red")),
            (ikey(b"apple", 1), val(b"green")),
            (ikey(b"banana", 2), Entry::Tombstone),
        ];

        let merge = MergeIterator::new(vec![vec_source(&items)]);
        let merged: Vec<_> = merge.collect();
        let expected: Vec<_> = items.iter().map(|(k, v)| (k, v)).collect();

        assert_eq!(merged, expected);
    }

    #[test]
    fn merges_disjoint_and_overlapping_keys_by_internal_key_order() {
        let older_sst = vec![
            (ikey(b"apple", 1), val(b"green")),
            (ikey(b"cherry", 2), val(b"dark red")),
            (ikey(b"grape", 3), val(b"purple")),
        ];
        let newer_memtable = vec![
            (ikey(b"apple", 5), val(b"red")),
            (ikey(b"banana", 4), Entry::Tombstone),
            (ikey(b"cherry", 6), Entry::Tombstone),
        ];

        let merge = MergeIterator::new(vec![vec_source(&older_sst), vec_source(&newer_memtable)]);
        let merged: Vec<_> = merge.collect();

        let expected = vec![
            (&newer_memtable[0].0, &newer_memtable[0].1), // (apple, 5)
            (&older_sst[0].0, &older_sst[0].1),           // (apple, 1)
            (&newer_memtable[1].0, &newer_memtable[1].1), // (banana, 4)
            (&newer_memtable[2].0, &newer_memtable[2].1), // (cherry, 6)
            (&older_sst[1].0, &older_sst[1].1),           // (cherry, 2)
            (&older_sst[2].0, &older_sst[2].1),           // (grape, 3)
        ];
        assert_eq!(merged, expected);
    }

    #[test]
    fn source_order_does_not_affect_output_order() {
        let s1 = vec![(ikey(b"a", 10), val(b"v10")), (ikey(b"b", 3), val(b"v3"))];
        let s2 = vec![
            (ikey(b"a", 5), Entry::Tombstone),
            (ikey(b"c", 8), val(b"v8")),
        ];
        let s3 = vec![(ikey(b"a", 2), val(b"v2")), (ikey(b"b", 7), val(b"v7"))];

        let forward: Vec<_> =
            MergeIterator::new(vec![vec_source(&s1), vec_source(&s2), vec_source(&s3)]).collect();
        let reverse: Vec<_> =
            MergeIterator::new(vec![vec_source(&s3), vec_source(&s2), vec_source(&s1)]).collect();
        let interleaved: Vec<_> =
            MergeIterator::new(vec![vec_source(&s2), vec_source(&s1), vec_source(&s3)]).collect();

        assert_eq!(forward, reverse);
        assert_eq!(forward, interleaved);
    }

    #[test]
    fn handles_mix_of_empty_and_non_empty_sources() {
        let empty: Vec<(InternalKey, Entry)> = vec![];
        let non_empty = vec![
            (ikey(b"k1", 2), val(b"v2")),
            (ikey(b"k2", 1), Entry::Tombstone),
        ];

        let merged: Vec<_> = MergeIterator::new(vec![
            vec_source(&empty),
            vec_source(&non_empty),
            vec_source(&empty),
        ])
        .collect();

        let expected: Vec<_> = non_empty.iter().map(|(k, v)| (k, v)).collect();
        assert_eq!(merged, expected);
    }

    #[test]
    fn merges_memtable_and_sstable_sources() {
        let dir = tempdir().unwrap();
        let sst_path = dir.path().join("000001.sst");

        let mut flushed = MemTable::new();
        flushed.put(b"apple".to_vec(), 1, b"green".to_vec());
        flushed.put(b"banana".to_vec(), 2, b"yellow".to_vec());
        SsTable::write_new(&sst_path, flushed.iter()).unwrap();
        let sst = SsTable::open(&sst_path).unwrap();

        let mut active = MemTable::new();
        active.put(b"apple".to_vec(), 3, b"red".to_vec());
        active.remove(b"banana", 4);
        active.put(b"cherry".to_vec(), 5, b"dark red".to_vec());

        let merged: Vec<_> =
            MergeIterator::new(vec![Box::new(sst.iter()), Box::new(active.iter())]).collect();

        let (k_a3, v_red) = (ikey(b"apple", 3), val(b"red"));
        let (k_a1, v_green) = (ikey(b"apple", 1), val(b"green"));
        let (k_b4, k_b2, v_yellow) = (ikey(b"banana", 4), ikey(b"banana", 2), val(b"yellow"));
        let (k_c5, v_dark_red) = (ikey(b"cherry", 5), val(b"dark red"));

        assert_eq!(
            merged,
            vec![
                (&k_a3, &v_red),
                (&k_a1, &v_green),
                (&k_b4, &Entry::Tombstone),
                (&k_b2, &v_yellow),
                (&k_c5, &v_dark_red),
            ]
        );
    }
}
