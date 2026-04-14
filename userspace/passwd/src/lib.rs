//! Shared passwd helpers that stay usable from both the no_std binary and host-side tests.
#![no_std]

pub fn requested_username<'a>(args: &'a [&'a str]) -> Option<&'a [u8]> {
    args.get(1).map(|name| name.as_bytes())
}

pub fn user_exists(passwd: &[u8], username: &[u8]) -> bool {
    find_username(passwd, username).is_some()
}

fn find_username<'a>(passwd: &'a [u8], username: &[u8]) -> Option<&'a [u8]> {
    for line in passwd.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let Some(colon) = line.iter().position(|&b| b == b':') else {
            continue;
        };
        if &line[..colon] == username {
            return Some(&line[..colon]);
        }
    }
    None
}

pub fn build_hash_field(salt_hex: &[u8], hash_hex: &[u8], out: &mut [u8]) -> Option<usize> {
    let mut pos = 0usize;
    append_bytes(out, &mut pos, b"$sha256i$10000$").ok()?;
    append_bytes(out, &mut pos, salt_hex).ok()?;
    append_bytes(out, &mut pos, b"$").ok()?;
    append_bytes(out, &mut pos, hash_hex).ok()?;
    Some(pos)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShadowRewriteError {
    UserNotFound,
    OutputTooLarge,
}

pub fn rewrite_shadow_file(
    shadow: &[u8],
    username: &[u8],
    hash_field: &[u8],
    out: &mut [u8],
) -> Result<usize, ShadowRewriteError> {
    let mut out_pos = 0usize;
    let mut updated = false;

    for line in shadow.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        if rewrite_shadow_line(line, username, hash_field, out, &mut out_pos)? {
            updated = true;
        } else {
            append_bytes(out, &mut out_pos, line)?;
            append_bytes(out, &mut out_pos, b"\n")?;
        }
    }

    if updated {
        Ok(out_pos)
    } else {
        Err(ShadowRewriteError::UserNotFound)
    }
}

fn rewrite_shadow_line(
    line: &[u8],
    username: &[u8],
    hash_field: &[u8],
    out: &mut [u8],
    out_pos: &mut usize,
) -> Result<bool, ShadowRewriteError> {
    let Some(name_end) = line.iter().position(|&b| b == b':') else {
        return Ok(false);
    };
    if &line[..name_end] != username {
        return Ok(false);
    }

    append_bytes(out, out_pos, username)?;
    append_bytes(out, out_pos, b":")?;
    append_bytes(out, out_pos, hash_field)?;

    let rest = &line[name_end + 1..];
    if let Some(hash_end) = rest.iter().position(|&b| b == b':') {
        append_bytes(out, out_pos, &rest[hash_end..])?;
    }
    append_bytes(out, out_pos, b"\n")?;
    Ok(true)
}

fn append_bytes(
    out: &mut [u8],
    out_pos: &mut usize,
    bytes: &[u8],
) -> Result<(), ShadowRewriteError> {
    let end = out_pos
        .checked_add(bytes.len())
        .ok_or(ShadowRewriteError::OutputTooLarge)?;
    if end > out.len() {
        return Err(ShadowRewriteError::OutputTooLarge);
    }
    out[*out_pos..end].copy_from_slice(bytes);
    *out_pos = end;
    Ok(())
}

pub fn find_username_by_uid(passwd: &[u8], target_uid: u32) -> Option<&[u8]> {
    for line in passwd.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let mut fields = [&[] as &[u8]; 7];
        let mut start = 0;
        let mut field = 0;
        for (i, &b) in line.iter().enumerate() {
            if b == b':' && field < 7 {
                fields[field] = &line[start..i];
                field += 1;
                start = i + 1;
            }
        }
        if field == 6 {
            fields[6] = &line[start..];
            let Some(uid) = parse_u32(fields[2]) else {
                continue;
            };
            if uid == target_uid {
                return Some(fields[0]);
            }
        }
    }
    None
}

fn parse_u32(s: &[u8]) -> Option<u32> {
    let mut n: u32 = 0;
    let mut saw_digit = false;
    for &b in s {
        if !b.is_ascii_digit() {
            return None;
        }
        saw_digit = true;
        n = n.checked_mul(10)?.checked_add((b - b'0') as u32)?;
    }
    if saw_digit { Some(n) } else { None }
}
