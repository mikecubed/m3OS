//! UDP service IPC protocol — Phase 54 Track C.
//!
//! Defines the message labels and data layout shared between the kernel
//! syscall handler (IPC client on behalf of apps) and the ring-3
//! `net_server` process.
//!
//! # Design
//!
//! The service owns UDP **policy**: port binding table, socket state,
//! send/recv validation.  The kernel retains **mechanism**: packet I/O
//! via virtio-net, per-port datagram queues, wait-queue wakeup, and
//! user-buffer copy.
//!
//! # Operation labels
//!
//! | Label | Operation | Request | Reply |
//! |---|---|---|---|
//! | [`NET_UDP_CREATE`] | Create UDP handle | data\[0\]=kernel socket handle | label=0, data\[0\]=service handle |
//! | [`NET_UDP_BIND`] | Bind port | data\[0\]=handle, data\[1\]=port, data\[2\]=ip_u32 | label=0 or errno |
//! | [`NET_UDP_CONNECT`] | Set peer | data\[0\]=handle, data\[1\]=ip_port packed | label=0, data\[0\]=ephemeral_port (if auto-bound) |
//! | [`NET_UDP_SENDTO`] | Validate send | data\[0\]=handle, data\[1\]=dst_ip_port packed, data\[2\]=len | label=0, data\[0\]=src_port |
//! | [`NET_UDP_RECVFROM`] | Validate recv | data\[0\]=handle | label=0, data\[0\]=local_port |
//! | [`NET_UDP_CLOSE`] | Close handle | data\[0\]=handle | label=0 |

/// Create a new UDP socket handle in the service.
///
/// Request: `data[0]` = kernel socket-table index.
/// Reply:   label = 0, `data[0]` = service handle (same value echoed back).
pub const NET_UDP_CREATE: u64 = 100;

/// Bind a UDP port.
///
/// Request: `data[0]` = handle, `data[1]` = port (u16), `data[2]` = local IPv4 as u32.
/// Reply:   label = 0 on success, negative errno on error.
pub const NET_UDP_BIND: u64 = 101;

/// Set the default peer address (connect semantics for UDP).
///
/// Request: `data[0]` = handle, `data[1]` = `(ip_u32 as u64) << 16 | port`.
/// Reply:   label = 0, `data[0]` = ephemeral src port (0 if already bound).
pub const NET_UDP_CONNECT: u64 = 102;

/// Validate a sendto operation.  The kernel provides the destination;
/// the service checks policy and replies with the source port to use.
///
/// Request: `data[0]` = handle, `data[1]` = `(dst_ip_u32 as u64) << 16 | dst_port`,
///          `data[2]` = payload length.
/// Reply:   label = 0, `data[0]` = src_port to transmit from.
///          label = negative errno on error.
pub const NET_UDP_SENDTO: u64 = 103;

/// Validate a recvfrom operation.  The service replies with the local
/// port so the kernel can dequeue from its mechanism-layer queue.
///
/// Request: `data[0]` = handle.
/// Reply:   label = 0, `data[0]` = local_port.
///          label = negative errno on error.
pub const NET_UDP_RECVFROM: u64 = 104;

/// Close a UDP handle.  The service unbinds the port if bound.
///
/// Request: `data[0]` = handle.
/// Reply:   label = 0, `data[0]` = port that was unbound (0 if none).
pub const NET_UDP_CLOSE: u64 = 105;

/// Maximum UDP payload forwarded through a single IPC exchange.
pub const NET_UDP_MAX_PAYLOAD: usize = 4096;

// ---- Packing helpers for ip+port into a single u64 ----

/// Pack an IPv4 address (as `[u8; 4]`) and a port into one `u64`.
///
/// Layout: `bits[63:16]` = IPv4 (big-endian u32 zero-extended),
///         `bits[15:0]`  = port.
#[inline]
pub const fn pack_ip_port(ip: [u8; 4], port: u16) -> u64 {
    let ip_u32 =
        ((ip[0] as u32) << 24) | ((ip[1] as u32) << 16) | ((ip[2] as u32) << 8) | (ip[3] as u32);
    ((ip_u32 as u64) << 16) | (port as u64)
}

/// Unpack an IPv4 address and port from a single `u64`.
#[inline]
pub const fn unpack_ip_port(packed: u64) -> ([u8; 4], u16) {
    let port = (packed & 0xFFFF) as u16;
    let ip_u32 = (packed >> 16) as u32;
    let ip = [
        (ip_u32 >> 24) as u8,
        (ip_u32 >> 16) as u8,
        (ip_u32 >> 8) as u8,
        ip_u32 as u8,
    ];
    (ip, port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_distinct() {
        let labels = [
            NET_UDP_CREATE,
            NET_UDP_BIND,
            NET_UDP_CONNECT,
            NET_UDP_SENDTO,
            NET_UDP_RECVFROM,
            NET_UDP_CLOSE,
        ];
        for (i, a) in labels.iter().enumerate() {
            for (j, b) in labels.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "labels[{i}] == labels[{j}]");
                }
            }
        }
    }

    #[test]
    fn labels_do_not_collide_with_vfs() {
        // VFS labels are 10..17; UDP labels start at 100.
        assert!(NET_UDP_CREATE >= 100);
    }

    #[test]
    fn pack_unpack_round_trip() {
        let ip = [10, 0, 2, 15];
        let port = 5353;
        let packed = pack_ip_port(ip, port);
        let (ip2, port2) = unpack_ip_port(packed);
        assert_eq!(ip, ip2);
        assert_eq!(port, port2);
    }

    #[test]
    fn pack_unpack_zeros() {
        let (ip, port) = unpack_ip_port(pack_ip_port([0, 0, 0, 0], 0));
        assert_eq!(ip, [0, 0, 0, 0]);
        assert_eq!(port, 0);
    }

    #[test]
    fn pack_unpack_max_values() {
        let ip = [255, 255, 255, 255];
        let port = 65535;
        let packed = pack_ip_port(ip, port);
        let (ip2, port2) = unpack_ip_port(packed);
        assert_eq!(ip, ip2);
        assert_eq!(port, port2);
    }

    #[test]
    fn max_payload_is_block_aligned() {
        assert!(NET_UDP_MAX_PAYLOAD > 0);
        assert_eq!(NET_UDP_MAX_PAYLOAD % 512, 0);
    }
}
