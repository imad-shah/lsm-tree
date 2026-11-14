use std::num::TryFromIntError;

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
    let mut res: Vec<u8> = Vec::new();

    // take the length, make sure it fits into a u32, make into le bytes
    res.extend_from_slice(&u32::try_from(key.len())?.to_le_bytes());
    res.extend_from_slice(&u32::try_from(value.len())?.to_le_bytes());

    // append the key and value bytes
    res.extend_from_slice(key);
    res.extend_from_slice(value);

    Ok(res)
}

pub(crate) fn decode_lengths(input: &[u8]) -> Result<Option<(u32, u32)>, DecodeError> {
    if input.is_empty() {
        Ok(None)
    } else if input.len() < 8 {
        Err(DecodeError::TruncatedHeader)
    } else {
        let key_len = u32::from_le_bytes([input[0], input[1], input[2], input[3]]);
        let value_len = u32::from_le_bytes([input[4], input[5], input[6], input[7]]);
        Ok(Some((key_len, value_len)))
    }
}

pub(crate) fn decode(input: &[u8]) -> Result<Option<(Record, usize)>, DecodeError> {
    let Some((key_len, val_len)) = decode_lengths(input)? else {
        return Ok(None);
    };
    let key_len = usize::try_from(key_len).map_err(|_| DecodeError::LengthOverflow)?;
    let val_len = usize::try_from(val_len).map_err(|_| DecodeError::LengthOverflow)?;

    let key_end = 8_usize
        .checked_add(key_len)
        .ok_or(DecodeError::LengthOverflow)?;

    let val_end = key_end
        .checked_add(val_len)
        .ok_or(DecodeError::LengthOverflow)?;

    if val_end > input.len() {
        return Err(DecodeError::TruncatedPayload);
    }

    let record = Record {
        key: input[8..key_end].to_vec(),
        value: input[key_end..val_end].to_vec(),
    };

    Ok(Some((record, val_end)))
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn len_before_key_and_value() {
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
    fn empty_key_and_value_have_zero_lengths() {
        let key = b"";
        let value = b"";
        let res = encode(key, value).unwrap();
        let expected: Vec<u8> = vec![0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        assert_eq!(res, expected);
    }

    #[test]
    fn non_empty_key_empty_value() {
        let key = b"a";
        let value = b"";
        let res = encode(key, value).unwrap();
        let expected: Vec<u8> = vec![0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x61];
        assert_eq!(res, expected);
    }

    #[test]
    fn zero_and_non_utf8_bytes() {
        let key: &[u8] = &[0x00, 0xff];
        let value: &[u8] = &[0x80, 0x00];
        let expected = vec![
            0x02, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0xff, 0x80, 0x00,
        ];
        let res = encode(key, value).unwrap();
        assert_eq!(res, expected);
    }

    #[test]
    fn key_length_256_uses_little_endian_bytes() {
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
    fn empty_input_returns_none() {
        let input: &[u8; 0] = &[];
        assert_eq!(decode_lengths(input), Ok(None));
    }

    #[test]
    fn zero_len_header_returns_zero() {
        let input: &[u8; 8] = &[0x00; 8];
        assert_eq!(decode_lengths(input), Ok(Some((0, 0))));
    }

    #[test]
    fn len_1_to_7_throws_truncated_err() {
        for len in 1..8 {
            let input = vec![0_u8; len];
            assert_eq!(
                decode_lengths(&input),
                Err(DecodeError::TruncatedHeader),
                "header length {len} should be rejected",
            );
        }
    }

    #[test]
    fn full_test_returns_length() {
        let input: &[u8] = &[
            5, 0, 0, 0, 3, 0, 0, 0, 97, 112, 112, 108, 101, 114, 101, 100,
        ];
        assert_eq!(decode_lengths(input), Ok(Some((5, 3))));
    }

    #[test]
    fn keylen_256_vallen_0() {
        let input: &[u8] = &[0, 1, 0, 0, 0, 0, 0, 0];
        assert_eq!(decode_lengths(input), Ok(Some((256, 0))));
    }

    #[test]
    fn empty_input_decode() {
        let input: &[u8; 0] = &[];
        assert_eq!(decode(input), Ok(None))
    }

    #[test]
    fn truncated_header() {
        for len in 1..8 {
            let input = vec![0_u8; len];
            assert_eq!(
                decode(&input),
                Err(DecodeError::TruncatedHeader),
                "header length {len} should be rejected",
            );
        }
    }

    #[test]
    fn empty_key_and_value() {
        let input: &[u8; 8] = &[0, 0, 0, 0, 0, 0, 0, 0];
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
    fn decoding_exact_payload() {
        let input: &[u8] = &[
            5, 0, 0, 0, 3, 0, 0, 0, 97, 112, 112, 108, 101, 114, 101, 100,
        ];
        assert_eq!(
            decode(input),
            Ok(Some((
                Record {
                    key: vec![97, 112, 112, 108, 101],
                    value: vec![114, 101, 100],
                },
                16
            )))
        );
    }

    #[test]
    fn incomplete_key() {
        let input: &[u8] = &[5, 0, 0, 0, 3, 0, 0, 0, 97]; // key cut off after 1 byte
        assert_eq!(decode(input), Err(DecodeError::TruncatedPayload));
    }

    #[test]
    fn incomplete_value() {
        let input: &[u8] = &[5, 0, 0, 0, 3, 0, 0, 0, 97, 112, 112, 108, 101, 114, 101];
        // value missing 1 byte
        assert_eq!(decode(input), Err(DecodeError::TruncatedPayload));
    }

    #[test]
    fn decoding_extra_payload() {
        let input: &[u8] = &[
            5, 0, 0, 0, 3, 0, 0, 0, 97, 112, 112, 108, 101, 114, 101, 100, 99, 99, 99, 99, 99, 99,
            99, 99, // trailing extra 99s, should still only consume 16 bytes
        ];
        assert_eq!(
            decode(input),
            Ok(Some((
                Record {
                    key: vec![97, 112, 112, 108, 101],
                    value: vec![114, 101, 100],
                },
                16
            )))
        );
    }

    #[test]
    fn round_trip_encode_then_decode() {
        let key = b"grape";
        let val = b"purple";

        let encoded = encode(key, val).unwrap();
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
                value: val.to_vec(),
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
