//! The format for one record (an internal key plus a value or a tombstone), and how to
//! encode/decode it
//!
//! A live value (`kind = 0x00`, `seq = 1`):
//!
//! ```text
//! offset:  0     1                         9             13            17                22         25
//!          ┌─────┬─────────────────────────┬─────────────┬─────────────┬─────────────────┬──────────┐
//!          │ kind│ seq                     │ key_len     │ value_len   │ key             │ value    │
//!          │ 00  │ 01 00 00 00 00 00 00 00 │ 05 00 00 00 │ 03 00 00 00 │ 61 70 70 6c 65  │ 72 65 64 │
//!          │ val │ = 1                     │ = 5         │ = 3         │ a  p  p  l  e   │ r  e  d  │
//!          └─────┴─────────────────────────┴─────────────┴─────────────┴─────────────────┴──────────┘
//! ```
//!
//! A tombstone (`kind = 0x01`, `seq = 2`) has `value_len = 0` and no value bytes:
//!
//! ```text
//! offset:  0     1                         9             13            17                22
//!          ┌─────┬─────────────────────────┬─────────────┬─────────────┬─────────────────┐
//!          │ kind│ seq                     │ key_len     │ value_len   │ key             │
//!          │ 01  │ 02 00 00 00 00 00 00 00 │ 05 00 00 00 │ 00 00 00 00 │ 61 70 70 6c 65  │
//!          │ del │ = 2                     │ = 5         │ = 0         │ a  p  p  l  e   │
//!          └─────┴─────────────────────────┴─────────────┴─────────────┴─────────────────┘
//! ```
//!
//! `seq` is a `u64` and both lengths are `u32`s, all stored little-endian
use crate::entry::Entry;
use crate::keys::InternalKey;
use std::num::TryFromIntError;

/// Size of the record header
/// 1 byte kind + 8 bytes seq + 4 byte key_len + 4 byte value_len
const HEADER_LEN: usize = 17;

/// Kind byte for a record holding a live value
const KIND_VALUE: u8 = 0x00;

/// Kind byte for a tombstone record
const KIND_TOMBSTONE: u8 = 0x01;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DecodeError {
    /// Input is non-empty but shorter than [`HEADER_LEN`]
    TruncatedHeader,
    /// The header promises more key/value bytes than the input has
    TruncatedPayload,
    /// A length doesn't fit in `usize`, or header + key + value overflows it
    LengthOverflow,
    /// The kind byte is neither `0x00` (value) nor `0x01` (tombstone)
    UnknownKind(u8),
    /// A tombstone header with a non-zero `value_len`
    InvalidTombstone,
}

/// An [`InternalKey`] and its [`Entry`],
/// as stored in one record
///
/// Used internally to represent a single versioned record. The key pairs a user key
/// byte sequence with a sequence number, and the entry is either live bytes or a tombstone
///
/// # Examples
///
/// ```ignore
/// let record = Record {
///     key: InternalKey {
///         user_key: b"user:1001".to_vec(),
///         seq: 1,
///     },
///     entry: Entry::Value(b"alice".to_vec()),
/// };
///
/// assert_eq!(record.key.user_key, b"user:1001");
/// assert_eq!(record.key.seq, 1);
/// assert_eq!(record.entry, Entry::Value(b"alice".to_vec()));
///
/// let deleted = Record {
///     key: InternalKey {
///         user_key: b"user:1002".to_vec(),
///         seq: 2,
///     },
///     entry: Entry::Tombstone,
/// };
/// ```
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Record {
    pub(crate) key: InternalKey,
    pub(crate) entry: Entry,
}

/// Encodes `key` and `entry` into one record
///
/// A [`Tombstone`](Entry::Tombstone) is written with kind `0x01`,
/// `value_len = 0` and no value bytes
///
/// # Errors
///
/// Returns the conversion error if the key or value is longer than `u32::MAX` bytes
pub(crate) fn encode(key: &InternalKey, entry: &Entry) -> Result<Vec<u8>, TryFromIntError> {
    let mut res: Vec<u8> = Vec::new();

    let key_len = &u32::try_from(key.user_key.len())?.to_le_bytes();
    let (kind, value_bytes): (u8, &[u8]) = match entry {
        Entry::Value(bytes) => (KIND_VALUE, bytes.as_slice()),
        Entry::Tombstone => (KIND_TOMBSTONE, &[]),
    };
    let value_len = &u32::try_from(value_bytes.len())?.to_le_bytes();
    let seq_bytes = &key.seq.to_le_bytes();

    res.push(kind);
    res.extend_from_slice(seq_bytes);
    res.extend_from_slice(key_len);
    res.extend_from_slice(value_len);
    res.extend_from_slice(key.user_key.as_slice());
    res.extend_from_slice(value_bytes);

    Ok(res)
}

fn decode_header(input: &[u8]) -> Result<Option<(u8, u64, u32, u32)>, DecodeError> {
    if input.is_empty() {
        Ok(None)
    } else if input.len() < HEADER_LEN {
        Err(DecodeError::TruncatedHeader)
    } else {
        // 1 byte
        let kind = input[0];
        // 8 bytes
        let seq = u64::from_le_bytes([
            input[1], input[2], input[3], input[4], input[5], input[6], input[7], input[8],
        ]);
        // 4 bytes
        let key_len = u32::from_le_bytes([input[9], input[10], input[11], input[12]]);
        // 4 bytes
        let value_len = u32::from_le_bytes([input[13], input[14], input[15], input[16]]);
        Ok(Some((kind, seq, key_len, value_len)))
    }
}

/// Decodes one record from the start of `input`
///
/// Returns:
/// - `Ok(Some((record, consumed)))`: a full record was read, `consumed` is the number
///   of bytes it occupied
/// - `Ok(None)`: `input` is empty
/// - `Err(TruncatedHeader)`: header fewer than [`HEADER_LEN`] (17) bytes, so it's incomplete
/// - `Err(UnknownKind(k))`: the kind byte is not `0x00` or `0x01`
/// - `Err(InvalidTombstone)`: a tombstone header with `value_len != 0`
/// - `Err(LengthOverflow)`: the lengths don't fit in `usize`
/// - `Err(TruncatedPayload)`: the header promises more bytes than `input` has
///
/// Bytes after the first record are ignored
pub(crate) fn decode(input: &[u8]) -> Result<Option<(Record, usize)>, DecodeError> {
    let Some((kind, seq, key_len, value_len)) = decode_header(input)? else {
        return Ok(None);
    };
    let is_tombstone = match kind {
        KIND_VALUE => false,
        KIND_TOMBSTONE => true,
        other => return Err(DecodeError::UnknownKind(other)),
    };
    let key_len = usize::try_from(key_len).map_err(|_| DecodeError::LengthOverflow)?;
    let value_len = usize::try_from(value_len).map_err(|_| DecodeError::LengthOverflow)?;

    let key_end = HEADER_LEN
        .checked_add(key_len)
        .ok_or(DecodeError::LengthOverflow)?;

    let value_end = key_end
        .checked_add(value_len)
        .ok_or(DecodeError::LengthOverflow)?;

    if is_tombstone && value_len > 0 {
        return Err(DecodeError::InvalidTombstone);
    }

    if value_end > input.len() {
        return Err(DecodeError::TruncatedPayload);
    }
    let entry = if is_tombstone {
        Entry::Tombstone
    } else {
        Entry::Value(input[key_end..value_end].to_vec())
    };
    let key = InternalKey {
        user_key: input[HEADER_LEN..key_end].to_vec(),
        seq,
    };
    let record = Record { key, entry };

    Ok(Some((record, value_end)))
}

/// Decodes back-to-back records until `input` is exhausted
///
/// # Errors
///
/// Returns the first [`DecodeError`] hit; any trailing bytes that don't form a
/// complete record are an error, not silently ignored
pub(crate) fn decode_all(mut input: &[u8]) -> Result<Vec<Record>, DecodeError> {
    let mut records = Vec::new();
    while let Some((record, consumed)) = decode(input)? {
        records.push(record);
        input = &input[consumed..];
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{ikey, val};

    #[test]
    fn encode_writes_kind_seq_and_lengths_before_key_and_value() {
        let res = encode(&ikey(b"apple", 1), &val(b"red")).unwrap();
        let expected: Vec<u8> = vec![
            0x00, // kind = value
            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // seq = 1
            0x05, 0x00, 0x00, 0x00, // key_len = 5
            0x03, 0x00, 0x00, 0x00, // value_len = 3
            0x61, 0x70, 0x70, 0x6c, 0x65, // "apple"
            0x72, 0x65, 0x64, // "red"
        ];

        assert_eq!(res, expected);
    }

    #[test]
    fn encode_empty_key_and_value_has_zero_lengths() {
        let res = encode(&ikey(b"", 0), &val(b"")).unwrap();
        let expected: Vec<u8> = vec![0x00; HEADER_LEN];
        assert_eq!(res, expected);
    }

    #[test]
    fn encode_key_with_empty_value() {
        let res = encode(&ikey(b"a", 9), &val(b"")).unwrap();
        let expected: Vec<u8> = vec![
            0x00, // kind = value
            0x09, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // seq = 9
            0x01, 0x00, 0x00, 0x00, // key_len = 1
            0x00, 0x00, 0x00, 0x00, // value_len = 0
            0x61, // "a"
        ];
        assert_eq!(res, expected);
    }

    #[test]
    fn encode_zero_and_non_utf8_bytes() {
        let key = ikey(&[0x00, 0xff], 7);
        let expected = vec![
            0x00, // kind
            0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // seq = 7
            0x02, 0x00, 0x00, 0x00, // key_len = 2
            0x02, 0x00, 0x00, 0x00, // value_len = 2
            0x00, 0xff, 0x80, 0x00,
        ];
        let res = encode(&key, &val(&[0x80, 0x00])).unwrap();
        assert_eq!(res, expected);
    }

    #[test]
    fn encode_key_length_256_uses_little_endian() {
        let key = ikey(&vec![b'a'; 256], 256);
        let res = encode(&key, &val(b"")).unwrap();
        let mut expected = vec![
            0x00, // kind
            0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // seq = 256
            0x00, 0x01, 0x00, 0x00, // key_len = 256
            0x00, 0x00, 0x00, 0x00, // value_len = 0
        ];
        expected.extend_from_slice(&vec![b'a'; 256]);
        assert_eq!(res, expected);
    }

    #[test]
    fn encode_tombstone_has_kind_one_and_no_value() {
        let res = encode(&ikey(b"apple", 9), &Entry::Tombstone).unwrap();
        let expected: Vec<u8> = vec![
            0x01, // kind = tombstone
            0x09, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // seq = 9
            0x05, 0x00, 0x00, 0x00, // key_len = 5
            0x00, 0x00, 0x00, 0x00, // value_len = 0
            0x61, 0x70, 0x70, 0x6c, 0x65, // "apple"
        ];
        assert_eq!(res, expected);
    }

    #[test]
    fn encode_tombstone_differs_from_empty_value() {
        let tomb = encode(&ikey(b"k", 3), &Entry::Tombstone).unwrap();
        let empty = encode(&ikey(b"k", 3), &val(b"")).unwrap();
        assert_ne!(tomb, empty);
        assert_eq!(tomb[0], 0x01);
        assert_eq!(empty[0], 0x00);
        assert_eq!(tomb[1..], empty[1..]);
    }

    #[test]
    fn decode_header_empty_input_returns_none() {
        let input: &[u8; 0] = &[];
        assert_eq!(decode_header(input), Ok(None));
    }

    #[test]
    fn decode_header_zero_header_returns_zeros() {
        let input = [0x00; HEADER_LEN];
        assert_eq!(decode_header(&input), Ok(Some((0, 0, 0, 0))));
    }

    #[test]
    fn decode_header_rejects_partial_header() {
        for len in 1..HEADER_LEN {
            let input = vec![0_u8; len];
            assert_eq!(
                decode_header(&input),
                Err(DecodeError::TruncatedHeader),
                "header length {len} should be rejected",
            );
        }
    }

    #[test]
    fn decode_header_reads_header_of_full_record() {
        let input: &[u8] = &[
            0x00, // kind
            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // seq = 1
            0x05, 0x00, 0x00, 0x00, // key_len = 5
            0x03, 0x00, 0x00, 0x00, // value_len = 3
            0x61, 0x70, 0x70, 0x6c, 0x65, 0x72, 0x65, 0x64,
        ];
        assert_eq!(decode_header(input), Ok(Some((0, 1, 5, 3))));
    }

    #[test]
    fn decode_header_reads_little_endian_256() {
        let input: &[u8] = &[
            0x00, // kind
            0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // seq = 256
            0x00, 0x01, 0x00, 0x00, // key_len = 256
            0x00, 0x00, 0x00, 0x00, // value_len = 0
        ];
        assert_eq!(decode_header(input), Ok(Some((0, 256, 256, 0))));
    }

    #[test]
    fn decode_header_reads_tombstone_kind() {
        let input: &[u8] = &[
            0x01, // kind
            0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // seq = 2
            0x05, 0x00, 0x00, 0x00, // key_len = 5
            0x00, 0x00, 0x00, 0x00, // value_len = 0
        ];
        assert_eq!(decode_header(input), Ok(Some((1, 2, 5, 0))));
    }

    #[test]
    fn decode_header_does_not_validate_kind() {
        let input: &[u8] = &[
            0x7f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00,
        ];
        assert_eq!(decode_header(input), Ok(Some((0x7f, 0, 0, 0))));
    }

    #[test]
    fn decode_empty_input_returns_none() {
        let input: &[u8; 0] = &[];
        assert_eq!(decode(input), Ok(None));
    }

    #[test]
    fn decode_rejects_partial_header() {
        for len in 1..HEADER_LEN {
            let input = vec![0_u8; len];
            assert_eq!(
                decode(&input),
                Err(DecodeError::TruncatedHeader),
                "header length {len} should be rejected",
            );
        }
    }

    #[test]
    fn decode_empty_key_and_value() {
        let input = [0x00; HEADER_LEN];
        assert_eq!(
            decode(&input),
            Ok(Some((
                Record {
                    key: ikey(b"", 0),
                    entry: val(b""),
                },
                HEADER_LEN
            )))
        );
    }

    #[test]
    fn decode_exact_record() {
        let input: &[u8] = &[
            0x00, // kind
            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // seq = 1
            0x05, 0x00, 0x00, 0x00, // key_len = 5
            0x03, 0x00, 0x00, 0x00, // value_len = 3
            0x61, 0x70, 0x70, 0x6c, 0x65, // "apple"
            0x72, 0x65, 0x64, // "red"
        ];
        assert_eq!(
            decode(input),
            Ok(Some((
                Record {
                    key: ikey(b"apple", 1),
                    entry: val(b"red"),
                },
                25
            )))
        );
    }

    #[test]
    fn decode_rejects_cut_off_key() {
        // key cut off after 1 byte
        let input: &[u8] = &[
            0x00, // kind
            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // seq = 1
            0x05, 0x00, 0x00, 0x00, // key_len = 5
            0x03, 0x00, 0x00, 0x00, // value_len = 3
            0x61, // 1 byte of key
        ];
        assert_eq!(decode(input), Err(DecodeError::TruncatedPayload));
    }

    #[test]
    fn decode_rejects_cut_off_value() {
        // value missing its last byte
        let input: &[u8] = &[
            0x00, // kind
            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // seq = 1
            0x05, 0x00, 0x00, 0x00, // key_len = 5
            0x03, 0x00, 0x00, 0x00, // value_len = 3
            0x61, 0x70, 0x70, 0x6c, 0x65, // "apple"
            0x72, 0x65, // "re" (missing "d")
        ];
        assert_eq!(decode(input), Err(DecodeError::TruncatedPayload));
    }

    #[test]
    fn decode_ignores_trailing_bytes() {
        // trailing 0x63s after the record, should still only consume 25 bytes
        let input: &[u8] = &[
            0x00, // kind
            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // seq = 1
            0x05, 0x00, 0x00, 0x00, // key_len = 5
            0x03, 0x00, 0x00, 0x00, // value_len = 3
            0x61, 0x70, 0x70, 0x6c, 0x65, 0x72, 0x65, 0x64, 0x63, 0x63, 0x63, 0x63, 0x63, 0x63,
            0x63, 0x63,
        ];
        assert_eq!(
            decode(input),
            Ok(Some((
                Record {
                    key: ikey(b"apple", 1),
                    entry: val(b"red"),
                },
                25
            )))
        );
    }

    #[test]
    fn round_trip_encode_then_decode() {
        let key = ikey(b"grape", 12);
        let entry = val(b"purple");

        let encoded = encode(&key, &entry).unwrap();
        let expected_encoding: Vec<u8> = vec![
            0x00, // kind
            0x0c, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // seq = 12
            0x05, 0x00, 0x00, 0x00, // key_len = 5
            0x06, 0x00, 0x00, 0x00, // value_len = 6
            0x67, 0x72, 0x61, 0x70, 0x65, // "grape"
            0x70, 0x75, 0x72, 0x70, 0x6c, 0x65, // "purple"
        ];
        assert_eq!(encoded, expected_encoding);

        let decoded_header = decode_header(&encoded);
        let expected_header: Result<Option<(u8, u64, u32, u32)>, DecodeError> =
            Ok(Some((0, 12, 5, 6)));
        assert_eq!(decoded_header, expected_header);

        let decoded = decode(&encoded);
        let expected_decoded: Result<Option<(Record, usize)>, DecodeError> =
            Ok(Some((Record { key, entry }, 28)));
        assert_eq!(decoded, expected_decoded);
    }

    #[test]
    fn decode_arbitrary_bytes() {
        let input: &[u8] = &[
            0x00, // kind
            0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // seq = 7
            0x02, 0x00, 0x00, 0x00, // key_len = 2
            0x02, 0x00, 0x00, 0x00, // value_len = 2
            0x00, 0xff, 0x80, 0x00,
        ];
        assert_eq!(
            decode(input),
            Ok(Some((
                Record {
                    key: ikey(&[0x00, 0xff], 7),
                    entry: val(&[0x80, 0x00]),
                },
                21
            )))
        );
    }

    #[test]
    fn decode_concatenated_records_using_returned_count() {
        let mut input = Vec::new();
        input.extend(encode(&ikey(b"", 0), &val(b"")).unwrap());
        input.extend(encode(&ikey(b"apple", 1), &val(b"red")).unwrap());
        input.extend(encode(&ikey(b"b", 2), &Entry::Tombstone).unwrap());

        let (first, consumed) = decode(&input).unwrap().unwrap();
        assert_eq!(
            first,
            Record {
                key: ikey(b"", 0),
                entry: val(b""),
            }
        );
        assert_eq!(consumed, HEADER_LEN);

        let rest = &input[consumed..];
        let (second, consumed) = decode(rest).unwrap().unwrap();
        assert_eq!(
            second,
            Record {
                key: ikey(b"apple", 1),
                entry: val(b"red"),
            }
        );
        assert_eq!(consumed, 25);

        let rest = &rest[consumed..];
        let (third, consumed) = decode(rest).unwrap().unwrap();
        assert_eq!(
            third,
            Record {
                key: ikey(b"b", 2),
                entry: Entry::Tombstone,
            }
        );
        assert_eq!(consumed, 18);

        let rest = &rest[consumed..];
        assert!(rest.is_empty());
        assert_eq!(decode(rest), Ok(None));
    }

    #[test]
    fn decode_tombstone() {
        let input: &[u8] = &[
            0x01, // kind
            0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // seq = 4
            0x05, 0x00, 0x00, 0x00, // key_len = 5
            0x00, 0x00, 0x00, 0x00, // value_len = 0
            0x61, 0x70, 0x70, 0x6c, 0x65, // "apple"
        ];
        assert_eq!(
            decode(input),
            Ok(Some((
                Record {
                    key: ikey(b"apple", 4),
                    entry: Entry::Tombstone,
                },
                22
            )))
        );
    }

    #[test]
    fn round_trip_tombstone() {
        let encoded = encode(&ikey(b"grape", 5), &Entry::Tombstone).unwrap();
        assert_eq!(
            decode(&encoded),
            Ok(Some((
                Record {
                    key: ikey(b"grape", 5),
                    entry: Entry::Tombstone,
                },
                encoded.len()
            )))
        );
    }

    #[test]
    fn decode_tombstone_with_empty_key() {
        let mut input = vec![0x00; HEADER_LEN];
        input[0] = 0x01;
        assert_eq!(
            decode(&input),
            Ok(Some((
                Record {
                    key: ikey(b"", 0),
                    entry: Entry::Tombstone,
                },
                HEADER_LEN
            )))
        );
    }

    #[test]
    fn decode_rejects_unknown_kind() {
        for kind in [0x02_u8, 0x7f, 0xff] {
            let mut input = vec![0x00; HEADER_LEN];
            input[0] = kind;
            assert_eq!(
                decode(&input),
                Err(DecodeError::UnknownKind(kind)),
                "kind {kind:#04x} should be rejected",
            );
        }
    }

    #[test]
    fn decode_rejects_tombstone_with_nonzero_value_len() {
        let input: &[u8] = &[
            0x01, // kind
            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // seq = 1
            0x05, 0x00, 0x00, 0x00, // key_len = 5
            0x03, 0x00, 0x00, 0x00, // value_len = 3
            0x61, 0x70, 0x70, 0x6c, 0x65, 0x72, 0x65, 0x64,
        ];
        assert_eq!(decode(input), Err(DecodeError::InvalidTombstone));
    }

    #[test]
    fn decode_rejects_cut_off_tombstone_key() {
        let input: &[u8] = &[
            0x01, // kind
            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // seq = 1
            0x05, 0x00, 0x00, 0x00, // key_len = 5
            0x00, 0x00, 0x00, 0x00, // value_len = 0
            0x61, 0x70, // 2 bytes of key
        ];
        assert_eq!(decode(input), Err(DecodeError::TruncatedPayload));
    }

    #[test]
    fn decode_all_empty_input_returns_no_records() {
        assert_eq!(decode_all(&[]), Ok(vec![]));
    }

    #[test]
    fn decode_all_reads_every_record() {
        let mut input = encode(&ikey(b"apple", 1), &val(b"red")).unwrap();
        input.extend(encode(&ikey(b"banana", 2), &Entry::Tombstone).unwrap());
        input.extend(encode(&ikey(b"cherry", 3), &val(b"")).unwrap());
        assert_eq!(
            decode_all(&input),
            Ok(vec![
                Record {
                    key: ikey(b"apple", 1),
                    entry: val(b"red"),
                },
                Record {
                    key: ikey(b"banana", 2),
                    entry: Entry::Tombstone,
                },
                Record {
                    key: ikey(b"cherry", 3),
                    entry: val(b""),
                },
            ])
        );
    }

    #[test]
    fn decode_all_rejects_trailing_partial_record() {
        let mut input = encode(&ikey(b"apple", 1), &val(b"red")).unwrap();
        input.extend_from_slice(&[0x00, 0x01]);
        assert_eq!(decode_all(&input), Err(DecodeError::TruncatedHeader));
    }
}
