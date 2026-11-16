//! The format for one record (a key plus a value or a tombstone), and how to
//! encode/decode it
//!
//! A live value (`kind = 0x00`):
//!
//! ```text
//! offset:  0     1             5             9                 14         17
//!          ┌─────┬─────────────┬─────────────┬─────────────────┬──────────┐
//!          │ kind│ key_len     │ value_len   │ key             │ value    │
//!          │ 00  │ 05 00 00 00 │ 03 00 00 00 │ 61 70 70 6c 65  │ 72 65 64 │
//!          │ val │ = 5         │ = 3         │ a  p  p  l  e   │ r  e  d  │
//!          └─────┴─────────────┴─────────────┴─────────────────┴──────────┘
//! ```
//!
//! A tombstone (`kind = 0x01`) has `value_len = 0` and no value bytes:
//!
//! ```text
//! offset:  0     1             5             9                 14
//!          ┌─────┬─────────────┬─────────────┬─────────────────┐
//!          │ kind│ key_len     │ value_len   │ key             │
//!          │ 01  │ 05 00 00 00 │ 00 00 00 00 │ 61 70 70 6c 65  │
//!          │ del │ = 5         │ = 0         │ a  p  p  l  e   │
//!          └─────┴─────────────┴─────────────┴─────────────────┘
//! ```
//!
//! Both lengths are `u32`s stored little-endian
use std::num::TryFromIntError;

/// Size of the record header
/// 1 byte kind + 4 byte key_len + 4 byte value_len
const HEADER_LEN: usize = 9;

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

/// A key and its [`Entry`](crate::entry::Entry), as stored in one record.
///
/// Used internally to represent a single record. The key is a byte sequence,
/// the value is either live bytes or a tombstone.
///
/// # Examples
///
/// ```ignore
/// let record = Record {
///     key: b"user:1001".to_vec(),
///     value: Entry::Value(b"alice".to_vec()),
/// };
///
/// assert_eq!(record.key, b"user:1001");
/// assert_eq!(record.value, Entry::Value(b"alice".to_vec()));
///
/// let deleted = Record {
///     key: b"user:1002".to_vec(),
///     value: Entry::Tombstone,
/// };
/// ```
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Record {
    pub(crate) key: Vec<u8>,
    pub(crate) value: crate::entry::Entry,
}

/// Encodes `key` and `value` into one record
///
/// A [`Tombstone`](crate::entry::Entry::Tombstone) is written with kind `0x01`,
/// `value_len = 0` and no value bytes.
///
/// # Errors
///
/// Returns the conversion error if the key or value is longer than `u32::MAX` bytes.
pub(crate) fn encode(key: &[u8], value: &crate::entry::Entry) -> Result<Vec<u8>, TryFromIntError> {
    let mut res: Vec<u8> = Vec::new();

    let key_len = &u32::try_from(key.len())?.to_le_bytes();
    let (kind, value_bytes): (&[u8], &[u8]) = match value {
        crate::entry::Entry::Value(bytes) => (&[0x00], bytes.as_slice()),
        crate::entry::Entry::Tombstone => (&[0x01], &[]),
    };
    let value_len = &u32::try_from(value_bytes.len())?.to_le_bytes();

    res.extend_from_slice(kind);
    res.extend_from_slice(key_len);
    res.extend_from_slice(value_len);
    res.extend_from_slice(key);
    res.extend_from_slice(value_bytes);

    Ok(res)
}

fn decode_header(input: &[u8]) -> Result<Option<(u8, u32, u32)>, DecodeError> {
    if input.is_empty() {
        Ok(None)
    } else if input.len() < HEADER_LEN {
        Err(DecodeError::TruncatedHeader)
    } else {
        let kind = input[0];
        let key_len = u32::from_le_bytes([input[1], input[2], input[3], input[4]]);
        let value_len = u32::from_le_bytes([input[5], input[6], input[7], input[8]]);
        Ok(Some((kind, key_len, value_len)))
    }
}
/// Decodes one record from the start of `input`
///
/// Returns:
/// - `Ok(Some((record, consumed)))`: a full record was read, `consumed` is the number
///   of bytes it occupied
/// - `Ok(None)`: `input` is empty
/// - `Err(TruncatedHeader)`: header fewer than 9 bytes, so it's incomplete
/// - `Err(UnknownKind(k))`: the kind byte is not `0x00` or `0x01`
/// - `Err(InvalidTombstone)`: a tombstone header with `value_len != 0`
/// - `Err(LengthOverflow)`: the lengths don't fit in `usize`
/// - `Err(TruncatedPayload)`: the header promises more bytes than `input` has
///
/// Bytes after the first record are ignored
pub(crate) fn decode(input: &[u8]) -> Result<Option<(Record, usize)>, DecodeError> {
    let Some((kind, key_len, value_len)) = decode_header(input)? else {
        return Ok(None);
    };
    if kind != 0 && kind != 1 {
        return Err(DecodeError::UnknownKind(kind));
    }
    let key_len = usize::try_from(key_len).map_err(|_| DecodeError::LengthOverflow)?;
    let value_len = usize::try_from(value_len).map_err(|_| DecodeError::LengthOverflow)?;

    let key_end = HEADER_LEN
        .checked_add(key_len)
        .ok_or(DecodeError::LengthOverflow)?;

    let value_end = key_end
        .checked_add(value_len)
        .ok_or(DecodeError::LengthOverflow)?;

    if kind == 1 && value_len > 0 {
        return Err(DecodeError::InvalidTombstone);
    }

    if value_end > input.len() {
        return Err(DecodeError::TruncatedPayload);
    }
    let entry_value = match kind {
        0 => crate::entry::Entry::Value(input[key_end..value_end].to_vec()),
        1 => crate::entry::Entry::Tombstone,
        _ => return Err(DecodeError::UnknownKind(kind)),
    };
    let record = Record {
        key: input[HEADER_LEN..key_end].to_vec(),
        value: entry_value,
    };

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
    use crate::entry::Entry;

    // helper so expected values are readable
    fn val(bytes: &[u8]) -> Entry {
        Entry::Value(bytes.to_vec())
    }

    #[test]
    fn encode_writes_kind_and_lengths_before_key_and_value() {
        let res = encode(b"apple", &val(b"red")).unwrap();
        let expected: Vec<u8> = vec![
            0x00, // kind = value
            0x05, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x61, 0x70, 0x70, 0x6c, 0x65, 0x72,
            0x65, 0x64,
        ];

        assert_eq!(res, expected);
    }

    #[test]
    fn encode_empty_key_and_value_has_zero_lengths() {
        let res = encode(b"", &val(b"")).unwrap();
        let expected: Vec<u8> = vec![0x00; HEADER_LEN];
        assert_eq!(res, expected);
    }

    #[test]
    fn encode_key_with_empty_value() {
        let res = encode(b"a", &val(b"")).unwrap();
        let expected: Vec<u8> = vec![0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x61];
        assert_eq!(res, expected);
    }

    #[test]
    fn encode_zero_and_non_utf8_bytes() {
        let key: &[u8] = &[0x00, 0xff];
        let expected = vec![
            0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0xff, 0x80, 0x00,
        ];
        let res = encode(key, &val(&[0x80, 0x00])).unwrap();
        assert_eq!(res, expected);
    }

    #[test]
    fn encode_key_length_256_uses_little_endian() {
        let key = vec![b'a'; 256];
        let res = encode(&key, &val(b"")).unwrap();
        let mut expected = vec![0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        expected.extend_from_slice(&vec![b'a'; 256]);
        assert_eq!(res, expected);
    }

    #[test]
    fn encode_tombstone_has_kind_one_and_no_value() {
        let res = encode(b"apple", &Entry::Tombstone).unwrap();
        let expected: Vec<u8> = vec![
            0x01, // kind = tombstone
            0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x61, 0x70, 0x70, 0x6c, 0x65,
        ];
        assert_eq!(res, expected);
    }

    #[test]
    fn encode_tombstone_differs_from_empty_value() {
        let tomb = encode(b"k", &Entry::Tombstone).unwrap();
        let empty = encode(b"k", &val(b"")).unwrap();
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
        assert_eq!(decode_header(&input), Ok(Some((0, 0, 0))));
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
            0x00, 0x05, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x61, 0x70, 0x70, 0x6c, 0x65,
            0x72, 0x65, 0x64,
        ];
        assert_eq!(decode_header(input), Ok(Some((0, 5, 3))));
    }

    #[test]
    fn decode_header_reads_little_endian_256() {
        let input: &[u8] = &[0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        assert_eq!(decode_header(input), Ok(Some((0, 256, 0))));
    }

    #[test]
    fn decode_header_reads_tombstone_kind() {
        let input: &[u8] = &[0x01, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        assert_eq!(decode_header(input), Ok(Some((1, 5, 0))));
    }

    #[test]
    fn decode_header_does_not_validate_kind() {
        let input: &[u8] = &[0x7f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        assert_eq!(decode_header(input), Ok(Some((0x7f, 0, 0))));
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
                    key: vec![],
                    value: val(b""),
                },
                HEADER_LEN
            )))
        );
    }

    #[test]
    fn decode_exact_record() {
        let input: &[u8] = &[
            0x00, 0x05, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x61, 0x70, 0x70, 0x6c, 0x65,
            0x72, 0x65, 0x64,
        ];
        assert_eq!(
            decode(input),
            Ok(Some((
                Record {
                    key: b"apple".to_vec(),
                    value: val(b"red"),
                },
                17
            )))
        );
    }

    #[test]
    fn decode_rejects_cut_off_key() {
        // key cut off after 1 byte
        let input: &[u8] = &[0x00, 0x05, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x61];
        assert_eq!(decode(input), Err(DecodeError::TruncatedPayload));
    }

    #[test]
    fn decode_rejects_cut_off_value() {
        // value missing its last byte
        let input: &[u8] = &[
            0x00, 0x05, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x61, 0x70, 0x70, 0x6c, 0x65,
            0x72, 0x65,
        ];
        assert_eq!(decode(input), Err(DecodeError::TruncatedPayload));
    }

    #[test]
    fn decode_ignores_trailing_bytes() {
        // trailing 0x63s after the record, should still only consume 17 bytes
        let input: &[u8] = &[
            0x00, 0x05, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x61, 0x70, 0x70, 0x6c, 0x65,
            0x72, 0x65, 0x64, 0x63, 0x63, 0x63, 0x63, 0x63, 0x63, 0x63, 0x63,
        ];
        assert_eq!(
            decode(input),
            Ok(Some((
                Record {
                    key: b"apple".to_vec(),
                    value: val(b"red"),
                },
                17
            )))
        );
    }

    #[test]
    fn round_trip_encode_then_decode() {
        let key = b"grape";
        let value = val(b"purple");

        let encoded = encode(key, &value).unwrap();
        let expected_encoding: Vec<u8> = vec![
            0x00, 0x05, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x67, 0x72, 0x61, 0x70, 0x65,
            0x70, 0x75, 0x72, 0x70, 0x6c, 0x65,
        ];
        assert_eq!(encoded, expected_encoding);

        let decoded_header = decode_header(&encoded);
        let expected_header: Result<Option<(u8, u32, u32)>, DecodeError> = Ok(Some((0, 5, 6)));
        assert_eq!(decoded_header, expected_header);

        let decoded = decode(&encoded);
        let expected_decoded: Result<Option<(Record, usize)>, DecodeError> = Ok(Some((
            Record {
                key: key.to_vec(),
                value,
            },
            20,
        )));
        assert_eq!(decoded, expected_decoded);
    }

    #[test]
    fn decode_arbitrary_bytes() {
        let input: &[u8] = &[
            0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0xff, 0x80, 0x00,
        ];
        assert_eq!(
            decode(input),
            Ok(Some((
                Record {
                    key: vec![0x00, 0xff],
                    value: val(&[0x80, 0x00]),
                },
                13
            )))
        );
    }

    #[test]
    fn decode_concatenated_records_using_returned_count() {
        let input: &[u8] = &[
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00,
            0x03, 0x00, 0x00, 0x00, 0x61, 0x70, 0x70, 0x6c, 0x65, 0x72, 0x65, 0x64, 0x01, 0x01,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x62,
        ];

        let (first, consumed) = decode(input).unwrap().unwrap();
        assert_eq!(
            first,
            Record {
                key: vec![],
                value: val(b""),
            }
        );
        assert_eq!(consumed, 9);

        let rest = &input[consumed..];
        let (second, consumed) = decode(rest).unwrap().unwrap();
        assert_eq!(
            second,
            Record {
                key: b"apple".to_vec(),
                value: val(b"red"),
            }
        );
        assert_eq!(consumed, 17);

        let rest = &rest[consumed..];
        let (third, consumed) = decode(rest).unwrap().unwrap();
        assert_eq!(
            third,
            Record {
                key: b"b".to_vec(),
                value: Entry::Tombstone,
            }
        );
        assert_eq!(consumed, 10);

        let rest = &rest[consumed..];
        assert!(rest.is_empty());
        assert_eq!(decode(rest), Ok(None));
    }

    #[test]
    fn decode_tombstone() {
        let input: &[u8] = &[
            0x01, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x61, 0x70, 0x70, 0x6c, 0x65,
        ];
        assert_eq!(
            decode(input),
            Ok(Some((
                Record {
                    key: b"apple".to_vec(),
                    value: Entry::Tombstone,
                },
                14
            )))
        );
    }

    #[test]
    fn round_trip_tombstone() {
        let encoded = encode(b"grape", &Entry::Tombstone).unwrap();
        assert_eq!(
            decode(&encoded),
            Ok(Some((
                Record {
                    key: b"grape".to_vec(),
                    value: Entry::Tombstone,
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
                    key: vec![],
                    value: Entry::Tombstone,
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
            0x01, 0x05, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x61, 0x70, 0x70, 0x6c, 0x65,
            0x72, 0x65, 0x64,
        ];
        assert_eq!(decode(input), Err(DecodeError::InvalidTombstone));
    }

    #[test]
    fn decode_rejects_cut_off_tombstone_key() {
        let input: &[u8] = &[
            0x01, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x61, 0x70,
        ];
        assert_eq!(decode(input), Err(DecodeError::TruncatedPayload));
    }

    #[test]
    fn decode_all_empty_input_returns_no_records() {
        assert_eq!(decode_all(&[]), Ok(vec![]));
    }
    #[test]
    fn decode_all_reads_every_record() {
        let mut input = encode(b"apple", &val(b"red")).unwrap();
        input.extend(encode(b"banana", &Entry::Tombstone).unwrap());
        input.extend(encode(b"cherry", &val(b"")).unwrap());
        assert_eq!(
            decode_all(&input),
            Ok(vec![
                Record {
                    key: b"apple".to_vec(),
                    value: val(b"red"),
                },
                Record {
                    key: b"banana".to_vec(),
                    value: Entry::Tombstone,
                },
                Record {
                    key: b"cherry".to_vec(),
                    value: val(b""),
                },
            ])
        );
    }

    #[test]
    fn decode_all_rejects_trailing_partial_record() {
        let mut input = encode(b"apple", &val(b"red")).unwrap();
        input.extend_from_slice(&[0x00, 0x01]);
        assert_eq!(decode_all(&input), Err(DecodeError::TruncatedHeader));
    }
}
