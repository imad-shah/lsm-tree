//! Small helpers shared by the unit tests

use crate::entry::Entry;
use crate::keys::{InternalKey, SeqNum};

/// Shorthand for a live [`Entry::Value`] holding `bytes`
pub(crate) fn val(bytes: &[u8]) -> Entry {
    Entry::Value(bytes.to_vec())
}

/// Shorthand for an [`InternalKey`] of `bytes` at sequence number `seq`
pub(crate) fn ikey(bytes: &[u8], seq: SeqNum) -> InternalKey {
    InternalKey {
        user_key: bytes.to_vec(),
        seq,
    }
}
