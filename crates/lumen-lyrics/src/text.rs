//! Text decoding for `.lrc` files: UTF-8 (with or without BOM), UTF-16 with
//! a BOM, and Latin-1 for old files that are not valid UTF-8.

pub fn decode_text(buf: &[u8]) -> String {
    let utf16 = |le: bool, b: &[u8]| -> String {
        let units: Vec<u16> = b
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| {
                if le {
                    u16::from_le_bytes(*c)
                } else {
                    u16::from_be_bytes(*c)
                }
            })
            .collect();
        String::from_utf16_lossy(&units)
    };
    match buf {
        [0xFF, 0xFE, rest @ ..] => utf16(true, rest),
        [0xFE, 0xFF, rest @ ..] => utf16(false, rest),
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        _ => match std::str::from_utf8(buf) {
            Ok(s) => s.to_string(),
            // Latin-1: every byte is the code point of the same value.
            Err(_) => buf.iter().map(|&b| char::from(b)).collect(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::decode_text;

    #[test]
    fn encodings() {
        assert_eq!(decode_text("[00:01.00]മഴ".as_bytes()), "[00:01.00]മഴ");
        let mut bom = vec![0xEF, 0xBB, 0xBF];
        bom.extend_from_slice("ñ".as_bytes());
        assert_eq!(decode_text(&bom), "ñ");
        let mut le = vec![0xFF, 0xFE];
        for u in "नमस्ते".encode_utf16() {
            le.extend_from_slice(&u.to_le_bytes());
        }
        assert_eq!(decode_text(&le), "नमस्ते");
        let mut be = vec![0xFE, 0xFF];
        for u in "Año".encode_utf16() {
            be.extend_from_slice(&u.to_be_bytes());
        }
        assert_eq!(decode_text(&be), "Año");
        assert_eq!(decode_text(&[b'A', 0xF1, b'o']), "Año");
    }
}
