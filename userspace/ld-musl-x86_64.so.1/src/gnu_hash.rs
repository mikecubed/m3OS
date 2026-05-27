//! Phase 76d.D1 — GNU hash table primitives.
//!
//! `DT_GNU_HASH` is the modern hash table format emitted by GNU `ld`
//! when invoked with `--hash-style=gnu` (or `--hash-style=both`). It
//! exists because the SysV `DT_HASH` walker degenerates on real-world
//! symbol tables that carry tens of thousands of names: every lookup
//! pays for a chain walk regardless of whether the symbol is even
//! present in the DSO.
//!
//! `DT_GNU_HASH` adds two short-circuits:
//!
//! 1. A **Bloom filter** at the head of the table that probabilistically
//!    answers "is this symbol definitely not here?" in O(1) without
//!    touching the bucket array or the symbol table.
//! 2. The hash value of each in-table symbol is stored alongside the
//!    chain so the walker can skip-then-bail on chain links whose hash
//!    upper-bits don't match, avoiding a string compare per hop.
//!
//! See the ABI write-up at
//! <https://flapenguin.me/elf-dt-gnu-hash> and the Sun/GNU spec at
//! <https://sourceware.org/legacy-ml/binutils/2006-10/msg00377.html>.
//!
//! ## Table layout
//!
//! ```text
//! header: [nbuckets, symoffset, bloom_size, bloom_shift] (4 × u32)
//! bloom:  [u64; bloom_size]   (the filter — bloom_size is power-of-two)
//! buckets:[u32; nbuckets]     (each value is the first sym index in
//!                              a chain, OR 0 = empty bucket)
//! hashes: [u32; …]            (one u32 per symbol from `symoffset`
//!                              upward; bit 0 == 1 marks chain end)
//! ```
//!
//! `symoffset` is the first symbol index that is referenced by the
//! GNU hash; symbols `[0, symoffset)` are unreachable through the
//! GNU walker (typically `STN_UNDEF` and any locals the linker put
//! at the head of the dynsym).
//!
//! ## Why a separate module
//!
//! `dynlink.rs` already houses the SysV `DT_HASH` walker. Splitting
//! the GNU helpers into their own module keeps the SysV path's
//! footprint unchanged and gives `cfg(test)` a clean surface for
//! D1.1/D1.2 fixtures. The runtime dispatcher in `crate::sym` re-exports
//! and wraps these helpers.

/// GNU djb2-derived symbol hash. Identical to the function the GNU ABI
/// references as `dl_new_hash`:
///
/// ```c
/// uint32_t dl_new_hash(const char *s) {
///     uint32_t h = 5381;
///     for (unsigned char c; (c = *s); s++)
///         h = h * 33 + c;       // equivalently: (h << 5) + h + c
///     return h;
///  }
/// ```
///
/// Takes a byte slice (NOT a NUL-terminated C string) so the function
/// is callable from host tests without `unsafe`.
pub fn gnu_hash(name: &[u8]) -> u32 {
    let mut h: u32 = 5381;
    for &b in name {
        h = h.wrapping_mul(33).wrapping_add(b as u32);
    }
    h
}

/// Bloom-filter probe. Returns `true` if the filter says the symbol
/// **might** be in the table; returns `false` only when the filter
/// proves the symbol is **definitely not** present (the negative
/// answer is always correct; the positive answer is probabilistic and
/// requires the bucket walk to confirm).
///
/// The probe sets two bits per inserted symbol (a "double hashing"
/// scheme):
///
/// * `bit0 = 1 << (hash % 64)`
/// * `bit1 = 1 << ((hash >> bloom_shift) % 64)`
///
/// and reads the `(hash / 64) % bloom_size`-th 64-bit word. The symbol
/// is *possibly* present iff BOTH bits are set in that word.
///
/// `bloom_size` is always a power of two in GNU `ld` output, so
/// `% bloom_size` collapses to a bit-mask in the hot path — but this
/// helper takes the generic-modulo form because it's pure-logic and
/// the few cycles do not matter.
pub fn bloom_probe(hash: u32, bloom: &[u64], bloom_shift: u32) -> bool {
    if bloom.is_empty() {
        // Defensively defined: an empty filter cannot accept anything.
        return false;
    }
    let bit0: u64 = 1u64 << (hash % 64);
    let bit1: u64 = 1u64 << (hash.wrapping_shr(bloom_shift) % 64);
    let mask: u64 = bit0 | bit1;
    let word_idx = (hash as usize / 64) % bloom.len();
    let word = bloom[word_idx];
    (word & mask) == mask
}

/// Result of walking the GNU hash chain for `name`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GnuLookupOutcome {
    /// No symbol with that name. Either the Bloom filter ruled it out,
    /// the bucket was empty, the chain ended without a hash match, or
    /// every hash-matching candidate failed the byte-exact name
    /// compare (hash collision).
    NotFound,
    /// A symbol whose hash matched the chain entry AND whose byte-exact
    /// name matches `name`. The payload is the symbol table index the
    /// caller should look up in `DT_SYMTAB`.
    Found(u32),
    /// The header carried a malformed layout (e.g. zero nbuckets, or
    /// bloom_size == 0). Treat the same as `NotFound` at the dispatcher
    /// level — the runtime falls through to SysV when GNU returns
    /// nothing useful — but expose the variant so host tests can pin
    /// the rejection behaviour.
    Malformed,
}

/// Decoded `DT_GNU_HASH` header (the four `u32` words at the top of
/// the table). Bundled so [`gnu_hash_lookup`] takes a single header
/// argument instead of three scalars.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GnuHashHeader {
    pub nbuckets: u32,
    pub symoffset: u32,
    pub bloom_shift: u32,
}

/// Pure-logic GNU hash lookup. Mirrors the runtime helper that walks a
/// real `DT_GNU_HASH` table, but takes every input as a borrow so the
/// function is callable from host tests without `unsafe`.
///
/// `header.nbuckets`, `header.symoffset`, `header.bloom_shift` come
/// from the four-word DT_GNU_HASH header. `bloom` is the per-table
/// filter; `buckets` and `hashes` are the post-bloom arrays.
/// `name_of_symbol` resolves a symbol index back into the raw bytes
/// of its name (caller-provided so the runtime can wrap `DT_SYMTAB`
/// + `DT_STRTAB` reads without `gnu_hash` needing to know about
///   either).
///
/// The walker stops at the first chain entry whose top-31 hash bits
/// match `gnu_hash(name) >> 1` AND whose resolved name is byte-equal
/// to `name`. Chain ends are marked by bit 0 set in the hash entry.
pub fn gnu_hash_lookup(
    header: GnuHashHeader,
    bloom: &[u64],
    buckets: &[u32],
    hashes: &[u32],
    name: &[u8],
    mut name_of_symbol: impl FnMut(u32) -> Option<&'static [u8]>,
) -> GnuLookupOutcome {
    let GnuHashHeader {
        nbuckets,
        symoffset,
        bloom_shift,
    } = header;
    if nbuckets == 0 || bloom.is_empty() {
        return GnuLookupOutcome::Malformed;
    }
    if (buckets.len() as u32) != nbuckets {
        return GnuLookupOutcome::Malformed;
    }
    let h = gnu_hash(name);
    if !bloom_probe(h, bloom, bloom_shift) {
        return GnuLookupOutcome::NotFound;
    }
    let bucket_idx = (h % nbuckets) as usize;
    let mut sym_idx = buckets[bucket_idx];
    if sym_idx < symoffset {
        // 0 (empty bucket) or any value below symoffset is unreachable
        // through the GNU walker.
        return GnuLookupOutcome::NotFound;
    }
    loop {
        let chain_idx = (sym_idx - symoffset) as usize;
        if chain_idx >= hashes.len() {
            // Corrupt chain — bail to NotFound rather than reading
            // out-of-bounds.
            return GnuLookupOutcome::NotFound;
        }
        let h2 = hashes[chain_idx];
        // Upper 31 bits of `gnu_hash(name)` must match the chain entry
        // (the bottom bit is the chain-end marker, not part of the
        // hash). `h | 1 == h2 | 1` collapses to upper-31-bit equality.
        if (h | 1) == (h2 | 1)
            && let Some(resolved) = name_of_symbol(sym_idx)
            && resolved == name
        {
            return GnuLookupOutcome::Found(sym_idx);
        }
        // Bit 0 set on the chain entry marks the chain end.
        if (h2 & 1) != 0 {
            return GnuLookupOutcome::NotFound;
        }
        sym_idx += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------------
    // D1.1 — `gnu_hash` known-answer vectors.
    // ---------------------------------------------------------------------
    //
    // Reference values cross-checked against:
    //   * https://flapenguin.me/elf-dt-gnu-hash (worked examples)
    //   * `python3 -c "h=5381\nfor c in b'…':\n  h=(h*33+c) & 0xFFFFFFFF"`
    //   * gcc-emitted DT_GNU_HASH on a real .so

    #[test]
    fn gnu_hash_empty_returns_seed() {
        // Empty input must return the djb2 seed unchanged.
        assert_eq!(gnu_hash(b""), 5381);
    }

    #[test]
    fn gnu_hash_single_byte() {
        // h = 5381 * 33 + b
        assert_eq!(
            gnu_hash(b"a"),
            5381u32.wrapping_mul(33).wrapping_add(b'a' as u32)
        );
        assert_eq!(gnu_hash(b"a"), 177670);
    }

    #[test]
    fn gnu_hash_known_symbol_printf() {
        // Cross-referenced against the GNU ABI spec worked example.
        // Computed iteratively:
        //   5381*33+'p' = 177666 + 112 = 177778... actually let's
        //   compute and pin the known answer directly.
        let h = gnu_hash(b"printf");
        // Pre-computed: starting from 5381, multiply by 33 and add each
        // byte of "printf" (p=0x70, r=0x72, i=0x69, n=0x6e, t=0x74, f=0x66).
        let mut expected: u32 = 5381;
        for &b in b"printf" {
            expected = expected.wrapping_mul(33).wrapping_add(b as u32);
        }
        assert_eq!(h, expected);
        // The canonical published value for "printf" is 0x156B2BB8.
        assert_eq!(h, 0x156B_2BB8);
    }

    #[test]
    fn gnu_hash_known_symbol_exit() {
        assert_eq!(gnu_hash(b"exit"), 0x7C967E3F);
    }

    #[test]
    fn gnu_hash_known_symbol_dlopen() {
        // Cross-checked against:
        //   python3 -c "h=5381\nfor c in b'dlopen':\n  h=(h*33+c) & 0xFFFFFFFF\nprint(hex(h))"
        assert_eq!(gnu_hash(b"dlopen"), 0xF904_0207);
    }

    #[test]
    fn gnu_hash_long_name_does_not_overflow_panic() {
        let name: Vec<u8> = (0..256).map(|i| (i % 256) as u8).collect();
        // Mostly: just doesn't panic with wrapping_mul/wrapping_add.
        let _h = gnu_hash(&name);
    }

    // ---------------------------------------------------------------------
    // D1.1 — `bloom_probe` semantics.
    // ---------------------------------------------------------------------

    #[test]
    fn bloom_probe_zero_filter_rejects_everything() {
        let bloom = [0u64; 4];
        // Any hash must miss when both bits are clear.
        assert!(!bloom_probe(0, &bloom, 0));
        assert!(!bloom_probe(42, &bloom, 5));
        assert!(!bloom_probe(0xDEAD_BEEF, &bloom, 13));
    }

    #[test]
    fn bloom_probe_set_bits_for_known_hash_accepts() {
        // Pick hash = 0x1234_5678, bloom_size = 4, bloom_shift = 7.
        let h: u32 = 0x1234_5678;
        let bloom_shift: u32 = 7;
        let bit0 = 1u64 << (h % 64);
        let bit1 = 1u64 << ((h >> bloom_shift) % 64);
        let word_idx = (h as usize / 64) % 4;
        let mut bloom = [0u64; 4];
        bloom[word_idx] = bit0 | bit1;
        assert!(bloom_probe(h, &bloom, bloom_shift));
    }

    #[test]
    fn bloom_probe_only_one_bit_set_rejects() {
        let h: u32 = 0x1234_5678;
        let bloom_shift: u32 = 7;
        let bit0 = 1u64 << (h % 64);
        let word_idx = (h as usize / 64) % 4;
        let mut bloom = [0u64; 4];
        bloom[word_idx] = bit0; // only first bit, not both
        assert!(!bloom_probe(h, &bloom, bloom_shift));
    }

    #[test]
    fn bloom_probe_empty_filter_rejects() {
        let bloom: [u64; 0] = [];
        assert!(!bloom_probe(0, &bloom, 5));
    }

    // ---------------------------------------------------------------------
    // D1.2 — `gnu_hash_lookup` end-to-end with a hand-built table.
    // ---------------------------------------------------------------------

    /// Build a minimal but valid GNU hash table for `names`. Returns
    /// `(nbuckets, symoffset, bloom_shift, bloom, buckets, hashes)`.
    /// `symoffset` is fixed at 1 so symbol index 0 stays the
    /// always-undefined `STN_UNDEF` slot.
    fn build_gnu_hash_table(
        names: &[&'static [u8]],
        nbuckets: u32,
        bloom_size: usize,
        bloom_shift: u32,
    ) -> (u32, u32, u32, Vec<u64>, Vec<u32>, Vec<u32>) {
        let symoffset: u32 = 1;
        let mut bloom = vec![0u64; bloom_size];
        // Bucket -> chain of (sym_idx, h).
        let mut bucket_chains: Vec<Vec<(u32, u32)>> = (0..nbuckets).map(|_| Vec::new()).collect();
        for (i, name) in names.iter().enumerate() {
            let sym_idx = symoffset + i as u32;
            let h = gnu_hash(name);
            let bit0 = 1u64 << (h % 64);
            let bit1 = 1u64 << ((h >> bloom_shift) % 64);
            let word_idx = (h as usize / 64) % bloom_size;
            bloom[word_idx] |= bit0 | bit1;
            let bucket = (h % nbuckets) as usize;
            bucket_chains[bucket].push((sym_idx, h));
        }
        // Sort each chain by sym_idx so the chain table can be laid out
        // contiguously (GNU `ld` re-sorts the dynsym after assigning
        // hashes to buckets so consecutive in-table symbols share a
        // bucket).
        for chain in bucket_chains.iter_mut() {
            chain.sort_by_key(|(idx, _)| *idx);
        }
        // Flatten: emit each bucket's chain end-to-end. Re-assign
        // contiguous sym_idx values to each chain so the test layout
        // matches GNU `ld` post-sort output.
        let mut buckets = vec![0u32; nbuckets as usize];
        let mut hashes = Vec::new();
        let mut cur_sym = symoffset;
        for (b_idx, chain) in bucket_chains.iter().enumerate() {
            if chain.is_empty() {
                continue;
            }
            buckets[b_idx] = cur_sym;
            for (i, (_orig_idx, h)) in chain.iter().enumerate() {
                let last = i + 1 == chain.len();
                let entry = if last { *h | 1 } else { *h & !1 };
                hashes.push(entry);
                cur_sym += 1;
            }
        }
        (nbuckets, symoffset, bloom_shift, bloom, buckets, hashes)
    }

    /// Wrapper that drives `gnu_hash_lookup` from the per-test build
    /// helper and resolves symbol names through a `Vec<&'static [u8]>`
    /// the test owns. The returned sym_idx is the post-sort index in
    /// `hashes` PLUS `symoffset`.
    fn run_lookup(
        names: &[&'static [u8]],
        nbuckets: u32,
        bloom_size: usize,
        bloom_shift: u32,
        query: &[u8],
    ) -> GnuLookupOutcome {
        let (nb, symoffset, shift, bloom, buckets, hashes) =
            build_gnu_hash_table(names, nbuckets, bloom_size, bloom_shift);
        let header = GnuHashHeader {
            nbuckets: nb,
            symoffset,
            bloom_shift: shift,
        };
        // The hash table assigns sym indices post-sort; build a
        // resolver that walks each bucket in the same order as the
        // builder to recover the name for an index.
        let mut bucket_chains: Vec<Vec<(u32, u32, &'static [u8])>> =
            (0..nb).map(|_| Vec::new()).collect();
        for (i, name) in names.iter().enumerate() {
            let sym_idx = symoffset + i as u32;
            let h = gnu_hash(name);
            let bucket = (h % nb) as usize;
            bucket_chains[bucket].push((sym_idx, h, *name));
        }
        for chain in bucket_chains.iter_mut() {
            chain.sort_by_key(|(idx, _, _)| *idx);
        }
        let mut resolved_names: Vec<&'static [u8]> = Vec::new();
        for chain in bucket_chains.iter() {
            for (_, _, name) in chain {
                resolved_names.push(*name);
            }
        }
        let resolver = move |sym_idx: u32| -> Option<&'static [u8]> {
            let cidx = (sym_idx - symoffset) as usize;
            resolved_names.get(cidx).copied()
        };
        gnu_hash_lookup(header, &bloom, &buckets, &hashes, query, resolver)
    }

    #[test]
    fn gnu_lookup_finds_single_symbol() {
        let names: &[&[u8]] = &[b"hello_str"];
        let outcome = run_lookup(names, 1, 1, 6, b"hello_str");
        assert!(matches!(outcome, GnuLookupOutcome::Found(_)));
    }

    #[test]
    fn gnu_lookup_misses_absent_symbol() {
        let names: &[&[u8]] = &[b"hello_str"];
        let outcome = run_lookup(names, 1, 1, 6, b"goodbye_str");
        // Bloom filter or chain walk both legitimate ways to miss.
        assert!(matches!(outcome, GnuLookupOutcome::NotFound));
    }

    #[test]
    fn gnu_lookup_multi_bucket() {
        let names: &[&[u8]] = &[
            b"foo", b"bar", b"baz", b"qux", b"printf", b"exit", b"dlopen",
        ];
        for name in names {
            let outcome = run_lookup(names, 4, 2, 6, name);
            assert!(
                matches!(outcome, GnuLookupOutcome::Found(_)),
                "expected to find {:?}, got {outcome:?}",
                core::str::from_utf8(name)
            );
        }
    }

    #[test]
    fn gnu_lookup_walks_chain_when_first_entry_is_collision() {
        // Force two names into the same bucket by using nbuckets=1.
        let names: &[&[u8]] = &[b"alpha", b"beta", b"gamma", b"delta"];
        for name in names {
            let outcome = run_lookup(names, 1, 2, 6, name);
            assert!(
                matches!(outcome, GnuLookupOutcome::Found(_)),
                "expected to find {:?}, got {outcome:?}",
                core::str::from_utf8(name)
            );
        }
    }

    #[test]
    fn gnu_lookup_rejects_empty_filter() {
        let _names: &[&[u8]] = &[b"hello"];
        let outcome = gnu_hash_lookup(
            GnuHashHeader {
                nbuckets: 1,
                symoffset: 1,
                bloom_shift: 6,
            },
            &[], // empty bloom
            &[1u32],
            &[0u32 | 1],
            b"hello",
            |_| Some(&b"hello"[..]),
        );
        assert!(matches!(outcome, GnuLookupOutcome::Malformed));
    }

    #[test]
    fn gnu_lookup_rejects_zero_nbuckets() {
        let outcome = gnu_hash_lookup(
            GnuHashHeader {
                nbuckets: 0,
                symoffset: 1,
                bloom_shift: 6,
            },
            &[0u64],
            &[],
            &[],
            b"hello",
            |_| Some(&b"hello"[..]),
        );
        assert!(matches!(outcome, GnuLookupOutcome::Malformed));
    }

    #[test]
    fn gnu_lookup_rejects_buckets_len_mismatch() {
        // nbuckets=2 but buckets slice has 1 entry.
        let outcome = gnu_hash_lookup(
            GnuHashHeader {
                nbuckets: 2,
                symoffset: 1,
                bloom_shift: 6,
            },
            &[0u64],
            &[1u32],
            &[1u32],
            b"hello",
            |_| Some(&b"hello"[..]),
        );
        assert!(matches!(outcome, GnuLookupOutcome::Malformed));
    }

    #[test]
    fn gnu_lookup_hash_collision_then_real_match() {
        // Two names whose hash differs only in bit 0 (chain marker)
        // should still resolve correctly — the (h | 1) == (h2 | 1)
        // check should ignore the chain bit.
        let names: &[&[u8]] = &[b"sym_a", b"sym_b"];
        for name in names {
            let outcome = run_lookup(names, 1, 2, 6, name);
            assert!(matches!(outcome, GnuLookupOutcome::Found(_)));
        }
    }
}
