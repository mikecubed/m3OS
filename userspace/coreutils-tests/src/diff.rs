/// Compare two byte slices (whole file contents), split into lines.
/// Returns true if identical (byte-for-byte).
pub fn files_equal(a: &[u8], b: &[u8]) -> bool {
    a == b
}

/// Split content into lines, keeping the newline character at the end of each.
pub fn split_lines(content: &[u8]) -> Vec<&[u8]> {
    let mut lines = Vec::new();
    let mut pos = 0;

    while pos < content.len() {
        match content[pos..].iter().position(|&b| b == b'\n') {
            Some(offset) => {
                lines.push(&content[pos..pos + offset + 1]);
                pos += offset + 1;
            }
            None => {
                lines.push(&content[pos..]);
                break;
            }
        }
    }
    lines
}

fn write_u64_decimal(n: usize, out: &mut Vec<u8>) {
    if n == 0 {
        out.push(b'0');
        return;
    }
    let mut buf = [0u8; 20];
    let mut i = 0;
    let mut v = n;
    while v > 0 {
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        i += 1;
    }
    for &c in buf[..i].iter().rev() {
        out.push(c);
    }
}

/// Build a unified diff: header + all old lines as removals + all new lines as additions.
pub fn build_diff(old_path: &[u8], old: &[u8], new_path: &[u8], new: &[u8]) -> Vec<u8> {
    if old == new {
        return Vec::new();
    }

    let lines1 = split_lines(old);
    let lines2 = split_lines(new);

    let mut out = Vec::new();

    // Header
    out.extend_from_slice(b"--- ");
    out.extend_from_slice(old_path);
    out.push(b'\n');
    out.extend_from_slice(b"+++ ");
    out.extend_from_slice(new_path);
    out.push(b'\n');

    // Hunk header
    out.extend_from_slice(b"@@ -");
    if lines1.is_empty() {
        out.extend_from_slice(b"0,0");
    } else {
        out.extend_from_slice(b"1,");
        write_u64_decimal(lines1.len(), &mut out);
    }
    out.extend_from_slice(b" +");
    if lines2.is_empty() {
        out.extend_from_slice(b"0,0");
    } else {
        out.extend_from_slice(b"1,");
        write_u64_decimal(lines2.len(), &mut out);
    }
    out.extend_from_slice(b" @@\n");

    // Removals
    for line in &lines1 {
        out.push(b'-');
        out.extend_from_slice(line);
        if !line.ends_with(b"\n") {
            out.push(b'\n');
        }
    }

    // Additions
    for line in &lines2 {
        out.push(b'+');
        out.extend_from_slice(line);
        if !line.ends_with(b"\n") {
            out.push(b'\n');
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- files_equal ---

    #[test]
    fn files_equal_identical_content() {
        assert!(files_equal(b"hello\n", b"hello\n"));
    }

    #[test]
    fn files_equal_different_content() {
        assert!(!files_equal(b"foo\n", b"bar\n"));
    }

    #[test]
    fn files_equal_both_empty() {
        assert!(files_equal(b"", b""));
    }

    #[test]
    fn files_equal_empty_vs_nonempty() {
        assert!(!files_equal(b"", b"x"));
    }

    // --- split_lines ---

    #[test]
    fn split_lines_single_line_with_newline() {
        assert_eq!(split_lines(b"hello\n"), vec![&b"hello\n"[..]]);
    }

    #[test]
    fn split_lines_multiple_lines() {
        let lines = split_lines(b"a\nb\nc\n");
        assert_eq!(lines, vec![&b"a\n"[..], &b"b\n"[..], &b"c\n"[..]]);
    }

    #[test]
    fn split_lines_no_trailing_newline() {
        let lines = split_lines(b"a\nb");
        assert_eq!(lines, vec![&b"a\n"[..], &b"b"[..]]);
    }

    #[test]
    fn split_lines_empty_content() {
        let lines = split_lines(b"");
        assert!(lines.is_empty());
    }

    // --- build_diff ---

    #[test]
    fn build_diff_identical_files_returns_empty() {
        let result = build_diff(b"f1", b"hello\n", b"f2", b"hello\n");
        assert_eq!(result, b"");
    }

    #[test]
    fn build_diff_contains_correct_header_lines() {
        let result = build_diff(b"old.txt", b"old\n", b"new.txt", b"new\n");
        let text = std::str::from_utf8(&result).unwrap();
        assert!(
            text.starts_with("--- old.txt\n+++ new.txt\n"),
            "got: {text}"
        );
    }

    #[test]
    fn build_diff_old_lines_prefixed_with_minus() {
        let result = build_diff(b"a", b"remove\n", b"b", b"add\n");
        let text = std::str::from_utf8(&result).unwrap();
        assert!(text.contains("-remove\n"), "got: {text}");
    }

    #[test]
    fn build_diff_new_lines_prefixed_with_plus() {
        let result = build_diff(b"a", b"remove\n", b"b", b"add\n");
        let text = std::str::from_utf8(&result).unwrap();
        assert!(text.contains("+add\n"), "got: {text}");
    }

    #[test]
    fn build_diff_hunk_header_includes_line_counts() {
        let result = build_diff(b"a", b"line1\nline2\n", b"b", b"line3\n");
        let text = std::str::from_utf8(&result).unwrap();
        assert!(text.contains("@@ -1,2 +1,1 @@"), "got: {text}");
    }
}
