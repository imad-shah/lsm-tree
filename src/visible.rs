use crate::entry::Entry;
use crate::keys::{InternalKey, SeqNum};

/// Wraps a merged stream. For a user key, yields the newest version with
/// seq <= read_seq as (user_key, value), skips that key's other versions,
/// and hides the key if that version is a tombstone.
pub(crate) struct Visible<'a, I> {
    iter: I,
    read_seq: SeqNum,
    last_user_key: Option<&'a [u8]>,
}

// todo: implement the iterator for visible
impl<'a, I> Iterator for Visible<'a, I>
where
    I: Iterator<Item = (&'a InternalKey, &'a Entry)>,
{
    type Item = (&'a [u8], &'a [u8]);
}
