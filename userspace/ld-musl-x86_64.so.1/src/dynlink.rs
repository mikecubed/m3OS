//! `PT_DYNAMIC` parser, dependency-graph loader, hash-based symbol
//! lookup, and constructor invocation.
//!
//! Phase 76b shape (filled in track by track):
//!
//! * [`DynamicSection`] — typed view of a `PT_DYNAMIC` section.
//! * `LoadedDso` / dependency loader — to come (B2.2).
//! * `topo_sort` — to come (B2.3).
//! * `DynamicSection::lookup_symbol` — SysV `DT_HASH` (B3.4).
//! * `run_constructors` — deepest-first walker (B5.1).

use crate::elf64::{
    DT_FINI, DT_FINI_ARRAY, DT_FINI_ARRAYSZ, DT_GNU_HASH, DT_HASH, DT_INIT, DT_INIT_ARRAY,
    DT_INIT_ARRAYSZ, DT_JMPREL, DT_NEEDED, DT_NULL, DT_PLTGOT, DT_PLTREL, DT_PLTRELSZ, DT_RELA,
    DT_RELAENT, DT_RELASZ, DT_SONAME, DT_STRSZ, DT_STRTAB, DT_SYMENT, DT_SYMTAB, DT_VERDEF,
    DT_VERDEFNUM, DT_VERNEED, DT_VERNEEDNUM, DT_VERSYM, Dyn, Sym,
};
use core::ptr::NonNull;

/// Hard upper bound on `DT_NEEDED` entries `DynamicSection` will
/// index. Real binaries have ~5; reserving 16 is generous for Phase
/// 76b's bring-up linker.
pub const MAX_NEEDED: usize = 16;

/// Typed view of a `PT_DYNAMIC` section. Pointer slots are
/// `Option<NonNull<_>>` so the consumer cannot accidentally
/// dereference an absent tag.
#[derive(Debug, Clone, Copy)]
pub struct DynamicSection {
    /// `DT_STRTAB` — address of the string table inside the DSO.
    pub strtab: Option<NonNull<u8>>,
    /// `DT_STRSZ` — size in bytes of the string table.
    pub strsz: u64,
    /// `DT_SYMTAB` — address of the dynamic symbol table.
    pub symtab: Option<NonNull<Sym>>,
    /// `DT_SYMENT` — size of one symbol-table entry (must be 24 on x86_64).
    pub syment: u64,
    /// `DT_RELA` — address of the `Rela` table.
    pub rela: Option<NonNull<u8>>,
    /// `DT_RELASZ` — total size in bytes of the `Rela` table.
    pub relasz: u64,
    /// `DT_RELAENT` — size of one `Rela` entry (must be 24).
    pub relaent: u64,
    /// `DT_JMPREL` — address of the PLT relocation table.
    pub jmprel: Option<NonNull<u8>>,
    /// `DT_PLTRELSZ` — size in bytes of the PLT relocation table.
    pub pltrelsz: u64,
    /// `DT_PLTREL` — `DT_REL` (17) or `DT_RELA` (7); Phase 76b only
    /// supports `DT_RELA`.
    pub pltrel: i64,
    /// `DT_INIT` — address of the `_init` function (legacy single-init).
    pub init: Option<NonNull<u8>>,
    /// `DT_INIT_ARRAY` — address of the constructor pointer array.
    pub init_array: Option<NonNull<u8>>,
    /// `DT_INIT_ARRAYSZ` — size in bytes of the constructor array.
    pub init_arraysz: u64,
    /// `DT_FINI` — address of the `_fini` function (legacy single-fini).
    pub fini: Option<NonNull<u8>>,
    /// `DT_FINI_ARRAY` — address of the destructor pointer array.
    pub fini_array: Option<NonNull<u8>>,
    /// `DT_FINI_ARRAYSZ` — size in bytes of the destructor array.
    pub fini_arraysz: u64,
    /// `DT_PLTGOT` (Phase 76d.B4.3) — address of the DSO's GOT,
    /// indexed so `GOT[0]` is the `DT_DYNAMIC` back-pointer, `GOT[1]`
    /// holds the link-map (we use `*const LoadedDso`), and `GOT[2]`
    /// holds `&_dl_runtime_resolve`. Absent when the DSO has no PLT
    /// (e.g. the bring-up linker itself).
    pub pltgot: Option<NonNull<u64>>,
    /// `DT_HASH` — address of the SysV hash table.
    pub hash: Option<NonNull<u32>>,
    /// `DT_GNU_HASH` (Phase 76d.D1) — address of the GNU hash table
    /// header. The layout is `[nbuckets, symoffset, bloom_size,
    /// bloom_shift] u32` followed by `[u64; bloom_size]` followed by
    /// `[u32; nbuckets]` (buckets) followed by `[u32; …]` (hashes).
    /// Absent when the DSO was built with `--hash-style=sysv` (the
    /// Phase 76b default).
    pub gnu_hash: Option<NonNull<u32>>,
    /// `DT_VERSYM` (Phase 76d.D2) — `Elf64_Half` (u16) per
    /// dynsym entry indexing into `DT_VERDEF` / `DT_VERNEED`. Absent
    /// when the DSO is unversioned (the Phase 76b/c default).
    pub versym: Option<NonNull<u16>>,
    /// `DT_VERDEF` (Phase 76d.D2) — first `Verdef` record. Absent when
    /// the DSO defines no versioned symbols.
    pub verdef: Option<NonNull<u8>>,
    /// `DT_VERDEFNUM` — number of `Verdef` records.
    pub verdefnum: u64,
    /// `DT_VERNEED` (Phase 76d.D2) — first `Verneed` record. Absent
    /// when the DSO requires no versioned symbols from its
    /// dependencies.
    pub verneed: Option<NonNull<u8>>,
    /// `DT_VERNEEDNUM` — number of `Verneed` records.
    pub verneednum: u64,
    /// `DT_SONAME` — offset into `strtab` of the library's `SONAME`,
    /// or `u64::MAX` if absent (no `NonNull` here because zero is a
    /// legal offset).
    pub soname: u64,
    /// `DT_NEEDED` entries — each value is the offset into `strtab`
    /// of one needed library name. Up to `MAX_NEEDED` entries.
    pub needed: [u64; MAX_NEEDED],
    /// Number of valid entries in `needed`.
    pub n_needed: u8,
}

impl DynamicSection {
    /// An empty `DynamicSection` (all tags absent). Useful as a
    /// starting point for `parse` and for unit-test scaffolding.
    pub const fn empty() -> Self {
        Self {
            strtab: None,
            strsz: 0,
            symtab: None,
            syment: 0,
            rela: None,
            relasz: 0,
            relaent: 0,
            jmprel: None,
            pltrelsz: 0,
            pltrel: 0,
            init: None,
            init_array: None,
            init_arraysz: 0,
            fini: None,
            fini_array: None,
            fini_arraysz: 0,
            pltgot: None,
            hash: None,
            gnu_hash: None,
            versym: None,
            verdef: None,
            verdefnum: 0,
            verneed: None,
            verneednum: 0,
            soname: u64::MAX,
            needed: [0; MAX_NEEDED],
            n_needed: 0,
        }
    }

    /// Walk a `PT_DYNAMIC` section, indexing every tag Phase 76b
    /// understands. `dyn_entries` is the in-memory slice of `Dyn`
    /// records terminated by `DT_NULL`; `load_bias` is the DSO's load
    /// bias so any tag carrying an address can be relocated against
    /// the running image.
    ///
    /// Tags Phase 76b does not understand are ignored — this matches
    /// the SysV ELF spec, which allows unknown tags in the
    /// implementation-defined range. The caller is expected to
    /// validate `relaent`/`syment`/`pltrel` after parsing.
    ///
    /// Returns the parsed section. Excess `DT_NEEDED` entries beyond
    /// `MAX_NEEDED` are truncated and the caller can detect the
    /// truncation by comparing `n_needed` to the count it expected.
    pub fn parse(dyn_entries: &[Dyn], load_bias: u64) -> Self {
        let mut out = Self::empty();
        for entry in dyn_entries {
            match entry.d_tag {
                DT_NULL => break,
                DT_NEEDED if (out.n_needed as usize) < MAX_NEEDED => {
                    out.needed[out.n_needed as usize] = entry.d_val;
                    out.n_needed += 1;
                }
                DT_PLTRELSZ => out.pltrelsz = entry.d_val,
                DT_HASH => {
                    out.hash = NonNull::new((entry.d_val.wrapping_add(load_bias)) as *mut u32);
                }
                DT_STRTAB => {
                    out.strtab = NonNull::new((entry.d_val.wrapping_add(load_bias)) as *mut u8);
                }
                DT_SYMTAB => {
                    out.symtab = NonNull::new((entry.d_val.wrapping_add(load_bias)) as *mut Sym);
                }
                DT_RELA => {
                    out.rela = NonNull::new((entry.d_val.wrapping_add(load_bias)) as *mut u8);
                }
                DT_RELASZ => out.relasz = entry.d_val,
                DT_RELAENT => out.relaent = entry.d_val,
                DT_STRSZ => out.strsz = entry.d_val,
                DT_SYMENT => out.syment = entry.d_val,
                DT_INIT => {
                    out.init = NonNull::new((entry.d_val.wrapping_add(load_bias)) as *mut u8);
                }
                DT_SONAME => out.soname = entry.d_val,
                DT_PLTREL => out.pltrel = entry.d_val as i64,
                DT_JMPREL => {
                    out.jmprel = NonNull::new((entry.d_val.wrapping_add(load_bias)) as *mut u8);
                }
                DT_INIT_ARRAY => {
                    out.init_array = NonNull::new((entry.d_val.wrapping_add(load_bias)) as *mut u8);
                }
                DT_INIT_ARRAYSZ => out.init_arraysz = entry.d_val,
                DT_FINI => {
                    out.fini = NonNull::new((entry.d_val.wrapping_add(load_bias)) as *mut u8);
                }
                DT_FINI_ARRAY => {
                    out.fini_array = NonNull::new((entry.d_val.wrapping_add(load_bias)) as *mut u8);
                }
                DT_FINI_ARRAYSZ => out.fini_arraysz = entry.d_val,
                // Phase 76d.B4 — PLT GOT for lazy-resolve trampoline.
                DT_PLTGOT => {
                    out.pltgot = NonNull::new((entry.d_val.wrapping_add(load_bias)) as *mut u64);
                }
                // Phase 76d.D1 — GNU hash table.
                DT_GNU_HASH => {
                    out.gnu_hash = NonNull::new((entry.d_val.wrapping_add(load_bias)) as *mut u32);
                }
                // Phase 76d.D2 — symbol versioning.
                DT_VERSYM => {
                    out.versym = NonNull::new((entry.d_val.wrapping_add(load_bias)) as *mut u16);
                }
                DT_VERDEF => {
                    out.verdef = NonNull::new((entry.d_val.wrapping_add(load_bias)) as *mut u8);
                }
                DT_VERDEFNUM => out.verdefnum = entry.d_val,
                DT_VERNEED => {
                    out.verneed = NonNull::new((entry.d_val.wrapping_add(load_bias)) as *mut u8);
                }
                DT_VERNEEDNUM => out.verneednum = entry.d_val,
                _ => {} // unknown tag — ignore per SysV ELF spec
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// SysV ELF symbol hash (`elf_hash` from the System V ABI). Used by
// `DT_HASH` lookups. Pure-logic so it is host-testable.
// ---------------------------------------------------------------------------

/// Reference implementation of the SysV ELF hash function. Takes a
/// byte slice (NOT a NUL-terminated C string) so it is callable from
/// host tests without `unsafe`.
pub fn elf_hash(name: &[u8]) -> u32 {
    let mut h: u32 = 0;
    for &b in name {
        h = h.wrapping_mul(16).wrapping_add(b as u32);
        let g = h & 0xF000_0000;
        if g != 0 {
            h ^= g >> 24;
        }
        h &= !g;
    }
    h
}

/// SysV `DT_HASH` table layout:
///
/// ```text
/// nbuckets : u32
/// nchain   : u32
/// buckets  : [u32; nbuckets]
/// chain    : [u32; nchain]
/// ```
///
/// To resolve a name `n`:
///   1. compute `h = elf_hash(n)`;
///   2. read `idx = buckets[h % nbuckets]`;
///   3. while `idx != 0`: if `symtab[idx].name == n` → hit; else
///      `idx = chain[idx]`.
///
/// `STN_UNDEF == 0` is the chain terminator and also the index of the
/// always-undefined first symbol-table entry, so a `0` from `buckets`
/// or `chain` means "not present".
///
/// `lookup_in_hash_table` is a pure-logic implementation that takes
/// the raw `DT_HASH` payload as a `&[u32]`, a callback that resolves
/// `(symbol_index → symbol-name bytes)`, and the name being searched
/// for. The runtime walker wraps this with raw-pointer reads of the
/// real `DT_HASH` table.
pub fn lookup_in_hash_table(
    hash_table: &[u32],
    name: &[u8],
    mut name_of_symbol: impl FnMut(u32) -> Option<&'static [u8]>,
) -> Option<u32> {
    if hash_table.len() < 2 {
        return None;
    }
    let nbuckets = hash_table[0] as usize;
    let nchain = hash_table[1] as usize;
    if hash_table.len() < 2 + nbuckets + nchain || nbuckets == 0 {
        return None;
    }
    let buckets = &hash_table[2..2 + nbuckets];
    let chain = &hash_table[2 + nbuckets..2 + nbuckets + nchain];

    let h = elf_hash(name);
    let mut idx = buckets[(h as usize) % nbuckets];
    // STN_UNDEF (0) is both the always-undefined first slot AND the
    // chain terminator — walking it would always miss; bail out fast.
    let mut hops = 0usize;
    while idx != 0 {
        // Defensive: a corrupt chain could otherwise loop forever.
        // `nchain` is an upper bound on legitimate hops because each
        // symbol appears at most once in the chain.
        if hops > nchain {
            return None;
        }
        if (idx as usize) >= nchain {
            return None;
        }
        if let Some(symbol_name) = name_of_symbol(idx)
            && symbol_name == name
        {
            return Some(idx);
        }
        idx = chain[idx as usize];
        hops += 1;
    }
    None
}

// ---------------------------------------------------------------------------
// LoadedDso — one mapped shared library in the linker's address space.
//
// Phase 76c moves this struct out of the binary so the library can
// expose `unmap_dso` as a pure-logic helper that downstream tests can
// drive without invoking real syscalls.
// ---------------------------------------------------------------------------

/// One mapped DSO. `load_bias` + `image_len` are the byte range the
/// runtime `mmap`ed for the whole image; `dyn_` is the parsed
/// `PT_DYNAMIC` view rebased against `load_bias`.
#[derive(Debug, Clone, Copy)]
pub struct LoadedDso {
    /// Address the kernel mapped the DSO at. Equal to the value
    /// returned by `mmap(addr=0, …)` for the DSO's whole-image
    /// anonymous mapping.
    pub load_bias: u64,
    /// Page-rounded in-memory span of the DSO's image — the full
    /// `mmap` length so a single `munmap(load_bias, image_len)`
    /// matches the load shape. `0` means "trust the caller" (only
    /// the [`LoadedDso::empty`] placeholder uses this).
    pub image_len: u64,
    /// Parsed `PT_DYNAMIC` view, pointers rebased against
    /// `load_bias`.
    pub dyn_: DynamicSection,
}

impl LoadedDso {
    /// Placeholder `LoadedDso` for slot prefill. All pointer fields
    /// of `dyn_` are `None` so any attempt to read symbols / strings
    /// off the placeholder bails out safely.
    pub const fn empty() -> Self {
        Self {
            load_bias: 0,
            image_len: 0,
            dyn_: DynamicSection::empty(),
        }
    }
}

/// `unmap_dso` errors. `EmptyImage` is the placeholder-`LoadedDso`
/// shape — unmap is meaningless there. `MunmapFailed` carries the
/// negative `errno` returned by the host `munmap` callback so the
/// runtime caller can log the exact failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnmapError {
    /// `image_len == 0` — no mapping to release.
    EmptyImage,
    /// The munmap callback returned `< 0`.
    MunmapFailed(i64),
}

/// Issue a single `munmap(load_bias, image_len)` matching the 76b
/// whole-image mmap shape (one anonymous mmap covering the highest
/// `PT_LOAD`-aligned-up range). The caller provides the `munmap`
/// closure so this function is pure-logic and host-testable; the
/// runtime caller passes a wrapper around `sys_munmap`.
///
/// Returns `Ok(())` when the callback returns `>= 0`. The DSO record
/// itself is not mutated — the caller is responsible for evicting
/// the entry from its load list, since this function does not know
/// how the runtime stores its DSOs.
pub fn unmap_dso<F>(dso: &LoadedDso, mut munmap: F) -> Result<(), UnmapError>
where
    F: FnMut(u64, u64) -> i64,
{
    if dso.image_len == 0 {
        return Err(UnmapError::EmptyImage);
    }
    let r = munmap(dso.load_bias, dso.image_len);
    if r < 0 {
        return Err(UnmapError::MunmapFailed(r));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Topological sort of the loaded-DSO dependency graph.
// ---------------------------------------------------------------------------

/// Lightweight identifier for one loaded DSO. The runtime allocates
/// `DsoId`s contiguously starting at 0 (main binary) so the topo-sort
/// can use them as array indices without an extra hash map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DsoId(pub u32);

/// Errors `topo_sort` can return.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TopoError {
    /// A cycle was detected. The payload is the two DSO IDs forming
    /// the back-edge; the runtime caller maps them back to SONAMEs
    /// for the log message.
    CircularDependency(DsoId, DsoId),
    /// Graph exceeds [`MAX_DSOS`].
    Overflow,
}

/// Maximum nodes the topo-sort accepts in one pass. Real binaries
/// have at most a handful of DSOs; the cap is a safety belt against
/// runaway recursion in pathological inputs.
pub const MAX_DSOS: usize = 32;

/// Topologically sort a dependency graph deepest-first (post-order
/// DFS). Returns the sort order whose first element is the deepest
/// dependency and whose last element is the root.
///
/// `deps[i]` is the slice of nodes `DsoId(i as u32)` depends on
/// directly. The function never mutates external state — the
/// runtime caller builds `deps` once after parsing every loaded
/// DSO's `DT_NEEDED` list.
///
/// Cycle handling: a cycle is detected when a node is revisited
/// while still on the DFS stack. The two IDs forming the back-edge
/// are returned in `TopoError::CircularDependency`.
pub fn topo_sort(deps: &[&[DsoId]]) -> Result<heapless::Vec<DsoId, MAX_DSOS>, TopoError> {
    let n = deps.len();
    if n > MAX_DSOS {
        return Err(TopoError::Overflow);
    }
    // 0 = white (unvisited), 1 = gray (on stack), 2 = black (done).
    let mut color = [0u8; MAX_DSOS];
    let mut order: heapless::Vec<DsoId, MAX_DSOS> = heapless::Vec::new();

    for i in 0..n {
        if color[i] != 0 {
            continue;
        }
        // Iterative DFS — each stack frame is `(node, next_child_idx)`.
        let mut stack: heapless::Vec<(DsoId, usize), MAX_DSOS> = heapless::Vec::new();
        let start = DsoId(i as u32);
        stack.push((start, 0)).map_err(|_| TopoError::Overflow)?;
        color[i] = 1;
        while let Some(&(node, next_child)) = stack.last() {
            let nidx = node.0 as usize;
            let children = if nidx < deps.len() { deps[nidx] } else { &[] };
            if next_child < children.len() {
                let child = children[next_child];
                // Advance the iterator for `node` before recursing.
                if let Some(last) = stack.last_mut() {
                    last.1 = next_child + 1;
                }
                let cidx = child.0 as usize;
                if cidx >= n {
                    return Err(TopoError::CircularDependency(node, child));
                }
                match color[cidx] {
                    0 => {
                        color[cidx] = 1;
                        stack.push((child, 0)).map_err(|_| TopoError::Overflow)?;
                    }
                    1 => {
                        return Err(TopoError::CircularDependency(node, child));
                    }
                    _ => {}
                }
            } else {
                color[nidx] = 2;
                order.push(node).map_err(|_| TopoError::Overflow)?;
                stack.pop();
            }
        }
    }
    Ok(order)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(tag: i64, val: u64) -> Dyn {
        Dyn {
            d_tag: tag,
            d_val: val,
        }
    }

    #[test]
    fn empty_dynamic_section_terminates_immediately() {
        let entries = [entry(DT_NULL, 0)];
        let d = DynamicSection::parse(&entries, 0);
        assert!(d.strtab.is_none());
        assert_eq!(d.n_needed, 0);
        assert_eq!(d.soname, u64::MAX);
    }

    #[test]
    fn dynamic_section_indexes_canonical_tags() {
        let entries = [
            entry(DT_NEEDED, 1),
            entry(DT_NEEDED, 2),
            entry(DT_STRTAB, 0x1000),
            entry(DT_SYMTAB, 0x2000),
            entry(DT_RELA, 0x3000),
            entry(DT_RELASZ, 48),
            entry(DT_RELAENT, 24),
            entry(DT_JMPREL, 0x4000),
            entry(DT_PLTRELSZ, 24),
            entry(DT_PLTREL, 7),
            entry(DT_INIT, 0x5000),
            entry(DT_INIT_ARRAY, 0x6000),
            entry(DT_INIT_ARRAYSZ, 16),
            entry(DT_HASH, 0x7000),
            entry(DT_SONAME, 42),
            entry(DT_STRSZ, 128),
            entry(DT_SYMENT, 24),
            entry(DT_NULL, 0),
        ];
        let d = DynamicSection::parse(&entries, 0x1_0000_0000);
        assert_eq!(d.n_needed, 2);
        assert_eq!(d.needed[0], 1);
        assert_eq!(d.needed[1], 2);
        assert_eq!(d.strtab.unwrap().as_ptr() as u64, 0x1_0000_1000);
        assert_eq!(d.symtab.unwrap().as_ptr() as u64, 0x1_0000_2000);
        assert_eq!(d.rela.unwrap().as_ptr() as u64, 0x1_0000_3000);
        assert_eq!(d.relasz, 48);
        assert_eq!(d.relaent, 24);
        assert_eq!(d.jmprel.unwrap().as_ptr() as u64, 0x1_0000_4000);
        assert_eq!(d.pltrelsz, 24);
        assert_eq!(d.pltrel, 7);
        assert_eq!(d.init.unwrap().as_ptr() as u64, 0x1_0000_5000);
        assert_eq!(d.init_array.unwrap().as_ptr() as u64, 0x1_0000_6000);
        assert_eq!(d.init_arraysz, 16);
        assert_eq!(d.hash.unwrap().as_ptr() as u64, 0x1_0000_7000);
        assert_eq!(d.soname, 42);
        assert_eq!(d.strsz, 128);
        assert_eq!(d.syment, 24);
    }

    #[test]
    fn dt_null_terminates_iteration() {
        let entries = [
            entry(DT_STRTAB, 0x1000),
            entry(DT_NULL, 0),
            // The parser must stop here; the entries below should NOT
            // be indexed.
            entry(DT_SYMTAB, 0x9999),
        ];
        let d = DynamicSection::parse(&entries, 0);
        assert!(d.strtab.is_some());
        assert!(d.symtab.is_none(), "parser walked past DT_NULL");
    }

    #[test]
    fn dynamic_section_indexes_fini_tags() {
        let entries = [
            entry(DT_FINI, 0x8000),
            entry(DT_FINI_ARRAY, 0x9000),
            entry(DT_FINI_ARRAYSZ, 24),
            entry(DT_NULL, 0),
        ];
        let d = DynamicSection::parse(&entries, 0x1_0000_0000);
        assert_eq!(d.fini.unwrap().as_ptr() as u64, 0x1_0000_8000);
        assert_eq!(d.fini_array.unwrap().as_ptr() as u64, 0x1_0000_9000);
        assert_eq!(d.fini_arraysz, 24);
    }

    #[test]
    fn unknown_tags_are_ignored() {
        let entries = [
            entry(0xCAFE_BABE, 0xDEAD),
            entry(DT_STRTAB, 0x1000),
            entry(DT_NULL, 0),
        ];
        let d = DynamicSection::parse(&entries, 0);
        assert_eq!(d.strtab.unwrap().as_ptr() as u64, 0x1000);
    }

    #[test]
    fn needed_truncates_at_max() {
        let mut entries: Vec<Dyn> = (1..=MAX_NEEDED as u64 + 2)
            .map(|i| Dyn {
                d_tag: DT_NEEDED,
                d_val: i,
            })
            .collect();
        entries.push(Dyn {
            d_tag: DT_NULL,
            d_val: 0,
        });
        let d = DynamicSection::parse(&entries, 0);
        assert_eq!(d.n_needed as usize, MAX_NEEDED);
        assert_eq!(d.needed[0], 1);
        assert_eq!(d.needed[MAX_NEEDED - 1], MAX_NEEDED as u64);
    }

    // SysV ELF hash known-answer vectors, taken from the System V ABI
    // (Section 5: Hash Table) reference implementation comments.
    #[test]
    fn elf_hash_known_answers() {
        // Empty string hashes to 0.
        assert_eq!(elf_hash(b""), 0);
        // Single-byte hash matches the spec: h*16 + b.
        assert_eq!(elf_hash(b"a"), 97);
        // Two-byte: first round h=97, second round h = 97*16 + 98 = 1650.
        // No upper nibble set, so no XOR/mask.
        assert_eq!(elf_hash(b"ab"), 1650);
        // Spot-check a real symbol name often present in libc.so:
        // `printf` should produce the canonical value the SysV spec
        // documents.
        // Computed: ((((((0*16+0x70)*16+0x72)*16+0x69)*16+0x6E)*16+0x74)*16+0x66)
        // = 0x07790_5A6. No iteration sets the top nibble, so no
        // XOR/mask folding occurs along the way — but the final
        // value is the byte-exact known answer.
        assert_eq!(elf_hash(b"printf"), 0x0779_05A6);
    }

    // -----------------------------------------------------------------
    // lookup_in_hash_table tests.
    // -----------------------------------------------------------------

    /// Build a minimal SysV DT_HASH table for a fixed set of names.
    /// Layout: `[nbuckets, nchain, buckets..., chain...]`. The dummy
    /// `STN_UNDEF` symbol occupies index 0 in the chain (value 0 == end).
    fn build_hash_table(names: &[&'static [u8]], nbuckets: usize) -> Vec<u32> {
        let nchain = names.len() + 1; // +1 for STN_UNDEF
        let mut buckets = vec![0u32; nbuckets];
        let mut chain = vec![0u32; nchain];
        for (i, name) in names.iter().enumerate() {
            let sym_idx = (i + 1) as u32; // +1 to skip STN_UNDEF
            let bucket = (elf_hash(name) as usize) % nbuckets;
            // Push onto the chain head for this bucket.
            chain[sym_idx as usize] = buckets[bucket];
            buckets[bucket] = sym_idx;
        }
        let mut out = vec![nbuckets as u32, nchain as u32];
        out.extend_from_slice(&buckets);
        out.extend_from_slice(&chain);
        out
    }

    #[test]
    fn lookup_resolves_a_symbol_with_one_bucket() {
        let names: &[&[u8]] = &[b"foo", b"bar", b"baz"];
        let ht = build_hash_table(names, 1);
        let idx = lookup_in_hash_table(&ht, b"bar", |i| names.get((i - 1) as usize).copied())
            .expect("bar should be found");
        assert_eq!(idx, 2); // names[1] -> sym idx 2
    }

    #[test]
    fn lookup_resolves_a_symbol_with_multiple_buckets() {
        let names: &[&[u8]] = &[b"foo", b"bar", b"baz", b"qux", b"hello"];
        let ht = build_hash_table(names, 4);
        for (i, n) in names.iter().enumerate() {
            let idx = lookup_in_hash_table(&ht, n, |j| names.get((j - 1) as usize).copied())
                .unwrap_or_else(|| panic!("{:?} should be found", core::str::from_utf8(n)));
            assert_eq!(idx as usize, i + 1);
        }
    }

    #[test]
    fn lookup_returns_none_for_absent_name() {
        let names: &[&[u8]] = &[b"foo", b"bar"];
        let ht = build_hash_table(names, 1);
        let r = lookup_in_hash_table(&ht, b"baz", |i| names.get((i - 1) as usize).copied());
        assert!(r.is_none());
    }

    #[test]
    fn lookup_rejects_malformed_table() {
        // 1-element table (only nbuckets, no nchain) is malformed.
        let ht = vec![1u32];
        let r = lookup_in_hash_table(&ht, b"foo", |_| Some(&b"foo"[..]));
        assert!(r.is_none());
        // zero buckets is malformed (would divide by zero).
        let ht2 = vec![0u32, 1, 0];
        let r2 = lookup_in_hash_table(&ht2, b"foo", |_| Some(&b"foo"[..]));
        assert!(r2.is_none());
    }

    // -----------------------------------------------------------------
    // topo_sort tests.
    // -----------------------------------------------------------------

    #[test]
    fn topo_sort_linear_chain_deepest_first() {
        // 0 -> 1 -> 2  (main -> libA -> libB)
        let d1: &[DsoId] = &[DsoId(1)];
        let d2: &[DsoId] = &[DsoId(2)];
        let d3: &[DsoId] = &[];
        let deps: &[&[DsoId]] = &[d1, d2, d3];
        let order = topo_sort(deps).unwrap();
        // Deepest first → leaf 2, then 1, then root 0.
        assert_eq!(&order[..], &[DsoId(2), DsoId(1), DsoId(0)]);
    }

    #[test]
    fn topo_sort_diamond_visits_shared_dep_once() {
        // 0 -> {1, 2}; 1 -> 3; 2 -> 3
        let d0: &[DsoId] = &[DsoId(1), DsoId(2)];
        let d1: &[DsoId] = &[DsoId(3)];
        let d2: &[DsoId] = &[DsoId(3)];
        let d3: &[DsoId] = &[];
        let deps: &[&[DsoId]] = &[d0, d1, d2, d3];
        let order = topo_sort(deps).unwrap();
        assert_eq!(order.len(), 4);
        // 3 must come before both 1 and 2; 1 and 2 must come before 0.
        let pos: heapless::Vec<usize, MAX_DSOS> = (0..4)
            .map(|i| order.iter().position(|d| d.0 == i as u32).unwrap())
            .collect();
        assert!(pos[3] < pos[1]);
        assert!(pos[3] < pos[2]);
        assert!(pos[1] < pos[0]);
        assert!(pos[2] < pos[0]);
    }

    #[test]
    fn topo_sort_two_node_cycle_errors() {
        // 0 -> 1 -> 0
        let d0: &[DsoId] = &[DsoId(1)];
        let d1: &[DsoId] = &[DsoId(0)];
        let deps: &[&[DsoId]] = &[d0, d1];
        match topo_sort(deps) {
            Err(TopoError::CircularDependency(a, b)) => {
                // Either back-edge orientation is acceptable.
                assert!(
                    (a == DsoId(1) && b == DsoId(0)) || (a == DsoId(0) && b == DsoId(1)),
                    "expected back-edge between 0 and 1, got ({a:?}, {b:?})"
                );
            }
            other => panic!("expected CircularDependency, got {other:?}"),
        }
    }

    #[test]
    fn topo_sort_self_loop_is_a_cycle() {
        // 0 -> 0
        let d0: &[DsoId] = &[DsoId(0)];
        let deps: &[&[DsoId]] = &[d0];
        assert!(matches!(
            topo_sort(deps),
            Err(TopoError::CircularDependency(DsoId(0), DsoId(0)))
        ));
    }

    #[test]
    fn topo_sort_empty_graph_returns_empty_order() {
        let deps: &[&[DsoId]] = &[];
        let order = topo_sort(deps).unwrap();
        assert_eq!(order.len(), 0);
    }

    // -----------------------------------------------------------------
    // LoadedDso + unmap_dso tests (Phase 76c C2.3).
    // -----------------------------------------------------------------

    #[test]
    fn loaded_dso_empty_has_zero_image_len() {
        let d = LoadedDso::empty();
        assert_eq!(d.load_bias, 0);
        assert_eq!(d.image_len, 0);
        assert!(d.dyn_.strtab.is_none());
    }

    #[test]
    fn unmap_dso_empty_image_errors() {
        let d = LoadedDso::empty();
        let r = unmap_dso(&d, |_addr, _len| 0);
        assert_eq!(r, Err(UnmapError::EmptyImage));
    }

    #[test]
    fn unmap_dso_calls_munmap_with_whole_image() {
        let d = LoadedDso {
            load_bias: 0x4000_0000,
            image_len: 0x3000, // page-aligned, 12 KiB
            dyn_: DynamicSection::empty(),
        };
        let mut observed: Option<(u64, u64)> = None;
        let r = unmap_dso(&d, |addr, len| {
            observed = Some((addr, len));
            0
        });
        assert_eq!(r, Ok(()));
        assert_eq!(observed, Some((0x4000_0000, 0x3000)));
    }

    #[test]
    fn unmap_dso_propagates_munmap_failure() {
        let d = LoadedDso {
            load_bias: 0x4000_0000,
            image_len: 0x3000,
            dyn_: DynamicSection::empty(),
        };
        let r = unmap_dso(&d, |_addr, _len| -22); // -EINVAL
        assert_eq!(r, Err(UnmapError::MunmapFailed(-22)));
    }
}
