//! Build-script drift check for `include/display_client.h`.
//!
//! Every `#define DC_* <int>` line in the header must match the
//! corresponding `pub const` in `src/lib.rs`. Mismatch fails the build
//! with `display_client.h drift: <NAME> header={h} rust={r}`.
//!
//! Phase 70 mirror of the `audio_client_ffi/build.rs` pattern.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env_var("CARGO_MANIFEST_DIR"));
    let header_path = manifest_dir.join("include/display_client.h");
    let lib_path = manifest_dir.join("src/lib.rs");

    println!("cargo:rerun-if-changed={}", header_path.display());
    println!("cargo:rerun-if-changed={}", lib_path.display());

    let header = fs::read_to_string(&header_path)
        .unwrap_or_else(|e| panic!("display_client_ffi build.rs: read {:?}: {e}", header_path));
    let rust = fs::read_to_string(&lib_path)
        .unwrap_or_else(|e| panic!("display_client_ffi build.rs: read {:?}: {e}", lib_path));

    let header_defines = parse_defines(&header);
    let rust_consts = parse_consts(&rust);

    for (name, header_value) in &header_defines {
        let rust_value = rust_consts.get(name).copied().unwrap_or_else(|| {
            panic!(
                "display_client.h drift: {} present in header but missing in src/lib.rs",
                name
            )
        });
        if rust_value != *header_value {
            panic!(
                "display_client.h drift: {} header={} rust={}",
                name, header_value, rust_value
            );
        }
    }

    for name in rust_consts.keys() {
        if !header_defines.contains_key(name) {
            panic!(
                "display_client.h drift: {} present in src/lib.rs but missing in header",
                name
            );
        }
    }
}

fn env_var(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| panic!("display_client_ffi build.rs: ${} unset", key))
}

/// Parse `#define DC_<NAME> <int>` lines from a C header.
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
        if !name.starts_with("DC_") {
            continue;
        }
        let value = match parts.next() {
            Some(v) => v,
            None => continue,
        };
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

/// Parse `pub const DC_<NAME>: c_int = <int>;` or `pub const DC_<NAME>: u32 = <int>;` lines.
fn parse_consts(src: &str) -> HashMap<String, i64> {
    let mut out = HashMap::new();
    for line in src.lines() {
        let trimmed = line.trim();
        let body = match trimmed.strip_prefix("pub const ") {
            Some(b) => b,
            None => continue,
        };
        let (name, rest) = match body.split_once(':') {
            Some(p) => p,
            None => continue,
        };
        let name = name.trim();
        if !name.starts_with("DC_") {
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
