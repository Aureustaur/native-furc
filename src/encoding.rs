//! Furcadia numeric and string encodings (`Base95` and `Base220`).

use std::fmt;

/// Offset for Base95 printable ASCII characters (`' '` = 32 through `'~'` = 126).
const BASE95_OFFSET: u8 = 32;
const BASE95_RADIX: u64 = 95;

/// Offset for Base220 encoding (`'#'` = 35 through 254).
const BASE220_OFFSET: u8 = 35;
const BASE220_RADIX: u64 = 220;

/// Errors returned when encoding or decoding Furcadia protocol values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodingError {
    /// Input slice is empty where at least one byte was required.
    EmptyInput,
    /// Encountered a byte outside the valid encoding alphabet.
    InvalidByte { byte: u8, encoding: &'static str },
    /// Decoded value overflows the target integer type.
    Overflow,
    /// String length exceeds the maximum representable length (219 bytes for b220str).
    StringTooLong(usize),
    /// Byte slice ended before the declared length was read.
    UnexpectedEof,
}

impl fmt::Display for EncodingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyInput => write!(f, "empty input"),
            Self::InvalidByte { byte, encoding } => {
                write!(f, "invalid {encoding} byte: 0x{byte:02x}")
            }
            Self::Overflow => write!(f, "numeric overflow"),
            Self::StringTooLong(len) => write!(f, "string length {len} exceeds maximum 219"),
            Self::UnexpectedEof => write!(f, "unexpected end of input"),
        }
    }
}

impl std::error::Error for EncodingError {}

/// Decodes a big-endian Base95 byte slice into a `u64`.
pub fn decode_base95(bytes: &[u8]) -> Result<u64, EncodingError> {
    if bytes.is_empty() {
        return Err(EncodingError::EmptyInput);
    }
    let mut acc: u64 = 0;
    for &b in bytes {
        if !(BASE95_OFFSET..=BASE95_OFFSET + 94).contains(&b) {
            return Err(EncodingError::InvalidByte {
                byte: b,
                encoding: "Base95",
            });
        }
        let digit = (b - BASE95_OFFSET) as u64;
        acc = acc
            .checked_mul(BASE95_RADIX)
            .and_then(|v| v.checked_add(digit))
            .ok_or(EncodingError::Overflow)?;
    }
    Ok(acc)
}

/// Encodes a `u64` into a big-endian Base95 byte vector of `width` characters.
pub fn encode_base95(mut value: u64, width: usize) -> Result<Vec<u8>, EncodingError> {
    if width == 0 {
        return Err(EncodingError::EmptyInput);
    }
    let mut out = vec![BASE95_OFFSET; width];
    for slot in out.iter_mut().rev() {
        *slot = BASE95_OFFSET + (value % BASE95_RADIX) as u8;
        value /= BASE95_RADIX;
    }
    if value > 0 {
        return Err(EncodingError::Overflow);
    }
    Ok(out)
}

/// Decodes a little-endian Base220 byte slice into a `u64`.
pub fn decode_base220(bytes: &[u8]) -> Result<u64, EncodingError> {
    if bytes.is_empty() {
        return Err(EncodingError::EmptyInput);
    }
    let mut acc: u64 = 0;
    let mut multiplier: u64 = 1;
    for (idx, &b) in bytes.iter().enumerate() {
        if b < BASE220_OFFSET || b as u16 >= BASE220_OFFSET as u16 + BASE220_RADIX as u16 {
            return Err(EncodingError::InvalidByte {
                byte: b,
                encoding: "Base220",
            });
        }
        let digit = (b - BASE220_OFFSET) as u64;
        let term = digit
            .checked_mul(multiplier)
            .ok_or(EncodingError::Overflow)?;
        acc = acc.checked_add(term).ok_or(EncodingError::Overflow)?;
        if idx + 1 < bytes.len() {
            multiplier = multiplier
                .checked_mul(BASE220_RADIX)
                .ok_or(EncodingError::Overflow)?;
        }
    }
    Ok(acc)
}

/// Encodes a `u64` into a little-endian Base220 byte vector of `width` bytes.
pub fn encode_base220(mut value: u64, width: usize) -> Result<Vec<u8>, EncodingError> {
    if width == 0 {
        return Err(EncodingError::EmptyInput);
    }
    let mut out = Vec::with_capacity(width);
    for _ in 0..width {
        out.push(BASE220_OFFSET + (value % BASE220_RADIX) as u8);
        value /= BASE220_RADIX;
    }
    if value > 0 {
        return Err(EncodingError::Overflow);
    }
    Ok(out)
}

/// Encodes a short string as a Base220 length-prefixed `b220str`.
pub fn encode_b220_str(s: &str) -> Result<Vec<u8>, EncodingError> {
    let bytes = s.as_bytes();
    if bytes.len() >= BASE220_RADIX as usize {
        return Err(EncodingError::StringTooLong(bytes.len()));
    }
    let mut out = Vec::with_capacity(1 + bytes.len());
    out.push(BASE220_OFFSET + bytes.len() as u8);
    out.extend_from_slice(bytes);
    Ok(out)
}

/// Decodes a Base220 length-prefixed `b220str` from the start of `bytes`,
/// returning the decoded string (lossy UTF-8) and the remaining unconsumed bytes.
pub fn decode_b220_str(bytes: &[u8]) -> Result<(String, &[u8]), EncodingError> {
    let (&len_byte, rest) = bytes.split_first().ok_or(EncodingError::EmptyInput)?;
    let len = decode_base220(&[len_byte])? as usize;
    if rest.len() < len {
        return Err(EncodingError::UnexpectedEof);
    }
    let (payload, tail) = rest.split_at(len);
    Ok((String::from_utf8_lossy(payload).into_owned(), tail))
}

/// Decodes a little-endian Base220 byte slice matching `Furcadia.exe` `0x456050`.
/// Bytes below `BASE220_OFFSET` (`0x23`) contribute 0 while still advancing the
/// base-220 positional multiplier, and byte `0xff` (value 220) is accepted.
pub fn decode_b220_raw(bytes: &[u8]) -> u64 {
    let mut acc: u64 = 0;
    let mut mult: u64 = 1;
    for &b in bytes {
        if b >= BASE220_OFFSET {
            acc = acc.saturating_add(((b - BASE220_OFFSET) as u64).saturating_mul(mult));
        }
        mult = mult.saturating_mul(BASE220_RADIX);
    }
    acc
}

/// Decodes a Base220 length-prefixed byte slice matching `Furcadia.exe` `0x4561a0`,
/// returning `(payload_bytes, remaining_bytes)` without UTF-8 conversion.
pub fn decode_b220_str_raw(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    let (&len_byte, rest) = bytes.split_first()?;
    if len_byte < BASE220_OFFSET {
        return None;
    }
    let len = (len_byte - BASE220_OFFSET) as usize;
    if rest.len() < len {
        return None;
    }
    Some(rest.split_at(len))
}

/// Decodes a 2-byte signed Base95 coordinate as used by `Furcadia.exe` `0x471580` (`]s`/`]t`):
/// `(b0 as i8 as i32) * 95 + (b1 as i8 as i32) - 3072`.
pub fn decode_base95_signed(b0: u8, b1: u8) -> i32 {
    (b0 as i8 as i32) * (BASE95_RADIX as i32) + (b1 as i8 as i32) - 3072
}

/// Windows-1252 `0x80..=0x9F` code point mapping.
const CP1252_UPPER: [char; 32] = [
    '\u{20AC}', '\u{0081}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{008D}', '\u{017D}', '\u{008F}',
    '\u{0090}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}', '\u{0153}', '\u{009D}', '\u{017E}', '\u{0178}',
];

/// Decodes raw Furcadia text bytes as UTF-8 if valid, otherwise Windows-1252 / Latin-1.
pub fn decode_win1252(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_owned();
    }
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        let ch = match b {
            0x00..=0x7F | 0xA0..=0xFF => b as char,
            0x80..=0x9F => CP1252_UPPER[(b - 0x80) as usize],
        };
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod raw_encoding_tests {
    use super::*;

    #[test]
    fn raw_b220_matches_furcadia_exe() {
        assert_eq!(decode_b220_raw(b""), 0);
        assert_eq!(decode_b220_raw(&[35, 35]), 0);
        // Byte < 35 treated as digit 0 without losing place value for higher bytes.
        assert_eq!(decode_b220_raw(&[32, 36]), 220);
        // Byte 255 -> 220 digit supported cleanly.
        assert_eq!(decode_b220_raw(&[255]), 220);
    }

    #[test]
    fn raw_b220_str_preserves_high_bytes() {
        let pkt = [35 + 4, 0x80, 0x99, 0xE9, 0xFF, b'x', b'y'];
        let (payload, tail) = decode_b220_str_raw(&pkt).expect("valid raw b220_str");
        assert_eq!(payload, &[0x80, 0x99, 0xE9, 0xFF]);
        assert_eq!(tail, b"xy");
        assert!(decode_b220_str_raw(&[34, b'a']).is_none());
        assert!(decode_b220_str_raw(&[35 + 5, b'a', b'b']).is_none());
    }

    #[test]
    fn base95_signed_and_win1252() {
        assert_eq!(decode_base95_signed(b' ', b' '), 0);
        assert_eq!(decode_base95_signed(b' ', b'~'), 94);
        assert_eq!(decode_base95_signed(31, 126), -1);
        assert_eq!(decode_win1252(&[b'A', 0x92, 0xE9, 0x80]), "A’é€");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base95_roundtrip() {
        for val in [0, 1, 94, 95, 1234, 9024] {
            let encoded = encode_base95(val, 2).unwrap();
            assert_eq!(decode_base95(&encoded).unwrap(), val);
        }
        assert_eq!(encode_base95(9025, 2), Err(EncodingError::Overflow));
    }

    #[test]
    fn base220_roundtrip() {
        for val in [0, 1, 219, 220, 48399] {
            let encoded = encode_base220(val, 2).unwrap();
            assert_eq!(decode_base220(&encoded).unwrap(), val);
        }
        assert_eq!(encode_base220(48400, 2), Err(EncodingError::Overflow));
    }

    #[test]
    fn b220_str_roundtrip() {
        let encoded = encode_b220_str("Furcadia").unwrap();
        assert_eq!(encoded[0], BASE220_OFFSET + 8);
        let (decoded, rest) = decode_b220_str(&encoded).unwrap();
        assert_eq!(decoded, "Furcadia");
        assert!(rest.is_empty());
    }
}
