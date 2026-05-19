//! `display_client_ffi` — Phase 70 Track A0: C-ABI veneer over the
//! Phase 56 display-protocol codec + the shared-memory pixel-buffer
//! lifecycle.
//!
//! The doomgeneric platform layer (`dg_m3os.c`) used to write pixels
//! directly into the framebuffer via `sys_framebuffer_mmap`. Phase 70
//! converts DOOM into a regular `display_server` client: pixels go
//! into a client-allocated SHM region, the compositor reads from
//! that region, and per-frame updates are `DamageSurface` +
//! `CommitSurface` verbs. Since DOOM is C and the Phase 56 protocol
//! codec is Rust-only, this crate exposes the minimum verb set
//! (`dc_connect`, `dc_create_toplevel`, `dc_attach_shm_buffer`,
//! `dc_damage_and_commit`, `dc_poll_event`, `dc_disconnect`) behind
//! a stable C ABI.
//!
//! Mirrors the Phase 63a `audio_client_ffi` pattern: a tiny stable
//! `c_int` error table, one opaque `DcHandle`, and `extern "C"`
//! verb functions. The wire protocol is single-sourced in
//! `kernel_core::display::protocol`; this crate is a translation
//! layer, never a parallel codec.

#![cfg_attr(not(test), no_std)]

#[cfg(all(not(test), target_env = "musl"))]
mod staticlib_runtime;

extern crate alloc;

use core::ffi::c_int;

#[cfg(not(test))]
use alloc::boxed::Box;
use kernel_core::display::protocol::{
    BufferId, ClientMessage, PROTOCOL_VERSION, ProtocolError, Rect, ServerMessage, SurfaceId,
    SurfaceRole,
};

// ---------------------------------------------------------------------------
// Stable error-code table — mirrored byte-for-byte in
// include/display_client.h. build.rs asserts the constants are equal.
// ---------------------------------------------------------------------------

pub const DC_OK: c_int = 0;
pub const DC_ERR_CONNECT: c_int = -1;
pub const DC_ERR_ENCODE: c_int = -2;
pub const DC_ERR_IPC: c_int = -3;
pub const DC_ERR_INVALID_ARG: c_int = -4;
pub const DC_ERR_NULL_HANDLE: c_int = -5;
pub const DC_ERR_PROTOCOL: c_int = -6;

// ---------------------------------------------------------------------------
// Event tag values — mirrored in display_client.h DC_EVENT_*.
// ---------------------------------------------------------------------------

pub const DC_EVENT_NONE: u32 = 0;
pub const DC_EVENT_KEY: u32 = 1;
pub const DC_EVENT_FOCUS_IN: u32 = 2;
pub const DC_EVENT_FOCUS_OUT: u32 = 3;
pub const DC_EVENT_SURFACE_RESIZED: u32 = 4;
pub const DC_EVENT_BUFFER_RELEASED: u32 = 5;
pub const DC_EVENT_DISCONNECT: u32 = 6;

// ---------------------------------------------------------------------------
// KeyEventKind discriminants — mirrored in display_client.h DC_KEY_KIND_*.
// Values match `kernel_core::input::events::KeyEventKind` enum tags.
// ---------------------------------------------------------------------------

pub const DC_KEY_KIND_DOWN: u32 = 0;
pub const DC_KEY_KIND_UP: u32 = 1;
pub const DC_KEY_KIND_REPEAT: u32 = 2;

// ---------------------------------------------------------------------------
// IPC label constants. These mirror `display_server::client::LABEL_*`
// and must stay in sync with that file. They are not re-exported from
// `display_server` because `display_server` is a binary crate, not a
// library.
// ---------------------------------------------------------------------------

/// `ClientMessage` request opcode (matches `display_server::client::LABEL_VERB`).
#[cfg_attr(test, allow(dead_code))]
const LABEL_VERB: u64 = 1;
/// Async server-event pull verb (matches
/// `display_server::client::LABEL_CLIENT_EVENT_PULL`).
#[cfg_attr(test, allow(dead_code))]
const LABEL_CLIENT_EVENT_PULL: u64 = 3;
/// Reply label when no event is ready (matches
/// `display_server::client::LABEL_CLIENT_EVENT_NONE`).
#[cfg_attr(test, allow(dead_code))]
const LABEL_CLIENT_EVENT_NONE: u64 = 4;

/// Stack-sized encode buffer for any single `ClientMessage`. The
/// widest body — `SetSurfaceRole(Layer{..})` plus the frame header —
/// is well under 64 bytes; `AttachSharedBuffer` is 24 bytes incl.
/// header.
const VERB_ENCODE_BUF_LEN: usize = 64;

/// Server-side outbound frames carrying a `KeyEvent` body land at
/// ~32 bytes after the protocol header. 256 bytes leaves comfortable
/// room for any other server-pushed `ServerMessage` variant.
#[cfg_attr(test, allow(dead_code))]
const EVENT_DECODE_BUF_LEN: usize = 256;

/// Backoff between `"display"` registry lookups (5 ms). Mirrors term's
/// connect path.
#[cfg_attr(test, allow(dead_code))]
const LOOKUP_BACKOFF_NS: u32 = 5_000_000;
/// Maximum lookup attempts before [`dc_connect`] gives up. 10 s total
/// when paired with `LOOKUP_BACKOFF_NS`.
#[cfg_attr(test, allow(dead_code))]
const LOOKUP_MAX_ATTEMPTS: u32 = 2000;

// ---------------------------------------------------------------------------
// Opaque handle exposed to C.
// ---------------------------------------------------------------------------

/// Bundle of per-process state: the resolved `display_server` IPC
/// handle and the next `SurfaceId` to allocate. `DcHandle` is opaque
/// to C; callers always interact via pointer.
pub struct DcHandle {
    #[cfg_attr(test, allow(dead_code))]
    server_handle: u32,
    /// Monotonic surface-id counter. DOOM only creates one Toplevel
    /// surface per process so this stays at 2 after the first
    /// `dc_create_toplevel`; kept extensible for a future tile-window
    /// experiment.
    #[cfg_attr(test, allow(dead_code))]
    next_surface_id: u32,
    /// Phase 70 — surface ids this handle has allocated through
    /// [`dc_create_toplevel`]. Recorded so [`dc_disconnect`] can send
    /// a `DestroySurface` for each one before saying Goodbye, freeing
    /// the compositor-side `SurfaceRegistry` entry without disturbing
    /// any other client's surfaces. Bounded at 4 — DOOM uses one
    /// surface, the slack covers a future multi-surface experiment
    /// without an unbounded allocation per handle.
    #[cfg_attr(test, allow(dead_code))]
    owned_surfaces: [Option<u32>; 4],
}

// ---------------------------------------------------------------------------
// C-callable event tag union. Memory layout is `#[repr(C)]` so the
// hand-written header sees the same offsets the build.rs drift check
// validates.
// ---------------------------------------------------------------------------

/// Tagged union of server-pushed events. Mirrors the same-named C
/// struct in `include/display_client.h`. Inactive union members are
/// zero-initialised by the producer; consumers must branch on `tag`
/// before reading any payload field.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DcEvent {
    pub tag: u32,
    pub payload: DcEventPayload,
}

/// Inline payload union — see [`DcEvent`].
#[repr(C)]
#[derive(Clone, Copy)]
pub union DcEventPayload {
    pub key: DcKeyPayload,
    pub focus_in: DcFocusPayload,
    pub focus_out: DcFocusPayload,
    pub surface_resized: DcResizePayload,
    pub buffer_released: DcBufferReleasedPayload,
    pub disconnect: DcDisconnectPayload,
    /// Used for `DC_EVENT_NONE` to keep the union initialised.
    pub none: [u32; 6],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct DcKeyPayload {
    pub timestamp_ms: u64,
    pub keycode: u32,
    pub symbol: u32,
    pub modifiers: u16,
    pub kind: u8,
    pub modifier_side: u8,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct DcFocusPayload {
    pub surface_id: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct DcResizePayload {
    pub surface_id: u32,
    pub width: u32,
    pub height: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct DcBufferReleasedPayload {
    pub surface_id: u32,
    pub buffer_id: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct DcDisconnectPayload {
    pub reason: u32,
}

impl DcEvent {
    /// All-zero `DC_EVENT_NONE` sentinel.
    pub fn none() -> Self {
        Self {
            tag: DC_EVENT_NONE,
            payload: DcEventPayload { none: [0; 6] },
        }
    }
}

// ---------------------------------------------------------------------------
// Pure-logic message builders — host-testable. Each emits a single
// `ClientMessage` into a caller-provided buffer and returns the
// encoded length so the caller can hand the slice to `ipc_call_buf`.
// ---------------------------------------------------------------------------

/// Encode `ClientMessage::Hello { protocol_version, capabilities: 0 }`.
pub fn build_hello(buf: &mut [u8]) -> Result<usize, ProtocolError> {
    let msg = ClientMessage::Hello {
        protocol_version: PROTOCOL_VERSION,
        capabilities: 0,
    };
    msg.encode(buf)
}

/// Encode `ClientMessage::CreateSurface { surface_id }`.
pub fn build_create_surface(buf: &mut [u8], surface_id: u32) -> Result<usize, ProtocolError> {
    let msg = ClientMessage::CreateSurface {
        surface_id: SurfaceId(surface_id),
    };
    msg.encode(buf)
}

/// Encode `ClientMessage::SetSurfaceRole { surface_id, role: Toplevel }`.
pub fn build_set_role_toplevel(buf: &mut [u8], surface_id: u32) -> Result<usize, ProtocolError> {
    let msg = ClientMessage::SetSurfaceRole {
        surface_id: SurfaceId(surface_id),
        role: SurfaceRole::Toplevel,
    };
    msg.encode(buf)
}

/// Encode `ClientMessage::AttachSharedBuffer`.
pub fn build_attach_shared_buffer(
    buf: &mut [u8],
    surface_id: u32,
    buffer_id: u32,
    shm_id: u32,
    width: u32,
    height: u32,
) -> Result<usize, ProtocolError> {
    let msg = ClientMessage::AttachSharedBuffer {
        surface_id: SurfaceId(surface_id),
        buffer_id: BufferId(buffer_id),
        shm_id,
        width,
        height,
    };
    msg.encode(buf)
}

/// Encode `ClientMessage::DamageSurface { rect }`.
pub fn build_damage(
    buf: &mut [u8],
    surface_id: u32,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
) -> Result<usize, ProtocolError> {
    let msg = ClientMessage::DamageSurface {
        surface_id: SurfaceId(surface_id),
        rect: Rect { x, y, w, h },
    };
    msg.encode(buf)
}

/// Encode `ClientMessage::CommitSurface { surface_id }`.
pub fn build_commit(buf: &mut [u8], surface_id: u32) -> Result<usize, ProtocolError> {
    let msg = ClientMessage::CommitSurface {
        surface_id: SurfaceId(surface_id),
    };
    msg.encode(buf)
}

/// Encode `ClientMessage::Goodbye`.
pub fn build_goodbye(buf: &mut [u8]) -> Result<usize, ProtocolError> {
    ClientMessage::Goodbye.encode(buf)
}

/// Encode `ClientMessage::DestroySurface { surface_id }`. Phase 70 —
/// emitted from [`dc_disconnect`] before [`build_goodbye`] so the
/// compositor cleanly removes only this client's surface instead of
/// relying on the per-client tracking the Phase 56 server does not
/// yet implement.
pub fn build_destroy_surface(buf: &mut [u8], surface_id: u32) -> Result<usize, ProtocolError> {
    let msg = ClientMessage::DestroySurface {
        surface_id: SurfaceId(surface_id),
    };
    msg.encode(buf)
}

/// Translate a decoded [`ServerMessage`] into a [`DcEvent`]. Variants
/// the FFI does not surface (`Welcome`, `SurfaceConfigured`,
/// `SurfaceDestroyed`, `Pointer`) collapse to `DC_EVENT_NONE`. Pure
/// function so the host tests exhaustively cover every branch.
pub fn server_message_to_dc_event(msg: ServerMessage) -> DcEvent {
    match msg {
        ServerMessage::Key(ev) => DcEvent {
            tag: DC_EVENT_KEY,
            payload: DcEventPayload {
                key: DcKeyPayload {
                    timestamp_ms: ev.timestamp_ms,
                    keycode: ev.keycode,
                    symbol: ev.symbol,
                    modifiers: ev.modifiers.bits(),
                    kind: ev.kind as u8,
                    modifier_side: ev.modifier_side as u8,
                },
            },
        },
        ServerMessage::FocusIn { surface_id } => DcEvent {
            tag: DC_EVENT_FOCUS_IN,
            payload: DcEventPayload {
                focus_in: DcFocusPayload {
                    surface_id: surface_id.0,
                },
            },
        },
        ServerMessage::FocusOut { surface_id } => DcEvent {
            tag: DC_EVENT_FOCUS_OUT,
            payload: DcEventPayload {
                focus_out: DcFocusPayload {
                    surface_id: surface_id.0,
                },
            },
        },
        ServerMessage::SurfaceResized {
            surface_id,
            width,
            height,
        } => DcEvent {
            tag: DC_EVENT_SURFACE_RESIZED,
            payload: DcEventPayload {
                surface_resized: DcResizePayload {
                    surface_id: surface_id.0,
                    width,
                    height,
                },
            },
        },
        ServerMessage::BufferReleased {
            surface_id,
            buffer_id,
        } => DcEvent {
            tag: DC_EVENT_BUFFER_RELEASED,
            payload: DcEventPayload {
                buffer_released: DcBufferReleasedPayload {
                    surface_id: surface_id.0,
                    buffer_id: buffer_id.0,
                },
            },
        },
        ServerMessage::Disconnect { reason } => DcEvent {
            tag: DC_EVENT_DISCONNECT,
            payload: DcEventPayload {
                disconnect: DcDisconnectPayload {
                    reason: reason as u32,
                },
            },
        },
        // Welcome, SurfaceConfigured, SurfaceDestroyed, Pointer:
        // not part of DOOM's contract — drop silently.
        _ => DcEvent::none(),
    }
}

// ---------------------------------------------------------------------------
// IPC plumbing — production-only; host tests cover the pure builders
// and the event-translation helper above without invoking IPC.
// ---------------------------------------------------------------------------

#[cfg(not(test))]
fn lookup_display_with_backoff() -> Option<u32> {
    for attempt in 0..LOOKUP_MAX_ATTEMPTS {
        let raw = syscall_lib::ipc_lookup_service("display");
        if raw != u64::MAX {
            return Some(raw as u32);
        }
        if attempt + 1 == LOOKUP_MAX_ATTEMPTS {
            return None;
        }
        let _ = syscall_lib::nanosleep_for(0, LOOKUP_BACKOFF_NS);
    }
    None
}

#[cfg(not(test))]
fn send_encoded(handle: u32, bytes: &[u8]) -> bool {
    syscall_lib::ipc_call_buf(handle, LABEL_VERB, 0, bytes) != u64::MAX
}

// ---------------------------------------------------------------------------
// C-ABI verbs
// ---------------------------------------------------------------------------

/// Resolve `display_server`, handshake with `Hello`, and return an
/// owning `DcHandle`. Returns `DC_OK` on success and writes the handle
/// pointer to `*out`; on failure returns a negative `DC_ERR_*` and
/// leaves `*out` untouched (or sets it to NULL if the caller has not
/// pre-initialised it).
///
/// # Safety
///
/// `out` must point to a writable `*mut DcHandle` storage slot.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dc_connect(out: *mut *mut DcHandle) -> c_int {
    if out.is_null() {
        return DC_ERR_INVALID_ARG;
    }
    let server_handle = match lookup_display_with_backoff() {
        Some(h) => h,
        None => return DC_ERR_CONNECT,
    };
    let mut buf = [0u8; VERB_ENCODE_BUF_LEN];
    let n = match build_hello(&mut buf) {
        Ok(n) => n,
        Err(_) => return DC_ERR_ENCODE,
    };
    if !send_encoded(server_handle, &buf[..n]) {
        return DC_ERR_CONNECT;
    }
    // Phase 70 Track F — derive the per-process surface-id seed from
    // PID so two concurrent DOOMs do not both claim `SurfaceId(1)` and
    // collide with each other (and with `term`'s long-lived
    // `SurfaceId(1)` toplevel). The compositor rejects `CreateSurface`
    // with `DuplicateSurface` on collision but the dispatcher swallows
    // that error rather than disconnecting — the client would
    // otherwise believe its create succeeded and drive frames into
    // the wrong surface.
    //
    // PID values on m3OS are small unsigned integers; +0x4000 keeps
    // them clear of `term`'s `SurfaceId(1)` and reserves the low 16k
    // for future statically-allocated surfaces.
    let pid = syscall_lib::getpid();
    let seed = if pid > 0 {
        0x4000u32.wrapping_add(pid as u32)
    } else {
        // PID lookup failed — fall back to a fixed mid-range id. Two
        // such fallbacks would collide; the assumption is that a
        // userspace process always has a valid PID.
        0x4001
    };
    let handle = Box::new(DcHandle {
        server_handle,
        next_surface_id: seed,
        owned_surfaces: [None; 4],
    });
    // SAFETY: `out` validity is the caller's contract.
    unsafe {
        core::ptr::write(out, Box::into_raw(handle));
    }
    DC_OK
}

/// Send `CreateSurface { surface_id }` + `SetSurfaceRole { surface_id,
/// Toplevel }`. Allocates the surface id from the handle's monotonic
/// counter.
///
/// # Safety
///
/// `h` must be a pointer previously returned by [`dc_connect`] and not
/// yet freed; `out_surface_id` must point to a writable `u32` slot.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dc_create_toplevel(h: *mut DcHandle, out_surface_id: *mut u32) -> c_int {
    if h.is_null() {
        return DC_ERR_NULL_HANDLE;
    }
    if out_surface_id.is_null() {
        return DC_ERR_INVALID_ARG;
    }
    // SAFETY: caller upholds validity.
    let handle = unsafe { &mut *h };
    let surface_id = handle.next_surface_id;
    handle.next_surface_id = handle.next_surface_id.wrapping_add(1);

    let mut buf = [0u8; VERB_ENCODE_BUF_LEN];
    let n = match build_create_surface(&mut buf, surface_id) {
        Ok(n) => n,
        Err(_) => return DC_ERR_ENCODE,
    };
    if !send_encoded(handle.server_handle, &buf[..n]) {
        return DC_ERR_IPC;
    }
    let n = match build_set_role_toplevel(&mut buf, surface_id) {
        Ok(n) => n,
        Err(_) => return DC_ERR_ENCODE,
    };
    if !send_encoded(handle.server_handle, &buf[..n]) {
        return DC_ERR_IPC;
    }
    // Record the surface id so `dc_disconnect` can clean it up on the
    // compositor side. Silently drop on overflow — a handle that
    // claims more than four surfaces is outside DOOM's contract and
    // should not gain implicit cleanup either way.
    for slot in handle.owned_surfaces.iter_mut() {
        if slot.is_none() {
            *slot = Some(surface_id);
            break;
        }
    }
    // SAFETY: `out_surface_id` validity is the caller's contract.
    unsafe {
        core::ptr::write(out_surface_id, surface_id);
    }
    DC_OK
}

/// Send `AttachSharedBuffer { surface_id, buffer_id, shm_id, width,
/// height }`. The caller must have already allocated `shm_id` via
/// `sys_shm_create` and mapped it into its own address space (the
/// mapping is the SHM region the compositor will read pixels from).
///
/// # Safety
///
/// `h` must be a valid `DcHandle` pointer.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dc_attach_shm_buffer(
    h: *mut DcHandle,
    surface_id: u32,
    buffer_id: u32,
    shm_id: u32,
    width: u32,
    height: u32,
) -> c_int {
    if h.is_null() {
        return DC_ERR_NULL_HANDLE;
    }
    if width == 0 || height == 0 {
        return DC_ERR_INVALID_ARG;
    }
    // SAFETY: caller upholds validity.
    let handle = unsafe { &*h };
    let mut buf = [0u8; VERB_ENCODE_BUF_LEN];
    let n = match build_attach_shared_buffer(&mut buf, surface_id, buffer_id, shm_id, width, height)
    {
        Ok(n) => n,
        Err(_) => return DC_ERR_ENCODE,
    };
    if !send_encoded(handle.server_handle, &buf[..n]) {
        return DC_ERR_IPC;
    }
    DC_OK
}

/// Send `DamageSurface { rect }` then `CommitSurface { surface_id }`.
///
/// # Safety
///
/// `h` must be a valid `DcHandle` pointer.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dc_damage_and_commit(
    h: *mut DcHandle,
    surface_id: u32,
    x: i32,
    y: i32,
    w: u32,
    h_px: u32,
) -> c_int {
    if h.is_null() {
        return DC_ERR_NULL_HANDLE;
    }
    if w == 0 || h_px == 0 {
        return DC_ERR_INVALID_ARG;
    }
    // SAFETY: caller upholds validity.
    let handle = unsafe { &*h };
    let mut buf = [0u8; VERB_ENCODE_BUF_LEN];
    let n = match build_damage(&mut buf, surface_id, x, y, w, h_px) {
        Ok(n) => n,
        Err(_) => return DC_ERR_ENCODE,
    };
    if !send_encoded(handle.server_handle, &buf[..n]) {
        return DC_ERR_IPC;
    }
    let n = match build_commit(&mut buf, surface_id) {
        Ok(n) => n,
        Err(_) => return DC_ERR_ENCODE,
    };
    if !send_encoded(handle.server_handle, &buf[..n]) {
        return DC_ERR_IPC;
    }
    DC_OK
}

/// Non-blocking event drain. Issues `ipc_call(handle,
/// LABEL_CLIENT_EVENT_PULL, 0)`; if the reply is `LABEL_CLIENT_EVENT_PULL`
/// the staged bulk holds an encoded `ServerMessage`. Returns `1` if
/// `*out` was populated, `0` if the server queue was empty, or a
/// negative `DC_ERR_*` code on transport / decode failure.
///
/// # Safety
///
/// `h` and `out` must be valid pointers.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dc_poll_event(h: *mut DcHandle, out: *mut DcEvent) -> c_int {
    if h.is_null() {
        return DC_ERR_NULL_HANDLE;
    }
    if out.is_null() {
        return DC_ERR_INVALID_ARG;
    }
    // SAFETY: caller upholds validity.
    let handle = unsafe { &*h };
    let label = syscall_lib::ipc_call(handle.server_handle, LABEL_CLIENT_EVENT_PULL, 0);
    if label == LABEL_CLIENT_EVENT_NONE {
        // Drain any empty staged bulk to keep the per-task slot clean.
        let mut sink = [0u8; 1];
        let _ = syscall_lib::ipc_take_pending_bulk(&mut sink);
        // SAFETY: out validity is the caller's contract.
        unsafe {
            core::ptr::write(out, DcEvent::none());
        }
        return 0;
    }
    if label != LABEL_CLIENT_EVENT_PULL {
        // Any other label is a transport error or unexpected server
        // reply — keep the slot clean and surface IO failure.
        let mut sink = [0u8; 1];
        let _ = syscall_lib::ipc_take_pending_bulk(&mut sink);
        return DC_ERR_IPC;
    }
    let mut buf = [0u8; EVENT_DECODE_BUF_LEN];
    let n = syscall_lib::ipc_take_pending_bulk(&mut buf);
    if n == 0 || n == u64::MAX {
        // PULL acknowledged but no bulk staged — treat as empty.
        unsafe {
            core::ptr::write(out, DcEvent::none());
        }
        return 0;
    }
    let len = n as usize;
    if len > buf.len() {
        return DC_ERR_PROTOCOL;
    }
    match ServerMessage::decode(&buf[..len]) {
        Ok((msg, _)) => {
            let ev = server_message_to_dc_event(msg);
            // SAFETY: out validity is the caller's contract.
            unsafe {
                core::ptr::write(out, ev);
            }
            // DC_EVENT_NONE means the variant was non-actionable
            // (Welcome / SurfaceConfigured / ...). Surface as
            // "no event" so the caller's drain loop terminates the
            // same way an empty server queue would.
            if ev.tag == DC_EVENT_NONE { 0 } else { 1 }
        }
        Err(_) => DC_ERR_PROTOCOL,
    }
}

/// Send `Goodbye`, drop the handle, and free the box. After return
/// the pointer must not be reused. Passing NULL is a safe no-op.
///
/// # Safety
///
/// `h` must be a pointer previously returned by [`dc_connect`] and not
/// yet freed. Calling twice on the same pointer is undefined.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dc_disconnect(h: *mut DcHandle) {
    if h.is_null() {
        return;
    }
    // SAFETY: caller upholds validity.
    let boxed = unsafe { Box::from_raw(h) };
    let mut buf = [0u8; VERB_ENCODE_BUF_LEN];
    // Phase 70 — destroy each surface we own so the compositor's
    // `SurfaceRegistry` reclaims the entry. Failures here are
    // best-effort: the process is exiting either way, and a stuck
    // entry is cleared on the next `display_server` restart.
    for owned in boxed.owned_surfaces.iter().flatten() {
        if let Ok(n) = build_destroy_surface(&mut buf, *owned) {
            let _ = syscall_lib::ipc_call_buf(boxed.server_handle, LABEL_VERB, 0, &buf[..n]);
        }
    }
    if let Ok(n) = build_goodbye(&mut buf) {
        let _ = syscall_lib::ipc_call_buf(boxed.server_handle, LABEL_VERB, 0, &buf[..n]);
    }
    drop(boxed);
}

// ---------------------------------------------------------------------------
// Host tests — round-trip every emitted ClientMessage against the
// kernel-core codec and exercise the ServerMessage → DcEvent
// translation table.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use kernel_core::display::protocol::DisconnectReason;
    use kernel_core::input::events::{
        KeyEvent, KeyEventKind, MOD_CTRL, ModifierSide, ModifierState,
    };

    fn round_trip<F>(encode: F) -> ClientMessage
    where
        F: FnOnce(&mut [u8]) -> Result<usize, ProtocolError>,
    {
        let mut buf = [0u8; VERB_ENCODE_BUF_LEN];
        let n = encode(&mut buf).expect("encode");
        let (msg, consumed) = ClientMessage::decode(&buf[..n]).expect("decode");
        assert_eq!(consumed, n, "consumed != encoded");
        msg
    }

    #[test]
    fn hello_round_trip() {
        let msg = round_trip(build_hello);
        match msg {
            ClientMessage::Hello {
                protocol_version,
                capabilities,
            } => {
                assert_eq!(protocol_version, PROTOCOL_VERSION);
                assert_eq!(capabilities, 0);
            }
            _ => panic!("not Hello: {:?}", msg),
        }
    }

    #[test]
    fn create_surface_round_trip() {
        let msg = round_trip(|b| build_create_surface(b, 42));
        match msg {
            ClientMessage::CreateSurface { surface_id } => assert_eq!(surface_id, SurfaceId(42)),
            _ => panic!("not CreateSurface: {:?}", msg),
        }
    }

    #[test]
    fn set_role_toplevel_round_trip() {
        let msg = round_trip(|b| build_set_role_toplevel(b, 7));
        match msg {
            ClientMessage::SetSurfaceRole { surface_id, role } => {
                assert_eq!(surface_id, SurfaceId(7));
                assert!(matches!(role, SurfaceRole::Toplevel));
            }
            _ => panic!("not SetSurfaceRole: {:?}", msg),
        }
    }

    #[test]
    fn attach_shared_buffer_round_trip() {
        let msg = round_trip(|b| build_attach_shared_buffer(b, 1, 2, 9999, 320, 200));
        match msg {
            ClientMessage::AttachSharedBuffer {
                surface_id,
                buffer_id,
                shm_id,
                width,
                height,
            } => {
                assert_eq!(surface_id, SurfaceId(1));
                assert_eq!(buffer_id, BufferId(2));
                assert_eq!(shm_id, 9999);
                assert_eq!(width, 320);
                assert_eq!(height, 200);
            }
            _ => panic!("not AttachSharedBuffer: {:?}", msg),
        }
    }

    #[test]
    fn damage_round_trip() {
        let msg = round_trip(|b| build_damage(b, 3, 0, 0, 320, 200));
        match msg {
            ClientMessage::DamageSurface { surface_id, rect } => {
                assert_eq!(surface_id, SurfaceId(3));
                assert_eq!(rect.x, 0);
                assert_eq!(rect.y, 0);
                assert_eq!(rect.w, 320);
                assert_eq!(rect.h, 200);
            }
            _ => panic!("not DamageSurface: {:?}", msg),
        }
    }

    #[test]
    fn commit_round_trip() {
        let msg = round_trip(|b| build_commit(b, 4));
        match msg {
            ClientMessage::CommitSurface { surface_id } => assert_eq!(surface_id, SurfaceId(4)),
            _ => panic!("not CommitSurface: {:?}", msg),
        }
    }

    #[test]
    fn goodbye_round_trip() {
        let msg = round_trip(build_goodbye);
        assert!(matches!(msg, ClientMessage::Goodbye));
    }

    #[test]
    fn destroy_surface_round_trip() {
        let msg = round_trip(|b| build_destroy_surface(b, 0x4001));
        match msg {
            ClientMessage::DestroySurface { surface_id } => {
                assert_eq!(surface_id, SurfaceId(0x4001));
            }
            _ => panic!("not DestroySurface: {:?}", msg),
        }
    }

    #[test]
    fn server_key_translates() {
        let ev = KeyEvent {
            timestamp_ms: 1234,
            keycode: 0xAB,
            symbol: b'x' as u32,
            modifiers: ModifierState(MOD_CTRL),
            kind: KeyEventKind::Down,
            modifier_side: ModifierSide::Either,
        };
        let dc = server_message_to_dc_event(ServerMessage::Key(ev));
        assert_eq!(dc.tag, DC_EVENT_KEY);
        let key = unsafe { dc.payload.key };
        assert_eq!(key.timestamp_ms, 1234);
        assert_eq!(key.keycode, 0xAB);
        assert_eq!(key.symbol, b'x' as u32);
        assert_eq!(key.modifiers, MOD_CTRL);
        assert_eq!(key.kind, DC_KEY_KIND_DOWN as u8);
        assert_eq!(key.modifier_side, ModifierSide::Either as u8);
    }

    #[test]
    fn server_focus_in_translates() {
        let dc = server_message_to_dc_event(ServerMessage::FocusIn {
            surface_id: SurfaceId(11),
        });
        assert_eq!(dc.tag, DC_EVENT_FOCUS_IN);
        let p = unsafe { dc.payload.focus_in };
        assert_eq!(p.surface_id, 11);
    }

    #[test]
    fn server_focus_out_translates() {
        let dc = server_message_to_dc_event(ServerMessage::FocusOut {
            surface_id: SurfaceId(12),
        });
        assert_eq!(dc.tag, DC_EVENT_FOCUS_OUT);
        let p = unsafe { dc.payload.focus_out };
        assert_eq!(p.surface_id, 12);
    }

    #[test]
    fn server_surface_resized_translates() {
        let dc = server_message_to_dc_event(ServerMessage::SurfaceResized {
            surface_id: SurfaceId(13),
            width: 1280,
            height: 800,
        });
        assert_eq!(dc.tag, DC_EVENT_SURFACE_RESIZED);
        let p = unsafe { dc.payload.surface_resized };
        assert_eq!(p.surface_id, 13);
        assert_eq!(p.width, 1280);
        assert_eq!(p.height, 800);
    }

    #[test]
    fn server_buffer_released_translates() {
        let dc = server_message_to_dc_event(ServerMessage::BufferReleased {
            surface_id: SurfaceId(14),
            buffer_id: BufferId(7),
        });
        assert_eq!(dc.tag, DC_EVENT_BUFFER_RELEASED);
        let p = unsafe { dc.payload.buffer_released };
        assert_eq!(p.surface_id, 14);
        assert_eq!(p.buffer_id, 7);
    }

    #[test]
    fn server_disconnect_translates() {
        let dc = server_message_to_dc_event(ServerMessage::Disconnect {
            reason: DisconnectReason::ServerShutdown,
        });
        assert_eq!(dc.tag, DC_EVENT_DISCONNECT);
        let p = unsafe { dc.payload.disconnect };
        assert_eq!(p.reason, DisconnectReason::ServerShutdown as u32);
    }

    #[test]
    fn server_welcome_collapses_to_none() {
        let dc = server_message_to_dc_event(ServerMessage::Welcome {
            protocol_version: PROTOCOL_VERSION,
            capabilities: 0,
        });
        assert_eq!(dc.tag, DC_EVENT_NONE);
    }

    #[test]
    fn key_kind_tag_layout_matches_header() {
        // Each `KeyEventKind` discriminant must equal the matching
        // `DC_KEY_KIND_*` value the header exposes.
        assert_eq!(KeyEventKind::Down as u8, DC_KEY_KIND_DOWN as u8);
        assert_eq!(KeyEventKind::Up as u8, DC_KEY_KIND_UP as u8);
        assert_eq!(KeyEventKind::Repeat as u8, DC_KEY_KIND_REPEAT as u8);
    }
}
