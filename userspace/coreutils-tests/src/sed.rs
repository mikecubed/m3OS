pub enum SedCmd {
    Subst {
        old: Vec<u8>,
        new: Vec<u8>,
        global: bool,
    },
    PrintRange {
        start: usize,
        end: usize,
    },
    DeleteRange {
        start: usize,
        end: usize,
    },
}

fn parse_number(s: &[u8]) -> Option<(usize, &[u8])> {
    if s.is_empty() || !s[0].is_ascii_digit() {
        return None;
    }
    let mut n = 0usize;
    let mut i = 0;
    while i < s.len() && s[i].is_ascii_digit() {
        n = n * 10 + (s[i] - b'0') as usize;
        i += 1;
    }
    Some((n, &s[i..]))
}

/// Parse a sed script string into a SedCmd. Returns None on parse error.
pub fn parse_sed_script(script: &[u8]) -> Option<SedCmd> {
    if script.is_empty() {
        return None;
    }

    if script[0] == b's' {
        if script.len() < 2 {
            return None;
        }
        let delim = script[1];
        let rest = &script[2..];

        let old_end = rest.iter().position(|&b| b == delim)?;
        if old_end == 0 {
            return None; // empty pattern not supported
        }
        let old = rest[..old_end].to_vec();

        let rest2 = &rest[old_end + 1..];
        let new_end = rest2.iter().position(|&b| b == delim)?;
        let new_text = rest2[..new_end].to_vec();

        let flags = &rest2[new_end + 1..];
        let global = match flags {
            b"g" => true,
            b"" => false,
            _ => return None, // unknown flag
        };

        return Some(SedCmd::Subst {
            old,
            new: new_text,
            global,
        });
    }

    // Try range or single-line address commands
    let (start, rest) = parse_number(script)?;

    if rest.first() == Some(&b',') {
        let (end, rest2) = parse_number(&rest[1..])?;
        return match rest2 {
            b"p" => Some(SedCmd::PrintRange { start, end }),
            b"d" => Some(SedCmd::DeleteRange { start, end }),
            _ => None,
        };
    }

    match rest {
        b"p" => Some(SedCmd::PrintRange { start, end: start }),
        b"d" => Some(SedCmd::DeleteRange { start, end: start }),
        _ => None,
    }
}

/// Apply substitution to a single line (without trailing newline).
/// Returns modified line without trailing newline.
pub fn apply_subst(line: &[u8], old: &[u8], new_text: &[u8], global: bool) -> Vec<u8> {
    if old.is_empty() {
        return line.to_vec();
    }
    let mut result = Vec::new();
    let mut pos = 0;
    let mut replaced = false;

    while pos <= line.len() {
        if !global && replaced {
            result.extend_from_slice(&line[pos..]);
            break;
        }
        if pos + old.len() <= line.len() && line[pos..pos + old.len()] == *old {
            result.extend_from_slice(new_text);
            pos += old.len();
            replaced = true;
        } else if pos < line.len() {
            result.push(line[pos]);
            pos += 1;
        } else {
            break;
        }
    }
    result
}

/// Process all lines with the given command. quiet=true suppresses default output.
pub fn process_sed(input: &[u8], cmd: &SedCmd, quiet: bool) -> Vec<u8> {
    let mut result = Vec::new();
    let mut pos = 0;
    let mut line_no = 0usize;

    while pos <= input.len() {
        let nl = input[pos..].iter().position(|&b| b == b'\n');
        let (line, next) = match nl {
            Some(offset) => (&input[pos..pos + offset], pos + offset + 1),
            None => {
                if pos < input.len() {
                    (&input[pos..], input.len() + 1)
                } else {
                    break;
                }
            }
        };

        line_no += 1;

        match cmd {
            SedCmd::Subst { old, new, global } => {
                if !quiet {
                    let out = apply_subst(line, old, new, *global);
                    result.extend_from_slice(&out);
                    result.push(b'\n');
                }
            }
            SedCmd::PrintRange { start, end } => {
                if !quiet {
                    result.extend_from_slice(line);
                    result.push(b'\n');
                }
                if line_no >= *start && line_no <= *end {
                    result.extend_from_slice(line);
                    result.push(b'\n');
                }
            }
            SedCmd::DeleteRange { start, end } => {
                if !(line_no >= *start && line_no <= *end) && !quiet {
                    result.extend_from_slice(line);
                    result.push(b'\n');
                }
            }
        }

        pos = next;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse_sed_script ---

    #[test]
    fn parse_sed_script_basic_substitution() {
        match parse_sed_script(b"s/foo/bar/") {
            Some(SedCmd::Subst { old, new, global }) => {
                assert_eq!(old, b"foo");
                assert_eq!(new, b"bar");
                assert!(!global);
            }
            _ => panic!("expected Subst"),
        }
    }

    #[test]
    fn parse_sed_script_global_substitution() {
        match parse_sed_script(b"s/foo/bar/g") {
            Some(SedCmd::Subst { old, new, global }) => {
                assert_eq!(old, b"foo");
                assert_eq!(new, b"bar");
                assert!(global);
            }
            _ => panic!("expected global Subst"),
        }
    }

    #[test]
    fn parse_sed_script_alternate_delimiter() {
        match parse_sed_script(b"s|a|b|") {
            Some(SedCmd::Subst { old, new, global }) => {
                assert_eq!(old, b"a");
                assert_eq!(new, b"b");
                assert!(!global);
            }
            _ => panic!("expected Subst with | delimiter"),
        }
    }

    #[test]
    fn parse_sed_script_print_range() {
        match parse_sed_script(b"1,3p") {
            Some(SedCmd::PrintRange { start, end }) => {
                assert_eq!(start, 1);
                assert_eq!(end, 3);
            }
            _ => panic!("expected PrintRange"),
        }
    }

    #[test]
    fn parse_sed_script_delete_single_line() {
        match parse_sed_script(b"2d") {
            Some(SedCmd::DeleteRange { start, end }) => {
                assert_eq!(start, 2);
                assert_eq!(end, 2);
            }
            _ => panic!("expected DeleteRange"),
        }
    }

    #[test]
    fn parse_sed_script_delete_range() {
        match parse_sed_script(b"1,2d") {
            Some(SedCmd::DeleteRange { start, end }) => {
                assert_eq!(start, 1);
                assert_eq!(end, 2);
            }
            _ => panic!("expected DeleteRange 1,2"),
        }
    }

    #[test]
    fn parse_sed_script_invalid_returns_none() {
        assert!(parse_sed_script(b"invalid").is_none());
    }

    #[test]
    fn parse_sed_script_empty_returns_none() {
        assert!(parse_sed_script(b"").is_none());
    }

    #[test]
    fn parse_sed_script_subst_empty_pattern_returns_none() {
        assert!(parse_sed_script(b"s//bar/").is_none());
    }

    // --- apply_subst ---

    #[test]
    fn apply_subst_basic_replacement() {
        let result = apply_subst(b"foo bar foo", b"foo", b"baz", false);
        assert_eq!(result, b"baz bar foo");
    }

    #[test]
    fn apply_subst_global_replaces_all_occurrences() {
        let result = apply_subst(b"foo bar foo", b"foo", b"baz", true);
        assert_eq!(result, b"baz bar baz");
    }

    #[test]
    fn apply_subst_pattern_not_found_returns_original() {
        let result = apply_subst(b"hello", b"xyz", b"abc", false);
        assert_eq!(result, b"hello");
    }

    #[test]
    fn apply_subst_empty_old_returns_line_unchanged() {
        let result = apply_subst(b"hello", b"", b"X", false);
        assert_eq!(result, b"hello");
    }

    #[test]
    fn apply_subst_replace_with_empty_string() {
        let result = apply_subst(b"remove me please", b"remove ", b"", true);
        assert_eq!(result, b"me please");
    }

    #[test]
    fn apply_subst_non_global_replaces_only_first() {
        let result = apply_subst(b"aaa", b"a", b"b", false);
        assert_eq!(result, b"baa");
    }

    #[test]
    fn apply_subst_global_replaces_all_adjacent() {
        let result = apply_subst(b"aaa", b"a", b"b", true);
        assert_eq!(result, b"bbb");
    }

    // --- process_sed ---

    #[test]
    fn process_sed_substitution_over_multiline_input() {
        let cmd = SedCmd::Subst {
            old: b"x".to_vec(),
            new: b"y".to_vec(),
            global: false,
        };
        let result = process_sed(b"ax\nbx\ncx\n", &cmd, false);
        assert_eq!(result, b"ay\nby\ncy\n");
    }

    #[test]
    fn process_sed_substitution_quiet_suppresses_output() {
        let cmd = SedCmd::Subst {
            old: b"x".to_vec(),
            new: b"y".to_vec(),
            global: false,
        };
        let result = process_sed(b"ax\nbx\n", &cmd, true);
        assert_eq!(result, b"");
    }

    #[test]
    fn process_sed_print_range_quiet_prints_only_range() {
        let cmd = SedCmd::PrintRange { start: 2, end: 3 };
        let result = process_sed(b"line1\nline2\nline3\nline4\n", &cmd, true);
        assert_eq!(result, b"line2\nline3\n");
    }

    #[test]
    fn process_sed_print_range_not_quiet_duplicates_range_lines() {
        let cmd = SedCmd::PrintRange { start: 2, end: 2 };
        let result = process_sed(b"a\nb\nc\n", &cmd, false);
        // b is printed twice (default + explicit)
        assert_eq!(result, b"a\nb\nb\nc\n");
    }

    #[test]
    fn process_sed_delete_range_removes_specified_lines() {
        let cmd = SedCmd::DeleteRange { start: 2, end: 3 };
        let result = process_sed(b"line1\nline2\nline3\nline4\n", &cmd, false);
        assert_eq!(result, b"line1\nline4\n");
    }

    #[test]
    fn process_sed_delete_range_quiet_suppresses_all() {
        let cmd = SedCmd::DeleteRange { start: 1, end: 2 };
        let result = process_sed(b"a\nb\nc\n", &cmd, true);
        assert_eq!(result, b"");
    }
}
