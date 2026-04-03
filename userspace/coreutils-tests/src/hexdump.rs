const HEX: &[u8; 16] = b"0123456789abcdef";

fn byte_to_hex(b: u8) -> [u8; 2] {
    [HEX[(b >> 4) as usize], HEX[(b & 0xf) as usize]]
}

fn offset_to_hex8(mut n: usize) -> [u8; 8] {
    let mut buf = [b'0'; 8];
    let mut i = 8usize;
    while i > 0 {
        i -= 1;
        buf[i] = HEX[n & 0xf];
        n >>= 4;
    }
    buf
}

/// Format a 16-byte chunk as a hexdump line.
/// Format: `XXXXXXXX  HH HH HH HH HH HH HH HH  HH HH HH HH HH HH HH HH  |ASCII...|\n`
pub fn format_hex_line(offset: usize, chunk: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();

    // Offset (8 hex digits)
    out.extend_from_slice(&offset_to_hex8(offset));
    out.extend_from_slice(b"  ");

    // Hex section: 16 slots of "HH " (3 chars each), with extra space after slot 7
    for i in 0..16 {
        if i < chunk.len() {
            let [hi, lo] = byte_to_hex(chunk[i]);
            out.push(hi);
            out.push(lo);
            out.push(b' ');
        } else {
            out.extend_from_slice(b"   ");
        }
        if i == 7 {
            out.push(b' ');
        }
    }

    // ASCII section
    out.extend_from_slice(b" |");
    for i in 0..16 {
        if i < chunk.len() {
            let c = chunk[i];
            out.push(if c.is_ascii_graphic() || c == b' ' {
                c
            } else {
                b'.'
            });
        } else {
            out.push(b' ');
        }
    }
    out.push(b'|');
    out.push(b'\n');

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_hex_line_full_16_byte_chunk_correct_offset() {
        let chunk = b"ABCDEFGHIJKLMNOP";
        let result = format_hex_line(0, chunk);
        let text = std::str::from_utf8(&result).unwrap();
        assert!(text.starts_with("00000000  "), "got: {text}");
    }

    #[test]
    fn format_hex_line_full_16_byte_chunk_correct_hex() {
        let chunk = [0x41u8; 16]; // 'A' = 0x41
        let result = format_hex_line(0, &chunk);
        let text = std::str::from_utf8(&result).unwrap();
        assert!(text.contains("41 41 41 41 41 41 41 41  41"), "got: {text}");
    }

    #[test]
    fn format_hex_line_full_16_byte_chunk_correct_ascii() {
        let chunk = b"ABCDEFGHIJKLMNOP";
        let result = format_hex_line(0, chunk);
        let text = std::str::from_utf8(&result).unwrap();
        assert!(text.contains("|ABCDEFGHIJKLMNOP|"), "got: {text}");
    }

    #[test]
    fn format_hex_line_partial_chunk_pads_hex_with_spaces() {
        let chunk = b"ABC";
        let result = format_hex_line(0, chunk);
        let text = std::str::from_utf8(&result).unwrap();
        // Slots 3-15 should be "   " (3 spaces)
        assert!(text.contains("41 42 43    "), "got: {text}");
    }

    #[test]
    fn format_hex_line_partial_chunk_pads_ascii_with_spaces() {
        let chunk = b"ABC";
        let result = format_hex_line(0, chunk);
        let text = std::str::from_utf8(&result).unwrap();
        // ASCII column: ABC followed by 13 spaces
        assert!(text.contains("|ABC             |"), "got: {text}");
    }

    #[test]
    fn format_hex_line_non_printable_bytes_shown_as_dot() {
        let chunk = [0x00u8, 0x01, 0x02];
        let result = format_hex_line(0, &chunk);
        let text = std::str::from_utf8(&result).unwrap();
        assert!(text.contains("|..."), "got: {text}");
    }

    #[test]
    fn format_hex_line_nonzero_offset_encoded_correctly() {
        let chunk = b"X";
        let result = format_hex_line(0x10, chunk);
        let text = std::str::from_utf8(&result).unwrap();
        assert!(text.starts_with("00000010  "), "got: {text}");
    }

    #[test]
    fn format_hex_line_ends_with_newline() {
        let result = format_hex_line(0, b"A");
        assert_eq!(*result.last().unwrap(), b'\n');
    }
}
