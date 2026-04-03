/// Return the first `n` lines of input (newline-terminated).
pub fn head(input: &[u8], n: usize) -> Vec<u8> {
    if n == 0 {
        return Vec::new();
    }
    let mut result = Vec::new();
    let mut count = 0usize;
    let mut pos = 0;

    while pos < input.len() && count < n {
        let nl = input[pos..].iter().position(|&b| b == b'\n');
        match nl {
            Some(offset) => {
                result.extend_from_slice(&input[pos..pos + offset + 1]);
                pos += offset + 1;
                count += 1;
            }
            None => {
                // Last line without trailing newline
                result.extend_from_slice(&input[pos..]);
                break;
            }
        }
    }
    result
}

/// Return the last `n` lines of input.
pub fn tail(input: &[u8], n: usize) -> Vec<u8> {
    if n == 0 || input.is_empty() {
        return Vec::new();
    }

    // Collect line boundaries
    let mut line_starts: Vec<usize> = vec![0];
    for (i, &b) in input.iter().enumerate() {
        if b == b'\n' && i + 1 < input.len() {
            line_starts.push(i + 1);
        }
    }

    let total = line_starts.len();
    let skip = total.saturating_sub(n);
    let start = line_starts[skip];
    input[start..].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- head ---

    #[test]
    fn head_returns_first_line() {
        assert_eq!(head(b"line1\nline2\nline3\n", 1), b"line1\n");
    }

    #[test]
    fn head_returns_first_three_lines() {
        let input = b"a\nb\nc\nd\ne\n";
        assert_eq!(head(input, 3), b"a\nb\nc\n");
    }

    #[test]
    fn head_n_greater_than_total_returns_all() {
        let input = b"a\nb\n";
        assert_eq!(head(input, 100), b"a\nb\n");
    }

    #[test]
    fn head_n_zero_returns_empty() {
        assert_eq!(head(b"a\nb\n", 0), b"");
    }

    #[test]
    fn head_no_trailing_newline_on_last_line() {
        // If n exceeds lines and last line lacks newline, return without newline
        assert_eq!(head(b"a\nb", 2), b"a\nb");
    }

    #[test]
    fn head_empty_input_returns_empty() {
        assert_eq!(head(b"", 5), b"");
    }

    #[test]
    fn head_exactly_n_lines() {
        assert_eq!(head(b"x\ny\nz\n", 3), b"x\ny\nz\n");
    }

    // --- tail ---

    #[test]
    fn tail_returns_last_line() {
        assert_eq!(tail(b"line1\nline2\nline3\n", 1), b"line3\n");
    }

    #[test]
    fn tail_returns_last_three_lines() {
        let input = b"a\nb\nc\nd\ne\n";
        assert_eq!(tail(input, 3), b"c\nd\ne\n");
    }

    #[test]
    fn tail_n_greater_than_total_returns_all() {
        let input = b"a\nb\n";
        assert_eq!(tail(input, 100), b"a\nb\n");
    }

    #[test]
    fn tail_n_zero_returns_empty() {
        assert_eq!(tail(b"a\nb\n", 0), b"");
    }

    #[test]
    fn tail_empty_input_returns_empty() {
        assert_eq!(tail(b"", 3), b"");
    }

    #[test]
    fn tail_single_line_no_newline() {
        assert_eq!(tail(b"only", 1), b"only");
    }
}
