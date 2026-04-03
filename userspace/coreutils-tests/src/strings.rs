fn is_string_char(b: u8) -> bool {
    b == b'\t' || b.is_ascii_graphic() || b == b' '
}

/// Extract printable-ASCII sequences of length >= min_len from binary data.
pub fn extract_strings(data: &[u8], min_len: usize) -> Vec<Vec<u8>> {
    let mut result = Vec::new();
    let mut current: Vec<u8> = Vec::new();

    for &b in data {
        if is_string_char(b) {
            current.push(b);
        } else {
            if current.len() >= min_len {
                result.push(current.clone());
            }
            current.clear();
        }
    }

    if current.len() >= min_len {
        result.push(current);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_strings_pure_ascii_text_returns_one_string() {
        let result = extract_strings(b"hello world", 4);
        assert_eq!(result, vec![b"hello world".to_vec()]);
    }

    #[test]
    fn extract_strings_binary_with_embedded_ascii_sequence() {
        let data = b"\x00\x01\x02hello\x00\x03world\x04";
        let result = extract_strings(data, 4);
        assert_eq!(result, vec![b"hello".to_vec(), b"world".to_vec()]);
    }

    #[test]
    fn extract_strings_multiple_sequences_above_min_len() {
        let data = b"\x00abcdef\x00GHIJKL\x00";
        let result = extract_strings(data, 4);
        assert_eq!(result, vec![b"abcdef".to_vec(), b"GHIJKL".to_vec()]);
    }

    #[test]
    fn extract_strings_sequences_shorter_than_min_len_excluded() {
        let data = b"\x00abc\x00abcde\x00";
        let result = extract_strings(data, 4);
        // "abc" is length 3, excluded; "abcde" is length 5, included
        assert_eq!(result, vec![b"abcde".to_vec()]);
    }

    #[test]
    fn extract_strings_no_printable_sequences_returns_empty() {
        let data = b"\x00\x01\x02\x03\x04";
        let result = extract_strings(data, 4);
        assert!(result.is_empty());
    }

    #[test]
    fn extract_strings_min_len_one_returns_all_sequences() {
        let data = b"\x00a\x00bc\x00";
        let result = extract_strings(data, 1);
        assert_eq!(result, vec![b"a".to_vec(), b"bc".to_vec()]);
    }

    #[test]
    fn extract_strings_sequence_at_end_without_terminator() {
        let result = extract_strings(b"\x00trailing", 4);
        assert_eq!(result, vec![b"trailing".to_vec()]);
    }

    #[test]
    fn extract_strings_tab_counts_as_string_char() {
        let data = b"abc\tdef";
        let result = extract_strings(data, 4);
        assert_eq!(result, vec![b"abc\tdef".to_vec()]);
    }
}
