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
use ldso_core::gnu_hash::{GnuHashHeader, GnuLookupOutcome, gnu_hash, gnu_hash_lookup};

/// Backend chosen for one DSO's symbol-table walk.
///
/// Phase 76d.D1.3 added [`Backend::Gnu`] so libraries built with
/// `--hash-style=gnu` (or `--hash-style=both`) get the Bloom-filter
/// short-circuit. When both tables are present the dispatcher picks
/// GNU; when only SysV is present it falls through to that.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Backend {
    SysV,
    Gnu,
}

/// Look up `name` across `scope` and return the first DSO's
/// definition address, or `None` if no DSO defines it.
///
/// `version` is reserved for D2 and ignored by Phase 76d.S1/D1
/// callers (they pass `None`). Carrying the parameter through S1
/// means D2 does not need a second pass over every call site.
///
/// # Safety
/// Every `LoadedDso` in `scope` whose `dyn_.hash` / `dyn_.gnu_hash` /
/// `dyn_.symtab` / `dyn_.strtab` are populated must have those
/// pointers reference the DSO's mapped image; `validate_dyn_pointers`
/// in `main.rs` runs this check at load time. Pointers must remain
/// valid for the call.
pub unsafe fn lookup(scope: &[LoadedDso], name: &[u8], version: Option<&[u8]>) -> Option<u64> {
    let _ = version; // wired by D2; Phase 76d.S1/D1 are version-blind
    for dso in scope {
        // D1.3 dispatcher: prefer GNU when present (Bloom-filter
        // short-circuit); fall back to SysV when only DT_HASH is
        // available; skip entirely when neither table is populated.
        let backend = if dso.dyn_.gnu_hash.is_some() {
            Backend::Gnu
        } else if dso.dyn_.hash.is_some() {
            Backend::SysV
        } else {
            continue;
        };
        let hit = match backend {
            Backend::SysV => unsafe { lookup_sysv(dso, name) },
            Backend::Gnu => unsafe { lookup_gnu(dso, name) },
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
/// Phase 76d.D1.2 — GNU `DT_GNU_HASH` runtime walker. Mirrors
/// [`lookup_sysv`] in shape (per-DSO, returns the relocated symbol
/// address) but consults the GNU table's Bloom filter + bucket +
/// chain layout.
///
/// The pure-logic walk lives in `ldso_core::gnu_hash::gnu_hash_lookup`;
/// this function wraps it with raw-pointer reads of the in-memory
/// table.
///
/// # Safety
/// `dso.dyn_.gnu_hash` must reference at least 16 bytes (the four
/// `u32` header words) of mapped memory. The header's `nbuckets`,
/// `bloom_size`, and the chain table length must keep all subsequent
/// reads inside the DSO image (the runtime currently trusts the
/// validate_dyn_pointers pass at load time + the chain end-marker bit
/// to bound the walk).
unsafe fn lookup_gnu(dso: &LoadedDso, name: &[u8]) -> Option<u64> {
    let header = dso.dyn_.gnu_hash?.as_ptr();
    let symtab = dso.dyn_.symtab?.as_ptr();
    let strtab = dso.dyn_.strtab?.as_ptr();
    // Header is four u32 words: [nbuckets, symoffset, bloom_size, bloom_shift].
    let nbuckets = unsafe { *header };
    let symoffset = unsafe { *header.add(1) };
    let bloom_size = unsafe { *header.add(2) };
    let bloom_shift = unsafe { *header.add(3) };
    if nbuckets == 0 || bloom_size == 0 {
        return None;
    }
    // Bloom array follows the header (u64 aligned). The header is 16
    // bytes (4 × u32), so the bloom array starts at `header + 4` u32s.
    let bloom_ptr = unsafe { header.add(4) } as *const u64;
    let buckets_ptr = unsafe { bloom_ptr.add(bloom_size as usize) } as *const u32;
    let hashes_ptr = unsafe { buckets_ptr.add(nbuckets as usize) };

    // Bloom probe inline so the dispatcher exits without ever forming
    // a temporary slice over the bucket/chain arrays when the symbol is
    // proven absent. This is the D1 hot-path short-circuit.
    let h = gnu_hash(name);
    let bit0 = 1u64 << (h % 64);
    let bit1 = 1u64 << (h.wrapping_shr(bloom_shift) % 64);
    let mask = bit0 | bit1;
    let word_idx = (h as usize / 64) % bloom_size as usize;
    let bloom_word = unsafe { *bloom_ptr.add(word_idx) };
    if (bloom_word & mask) != mask {
        return None;
    }

    // Bucket walk inline. Build a chain length bound by scanning until
    // a chain-end marker (bit 0 of the hash entry); cap at a generous
    // 65536 to defend against a corrupt table that lacks an end marker.
    let bucket_idx = (h % nbuckets) as usize;
    let mut sym_idx = unsafe { *buckets_ptr.add(bucket_idx) };
    if sym_idx < symoffset {
        return None;
    }
    let mut hops: usize = 0;
    const MAX_HOPS: usize = 65536;
    loop {
        if hops >= MAX_HOPS {
            return None;
        }
        let chain_idx = (sym_idx - symoffset) as usize;
        let h2 = unsafe { *hashes_ptr.add(chain_idx) };
        // Compare upper 31 bits (bit 0 is the chain-end marker).
        if (h | 1) == (h2 | 1) {
            let sym = unsafe { &*symtab.add(sym_idx as usize) };
            let nm = unsafe { crate::strtab_get(strtab, sym.st_name as u64, dso.dyn_.strsz) };
            if nm == name && sym.st_value != 0 {
                return Some(dso.load_bias.wrapping_add(sym.st_value));
            }
        }
        // Bit 0 set ⇒ chain end.
        if (h2 & 1) != 0 {
            return None;
        }
        sym_idx += 1;
        hops += 1;
    }
}

/// Unused-but-imported: keeps `gnu_hash_lookup` available for host
/// tests / future call sites that want the slice-based version
/// instead of the inline header walk above. The inline walk avoids
/// the slice-construction step in the hot path.
#[allow(dead_code)]
fn _gnu_hash_lookup_keepalive() -> GnuLookupOutcome {
    let buckets = [0u32];
    let hashes = [1u32];
    let bloom = [0u64];
    gnu_hash_lookup(
        GnuHashHeader {
            nbuckets: 1,
            symoffset: 1,
            bloom_shift: 6,
        },
        &bloom,
        &buckets,
        &hashes,
        b"",
        |_| None,
    )
}

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
