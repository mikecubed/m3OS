// Device-host syscall numbers — Phase 55b Track B.
//
// Single source of truth for the five device-host syscall numbers reserved in
// the `0x11xx` block. Declared in `kernel-core` so the kernel-side dispatcher
// (Track B) and the userspace `driver_runtime` wrappers (Track C) compile
// against the same constants — per the Phase 55b DRY discipline, no other
// file in the workspace is permitted to redeclare these numbers.
//
// Numbering: the `0x11xx` block is carved out of the custom m3OS syscall
// range (0x1000–0x1FFF); `0x1100–0x110F` are consumed by the IPC subsystem,
// so the device-host family starts at `0x1120` to leave room for future IPC
// additions without renumbering. Track B.1 uses `SYS_DEVICE_CLAIM`; Tracks
// B.2–B.4 reserve their numbers here now so they land without re-editing
// this block.

/// Reserve the IOMMU- and capability-gated claim on a PCI(e) BDF.
/// Track B.1 — `sys_device_claim(segment, bus, dev, func) -> isize`.
pub const SYS_DEVICE_CLAIM: u64 = 0x1120;

/// Map a claimed device's BAR window into the caller's address space.
/// Track B.2 — `sys_device_mmio_map(dev_cap, bar_index) -> isize`.
pub const SYS_DEVICE_MMIO_MAP: u64 = 0x1121;

/// Allocate a DMA-mapped buffer against a claimed device's IOMMU domain.
/// Track B.3 — `sys_device_dma_alloc(dev_cap, size, align) -> isize`.
pub const SYS_DEVICE_DMA_ALLOC: u64 = 0x1122;

/// Look up the `(user_va, iova, len)` tuple for a `Capability::Dma` handle.
/// Reserved alongside Track B.3 for the userspace wrapper's handle
/// introspection path — declared now so the block numbering stays dense.
pub const SYS_DEVICE_DMA_HANDLE_INFO: u64 = 0x1123;

/// Subscribe to a device-originated IRQ and receive it as a notification.
/// Track B.4b — `sys_device_irq_subscribe(dev_cap, bit_index, notification_arg) -> isize`.
///
/// `bit_index` (arg2, 0..=63) selects the bit within the 64-bit notification
/// word the ISR will set on delivery. `notification_arg` (arg3) is either the
/// sentinel [`NOTIFICATION_SENTINEL_NEW`] — in which case the kernel allocates
/// a fresh `Notification` owned by the caller — or a `CapHandle` to an
/// existing `Capability::Notification` the caller already holds. The ABI
/// shape (three `u32` args) is enforced by the arch dispatcher in
/// `kernel/src/arch/x86_64/syscall/mod.rs` and by
/// `syscall_numbers_are_pinned_in_the_device_host_block()` below.
pub const SYS_DEVICE_IRQ_SUBSCRIBE: u64 = 0x1124;

/// Read a value from an I/O-port BAR of a claimed device.
/// Phase 63 Track Z.1 — `sys_device_pio_read(dev_cap, bar_index, offset, width) -> isize`.
///
/// Returns the port value zero-extended into the low bits on success, or a
/// negative errno. `width` must be 1, 2, or 4; any other value returns
/// `-EINVAL`. The BAR must be a PIO BAR; MMIO BARs return `-EINVAL`.
/// `offset + width` must fit within the BAR size; an out-of-range access
/// returns `-ERANGE`.
pub const SYS_DEVICE_PIO_READ: u64 = 0x1125;

/// Write a value to an I/O-port BAR of a claimed device.
/// Phase 63 Track Z.1 — `sys_device_pio_write(dev_cap, bar_index, offset, value, width) -> isize`.
///
/// Returns 0 on success, or a negative errno. `width` must be 1, 2, or 4;
/// any other value returns `-EINVAL`. The BAR must be a PIO BAR; MMIO BARs
/// return `-EINVAL`. `offset + width` must fit within the BAR size; an
/// out-of-range access returns `-ERANGE`.
pub const SYS_DEVICE_PIO_WRITE: u64 = 0x1126;

/// Enumerate PCI devices matching a class/subclass/prog_if triple.
/// Phase 78b Track C.1 —
/// `sys_device_pci_enumerate(class, subclass, prog_if, out_user_ptr, max_entries) -> isize`.
///
/// ## ABI
///
/// | Register | Value |
/// |----------|-------|
/// | `rax`    | `SYS_DEVICE_PCI_ENUMERATE` (0x1127) |
/// | `rdi`    | `class: u8`  — PCI class code (e.g. `0x0C` for Serial Bus) |
/// | `rsi`    | `subclass: u8` — PCI subclass (e.g. `0x03` for USB) |
/// | `rdx`    | `prog_if: u8` — Programming Interface (e.g. `0x30` for xHCI) |
/// | `r10`    | `out_user_ptr: usize` — pointer to caller's output buffer |
/// | `r8`     | `max_entries: usize` — capacity of the output buffer in entries |
///
/// On success the kernel writes up to `max_entries` packed BDF entries to
/// `out_user_ptr` and returns the **total** match count (which may exceed
/// `max_entries` if the buffer was too small). The caller must size the
/// buffer conservatively (e.g. 8 entries for USB controllers) or call once
/// with `max_entries=0` to query the count then call again with a real
/// buffer. A negative return value is a negated `errno`:
///
/// - `-EACCES` (`-13`): caller's exec-path is not under `/drivers/` — only
///   authorized driver processes may enumerate PCI devices.
/// - `-EFAULT` (`-14`): `out_user_ptr` is invalid or the copy-out failed.
/// - `-EINVAL` (`-22`): `class`, `subclass`, or `prog_if` is out of `u8` range
///   (rejected at the dispatcher before reaching this function).
/// - `-ESRCH` (`-3`): kernel context (PID 0) — use the in-kernel PCI scan
///   directly.
///
/// ## BDF packing format
///
/// Each entry is a `u32` in little-endian byte order:
///
/// ```text
/// bits [31:20] — PCI segment group (12 bits; encodable range 0–4095;
///                always 0 on current platforms)
/// bits [19:12] — bus number (8 bits; 0–255)
/// bits [11:10] — reserved / padding (always 0; dev is 5 bits, not 7)
/// bits [ 9: 5] — device number (5 bits; 0–31)
/// bits [ 4: 2] — function number (3 bits; 0–7)
/// bits [  1:0] — reserved (always 0)
/// ```
///
/// A future phase may promote segment to `u16` and extend the entry to `u64`
/// when multi-segment PCIe is implemented; a new syscall number will be
/// allocated at that time rather than silently changing this one's layout.
pub const SYS_DEVICE_PCI_ENUMERATE: u64 = 0x1127;

/// Read a value from the PCI configuration space of a device addressed by raw
/// BDF — **without** claiming it.
/// Phase 79 Track A.1 —
/// `sys_device_config_read(segment, bus, dev, func, offset, width) -> isize`.
///
/// ## Why a raw-BDF (pre-claim) config read
///
/// A modern NIC driver must decide *which* PCI function to claim by matching its
/// vendor:device ID against a per-family ID set. [`SYS_DEVICE_PCI_ENUMERATE`]
/// returns only the BDFs of a class/subclass/prog_if triple (e.g. all Ethernet
/// controllers); it does not reveal vendor/device. Claiming a function to read
/// its ID then releasing it is not viable — `sys_device_claim` enables
/// bus-mastering and installs an IOMMU domain, and there is no release syscall
/// (the claim drops only on process exit), so a probe-by-claim would
/// permanently lock every NIC the first driver inspected. This syscall lets an
/// authorized driver read the identifying config-space registers up-front so
/// exactly one driver claims each device.
///
/// ## ABI
///
/// | Register | Value |
/// |----------|-------|
/// | `rax`    | `SYS_DEVICE_CONFIG_READ` (0x1128) |
/// | `rdi`    | `segment: u16` — PCI segment group (always 0 on current platforms) |
/// | `rsi`    | `bus: u8` |
/// | `rdx`    | `dev: u8` (0–31) |
/// | `r10`    | `func: u8` (0–7) |
/// | `r8`     | packed `(offset << 8) | width` — `offset` is the 0–255 byte offset, `width` ∈ {1,2,4} |
///
/// `offset` and `width` are packed into a single register so the call uses the
/// same five-argument shape as [`SYS_DEVICE_PCI_ENUMERATE`] (no reliance on a
/// sixth `r9` argument). Use [`pack_config_read_arg`] / [`unpack_config_read_arg`].
///
/// On success returns the config value zero-extended into the low bits
/// (`offset`-aligned `width` bytes, little-endian). A negative return is a
/// negated `errno`:
///
/// - `-EACCES` (`-13`): caller's exec-path is not under `/drivers/`.
/// - `-EINVAL` (`-22`): `width` not in `{1,2,4}`, or `offset + width` exceeds
///   256, or `offset` is not naturally aligned to `width`.
/// - `-ENODEV` (`-19`): no PCI function exists at the supplied BDF (vendor ID
///   reads back `0xFFFF`).
/// - `-ESRCH` (`-3`): kernel context (PID 0).
pub const SYS_DEVICE_CONFIG_READ: u64 = 0x1128;

/// Write a value into the PCI configuration space of a device the caller has
/// **already claimed**.
/// Phase 80c Track F.1 —
/// `sys_device_config_write(segment, bus, dev, func, packed, value) -> isize`.
///
/// ## Why a config-space write (and why it is claim-gated)
///
/// Some controllers need vendor-specific configuration-space programming the
/// generic register path cannot express. The motivating case is the AMD
/// "Family 17h/19h HD Audio Controller" (`1022:15e3`): Linux's `snd_hda_intel`
/// (`azx_init_pci`) sets an AMD/ATI **snoop** bit in config space so the
/// controller's DMA stays cache-coherent. m3OS ships no kernel HDA quirk table,
/// so the ring-3 `hda_driver` performs this write itself.
///
/// Note: the snoop write is a **DMA-coherency** fix, **not** a codec-enumeration
/// gate. The Phase 80 VFIO capture (`docs/research/hda-realtek-capture.md`)
/// confirmed a codec enumerates in `STATESTS` without it (playback would just be
/// garbled); codec presence is gated by reset timing + codec power. Do not treat
/// snoop as a bring-up dependency.
///
/// Unlike [`SYS_DEVICE_CONFIG_READ`] — which is a *pre-claim* probe so a driver
/// can read vendor:device before deciding which function to claim — a config
/// **write** mutates device state, so it is gated on the caller **owning a
/// claim** on the target BDF (in addition to the `/drivers/` exec-path check).
/// A driver that has claimed a device can already drive its BARs and DMA, so
/// writing that same device's config space is within its existing authority;
/// writing a device it does not own is rejected.
///
/// ## ABI
///
/// | Register | Value |
/// |----------|-------|
/// | `rax`    | `SYS_DEVICE_CONFIG_WRITE` (0x1129) |
/// | `rdi`    | `segment: u16` — PCI segment group (always 0 on current platforms) |
/// | `rsi`    | `bus: u8` |
/// | `rdx`    | `dev: u8` (0–31) |
/// | `r10`    | `func: u8` (0–7) |
/// | `r8`     | packed `(offset << 8) | width` — `offset` is the 0–255 byte offset, `width` ∈ {1,2,4} |
/// | `r9`     | `value: u32` — must fit in `width` bytes |
///
/// The `(offset, width)` packing reuses [`pack_config_read_arg`] /
/// [`unpack_config_read_arg`]; the value rides in the sixth register so the
/// BDF + access shape stay identical to the read path.
///
/// On success returns `0`. A negative return is a negated `errno`:
///
/// - `-EACCES` (`-13`): caller's exec-path is not under `/drivers/`, **or** the
///   caller does not own a claim on the target BDF.
/// - `-EINVAL` (`-22`): `width` not in `{1,2,4}`, `offset + width` exceeds 256,
///   `offset` not naturally aligned to `width`, or `value` does not fit in
///   `width` bytes.
/// - `-ENODEV` (`-19`): no PCI function exists at the supplied BDF, or a
///   non-zero segment (multi-segment PCIe is not supported yet).
/// - `-ESRCH` (`-3`): kernel context (PID 0).
pub const SYS_DEVICE_CONFIG_WRITE: u64 = 0x1129;

/// IOMMU-map a **shared-memory region** (created via `sys_shm_create`) into a
/// claimed device's IOMMU domain so the device can DMA into/out of it
/// zero-copy. Phase 92a H.4 —
/// `sys_device_dma_map_shm(dev_cap, shm_id) -> isize`.
///
/// The shm region is a physically-contiguous frame run shared by integer id, so
/// both the owning class driver (which fills/reads it) and the host driver's
/// device (which DMAs it) see the same memory — the zero-copy substrate the USB
/// `SubmitTransfer` path needs without a per-transfer inline copy. The caller
/// must own `dev_cap` (a `Capability::Device` claim). On success the region is
/// ref-pinned and the **device IOVA** (>= 0) the caller programs into a transfer
/// descriptor is returned; the caller already knows the region length. Errors:
/// `-EBADF` (bad cap), `-EPERM` (caller does not own the claim), `-ENODEV` (no
/// such shm id), `-EIO` (IOMMU map failed).
pub const SYS_DEVICE_DMA_MAP_SHM: u64 = 0x112A;

/// Tear down a mapping installed by [`SYS_DEVICE_DMA_MAP_SHM`], removing the
/// device-domain IOMMU entry and dropping the shm ref. Phase 92a H.4 —
/// `sys_device_dma_unmap_shm(dev_cap, iova) -> isize`. The IOVA identifies the
/// mapping. Returns 0 or a negated errno (`-EBADF`/`-EINVAL`).
pub const SYS_DEVICE_DMA_UNMAP_SHM: u64 = 0x112B;

/// Sentinel passed as `notification_arg` (arg3) of [`SYS_DEVICE_IRQ_SUBSCRIBE`]
/// to request that the kernel allocate a fresh `Notification` object on the
/// caller's behalf, rather than binding the IRQ to an existing notification
/// the caller already holds. Any other value in that slot is interpreted as a
/// `CapHandle` into the caller's capability table. Single source of truth for
/// both the kernel syscall handler and ring-3 driver_runtime backends.
pub const NOTIFICATION_SENTINEL_NEW: u32 = u32::MAX;

/// Pack the `offset` (0–255) and `width` (1/2/4) of a [`SYS_DEVICE_CONFIG_READ`]
/// into the single `r8` argument. The driver_runtime wrapper packs; the kernel
/// dispatcher unpacks with [`unpack_config_read_arg`].
#[inline]
pub const fn pack_config_read_arg(offset: u16, width: u8) -> u64 {
    ((offset as u64) << 8) | (width as u64)
}

/// Inverse of [`pack_config_read_arg`] — returns `(offset, width)`.
#[inline]
pub const fn unpack_config_read_arg(packed: u64) -> (u16, u8) {
    (((packed >> 8) & 0xFFFF) as u16, (packed & 0xFF) as u8)
}

// ---------------------------------------------------------------------------
// Phase 101 Tracks D/E — platform-ACPI syscalls for the ring-3 `acpid`.
//
// These extend the device-host block but are gated on the `/drivers/`
// exec-path only (like `SYS_DEVICE_PCI_ENUMERATE`), not on a
// `Capability::Device`: ACPI's register blocks are FADT-enumerated fixed
// ports, not a claimable PCI function's BAR, and the DSDT/SSDT blobs are
// firmware memory no device owns.
// ---------------------------------------------------------------------------

/// Fetch an ACPI table's raw bytes read-only:
/// `sys_acpi_table_get(sig_ptr, index, out_ptr, out_len) -> isize`.
/// `sig_ptr` names a 4-byte signature (`FACP`, `DSDT`, `SSDT`, …); `index`
/// selects among same-signature tables (SSDTs). Returns the table's full
/// byte length (callers with a short buffer get a truncated copy and can
/// re-size), `-ENOENT` if absent, `-EACCES` outside `/drivers/`.
pub const SYS_ACPI_TABLE_GET: u64 = 0x112C;

/// Subscribe the platform SCI:
/// `sys_acpi_sci_subscribe(notification_arg) -> isize` (a fresh
/// `Capability::Notification` handle). The kernel routes the FADT's
/// `SCI_INT` GSI, demuxes + masks PM1/GPE events in its ISR, and signals
/// the notification ([`ACPI_SCI_BIT_PM1`]/[`ACPI_SCI_BIT_GPE`]). Pass
/// [`NOTIFICATION_SENTINEL_NEW`]; one subscriber at a time (`-EBUSY`).
pub const SYS_ACPI_SCI_SUBSCRIBE: u64 = 0x112D;

/// Read an ACPI PM register:
/// `sys_acpi_pm_read(reg_sel, byte_index) -> isize` (the register value).
pub const SYS_ACPI_PM_READ: u64 = 0x112E;

/// Write an ACPI PM register:
/// `sys_acpi_pm_write(reg_sel, byte_index, value) -> isize`.
pub const SYS_ACPI_PM_WRITE: u64 = 0x112F;

/// Read a raw I/O port for an AML `OperationRegion(SystemIO)` access:
/// `sys_acpi_io_read(port, width_bytes) -> isize` (the value,
/// zero-extended; `width_bytes` ∈ {1, 2, 4}).
///
/// Phase 101 Track E.3: unlike the role-named PM selectors below, AML
/// declares arbitrary ports (EC `0x62`/`0x66`, GPIO blocks, …), so the
/// `acpid` `RegionSpace` backend needs raw-port access. Gated on the
/// `/drivers/` exec path like every platform-ACPI syscall.
pub const SYS_ACPI_IO_READ: u64 = 0x1130;

/// Write a raw I/O port for an AML `OperationRegion(SystemIO)` access:
/// `sys_acpi_io_write(port, width_bytes, value) -> isize` (0 on success).
pub const SYS_ACPI_IO_WRITE: u64 = 0x1131;

/// Read physical memory for an AML `OperationRegion(SystemMemory)`
/// access: `sys_acpi_mem_read(phys_addr, width_bytes) -> isize` (the
/// value; `width_bytes` ∈ {1, 2, 4} — a 64-bit field chunk is split into
/// two 32-bit accesses ring-3 side so the value always fits the positive
/// `isize` range).
pub const SYS_ACPI_MEM_READ: u64 = 0x1132;

/// Write physical memory for an AML `OperationRegion(SystemMemory)`
/// access: `sys_acpi_mem_write(phys_addr, width_bytes, value) -> isize`
/// (0 on success).
pub const SYS_ACPI_MEM_WRITE: u64 = 0x1133;

/// Register the platform's `\_S5` sleep-type values so `sys_reboot`'s
/// poweroff arm can perform a real ACPI S5 (`SLP_TYPa<<10 | SLP_EN` to
/// PM1a_CNT): `sys_acpi_register_s5(slp_typa, slp_typb) -> isize`
/// (0 on success; values masked to 3 bits).
///
/// Phase 103 D.3: `acpid` evaluates the `\_S5` package at boot (AML —
/// ring 3 per the 101 split) and hands the kernel just the two integers;
/// the final register write stays in ring 0 because it must happen
/// *after* the kernel's own shutdown sync, long after acpid itself has
/// been SIGTERMed by init's service teardown.
pub const SYS_ACPI_REGISTER_S5: u64 = 0x1134;

/// `reg_sel` values for [`SYS_ACPI_PM_READ`]/[`SYS_ACPI_PM_WRITE`]. The
/// kernel resolves each selector to its FADT-declared port and access
/// width — ring 3 never names a raw port number.
pub const ACPI_PM_REG_PM1A_STS: u64 = 0; // u16 @ PM1a_EVT_BLK
pub const ACPI_PM_REG_PM1A_EN: u64 = 1; // u16 @ PM1a_EVT_BLK + PM1_EVT_LEN/2
pub const ACPI_PM_REG_PM1A_CNT: u64 = 2; // u16 @ PM1a_CNT_BLK (SCI_EN = bit 0)
pub const ACPI_PM_REG_GPE0_STS: u64 = 3; // u8  @ GPE0_BLK + byte_index
pub const ACPI_PM_REG_GPE0_EN: u64 = 4; // u8  @ GPE0_BLK + GPE0_BLK_LEN/2 + byte_index
pub const ACPI_PM_REG_SMI_CMD: u64 = 5; // u8  @ SMI_CMD (ACPI-enable handshake)

/// Notification bit the SCI ISR sets for pending PM1 fixed events.
pub const ACPI_SCI_BIT_PM1: u8 = 0;
/// Notification bit the SCI ISR sets for pending GPE0 events.
pub const ACPI_SCI_BIT_GPE: u8 = 1;

/// Lowest syscall number in the reserved device-host block.
///
/// Track B dispatch arms match `DEVICE_HOST_BASE..=DEVICE_HOST_LAST` so new
/// numbers can be appended here and dispatched through a single match arm
/// per Phase 55b task list C.2.
pub const DEVICE_HOST_BASE: u64 = SYS_DEVICE_CLAIM;

/// Highest syscall number reserved in the device-host block.
///
/// Adjust upward when adding new device-host syscalls; the Track B acceptance
/// items pin this constant as the authoritative upper bound.
pub const DEVICE_HOST_LAST: u64 = SYS_ACPI_REGISTER_S5;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syscall_numbers_are_pinned_in_the_device_host_block() {
        // Pin the exact numeric values so a typo during a future phase turns
        // into a test failure rather than a silent renumbering.
        assert_eq!(SYS_DEVICE_CLAIM, 0x1120);
        assert_eq!(SYS_DEVICE_MMIO_MAP, 0x1121);
        assert_eq!(SYS_DEVICE_DMA_ALLOC, 0x1122);
        assert_eq!(SYS_DEVICE_DMA_HANDLE_INFO, 0x1123);
        assert_eq!(SYS_DEVICE_IRQ_SUBSCRIBE, 0x1124);
        // Phase 63 Track Z.1 — PIO syscalls appended after IRQ_SUBSCRIBE.
        assert_eq!(SYS_DEVICE_PIO_READ, 0x1125);
        assert_eq!(SYS_DEVICE_PIO_WRITE, 0x1126);
        // Phase 78b Track C.1 — PCI class enumeration.
        assert_eq!(SYS_DEVICE_PCI_ENUMERATE, 0x1127);
        // Phase 79 Track A.1 — raw-BDF PCI config-space read.
        assert_eq!(SYS_DEVICE_CONFIG_READ, 0x1128);
        // Phase 80c Track F.1 — claim-gated PCI config-space write.
        assert_eq!(SYS_DEVICE_CONFIG_WRITE, 0x1129);
        // Phase 92a H.4 — IOMMU-map / unmap a shared-memory region for device DMA.
        assert_eq!(SYS_DEVICE_DMA_MAP_SHM, 0x112A);
        assert_eq!(SYS_DEVICE_DMA_UNMAP_SHM, 0x112B);
        // Phase 101 D/E — platform-ACPI syscalls for the ring-3 acpid.
        assert_eq!(SYS_ACPI_TABLE_GET, 0x112C);
        assert_eq!(SYS_ACPI_SCI_SUBSCRIBE, 0x112D);
        assert_eq!(SYS_ACPI_PM_READ, 0x112E);
        assert_eq!(SYS_ACPI_PM_WRITE, 0x112F);
        // Phase 101 E.3 — raw-port + physical-memory region backend.
        assert_eq!(SYS_ACPI_IO_READ, 0x1130);
        assert_eq!(SYS_ACPI_IO_WRITE, 0x1131);
        assert_eq!(SYS_ACPI_MEM_READ, 0x1132);
        assert_eq!(SYS_ACPI_MEM_WRITE, 0x1133);
        // Phase 103 D.3 — S5 sleep-type registration for real poweroff.
        assert_eq!(SYS_ACPI_REGISTER_S5, 0x1134);
        assert_eq!(DEVICE_HOST_LAST, SYS_ACPI_REGISTER_S5);
    }

    #[test]
    fn syscall_numbers_are_distinct() {
        let all = [
            SYS_DEVICE_CLAIM,
            SYS_DEVICE_MMIO_MAP,
            SYS_DEVICE_DMA_ALLOC,
            SYS_DEVICE_DMA_HANDLE_INFO,
            SYS_DEVICE_IRQ_SUBSCRIBE,
            SYS_DEVICE_PIO_READ,
            SYS_DEVICE_PIO_WRITE,
            SYS_DEVICE_PCI_ENUMERATE,
            SYS_DEVICE_CONFIG_READ,
            SYS_DEVICE_CONFIG_WRITE,
            SYS_DEVICE_DMA_MAP_SHM,
            SYS_DEVICE_DMA_UNMAP_SHM,
            SYS_ACPI_TABLE_GET,
            SYS_ACPI_SCI_SUBSCRIBE,
            SYS_ACPI_PM_READ,
            SYS_ACPI_PM_WRITE,
            SYS_ACPI_IO_READ,
            SYS_ACPI_IO_WRITE,
            SYS_ACPI_MEM_READ,
            SYS_ACPI_MEM_WRITE,
            SYS_ACPI_REGISTER_S5,
        ];
        for (i, a) in all.iter().enumerate() {
            for (j, b) in all.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "syscall numbers {i} and {j} collide");
                }
            }
        }
    }

    #[test]
    fn every_device_host_syscall_falls_inside_the_reserved_block() {
        let all = [
            SYS_DEVICE_CLAIM,
            SYS_DEVICE_MMIO_MAP,
            SYS_DEVICE_DMA_ALLOC,
            SYS_DEVICE_DMA_HANDLE_INFO,
            SYS_DEVICE_IRQ_SUBSCRIBE,
            SYS_DEVICE_PIO_READ,
            SYS_DEVICE_PIO_WRITE,
            SYS_DEVICE_PCI_ENUMERATE,
            SYS_DEVICE_CONFIG_READ,
            SYS_DEVICE_CONFIG_WRITE,
            SYS_DEVICE_DMA_MAP_SHM,
            SYS_DEVICE_DMA_UNMAP_SHM,
            SYS_ACPI_TABLE_GET,
            SYS_ACPI_SCI_SUBSCRIBE,
            SYS_ACPI_PM_READ,
            SYS_ACPI_PM_WRITE,
            SYS_ACPI_IO_READ,
            SYS_ACPI_IO_WRITE,
            SYS_ACPI_MEM_READ,
            SYS_ACPI_MEM_WRITE,
            SYS_ACPI_REGISTER_S5,
        ];
        for n in all {
            assert!(
                (DEVICE_HOST_BASE..=DEVICE_HOST_LAST).contains(&n),
                "syscall {n:#x} outside device-host block"
            );
        }
    }

    #[test]
    fn pio_syscalls_follow_irq_subscribe_without_gap() {
        // Phase 63 Track Z.1 pin: PIO numbers must be contiguous with
        // IRQ_SUBSCRIBE so the block stays dense per the Phase 55b numbering
        // discipline.
        assert_eq!(SYS_DEVICE_PIO_READ, SYS_DEVICE_IRQ_SUBSCRIBE + 1);
        assert_eq!(SYS_DEVICE_PIO_WRITE, SYS_DEVICE_PIO_READ + 1);
        // Phase 78b Track C.1 pin: PCI_ENUMERATE follows PIO_WRITE without gap.
        assert_eq!(SYS_DEVICE_PCI_ENUMERATE, SYS_DEVICE_PIO_WRITE + 1);
        // Phase 79 Track A.1 pin: CONFIG_READ follows PCI_ENUMERATE without gap.
        assert_eq!(SYS_DEVICE_CONFIG_READ, SYS_DEVICE_PCI_ENUMERATE + 1);
        // Phase 80c Track F.1 pin: CONFIG_WRITE follows CONFIG_READ without gap.
        assert_eq!(SYS_DEVICE_CONFIG_WRITE, SYS_DEVICE_CONFIG_READ + 1);
        // Phase 92a H.4 pin: the shm-DMA map/unmap pair follows CONFIG_WRITE.
        assert_eq!(SYS_DEVICE_DMA_MAP_SHM, SYS_DEVICE_CONFIG_WRITE + 1);
        assert_eq!(SYS_DEVICE_DMA_UNMAP_SHM, SYS_DEVICE_DMA_MAP_SHM + 1);
        // Phase 101 D/E pin: the platform-ACPI quartet.
        assert_eq!(SYS_ACPI_TABLE_GET, SYS_DEVICE_DMA_UNMAP_SHM + 1);
        assert_eq!(SYS_ACPI_SCI_SUBSCRIBE, SYS_ACPI_TABLE_GET + 1);
        assert_eq!(SYS_ACPI_PM_READ, SYS_ACPI_SCI_SUBSCRIBE + 1);
        assert_eq!(SYS_ACPI_PM_WRITE, SYS_ACPI_PM_READ + 1);
        // Phase 101 E.3 pin: the RegionSpace io/mem quartet closes the block.
        assert_eq!(SYS_ACPI_IO_READ, SYS_ACPI_PM_WRITE + 1);
        assert_eq!(SYS_ACPI_IO_WRITE, SYS_ACPI_IO_READ + 1);
        assert_eq!(SYS_ACPI_MEM_READ, SYS_ACPI_IO_WRITE + 1);
        assert_eq!(SYS_ACPI_MEM_WRITE, SYS_ACPI_MEM_READ + 1);
        // Phase 103 D.3 pin: S5 registration closes the block.
        assert_eq!(SYS_ACPI_REGISTER_S5, SYS_ACPI_MEM_WRITE + 1);
        assert_eq!(DEVICE_HOST_LAST, SYS_ACPI_REGISTER_S5);
    }

    #[test]
    fn config_read_arg_packing_round_trips() {
        for &(off, w) in &[
            (0u16, 4u8),
            (0x00, 2),
            (0x02, 2),
            (0x08, 1),
            (0xFC, 4),
            (0xFF, 1),
        ] {
            let packed = pack_config_read_arg(off, w);
            assert_eq!(unpack_config_read_arg(packed), (off, w));
        }
        // The packed value never collides width into offset.
        assert_eq!(pack_config_read_arg(0x00, 4), 4);
        assert_eq!(pack_config_read_arg(0x02, 2), 0x202);
    }

    #[test]
    fn device_host_block_does_not_collide_with_ipc_block() {
        // IPC block is 0x1100..=0x1110 (see arch/x86_64/syscall/mod.rs
        // `IPC_BASE` / `IPC_LAST`). The device-host block must sit above it.
        const IPC_LAST_RESERVED: u64 = 0x1110;
        assert!(DEVICE_HOST_BASE > IPC_LAST_RESERVED);
    }
}
