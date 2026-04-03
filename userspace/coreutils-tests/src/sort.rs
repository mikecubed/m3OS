use std::cmp::Ordering;

/// Lexicographic comparison of two byte slices.
pub fn lex_cmp(a: &[u8], b: &[u8]) -> Ordering {
    a.cmp(b)
}

fn parse_leading_u64(s: &[u8]) -> u64 {
    let mut v = 0u64;
    for &b in s {
        if b.is_ascii_digit() {
            v = v.wrapping_mul(10).wrapping_add((b - b'0') as u64);
        } else {
            break;
        }
    }
    v
}

/// Numeric comparison: parse leading number from each, fall back to lex on tie.
pub fn num_cmp(a: &[u8], b: &[u8]) -> Ordering {
    let na = parse_leading_u64(a);
    let nb = parse_leading_u64(b);
    na.cmp(&nb).then_with(|| a.cmp(b))
}

/// Sort lines from input. Returns sorted output with newlines.
pub fn sort_lines(input: &[u8], numeric: bool, reverse: bool) -> Vec<u8> {
    if input.is_empty() {
        return Vec::new();
    }

    let mut lines: Vec<&[u8]> = input.split(|&b| b == b'\n').collect();

    // Remove trailing empty element from a final newline
    if lines.last() == Some(&&b""[..]) {
        lines.pop();
    }

    if numeric {
        lines.sort_by(|a, b| num_cmp(a, b));
    } else {
        lines.sort_by(|a, b| lex_cmp(a, b));
    }

    if reverse {
        lines.reverse();
    }

    let mut result = Vec::new();
    for line in lines {
        result.extend_from_slice(line);
        result.push(b'\n');
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering::*;

    // --- lex_cmp ---

    #[test]
    fn lex_cmp_equal_slices() {
        assert_eq!(lex_cmp(b"abc", b"abc"), Equal);
    }

    #[test]
    fn lex_cmp_alphabetic_ordering() {
        assert_eq!(lex_cmp(b"apple", b"banana"), Less);
        assert_eq!(lex_cmp(b"banana", b"apple"), Greater);
    }

    #[test]
    fn lex_cmp_uppercase_before_lowercase_in_ascii() {
        // ASCII 'A'=65, 'a'=97
        assert_eq!(lex_cmp(b"Apple", b"apple"), Less);
    }

    #[test]
    fn lex_cmp_shorter_prefix_is_less() {
        assert_eq!(lex_cmp(b"ab", b"abc"), Less);
    }

    // --- num_cmp ---

    #[test]
    fn num_cmp_simple_integers_ordered_numerically() {
        assert_eq!(num_cmp(b"1", b"2"), Less);
        assert_eq!(num_cmp(b"10", b"9"), Greater);
        assert_eq!(num_cmp(b"100", b"100"), Equal);
    }

    #[test]
    fn num_cmp_two_before_ten_numerically() {
        assert_eq!(num_cmp(b"2", b"10"), Less);
    }

    #[test]
    fn num_cmp_non_numeric_falls_back_to_lex() {
        assert_eq!(num_cmp(b"abc", b"abd"), Less);
    }

    #[test]
    fn num_cmp_zero_prefix_treated_as_zero() {
        assert_eq!(num_cmp(b"0", b"1"), Less);
    }

    // --- sort_lines ---

    #[test]
    fn sort_lines_basic_lexicographic() {
        let input = b"banana\napple\ncherry\n";
        let result = sort_lines(input, false, false);
        assert_eq!(result, b"apple\nbanana\ncherry\n");
    }

    #[test]
    fn sort_lines_numeric_sorts_by_value_not_string() {
        let input = b"10\n9\n2\n100\n";
        let result = sort_lines(input, true, false);
        assert_eq!(result, b"2\n9\n10\n100\n");
    }

    #[test]
    fn sort_lines_lex_puts_10_before_9() {
        let input = b"10\n9\n2\n";
        let result = sort_lines(input, false, false);
        // Lexicographic: "10" < "2" < "9"
        assert_eq!(result, b"10\n2\n9\n");
    }

    #[test]
    fn sort_lines_reverse_order() {
        let input = b"apple\nbanana\ncherry\n";
        let result = sort_lines(input, false, true);
        assert_eq!(result, b"cherry\nbanana\napple\n");
    }

    #[test]
    fn sort_lines_empty_input_returns_empty() {
        let result = sort_lines(b"", false, false);
        assert_eq!(result, b"");
    }

    #[test]
    fn sort_lines_single_line_returns_unchanged() {
        let result = sort_lines(b"only\n", false, false);
        assert_eq!(result, b"only\n");
    }
}
