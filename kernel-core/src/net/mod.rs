pub mod arp;
pub mod ethernet;
pub mod icmp;
pub mod ipv4;
pub mod tcp;
pub mod udp;

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

#[cfg(test)]
mod tests {
    use super::SockaddrIn;
    use core::mem;

    #[test]
    fn sockaddr_in_size() {
        assert_eq!(mem::size_of::<SockaddrIn>(), 16);
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
            sin_addr: u32::from_be_bytes([10, 0, 2, 15]),
            sin_zero: [0; 8],
        };
        assert_eq!(addr.sin_family, 2);
        assert_eq!(u16::from_be(addr.sin_port), 80);
        assert_eq!(addr.sin_addr.to_be_bytes(), [10, 0, 2, 15]);
    }
}
