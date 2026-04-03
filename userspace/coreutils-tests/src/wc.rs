/// Count lines, words, and bytes in input.
/// Returns `(lines, words, bytes)`.
pub fn count(input: &[u8]) -> (usize, usize, usize) {
    let bytes = input.len();
    let mut lines = 0usize;
    let mut words = 0usize;
    let mut in_word = false;

    for &b in input {
        if b == b'\n' {
            lines += 1;
        }
        if b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' {
            in_word = false;
        } else if !in_word {
            in_word = true;
            words += 1;
        }
    }

    (lines, words, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_empty_input_returns_zeros() {
        assert_eq!(count(b""), (0, 0, 0));
    }

    #[test]
    fn count_single_word_no_newline() {
        assert_eq!(count(b"hello"), (0, 1, 5));
    }

    #[test]
    fn count_single_word_with_newline() {
        assert_eq!(count(b"hello\n"), (1, 1, 6));
    }

    #[test]
    fn count_multiple_lines_and_words() {
        // "one two\nthree four\n" → 2 lines, 4 words, 19 bytes
        assert_eq!(count(b"one two\nthree four\n"), (2, 4, 19));
    }

    #[test]
    fn count_line_with_only_whitespace_counts_as_line_zero_words() {
        assert_eq!(count(b"   \n"), (1, 0, 4));
    }

    #[test]
    fn count_tab_separated_words() {
        assert_eq!(count(b"a\tb\tc\n"), (1, 3, 6));
    }

    #[test]
    fn count_multiple_spaces_between_words() {
        assert_eq!(count(b"a  b  c\n"), (1, 3, 8));
    }

    #[test]
    fn count_three_lines_three_words() {
        let input = b"alpha\nbeta\ngamma\n";
        let (lines, words, bytes) = count(input);
        assert_eq!(lines, 3);
        assert_eq!(words, 3);
        assert_eq!(bytes, 17);
    }
}
