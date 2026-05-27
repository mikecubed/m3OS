//! ELF-related pure-logic helpers used by both the kernel ELF loader
//! (`kernel/src/mm/elf.rs`) and the userspace dynamic linker
//! (`userspace/ld-musl-x86_64.so.1/`).
//!
//! Phase 76: only the auxiliary-vector layout helpers live here. The
//! relocation engine + dependency-graph code that lands in Phase 76b
//! will sit alongside (`reloc.rs`, `dynamic.rs`).

pub mod auxv;
