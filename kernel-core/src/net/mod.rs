pub mod arp;
pub mod ethernet;
pub mod icmp;
pub mod ipv4;
pub mod ipv6;
pub mod msghdr;
pub mod tcp;
pub mod udp;
pub mod udp_protocol;

// ===========================================================================
// Phase 23: SockaddrIn ABI layout tests
// ===========================================================================

/// Mirrors the Linux `struct sockaddr_in` layout for ABI compatibility testing.
#[repr(C)]
pub struct SockaddrIn {
    pub sin_family: u16,
    pub sin_port: u16,
    pub sin_addr: u32,
    pub sin_zero: [u8; 8],
}

// ===========================================================================
// Phase 91: SockaddrIn6 ABI layout (mirrors musl `struct sockaddr_in6`, 28 bytes)
// ===========================================================================

/// Mirrors the Linux/musl `struct sockaddr_in6` layout for ABI compatibility.
/// Duplicated (not shared) with `userspace/syscall-lib`'s `SockaddrIn6`, exactly
/// as `SockaddrIn` is — these offset tests prove the two agree byte-for-byte.
#[repr(C)]
pub struct SockaddrIn6 {
    pub sin6_family: u16,
    pub sin6_port: u16,
    pub sin6_flowinfo: u32,
    pub sin6_addr: [u8; 16],
    pub sin6_scope_id: u32,
}

#[cfg(test)]
mod tests {
    use super::{SockaddrIn, SockaddrIn6};
    use core::mem;

    #[test]
    fn sockaddr_in_size() {
        assert_eq!(mem::size_of::<SockaddrIn>(), 16);
    }

    #[test]
    fn sockaddr_in6_size() {
        assert_eq!(mem::size_of::<SockaddrIn6>(), 28);
    }

    #[test]
    fn sockaddr_in6_field_offsets() {
        // Must match musl: family@0, port@2, flowinfo@4, addr@8, scope_id@24.
        assert_eq!(mem::offset_of!(SockaddrIn6, sin6_family), 0);
        assert_eq!(mem::offset_of!(SockaddrIn6, sin6_port), 2);
        assert_eq!(mem::offset_of!(SockaddrIn6, sin6_flowinfo), 4);
        assert_eq!(mem::offset_of!(SockaddrIn6, sin6_addr), 8);
        assert_eq!(mem::offset_of!(SockaddrIn6, sin6_scope_id), 24);
    }

    #[test]
    fn sockaddr_in6_network_byte_order() {
        let addr = SockaddrIn6 {
            sin6_family: 10, // AF_INET6
            sin6_port: 53u16.to_be(),
            sin6_flowinfo: 0,
            sin6_addr: [
                0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0x50, 0x54, 0x00, 0xff, 0xfe, 0x12, 0x34, 0x56,
            ],
            sin6_scope_id: 0,
        };
        assert_eq!(addr.sin6_family, 10);
        assert_eq!(addr.sin6_port.to_ne_bytes(), 53u16.to_be_bytes());
        assert_eq!(addr.sin6_addr[0], 0xfe);
    }

    #[test]
    fn sockaddr_in_field_offsets() {
        let base = 0usize;
        // sin_family at offset 0
        assert_eq!(mem::offset_of!(SockaddrIn, sin_family), base);
        // sin_port at offset 2
        assert_eq!(mem::offset_of!(SockaddrIn, sin_port), 2);
        // sin_addr at offset 4
        assert_eq!(mem::offset_of!(SockaddrIn, sin_addr), 4);
        // sin_zero at offset 8
        assert_eq!(mem::offset_of!(SockaddrIn, sin_zero), 8);
    }

    #[test]
    fn sockaddr_in_network_byte_order() {
        let addr = SockaddrIn {
            sin_family: 2, // AF_INET
            sin_port: 80u16.to_be(),
            sin_addr: u32::from_ne_bytes([10, 0, 2, 15]),
            sin_zero: [0; 8],
        };
        assert_eq!(addr.sin_family, 2);
        // In-memory bytes must be network order
        assert_eq!(addr.sin_port.to_ne_bytes(), 80u16.to_be_bytes());
        assert_eq!(addr.sin_addr.to_ne_bytes(), [10, 0, 2, 15]);
    }
}
