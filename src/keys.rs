use std::cmp::Ordering;
pub(crate) type SeqNum = u64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InternalKey {
    pub(crate) user_key: Vec<u8>,
    pub(crate) seq: SeqNum,
}

impl Ord for InternalKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.user_key
            .cmp(&other.user_key)
            .then_with(|| other.seq.cmp(&self.seq))
    }
}

impl PartialOrd for InternalKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use crate::test_util::ikey;

    #[test]
    fn orders_by_user_key_asc_then_seq_desc() {
        let mut values = [
            ikey(b"banana", 3),
            ikey(b"apple", 5),
            ikey(b"apple", 3),
            ikey(b"apple", 8),
        ];
        values.sort();
        assert_eq!(
            values,
            [
                ikey(b"apple", 8),
                ikey(b"apple", 5),
                ikey(b"apple", 3),
                ikey(b"banana", 3),
            ]
        );
    }

    #[test]
    fn shorter_prefix_sorts_first_regardless_of_seq() {
        let mut values = [ikey(b"ab", 9), ikey(b"a", 1)];
        values.sort();
        assert_eq!(values, [ikey(b"a", 1), ikey(b"ab", 9)]);
    }
}
