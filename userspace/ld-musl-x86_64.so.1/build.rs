//! `build.rs` — Phase 76c link-time wiring.
//!
//! These flags graduate the linker binary from "PIE executable
//! mapped by `PT_INTERP`" (the Phase 76 / 76b shape) into "PIE
//! executable that ALSO works as a shared library":
//!
//! * `--hash-style=sysv` — emit `DT_HASH` (without `DT_GNU_HASH`)
//!   so the bring-up `lookup_symbol` walker (which only understands
//!   the SysV hash table; `DT_GNU_HASH` ships in Phase 76d) can
//!   resolve `dlopen` / `dlsym` / `dlclose` / `dlerror` against the
//!   linker's own dynamic symbol table.
//!
//! * `--export-dynamic` — promote every `#[unsafe(no_mangle)] pub
//!   extern "C" fn` defined in this binary into the *dynamic*
//!   symbol table (not just the static one). Without this, the four
//!   libdl entry points would be unreachable through SysV symbol
//!   search because they would not appear in the linker's
//!   `DT_HASH`/`DT_SYMTAB`.
//!
//! * `-soname=ld-musl-x86_64.so.1` — set `DT_SONAME` so GNU ld at
//!   link time can scan the linker as a shared library and emit
//!   `DT_NEEDED = ld-musl-x86_64.so.1` into the consumer (e.g.
//!   `dlopen_test`) when it links against the linker for the libdl
//!   entry-point symbols.

fn main() {
    // The host-target build (cargo test against
    // `x86_64-unknown-linux-gnu`) uses `cc` as the linker driver,
    // which would reject the bare lld flag form below. Only emit
    // them when building for the bare-metal `x86_64-unknown-none`
    // target that this binary actually ships into.
    let target = std::env::var("TARGET").unwrap_or_default();
    if target == "x86_64-unknown-none" {
        println!("cargo:rustc-link-arg=--hash-style=sysv");
        println!("cargo:rustc-link-arg=--export-dynamic");
        println!("cargo:rustc-link-arg=-soname=ld-musl-x86_64.so.1");
    }
}
