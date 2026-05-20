//! Phase 56 Track C.3 — userspace surface shim.
//!
//! Thin wiring layer between the protocol verbs that arrive on the IPC
//! endpoint (Track C.5) and the pure-logic [`SurfaceStateMachine`] that
//! lives in `kernel-core::display::surface`. Per the engineering-discipline
//! rule "no state logic in the userspace shim", this module is intentionally
//! mechanical: it owns a map of `SurfaceId → SurfaceStateMachine` plus the
//! committed pixel buffers, and forwards `ClientMessage` verbs onto the
//! state machine, collecting the resulting [`SurfaceEffect`]s for the
//! composer / event-emit step.
//!
//! # Buffer storage
//!
//! Each surface keeps an optional `committed_buffer` slot — the bytes that
//! the client most recently committed via `AttachBuffer` + `CommitSurface`.
//! The composer (C.4) reads these bytes during `compose_frame`. When the
//! state machine emits `ReleaseBuffer`, the slot is cleared and a
//! `BufferReleased` is queued for the protocol-out path to deliver.

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use kernel_core::display::compose::ComposeLayer;
use kernel_core::display::cursor::ClientCursor;
use kernel_core::display::layer::{
    LayerConflictTracker, LayerError, compute_layer_geometry, derive_exclusive_rect,
};
use kernel_core::display::pixel_chunk::{
    AddChunkOutcome, ChunkAccumulator, ChunkError, PixelChunkHeader,
};
use kernel_core::display::protocol::{
    BufferId, ClientMessage, CursorConfig, Layer, Rect, ServerMessage, SurfaceId, SurfaceRole,
};

/// High-water mark for the pending-bulk queue. A client that ships
/// `LABEL_PIXELS` without a matching `AttachBuffer` more than this many
/// times in flight is exceeding the documented Phase 56 resource bound;
/// the dispatcher closes the connection on overflow instead of growing
/// the compositor's memory unboundedly. Recorded in the H.1 learning doc
/// alongside the per-client surface and outbound-event-queue caps.
pub const MAX_PENDING_BULK: usize = 4;

/// High-water mark for in-flight chunked-buffer accumulators. Each
/// open accumulator can hold up to
/// `kernel_core::display::pixel_chunk::MAX_TOTAL_BYTES` (4 MiB) of
/// pending pixel data, so the per-client cap on simultaneously open
/// chunked uploads is the primary defence against memory exhaustion
/// from a misbehaving client. Sized to comfortably exceed the typical
/// "double-buffered terminal" pattern (one buffer being attached, one
/// being uploaded) without any practical legitimate need to go higher.
pub const MAX_PENDING_CHUNK_BUFFERS: usize = 4;
use kernel_core::display::surface::{
    SurfaceEffect, SurfaceError, SurfaceEvent, SurfaceStateMachine,
};

/// Storage backing for a committed buffer's pixel data.
///
/// Phase 56's chunked-pixel path produced [`Vec<u8>`]-owned buffers
/// reassembled from IPC bulk fragments. The Phase 57d follow-up adds
/// [`BufferStorage::Shared`], where pixel bytes live in a shared-memory
/// region the client allocated via `sys_shm_create` and the compositor
/// mapped writable via `sys_shm_map`. The compose path treats both
/// uniformly through [`BufferStorage::as_slice`].
///
/// `Owned` cleans up automatically on drop. `Shared` carries the
/// owning user virtual address + length so [`Drop`] can call
/// `sys_shm_unmap` to release the mapping (and decrement the registry
/// refcount) when the surface is destroyed or re-attached.
///
/// Note: the kernel currently maps SHM pages writable for every mapper
/// — the compositor does not enforce read-only on its side. Phase 57d
/// callers do not write through the compositor's mapping, but the
/// [`BufferStorage::as_slice`] API is `unsafe` because the underlying
/// bytes are concurrently writable by the originating client.
#[derive(Debug)]
pub enum BufferStorage {
    Owned(Vec<u8>),
    Shared {
        /// User virtual address returned by `sys_shm_map`.
        user_va: u64,
        /// Byte length of the visible pixel region: exactly
        /// `width * height * 4`. The kernel's underlying mapping is
        /// page-aligned and may extend past this length, but only
        /// the first `len` bytes are valid pixel data.
        len: usize,
    },
}

impl Clone for BufferStorage {
    fn clone(&self) -> Self {
        match self {
            BufferStorage::Owned(v) => BufferStorage::Owned(v.clone()),
            // Shared mappings are not deep-cloneable: the kernel
            // refcount is decremented on drop, and we cannot bump it
            // again from userspace without a syscall round-trip. The
            // composer takes `&CommittedBuffer` borrows so a clone is
            // never structurally required; if it ever is, fall back
            // to materialising an owned copy of the visible bytes.
            //
            // SAFETY: copy directly through a raw pointer rather than
            // synthesising an `&[u8]` so the producer's concurrent
            // writes never alias a Rust reference, even briefly. Torn
            // reads are inherent to the cross-process editing pattern;
            // the resulting `Vec<u8>` is a single-point-in-time
            // snapshot.
            BufferStorage::Shared { user_va, len } => {
                let mut owned = alloc::vec::Vec::<u8>::with_capacity(*len);
                unsafe {
                    core::ptr::copy_nonoverlapping(*user_va as *const u8, owned.as_mut_ptr(), *len);
                    owned.set_len(*len);
                }
                BufferStorage::Owned(owned)
            }
        }
    }
}

impl Drop for BufferStorage {
    fn drop(&mut self) {
        if let BufferStorage::Shared { user_va, .. } = self {
            // Best-effort release. If the syscall fails the SHM
            // registry still has us as a holder; the leak is bounded
            // by the surviving kernel registry's lifetime, not by
            // accumulating across many surfaces.
            let _ = syscall_lib::shm_unmap(*user_va);
        }
    }
}

impl BufferStorage {
    /// Materialise an owned byte snapshot of the committed pixels.
    ///
    /// For [`BufferStorage::Owned`] this clones the existing
    /// `Vec<u8>`. For [`BufferStorage::Shared`] this allocates a
    /// fresh `Vec<u8>` and copies the pixel bytes through a raw
    /// pointer — no Rust slice reference (`&[u8]`) ever aliases the
    /// concurrently-writable shared-memory mapping, so callers can
    /// hand the resulting `Vec` to safe code without violating the
    /// aliasing model. Torn reads are inherent to the cross-process
    /// editing pattern; the snapshot is a single-point-in-time view.
    ///
    /// This is the safe entry point. Prefer this over [`as_slice`]
    /// for any code path that does not strictly need a borrow with
    /// the original lifetime.
    pub fn snapshot(&self) -> Vec<u8> {
        match self {
            BufferStorage::Owned(v) => v.clone(),
            BufferStorage::Shared { user_va, len } => {
                let mut owned = Vec::<u8>::with_capacity(*len);
                // SAFETY: kernel mapped `*len` bytes at `*user_va` for
                // this process; we copy through a raw pointer rather
                // than synthesising an `&[u8]` so the producer's
                // concurrent writes never alias a Rust reference.
                unsafe {
                    core::ptr::copy_nonoverlapping(*user_va as *const u8, owned.as_mut_ptr(), *len);
                    owned.set_len(*len);
                }
                owned
            }
        }
    }

    /// Borrow the pixels as a contiguous byte slice.
    ///
    /// # Safety
    ///
    /// For [`BufferStorage::Shared`] the returned slice aliases a
    /// shared-memory region that the originating client can mutate
    /// concurrently (the kernel maps these pages writable for every
    /// mapper). Holding an `&[u8]` across operations that allow the
    /// client to publish new bytes — even just yielding to the
    /// scheduler — gives LLVM permission to assume the bytes are
    /// stable, which they are not.
    ///
    /// Most callers should prefer [`BufferStorage::snapshot`] which
    /// performs a raw-pointer copy and never materialises an `&[u8]`
    /// to shared memory. `as_slice` exists only for borrow-only
    /// callers that can prove they consume the slice within a single
    /// operation that the producer cannot interleave.
    ///
    /// For [`BufferStorage::Owned`] the slice is just a normal
    /// `&Vec<u8>` borrow and the safety conditions are vacuously
    /// satisfied; the unsafety lives entirely on the `Shared` arm.
    pub unsafe fn as_slice(&self) -> &[u8] {
        match self {
            BufferStorage::Owned(v) => v,
            BufferStorage::Shared { user_va, len } => unsafe {
                core::slice::from_raw_parts(*user_va as *const u8, *len)
            },
        }
    }
}

/// Committed pixel buffer for one surface.
///
/// Phase 56 transports pixel bytes inline via the bulk-IPC primitive (see
/// `surface_buffer.rs` for the userspace helper); the actual `Vec<u8>` lives
/// here once committed. Width/height are tracked separately so the composer
/// can clip and stride correctly without trusting client-supplied geometry
/// on the protocol surface.
///
/// Phase 57d follow-up — `pixels` now carries a [`BufferStorage`] so
/// shared-memory-backed buffers can avoid the chunked-IPC double copy.
/// Existing readers go through [`pixels_slice`] for both variants.
#[derive(Debug, Clone)]
pub struct CommittedBuffer {
    pub buffer_id: BufferId,
    pub width: u32,
    pub height: u32,
    pub pixels: BufferStorage,
}

impl CommittedBuffer {
    /// Construct a `CommittedBuffer` from an owned pixel `Vec<u8>` —
    /// the legacy path used by the chunked-pixel transport. New
    /// shared-buffer attaches build the struct directly with a
    /// `BufferStorage::Shared` value, never going through this helper.
    pub fn from_owned(buffer_id: BufferId, width: u32, height: u32, pixels: Vec<u8>) -> Self {
        Self {
            buffer_id,
            width,
            height,
            pixels: BufferStorage::Owned(pixels),
        }
    }

    /// Borrow the committed pixels uniformly across owned and shared
    /// storage. Length is exactly `width * height * 4` for shared
    /// storage; for owned storage it's whatever the bulk transport
    /// reassembled (validated against the same expression at intake).
    ///
    /// # Safety
    ///
    /// Same conditions as [`BufferStorage::as_slice`]: for `Shared`
    /// pixels the returned slice aliases a concurrently-writable
    /// shared-memory mapping and must be consumed before yielding to
    /// the producer. Most callers should prefer
    /// [`CommittedBuffer::pixels_snapshot`].
    pub unsafe fn pixels_slice(&self) -> &[u8] {
        unsafe { self.pixels.as_slice() }
    }

    /// Materialise an owned byte snapshot of the committed pixels.
    /// Safe: for shared-memory buffers the copy goes through a raw
    /// pointer so no `&[u8]` ever aliases the producer's mapping.
    /// See [`BufferStorage::snapshot`] for the underlying mechanism.
    pub fn pixels_snapshot(&self) -> Vec<u8> {
        self.pixels.snapshot()
    }
}

/// Per-surface state owned by the display server.
///
/// Combines the pure-logic [`SurfaceStateMachine`] (event/effect engine)
/// with the userspace concerns the state machine deliberately does not
/// know about: the actual pixel storage, the committed-buffer geometry,
/// and any pending bulk-buffer that arrived but whose `CommitSurface`
/// hasn't followed yet.
struct ServerSurface {
    state: SurfaceStateMachine,
    role: Option<SurfaceRole>,
    pending_buffer: Option<CommittedBuffer>,
    committed_buffer: Option<CommittedBuffer>,
    /// Set when a new buffer is committed and the composer hasn't yet
    /// re-blitted this surface. Cleared by [`SurfaceRegistry::mark_clean`]
    /// after each compose pass.
    dirty: bool,
}

/// Outcome of forwarding a [`ClientMessage`] into the registry.
///
/// Carries any [`ServerMessage`]s the dispatcher should send back to the
/// client (e.g. `SurfaceConfigured`, `BufferReleased`, `SurfaceDestroyed`)
/// plus a typed error for protocol-violating messages.
#[derive(Debug, Default)]
pub struct DispatchResult {
    pub outbound: Vec<ServerMessage>,
    pub destroyed: Vec<SurfaceId>,
    pub created: Vec<(SurfaceId, SurfaceRole)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurfaceShimError {
    UnknownSurface(SurfaceId),
    DuplicateSurface(SurfaceId),
    StateMachine(SurfaceError),
    /// `AttachBuffer` referenced a `BufferId` that is not present in
    /// the dispatcher's pending-bulk queue. Distinct from the prior
    /// `NoPendingBulk` which conflated "no bulk at all" with "wrong
    /// id"; the two diagnostics behave very differently for a debugging
    /// reviewer or a future control-socket query.
    PendingBulkIdMismatch {
        expected: BufferId,
        pending: Vec<BufferId>,
    },
    /// A `Layer` surface tried to map with
    /// `keyboard_interactivity == Exclusive` while another `Layer`
    /// already holds the global exclusive-keyboard claim. Phase 56
    /// E.2 enforces a single exclusive-keyboard layer.
    Layer(LayerError),
    /// `AttachSharedBuffer` could not map the SHM region into the
    /// compositor's address space. Distinct from
    /// `PendingBulkIdMismatch` so the dispatcher can treat it as a
    /// hard transport failure rather than a recoverable client
    /// protocol error.
    ShmMapFailed {
        shm_id: u32,
        buffer_id: BufferId,
    },
}

impl From<LayerError> for SurfaceShimError {
    fn from(err: LayerError) -> Self {
        SurfaceShimError::Layer(err)
    }
}

/// Registry of all surfaces owned by all connected clients.
///
/// Phase 56 has a single connected client at the protocol level — the
/// dispatcher (C.5) holds one of these and forwards every message into it.
/// In a richer multi-client world this would be partitioned by ClientId;
/// the API would not change shape.
pub struct SurfaceRegistry {
    surfaces: BTreeMap<SurfaceId, ServerSurface>,
    /// Pending buffer bytes received via the bulk-transport path but not
    /// yet attached to a surface. `AttachBuffer` consumes from here.
    pending_bulk: Vec<CommittedBuffer>,
    /// Phase 57 — in-flight chunked-buffer reassembly state, keyed by
    /// `BufferId`. The first `LABEL_PIXELS_CHUNK` for a buffer creates
    /// the entry; subsequent chunks append; on completion the buffer
    /// moves into `pending_bulk` and the entry is removed. Capped at
    /// [`MAX_PENDING_CHUNK_BUFFERS`].
    chunk_accumulators: BTreeMap<BufferId, ChunkAccumulator>,
    /// Tracks which `Layer` surface (if any) currently holds the
    /// global exclusive-keyboard claim. Populated on
    /// `SetSurfaceRole(Layer { keyboard_interactivity == Exclusive })`,
    /// cleared on the holder's `DestroySurface`. The D.3 input
    /// dispatcher reads `active_exclusive_layer` to gate keyboard
    /// routing.
    layer_conflicts: LayerConflictTracker,
    /// Phase 56 Track E.3 — client-supplied cursor (the bytes most
    /// recently committed against a `SurfaceRole::Cursor` surface).
    /// `None` means the composer falls back to `DefaultArrowCursor`.
    /// At most one client cursor is active at a time; a second
    /// `SetSurfaceRole(Cursor)` + `CommitSurface` overwrites this slot.
    client_cursor: Option<ClientCursor>,
    /// `SurfaceId` of the surface that currently owns the
    /// `client_cursor` slot. Tracked separately so `DestroySurface`
    /// for that id clears the slot — without this, a destroyed
    /// cursor surface would leave a stale `ClientCursor` behind.
    client_cursor_owner: Option<SurfaceId>,
}

impl SurfaceRegistry {
    pub fn new() -> Self {
        Self {
            surfaces: BTreeMap::new(),
            pending_bulk: Vec::new(),
            chunk_accumulators: BTreeMap::new(),
            layer_conflicts: LayerConflictTracker::new(),
            client_cursor: None,
            client_cursor_owner: None,
        }
    }

    /// Phase 56 Track E.3 — the currently active client cursor, if any.
    /// `None` means the composer falls back to
    /// [`kernel_core::display::cursor::DefaultArrowCursor`]. The
    /// composer's wiring (`compose::run_compose`) calls this once per
    /// frame.
    pub fn client_cursor(&self) -> Option<&ClientCursor> {
        self.client_cursor.as_ref()
    }

    /// Number of surfaces tracked. Used by tests and the control-socket
    /// `list-surfaces` verb (E.4 — pending).
    #[allow(dead_code)]
    pub fn surface_count(&self) -> usize {
        self.surfaces.len()
    }

    /// All registered surface ids in ascending order. Used by the
    /// control-socket `list-surfaces` verb (E.4) and by `main.rs` to
    /// compute create / destroy deltas for subscription event push.
    /// Ascending order is stable because the underlying `BTreeMap`
    /// orders by `SurfaceId(u32)`; a reviewer can rely on it.
    pub fn surface_ids(&self) -> Vec<SurfaceId> {
        self.surfaces.keys().copied().collect()
    }

    /// Lookup the registered role of `surface_id`. Returns `None` if
    /// the surface is unknown or has not yet had `SetSurfaceRole`
    /// called on it. Used by the control-socket `SurfaceCreated` event
    /// emit path (E.4) so the wire-tag (`SurfaceRoleTag`) reflects the
    /// actual role rather than a default guess.
    pub fn surface_role(&self, surface_id: SurfaceId) -> Option<SurfaceRole> {
        self.surfaces.get(&surface_id).and_then(|s| s.role)
    }

    /// Receive a bulk-transported pixel buffer and queue it for the next
    /// `AttachBuffer` verb. Returns `true` if accepted, `false` if the
    /// pending-bulk queue is at [`MAX_PENDING_BULK`] and the dispatcher
    /// should treat the over-flood as a protocol violation. Phase 56's
    /// engineering-discipline rule on per-client resource bounds is
    /// enforced here, not at the IPC seam, so a future multi-client
    /// world can apply the same cap per ClientId.
    pub fn receive_bulk(&mut self, buf: CommittedBuffer) -> bool {
        if self.pending_bulk.len() >= MAX_PENDING_BULK {
            return false;
        }
        self.pending_bulk.push(buf);
        true
    }

    /// Phase 57 — apply one chunk of a chunked surface upload. The
    /// dispatcher decodes the [`PixelChunkHeader`] from the bulk and
    /// hands header + body here. Returns:
    ///
    /// - `Ok(())` on a `Pending` chunk (buffer not yet complete) or on
    ///   `Complete` (the assembled buffer is automatically pushed onto
    ///   `pending_bulk` so the next `AttachBuffer { buffer_id }`
    ///   consumes it).
    /// - `Err(_)` on geometry mismatch, body-length mismatch, or
    ///   capacity exhaustion. The caller should treat this as a
    ///   protocol violation.
    pub fn receive_chunk(
        &mut self,
        header: PixelChunkHeader,
        body: &[u8],
    ) -> Result<(), ChunkError> {
        let buffer_id = BufferId(header.buffer_id);
        // Refuse to open a NEW accumulator if we're already at the cap.
        // Refresh on existing accumulator is fine; only the open-count
        // is capped.
        if !self.chunk_accumulators.contains_key(&buffer_id)
            && self.chunk_accumulators.len() >= MAX_PENDING_CHUNK_BUFFERS
        {
            return Err(ChunkError::TotalTooLarge);
        }
        let acc = self.chunk_accumulators.entry(buffer_id).or_default();
        let outcome = acc.add_chunk(header, body)?;
        if let AddChunkOutcome::Complete {
            width,
            height,
            pixels,
        } = outcome
        {
            // Buffer is whole — drop the accumulator and queue the
            // completed buffer for `AttachBuffer`. Apply the same
            // pending-bulk cap as the single-shot LABEL_PIXELS path.
            self.chunk_accumulators.remove(&buffer_id);
            if self.pending_bulk.len() >= MAX_PENDING_BULK {
                return Err(ChunkError::TotalTooLarge);
            }
            self.pending_bulk.push(CommittedBuffer::from_owned(
                buffer_id, width, height, pixels,
            ));
        }
        Ok(())
    }

    /// Forward a [`ClientMessage`] into the appropriate surface. Returns
    /// the dispatcher result (outbound messages, lifecycle changes).
    pub fn handle_message(
        &mut self,
        msg: &ClientMessage,
    ) -> Result<DispatchResult, SurfaceShimError> {
        let mut result = DispatchResult::default();
        match msg {
            ClientMessage::CreateSurface { surface_id } => {
                if self.surfaces.contains_key(surface_id) {
                    return Err(SurfaceShimError::DuplicateSurface(*surface_id));
                }
                self.surfaces.insert(
                    *surface_id,
                    ServerSurface {
                        state: SurfaceStateMachine::new(*surface_id),
                        role: None,
                        pending_buffer: None,
                        committed_buffer: None,
                        dirty: false,
                    },
                );
            }
            ClientMessage::DestroySurface { surface_id } => {
                self.apply_event(*surface_id, SurfaceEvent::DestroySurface, &mut result)?;
                self.surfaces.remove(surface_id);
                // Release any exclusive-keyboard claim the destroyed
                // surface held. `release` is a no-op for surfaces that
                // never claimed the slot.
                self.layer_conflicts.release(*surface_id);
                // Phase 56 E.3 — if the destroyed surface owned the
                // active client cursor, clear the slot so the
                // composer falls back to `DefaultArrowCursor` on the
                // next frame.
                if self.client_cursor_owner == Some(*surface_id) {
                    self.client_cursor = None;
                    self.client_cursor_owner = None;
                }
                result.destroyed.push(*surface_id);
            }
            ClientMessage::SetSurfaceRole { surface_id, role } => {
                // Phase 56 E.2: a Layer surface declaring
                // `KeyboardInteractivity::Exclusive` is rejected if a
                // different layer already holds the global slot. We
                // validate *before* applying the state-machine event
                // so a conflict leaves the registry untouched. Non-
                // Layer roles bypass the check (try_claim returns Ok
                // for non-Exclusive configs).
                if let SurfaceRole::Layer(cfg) = role {
                    self.layer_conflicts.try_claim(*surface_id, cfg)?;
                }
                self.apply_event(*surface_id, SurfaceEvent::SetRole(*role), &mut result)?;
                if let Some(s) = self.surfaces.get_mut(surface_id) {
                    s.role = Some(*role);
                    result.created.push((*surface_id, *role));
                }
            }
            ClientMessage::AttachSharedBuffer {
                surface_id,
                buffer_id,
                shm_id,
                width,
                height,
            } => {
                // Map the SHM region into our address space. The
                // mapping survives until the resulting `CommittedBuffer`
                // is dropped (Drop calls `sys_shm_unmap`); the kernel
                // refcount keeps the underlying frames alive even if
                // the client exits before the surface is torn down.
                let user_va = syscall_lib::shm_map(*shm_id);
                if user_va == 0 {
                    // Phase 57d follow-up — log the failure so the
                    // intermittent boot-1 "no terminal" symptom (where
                    // display_server silently drops AttachSharedBuffer
                    // and the surface never gets pixels) produces a
                    // visible signal in the boot transcript.
                    syscall_lib::write_str(
                        syscall_lib::STDOUT_FILENO,
                        "display_server: AttachSharedBuffer shm_map failed shm_id=",
                    );
                    write_u32_log(*shm_id);
                    syscall_lib::write_str(syscall_lib::STDOUT_FILENO, "\n");
                    return Err(SurfaceShimError::ShmMapFailed {
                        shm_id: *shm_id,
                        buffer_id: *buffer_id,
                    });
                }
                syscall_lib::write_str(
                    syscall_lib::STDOUT_FILENO,
                    "display_server: AttachSharedBuffer ok shm_id=",
                );
                write_u32_log(*shm_id);
                syscall_lib::write_str(syscall_lib::STDOUT_FILENO, " va=");
                write_u64_log(user_va);
                syscall_lib::write_str(syscall_lib::STDOUT_FILENO, "\n");
                let byte_count = (*width as usize)
                    .saturating_mul(*height as usize)
                    .saturating_mul(4);
                let buf = CommittedBuffer {
                    buffer_id: *buffer_id,
                    width: *width,
                    height: *height,
                    pixels: BufferStorage::Shared {
                        user_va,
                        len: byte_count,
                    },
                };
                self.apply_event(
                    *surface_id,
                    SurfaceEvent::AttachBuffer(*buffer_id),
                    &mut result,
                )?;
                if let Some(s) = self.surfaces.get_mut(surface_id) {
                    s.pending_buffer = Some(buf);
                }
            }
            ClientMessage::AttachBuffer {
                surface_id,
                buffer_id,
            } => {
                // Search pending_bulk for the entry whose `buffer_id`
                // matches. This preserves attach order independent of
                // arrival order (clients can ship multiple LABEL_PIXELS
                // bulks before their matching AttachBuffer verbs) and
                // distinguishes "bulk for this id is missing" from
                // "wrong id pulled by LIFO" — the previous pop()-then-
                // push-back path could deadlock both: the same older
                // buffer kept getting re-popped on every attach.
                //
                // Validate first (apply_event), then commit the side
                // effect (remove from pending_bulk). Removing first and
                // then applying meant an `UnknownSurface` /
                // `StateMachine` error would silently drain the queue
                // — a malformed client could deplete pending_bulk
                // without ever attaching.
                let pending_index = self
                    .pending_bulk
                    .iter()
                    .position(|b| b.buffer_id == *buffer_id)
                    .ok_or_else(|| SurfaceShimError::PendingBulkIdMismatch {
                        expected: *buffer_id,
                        pending: self.pending_bulk.iter().map(|b| b.buffer_id).collect(),
                    })?;
                self.apply_event(
                    *surface_id,
                    SurfaceEvent::AttachBuffer(*buffer_id),
                    &mut result,
                )?;
                let buf = self.pending_bulk.remove(pending_index);
                if let Some(s) = self.surfaces.get_mut(surface_id) {
                    s.pending_buffer = Some(buf);
                }
            }
            ClientMessage::DamageSurface { surface_id, rect } => {
                self.apply_event(*surface_id, SurfaceEvent::DamageSurface(*rect), &mut result)?;
                // SHM-backed surfaces edit pixels in place: the buffer
                // identity does not change between commits, so the
                // existing CommitSurface arm (which marks dirty only
                // when `pending_buffer.take()` returns Some) leaves
                // them clean forever after the first commit. Damage
                // is the explicit "I changed pixel contents" signal,
                // so mark the surface dirty here whenever a buffer is
                // already attached. The chunked path is unaffected
                // because chunked clients always Damage before they
                // commit and the dirty bit is reset after compose.
                if let Some(s) = self.surfaces.get_mut(surface_id)
                    && (s.committed_buffer.is_some() || s.pending_buffer.is_some())
                {
                    s.dirty = true;
                }
            }
            ClientMessage::CommitSurface { surface_id } => {
                // The kernel-core state machine emits `SurfaceEffect::Configured`
                // with a strictly-monotone (saturating) serial on role /
                // geometry transitions; the shim used to synthesise its own
                // wrapping serial here, which (a) duplicated state-machine
                // logic and (b) could violate the protocol's monotone-serial
                // contract by wrapping past `u32::MAX`. `apply_event` now
                // forwards `Configured` straight to
                // `ServerMessage::SurfaceConfigured` — no shim-side serial
                // generation needed.
                self.apply_event(*surface_id, SurfaceEvent::CommitSurface, &mut result)?;
                if let Some(s) = self.surfaces.get_mut(surface_id)
                    && let Some(buf) = s.pending_buffer.take()
                {
                    // Phase 56 E.3 — if the surface holds a `Cursor`
                    // role, wrap the committed buffer as a
                    // `ClientCursor` for the composer's renderer
                    // path. The buffer ALSO populates the
                    // committed_buffer slot in case the surface is
                    // later re-rolled (Phase 56 doesn't allow this —
                    // SurfaceRole transitions are state-machine-
                    // checked — but storing it costs nothing and
                    // keeps the destroy-path uniform). The
                    // `client_cursor` slot wins for cursor sampling.
                    if let Some(SurfaceRole::Cursor(cfg)) = s.role {
                        match cursor_from_committed(&buf, cfg) {
                            Ok(cursor) => {
                                self.client_cursor = Some(cursor);
                                self.client_cursor_owner = Some(*surface_id);
                            }
                            Err(_) => {
                                // Malformed cursor buffer (zero size
                                // or pixel-length mismatch). Phase 56
                                // logs and ignores; the previous
                                // cursor (if any) stays active. A
                                // stricter future revision could emit
                                // a control-socket protocol error.
                            }
                        }
                    }
                    s.committed_buffer = Some(buf);
                    s.dirty = true;
                }
            }
            ClientMessage::AckConfigure { .. }
            | ClientMessage::Hello { .. }
            | ClientMessage::Goodbye => {
                // Hello / Goodbye / AckConfigure are dispatched at the
                // client-loop level (C.5), not here.
            }
            // `ClientMessage` is `#[non_exhaustive]` — accept future verbs
            // gracefully rather than crashing the compositor on an unknown
            // tag. The dispatcher logs the surrogate "fatal" path on a
            // genuine decode failure; a successfully-decoded but unknown
            // verb is benign and ignored.
            _ => {}
        }
        Ok(result)
    }

    fn apply_event(
        &mut self,
        surface_id: SurfaceId,
        event: SurfaceEvent,
        result: &mut DispatchResult,
    ) -> Result<(), SurfaceShimError> {
        let s = self
            .surfaces
            .get_mut(&surface_id)
            .ok_or(SurfaceShimError::UnknownSurface(surface_id))?;
        let (effects, err) = s.state.apply(event);
        for e in &effects {
            match e {
                SurfaceEffect::ReleaseBuffer(buffer_id) => {
                    // The state machine names the *specific* buffer to
                    // release. Drop the slot whose id matches — pending
                    // first (the state machine emits releases for both
                    // pending and active buffers, e.g. on destroy or on
                    // pending replacement) — and emit `BufferReleased`
                    // for that exact id. The previous code unconditionally
                    // dropped `committed_buffer` and could release the
                    // wrong buffer.
                    if let Some(buf) = s.pending_buffer.as_ref()
                        && buf.buffer_id == *buffer_id
                    {
                        let _ = s.pending_buffer.take();
                        result.outbound.push(ServerMessage::BufferReleased {
                            surface_id,
                            buffer_id: *buffer_id,
                        });
                        continue;
                    }
                    if let Some(buf) = s.committed_buffer.as_ref()
                        && buf.buffer_id == *buffer_id
                    {
                        let _ = s.committed_buffer.take();
                        result.outbound.push(ServerMessage::BufferReleased {
                            surface_id,
                            buffer_id: *buffer_id,
                        });
                    }
                }
                SurfaceEffect::EmitDamage(_) => {
                    // A damage-only commit (DamageSurface → CommitSurface
                    // without re-attaching a buffer) is valid — the state
                    // machine emits `EmitDamage` so the composer can re-
                    // blit the existing committed buffer's damaged regions.
                    // Mark the surface dirty so the C.4 compose gate picks
                    // it up; full-rect damage is the Phase 56 default and
                    // partial-damage plumbing lands with the C.4 follow-up.
                    s.dirty = true;
                }
                SurfaceEffect::NotifyLayoutRemoved => {
                    // C.4 (composer) and E.1 (layout) consume this via the
                    // dedicated trait paths; not surfaced to the client
                    // outbound queue.
                }
                SurfaceEffect::Configured { rect, serial } => {
                    // Forward the state machine's `Configured` straight to
                    // the wire as `ServerMessage::SurfaceConfigured`. The
                    // serial is the source of truth — strictly monotone
                    // (saturating in the state machine), so the shim does
                    // not need to track its own counter.
                    result.outbound.push(ServerMessage::SurfaceConfigured {
                        surface_id,
                        rect: *rect,
                        serial: *serial,
                    });
                }
            }
        }
        if let Some(err) = err {
            return Err(SurfaceShimError::StateMachine(err));
        }
        Ok(())
    }

    /// True when at least one surface has had a new buffer committed since
    /// the last [`Self::mark_clean`]. The composer uses this as a damage
    /// gate so a frame tick with no new commits does not produce framebuffer
    /// writes.
    pub fn has_damage(&self) -> bool {
        self.surfaces.values().any(|s| s.dirty)
    }

    /// Clear the dirty flag on every surface. Called after a compose pass
    /// completes successfully.
    pub fn mark_clean(&mut self) {
        for s in self.surfaces.values_mut() {
            s.dirty = false;
        }
    }

    /// Force a full repaint by marking every surface dirty. Used by the
    /// Tier 1 fullscreen-takeover reclaim path: between `YieldFb` and
    /// `ReclaimFb` a foreign program drew over the framebuffer, so the
    /// composer can't trust any cached "nothing changed" damage state.
    pub fn mark_all_dirty(&mut self) {
        for s in self.surfaces.values_mut() {
            s.dirty = true;
        }
    }

    /// Iterate all live surfaces with their current committed buffer (if
    /// any) and their layer / geometry. The composer wiring (C.4) consumes
    /// this to build `ComposeSurface`s for each frame.
    ///
    /// `Layer` surfaces are placed via
    /// [`kernel_core::display::layer::compute_layer_geometry`] using
    /// their `LayerConfig` (anchor mask + margins) and the buffer's
    /// committed dimensions as the intrinsic size (E.2). `Toplevel`
    /// surfaces center inside the output here; the composer wiring
    /// (C.4) overrides toplevel placement with the
    /// `LayoutPolicy::arrange` result.
    ///
    /// Phase 56 E.3 caveat: surfaces with `SurfaceRole::Cursor` are
    /// **not** returned here. The composer renders them through the
    /// [`CursorRenderer`](kernel_core::display::cursor::CursorRenderer)
    /// trait (via `client_cursor()`), not as a regular layered surface.
    pub fn iter_compose(&self, output: Rect) -> Vec<ComposeEntry<'_>> {
        self.iter_compose_filtered(output, |_| true)
    }

    /// Phase 72 — workspace-aware compose iterator. `include_toplevel`
    /// is consulted for every `Toplevel`-role surface; returning
    /// `false` skips that surface (it lives on a non-active
    /// workspace). `Layer` / `Background` / `Overlay` surfaces are
    /// always included (e.g. a status bar layer is visible across all
    /// workspaces). Cursor-role surfaces are always skipped — they
    /// render via the `CursorRenderer` path.
    pub fn iter_compose_filtered<F: Fn(SurfaceId) -> bool>(
        &self,
        output: Rect,
        include_toplevel: F,
    ) -> Vec<ComposeEntry<'_>> {
        let mut entries = Vec::new();
        for (id, surface) in self.surfaces.iter() {
            let Some(buf) = surface.committed_buffer.as_ref() else {
                continue;
            };
            let (role_layer, rect) = match surface.role {
                // Cursor-role surfaces render via the CursorRenderer
                // path (E.3) — `client_cursor()` and the cursor blit
                // in `compose::run_compose`. Skip them here so the
                // composer's per-surface blit does not double-draw.
                Some(SurfaceRole::Cursor(_)) => continue,
                Some(SurfaceRole::Layer(cfg)) => {
                    let layer_band = match cfg.layer {
                        Layer::Background => ComposeLayer::Background,
                        Layer::Bottom => ComposeLayer::Bottom,
                        Layer::Top => ComposeLayer::Top,
                        Layer::Overlay => ComposeLayer::Overlay,
                    };
                    let geometry = compute_layer_geometry(output, &cfg, (buf.width, buf.height));
                    (layer_band, geometry)
                }
                Some(SurfaceRole::Toplevel) | None => {
                    if !include_toplevel(*id) {
                        continue;
                    }
                    (
                        ComposeLayer::Toplevel,
                        centre_rect(output, buf.width, buf.height),
                    )
                }
            };
            entries.push(ComposeEntry {
                id: *id,
                layer: role_layer,
                rect,
                buf,
            });
        }
        // Stable order: by layer ascending (composer requires this),
        // then by surface id for determinism within a layer.
        entries.sort_by(|a, b| (a.layer as u8, a.id.0).cmp(&(b.layer as u8, b.id.0)));
        entries
    }

    /// Collect the exclusive-zone rectangles to subtract from the
    /// toplevel band, in output coordinates. The composer (C.4) feeds
    /// these to `LayoutPolicy::arrange` so toplevels are arranged
    /// outside any docked panels / taskbars / status bars.
    ///
    /// Each `Layer` surface with `exclusive_zone != 0` and a single-
    /// edge anchor pattern contributes one rectangle. Multi-edge
    /// anchors and zero `exclusive_zone` are skipped — see
    /// [`kernel_core::display::layer::derive_exclusive_rect`] for the
    /// full exclusion table.
    pub fn exclusive_zones(&self, output: Rect) -> Vec<Rect> {
        let mut zones = Vec::new();
        for surface in self.surfaces.values() {
            let Some(buf) = surface.committed_buffer.as_ref() else {
                continue;
            };
            let Some(SurfaceRole::Layer(cfg)) = surface.role else {
                continue;
            };
            let geometry = compute_layer_geometry(output, &cfg, (buf.width, buf.height));
            if let Some(rect) = derive_exclusive_rect(geometry, &cfg) {
                zones.push(rect);
            }
        }
        zones
    }

    /// The surface (if any) currently holding the global exclusive-
    /// keyboard claim. The D.3 input dispatcher consults this to gate
    /// `KeyboardInteractivity` routing so an exclusive layer always
    /// wins focus while mapped.
    ///
    /// `#[allow(dead_code)]` because D.3's `CompositorState`
    /// `active_exclusive_layer` field that consumes this getter has not
    /// yet merged into the integration branch. Once D.3 plumbs through,
    /// the allow can drop.
    #[allow(dead_code)]
    pub fn active_exclusive_layer(&self) -> Option<SurfaceId> {
        self.layer_conflicts.active()
    }
}

/// One entry in the per-frame compose plan emitted by
/// [`SurfaceRegistry::iter_compose`]. The composer turns this into a
/// `ComposeSurface` borrowing back into the registry.
pub struct ComposeEntry<'a> {
    pub id: SurfaceId,
    pub layer: ComposeLayer,
    pub rect: Rect,
    pub buf: &'a CommittedBuffer,
}

impl<'a> ComposeEntry<'a> {
    /// Phase 56 opacity heuristic: `Toplevel` and `Background` surfaces are
    /// opaque (no client-side alpha); `Layer`, `Cursor`, `Top`, `Overlay`,
    /// and `Bottom` are not.
    pub fn is_opaque(&self) -> bool {
        matches!(
            self.layer,
            ComposeLayer::Toplevel | ComposeLayer::Background
        )
    }
}

/// Phase 57d follow-up — minimal u32 → decimal serial log helper.
/// Used by the intermittent-AttachSharedBuffer trace lines so we can
/// see whether display_server's `shm_map` succeeded or failed and
/// what id it received. No `alloc` dependency; writes directly to
/// the stdout fd.
fn write_u32_log(mut value: u32) {
    let mut buf = [0u8; 10];
    let mut idx = buf.len();
    if value == 0 {
        idx -= 1;
        buf[idx] = b'0';
    } else {
        while value != 0 {
            idx -= 1;
            buf[idx] = b'0' + (value % 10) as u8;
            value /= 10;
        }
    }
    if let Ok(s) = core::str::from_utf8(&buf[idx..]) {
        syscall_lib::write_str(syscall_lib::STDOUT_FILENO, s);
    }
}

/// Phase 57d follow-up — companion u64 hex logger for the SHM va.
fn write_u64_log(mut value: u64) {
    let mut buf = [0u8; 18];
    buf[0] = b'0';
    buf[1] = b'x';
    let mut idx = buf.len();
    if value == 0 {
        idx -= 1;
        buf[idx] = b'0';
    } else {
        while value != 0 {
            idx -= 1;
            let digit = (value & 0xF) as u8;
            buf[idx] = if digit < 10 {
                b'0' + digit
            } else {
                b'a' + (digit - 10)
            };
            value >>= 4;
        }
    }
    syscall_lib::write_str(syscall_lib::STDOUT_FILENO, "0x");
    if let Ok(s) = core::str::from_utf8(&buf[idx..]) {
        syscall_lib::write_str(syscall_lib::STDOUT_FILENO, s);
    }
}

fn centre_rect(output: Rect, w: u32, h: u32) -> Rect {
    let cx = output.x + (output.w as i32 - w as i32) / 2;
    let cy = output.y + (output.h as i32 - h as i32) / 2;
    Rect { x: cx, y: cy, w, h }
}

/// Phase 56 E.3 — wrap a committed BGRA8888 byte buffer as a
/// [`ClientCursor`] for the composer's renderer slot. The buffer's
/// `pixels` field is a packed `Vec<u8>` (BGRA byte order); we
/// recompose it as a sequence of `u32`s (little-endian on the wire,
/// matching the framebuffer's pixel format).
///
/// Returns the underlying [`ClientCursor::new`] error verbatim so a
/// future control-socket path can emit a typed protocol error
/// instead of silently dropping the bind.
fn cursor_from_committed(
    buf: &CommittedBuffer,
    cfg: CursorConfig,
) -> Result<ClientCursor, kernel_core::display::cursor::ClientCursorError> {
    let byte_count = (buf.width as usize)
        .checked_mul(buf.height as usize)
        .and_then(|wh| wh.checked_mul(4))
        .unwrap_or(usize::MAX);
    // `pixels_snapshot` does a raw-pointer copy for shared-memory
    // buffers, so we never hold an `&[u8]` to concurrently-writable
    // memory while decoding into `packed`.
    let pixels = buf.pixels_snapshot();
    if pixels.len() != byte_count {
        return Err(
            kernel_core::display::cursor::ClientCursorError::PixelLengthMismatch {
                expected: byte_count,
                actual: pixels.len(),
            },
        );
    }
    // Decode the BGRA byte stream into u32 cells. Each cell is one
    // pixel (BGRA in little-endian wire byte order — `to_le_bytes`
    // round-trips back to `[B, G, R, A]`).
    let pixel_count = byte_count / 4;
    let mut packed: Vec<u32> = Vec::with_capacity(pixel_count);
    for chunk in pixels.chunks_exact(4) {
        let arr = [chunk[0], chunk[1], chunk[2], chunk[3]];
        packed.push(u32::from_le_bytes(arr));
    }
    ClientCursor::from_vec(packed, buf.width, buf.height, cfg)
}

impl Default for SurfaceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// NB: a `#[cfg(test)]` placeholder module was here. `display_server` is
// `no_std` + `no_main`, so the std `test` harness cannot compile it.
// The pure-logic invariants of this shim are covered by the
// kernel-core `surface` state-machine tests; end-to-end verification is
// the Phase 56 G.1 regression test in QEMU.
