#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Entry {
    Value(Vec<u8>),
    Tombstone,
}
