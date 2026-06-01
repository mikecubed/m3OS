//! Audio PCM shared-ring transport — Phase 80 Track A.2.
//!
//! Carries bulk PCM across the `audio_server` → audio-driver seam **out of**
//! the IPC message body (per the AGENTS.md rule "bulk data: page capability
//! grants, never IPC payloads"). The bytes live in a persistent
//! page-capability-backed shared region established once at stream open via
//! `sys_shm_*` (`kernel/src/mm/shm.rs` — the same primitive `display_server`
//! uses for surface buffers); each [`AudioRequest::SubmitFrames`] carries only
//! an `offset`/`len` window into that region, never samples.
//!
//! # Why shm rather than a per-submission page-grant
//!
//! A streaming audio period loop (~100 Hz) is a producer/consumer ring, not a
//! one-shot ownership handoff. `sys_page_grant_*` is a *move* primitive: it
//! unmaps the sender's pages and maps them at a fresh receiver VA with no
//! release path — re-granting every period would churn both address spaces
//! and leak frames/VA. A persistent shared ring (mapped once, reused every
//! period, refcounted teardown on `CloseStream`/exit) is the primitive every
//! real audio stack uses (PulseAudio/PipeWire mmap, CoreAudio, WASAPI) and is
//! the alternative the Phase 80 design doc explicitly sanctions. The driver
//! still **copies** each window into its own `sys_device_dma_alloc`
//! IOMMU-domain `DmaBuffer` before programming the controller, so IOMMU
//! isolation is preserved exactly as in the grant design.
//!
//! # Host-testable core
//!
//! [`copy_window`] and the bounds check are pure and exercised on the host
//! (`audio_pcm::tests::submission_bounds`); the shm-touching [`PcmRing`] /
//! [`PcmReceiver`] methods are production-only (`cfg(not(test))`).

use kernel_core::driver_ipc::audio::submission_in_bounds;

/// Size of the shared PCM ring, in bytes. A multiple of the page size and
/// `>= kernel_core::audio::MAX_SUBMIT_BYTES` (64 KiB) so the largest single
/// `SubmitFrames` window always fits. Both the sender ([`PcmRing`]) and the
/// receiver ([`PcmReceiver`]) agree on this size as the authoritative
/// region length for bounds checks — `sys_shm_map` does not report the
/// mapped length, so this constant is the shared anchor.
pub const PCM_RING_BYTES: usize = 64 * 1024;

/// Failure modes of the PCM shared-ring transport.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum AudioTransportError {
    /// `sys_shm_create` returned the `0` error sentinel.
    ShmCreate,
    /// `sys_shm_map` returned the `0` error sentinel.
    ShmMap,
    /// The `[offset, offset+len)` window does not fit inside the ring.
    OutOfBounds,
    /// The submission is larger than the destination buffer can hold.
    DestTooSmall,
}

// ---------------------------------------------------------------------------
// Pure window copy — host-testable, no syscalls
// ---------------------------------------------------------------------------

/// Copy `[offset, offset+len)` out of the shared `region` into `dst`.
///
/// Rejects (without copying) any window that is zero-length, overflows, or
/// spills past the granted region (via
/// [`kernel_core::driver_ipc::audio::submission_in_bounds`]), and any window
/// larger than `dst`. Returns the number of bytes copied on success.
pub fn copy_window(
    region: &[u8],
    offset: u32,
    len: u32,
    dst: &mut [u8],
) -> Result<usize, AudioTransportError> {
    if !submission_in_bounds(offset, len, region.len()) {
        return Err(AudioTransportError::OutOfBounds);
    }
    let (o, l) = (offset as usize, len as usize);
    if dst.len() < l {
        return Err(AudioTransportError::DestTooSmall);
    }
    dst[..l].copy_from_slice(&region[o..o + l]);
    Ok(l)
}

// ---------------------------------------------------------------------------
// Sender side — PcmRing (used by audio_server's AudioProxyBackend)
// ---------------------------------------------------------------------------

/// Sender-side handle to the shared PCM ring. Created once per stream by
/// `audio_server`; each submission [`stage`](PcmRing::stage)s the mixed PCM
/// into the ring and returns the `(offset, len)` window to put in the
/// `SubmitFrames` message.
#[cfg(not(test))]
pub struct PcmRing {
    shm_id: u32,
    base: u64,
    len: usize,
    cursor: usize,
}

#[cfg(not(test))]
impl PcmRing {
    /// Create + map a fresh [`PCM_RING_BYTES`] shared ring.
    pub fn create() -> Result<Self, AudioTransportError> {
        let shm_id = syscall_lib::shm_create(PCM_RING_BYTES);
        if shm_id == 0 {
            return Err(AudioTransportError::ShmCreate);
        }
        let base = syscall_lib::shm_map(shm_id);
        if base == 0 {
            let _ = syscall_lib::shm_destroy(shm_id);
            return Err(AudioTransportError::ShmMap);
        }
        Ok(Self {
            shm_id,
            base,
            len: PCM_RING_BYTES,
            cursor: 0,
        })
    }

    /// The shared-region id to put in `SubmitFrames { grant_handle }` so the
    /// driver can map the same region.
    pub fn shm_id(&self) -> u32 {
        self.shm_id
    }

    /// Copy `bytes` into the ring and return the `(offset, len)` window. The
    /// cursor rolls and wraps to 0 when the next submission would not fit
    /// contiguously; the synchronous `SubmitFrames` call guarantees the
    /// driver has copied the previous window out before this one is staged,
    /// so a single ring slot is sufficient and wrapping never clobbers an
    /// in-flight window.
    pub fn stage(&mut self, bytes: &[u8]) -> Result<(u32, u32), AudioTransportError> {
        let n = bytes.len();
        if n > self.len {
            return Err(AudioTransportError::DestTooSmall);
        }
        if self.cursor + n > self.len {
            self.cursor = 0;
        }
        let off = self.cursor;
        // SAFETY: `[base+off, base+off+n)` lies inside the mapped shm region
        // (`off + n <= self.len` by the wrap above, and `self.len` bytes are
        // mapped at `base`).
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), (self.base as *mut u8).add(off), n);
        }
        self.cursor += n;
        Ok((off as u32, n as u32))
    }
}

#[cfg(not(test))]
impl Drop for PcmRing {
    fn drop(&mut self) {
        // Unmap our view and drop the creator reference; the region is freed
        // once the driver's mapping is also dropped (refcounted in
        // `kernel/src/mm/shm.rs`).
        let _ = syscall_lib::shm_unmap(self.base);
        let _ = syscall_lib::shm_destroy(self.shm_id);
    }
}

// ---------------------------------------------------------------------------
// Receiver side — PcmReceiver (used by the ac97 / hda drivers)
// ---------------------------------------------------------------------------

/// Receiver-side helper: maps a shared PCM ring on demand (caching the most
/// recent mapping) and copies each submission window into the driver's own
/// DMA buffer. Re-maps transparently when the sender presents a new region id
/// (stream reconnect).
#[cfg(not(test))]
pub struct PcmReceiver {
    mapped: Option<(u32, u64)>,
}

#[cfg(not(test))]
impl Default for PcmReceiver {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(not(test))]
impl PcmReceiver {
    pub const fn new() -> Self {
        Self { mapped: None }
    }

    fn ensure_mapped(&mut self, shm_id: u32) -> Result<u64, AudioTransportError> {
        if let Some((id, base)) = self.mapped {
            if id == shm_id {
                return Ok(base);
            }
            // A different region id — drop the stale mapping first.
            let _ = syscall_lib::shm_unmap(base);
            self.mapped = None;
        }
        let base = syscall_lib::shm_map(shm_id);
        if base == 0 {
            return Err(AudioTransportError::ShmMap);
        }
        self.mapped = Some((shm_id, base));
        Ok(base)
    }

    /// Map (or reuse) the region `shm_id`, validate the `[offset, offset+len)`
    /// window against [`PCM_RING_BYTES`], and copy it into `dst` (the driver's
    /// own `sys_device_dma_alloc` DMA buffer). Returns bytes copied.
    pub fn recv_and_copy(
        &mut self,
        shm_id: u32,
        offset: u32,
        len: u32,
        dst: &mut [u8],
    ) -> Result<usize, AudioTransportError> {
        let base = self.ensure_mapped(shm_id)?;
        // SAFETY: `sys_shm_map` mapped at least `PCM_RING_BYTES` bytes
        // (the sender created the region at that size) contiguously at
        // `base`; we form a read-only slice of exactly that length and
        // `copy_window` bounds-checks the window before reading.
        let region = unsafe { core::slice::from_raw_parts(base as *const u8, PCM_RING_BYTES) };
        copy_window(region, offset, len, dst)
    }

    /// Release the cached mapping (called on stream close / driver shutdown).
    pub fn release(&mut self) {
        if let Some((_, base)) = self.mapped.take() {
            let _ = syscall_lib::shm_unmap(base);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submission_bounds() {
        let mut region = [0u8; 256];
        for (i, b) in region.iter_mut().enumerate() {
            *b = i as u8;
        }
        let mut dst = [0u8; 256];

        // In-range window copies the right bytes.
        let n = copy_window(&region, 16, 32, &mut dst).expect("in-range copy");
        assert_eq!(n, 32);
        assert_eq!(&dst[..32], &region[16..48]);

        // Full-region window.
        assert_eq!(copy_window(&region, 0, 256, &mut dst), Ok(256));

        // Zero-length, overflow, and past-end windows are rejected before any
        // copy (delegated to submission_in_bounds).
        assert_eq!(
            copy_window(&region, 0, 0, &mut dst),
            Err(AudioTransportError::OutOfBounds)
        );
        assert_eq!(
            copy_window(&region, 250, 10, &mut dst),
            Err(AudioTransportError::OutOfBounds)
        );
        assert_eq!(
            copy_window(&region, u32::MAX, 2, &mut dst),
            Err(AudioTransportError::OutOfBounds)
        );

        // Window larger than dst is rejected.
        let mut tiny = [0u8; 8];
        assert_eq!(
            copy_window(&region, 0, 32, &mut tiny),
            Err(AudioTransportError::DestTooSmall)
        );
    }

    #[test]
    fn ring_size_covers_max_submission() {
        assert!(PCM_RING_BYTES >= kernel_core::audio::MAX_SUBMIT_BYTES);
        assert_eq!(PCM_RING_BYTES % 4096, 0);
    }
}
