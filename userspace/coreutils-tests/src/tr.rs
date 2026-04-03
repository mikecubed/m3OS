fn parse_escape(c: u8) -> u8 {
    match c {
        b'n' => b'\n',
        b'r' => b'\r',
        b't' => b'\t',
        b'\\' => b'\\',
        _ => c,
    }
}

/// Expand a set specification (with ranges and escape sequences) into a flat byte list.
fn expand_set_spec(spec: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;

    while i < spec.len() {
        let first = if spec[i] == b'\\' && i + 1 < spec.len() {
            let c = parse_escape(spec[i + 1]);
            i += 2;
            c
        } else {
            let c = spec[i];
            i += 1;
            c
        };

        if i < spec.len() && spec[i] == b'-' && i + 1 < spec.len() {
            i += 1;
            let last = if spec[i] == b'\\' && i + 1 < spec.len() {
                let c = parse_escape(spec[i + 1]);
                i += 2;
                c
            } else {
                let c = spec[i];
                i += 1;
                c
            };
            if first <= last {
                for ch in first..=last {
                    out.push(ch);
                }
            }
        } else {
            out.push(first);
        }
    }
    out
}

/// Build a 256-byte character translation map. Identity by default; set1[i] maps to set2[i].
/// If set2 is shorter, the last character of set2 fills remaining positions.
pub fn build_tr_map(set1: &[u8], set2: &[u8]) -> [u8; 256] {
    let mut map = [0u8; 256];
    for (i, slot) in map.iter_mut().enumerate() {
        *slot = i as u8;
    }

    let expanded1 = expand_set_spec(set1);
    let expanded2 = expand_set_spec(set2);

    if expanded2.is_empty() {
        return map;
    }

    for (i, &ch) in expanded1.iter().enumerate() {
        let replacement = expanded2[i.min(expanded2.len() - 1)];
        map[ch as usize] = replacement;
    }
    map
}

/// Build a 256-entry boolean delete-set from a character set specification.
pub fn build_delete_set(set: &[u8]) -> [bool; 256] {
    let mut delete = [false; 256];
    let expanded = expand_set_spec(set);
    for &ch in &expanded {
        delete[ch as usize] = true;
    }
    delete
}

/// Apply character translation/deletion to input.
/// `squeeze`: if true, squeeze consecutive identical output characters to one.
pub fn apply_tr(input: &[u8], map: &[u8; 256], delete: &[bool; 256], squeeze: bool) -> Vec<u8> {
    let mut result = Vec::with_capacity(input.len());
    let mut last_out: Option<u8> = None;

    for &b in input {
        if delete[b as usize] {
            last_out = None;
            continue;
        }
        let out = map[b as usize];
        if squeeze {
            if Some(out) == last_out {
                continue;
            }
        }
        result.push(out);
        last_out = Some(out);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- build_tr_map ---

    #[test]
    fn build_tr_map_single_char_mapping() {
        let map = build_tr_map(b"a", b"A");
        assert_eq!(map[b'a' as usize], b'A');
        assert_eq!(map[b'b' as usize], b'b'); // identity
    }

    #[test]
    fn build_tr_map_set1_longer_than_set2_fills_with_last() {
        let map = build_tr_map(b"abc", b"X");
        assert_eq!(map[b'a' as usize], b'X');
        assert_eq!(map[b'b' as usize], b'X');
        assert_eq!(map[b'c' as usize], b'X');
    }

    #[test]
    fn build_tr_map_range_lowercase_to_uppercase() {
        let map = build_tr_map(b"a-z", b"A-Z");
        assert_eq!(map[b'a' as usize], b'A');
        assert_eq!(map[b'z' as usize], b'Z');
        assert_eq!(map[b'm' as usize], b'M');
    }

    #[test]
    fn build_tr_map_identity_for_unmapped_chars() {
        let map = build_tr_map(b"x", b"y");
        assert_eq!(map[b'0' as usize], b'0');
        assert_eq!(map[b' ' as usize], b' ');
    }

    // --- build_delete_set ---

    #[test]
    fn build_delete_set_marks_listed_chars() {
        let del = build_delete_set(b"aeiou");
        assert!(del[b'a' as usize]);
        assert!(del[b'e' as usize]);
        assert!(!del[b'b' as usize]);
    }

    #[test]
    fn build_delete_set_with_range() {
        let del = build_delete_set(b"0-9");
        for d in b'0'..=b'9' {
            assert!(
                del[d as usize],
                "digit {} should be in delete set",
                d as char
            );
        }
        assert!(!del[b'a' as usize]);
    }

    // --- apply_tr ---

    #[test]
    fn apply_tr_translates_lowercase_to_uppercase() {
        let map = build_tr_map(b"a-z", b"A-Z");
        let del = [false; 256];
        let result = apply_tr(b"hello world", &map, &del, false);
        assert_eq!(result, b"HELLO WORLD");
    }

    #[test]
    fn apply_tr_deletes_chars_in_delete_set() {
        let map: [u8; 256] = std::array::from_fn(|i| i as u8);
        let del = build_delete_set(b"aeiou");
        let result = apply_tr(b"hello world", &map, &del, false);
        assert_eq!(result, b"hll wrld");
    }

    #[test]
    fn apply_tr_squeeze_consecutive_identical_chars() {
        let map: [u8; 256] = std::array::from_fn(|i| i as u8);
        let del = [false; 256];
        let result = apply_tr(b"aaabbbccc", &map, &del, true);
        assert_eq!(result, b"abc");
    }

    #[test]
    fn apply_tr_squeeze_after_translation() {
        let map = build_tr_map(b"abc", b"X");
        let del = [false; 256];
        // a, b, c all map to X; squeeze should collapse consecutive Xs
        let result = apply_tr(b"aabbc", &map, &del, true);
        assert_eq!(result, b"X");
    }

    #[test]
    fn apply_tr_no_squeeze_keeps_duplicates() {
        let map: [u8; 256] = std::array::from_fn(|i| i as u8);
        let del = [false; 256];
        let result = apply_tr(b"aabb", &map, &del, false);
        assert_eq!(result, b"aabb");
    }

    #[test]
    fn apply_tr_empty_input_returns_empty() {
        let map: [u8; 256] = std::array::from_fn(|i| i as u8);
        let del = [false; 256];
        assert_eq!(apply_tr(b"", &map, &del, false), b"");
    }
}
