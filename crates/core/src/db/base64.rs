//! Base64, for the one place a record's bytes have to cross a JSON boundary.
//!
//! A record is a byte container, and JSON strings are UTF-8. A sub-value that
//! is not valid UTF-8 therefore cannot be a JSON string at all - it used to be
//! passed through `String::from_utf8_lossy`, which is how a write could be
//! acknowledged and read back as something else. Such a value is sent as
//! `{"$base64": "..."}` instead, and decoded back to the same bytes here.
//!
//! Hand-rolled rather than pulled in, for the same reason `crc32c` in
//! [`crate::db::hashfile`] is: it is forty lines, it is exercised by the round
//! trip tests below, and a dependency on the write path is a dependency to
//! audit forever.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const PAD: u8 = b'=';

/// Standard base64 with padding.
pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;

        out.push(ALPHABET[(triple >> 18) as usize & 0x3F] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 0x3F] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 0x3F] as char
        } else {
            PAD as char
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 0x3F] as char
        } else {
            PAD as char
        });
    }
    out
}

/// The inverse of [`encode`], or `None` for anything that is not base64.
///
/// Strict on purpose: a client that sends a malformed payload gets a refusal,
/// not a best guess at what it meant. Guessing here would put arbitrary bytes
/// into a record on the strength of a typo.
pub fn decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let mut triple = 0u32;
        let mut taken = 0;
        for (i, &c) in chunk.iter().enumerate() {
            if c == PAD {
                // Padding is only ever the last one or two characters.
                if i < 2 || chunk[i..].iter().any(|&p| p != PAD) {
                    return None;
                }
                break;
            }
            let six = match c {
                b'A'..=b'Z' => c - b'A',
                b'a'..=b'z' => c - b'a' + 26,
                b'0'..=b'9' => c - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                _ => return None,
            };
            triple |= (six as u32) << (18 - 6 * i);
            taken += 1;
        }
        match taken {
            4 => out.extend_from_slice(&[(triple >> 16) as u8, (triple >> 8) as u8, triple as u8]),
            3 => out.extend_from_slice(&[(triple >> 16) as u8, (triple >> 8) as u8]),
            2 => out.push((triple >> 16) as u8),
            _ => return None,
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rfc_vectors_encode_as_published() {
        assert_eq!(encode(b""), "");
        assert_eq!(encode(b"f"), "Zg==");
        assert_eq!(encode(b"fo"), "Zm8=");
        assert_eq!(encode(b"foo"), "Zm9v");
        assert_eq!(encode(b"foob"), "Zm9vYg==");
        assert_eq!(encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn every_byte_survives_a_round_trip_at_every_length() {
        // Every length modulo 3, so both padding cases are covered, over a
        // payload that includes the mark bytes and every other value a record
        // could hold.
        let all: Vec<u8> = (0..=255u8).collect();
        for len in 0..all.len() {
            let slice = &all[..len];
            let encoded = encode(slice);
            assert_eq!(decode(&encoded).as_deref(), Some(slice), "round trip failed at {}", len);
        }
    }

    #[test]
    fn malformed_input_is_refused_rather_than_guessed() {
        assert_eq!(decode("Zg="), None, "length not a multiple of four");
        assert_eq!(decode("Zg=="), Some(b"f".to_vec()));
        assert_eq!(decode("Z!=="), None, "character outside the alphabet");
        assert_eq!(decode("===="), None, "nothing but padding");
        assert_eq!(decode("Z==g"), None, "padding in the middle");
    }
}
