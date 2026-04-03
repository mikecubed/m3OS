#[derive(Debug, PartialEq, Clone)]
pub struct HunkLine {
    /// b' ' = context, b'+' = added, b'-' = removed
    pub kind: u8,
    pub text: Vec<u8>,
}

#[derive(Debug, PartialEq, Clone)]
pub struct Hunk {
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
    pub lines: Vec<HunkLine>,
}

#[derive(Debug, PartialEq, Clone)]
pub struct FilePatch {
    pub old_path: Vec<u8>,
    pub new_path: Vec<u8>,
    pub hunks: Vec<Hunk>,
}

fn trim_path(s: &[u8]) -> Vec<u8> {
    let end = s
        .iter()
        .position(|&b| b == b'\n' || b == b'\t' || b == b' ')
        .unwrap_or(s.len());
    s[..end].to_vec()
}

fn parse_range_in_header(s: &[u8]) -> Option<(usize, usize, &[u8])> {
    // Parse "start[,count]" from s; returns (start, count, rest)
    let mut i = 0;
    while i < s.len() && s[i].is_ascii_digit() {
        i += 1;
    }
    if i == 0 {
        return None;
    }
    let start: usize = std::str::from_utf8(&s[..i]).ok()?.parse().ok()?;
    if i < s.len() && s[i] == b',' {
        i += 1;
        let j_start = i;
        while i < s.len() && s[i].is_ascii_digit() {
            i += 1;
        }
        if i == j_start {
            return None;
        }
        let count: usize = std::str::from_utf8(&s[j_start..i]).ok()?.parse().ok()?;
        Some((start, count, &s[i..]))
    } else {
        Some((start, 1, &s[i..]))
    }
}

fn parse_hunk_header(line: &[u8]) -> Option<Hunk> {
    // Format: @@ -old_start[,old_count] +new_start[,new_count] @@...
    let line = line.strip_prefix(b"@@ ")?;
    let line = line.strip_prefix(b"-")?;

    let (old_start, old_count, rest) = parse_range_in_header(line)?;
    let rest = rest.strip_prefix(b" +")?;
    let (new_start, new_count, _) = parse_range_in_header(rest)?;

    Some(Hunk {
        old_start,
        old_count,
        new_start,
        new_count,
        lines: Vec::new(),
    })
}

/// Parse a unified diff into a list of file patches.
pub fn parse_patch(input: &[u8]) -> Result<Vec<FilePatch>, &'static str> {
    let raw_lines: Vec<&[u8]> = input.split(|&b| b == b'\n').collect();
    let mut patches = Vec::new();
    let mut i = 0;

    while i < raw_lines.len() {
        let line = raw_lines[i];

        if !line.starts_with(b"--- ") {
            i += 1;
            continue;
        }

        let old_path = trim_path(&line[4..]);
        i += 1;

        if i >= raw_lines.len() || !raw_lines[i].starts_with(b"+++ ") {
            return Err("expected +++ line after ---");
        }
        let new_path = trim_path(&raw_lines[i][4..]);
        i += 1;

        let mut hunks = Vec::new();

        while i < raw_lines.len() && raw_lines[i].starts_with(b"@@ ") {
            let mut hunk = parse_hunk_header(raw_lines[i]).ok_or("bad hunk header")?;
            i += 1;

            while i < raw_lines.len() {
                let hl = raw_lines[i];
                if hl.is_empty() {
                    // Could be blank line between hunks
                    i += 1;
                    break;
                }
                match hl[0] {
                    b' ' | b'+' | b'-' => {
                        let kind = hl[0];
                        let mut text = hl[1..].to_vec();
                        text.push(b'\n');
                        hunk.lines.push(HunkLine { kind, text });
                        i += 1;
                    }
                    b'@' | b'-' if hl.starts_with(b"@@ ") => break,
                    _ => break,
                }
            }

            hunks.push(hunk);
        }

        if hunks.is_empty() {
            return Err("no hunks found");
        }

        patches.push(FilePatch {
            old_path,
            new_path,
            hunks,
        });
    }

    Ok(patches)
}

/// Apply a single hunk to the `old_count` source lines for this hunk.
/// Returns the resulting new lines, or an error if context doesn't match.
pub fn apply_hunk(source_lines: &[Vec<u8>], hunk: &Hunk) -> Result<Vec<Vec<u8>>, &'static str> {
    let mut result = Vec::new();
    let mut src_idx = 0;

    for hl in &hunk.lines {
        match hl.kind {
            b' ' => {
                if src_idx >= source_lines.len() {
                    return Err("context line beyond source");
                }
                if source_lines[src_idx] != hl.text {
                    return Err("context mismatch");
                }
                result.push(source_lines[src_idx].clone());
                src_idx += 1;
            }
            b'-' => {
                if src_idx >= source_lines.len() {
                    return Err("removed line beyond source");
                }
                if source_lines[src_idx] != hl.text {
                    return Err("removed line mismatch");
                }
                src_idx += 1;
            }
            b'+' => {
                result.push(hl.text.clone());
            }
            _ => return Err("unknown hunk line kind"),
        }
    }

    Ok(result)
}

fn split_into_lines(content: &[u8]) -> Vec<Vec<u8>> {
    let mut lines = Vec::new();
    let mut pos = 0;
    while pos < content.len() {
        match content[pos..].iter().position(|&b| b == b'\n') {
            Some(offset) => {
                lines.push(content[pos..pos + offset + 1].to_vec());
                pos += offset + 1;
            }
            None => {
                lines.push(content[pos..].to_vec());
                break;
            }
        }
    }
    lines
}

/// Apply all hunks in a FilePatch to source content. Returns new content.
pub fn apply_file_patch(source: &[u8], patch: &FilePatch) -> Result<Vec<u8>, &'static str> {
    let mut lines = split_into_lines(source);
    let mut offset: isize = 0;

    for hunk in &patch.hunks {
        let start = (hunk.old_start as isize - 1 + offset) as usize;
        let end = start + hunk.old_count;

        if end > lines.len() {
            return Err("hunk out of range");
        }

        let new_lines = apply_hunk(&lines[start..end], hunk)?;
        let delta = new_lines.len() as isize - hunk.old_count as isize;
        lines.splice(start..end, new_lines);
        offset += delta;
    }

    let mut result = Vec::new();
    for line in &lines {
        result.extend_from_slice(line);
    }
    Ok(result)
}

/// Strip N leading path components from a path (components separated by `/`).
/// Returns None if there are fewer than N separators.
pub fn strip_components(path: &[u8], n: usize) -> Option<&[u8]> {
    if n == 0 {
        return Some(path);
    }
    let mut remaining = path;
    for _ in 0..n {
        match remaining.iter().position(|&b| b == b'/') {
            Some(pos) => remaining = &remaining[pos + 1..],
            None => return None,
        }
    }
    Some(remaining)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_hunk_line(kind: u8, text: &[u8]) -> HunkLine {
        let mut t = text.to_vec();
        if !t.ends_with(b"\n") {
            t.push(b'\n');
        }
        HunkLine { kind, text: t }
    }

    // --- parse_patch ---

    #[test]
    fn parse_patch_minimal_single_hunk() {
        // Note: b"..." line continuations strip leading whitespace, so we
        // concatenate explicit byte arrays to preserve the context-line space.
        let patch: &[u8] = b"--- a/file.txt\n+++ b/file.txt\n@@ -1,1 +1,2 @@\n context\n+added\n";
        let patches = parse_patch(patch).expect("should parse");
        assert_eq!(patches.len(), 1);
        let fp = &patches[0];
        assert_eq!(fp.old_path, b"a/file.txt");
        assert_eq!(fp.new_path, b"b/file.txt");
        assert_eq!(fp.hunks.len(), 1);
        let h = &fp.hunks[0];
        assert_eq!(h.old_start, 1);
        assert_eq!(h.old_count, 1);
        assert_eq!(h.new_start, 1);
        assert_eq!(h.new_count, 2);
        assert_eq!(h.lines.len(), 2);
        assert_eq!(h.lines[0].kind, b' ');
        assert_eq!(h.lines[1].kind, b'+');
    }

    #[test]
    fn parse_patch_new_file_from_dev_null() {
        let patch = b"\
--- /dev/null\n\
+++ b/newfile.txt\n\
@@ -0,0 +1,1 @@\n\
+hello\n\
";
        let patches = parse_patch(patch).expect("should parse");
        assert_eq!(patches[0].old_path, b"/dev/null");
        assert_eq!(patches[0].hunks[0].lines[0].kind, b'+');
    }

    #[test]
    fn parse_patch_invalid_missing_plus_plus_plus_returns_err() {
        let patch = b"--- a/file.txt\nnot a +++ line\n";
        assert!(parse_patch(patch).is_err());
    }

    #[test]
    fn parse_patch_no_hunks_returns_err() {
        let patch = b"--- a/f\n+++ b/f\n";
        assert!(parse_patch(patch).is_err());
    }

    // --- apply_hunk ---

    #[test]
    fn apply_hunk_adds_line_after_context() {
        let source = vec![b"context\n".to_vec()];
        let hunk = Hunk {
            old_start: 1,
            old_count: 1,
            new_start: 1,
            new_count: 2,
            lines: vec![
                make_hunk_line(b' ', b"context"),
                make_hunk_line(b'+', b"added"),
            ],
        };
        let result = apply_hunk(&source, &hunk).unwrap();
        assert_eq!(result, vec![b"context\n".to_vec(), b"added\n".to_vec()]);
    }

    #[test]
    fn apply_hunk_removes_line() {
        let source = vec![b"remove_me\n".to_vec(), b"keep_me\n".to_vec()];
        let hunk = Hunk {
            old_start: 1,
            old_count: 2,
            new_start: 1,
            new_count: 1,
            lines: vec![
                make_hunk_line(b'-', b"remove_me"),
                make_hunk_line(b' ', b"keep_me"),
            ],
        };
        let result = apply_hunk(&source, &hunk).unwrap();
        assert_eq!(result, vec![b"keep_me\n".to_vec()]);
    }

    #[test]
    fn apply_hunk_context_mismatch_returns_err() {
        let source = vec![b"actual\n".to_vec()];
        let hunk = Hunk {
            old_start: 1,
            old_count: 1,
            new_start: 1,
            new_count: 1,
            lines: vec![make_hunk_line(b' ', b"expected")],
        };
        assert!(apply_hunk(&source, &hunk).is_err());
    }

    #[test]
    fn apply_hunk_context_beyond_source_returns_err() {
        let source: Vec<Vec<u8>> = vec![];
        let hunk = Hunk {
            old_start: 1,
            old_count: 0,
            new_start: 1,
            new_count: 1,
            lines: vec![make_hunk_line(b' ', b"context")],
        };
        assert!(apply_hunk(&source, &hunk).is_err());
    }

    // --- apply_file_patch ---

    #[test]
    fn apply_file_patch_adds_line() {
        let source = b"line1\nline2\n";
        let patch = FilePatch {
            old_path: b"file".to_vec(),
            new_path: b"file".to_vec(),
            hunks: vec![Hunk {
                old_start: 1,
                old_count: 1,
                new_start: 1,
                new_count: 2,
                lines: vec![
                    make_hunk_line(b' ', b"line1"),
                    make_hunk_line(b'+', b"inserted"),
                ],
            }],
        };
        let result = apply_file_patch(source, &patch).unwrap();
        assert_eq!(result, b"line1\ninserted\nline2\n");
    }

    #[test]
    fn apply_file_patch_removes_line() {
        let source = b"keep\nremove\nkeep2\n";
        let patch = FilePatch {
            old_path: b"file".to_vec(),
            new_path: b"file".to_vec(),
            hunks: vec![Hunk {
                old_start: 1,
                old_count: 3,
                new_start: 1,
                new_count: 2,
                lines: vec![
                    make_hunk_line(b' ', b"keep"),
                    make_hunk_line(b'-', b"remove"),
                    make_hunk_line(b' ', b"keep2"),
                ],
            }],
        };
        let result = apply_file_patch(source, &patch).unwrap();
        assert_eq!(result, b"keep\nkeep2\n");
    }

    #[test]
    fn apply_file_patch_replaces_line() {
        let source = b"old_content\n";
        let patch = FilePatch {
            old_path: b"file".to_vec(),
            new_path: b"file".to_vec(),
            hunks: vec![Hunk {
                old_start: 1,
                old_count: 1,
                new_start: 1,
                new_count: 1,
                lines: vec![
                    make_hunk_line(b'-', b"old_content"),
                    make_hunk_line(b'+', b"new_content"),
                ],
            }],
        };
        let result = apply_file_patch(source, &patch).unwrap();
        assert_eq!(result, b"new_content\n");
    }

    #[test]
    fn apply_file_patch_multi_hunk() {
        let source = b"a\nb\nc\nd\n";
        let patch = FilePatch {
            old_path: b"file".to_vec(),
            new_path: b"file".to_vec(),
            hunks: vec![
                Hunk {
                    old_start: 1,
                    old_count: 1,
                    new_start: 1,
                    new_count: 1,
                    lines: vec![make_hunk_line(b'-', b"a"), make_hunk_line(b'+', b"A")],
                },
                Hunk {
                    old_start: 3,
                    old_count: 1,
                    new_start: 3,
                    new_count: 1,
                    lines: vec![make_hunk_line(b'-', b"c"), make_hunk_line(b'+', b"C")],
                },
            ],
        };
        let result = apply_file_patch(source, &patch).unwrap();
        assert_eq!(result, b"A\nb\nC\nd\n");
    }

    // --- strip_components ---

    #[test]
    fn strip_components_zero_returns_full_path() {
        assert_eq!(
            strip_components(b"a/b/c/file.txt", 0),
            Some(&b"a/b/c/file.txt"[..])
        );
    }

    #[test]
    fn strip_components_one_strips_first_component() {
        assert_eq!(
            strip_components(b"a/b/c/file.txt", 1),
            Some(&b"b/c/file.txt"[..])
        );
    }

    #[test]
    fn strip_components_two_strips_two_components() {
        assert_eq!(
            strip_components(b"a/b/c/file.txt", 2),
            Some(&b"c/file.txt"[..])
        );
    }

    #[test]
    fn strip_components_exactly_depth_returns_filename() {
        assert_eq!(strip_components(b"a/b/file.txt", 2), Some(&b"file.txt"[..]));
    }

    #[test]
    fn strip_components_beyond_depth_returns_none() {
        assert_eq!(strip_components(b"a/b/c", 4), None);
    }

    #[test]
    fn strip_components_leading_slash_counts_as_separator() {
        // "/a/b" with n=1: finds first '/' at 0, remaining is "a/b"
        assert_eq!(strip_components(b"/a/b", 1), Some(&b"a/b"[..]));
    }

    #[test]
    fn strip_components_no_slashes_and_n_one_returns_none() {
        assert_eq!(strip_components(b"file.txt", 1), None);
    }
}
