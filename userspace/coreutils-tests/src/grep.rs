/// Fixed-string substring search (equivalent to C strstr).
pub fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if needle.len() > haystack.len() {
        return false;
    }
    for i in 0..=(haystack.len() - needle.len()) {
        if haystack[i..i + needle.len()] == *needle {
            return true;
        }
    }
    false
}

/// Filter lines from input that contain pattern. Returns matching lines with newlines.
pub fn grep_bytes(input: &[u8], pattern: &[u8]) -> Vec<u8> {
    let mut result = Vec::new();
    let mut pos = 0;

    while pos < input.len() {
        let nl = input[pos..].iter().position(|&b| b == b'\n');
        let (line, next) = match nl {
            Some(offset) => (&input[pos..pos + offset], pos + offset + 1),
            None => (&input[pos..], input.len()),
        };
        if contains(line, pattern) {
            result.extend_from_slice(line);
            result.push(b'\n');
        }
        pos = next;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- contains ---

    #[test]
    fn contains_finds_substring_in_middle() {
        assert!(contains(b"hello world", b"world"));
    }

    #[test]
    fn contains_returns_false_when_not_found() {
        assert!(!contains(b"hello world", b"xyz"));
    }

    #[test]
    fn contains_empty_needle_always_true() {
        assert!(contains(b"anything", b""));
    }

    #[test]
    fn contains_empty_needle_in_empty_haystack() {
        assert!(contains(b"", b""));
    }

    #[test]
    fn contains_returns_false_for_non_empty_needle_in_empty_haystack() {
        assert!(!contains(b"", b"x"));
    }

    #[test]
    fn contains_prefix_match() {
        assert!(contains(b"prefix_suffix", b"prefix"));
    }

    #[test]
    fn contains_suffix_match() {
        assert!(contains(b"prefix_suffix", b"suffix"));
    }

    #[test]
    fn contains_overlapping_substring() {
        assert!(contains(b"aababab", b"abab"));
    }

    #[test]
    fn contains_needle_equals_haystack() {
        assert!(contains(b"exact", b"exact"));
    }

    #[test]
    fn contains_needle_longer_than_haystack() {
        assert!(!contains(b"hi", b"hello"));
    }

    // --- grep_bytes ---

    #[test]
    fn grep_bytes_no_matches_returns_empty() {
        let result = grep_bytes(b"foo\nbar\nbaz\n", b"xyz");
        assert_eq!(result, b"");
    }

    #[test]
    fn grep_bytes_single_match_returns_line_with_newline() {
        let result = grep_bytes(b"foo\nbar\nbaz\n", b"bar");
        assert_eq!(result, b"bar\n");
    }

    #[test]
    fn grep_bytes_multiple_matches_returns_all_matching_lines() {
        let result = grep_bytes(b"apple\nbanana\napricot\n", b"ap");
        assert_eq!(result, b"apple\napricot\n");
    }

    #[test]
    fn grep_bytes_last_line_without_newline_is_included() {
        let result = grep_bytes(b"foo\nbar", b"bar");
        assert_eq!(result, b"bar\n");
    }

    #[test]
    fn grep_bytes_empty_input_returns_empty() {
        let result = grep_bytes(b"", b"x");
        assert_eq!(result, b"");
    }

    #[test]
    fn grep_bytes_empty_pattern_matches_all_lines() {
        let result = grep_bytes(b"a\nb\n", b"");
        assert_eq!(result, b"a\nb\n");
    }
}
