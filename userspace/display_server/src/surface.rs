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
use kernel_core::display::protocol::{
    BufferId, ClientMessage, Layer, Rect, ServerMessage, SurfaceId, SurfaceRole,
};

/// High-water mark for the pending-bulk queue. A client that ships
/// `LABEL_PIXELS` without a matching `AttachBuffer` more than this many
/// times in flight is exceeding the documented Phase 56 resource bound;
/// the dispatcher closes the connection on overflow instead of growing
/// the compositor's memory unboundedly. Recorded in the H.1 learning doc
/// alongside the per-client surface and outbound-event-queue caps.
pub const MAX_PENDING_BULK: usize = 4;
use kernel_core::display::surface::{
    SurfaceEffect, SurfaceError, SurfaceEvent, SurfaceStateMachine,
};

/// Committed pixel buffer for one surface.
///
/// Phase 56 transports pixel bytes inline via the bulk-IPC primitive (see
/// `surface_buffer.rs` for the userspace helper); the actual `Vec<u8>` lives
/// here once committed. Width/height are tracked separately so the composer
/// can clip and stride correctly without trusting client-supplied geometry
/// on the protocol surface.
#[derive(Debug, Clone)]
pub struct CommittedBuffer {
    pub buffer_id: BufferId,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceShimError {
    UnknownSurface(SurfaceId),
    DuplicateSurface(SurfaceId),
    StateMachine(SurfaceError),
    /// Attached a buffer that was never queued via the bulk transport. The
    /// dispatcher attaches the most-recently-received bulk buffer when the
    /// client sends `AttachBuffer`; this error means no such bulk preceded
    /// the verb.
    NoPendingBulk,
}

/// Registry of all surfaces owned by all connected clients.
///
/// Phase 56 has a single connected client at the protocol level — the
/// dispatcher (C.5) holds one of these and forwards every message into it.
/// In a richer multi-client world this would be partitioned by ClientId;
/// the API would not change shape.
pub struct SurfaceRegistry {
    surfaces: BTreeMap<SurfaceId, ServerSurface>,
    next_serial: u32,
    /// Pending buffer bytes received via the bulk-transport path but not
    /// yet attached to a surface. `AttachBuffer` consumes from here.
    pending_bulk: Vec<CommittedBuffer>,
}

impl SurfaceRegistry {
    pub fn new() -> Self {
        Self {
            surfaces: BTreeMap::new(),
            next_serial: 1,
            pending_bulk: Vec::new(),
        }
    }

    /// Number of surfaces tracked. Used by tests and the control-socket
    /// `list-surfaces` verb (E.4 — pending).
    #[allow(dead_code)]
    pub fn surface_count(&self) -> usize {
        self.surfaces.len()
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
                result.destroyed.push(*surface_id);
            }
            ClientMessage::SetSurfaceRole { surface_id, role } => {
                self.apply_event(*surface_id, SurfaceEvent::SetRole(*role), &mut result)?;
                if let Some(s) = self.surfaces.get_mut(surface_id) {
                    s.role = Some(*role);
                    result.created.push((*surface_id, *role));
                }
            }
            ClientMessage::AttachBuffer {
                surface_id,
                buffer_id,
            } => {
                let buf = self
                    .pending_bulk
                    .pop()
                    .ok_or(SurfaceShimError::NoPendingBulk)?;
                if buf.buffer_id != *buffer_id {
                    // Recycle: protocol mismatch between the bulk we
                    // received and the AttachBuffer the client sent.
                    self.pending_bulk.push(buf);
                    return Err(SurfaceShimError::NoPendingBulk);
                }
                self.apply_event(
                    *surface_id,
                    SurfaceEvent::AttachBuffer(*buffer_id),
                    &mut result,
                )?;
                if let Some(s) = self.surfaces.get_mut(surface_id) {
                    s.pending_buffer = Some(buf);
                }
            }
            ClientMessage::DamageSurface { surface_id, rect } => {
                self.apply_event(*surface_id, SurfaceEvent::DamageSurface(*rect), &mut result)?;
            }
            ClientMessage::CommitSurface { surface_id } => {
                self.apply_event(*surface_id, SurfaceEvent::CommitSurface, &mut result)?;
                if let Some(s) = self.surfaces.get_mut(surface_id) {
                    if let Some(buf) = s.pending_buffer.take() {
                        s.committed_buffer = Some(buf);
                        s.dirty = true;
                    }
                    let serial = self.next_serial;
                    self.next_serial = self.next_serial.wrapping_add(1).max(1);
                    let geom = s.state.geometry();
                    result.outbound.push(ServerMessage::SurfaceConfigured {
                        surface_id: *surface_id,
                        rect: geom,
                        serial,
                    });
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
                SurfaceEffect::ReleaseBuffer(slot) => {
                    if let Some(b) = s.committed_buffer.take() {
                        result.outbound.push(ServerMessage::BufferReleased {
                            surface_id,
                            buffer_id: b.buffer_id,
                        });
                    }
                    let _ = slot;
                }
                SurfaceEffect::EmitDamage(_) | SurfaceEffect::NotifyLayoutRemoved => {
                    // C.4 (composer) and E.1 (layout) consume these via
                    // the dedicated trait paths; not surfaced to the
                    // client outbound queue.
                }
                SurfaceEffect::Configured { .. } => {
                    // Generated separately on commit; the state machine's
                    // own `Configured` is not directly forwarded to the
                    // client.
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

    /// Iterate all live surfaces with their current committed buffer (if
    /// any) and their layer / geometry. The composer wiring (C.4) consumes
    /// this to build `ComposeSurface`s for each frame.
    pub fn iter_compose(&self, output: Rect) -> Vec<ComposeEntry<'_>> {
        let mut entries = Vec::new();
        for (id, surface) in self.surfaces.iter() {
            let Some(buf) = surface.committed_buffer.as_ref() else {
                continue;
            };
            let role_layer = match surface.role {
                Some(SurfaceRole::Cursor(_)) => ComposeLayer::Cursor,
                Some(SurfaceRole::Layer(cfg)) => match cfg.layer {
                    Layer::Background => ComposeLayer::Background,
                    Layer::Bottom => ComposeLayer::Bottom,
                    Layer::Top => ComposeLayer::Top,
                    Layer::Overlay => ComposeLayer::Overlay,
                },
                Some(SurfaceRole::Toplevel) | None => ComposeLayer::Toplevel,
            };
            // Centre the surface inside the output by default; layout
            // policy (E.1 wiring) will override this for `Toplevel` once
            // C.4 plugs the LayoutPolicy in.
            let rect = centre_rect(output, buf.width, buf.height);
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

fn centre_rect(output: Rect, w: u32, h: u32) -> Rect {
    let cx = output.x + (output.w as i32 - w as i32) / 2;
    let cy = output.y + (output.h as i32 - h as i32) / 2;
    Rect { x: cx, y: cy, w, h }
}

impl Default for SurfaceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    // Tests live alongside `kernel_core::display::surface` for the pure-
    // logic invariants. The shim itself is mechanical wiring and is
    // exercised end-to-end by the QEMU regression suite (G.1).
    //
    // The placeholder below ensures the module compiles in test builds
    // without dragging in the QEMU-only entry-point glue.
    #[test]
    fn shim_module_compiles() {
        let _ = super::SurfaceRegistry::new();
    }
}
