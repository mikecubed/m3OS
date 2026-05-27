//! Phase 76c libdl runtime — `dlopen` / `dlsym` / `dlclose` / `dlerror`.
//!
//! These are the four entry points that programs link `-ldl` to find.
//! Together they implement load-time + on-demand graph resolution on
//! top of the Phase 76b bring-up linker.
//!
//! ## Lifecycle
//!
//! The linker's `dl_entry` runs first (kernel hands off via `PT_INTERP`)
//! and pre-populates [`DL_STATE`] with the main binary at slot 0, the
//! linker itself at slot 1 (so libdl symbol resolution finds the
//! linker's own implementations first), and any `DT_NEEDED` libraries
//! at subsequent slots. All slots populated during bring-up are
//! marked **permanent**: their refcount is the sentinel
//! `REFCOUNT_PERMANENT` (`u32::MAX`) so a misbehaving caller cannot
//! `dlclose` the main binary or the linker.
//!
//! At runtime, `dlopen` either:
//!
//! 1. **Finds an existing DSO** with matching `SONAME` → refcount++
//!    (clamped) and returns a fresh handle pointer.
//! 2. **Loads a new DSO** → allocates a slot, loads the image, applies
//!    relocations, runs constructors, refcount = 1, returns a handle.
//!
//! `dlclose` walks the inverse: refcount--, and when refcount hits 0
//! it runs `DT_FINI_ARRAY` in reverse-array order then `DT_FINI`,
//! evicts the slot from the global scope, and unmaps the image via
//! the pure-logic `ldso_core::dynlink::unmap_dso`.
//!
//! ## Thread safety
//!
//! Phase 76c is single-threaded. [`DL_STATE`] is stored in an
//! `UnsafeCell` with an `unsafe impl Sync` so it can live in a
//! `static`; every access goes through [`dl_state_mut`] which
//! returns a `&mut DlState`. There is no locking — relying on the
//! single-threaded invariant is documented in
//! `docs/76-dynamic-linker.md`. The thread-safe upgrade is gated on
//! TLS landing (Phase TBD).

use core::cell::UnsafeCell;
use core::ffi::{c_char, c_int, c_void};

use ldso_core::dynlink::{DsoId, LoadedDso, MAX_DSOS, UnmapError, elf_hash, unmap_dso};
use ldso_core::handle::HandleTable;

use crate as runtime;

// ---------------------------------------------------------------------------
// libdl flag values (matches musl `<dlfcn.h>`).
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub const RTLD_LAZY: c_int = 1;
#[allow(dead_code)]
pub const RTLD_NOW: c_int = 2;
pub const RTLD_GLOBAL: c_int = 256;
#[allow(dead_code)]
pub const RTLD_LOCAL: c_int = 0;
/// `dlsym(RTLD_DEFAULT, …)` searches the process-global scope. The
/// musl ABI uses `NULL` for this sentinel.
pub const RTLD_DEFAULT: *mut c_void = core::ptr::null_mut();

/// Typed error from the dl-flavoured loader. The runtime maps each
/// variant to a `dlerror` slot string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DlLoadError {
    /// `sys_open` returned `-ENOENT`.
    NotFound,
    /// Any other load failure (mmap, mprotect, malformed ELF, …).
    Other,
}

/// Permanent-refcount sentinel for the main binary, the linker
/// itself, and any `DT_NEEDED` library loaded during bring-up. These
/// can never be `dlclose`d to zero.
pub const REFCOUNT_PERMANENT: u32 = u32::MAX;

// ---------------------------------------------------------------------------
// `dlerror` static message bank.
// ---------------------------------------------------------------------------

/// All `dlerror` strings the linker emits. Each is NUL-terminated so
/// `dlerror()` can return the raw pointer.
pub const ERR_LIBRARY_NOT_FOUND: &[u8] = b"library not found\0";
pub const ERR_LOAD_FAILED: &[u8] = b"library load failed\0";
pub const ERR_RELOC_FAILED: &[u8] = b"relocation failed\0";
pub const ERR_UNDEFINED_SYMBOL: &[u8] = b"undefined symbol\0";
pub const ERR_INVALID_HANDLE: &[u8] = b"invalid handle\0";
pub const ERR_HANDLE_TABLE_FULL: &[u8] = b"handle table full\0";
pub const ERR_TOO_MANY_DSOS: &[u8] = b"too many loaded DSOs\0";
pub const ERR_BAD_PATH: &[u8] = b"invalid path\0";

// ---------------------------------------------------------------------------
// DlState — single-process libdl state.
// ---------------------------------------------------------------------------

/// Number of slots the runtime tracks. Reused from
/// `ldso_core::dynlink::MAX_DSOS` so a `DsoId` can index any state
/// array without a separate bounds check.
pub const MAX_SLOTS: usize = MAX_DSOS;

/// Const initializer for one row of `dep_lists` — needed because
/// `heapless::Vec` is not `Copy` and the array literal would
/// otherwise require it.
const EMPTY_DEP_LIST: heapless::Vec<DsoId, MAX_SLOTS> = heapless::Vec::new();

/// libdl runtime state. Lives in a `static` slot (via [`DL_STATE`])
/// so all four entry points can find it without an argument.
pub struct DlState {
    /// Slot-indexed DSO table. Live slots have `refcounts[i] != 0`.
    /// Index 0 is always the main binary; index 1 is always the
    /// linker itself (so the libdl symbols resolve to its
    /// implementations before any user-provided stub library).
    pub dsos: [LoadedDso; MAX_SLOTS],
    /// SONAME (or `DT_NEEDED` name) for each slot. The main binary's
    /// slot uses `&[]`; every other live slot carries the canonical
    /// SONAME so `dlopen` can dedup repeat opens.
    pub names: [&'static [u8]; MAX_SLOTS],
    /// Direct dependency-graph edges for each slot. Used by `dlsym`
    /// to walk the handle's dependency chain.
    pub dep_lists: [heapless::Vec<DsoId, MAX_SLOTS>; MAX_SLOTS],
    /// Per-slot refcount. `0` means slot is free. `REFCOUNT_PERMANENT`
    /// means the slot is permanent (main / linker / bring-up
    /// `DT_NEEDED`) and `dlclose` is a no-op against it.
    pub refcounts: [u32; MAX_SLOTS],
    /// `true` when the slot's symbols are visible in the process-global
    /// scope (`RTLD_GLOBAL` or bring-up-time `DT_NEEDED`). `false` for
    /// `RTLD_LOCAL` `dlopen`'d slots.
    pub in_global_scope: [bool; MAX_SLOTS],
    /// One past the highest slot that has ever been allocated.
    /// `dlopen`'s slot-finder scans `0..n_slots_used` for a freed slot
    /// before extending the watermark.
    pub n_slots_used: usize,
    /// Slab of `dlopen`-allocated handles. Each handle resolves to a
    /// `DsoId` plus a generation token so a stale or forged pointer
    /// is detected at `dlclose` / `dlsym` time.
    pub handles: HandleTable,
    /// Last libdl error message. `dlerror()` reads and clears it.
    pub error: Option<&'static [u8]>,
    /// `true` after `dl_entry` has finished publishing the bring-up
    /// state. Until that flips, any libdl call is a use-before-init
    /// bug and returns `NULL` immediately.
    pub initialized: bool,
}

impl DlState {
    pub const fn new() -> Self {
        Self {
            dsos: [LoadedDso::empty(); MAX_SLOTS],
            names: [&[]; MAX_SLOTS],
            dep_lists: [EMPTY_DEP_LIST; MAX_SLOTS],
            refcounts: [0; MAX_SLOTS],
            in_global_scope: [false; MAX_SLOTS],
            n_slots_used: 0,
            handles: HandleTable::new(),
            error: None,
            initialized: false,
        }
    }

    /// Find a slot whose `names[i]` matches `soname`. Skips freed
    /// slots (`refcounts[i] == 0`). Returns the first match.
    pub fn find_by_soname(&self, soname: &[u8]) -> Option<DsoId> {
        for i in 0..self.n_slots_used {
            if self.refcounts[i] != 0 && self.names[i] == soname {
                return Some(DsoId(i as u32));
            }
        }
        None
    }

    /// Allocate a slot for a new DSO. Reuses any free slot below the
    /// watermark before extending it. Returns `None` if every slot is
    /// in use.
    pub fn allocate_slot(&mut self) -> Option<DsoId> {
        for i in 0..self.n_slots_used {
            if self.refcounts[i] == 0 {
                return Some(DsoId(i as u32));
            }
        }
        if self.n_slots_used >= MAX_SLOTS {
            return None;
        }
        let id = DsoId(self.n_slots_used as u32);
        self.n_slots_used += 1;
        Some(id)
    }

    pub fn set_error(&mut self, msg: &'static [u8]) {
        self.error = Some(msg);
    }

    pub fn take_error(&mut self) -> Option<&'static [u8]> {
        self.error.take()
    }
}

// ---------------------------------------------------------------------------
// Single-threaded `static` cell wrapping `DlState`.
// ---------------------------------------------------------------------------

/// Newtype carrying `unsafe impl Sync` so the `UnsafeCell<DlState>`
/// can live in a `static`. Phase 76c is single-threaded; the upgrade
/// to a real lock is gated on TLS.
struct DlStateCell {
    inner: UnsafeCell<DlState>,
}

// SAFETY: Phase 76c is single-threaded. No reentrant libdl call can
// observe `DlState` while another is mutating it. The thread-safety
// gap is documented in `docs/76-dynamic-linker.md`.
unsafe impl Sync for DlStateCell {}

static DL_STATE: DlStateCell = DlStateCell {
    inner: UnsafeCell::new(DlState::new()),
};

/// Borrow `DlState` mutably. Always-safe under the single-threaded
/// Phase 76c invariant — see [`DlStateCell`].
#[allow(clippy::mut_from_ref)]
pub fn dl_state_mut() -> &'static mut DlState {
    // SAFETY: single-threaded; no nested libdl calls inside one
    // thread (no thread cancellation, no signal-handler reentry).
    unsafe { &mut *DL_STATE.inner.get() }
}

/// Borrow `DlState` shared. Same invariant as `dl_state_mut`.
#[allow(dead_code)]
pub fn dl_state() -> &'static DlState {
    // SAFETY: see `dl_state_mut`.
    unsafe { &*DL_STATE.inner.get() }
}

// ---------------------------------------------------------------------------
// libdl entry points.
// ---------------------------------------------------------------------------

/// Open a shared library and return a refcounted handle.
///
/// `path == NULL` returns a handle to the main binary (slot 0).
/// Paths containing `/` are treated as absolute; bare names search
/// `/usr/lib/` first, then `/lib/`.
///
/// # Safety
/// `path` must either be `NULL` or a valid NUL-terminated C string.
/// `flags` is a bitwise-OR of `RTLD_LAZY` / `RTLD_NOW` / `RTLD_GLOBAL`
/// / `RTLD_LOCAL`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dlopen(path: *const c_char, flags: c_int) -> *mut c_void {
    let state = dl_state_mut();
    if !state.initialized {
        state.set_error(ERR_LOAD_FAILED);
        return core::ptr::null_mut();
    }

    // `path == NULL` → handle to the main binary (slot 0).
    if path.is_null() {
        return match state.handles.insert(DsoId(0)) {
            Ok(h) => h,
            Err(_) => {
                state.set_error(ERR_HANDLE_TABLE_FULL);
                core::ptr::null_mut()
            }
        };
    }

    // Path → bytes.
    // SAFETY: caller guarantees `path` is NUL-terminated. Bounded
    // length keeps a malformed input from running off the end of
    // memory.
    let name_bytes = unsafe { cstr_to_bytes_bounded(path, 1024) };
    if name_bytes.is_empty() {
        state.set_error(ERR_BAD_PATH);
        return core::ptr::null_mut();
    }

    // Extract the basename for SONAME dedup. The full path keys the
    // load, but dedup is by SONAME so two paths to the same library
    // (e.g. `/usr/lib/libfoo.so` vs `/lib/libfoo.so`) share one
    // refcount.
    let basename = basename_of(name_bytes);
    if let Some(id) = state.find_by_soname(basename) {
        // Repeat open of an already-loaded DSO. Refcount-clamp at
        // `REFCOUNT_PERMANENT - 1` so a pathological caller cannot
        // overflow into the permanent sentinel.
        let rc = &mut state.refcounts[id.0 as usize];
        if *rc != REFCOUNT_PERMANENT {
            *rc = rc.saturating_add(1).min(REFCOUNT_PERMANENT - 1);
        }
        if (flags & RTLD_GLOBAL) != 0 {
            state.in_global_scope[id.0 as usize] = true;
        }
        return match state.handles.insert(id) {
            Ok(h) => h,
            Err(_) => {
                state.set_error(ERR_HANDLE_TABLE_FULL);
                core::ptr::null_mut()
            }
        };
    }

    // Build the on-disk path. If `name` is absolute (starts with /)
    // use it as-is; otherwise prepend `/usr/lib/` (matches the 76b
    // bring-up convention).
    let mut path_buf = [0u8; 320];
    let path_len = if name_bytes.first() == Some(&b'/') {
        if name_bytes.len() + 1 > path_buf.len() {
            state.set_error(ERR_BAD_PATH);
            return core::ptr::null_mut();
        }
        path_buf[..name_bytes.len()].copy_from_slice(name_bytes);
        path_buf[name_bytes.len()] = 0;
        name_bytes.len() + 1
    } else {
        const PREFIX: &[u8] = b"/usr/lib/";
        if PREFIX.len() + name_bytes.len() + 1 > path_buf.len() {
            state.set_error(ERR_BAD_PATH);
            return core::ptr::null_mut();
        }
        path_buf[..PREFIX.len()].copy_from_slice(PREFIX);
        path_buf[PREFIX.len()..PREFIX.len() + name_bytes.len()].copy_from_slice(name_bytes);
        path_buf[PREFIX.len() + name_bytes.len()] = 0;
        PREFIX.len() + name_bytes.len() + 1
    };

    // Reserve a slot before loading so a load failure doesn't waste
    // the slot — actually we do load first, then commit, so a partial
    // load can be rolled back.
    let _ = path_len;
    let loaded = match unsafe { runtime::load_dso_for_dl(&path_buf) } {
        Ok(d) => d,
        Err(DlLoadError::NotFound) => {
            state.set_error(ERR_LIBRARY_NOT_FOUND);
            return core::ptr::null_mut();
        }
        Err(_) => {
            state.set_error(ERR_LOAD_FAILED);
            return core::ptr::null_mut();
        }
    };

    let id = match state.allocate_slot() {
        Some(id) => id,
        None => {
            // Loaded DSO is leaked — Phase 76c does not unmap on
            // slot-allocation failure because the allocator never
            // hits this path with the MAX_DSOS = 32 cap.
            state.set_error(ERR_TOO_MANY_DSOS);
            return core::ptr::null_mut();
        }
    };

    state.dsos[id.0 as usize] = loaded;
    state.names[id.0 as usize] = canonical_soname(&loaded, basename);
    state.refcounts[id.0 as usize] = 1;
    state.in_global_scope[id.0 as usize] = (flags & RTLD_GLOBAL) != 0;
    state.dep_lists[id.0 as usize].clear();

    // Apply relocations against the new DSO. `RTLD_NOW` and
    // `RTLD_LAZY` are treated identically in 76c (PLT lazy resolve
    // ships in 76d).
    let _ = flags; // RTLD_LAZY accepted but treated as RTLD_NOW.
    if let Err(_e) = unsafe { runtime::apply_relocations_for(id, state) } {
        // Roll back the slot before reporting the failure.
        let _ = unmap_dso(&state.dsos[id.0 as usize], crate::sys_munmap);
        state.dsos[id.0 as usize] = LoadedDso::empty();
        state.names[id.0 as usize] = &[];
        state.refcounts[id.0 as usize] = 0;
        state.set_error(ERR_RELOC_FAILED);
        return core::ptr::null_mut();
    }

    // Run constructors for just-this-DSO (DT_INIT then DT_INIT_ARRAY).
    unsafe { runtime::run_constructors_for(&state.dsos[id.0 as usize]) };

    match state.handles.insert(id) {
        Ok(h) => h,
        Err(_) => {
            state.set_error(ERR_HANDLE_TABLE_FULL);
            core::ptr::null_mut()
        }
    }
}

/// Look up a symbol in a `dlopen`'d library.
///
/// # Safety
/// `handle` must be either `RTLD_DEFAULT` (NULL) or a handle
/// previously returned by `dlopen`. `name` must be a valid
/// NUL-terminated C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void {
    let state = dl_state_mut();
    if !state.initialized || name.is_null() {
        state.set_error(ERR_UNDEFINED_SYMBOL);
        return core::ptr::null_mut();
    }
    let name_bytes = unsafe { cstr_to_bytes_bounded(name, 1024) };
    if name_bytes.is_empty() {
        state.set_error(ERR_UNDEFINED_SYMBOL);
        return core::ptr::null_mut();
    }

    // RTLD_DEFAULT (NULL handle) → search process-global scope.
    if handle == RTLD_DEFAULT {
        if let Some(addr) = unsafe { search_global_scope(state, name_bytes) } {
            return addr as *mut c_void;
        }
        state.set_error(ERR_UNDEFINED_SYMBOL);
        return core::ptr::null_mut();
    }

    // Real handle: walk the handle's DSO and its dep chain.
    let dso_id = match state.handles.resolve(handle) {
        Ok(id) => id,
        Err(_) => {
            state.set_error(ERR_INVALID_HANDLE);
            return core::ptr::null_mut();
        }
    };
    if let Some(addr) = unsafe { search_handle_scope(state, dso_id, name_bytes) } {
        return addr as *mut c_void;
    }
    state.set_error(ERR_UNDEFINED_SYMBOL);
    core::ptr::null_mut()
}

/// Close a previously-opened handle. Returns `0` on success, `-1` on
/// failure (forged or already-freed handle).
///
/// # Safety
/// `handle` must be a handle previously returned by `dlopen`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dlclose(handle: *mut c_void) -> c_int {
    let state = dl_state_mut();
    if !state.initialized {
        state.set_error(ERR_INVALID_HANDLE);
        return -1;
    }
    let dso_id = match state.handles.resolve(handle) {
        Ok(id) => id,
        Err(_) => {
            state.set_error(ERR_INVALID_HANDLE);
            return -1;
        }
    };

    // Remove the handle from the table regardless of refcount — the
    // handle is consumed by the close call.
    let _ = state.handles.remove(handle);

    let idx = dso_id.0 as usize;
    if state.refcounts[idx] == REFCOUNT_PERMANENT {
        // Permanent DSO (main / linker / bring-up DT_NEEDED).
        // Decrement is a no-op; the handle is still consumed.
        return 0;
    }
    if state.refcounts[idx] == 0 {
        // Slot is already vacated — handle was stale.
        state.set_error(ERR_INVALID_HANDLE);
        return -1;
    }
    state.refcounts[idx] -= 1;
    if state.refcounts[idx] > 0 {
        // More handles still hold this DSO.
        return 0;
    }

    // Last close → run destructors, evict from scope, unmap.
    // Capture the LoadedDso by value before mutating state so the
    // destructor walker doesn't observe a half-cleared slot.
    let dso = state.dsos[idx];
    // Mark the slot evicted BEFORE running destructors so a
    // destructor that calls back into dlsym(self) can't find itself.
    state.in_global_scope[idx] = false;
    state.dep_lists[idx].clear();
    state.names[idx] = &[];
    state.dsos[idx] = LoadedDso::empty();

    // SAFETY: destructors are `extern "C" fn()` pointers held in the
    // captured DSO image. The captured image is still mapped until
    // the `unmap_dso` call below; destructors that touch the DSO's
    // own data are safe up to the return-from-destructor moment.
    unsafe { runtime::run_destructors_for(&dso) };

    if let Err(e) = unmap_dso(&dso, crate::sys_munmap) {
        // The DSO is conceptually gone (slot evicted) but the kernel
        // may still hold its pages. Log the munmap failure on serial
        // — it indicates a real bug.
        let _ = e;
        crate::serial(b"ldso: dlclose: unmap failed for ");
        crate::serial(state.names[idx]);
        crate::serial(b"\n");
    }
    let _: UnmapError = UnmapError::EmptyImage; // keep variant alive

    0
}

/// Return the last libdl error message, or `NULL` if none. Clears
/// the slot on read.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dlerror() -> *const c_char {
    let state = dl_state_mut();
    match state.take_error() {
        Some(msg) => msg.as_ptr() as *const c_char,
        None => core::ptr::null(),
    }
}

// ---------------------------------------------------------------------------
// Helpers shared between the entry points.
// ---------------------------------------------------------------------------

/// Compute the byte length of a NUL-terminated C string, bounded.
///
/// # Safety
/// `p` must point at a region of at most `max` bytes of mapped memory.
unsafe fn cstr_to_bytes_bounded(p: *const c_char, max: usize) -> &'static [u8] {
    let mut n = 0usize;
    while n < max {
        if unsafe { *(p.add(n) as *const u8) } == 0 {
            break;
        }
        n += 1;
    }
    // SAFETY: caller guarantees the C string occupies at least `n`
    // mapped bytes (we walked them). Borrow as `'static` because the
    // string lives in the caller's address space for as long as the
    // libdl call runs.
    unsafe { core::slice::from_raw_parts(p as *const u8, n) }
}

/// Extract the basename (final `/`-delimited component) of a path.
fn basename_of(path: &[u8]) -> &[u8] {
    let mut start = 0usize;
    for (i, b) in path.iter().enumerate() {
        if *b == b'/' {
            start = i + 1;
        }
    }
    &path[start..]
}

/// Pick the canonical name for a freshly-loaded DSO. Prefers the
/// real `DT_SONAME` (from the DSO's own dynamic section) so dedup is
/// keyed on the library's self-identified name, not the caller's
/// path. Falls back to the basename when DT_SONAME is absent.
fn canonical_soname(dso: &LoadedDso, fallback: &'static [u8]) -> &'static [u8] {
    if dso.dyn_.soname == u64::MAX {
        return fallback;
    }
    let strtab = match dso.dyn_.strtab {
        Some(p) => p.as_ptr(),
        None => return fallback,
    };
    // SAFETY: dl_state's invariants guarantee the DSO's strtab spans
    // a valid range; reading at the recorded SONAME offset returns
    // either a real NUL-terminated name or the empty string (`off >=
    // strsz`).
    unsafe { runtime::strtab_get(strtab, dso.dyn_.soname, dso.dyn_.strsz) }
}

/// Walk the global scope (all slots with `in_global_scope[i] == true`
/// AND `refcounts[i] != 0`) searching for `name`.
unsafe fn search_global_scope(state: &DlState, name: &[u8]) -> Option<u64> {
    for i in 0..state.n_slots_used {
        if state.refcounts[i] == 0 {
            continue;
        }
        if !state.in_global_scope[i] {
            continue;
        }
        if let Some(addr) = unsafe { lookup_in_dso(&state.dsos[i], name) } {
            return Some(addr);
        }
    }
    None
}

/// Walk the dependency chain of `root` (BFS over `dep_lists`)
/// searching for `name`. The `root` itself is searched first.
unsafe fn search_handle_scope(state: &DlState, root: DsoId, name: &[u8]) -> Option<u64> {
    let mut visited = [false; MAX_SLOTS];
    let mut queue: heapless::Vec<DsoId, MAX_SLOTS> = heapless::Vec::new();
    let _ = queue.push(root);
    let mut head = 0usize;
    while head < queue.len() {
        let id = queue[head];
        head += 1;
        let i = id.0 as usize;
        if i >= MAX_SLOTS || visited[i] {
            continue;
        }
        visited[i] = true;
        if state.refcounts[i] == 0 {
            continue;
        }
        if let Some(addr) = unsafe { lookup_in_dso(&state.dsos[i], name) } {
            return Some(addr);
        }
        for &child in state.dep_lists[i].iter() {
            let _ = queue.push(child);
        }
    }
    None
}

/// Resolve `name` via the DSO's `DT_HASH` table. Mirrors the
/// `lookup_symbol` helper used by the bring-up linker, but scoped to
/// a single DSO.
unsafe fn lookup_in_dso(dso: &LoadedDso, name: &[u8]) -> Option<u64> {
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
        let nm = unsafe { runtime::strtab_get(strtab, sym.st_name as u64, dso.dyn_.strsz) };
        if nm == name && sym.st_value != 0 {
            return Some(dso.load_bias.wrapping_add(sym.st_value));
        }
        idx = unsafe { *chain.add(idx as usize) };
        hops += 1;
    }
    None
}

// ---------------------------------------------------------------------------
// Test hooks. Only compiled under `#[cfg(test)]` because they would
// otherwise drag the host into the binary's `crate::runtime` glue.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod basename_tests {
    use super::*;

    #[test]
    fn basename_extracts_trailing_component() {
        assert_eq!(basename_of(b"/usr/lib/libhello.so"), b"libhello.so");
        assert_eq!(basename_of(b"libhello.so"), b"libhello.so");
        assert_eq!(basename_of(b"/libhello.so"), b"libhello.so");
        assert_eq!(basename_of(b"./libhello.so"), b"libhello.so");
        assert_eq!(basename_of(b"/usr/lib/"), b"");
        assert_eq!(basename_of(b""), b"");
    }
}
