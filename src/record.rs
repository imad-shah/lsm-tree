use std::num::TryFromIntError;

pub(crate) fn encode(key: &[u8], value: &[u8]) -> Result<Vec<u8>, TryFromIntError> {
    let key_len = u32::try_from(key.len())?;
    let val_len = u32::try_from(value.len())?;
    let mut res: Vec<u8> = Vec::new();

    res.extend_from_slice(&key_len.to_le_bytes());
    res.extend_from_slice(&val_len.to_le_bytes());

    res.extend_from_slice(key);
    res.extend_from_slice(value);

    Ok(res)
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
}
