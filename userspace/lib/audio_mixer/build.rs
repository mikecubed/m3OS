//! Build-script drift check: every `#define AUDIO_MIXER_* <int>` line
//! in `include/audio_mixer.h` must match the matching `pub const` in
//! `src/ffi.rs`. A mismatch fails the build with a `panic!()` tagged
//! `audio_mixer.h drift: <NAME> header={h} rust={r}` so the regression
//! is loud and grep-friendly.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env_var("CARGO_MANIFEST_DIR"));
    let header_path = manifest_dir.join("include/audio_mixer.h");
    let ffi_path = manifest_dir.join("src/ffi.rs");

    println!("cargo:rerun-if-changed={}", header_path.display());
    println!("cargo:rerun-if-changed={}", ffi_path.display());

    let header = fs::read_to_string(&header_path)
        .unwrap_or_else(|e| panic!("audio_mixer build.rs: read {:?}: {e}", header_path));
    let ffi = fs::read_to_string(&ffi_path)
        .unwrap_or_else(|e| panic!("audio_mixer build.rs: read {:?}: {e}", ffi_path));

    let header_defines = parse_defines(&header);
    let ffi_consts = parse_consts(&ffi);

    for (name, header_value) in &header_defines {
        let rust_value = ffi_consts.get(name).copied().unwrap_or_else(|| {
            panic!(
                "audio_mixer.h drift: {} present in header but missing in src/ffi.rs",
                name
            )
        });
        if rust_value != *header_value {
            panic!(
                "audio_mixer.h drift: {} header={} rust={}",
                name, header_value, rust_value
            );
        }
    }

    for name in ffi_consts.keys() {
        if !header_defines.contains_key(name) {
            panic!(
                "audio_mixer.h drift: {} present in src/ffi.rs but missing in header",
                name
            );
        }
    }
}

fn env_var(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| panic!("audio_mixer build.rs: ${} unset", key))
}

/// Parse `#define AUDIO_MIXER_<NAME> <int>` lines from a C header.
fn parse_defines(src: &str) -> HashMap<String, i64> {
    let mut out = HashMap::new();
    for line in src.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("#define") {
            continue;
        }
        let rest = trimmed.trim_start_matches("#define").trim();
        let mut parts = rest.split_whitespace();
        let name = match parts.next() {
            Some(n) => n,
            None => continue,
        };
        if !name.starts_with("AUDIO_MIXER_") {
            continue;
        }
        let value = match parts.next() {
            Some(v) => v,
            None => continue,
        };
        // Reject anything past the value to keep the parser strict.
        if parts.next().is_some() {
            continue;
        }
        let parsed: i64 = match value.parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        out.insert(name.to_string(), parsed);
    }
    out
}

/// Parse `pub const AUDIO_MIXER_<NAME>: c_int = <int>;` lines from
/// `src/ffi.rs`.
fn parse_consts(src: &str) -> HashMap<String, i64> {
    let mut out = HashMap::new();
    for line in src.lines() {
        let trimmed = line.trim();
        let body = match trimmed.strip_prefix("pub const ") {
            Some(b) => b,
            None => continue,
        };
        // Expect `AUDIO_MIXER_<NAME>: c_int = <value>;`
        let (name, rest) = match body.split_once(':') {
            Some(p) => p,
            None => continue,
        };
        let name = name.trim();
        if !name.starts_with("AUDIO_MIXER_") {
            continue;
        }
        let rest = rest.trim();
        let (_ty, value) = match rest.split_once('=') {
            Some(p) => p,
            None => continue,
        };
        let value = value.trim().trim_end_matches(';').trim();
        let parsed: i64 = match value.parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        out.insert(name.to_string(), parsed);
    }
    out
}
