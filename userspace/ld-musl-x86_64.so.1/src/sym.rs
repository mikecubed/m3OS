//! Unified symbol-lookup surface for the dynamic linker.
//!
//! Phase 76d.S1.1 collapses every per-call-site SysV `DT_HASH` walk
//! into a single entry point so the GNU-hash backend (D1) and the
//! version-aware path (D2) only have to extend one function instead
//! of every consumer.
//!
//! ## Lookup contract
//!
//! [`lookup`] walks `scope` in order and returns the address of the
//! first DSO that defines `name`. Behavior matches the Phase 76b
//! free-function `lookup_symbol` byte-for-byte: SysV global scope,
//! `STN_UNDEF == 0` terminator, `st_value != 0` filter, `nchain`
//! hops bound.
//!
//! ## Backend dispatch
//!
//! Each DSO is dispatched to a [`Backend`] based on which hash tags
//! its `PT_DYNAMIC` carries. Phase 76d.S1 ships only [`Backend::SysV`];
//! D1 inserts the GNU arm in front of it (so libraries with
//! `--hash-style=both` benefit from the Bloom-filter short-circuit).
//!
//! ## Version-awareness
//!
//! The `version` parameter is accepted today but threaded as `None`
//! by every Phase 76d.S1 caller — D2 wires real version constraints
//! into the path. Carrying the parameter from S1 means D2 does not
//! re-touch every call site.

use ldso_core::dynlink::{LoadedDso, elf_hash};

/// Backend chosen for one DSO's symbol-table walk. Phase 76d.S1 only
/// emits [`Backend::SysV`]; D1 adds `Gnu` in front of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Backend {
    SysV,
}

/// Look up `name` across `scope` and return the first DSO's
/// definition address, or `None` if no DSO defines it.
///
/// `version` is reserved for D2 and ignored by Phase 76d.S1 callers
/// (they pass `None`). Carrying the parameter through S1 means D2
/// does not need a second pass over every call site.
///
/// # Safety
/// Every `LoadedDso` in `scope` whose `dyn_.hash` /
/// `dyn_.symtab` / `dyn_.strtab` are populated must have those
/// pointers reference the DSO's mapped image; `validate_dyn_pointers`
/// in `main.rs` runs this check at load time. Pointers must remain
/// valid for the call.
pub unsafe fn lookup(scope: &[LoadedDso], name: &[u8], version: Option<&[u8]>) -> Option<u64> {
    let _ = version; // wired by D2; Phase 76d.S1 is version-blind
    for dso in scope {
        let backend = match dso.dyn_.hash {
            Some(_) => Backend::SysV,
            None => continue,
        };
        let hit = match backend {
            Backend::SysV => unsafe { lookup_sysv(dso, name) },
        };
        if let Some(addr) = hit {
            return Some(addr);
        }
    }
    None
}

/// SysV `DT_HASH` walk against one DSO. Extracted from the Phase 76b
/// free-function `lookup_symbol` so the dispatcher in [`lookup`] can
/// invoke it per-DSO without duplicating chain-walk semantics.
///
/// # Safety
/// `dso.dyn_.hash` must reference at least 8 bytes of mapped memory
/// (the `nbuckets`/`nchain` header); the bucket and chain arrays must
/// fit within the DSO's image span. `validate_dyn_pointers` enforces
/// the header bound; the chain-walk's `hops <= nchain` guard plus the
/// `idx >= nchain` bail-out keep the in-loop reads bounded even on
/// corrupt tables.
unsafe fn lookup_sysv(dso: &LoadedDso, name: &[u8]) -> Option<u64> {
    let hash_ptr = dso.dyn_.hash?.as_ptr();
    let symtab = dso.dyn_.symtab?.as_ptr();
    let strtab = dso.dyn_.strtab?.as_ptr();
    let nbuckets = unsafe { *hash_ptr } as usize;
    let nchain = unsafe { *hash_ptr.add(1) } as usize;
    if nbuckets == 0 {
        return None;
    }
    let buckets = unsafe { hash_ptr.add(2) };
    let chain = unsafe { buckets.add(nbuckets) };
    let h = elf_hash(name);
    let mut idx = unsafe { *buckets.add(h as usize % nbuckets) };
    let mut hops = 0usize;
    while idx != 0 && hops <= nchain {
        if (idx as usize) >= nchain {
            break;
        }
        let sym = unsafe { &*symtab.add(idx as usize) };
        let nm = unsafe { crate::strtab_get(strtab, sym.st_name as u64, dso.dyn_.strsz) };
        if nm == name && sym.st_value != 0 {
            return Some(dso.load_bias.wrapping_add(sym.st_value));
        }
        idx = unsafe { *chain.add(idx as usize) };
        hops += 1;
    }
    None
}
