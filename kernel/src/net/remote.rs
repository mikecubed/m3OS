//! `RemoteNic` kernel facade — Phase 55b Track E.4.
// Allow dead-code: public API methods are wired in by Track E.5 (dispatch
// integration) and Track F (supervision). Suppressing here so `cargo xtask
// check` is green without requiring all callers to land in the same commit.
#![allow(dead_code)]
//!
//! Provides the kernel-side forwarding shim that routes `net::send_frame`
//! calls to the ring-3 e1000 driver process over IPC, and accepts inbound
//! `NET_RX_FRAME` notifications from that driver to feed into
//! `net::dispatch::process_rx_frames` — the same entry point virtio-net uses.
//!
//! # Dispatch priority
//!
//! Once `RemoteNic::register` is called the global `REMOTE_NIC` static holds
//! the facade. `net::send_frame` checks `RemoteNic::is_registered()` first;
//! if the check succeeds it calls `RemoteNic::send_frame`, otherwise it falls
//! back to virtio-net. This matches the Phase 55 priority ordering with the
//! in-kernel e1000 removed.
//!
//! # TX path
//!
//! `send_frame` enqueues a raw Ethernet frame into a fixed-depth ring buffer.
//! The driver endpoint is expected to issue `NET_SEND_FRAME` IPC receives in a
//! tight loop; the kernel net task drains the queue by calling `drain_tx_queue`
//! which attempts one non-blocking IPC send per queued frame. When the driver
//! is not yet waiting the frame stays queued until the next wake.
//!
//! # RX path
//!
//! The ring-3 driver uses `ipc_send_buf` to deliver a `NET_RX_FRAME` header
//! (8 bytes) + bulk frame. The kernel net task's existing receive endpoint
//! calls `RemoteNic::inject_rx_frame` directly with the decoded frame bytes;
//! that function calls `dispatch::process_rx_frames` and sets `NIC_WOKEN`.
//!
//! # Link-state
//!
//! When the driver sends `NET_LINK_STATE` the facade decodes the event and,
//! on link-down, calls `tcp::on_link_down()` — the one-line hook that resets
//! pending retransmit timers per the Phase 16 contract.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

#[allow(dead_code)]
use alloc::collections::VecDeque;
use kernel_core::driver_ipc::net::{
    NetDriverError, NetLinkEvent, decode_net_link_event, decode_net_rx_notify,
};
use kernel_core::types::{EndpointId, MacAddr};
use spin::Mutex;

// ---------------------------------------------------------------------------
// Tunable — TX queue depth cap
// ---------------------------------------------------------------------------

/// Maximum number of in-flight TX frames queued while the driver endpoint
/// is not yet receiving. Frames beyond this cap are dropped with a warn log,
/// matching the Phase 16 net-stack convention for overflow conditions.
const TX_QUEUE_DEPTH: usize = 64;

// ---------------------------------------------------------------------------
// Global facade registry
// ---------------------------------------------------------------------------

/// A registered ring-3 NIC driver entry.
struct NicEntry {
    #[allow(dead_code)]
    endpoint: EndpointId,
    mac: MacAddr,
    /// Pending TX frames: raw Ethernet bytes waiting to be forwarded to the
    /// driver endpoint via IPC.
    tx_queue: VecDeque<alloc::vec::Vec<u8>>,
}

/// Global slot for the registered `RemoteNic`. `None` while no ring-3 NIC
/// driver has registered; `Some(…)` once `RemoteNic::register` completes.
static REMOTE_NIC: Mutex<Option<NicEntry>> = Mutex::new(None);

/// Set when `RemoteNic::register` succeeds; checked lock-free on the hot
/// TX path by `net::send_frame`.
static REMOTE_NIC_REGISTERED: AtomicBool = AtomicBool::new(false);

/// Set when an IPC transport failure is detected on `drain_tx_queue`, cleared
/// on `register`. When this flag is set `send_frame` returns
/// `NetDriverError::DriverRestarting` instead of queuing the frame, mirroring
/// the Phase 55b D.4b / F.2b semantics for the block path.
static RESTART_SUSPECTED: AtomicBool = AtomicBool::new(false);

/// Deduplicate the "driver absent" warn log on the `send_frame` hot path.
/// Set when the first absent-driver warn is emitted; cleared on `register`.
/// Subsequent `send_frame` calls during the same restart window skip the
/// warn so the log is not flooded.  Matches the observability requirement in
/// Phase 55c Track G.3: "logs `driver.absent` (warn, deduplicated) on the
/// first send during a restart window; subsequent sends during the same window
/// do not re-log until restart completes."
static ABSENT_WARN_EMITTED: AtomicBool = AtomicBool::new(false);

/// Cold-path miss counter used by `is_registered` to rate-limit repeated
/// service-registry lookups when no ring-3 NIC driver is present. An actual
/// registry lookup is attempted only when the low `LOOKUP_RETRY_MASK` bits
/// are zero, so one lookup is performed per `LOOKUP_RETRY_MASK + 1` misses
/// instead of one lookup per TX frame. Reset to zero by `register` so the
/// re-registration path does not defer the first post-restart lookup.
static LOOKUP_MISS_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Cold-path retry cadence — an actual service-registry lookup runs once
/// every `LOOKUP_RETRY_MASK + 1` calls while the fast-path atomic is false.
/// Power-of-two minus one so the check compiles to a single `AND`.
const LOOKUP_RETRY_MASK: u32 = 0x3ff;

// ---------------------------------------------------------------------------
// RemoteNic public API
// ---------------------------------------------------------------------------

/// Kernel-side forwarding facade for the ring-3 e1000 driver.
///
/// Callers interact with this type exclusively through its static methods;
/// there is no heap-allocated handle. Ownership is tracked by the global
/// `REMOTE_NIC` slot.
pub struct RemoteNic;

impl RemoteNic {
    /// Register a ring-3 NIC driver endpoint so TX frames are forwarded to it
    /// and RX frames from it are delivered to the kernel net stack.
    ///
    /// Replaces any previously registered entry. Logs a structured
    /// `remote_nic.registered` event at info level.
    pub fn register(endpoint: EndpointId, mac: MacAddr) {
        {
            let mut slot = REMOTE_NIC.lock();
            *slot = Some(NicEntry {
                endpoint,
                mac,
                tx_queue: VecDeque::new(),
            });
        }
        REMOTE_NIC_REGISTERED.store(true, Ordering::Release);
        // Clear restart-suspected on successful re-registration so subsequent
        // send_frame calls are admitted again.
        RESTART_SUSPECTED.store(false, Ordering::Release);
        // Clear the absent-warn dedup flag so the first send after a restart
        // emits a fresh log line if the driver goes absent again.
        ABSENT_WARN_EMITTED.store(false, Ordering::Relaxed);
        // Reset the cold-path miss counter so the first post-(re)registration
        // lookup is not deferred by the retry cadence.
        LOOKUP_MISS_COUNTER.store(0, Ordering::Relaxed);
        log::info!(
            "[remote_nic] registered ring-3 NIC driver: endpoint={:?} mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            endpoint,
            mac[0],
            mac[1],
            mac[2],
            mac[3],
            mac[4],
            mac[5],
        );
    }

    /// Remove the registered ring-3 NIC entry. After this call `send_frame`
    /// falls back to virtio-net. Logs a `remote_nic.unregistered` event.
    pub fn unregister() {
        {
            let mut slot = REMOTE_NIC.lock();
            *slot = None;
        }
        REMOTE_NIC_REGISTERED.store(false, Ordering::Release);
        log::info!("[remote_nic] ring-3 NIC driver unregistered");
    }

    /// Return `true` when a ring-3 NIC driver is currently registered.
    ///
    /// Fast path: lock-free `AtomicBool` read with `Acquire` ordering.
    ///
    /// Cold path: when the atomic is `false` the facade tries a service-registry
    /// lookup for `"net.nic"`. To avoid per-packet lock contention on systems
    /// that boot without a ring-3 e1000 driver, the cold-path lookup is rate-
    /// limited: it runs once every `LOOKUP_RETRY_MASK + 1` calls while the
    /// fast-path atomic stays false. Successful registration clears the counter
    /// so the first post-(re)registration lookup runs immediately.
    ///
    /// When the ring-3 e1000 driver has published its endpoint under that name,
    /// the facade installs it with a placeholder MAC (`[0; 6]`) and returns
    /// `true`. The real MAC is filled in when the driver emits a
    /// `NET_LINK_STATE` IPC message that reaches `handle_link_state` →
    /// `apply_link_event`, which updates the stored MAC.
    ///
    /// After the first successful cold-path lookup all subsequent calls return
    /// immediately via the atomic.
    pub fn is_registered() -> bool {
        // Fast path.
        if REMOTE_NIC_REGISTERED.load(Ordering::Acquire) {
            return true;
        }
        // Negative-latch: only attempt an actual registry lookup every
        // `LOOKUP_RETRY_MASK + 1` misses. This keeps the hot TX path lock-free
        // on systems where no ring-3 NIC driver ever registers while still
        // catching a driver that registers lazily after early boot.
        let miss = LOOKUP_MISS_COUNTER.fetch_add(1, Ordering::Relaxed);
        if (miss & LOOKUP_RETRY_MASK) != 0 {
            return false;
        }
        // Cold path — service-registry lookup (throttled to once per 1024 misses).
        if let Some(ep) = crate::ipc::registry::lookup_endpoint_id("net.nic") {
            {
                let mut slot = REMOTE_NIC.lock();
                // Guard against a concurrent cold-path race.
                if slot.is_none() {
                    *slot = Some(NicEntry {
                        endpoint: ep,
                        mac: [0u8; 6],
                        tx_queue: VecDeque::new(),
                    });
                }
            }
            REMOTE_NIC_REGISTERED.store(true, Ordering::Release);
            RESTART_SUSPECTED.store(false, Ordering::Release);
            log::info!(
                "[remote_nic] auto-registered ring-3 e1000 driver via service \
                 registry ('net.nic' → endpoint {:?}); MAC pending NET_LINK_STATE",
                ep
            );
            return true;
        }
        false
    }

    /// Return the MAC address of the registered ring-3 NIC, or `None` if no
    /// driver is registered.
    pub fn mac_address() -> Option<MacAddr> {
        REMOTE_NIC.lock().as_ref().map(|e| e.mac)
    }

    /// Phase 55c Track G R1 — pre-send errno gate for `sys_sendto`.
    ///
    /// Loads the live restart flags and forwards them to the host-testable
    /// `kernel_core::driver_ipc::net::sendto_restart_errno()` seam that the
    /// UDP/ICMP syscall path also relies on.
    pub fn sendto_restart_ret() -> Option<i64> {
        kernel_core::driver_ipc::net::sendto_restart_errno(
            REMOTE_NIC_REGISTERED.load(Ordering::Acquire),
            RESTART_SUSPECTED.load(Ordering::Acquire),
        )
    }

    /// Enqueue a raw Ethernet frame for delivery to the ring-3 driver over IPC.
    ///
    /// Returns [`NetDriverError::DeviceAbsent`] when no driver is registered,
    /// [`NetDriverError::DriverRestarting`] when an IPC transport failure was
    /// previously observed (the driver is presumed to be restarting),
    /// [`NetDriverError::InvalidFrame`] when the frame is oversized, and
    /// [`NetDriverError::RingFull`] when the TX queue is at capacity (the
    /// frame is dropped — callers may retry on the next network tick).
    ///
    /// Phase 55b Track F.3d-3: mirrors the D.4b / F.2b block-path semantics.
    pub fn send_frame(frame: &[u8]) -> Result<(), NetDriverError> {
        if frame.len() > kernel_core::driver_ipc::net::MAX_FRAME_BYTES as usize {
            return Err(NetDriverError::InvalidFrame);
        }
        // If a previous IPC drain detected an endpoint closure, surface
        // DriverRestarting immediately — the TX queue is cleared on restart.
        if RESTART_SUSPECTED.load(Ordering::Acquire) {
            log::warn!(
                "[remote_nic] send_frame: driver restart suspected, returning DriverRestarting"
            );
            return Err(NetDriverError::DriverRestarting);
        }
        let mut slot = REMOTE_NIC.lock();
        let entry = match slot.as_mut() {
            Some(e) => e,
            None => {
                // Deduplicate: only emit the warn on the first absent-driver
                // call per restart window.  Subsequent calls are silently
                // counted until `register()` clears `ABSENT_WARN_EMITTED`.
                if !ABSENT_WARN_EMITTED.swap(true, Ordering::Relaxed) {
                    log::warn!("[remote_nic] driver.absent: no ring-3 NIC registered");
                }
                return Err(NetDriverError::DeviceAbsent);
            }
        };
        if entry.tx_queue.len() >= TX_QUEUE_DEPTH {
            log::warn!(
                "[remote_nic] send_frame: TX queue full ({} frames) — dropping",
                TX_QUEUE_DEPTH
            );
            return Err(NetDriverError::RingFull);
        }
        entry.tx_queue.push_back(frame.to_vec());
        log::debug!(
            "[remote_nic] TX {} bytes queued (queue depth {})",
            frame.len(),
            entry.tx_queue.len(),
        );
        crate::net::virtio_net::wake_net_task();
        Ok(())
    }

    /// Drain the TX queue by forwarding each pending frame to the registered
    /// driver endpoint via IPC `send_buf`.
    ///
    /// Called from the network processing task's main loop alongside
    /// `dispatch::process_rx`. For each queued frame it constructs a
    /// `NET_SEND_FRAME` header and delivers header + frame bytes to the driver
    /// endpoint. Returns the number of frames forwarded.
    ///
    /// If the driver endpoint has no receiver waiting, `ipc::endpoint::send`
    /// will queue the message; the driver will drain it when it next calls
    /// `ipc_recv_msg`. This is safe because the TX queue is bounded.
    pub fn drain_tx_queue() -> usize {
        use crate::ipc::endpoint;
        use crate::ipc::message::Message;
        use crate::task::scheduler;
        use kernel_core::driver_ipc::net::{NET_SEND_FRAME, NetFrameHeader, encode_net_send};

        // Validate task context before touching the queue. If there is no
        // current task (e.g., we were called outside a scheduled kernel task
        // somehow) we must leave queued frames in place rather than draining
        // them into the floor — the next call will retry.
        let task_id = match scheduler::current_task_id() {
            Some(id) => id,
            None => return 0,
        };
        let (endpoint, frames) = {
            let mut slot = REMOTE_NIC.lock();
            let entry = match slot.as_mut() {
                Some(e) => e,
                None => return 0,
            };
            let ep = entry.endpoint;
            let frames: alloc::vec::Vec<_> = entry.tx_queue.drain(..).collect();
            (ep, frames)
        };
        let mut forwarded = 0usize;
        let total = frames.len();
        for (idx, frame) in frames.iter().enumerate() {
            let header = NetFrameHeader {
                kind: NET_SEND_FRAME,
                frame_len: frame.len() as u16,
                flags: 0,
            };
            let hdr_bytes = encode_net_send(header);
            // Deliver header + frame through the IPC send_bulk path. Since
            // this runs in the kernel net task (not an ISR), blocking briefly
            // while the driver loop catches up is acceptable.
            let mut bulk = alloc::vec::Vec::with_capacity(hdr_bytes.len() + frame.len());
            bulk.extend_from_slice(&hdr_bytes);
            bulk.extend_from_slice(frame);
            let bulk_len = bulk.len();
            scheduler::deliver_bulk(task_id, bulk);
            let msg = Message::with2(NET_SEND_FRAME as u64, 0, bulk_len as u64);
            if endpoint::send(task_id, endpoint, msg) {
                forwarded += 1;
            } else {
                // First IPC failure: mark restart-suspected and stop draining.
                // Continuing would overwrite the bulk buffer on every iteration
                // and emit one warn line per remaining frame; the driver is
                // already presumed down, so remaining frames are dropped and
                // future send_frame() calls will surface DriverRestarting until
                // the driver re-registers.
                let dropped = total.saturating_sub(idx + 1);
                log::warn!(
                    "[remote_nic] drain_tx_queue: IPC send failed after {} forwarded, \
                     dropping {} remaining — marking restart-suspected",
                    forwarded,
                    dropped,
                );
                Self::on_ipc_error();
                break;
            }
        }
        forwarded
    }

    /// Mark the driver as restart-suspected after an IPC transport failure.
    ///
    /// Sets `RESTART_SUSPECTED` so subsequent `send_frame` calls return
    /// [`NetDriverError::DriverRestarting`] rather than queuing frames into
    /// the now-unreachable driver endpoint. The flag is cleared by `register`
    /// when the driver re-registers after restart.
    ///
    /// Mirrors `on_ipc_error()` in `kernel/src/blk/remote.rs` (Phase 55b D.4b
    /// / F.2b). Idempotent — safe to call from `drain_tx_queue` on repeated
    /// IPC failures.
    pub fn on_ipc_error() {
        if !RESTART_SUSPECTED.load(Ordering::Acquire) {
            RESTART_SUSPECTED.store(true, Ordering::Release);
            log::warn!("[remote_nic] IPC transport failure — driver presumed restarting");
        }
    }

    /// Inject a single received Ethernet frame into the kernel net stack.
    ///
    /// Called from the net task's IPC receive loop when the driver sends a
    /// `NET_RX_FRAME` message. The `header_and_frame` slice must contain a
    /// valid `NET_RX_FRAME` header (8 bytes) followed by the raw Ethernet
    /// frame bytes.
    ///
    /// On success, the frame is passed to `dispatch::process_rx_frames` and
    /// `NIC_WOKEN` is set to wake the net task's next poll iteration.
    /// Returns the number of frames actually dispatched (0 or 1) — a frame
    /// rejected by the Ethernet parser is counted as zero so metrics and
    /// callers that read the return value do not over-report throughput.
    pub fn inject_rx_frame(header_and_frame: &[u8]) -> usize {
        match decode_net_rx_notify(header_and_frame) {
            Ok(hdr) => {
                let frame_len = hdr.frame_len as usize;
                let header_size = kernel_core::driver_ipc::net::NET_FRAME_HEADER_SIZE;
                if header_and_frame.len() < header_size + frame_len {
                    log::warn!(
                        "[remote_nic] RX: payload shorter than declared frame_len {}",
                        frame_len,
                    );
                    return 0;
                }
                let frame = &header_and_frame[header_size..header_size + frame_len];
                if super::dispatch::process_rx_frames(frame) {
                    crate::net::virtio_net::wake_net_task();
                    1
                } else {
                    log::warn!(
                        "[remote_nic] RX: malformed Ethernet frame dropped ({} bytes)",
                        frame.len(),
                    );
                    0
                }
            }
            Err(e) => {
                log::warn!("[remote_nic] RX: bad NET_RX_FRAME header: {:?}", e);
                0
            }
        }
    }

    /// Handle a `NET_LINK_STATE` IPC payload from the ring-3 driver.
    ///
    /// Decodes the [`NetLinkEvent`] and propagates the state to the net
    /// subsystem. On link-down, calls `tcp::on_link_down()` — the one-line
    /// hook that resets pending TCP retransmit timers per the Phase 16
    /// contract. Always sets `NIC_WOKEN` so the net task's next iteration
    /// re-evaluates MAC / route state.
    pub fn handle_link_state(payload: &[u8]) {
        match decode_net_link_event(payload) {
            Ok(event) => {
                Self::apply_link_event(event);
            }
            Err(e) => {
                log::warn!(
                    "[remote_nic] link-state: bad NET_LINK_STATE payload: {:?}",
                    e,
                );
            }
        }
    }

    fn apply_link_event(event: NetLinkEvent) {
        log::info!(
            "[remote_nic] link-state: up={} speed={}Mbps mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            event.up,
            event.speed_mbps,
            event.mac[0],
            event.mac[1],
            event.mac[2],
            event.mac[3],
            event.mac[4],
            event.mac[5],
        );
        if let Some(ep) = Self::ensure_link_event_entry(
            event.mac,
            crate::ipc::registry::lookup_endpoint_id("net.nic"),
        ) {
            log::info!(
                "[remote_nic] link-state bootstrap registered ring-3 NIC driver: endpoint={:?} mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                ep,
                event.mac[0],
                event.mac[1],
                event.mac[2],
                event.mac[3],
                event.mac[4],
                event.mac[5],
            );
        }
        if !event.up {
            // One-line hook: link-down resets TCP retransmit timers per Phase 16.
            super::tcp::on_link_down();
        }
        crate::net::virtio_net::wake_net_task();
    }

    fn ensure_link_event_entry(
        mac: MacAddr,
        fallback_endpoint: Option<EndpointId>,
    ) -> Option<EndpointId> {
        let mut live_endpoint = None;
        {
            let mut slot = REMOTE_NIC.lock();
            if let Some(ep) = fallback_endpoint {
                if let Some(entry) = slot.as_mut() {
                    entry.endpoint = ep;
                    entry.mac = mac;
                } else {
                    *slot = Some(NicEntry {
                        endpoint: ep,
                        mac,
                        tx_queue: VecDeque::new(),
                    });
                }
                live_endpoint = Some(ep);
            } else if let Some(entry) = slot.as_mut() {
                entry.mac = mac;
            }
        }
        if live_endpoint.is_some() {
            REMOTE_NIC_REGISTERED.store(true, Ordering::Release);
            RESTART_SUSPECTED.store(false, Ordering::Release);
            ABSENT_WARN_EMITTED.store(false, Ordering::Relaxed);
            LOOKUP_MISS_COUNTER.store(0, Ordering::Relaxed);
        }
        live_endpoint
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reset_remote_nic_state() {
        *REMOTE_NIC.lock() = None;
        REMOTE_NIC_REGISTERED.store(false, Ordering::Release);
        RESTART_SUSPECTED.store(false, Ordering::Release);
        ABSENT_WARN_EMITTED.store(false, Ordering::Relaxed);
        LOOKUP_MISS_COUNTER.store(0, Ordering::Relaxed);
    }

    fn registered_endpoint() -> Option<EndpointId> {
        REMOTE_NIC.lock().as_ref().map(|entry| entry.endpoint)
    }

    #[test_case]
    fn link_event_bootstraps_missing_remote_nic_entry() {
        reset_remote_nic_state();

        let ep = EndpointId(9);
        let mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];

        assert_eq!(RemoteNic::ensure_link_event_entry(mac, Some(ep)), Some(ep));
        assert!(REMOTE_NIC_REGISTERED.load(Ordering::Acquire));
        assert_eq!(RemoteNic::mac_address(), Some(mac));
    }

    #[test_case]
    fn link_event_updates_existing_remote_nic_mac_in_place() {
        reset_remote_nic_state();
        RemoteNic::register(EndpointId(3), [0; 6]);

        let mac = [0x52, 0x54, 0x00, 0xaa, 0xbb, 0xcc];
        assert_eq!(RemoteNic::ensure_link_event_entry(mac, None), None);
        assert_eq!(RemoteNic::mac_address(), Some(mac));
    }

    #[test_case]
    fn link_event_recovers_restart_suspected_slot_with_live_endpoint() {
        reset_remote_nic_state();
        RemoteNic::register(EndpointId(3), [0; 6]);
        RemoteNic::on_ipc_error();

        let recovered_ep = EndpointId(7);
        let mac = [0x52, 0x54, 0x00, 0xde, 0xad, 0xbe];
        assert_eq!(
            RemoteNic::ensure_link_event_entry(mac, Some(recovered_ep)),
            Some(recovered_ep)
        );
        assert!(!RESTART_SUSPECTED.load(Ordering::Acquire));
        assert_eq!(registered_endpoint(), Some(recovered_ep));
        assert_eq!(RemoteNic::mac_address(), Some(mac));
    }
}
