//! Phase 69d follow-up — pure-logic msghdr / cmsg codec.
//!
//! Mirrors the Linux x86_64 layout for `struct msghdr`, `struct iovec`,
//! and the cmsg ancillary-data chain. All parsing is byte-oriented and
//! never dereferences user pointers — the caller is responsible for
//! copying through the user-range API before invoking these helpers.
//!
//! The codec is `no_std` and host-testable; it backs `sys_sendmsg` /
//! `sys_recvmsg` in the x86_64 syscall layer.

use alloc::vec::Vec;

// ===========================================================================
// Linux x86_64 sizes / alignment constants
// ===========================================================================

/// `sizeof(struct msghdr)` on Linux x86_64.
pub const MSGHDR_SIZE: usize = 56;

/// `sizeof(struct iovec)` on Linux x86_64.
pub const IOVEC_SIZE: usize = 16;

/// `sizeof(struct cmsghdr)` on Linux x86_64.
pub const CMSGHDR_SIZE: usize = 16;

/// `SOL_SOCKET` constant (cmsg_level for socket-layer ancillary data).
pub const SOL_SOCKET: i32 = 1;

/// `SCM_RIGHTS` cmsg_type — payload is an array of `i32` file descriptors.
pub const SCM_RIGHTS: i32 = 1;

/// Maximum file descriptors per `SCM_RIGHTS` cmsg accepted by the kernel.
/// Linux caps this at `SCM_MAX_FD = 253`; we pick a smaller bound that's
/// still comfortably above tmux's requirement (one or two fds per call).
pub const SCM_MAX_FD: usize = 64;

/// `MSG_TRUNC` flag — recv message was truncated to fit the buffer.
pub const MSG_TRUNC: i32 = 0x20;

/// `MSG_CTRUNC` flag — control buffer was too small to hold all cmsgs.
pub const MSG_CTRUNC: i32 = 0x08;

/// `MSG_PEEK` flag — recvmsg returns data without consuming it.
pub const MSG_PEEK: i32 = 0x02;

/// `MSG_DONTWAIT` flag — operate in non-blocking mode for this call.
pub const MSG_DONTWAIT: i32 = 0x40;

/// `MSG_CMSG_CLOEXEC` flag — set `FD_CLOEXEC` on every fd installed
/// by `SCM_RIGHTS` on the receiver side.  Without this flag the
/// installed fds have cloexec cleared, matching Linux's `recvmsg(2)`
/// contract regardless of what the sender's fd held.
pub const MSG_CMSG_CLOEXEC: i32 = 0x4000_0000_u32 as i32;

/// Bits the kernel knows how to honour on `sendmsg(2)`.  Today only
/// `MSG_DONTWAIT`, which the syscall layer ORs into its per-call
/// `force_nonblock` so a blocking fd can still request nonblocking
/// semantics for one send.  Other bits cause the kernel to reject the
/// call with `EOPNOTSUPP` so callers learn that requested semantics
/// were not applied.
pub const SENDMSG_SUPPORTED_FLAGS: i32 = MSG_DONTWAIT;

/// Bits the kernel knows how to honour on `recvmsg(2)`.
pub const RECVMSG_SUPPORTED_FLAGS: i32 = MSG_DONTWAIT | MSG_CMSG_CLOEXEC;

// ===========================================================================
// Alignment helpers (mirror the kernel CMSG_* macros)
// ===========================================================================

/// `CMSG_ALIGN(n) = round up n to the next multiple of sizeof(size_t)`.
pub const fn cmsg_align(len: usize) -> usize {
    (len + 7) & !7
}

/// `CMSG_LEN(payload)` = header bytes + payload bytes (caller-reported len).
pub const fn cmsg_len(payload: usize) -> usize {
    cmsg_align(CMSGHDR_SIZE) + payload
}

/// `CMSG_SPACE(payload)` = bytes one cmsg consumes in the control buffer,
/// including alignment padding so the next header starts cleanly.
pub const fn cmsg_space(payload: usize) -> usize {
    cmsg_align(CMSGHDR_SIZE) + cmsg_align(payload)
}

// ===========================================================================
// MsgHdr — parsed view of `struct msghdr`
// ===========================================================================

/// Decoded `struct msghdr` from user memory.  Pointers are kept as raw
/// user addresses so the syscall layer can decide how to copy each iov
/// without re-parsing the header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MsgHdr {
    pub msg_name: u64,
    pub msg_namelen: u32,
    pub msg_iov: u64,
    pub msg_iovlen: u64,
    pub msg_control: u64,
    pub msg_controllen: u64,
    pub msg_flags: i32,
}

impl MsgHdr {
    /// Decode 56 bytes copied from user space.
    ///
    /// Returns `None` only on length mismatch; field values are not
    /// otherwise validated (the syscall layer enforces bounds when it
    /// copies the underlying buffers).
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < MSGHDR_SIZE {
            return None;
        }
        let msg_name = u64::from_le_bytes(bytes[0..8].try_into().ok()?);
        let msg_namelen = u32::from_le_bytes(bytes[8..12].try_into().ok()?);
        // bytes 12..16: padding
        let msg_iov = u64::from_le_bytes(bytes[16..24].try_into().ok()?);
        let msg_iovlen = u64::from_le_bytes(bytes[24..32].try_into().ok()?);
        let msg_control = u64::from_le_bytes(bytes[32..40].try_into().ok()?);
        let msg_controllen = u64::from_le_bytes(bytes[40..48].try_into().ok()?);
        let msg_flags = i32::from_le_bytes(bytes[48..52].try_into().ok()?);
        // bytes 52..56: trailing padding
        Some(Self {
            msg_name,
            msg_namelen,
            msg_iov,
            msg_iovlen,
            msg_control,
            msg_controllen,
            msg_flags,
        })
    }

    /// Re-encode this header, primarily so `recvmsg` can write back the
    /// returned `msg_controllen` and `msg_flags` fields.
    pub fn encode(&self) -> [u8; MSGHDR_SIZE] {
        let mut out = [0u8; MSGHDR_SIZE];
        out[0..8].copy_from_slice(&self.msg_name.to_le_bytes());
        out[8..12].copy_from_slice(&self.msg_namelen.to_le_bytes());
        out[16..24].copy_from_slice(&self.msg_iov.to_le_bytes());
        out[24..32].copy_from_slice(&self.msg_iovlen.to_le_bytes());
        out[32..40].copy_from_slice(&self.msg_control.to_le_bytes());
        out[40..48].copy_from_slice(&self.msg_controllen.to_le_bytes());
        out[48..52].copy_from_slice(&self.msg_flags.to_le_bytes());
        out
    }
}

// ===========================================================================
// IoVec — parsed view of `struct iovec`
// ===========================================================================

/// Decoded `struct iovec` (base pointer + length).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoVec {
    pub iov_base: u64,
    pub iov_len: u64,
}

impl IoVec {
    /// Decode a single iovec from 16 bytes.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < IOVEC_SIZE {
            return None;
        }
        let iov_base = u64::from_le_bytes(bytes[0..8].try_into().ok()?);
        let iov_len = u64::from_le_bytes(bytes[8..16].try_into().ok()?);
        Some(Self { iov_base, iov_len })
    }

    /// Decode an array of `count` iovecs from a flat byte buffer.
    pub fn decode_array(bytes: &[u8], count: usize) -> Option<Vec<IoVec>> {
        if bytes.len() < count.checked_mul(IOVEC_SIZE)? {
            return None;
        }
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            out.push(Self::decode(&bytes[i * IOVEC_SIZE..(i + 1) * IOVEC_SIZE])?);
        }
        Some(out)
    }

    /// Sum the byte counts across all iovecs, saturating on overflow.
    pub fn total_len(slice: &[IoVec]) -> u64 {
        slice
            .iter()
            .fold(0u64, |acc, v| acc.saturating_add(v.iov_len))
    }
}

// ===========================================================================
// CmsgHdr — parsed view of one cmsghdr
// ===========================================================================

/// Decoded `struct cmsghdr`.  `payload` borrows the slice of bytes that
/// immediately follow the header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmsgView<'a> {
    pub cmsg_len: usize,
    pub cmsg_level: i32,
    pub cmsg_type: i32,
    pub payload: &'a [u8],
}

impl<'a> CmsgView<'a> {
    /// Decode the cmsg starting at `bytes[0]`. Returns the parsed view
    /// plus the offset of the next cmsg.  Returns `None` on header
    /// truncation, payload truncation, or invalid `cmsg_len`.
    pub fn decode(bytes: &'a [u8]) -> Option<(CmsgView<'a>, usize)> {
        if bytes.len() < CMSGHDR_SIZE {
            return None;
        }
        let cmsg_len = u64::from_le_bytes(bytes[0..8].try_into().ok()?) as usize;
        let cmsg_level = i32::from_le_bytes(bytes[8..12].try_into().ok()?);
        let cmsg_type = i32::from_le_bytes(bytes[12..16].try_into().ok()?);
        if cmsg_len < CMSGHDR_SIZE || cmsg_len > bytes.len() {
            return None;
        }
        let payload = &bytes[CMSGHDR_SIZE..cmsg_len];
        let next = cmsg_align(cmsg_len).min(bytes.len());
        Some((
            CmsgView {
                cmsg_len,
                cmsg_level,
                cmsg_type,
                payload,
            },
            next,
        ))
    }

    /// True when this cmsg carries a `SOL_SOCKET / SCM_RIGHTS` fd list.
    pub fn is_scm_rights(&self) -> bool {
        self.cmsg_level == SOL_SOCKET && self.cmsg_type == SCM_RIGHTS
    }

    /// Decode the SCM_RIGHTS payload as a list of i32 file descriptors.
    /// Returns `None` if `is_scm_rights()` is false, the payload length
    /// is not a multiple of 4, or it exceeds `SCM_MAX_FD`.
    pub fn scm_rights_fds(&self) -> Option<Vec<i32>> {
        if !self.is_scm_rights() {
            return None;
        }
        if !self.payload.len().is_multiple_of(4) {
            return None;
        }
        let count = self.payload.len() / 4;
        if count > SCM_MAX_FD {
            return None;
        }
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let bytes = &self.payload[i * 4..(i + 1) * 4];
            out.push(i32::from_le_bytes(bytes.try_into().ok()?));
        }
        Some(out)
    }
}

/// Reason a control buffer failed to parse end-to-end.  The kernel
/// surface translates these to `-EINVAL` so a malformed `sendmsg`
/// control buffer fails the syscall instead of silently dropping the
/// truncated tail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmsgWalkError {
    /// A header lies on a cursor that has fewer than `CMSGHDR_SIZE`
    /// bytes remaining — and the cursor is not exactly at the end of
    /// the buffer (zero-length tail is fine).
    TruncatedHeader,
    /// `cmsghdr::cmsg_len` declares a payload that runs past the end
    /// of the supplied buffer, or is shorter than the header itself.
    InvalidLength,
}

/// Walk every cmsg in the control buffer and emit each view to the
/// visitor.  Returns `Ok(())` only when the entire buffer parsed
/// cleanly.  Stops on the first malformed header and reports `Err`.
pub fn try_for_each_cmsg<'a, F>(control: &'a [u8], mut visitor: F) -> Result<(), CmsgWalkError>
where
    F: FnMut(CmsgView<'a>),
{
    let mut cursor = 0usize;
    while cursor < control.len() {
        if cursor + CMSGHDR_SIZE > control.len() {
            return Err(CmsgWalkError::TruncatedHeader);
        }
        let remaining = &control[cursor..];
        let Some((view, next)) = CmsgView::decode(remaining) else {
            return Err(CmsgWalkError::InvalidLength);
        };
        visitor(view);
        if next == 0 {
            // `next == 0` only happens on `cmsg_align(0)` which cannot
            // occur because `cmsg_len >= CMSGHDR_SIZE = 16`; defensive
            // break to keep the loop bounded.
            break;
        }
        cursor += next;
    }
    Ok(())
}

/// Walk every cmsg and silently ignore parse failures.  Kept for
/// existing call sites that intentionally tolerate truncation; new code
/// should prefer [`try_for_each_cmsg`].
pub fn for_each_cmsg<'a, F>(control: &'a [u8], visitor: F)
where
    F: FnMut(CmsgView<'a>),
{
    let _ = try_for_each_cmsg(control, visitor);
}

// ===========================================================================
// Encoder — produce SCM_RIGHTS reply cmsgs
// ===========================================================================

/// Encode a single `SCM_RIGHTS` cmsg into `dst` at offset 0.  Returns the
/// number of bytes written (including alignment padding) or `None` when
/// `dst` is too small.
pub fn encode_scm_rights(dst: &mut [u8], fds: &[i32]) -> Option<usize> {
    let payload_len = fds.len().checked_mul(4)?;
    let total_len = cmsg_len(payload_len);
    let space = cmsg_space(payload_len);
    if dst.len() < space {
        return None;
    }
    dst[0..8].copy_from_slice(&(total_len as u64).to_le_bytes());
    dst[8..12].copy_from_slice(&SOL_SOCKET.to_le_bytes());
    dst[12..16].copy_from_slice(&SCM_RIGHTS.to_le_bytes());
    for (i, &fd) in fds.iter().enumerate() {
        let off = CMSGHDR_SIZE + i * 4;
        dst[off..off + 4].copy_from_slice(&fd.to_le_bytes());
    }
    // Zero alignment tail.
    for byte in dst.iter_mut().take(space).skip(total_len) {
        *byte = 0;
    }
    Some(space)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn cmsg_align_rounds_up_to_eight() {
        assert_eq!(cmsg_align(0), 0);
        assert_eq!(cmsg_align(1), 8);
        assert_eq!(cmsg_align(7), 8);
        assert_eq!(cmsg_align(8), 8);
        assert_eq!(cmsg_align(9), 16);
        assert_eq!(cmsg_align(15), 16);
        assert_eq!(cmsg_align(16), 16);
    }

    #[test]
    fn cmsg_len_and_space_match_linux() {
        // One fd: cmsg_len = 16 + 4 = 20, cmsg_space = 16 + 8 = 24
        assert_eq!(cmsg_len(4), 20);
        assert_eq!(cmsg_space(4), 24);
        // Two fds: 16 + 8 = 24, 16 + 8 = 24
        assert_eq!(cmsg_len(8), 24);
        assert_eq!(cmsg_space(8), 24);
        // Three fds: 16 + 12 = 28, 16 + 16 = 32
        assert_eq!(cmsg_len(12), 28);
        assert_eq!(cmsg_space(12), 32);
    }

    fn round_trip_msghdr() -> MsgHdr {
        MsgHdr {
            msg_name: 0xCAFE_BABE_0000_1000,
            msg_namelen: 16,
            msg_iov: 0x0000_7FFF_DEAD_BEE0,
            msg_iovlen: 2,
            msg_control: 0x0000_7FFF_C0DE_0000,
            msg_controllen: 24,
            msg_flags: 0,
        }
    }

    #[test]
    fn msghdr_round_trip() {
        let h = round_trip_msghdr();
        let bytes = h.encode();
        assert_eq!(bytes.len(), MSGHDR_SIZE);
        let decoded = MsgHdr::decode(&bytes).expect("decode");
        assert_eq!(decoded, h);
    }

    #[test]
    fn msghdr_decode_rejects_short_buffer() {
        let short = [0u8; MSGHDR_SIZE - 1];
        assert!(MsgHdr::decode(&short).is_none());
    }

    #[test]
    fn iovec_array_round_trip() {
        let entries = [
            IoVec {
                iov_base: 0x1000,
                iov_len: 4,
            },
            IoVec {
                iov_base: 0x2000,
                iov_len: 8,
            },
        ];
        let mut raw = vec![0u8; IOVEC_SIZE * entries.len()];
        for (i, v) in entries.iter().enumerate() {
            raw[i * IOVEC_SIZE..i * IOVEC_SIZE + 8].copy_from_slice(&v.iov_base.to_le_bytes());
            raw[i * IOVEC_SIZE + 8..(i + 1) * IOVEC_SIZE].copy_from_slice(&v.iov_len.to_le_bytes());
        }
        let parsed = IoVec::decode_array(&raw, entries.len()).expect("decode");
        assert_eq!(parsed, entries.as_slice());
        assert_eq!(IoVec::total_len(&parsed), 12);
    }

    #[test]
    fn scm_rights_round_trip_one_fd() {
        let mut buf = [0u8; cmsg_space(4)];
        let n = encode_scm_rights(&mut buf, &[7]).expect("encode");
        assert_eq!(n, 24);

        let (view, next) = CmsgView::decode(&buf).expect("decode");
        assert!(view.is_scm_rights());
        assert_eq!(view.cmsg_len, 20);
        assert_eq!(view.payload, &7i32.to_le_bytes()[..]);
        // For a single fd the next offset is the aligned cmsg space = 24.
        assert_eq!(next, 24);
        assert_eq!(view.scm_rights_fds().expect("fds"), vec![7]);
    }

    #[test]
    fn scm_rights_round_trip_multiple_fds() {
        let mut buf = [0u8; cmsg_space(12)];
        let fds = [3i32, 4, 5];
        encode_scm_rights(&mut buf, &fds).expect("encode");
        let (view, _) = CmsgView::decode(&buf).expect("decode");
        assert_eq!(view.scm_rights_fds().expect("fds"), fds.to_vec());
    }

    #[test]
    fn cmsg_decode_rejects_truncated_payload() {
        let mut buf = [0u8; CMSGHDR_SIZE];
        // Declare cmsg_len = 32 (header + 16 bytes of payload), but the
        // buffer only contains the header — must fail.
        buf[0..8].copy_from_slice(&32u64.to_le_bytes());
        buf[8..12].copy_from_slice(&SOL_SOCKET.to_le_bytes());
        buf[12..16].copy_from_slice(&SCM_RIGHTS.to_le_bytes());
        assert!(CmsgView::decode(&buf).is_none());
    }

    #[test]
    fn cmsg_decode_rejects_undersize_cmsg_len() {
        let mut buf = [0u8; CMSGHDR_SIZE];
        // cmsg_len = 8 < CMSGHDR_SIZE — invalid.
        buf[0..8].copy_from_slice(&8u64.to_le_bytes());
        assert!(CmsgView::decode(&buf).is_none());
    }

    #[test]
    fn for_each_cmsg_walks_two_entries() {
        let mut buf = vec![0u8; cmsg_space(4) + cmsg_space(8)];
        let n1 = encode_scm_rights(&mut buf[..cmsg_space(4)], &[10]).expect("encode 1");
        let n2 = encode_scm_rights(&mut buf[n1..], &[20, 21]).expect("encode 2");
        let _ = n2; // suppress unused warning; total length is cmsg_space(4)+cmsg_space(8)

        let mut seen = Vec::new();
        for_each_cmsg(&buf, |view| {
            if let Some(fds) = view.scm_rights_fds() {
                seen.extend(fds);
            }
        });
        assert_eq!(seen, vec![10, 20, 21]);
    }

    #[test]
    fn scm_rights_rejects_misaligned_payload() {
        let mut buf = [0u8; cmsg_space(3)];
        // Pretend cmsg_len = 16 + 3 = 19 (not divisible by 4).
        buf[0..8].copy_from_slice(&19u64.to_le_bytes());
        buf[8..12].copy_from_slice(&SOL_SOCKET.to_le_bytes());
        buf[12..16].copy_from_slice(&SCM_RIGHTS.to_le_bytes());
        let (view, _) = CmsgView::decode(&buf).expect("decode");
        assert!(view.scm_rights_fds().is_none());
    }

    #[test]
    fn scm_rights_rejects_too_many_fds() {
        let too_many = SCM_MAX_FD + 1;
        let payload_bytes = too_many * 4;
        let len = cmsg_len(payload_bytes);
        let mut buf = vec![0u8; cmsg_space(payload_bytes)];
        buf[0..8].copy_from_slice(&(len as u64).to_le_bytes());
        buf[8..12].copy_from_slice(&SOL_SOCKET.to_le_bytes());
        buf[12..16].copy_from_slice(&SCM_RIGHTS.to_le_bytes());
        let (view, _) = CmsgView::decode(&buf).expect("decode");
        assert!(view.scm_rights_fds().is_none());
    }
}
