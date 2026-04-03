/// Split input into items on newlines (or NUL bytes if null_delimited). Blank items are skipped.
pub fn parse_items(input: &[u8], null_delimited: bool) -> Vec<Vec<u8>> {
    let delimiter = if null_delimited { b'\0' } else { b'\n' };
    let mut items = Vec::new();
    let mut start = 0;

    for (i, &b) in input.iter().enumerate() {
        if b == delimiter {
            if i > start {
                items.push(input[start..i].to_vec());
            }
            start = i + 1;
        }
    }

    // Handle trailing item without delimiter
    if start < input.len() {
        items.push(input[start..].to_vec());
    }

    items
}

/// Build argv for xargs without -I: [base_args..., item1, item2, ...].
pub fn build_argv_append(base_args: &[&[u8]], items: &[Vec<u8>]) -> Vec<Vec<u8>> {
    let mut argv: Vec<Vec<u8>> = base_args.iter().map(|a| a.to_vec()).collect();
    for item in items {
        argv.push(item.clone());
    }
    argv
}

/// Build argv for xargs with -I REPLSTR: replace REPLSTR with item in each base arg.
pub fn build_argv_replace(base_args: &[&[u8]], replstr: &[u8], item: &[u8]) -> Vec<Vec<u8>> {
    base_args
        .iter()
        .map(|arg| {
            if let Some(pos) = arg.windows(replstr.len()).position(|w| w == replstr) {
                let mut out = Vec::new();
                out.extend_from_slice(&arg[..pos]);
                out.extend_from_slice(item);
                out.extend_from_slice(&arg[pos + replstr.len()..]);
                out
            } else {
                arg.to_vec()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse_items ---

    #[test]
    fn parse_items_newline_separated_produces_correct_items() {
        let result = parse_items(b"foo\nbar\nbaz\n", false);
        assert_eq!(
            result,
            vec![b"foo".to_vec(), b"bar".to_vec(), b"baz".to_vec()]
        );
    }

    #[test]
    fn parse_items_nul_separated() {
        let input = b"foo\0bar\0baz\0";
        let result = parse_items(input, true);
        assert_eq!(
            result,
            vec![b"foo".to_vec(), b"bar".to_vec(), b"baz".to_vec()]
        );
    }

    #[test]
    fn parse_items_blank_lines_are_skipped() {
        let result = parse_items(b"foo\n\nbar\n\n", false);
        assert_eq!(result, vec![b"foo".to_vec(), b"bar".to_vec()]);
    }

    #[test]
    fn parse_items_no_trailing_newline_includes_last_item() {
        let result = parse_items(b"foo\nbar", false);
        assert_eq!(result, vec![b"foo".to_vec(), b"bar".to_vec()]);
    }

    #[test]
    fn parse_items_empty_input_returns_empty() {
        assert_eq!(parse_items(b"", false), Vec::<Vec<u8>>::new());
    }

    // --- build_argv_append ---

    #[test]
    fn build_argv_append_basic_appends_items_after_base() {
        let base: &[&[u8]] = &[b"echo"];
        let items = vec![b"hello".to_vec(), b"world".to_vec()];
        let result = build_argv_append(base, &items);
        assert_eq!(
            result,
            vec![b"echo".to_vec(), b"hello".to_vec(), b"world".to_vec()]
        );
    }

    #[test]
    fn build_argv_append_empty_items_returns_only_base() {
        let base: &[&[u8]] = &[b"ls", b"-l"];
        let result = build_argv_append(base, &[]);
        assert_eq!(result, vec![b"ls".to_vec(), b"-l".to_vec()]);
    }

    #[test]
    fn build_argv_append_no_base_args_returns_only_items() {
        let items = vec![b"item1".to_vec()];
        let result = build_argv_append(&[], &items);
        assert_eq!(result, vec![b"item1".to_vec()]);
    }

    // --- build_argv_replace ---

    #[test]
    fn build_argv_replace_replstr_found_in_arg() {
        let base: &[&[u8]] = &[b"cp", b"{}", b"/dest"];
        let result = build_argv_replace(base, b"{}", b"src/file.txt");
        assert_eq!(
            result,
            vec![b"cp".to_vec(), b"src/file.txt".to_vec(), b"/dest".to_vec()]
        );
    }

    #[test]
    fn build_argv_replace_replstr_not_found_passes_through_unchanged() {
        let base: &[&[u8]] = &[b"echo", b"no-replace"];
        let result = build_argv_replace(base, b"{}", b"value");
        assert_eq!(result, vec![b"echo".to_vec(), b"no-replace".to_vec()]);
    }

    #[test]
    fn build_argv_replace_multiple_args_with_replstr() {
        let base: &[&[u8]] = &[b"ln", b"-s", b"{}", b"{}.bak"];
        let result = build_argv_replace(base, b"{}", b"file.txt");
        assert_eq!(
            result,
            vec![
                b"ln".to_vec(),
                b"-s".to_vec(),
                b"file.txt".to_vec(),
                b"file.txt.bak".to_vec(),
            ]
        );
    }
}
