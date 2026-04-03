/// Extract a single field (1-based) from a line using `delim`.
/// Returns None if the field index is out of range.
pub fn cut_field(line: &[u8], field: usize, delim: u8) -> Option<&[u8]> {
    let mut current_field = 1usize;
    let mut start = 0usize;

    for (idx, &b) in line.iter().enumerate() {
        if b == delim {
            if current_field == field {
                return Some(&line[start..idx]);
            }
            current_field += 1;
            start = idx + 1;
        }
    }

    // Last field (no trailing delimiter)
    if current_field == field {
        Some(&line[start..])
    } else {
        None
    }
}

/// Extract character range [start-1, end) from a line (1-based inclusive).
/// Returns the slice, or an empty slice if the range is fully out of bounds.
pub fn cut_chars(line: &[u8], start: usize, end: usize) -> &[u8] {
    if start == 0 {
        return &[];
    }
    let lo = start - 1;
    let hi = end.min(line.len());
    if lo >= hi || lo >= line.len() {
        &[]
    } else {
        &line[lo..hi]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- cut_field ---

    #[test]
    fn cut_field_returns_first_field_tab_delimited() {
        assert_eq!(
            cut_field(b"alpha\tbeta\tgamma", 1, b'\t'),
            Some(&b"alpha"[..])
        );
    }

    #[test]
    fn cut_field_returns_second_field_tab_delimited() {
        assert_eq!(
            cut_field(b"alpha\tbeta\tgamma", 2, b'\t'),
            Some(&b"beta"[..])
        );
    }

    #[test]
    fn cut_field_returns_third_field_tab_delimited() {
        assert_eq!(
            cut_field(b"alpha\tbeta\tgamma", 3, b'\t'),
            Some(&b"gamma"[..])
        );
    }

    #[test]
    fn cut_field_returns_last_field_no_trailing_delimiter() {
        assert_eq!(cut_field(b"a:b:c", 3, b':'), Some(&b"c"[..]));
    }

    #[test]
    fn cut_field_returns_none_when_field_beyond_count() {
        assert_eq!(cut_field(b"a:b:c", 4, b':'), None);
    }

    #[test]
    fn cut_field_with_comma_delimiter_returns_second_field() {
        assert_eq!(
            cut_field(b"one,two,three", 2, b','),
            Some(&b"two"[..]),
            "should extract second comma-delimited field"
        );
    }

    #[test]
    fn cut_field_single_field_no_delimiter_in_line() {
        assert_eq!(cut_field(b"only", 1, b'\t'), Some(&b"only"[..]));
    }

    #[test]
    fn cut_field_single_field_no_delimiter_beyond_first() {
        assert_eq!(cut_field(b"only", 2, b'\t'), None);
    }

    #[test]
    fn cut_field_empty_field_between_delimiters() {
        assert_eq!(cut_field(b"a::b", 2, b':'), Some(&b""[..]));
    }

    #[test]
    fn cut_field_on_empty_line_returns_first_field_as_empty() {
        assert_eq!(cut_field(b"", 1, b'\t'), Some(&b""[..]));
    }

    // --- cut_chars ---

    #[test]
    fn cut_chars_returns_first_character() {
        assert_eq!(cut_chars(b"hello", 1, 1), b"h");
    }

    #[test]
    fn cut_chars_returns_last_character_of_line() {
        assert_eq!(cut_chars(b"hello", 5, 5), b"o");
    }

    #[test]
    fn cut_chars_returns_middle_range() {
        assert_eq!(cut_chars(b"hello", 2, 4), b"ell");
    }

    #[test]
    fn cut_chars_range_extending_beyond_line_is_clamped() {
        assert_eq!(cut_chars(b"hi", 1, 100), b"hi");
    }

    #[test]
    fn cut_chars_on_empty_line_returns_empty() {
        assert_eq!(cut_chars(b"", 1, 3), b"");
    }

    #[test]
    fn cut_chars_single_char_line() {
        assert_eq!(cut_chars(b"x", 1, 1), b"x");
    }

    #[test]
    fn cut_chars_start_beyond_line_length_returns_empty() {
        assert_eq!(cut_chars(b"hi", 10, 12), b"");
    }
}
