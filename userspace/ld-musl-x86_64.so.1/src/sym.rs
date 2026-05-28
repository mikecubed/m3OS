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
//! free-function `lookup_symbol` byte-for-byte when `version` is
//! `None`: SysV global scope, `STN_UNDEF == 0` terminator,
//! `st_value != 0` filter, `nchain` hops bound.
//!
//! ## Backend dispatch
//!
//! Each DSO is dispatched to a [`Backend`] based on which hash tags
//! its `PT_DYNAMIC` carries. Phase 76d.D1.3 added `Gnu` (preferred
//! when present); SysV is the fallback. Both backends return the
//! symbol's index in `DT_SYMTAB`, which the dispatcher then feeds
//! into the version-aware path (D2).
//!
//! ## Version-awareness
//!
//! Phase 76d.D2.2 wires the `version: Option<&[u8]>` parameter into
//! the walker. Behaviour:
//!
//!   * `version == None` — Phase 76b/c semantics (back-compat).
//!   * `version == Some(v)` — try each DSO for an exact-version
//!     match against the DSO's `DT_VERSYM` + `DT_VERDEF`. If no DSO
//!     provides the named symbol with the matching version:
//!       * Default mode (POSIX lazy + `LD_BIND_NOW` unset) — emit a
//!         serial warning and fall back to an unversioned scan.
//!       * Strict mode (`LD_BIND_NOW=1`, D2.3) — return `None`
//!         immediately and emit a serial error. The caller (apply_rela)
//!         surfaces this as a hard load-time failure.
//!
//! DSOs that carry NO `DT_VERSYM` (unversioned providers) satisfy
//! any version request for a matching name — matches the standard
//! glibc back-compat rule.

use ldso_core::dynlink::{LoadedDso, elf_hash};
use ldso_core::gnu_hash::{GnuHashHeader, GnuLookupOutcome, gnu_hash, gnu_hash_lookup};
use ldso_core::ver::{
    VER_NDX_GLOBAL, VER_NDX_LOCAL, VERSYM_HIDDEN, VERSYM_VERSION_MASK, VersionTable,
};

use crate::plt;

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

/// One per-DSO lookup hit. Carries both the resolved address and the
/// symbol-table index so the dispatcher can check `DT_VERSYM` /
/// `DT_VERDEF` for the version-aware path (D2.2).
#[derive(Clone, Copy)]
struct DsoHit {
    addr: u64,
    sym_idx: u32,
}

/// Look up `name` across `scope` and return the first DSO's
/// definition address, or `None` if no DSO defines it.
///
/// `version`:
///   * `None` → unversioned lookup (Phase 76b/c semantics).
///   * `Some(v)` → exact-version match required. Falls back to
///     unversioned with a serial warning when nothing matches in
///     default mode; returns `None` with a serial error in strict
///     mode (`plt::bind_now_set() == true`, Phase 76d.D2.3).
///
/// # Safety
/// Every `LoadedDso` in `scope` whose `dyn_.hash` / `dyn_.gnu_hash` /
/// `dyn_.symtab` / `dyn_.strtab` are populated must have those
/// pointers reference the DSO's mapped image; `validate_dyn_pointers`
/// in `main.rs` runs this check at load time. Pointers must remain
/// valid for the call.
pub unsafe fn lookup(scope: &[LoadedDso], name: &[u8], version: Option<&[u8]>) -> Option<u64> {
    // Pass 1: try every DSO for the name. When `version` is set, also
    // check the DSO's version constraint matches.
    let mut name_matched_somewhere = false;
    for dso in scope {
        let hit = match unsafe { lookup_in_dso(dso, name) } {
            Some(h) => h,
            None => continue,
        };
        // Phase 76d.D2 — for unversioned consumers (`version == None`),
        // skip hits whose `DT_VERSYM` entry has the VERSYM_HIDDEN bit
        // set. Hidden symbols are non-default version exports that
        // must NOT be returned to unversioned consumers (matches the
        // glibc rule + the documented intent in `ldso_core::ver`).
        if version.is_none() && unsafe { dso_symbol_is_hidden(dso, hit.sym_idx) } {
            // Same-name, hidden non-default — keep scanning for a
            // non-hidden definition in a later DSO.
            continue;
        }
        name_matched_somewhere = true;
        let requested = match version {
            Some(v) => v,
            None => return Some(hit.addr), // Unversioned lookup — first non-hidden match wins.
        };
        if unsafe { dso_version_matches(dso, hit.sym_idx, requested) } {
            return Some(hit.addr);
        }
        // Same-name, different-version — keep scanning. SysV semantics:
        // a versioned consumer can be satisfied by any DSO that defines
        // the matching version.
    }

    // Phase 76d.D2.2 — pass 1 found no exact-version match.
    if let Some(requested) = version {
        if !name_matched_somewhere {
            // Nothing in scope defines this name at all — neither
            // versioned nor unversioned fallback can help. Return
            // None silently; apply_rela will report the
            // undefined-symbol error.
            return None;
        }
        if plt::bind_now_set() {
            // D2.3 strict mode — hard fail with serial error.
            crate::serial(b"ldso: version mismatch (LD_BIND_NOW strict): symbol=");
            crate::serial(name);
            crate::serial(b" version=");
            crate::serial(requested);
            crate::serial(b"\n");
            return None;
        }
        crate::serial(b"ldso: version mismatch, falling back to unversioned: symbol=");
        crate::serial(name);
        crate::serial(b" version=");
        crate::serial(requested);
        crate::serial(b"\n");
        // Pass 2 — unversioned fallback. This block IS an unversioned
        // resolution, so it applies the same VERSYM_HIDDEN skip as the
        // pass-1 unversioned path: non-default hidden exports must not
        // surface during fallback either.
        for dso in scope {
            if let Some(hit) = unsafe { lookup_in_dso(dso, name) } {
                if unsafe { dso_symbol_is_hidden(dso, hit.sym_idx) } {
                    continue;
                }
                return Some(hit.addr);
            }
        }
    }

    None
}

/// Pick the GNU or SysV backend for `dso` based on which hash table
/// it carries and run it. Returns the resolved address + sym_idx if
/// the DSO defines `name`, or `None` if it doesn't.
unsafe fn lookup_in_dso(dso: &LoadedDso, name: &[u8]) -> Option<DsoHit> {
    let backend = if dso.dyn_.gnu_hash.is_some() {
        Backend::Gnu
    } else if dso.dyn_.hash.is_some() {
        Backend::SysV
    } else {
        return None;
    };
    match backend {
        Backend::SysV => unsafe { lookup_sysv(dso, name) },
        Backend::Gnu => unsafe { lookup_gnu(dso, name) },
    }
}

/// Phase 76d security hardening — read `versym[sym_idx]` only when the
/// 2-byte entry lies inside the DSO image. Returns `None` when the DSO
/// has no `DT_VERSYM`, or when the computed read would run past
/// `load_bias + image_len` (malformed table or an out-of-range
/// `sym_idx` produced by a corrupt hash table). `validate_dyn_pointers`
/// only checks the `DT_VERSYM` base pointer, so this per-index guard is
/// what keeps the parallel-array read inside the image. When
/// `image_len == 0` (placeholder DSO) the bound is unknown and the read
/// is trusted.
unsafe fn versym_entry(dso: &LoadedDso, sym_idx: u32) -> Option<u16> {
    let versym_ptr = dso.dyn_.versym?.as_ptr();
    if dso.image_len > 0
        && !ldso_core::bounds::elem_in_image(
            versym_ptr as u64,
            sym_idx as u64,
            2,
            dso.load_bias,
            dso.image_len,
        )
    {
        return None;
    }
    Some(unsafe { *versym_ptr.add(sym_idx as usize) })
}

/// Phase 76d.D2 — return `true` when the DSO's `DT_VERSYM` entry for
/// `sym_idx` has the `VERSYM_HIDDEN` (`0x8000`) bit set. Used by
/// `lookup` to skip non-default version exports when serving
/// unversioned consumers. DSOs without a `DT_VERSYM` (or an
/// out-of-image `sym_idx`) are never considered hidden.
unsafe fn dso_symbol_is_hidden(dso: &LoadedDso, sym_idx: u32) -> bool {
    matches!(unsafe { versym_entry(dso, sym_idx) }, Some(raw) if raw & VERSYM_HIDDEN != 0)
}

/// Phase 76d.D2.2 — return `true` when the DSO's `DT_VERSYM` /
/// `DT_VERDEF` say `sym_idx` is exported under `requested`. Returns
/// `true` for DSOs with no `DT_VERSYM` (unversioned providers
/// satisfy any version request — standard glibc back-compat).
/// Returns `true` when the symbol's version index is the special
/// `VER_NDX_GLOBAL` (1) — that's the unversioned default export
/// slot and matches any version request.
unsafe fn dso_version_matches(dso: &LoadedDso, sym_idx: u32, requested: &[u8]) -> bool {
    if dso.dyn_.versym.is_none() {
        return true; // Unversioned DSO satisfies any version request.
    }
    // VERSYM present — read the entry under the image-span guard. An
    // out-of-image `sym_idx` (corrupt table) cannot be verified, so
    // treat it as "no match" rather than dereferencing past the image.
    let raw_index = match unsafe { versym_entry(dso, sym_idx) } {
        Some(raw) => raw,
        None => return false,
    };
    let version_index = raw_index & VERSYM_VERSION_MASK;
    if version_index == VER_NDX_LOCAL || version_index == VER_NDX_GLOBAL {
        // Default / unversioned export — satisfies any version request.
        return true;
    }
    let verdef_ptr = match dso.dyn_.verdef {
        Some(p) => p.as_ptr(),
        None => return false, // VERSYM present but no VERDEF — no version names.
    };
    // Build slice views over the DSO's mapped image. We use the DSO's
    // strsz for strtab, and for verdef we bound the slice to the image
    // tail (`max_verdef_bytes` returns `image_end - verdef_ptr`, or a
    // 16 KiB fallback only when `image_len` is unknown). The pure-logic
    // walker bails at `verdef_num` records or the first out-of-range
    // offset, so an over-long slice is safe.
    let verdef_bytes = unsafe { core::slice::from_raw_parts(verdef_ptr, max_verdef_bytes(dso)) };
    let strtab_ptr = match dso.dyn_.strtab {
        Some(p) => p.as_ptr(),
        None => return false,
    };
    let strtab_bytes = unsafe { core::slice::from_raw_parts(strtab_ptr, dso.dyn_.strsz as usize) };
    let table = VersionTable {
        versym: &[],
        verdef_bytes,
        verdef_num: dso.dyn_.verdefnum as usize,
        verneed_bytes: &[],
        verneed_num: 0,
        strtab: strtab_bytes,
    };
    match table.defined_version_name(version_index) {
        Some(name) => name == requested,
        None => false,
    }
}

/// Bound the verdef byte slice. The verdef section sits inside the
/// DSO's image; without a `DT_VERDEFSZ` (which doesn't exist in
/// SysV ELF — only `DT_VERDEFNUM`), we cap at the image span minus
/// the verdef offset. For a typical DSO that's tens of KiB; the
/// pure-logic walker bails out at `verdef_num` records or the first
/// out-of-range offset, whichever comes first.
unsafe fn max_verdef_bytes(dso: &LoadedDso) -> usize {
    let verdef_ptr = match dso.dyn_.verdef {
        Some(p) => p.as_ptr() as u64,
        None => return 0,
    };
    if dso.image_len == 0 {
        return 16 * 1024;
    }
    let image_end = dso.load_bias.saturating_add(dso.image_len);
    image_end.saturating_sub(verdef_ptr) as usize
}

/// Phase 76d.D1.2 — GNU `DT_GNU_HASH` runtime walker. Returns the
/// resolved address + sym_idx so the dispatcher can route the result
/// through the version-aware path.
///
/// # Safety
/// `dso.dyn_.gnu_hash` must reference at least 16 bytes (the four
/// `u32` header words) of mapped memory; `validate_dyn_pointers`
/// enforces this at load time. The bloom, bucket, and hash arrays
/// derived from `bloom_size` / `nbuckets` are bounds-checked against
/// the DSO's `load_bias` + `image_len` window before any of their
/// elements are read. Chain hops are capped via the GNU end-marker
/// bit, a hard `MAX_HOPS` ceiling, and the per-element hash-array
/// bound (whichever comes first).
unsafe fn lookup_gnu(dso: &LoadedDso, name: &[u8]) -> Option<DsoHit> {
    let header = dso.dyn_.gnu_hash?.as_ptr();
    let symtab = dso.dyn_.symtab?.as_ptr();
    let strtab = dso.dyn_.strtab?.as_ptr();
    let nbuckets = unsafe { *header };
    let symoffset = unsafe { *header.add(1) };
    let bloom_size = unsafe { *header.add(2) };
    let bloom_shift = unsafe { *header.add(3) };
    if nbuckets == 0 || bloom_size == 0 {
        return None;
    }
    // `header.add(4)` is a fixed 16-byte offset; `validate_dyn_pointers`
    // proved the 16-byte header is in-image and provenance is the whole
    // mmap, so this small constant `.add` is in-bounds. The bucket/hash
    // array bases, by contrast, are offset by the UNTRUSTED `bloom_size`
    // / `nbuckets`, so they are derived with integer arithmetic — never
    // `<*const T>::add`, whose in-bounds precondition a corrupt header
    // could violate (UB even before any dereference). The typed pointers
    // are materialized only after the ranges are proven in-image.
    let bloom_ptr = unsafe { header.add(4) } as *const u64;
    let bloom_bytes = (bloom_size as u64).saturating_mul(8);
    let buckets_bytes = (nbuckets as u64).saturating_mul(4);
    let buckets_addr = (bloom_ptr as u64).wrapping_add(bloom_bytes);
    let hashes_addr = buckets_addr.wrapping_add(buckets_bytes);

    if dso.image_len > 0 {
        let bloom_ok = ldso_core::bounds::range_in_image(
            bloom_ptr as u64,
            bloom_bytes,
            dso.load_bias,
            dso.image_len,
        );
        let buckets_ok = ldso_core::bounds::range_in_image(
            buckets_addr,
            buckets_bytes,
            dso.load_bias,
            dso.image_len,
        );
        if !bloom_ok || !buckets_ok {
            return None;
        }
    }
    // Safe to form the typed array pointers now: either the ranges were
    // proven in-image above, or `image_len == 0` (placeholder DSO, no
    // real mapping — host-test shape only). Per-element `.add` below uses
    // indices bounded by `bloom_size` / `nbuckets` / `chain_len`.
    let buckets_ptr = buckets_addr as *const u32;
    let hashes_ptr = hashes_addr as *const u32;

    // Number of complete 4-byte hash-table slots that fit between
    // `hashes_addr` and the image end (unbounded when `image_len` is
    // unknown — placeholder DSO). Each `chain_idx` must be strictly
    // less than this. A `span < 4` window (not even one full `u32`
    // remains, including `hashes_addr == image_end`) yields 0 slots, so
    // every read — including `chain_idx == 0` — is rejected before it
    // can run off the image edge.
    let chain_len = if dso.image_len > 0 {
        let image_end = dso.load_bias.saturating_add(dso.image_len);
        let span = image_end.saturating_sub(hashes_addr);
        (span / 4) as usize
    } else {
        usize::MAX
    };

    let h = gnu_hash(name);
    let bit0 = 1u64 << (h % 64);
    let bit1 = 1u64 << (h.wrapping_shr(bloom_shift) % 64);
    let mask = bit0 | bit1;
    let word_idx = (h as usize / 64) % bloom_size as usize;
    let bloom_word = unsafe { *bloom_ptr.add(word_idx) };
    if (bloom_word & mask) != mask {
        return None;
    }

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
        if chain_idx >= chain_len {
            return None;
        }
        let h2 = unsafe { *hashes_ptr.add(chain_idx) };
        if (h | 1) == (h2 | 1) {
            let sym = unsafe { crate::sym_entry(symtab, sym_idx, dso.load_bias, dso.image_len) }?;
            let nm = unsafe { crate::strtab_get(strtab, sym.st_name as u64, dso.dyn_.strsz) };
            if nm == name && sym.st_value != 0 {
                return Some(DsoHit {
                    addr: dso.load_bias.wrapping_add(sym.st_value),
                    sym_idx,
                });
            }
        }
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

/// SysV `DT_HASH` walk against one DSO. Returns the resolved address
/// + sym_idx so the dispatcher can route the result through the
///   version-aware path.
///
/// # Safety
/// `dso.dyn_.hash` must reference at least 8 bytes of mapped memory
/// (the `nbuckets`/`nchain` header); `validate_dyn_pointers` enforces
/// that header bound. The bucket (`nbuckets`×`u32`) and chain
/// (`nchain`×`u32`) arrays — whose lengths come from the untrusted
/// header — are clamped against `load_bias + image_len` before any
/// element is read, so a corrupt `nbuckets`/`nchain` cannot drive an
/// out-of-image read. The symtab read routes through the bounded
/// `crate::sym_entry`. (`image_len == 0` is the placeholder shape — the
/// span is unknown and the legacy `nchain`-relative guards apply.)
unsafe fn lookup_sysv(dso: &LoadedDso, name: &[u8]) -> Option<DsoHit> {
    let hash_ptr = dso.dyn_.hash?.as_ptr();
    let symtab = dso.dyn_.symtab?.as_ptr();
    let strtab = dso.dyn_.strtab?.as_ptr();
    let nbuckets = unsafe { *hash_ptr } as usize;
    let nchain = unsafe { *hash_ptr.add(1) } as usize;
    if nbuckets == 0 {
        return None;
    }
    // `hash_ptr.add(2)` is a fixed 8-byte offset within the validated
    // header / image. The chain array, by contrast, is offset by the
    // UNTRUSTED `nbuckets`, so its base is derived with integer
    // arithmetic — never `<*const T>::add`, whose in-bounds precondition
    // a corrupt header could violate (UB even before a dereference).
    let buckets = unsafe { hash_ptr.add(2) };
    let buckets_bytes = (nbuckets as u64).saturating_mul(4);
    let chain_addr = (buckets as u64).wrapping_add(buckets_bytes);
    // Clamp both arrays against the image span. Each entry is a `u32`
    // (4 bytes); buckets has `nbuckets` entries, chain has `nchain`.
    if dso.image_len > 0 {
        let buckets_ok = ldso_core::bounds::range_in_image(
            buckets as u64,
            buckets_bytes,
            dso.load_bias,
            dso.image_len,
        );
        let chain_ok = ldso_core::bounds::range_in_image(
            chain_addr,
            (nchain as u64).saturating_mul(4),
            dso.load_bias,
            dso.image_len,
        );
        if !buckets_ok || !chain_ok {
            return None;
        }
    }
    // Materialized only after the range is proven in-image (or
    // `image_len == 0` placeholder). Per-element `.add(idx)` below uses
    // `idx < nchain`, within the validated range.
    let chain = chain_addr as *const u32;
    let h = elf_hash(name);
    let mut idx = unsafe { *buckets.add(h as usize % nbuckets) };
    let mut hops = 0usize;
    while idx != 0 && hops <= nchain {
        if (idx as usize) >= nchain {
            break;
        }
        let sym = unsafe { crate::sym_entry(symtab, idx, dso.load_bias, dso.image_len) }?;
        let nm = unsafe { crate::strtab_get(strtab, sym.st_name as u64, dso.dyn_.strsz) };
        if nm == name && sym.st_value != 0 {
            return Some(DsoHit {
                addr: dso.load_bias.wrapping_add(sym.st_value),
                sym_idx: idx,
            });
        }
        idx = unsafe { *chain.add(idx as usize) };
        hops += 1;
    }
    None
}
