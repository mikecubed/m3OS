/// Simple glob pattern matching supporting `*` as a multi-character wildcard.
/// `?` matches any single character.
pub fn glob_matches(pat: &[u8], s: &[u8]) -> bool {
    match pat.first() {
        None => s.is_empty(),
        Some(&b'*') => {
            let rest_pat = &pat[1..];
            if rest_pat.is_empty() {
                return true;
            }
            for i in 0..=s.len() {
                if glob_matches(rest_pat, &s[i..]) {
                    return true;
                }
            }
            false
        }
        Some(&b'?') => {
            if s.is_empty() {
                false
            } else {
                glob_matches(&pat[1..], &s[1..])
            }
        }
        Some(&pc) => match s.first() {
            None => false,
            Some(&sc) => pc == sc && glob_matches(&pat[1..], &s[1..]),
        },
    }
}

/// Return the base name (portion after last `/`) of a path.
pub fn base_name(path: &[u8]) -> &[u8] {
    match path.iter().rposition(|&b| b == b'/') {
        Some(i) => &path[i + 1..],
        None => path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- glob_matches ---

    #[test]
    fn glob_matches_exact_string() {
        assert!(glob_matches(b"file.txt", b"file.txt"));
    }

    #[test]
    fn glob_matches_different_string_returns_false() {
        assert!(!glob_matches(b"file.txt", b"other.txt"));
    }

    #[test]
    fn glob_matches_star_matches_empty_string() {
        assert!(glob_matches(b"*", b""));
    }

    #[test]
    fn glob_matches_star_matches_any_string() {
        assert!(glob_matches(b"*", b"anything_here"));
    }

    #[test]
    fn glob_matches_star_as_prefix_wildcard() {
        assert!(glob_matches(b"*.txt", b"hello.txt"));
        assert!(!glob_matches(b"*.txt", b"hello.rs"));
    }

    #[test]
    fn glob_matches_star_as_suffix_wildcard() {
        assert!(glob_matches(b"hello*", b"hello world"));
        assert!(!glob_matches(b"hello*", b"world hello"));
    }

    #[test]
    fn glob_matches_star_in_middle() {
        assert!(glob_matches(b"h*o", b"hello"));
        assert!(!glob_matches(b"h*o", b"world"));
    }

    #[test]
    fn glob_matches_question_mark_matches_single_char() {
        assert!(glob_matches(b"f?o", b"foo"));
        assert!(glob_matches(b"f?o", b"fxo"));
        assert!(!glob_matches(b"f?o", b"fo"));
    }

    #[test]
    fn glob_matches_question_mark_does_not_match_empty() {
        assert!(!glob_matches(b"?", b""));
    }

    #[test]
    fn glob_matches_empty_pattern_only_matches_empty_string() {
        assert!(glob_matches(b"", b""));
        assert!(!glob_matches(b"", b"x"));
    }

    #[test]
    fn glob_matches_multiple_stars() {
        assert!(glob_matches(b"*.rs", b"src/main.rs"));
        assert!(glob_matches(b"src/*.rs", b"src/main.rs"));
        assert!(!glob_matches(b"src/*.rs", b"lib/main.rs"));
    }

    // --- base_name ---

    #[test]
    fn base_name_path_with_slashes() {
        assert_eq!(base_name(b"a/b/c/file.txt"), b"file.txt");
    }

    #[test]
    fn base_name_no_slash_returns_full_path() {
        assert_eq!(base_name(b"file.txt"), b"file.txt");
    }

    #[test]
    fn base_name_trailing_slash_returns_empty() {
        assert_eq!(base_name(b"a/b/"), b"");
    }

    #[test]
    fn base_name_root_slash() {
        assert_eq!(base_name(b"/"), b"");
    }
}
