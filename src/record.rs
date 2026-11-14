use std::num::TryFromIntError;

/// Size of the record header
/// 4 byte key length
/// 4 byte value length
const HEADER_LEN: usize = 8;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DecodeError {
    TruncatedHeader,
    TruncatedPayload,
    LengthOverflow,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Record {
    pub(crate) key: Vec<u8>,
    pub(crate) value: Vec<u8>,
}

pub(crate) fn encode(key: &[u8], value: &[u8]) -> Result<Vec<u8>, TryFromIntError> {
    let mut res: Vec<u8> = Vec::with_capacity(HEADER_LEN + key.len() + value.len());

    // take the length, make sure it fits into a u32, make into le bytes
    res.extend_from_slice(&u32::try_from(key.len())?.to_le_bytes());
    res.extend_from_slice(&u32::try_from(value.len())?.to_le_bytes());

    // append the key and value bytes
    res.extend_from_slice(key);
    res.extend_from_slice(value);

    Ok(res)
}

fn decode_lengths(input: &[u8]) -> Result<Option<(u32, u32)>, DecodeError> {
    if input.is_empty() {
        Ok(None)
    } else if input.len() < HEADER_LEN {
        Err(DecodeError::TruncatedHeader)
    } else {
        let key_len = u32::from_le_bytes([input[0], input[1], input[2], input[3]]);
        let value_len = u32::from_le_bytes([input[4], input[5], input[6], input[7]]);
        Ok(Some((key_len, value_len)))
    }
}

/// Decodes one record from the start of `input`
///
/// Returns:
/// - `Ok(Some((record, consumed)))`: a full record was read, `consumed` is the number
///   of bytes it occupied
/// - `Ok(None)`: `input` is empty
/// - `Err(TruncatedHeader)`: header fewer than 8 bytes, so it's incomplete
/// - `Err(TruncatedPayload)`: the header promises more bytes than `input` has
///
/// Bytes after the first record are ignored
pub(crate) fn decode(input: &[u8]) -> Result<Option<(Record, usize)>, DecodeError> {
    let Some((key_len, value_len)) = decode_lengths(input)? else {
        return Ok(None);
    };
    let key_len = usize::try_from(key_len).map_err(|_| DecodeError::LengthOverflow)?;
    let value_len = usize::try_from(value_len).map_err(|_| DecodeError::LengthOverflow)?;

    let key_end = HEADER_LEN
        .checked_add(key_len)
        .ok_or(DecodeError::LengthOverflow)?;

    let value_end = key_end
        .checked_add(value_len)
        .ok_or(DecodeError::LengthOverflow)?;

    if value_end > input.len() {
        return Err(DecodeError::TruncatedPayload);
    }

    let record = Record {
        key: input[HEADER_LEN..key_end].to_vec(),
        value: input[key_end..value_end].to_vec(),
    };

    Ok(Some((record, value_end)))
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_writes_lengths_before_key_and_value() {
        let key = b"apple";
        let value = b"red";
        let res = encode(key, value).unwrap();
        let expected: Vec<u8> = vec![
            0x05, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x61, 0x70, 0x70, 0x6c, 0x65, 0x72,
            0x65, 0x64,
        ];

        assert_eq!(res, expected);
    }

    #[test]
    fn encode_empty_key_and_value_has_zero_lengths() {
        let key = b"";
        let value = b"";
        let res = encode(key, value).unwrap();
        let expected: Vec<u8> = vec![0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        assert_eq!(res, expected);
    }

    #[test]
    fn encode_key_with_empty_value() {
        let key = b"a";
        let value = b"";
        let res = encode(key, value).unwrap();
        let expected: Vec<u8> = vec![0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x61];
        assert_eq!(res, expected);
    }

    #[test]
    fn encode_zero_and_non_utf8_bytes() {
        let key: &[u8] = &[0x00, 0xff];
        let value: &[u8] = &[0x80, 0x00];
        let expected = vec![
            0x02, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0xff, 0x80, 0x00,
        ];
        let res = encode(key, value).unwrap();
        assert_eq!(res, expected);
    }

    #[test]
    fn encode_key_length_256_uses_little_endian() {
        let key = vec![b'a'; 256];
        let value = b"";
        let res = encode(&key, value).unwrap();
        // initial key_len and value_len
        let mut expected = vec![0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        // adding 0x61 256 times
        expected.extend_from_slice(&vec![b'a'; 256]);
        assert_eq!(res, expected);
    }

    #[test]
    fn decode_lengths_empty_input_returns_none() {
        let input: &[u8; 0] = &[];
        assert_eq!(decode_lengths(input), Ok(None));
    }

    #[test]
    fn decode_lengths_zero_header_returns_zeros() {
        let input: &[u8; 8] = &[0x00; 8];
        assert_eq!(decode_lengths(input), Ok(Some((0, 0))));
    }

    #[test]
    fn decode_lengths_rejects_partial_header() {
        for len in 1..HEADER_LEN {
            let input = vec![0_u8; len];
            assert_eq!(
                decode_lengths(&input),
                Err(DecodeError::TruncatedHeader),
                "header length {len} should be rejected",
            );
        }
    }

    #[test]
    fn decode_lengths_reads_header_of_full_record() {
        let input: &[u8] = &[
            0x05, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x61, 0x70, 0x70, 0x6c, 0x65, 0x72,
            0x65, 0x64,
        ];
        assert_eq!(decode_lengths(input), Ok(Some((5, 3))));
    }

    #[test]
    fn decode_lengths_reads_little_endian_256() {
        let input: &[u8] = &[0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        assert_eq!(decode_lengths(input), Ok(Some((256, 0))));
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
        let input: &[u8; 8] = &[0x00; 8];
        assert_eq!(
            decode(input),
            Ok(Some((
                Record {
                    key: vec![],
                    value: vec![]
                },
                8
            )))
        );
    }

    #[test]
    fn decode_exact_record() {
        let input: &[u8] = &[
            0x05, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x61, 0x70, 0x70, 0x6c, 0x65, 0x72,
            0x65, 0x64,
        ];
        assert_eq!(
            decode(input),
            Ok(Some((
                Record {
                    key: b"apple".to_vec(),
                    value: b"red".to_vec(),
                },
                16
            )))
        );
    }

    #[test]
    fn decode_rejects_cut_off_key() {
        // key cut off after 1 byte
        let input: &[u8] = &[0x05, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x61];
        assert_eq!(decode(input), Err(DecodeError::TruncatedPayload));
    }

    #[test]
    fn decode_rejects_cut_off_value() {
        // value missing its last byte
        let input: &[u8] = &[
            0x05, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x61, 0x70, 0x70, 0x6c, 0x65, 0x72,
            0x65,
        ];
        assert_eq!(decode(input), Err(DecodeError::TruncatedPayload));
    }

    #[test]
    fn decode_ignores_trailing_bytes() {
        // trailing 0x63s after the record, should still only consume 16 bytes
        let input: &[u8] = &[
            0x05, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x61, 0x70, 0x70, 0x6c, 0x65, 0x72,
            0x65, 0x64, 0x63, 0x63, 0x63, 0x63, 0x63, 0x63, 0x63, 0x63,
        ];
        assert_eq!(
            decode(input),
            Ok(Some((
                Record {
                    key: b"apple".to_vec(),
                    value: b"red".to_vec(),
                },
                16
            )))
        );
    }

    #[test]
    fn round_trip_encode_then_decode() {
        let key = b"grape";
        let value = b"purple";

        let encoded = encode(key, value).unwrap();
        let expected_encoding: Vec<u8> = vec![
            0x05, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x67, 0x72, 0x61, 0x70, 0x65, 0x70,
            0x75, 0x72, 0x70, 0x6c, 0x65,
        ];
        assert_eq!(encoded, expected_encoding);

        let decoded_lengths = decode_lengths(&encoded);
        let expected_decoding_lengths: Result<Option<(u32, u32)>, DecodeError> = Ok(Some((5, 6)));
        assert_eq!(decoded_lengths, expected_decoding_lengths);

        let decoded = decode(&encoded);
        let expected_decoded: Result<Option<(Record, usize)>, DecodeError> = Ok(Some((
            Record {
                key: key.to_vec(),
                value: value.to_vec(),
            },
            19,
        )));
        assert_eq!(decoded, expected_decoded);
    }

    #[test]
    fn decode_arbitrary_bytes() {
        let input: &[u8] = &[
            0x02, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0xff, 0x80, 0x00,
        ];
        assert_eq!(
            decode(input),
            Ok(Some((
                Record {
                    key: vec![0x00, 0xff],
                    value: vec![0x80, 0x00],
                },
                12
            )))
        );
    }

    #[test]
    fn decode_concatenated_records_using_returned_count() {
        let input: &[u8] = &[
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, 0x03, 0x00,
            0x00, 0x00, 0x61, 0x70, 0x70, 0x6c, 0x65, 0x72, 0x65, 0x64,
        ];

        let (first, consumed) = decode(input).unwrap().unwrap();
        assert_eq!(
            first,
            Record {
                key: vec![],
                value: vec![]
            }
        );
        assert_eq!(consumed, 8);

        let rest = &input[consumed..];
        let (second, consumed) = decode(rest).unwrap().unwrap();
        assert_eq!(
            second,
            Record {
                key: b"apple".to_vec(),
                value: b"red".to_vec(),
            }
        );
        assert_eq!(consumed, 16);

        let rest = &rest[consumed..];
        assert!(rest.is_empty());
        assert_eq!(decode(rest), Ok(None));
    }
}
