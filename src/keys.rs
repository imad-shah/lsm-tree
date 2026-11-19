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
mod test {
    #[test]
    fn test_sorting() {
        let mut values = [
            crate::keys::InternalKey {
                user_key: b"banana".to_vec(),
                seq: 3,
            },
            crate::keys::InternalKey {
                user_key: b"apple".to_vec(),
                seq: 5,
            },
            crate::keys::InternalKey {
                user_key: b"apple".to_vec(),
                seq: 3,
            },
            crate::keys::InternalKey {
                user_key: b"apple".to_vec(),
                seq: 8,
            },
        ];
        values.sort();
        assert_eq!(
            values,
            [
                crate::keys::InternalKey {
                    user_key: b"apple".to_vec(),
                    seq: 8,
                },
                crate::keys::InternalKey {
                    user_key: b"apple".to_vec(),
                    seq: 5,
                },
                crate::keys::InternalKey {
                    user_key: b"apple".to_vec(),
                    seq: 3,
                },
                crate::keys::InternalKey {
                    user_key: b"banana".to_vec(),
                    seq: 3,
                },
            ]
        );
    }

    #[test]
    fn test_prefix_sorting() {
        let mut value = [
            crate::keys::InternalKey {
                user_key: b"ab".to_vec(),
                seq: 9,
            },
            crate::keys::InternalKey {
                user_key: b"a".to_vec(),
                seq: 1,
            },
        ];
        value.sort();
        assert_eq!(
            value,
            [
                crate::keys::InternalKey {
                    user_key: b"a".to_vec(),
                    seq: 1,
                },
                crate::keys::InternalKey {
                    user_key: b"ab".to_vec(),
                    seq: 9,
                },
            ]
        );
    }
}
