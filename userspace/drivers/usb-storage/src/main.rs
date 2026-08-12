//! Phase 92a Tracks D.4 (block-device facade) + D.3 (UAS selection) + C.4
//! (detach cleanup) — resident USB Mass Storage daemon.
//!
//! Extends the Phase 92 Track D.1/D.2 daemon (which exited after identity/
//! capacity probing) into a **resident block-server** that:
//!
//! 1. Polls `NextAttach` until a `CLASS_MASS_STORAGE` device with bulk IN+OUT
//!    appears (up to ~30 s, re-walking every ~200 ms so tier-2 hub-enumerated
//!    devices arrive late without missing the window).
//! 2. Probes non-destructively (GET_MAX_LUN, TEST UNIT READY, INQUIRY, READ
//!    CAPACITY(10)) — emitting the existing sentinels so the smoke gate stays
//!    green.
//! 3. Reads LBA 0 to detect a **real filesystem** (MBR 0x55AA or ext2
//!    superblock magic); on scratch/blank media runs the destructive WRITE/READ
//!    self-test and emits `USB_STORAGE:rw-ok`. Real-FS detection skips the
//!    write to protect live data.
//! 4. **UAS transport selection (D.3)**: fetches the raw config descriptor and
//!    scans for a mass-storage interface with `bInterfaceProtocol == 0x62`
//!    (UAS). Logs `transport=uas` or `transport=bot`. BOT is the must-work
//!    path (QEMU `usb-storage` is BOT). UAS command routing is scaffolded with
//!    a clear `TODO(92a D.3)` marker.
//! 5. Registers the device as `usb0.block` via `ipc_register_service` and
//!    enters a `BlockServer`-style loop dispatching `BLK_READ`, `BLK_WRITE`,
//!    `BLK_FLUSH`, and `BLK_STATUS` requests over BOT.
//! 6. **Detach cleanup (C.4)**: the resident block-server loop bounds its recv
//!    with `ipc_recv_msg_timeout`; on an idle window it re-queries `NextAttach`
//!    and, when the bound device is gone, calls `umount("/mnt/usb0")` (which
//!    tears down the secondary ext2 volume + frees the kernel `blk::remote`
//!    slot) so a hot-unplugged stick leaves no stale mount, then exits.
//!
//! # Existing sentinels kept
//!
//! * `USB_STORAGE:bot-ok`          — BOT round-trip succeeded
//! * `USB_MASS_STORAGE:ready blocks=N bsize=S` — capacity read
//! * `USB_STORAGE:rw-ok`           — scratch device WRITE/READ verified

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]
#![cfg_attr(not(test), feature(alloc_error_handler))]

extern crate alloc;
#[cfg(test)]
extern crate std;

#[cfg(not(test))]
use alloc::vec::Vec;
#[cfg(not(test))]
use core::alloc::Layout;

#[cfg(not(test))]
use kernel_core::driver_ipc::block::{
    BLK_FLUSH, BLK_READ, BLK_STATUS, BLK_WRITE, BlkReplyHeader, BlockDriverError,
    decode_blk_request, encode_blk_reply,
};
#[cfg(not(test))]
use kernel_core::usb::mass_storage::{
    CSW_LEN, Cbw, Csw, InquiryData, ReadCapacity10, cdb_inquiry, cdb_read_capacity10, cdb_read10,
    cdb_test_unit_ready, cdb_write10,
};
#[cfg(not(test))]
use syscall_lib::heap::BrkAllocator;
#[cfg(not(test))]
use syscall_lib::write_str;
#[cfg(not(test))]
use syscall_lib::{IpcMessage, STDOUT_FILENO};
#[cfg(not(test))]
use usb_core::protocol::{
    AttachNotice, USB_MSG_MAX, USB_REQ_LABEL, USB_SERVICE_NAME, UsbReply, UsbRequest,
};

#[cfg(not(test))]
#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[cfg(not(test))]
#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    write_str(STDOUT_FILENO, "usb-storage: alloc error\n");
    syscall_lib::exit(99)
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "usb-storage: PANIC\n");
    syscall_lib::exit(101)
}

#[cfg(not(test))]
syscall_lib::entry_point!(program_main);

/// Boot-log marker written when the daemon starts.
pub const BOOT_LOG_MARKER: &str = "usb-storage: spawned\n";

/// Bare-metal de-risk toggle: emit the big, photo-legible PASS/FAIL banner
/// (`print_storage_pass_banner` / `print_storage_fail_banner`).
///
/// **Now `false` — the de-risk is concluded** (USB mass storage was validated on
/// the real Tiger Lake laptop, Phase 96). The FAIL banner fired on the no-device
/// timeout, so on a normal QEMU boot (where `usb_storage` runs without a stick)
/// it printed mid-boot every time and corrupted the `security-floor` regression's
/// prompt matching. Flip back to `true` only for another bare-metal photo-debug
/// session.
#[cfg(not(test))]
const PROBE_BANNER: bool = false;

/// USB Mass Storage interface class (USB-IF base-class 0x08).
const CLASS_MASS_STORAGE: u8 = 0x08;
/// USB Mass Storage BOT protocol (bInterfaceProtocol 0x50).
const PROTOCOL_BOT: u8 = 0x50;
/// USB Attached SCSI (UAS) protocol (bInterfaceProtocol 0x62).
const PROTOCOL_UAS: u8 = 0x62;
/// SCSI peripheral device type for a direct-access block device (disk).
#[cfg(not(test))]
const SCSI_TYPE_DIRECT_ACCESS: u8 = 0x00;

/// Standard INQUIRY allocation length (we only need the first 36 bytes).
#[cfg(not(test))]
const INQUIRY_LEN: u16 = 36;
/// READ CAPACITY(10) response length.
#[cfg(not(test))]
const READ_CAPACITY10_LEN: u16 = 8;

/// Scratch LBA used by the READ/WRITE round-trip self-test (512 KiB into
/// the medium — past any plausible boot sector / filesystem superblock).
#[cfg(not(test))]
const SCRATCH_LBA: u32 = 1024;

/// Conventional VFS mount prefix this daemon's device mounts under
/// (`/dev/usb0` → `/mnt/usb0`), used by the C.4 detach path to unmount on a
/// hot-unplug. NUL-terminated for the `umount` syscall.
#[cfg(not(test))]
const MOUNT_PREFIX_CSTR: &[u8] = b"/mnt/usb0\0";

/// Block-server idle window for the C.4 detach reconcile: when no block request
/// arrives within this many ms, re-check whether the bound device was
/// hot-unplugged (its `NextAttach` entry flipped to `attached:false`).
#[cfg(not(test))]
const DETACH_POLL_INTERVAL_MS: u64 = 1000;

/// `ipc_recv_msg_timeout` returns this sentinel (`-110` cast to `u64`,
/// `-ETIMEDOUT`) on a deadline expiry — distinct from a real message label or
/// the `u64::MAX` error return.
#[cfg(not(test))]
const NEG_ETIMEDOUT: u64 = (-110i64) as u64;

/// Maximum consecutive `handle_next`-style errors before the daemon exits
/// for restart (mirrors nvme driver).
#[cfg(not(test))]
const MAX_CONSECUTIVE_ERRORS: u32 = 8;

/// Maximum number of sectors per BOT READ/WRITE(10) sub-request.
///
/// The inline `SubmitBulkIn`/`SubmitBulkOut` data stage must fit `USB_MSG_MAX`
/// (4096) **including the wire-codec overhead**: a `BulkData` reply is
/// `data + 3` bytes and a `SubmitBulkOut` request is `data + 7` bytes. So the
/// largest sector-aligned data stage that fits is 7 × 512 = 3584 bytes (8 ×
/// 512 = 4096 overflows the reply and is rejected by the server's H.6 bound).
/// A larger `BLK_READ`/`BLK_WRITE` (e.g. a 4096-byte ext2 block = 8 sectors) is
/// split across multiple BOT sub-requests by `handle_read`/`handle_write` and
/// reassembled in the block-protocol reply (which has the larger MAX_BULK_LEN
/// budget). Oversized transfers (D.5) use the page-grant path instead.
#[cfg(not(test))]
const MAX_BOT_SECTORS: u16 = 7;

/// Maximum sectors per BOT READ/WRITE(10) command on the **zero-copy shm
/// path** (Phase 106): 128 sectors = 64 KiB per SCSI command.
///
/// The xHCI server's `SubmitShmTransfer` programs the whole data stage as a
/// **single Normal TRB** (`submit_bulk_iova`), whose 17-bit transfer-length
/// field caps one TRB at 128 KiB − 1 — so a full 256-sector block request
/// cannot be one TRB. 64 KiB stays comfortably inside that limit, is the
/// classic mass-storage transfer size real OSes issue over BOT, and turns a
/// 256-sector `BLK_READ` into 2 SCSI commands (6 IPC round-trips) instead of
/// 37 inline sub-requests (111 round-trips) — the difference between the
/// Phase 106 installer's image copy finishing in minutes vs hours under TCG.
#[cfg(not(test))]
const MAX_SHM_SECTORS: u32 = 128;

/// One persistent shared-memory bounce buffer (`MAX_SHM_SECTORS` × 512 =
/// 64 KiB) created at server start and reused for every multi-sector BOT
/// data stage. The xHCI server IOMMU-maps it per transfer and DMAs the
/// stage directly into/out of it (`SubmitShmTransfer`); this daemon then
/// memcpys between the region and the block-IPC payload. Setup failure is
/// non-fatal — `None` falls back to the inline 7-sector chunking.
#[cfg(not(test))]
struct ShmBounce {
    /// Kernel shm region id (passed to the xHCI server by value).
    id: u32,
    /// This process's writable mapping of the region.
    va: *mut u8,
}

/// Create + map the bounce region. Emits a one-line sentinel either way so
/// boot logs show which large-transfer path is live.
#[cfg(not(test))]
fn setup_shm_bounce() -> Option<ShmBounce> {
    let bytes = MAX_SHM_SECTORS as usize * 512;
    let id = syscall_lib::shm_create(bytes);
    if id == 0 {
        write_str(
            STDOUT_FILENO,
            "usb-storage: shm bounce create failed — inline transfers only\n",
        );
        return None;
    }
    let va = syscall_lib::shm_map(id);
    if va == 0 || va == u64::MAX {
        write_str(
            STDOUT_FILENO,
            "usb-storage: shm bounce map failed — inline transfers only\n",
        );
        return None;
    }
    write_str(STDOUT_FILENO, "USB_STORAGE:shm-bounce-ok sectors=");
    write_u32_dec(MAX_SHM_SECTORS);
    write_str(STDOUT_FILENO, "\n");
    Some(ShmBounce {
        id,
        va: va as *mut u8,
    })
}

// ---------------------------------------------------------------------------
// IPC plumbing (mirrors usb-hid's usb_call)
// ---------------------------------------------------------------------------

/// Issue a `UsbRequest` to the xHCI server and decode the `UsbReply`.
#[cfg(not(test))]
fn usb_call(usb_ep: u32, req: &UsbRequest) -> Option<UsbReply> {
    let req_bytes = req.encode();
    let rc = syscall_lib::ipc_call_buf(usb_ep, USB_REQ_LABEL, 0, &req_bytes);
    if rc == u64::MAX {
        return None;
    }
    let mut reply_buf = [0u8; USB_MSG_MAX];
    let n = syscall_lib::ipc_take_pending_bulk(&mut reply_buf);
    if n == u64::MAX {
        return None;
    }
    UsbReply::decode(&reply_buf[..n as usize])
}

#[cfg(not(test))]
fn lookup(name: &str) -> Option<u32> {
    let h = syscall_lib::ipc_lookup_service(name);
    if h == u64::MAX { None } else { Some(h as u32) }
}

/// Next monotonic CBW tag (a CSW echoes the CBW tag — we don't strictly
/// verify it here, but it must be unique per command).
#[cfg(not(test))]
fn next_tag() -> u32 {
    use core::sync::atomic::{AtomicU32, Ordering};
    static TAG: AtomicU32 = AtomicU32::new(0x1000);
    TAG.fetch_add(1, Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// BOT command execution
// ---------------------------------------------------------------------------

/// Issue a **synchronous, single-TRB** bulk-IN transfer of exactly `len`
/// bytes for one BOT phase (the data stage or the 13-byte CSW).
#[cfg(not(test))]
fn submit_bulk_in(usb_ep: u32, slot_id: u8, dci: u8, len: u16) -> Option<Vec<u8>> {
    match usb_call(usb_ep, &UsbRequest::SubmitBulkIn { slot_id, dci, len }) {
        Some(UsbReply::BulkData {
            data,
            completion_code: 1,
        }) => Some(data),
        _ => None,
    }
}

/// Run one BOT SCSI command: CBW on bulk-OUT, optional data phase on
/// bulk-IN, then the CSW on bulk-IN. Returns `(data, csw_status)`.
#[cfg(not(test))]
fn bot_command(
    usb_ep: u32,
    notice: &AttachNotice,
    cdb: &[u8],
    data_in: bool,
    data_len: u16,
) -> Option<(Vec<u8>, u8)> {
    let slot_id = notice.slot_id;
    let tag = next_tag();
    let cbw = Cbw::new(tag, data_len as u32, data_in, 0, cdb);

    if !bulk_out(usb_ep, slot_id, notice.bulk_out_dci, &cbw.encode()) {
        write_str(STDOUT_FILENO, "usb-storage: CBW bulk-OUT failed\n");
        return None;
    }

    let data = if data_in && data_len > 0 {
        submit_bulk_in(usb_ep, slot_id, notice.bulk_in_dci, data_len)?
    } else {
        Vec::new()
    };

    let csw_bytes = submit_bulk_in(usb_ep, slot_id, notice.bulk_in_dci, CSW_LEN as u16)?;
    let csw = Csw::parse(&csw_bytes)?;
    if csw.tag != tag {
        write_str(STDOUT_FILENO, "usb-storage: CSW tag mismatch\n");
        return None;
    }
    Some((data, csw.status))
}

/// Issue one BOT SCSI command carrying a **host-to-device data-OUT** stage
/// (e.g. WRITE(10)). Returns the CSW status (0 = passed).
#[cfg(not(test))]
fn bot_command_write(usb_ep: u32, notice: &AttachNotice, cdb: &[u8], payload: &[u8]) -> Option<u8> {
    let slot_id = notice.slot_id;
    let tag = next_tag();
    let cbw = Cbw::new(tag, payload.len() as u32, false, 0, cdb);

    if !bulk_out(usb_ep, slot_id, notice.bulk_out_dci, &cbw.encode()) {
        write_str(STDOUT_FILENO, "usb-storage: WRITE CBW bulk-OUT failed\n");
        return None;
    }
    if !payload.is_empty() && !bulk_out(usb_ep, slot_id, notice.bulk_out_dci, payload) {
        write_str(STDOUT_FILENO, "usb-storage: WRITE data bulk-OUT failed\n");
        return None;
    }
    let csw_bytes = submit_bulk_in(usb_ep, slot_id, notice.bulk_in_dci, CSW_LEN as u16)?;
    let csw = Csw::parse(&csw_bytes)?;
    if csw.tag != tag {
        write_str(STDOUT_FILENO, "usb-storage: WRITE CSW tag mismatch\n");
        return None;
    }
    Some(csw.status)
}

/// Submit a bulk-OUT transfer and block for completion.
#[cfg(not(test))]
fn bulk_out(usb_ep: u32, slot_id: u8, dci: u8, data: &[u8]) -> bool {
    matches!(
        usb_call(
            usb_ep,
            &UsbRequest::SubmitBulkOut {
                slot_id,
                dci,
                data: data.to_vec(),
            },
        ),
        Some(UsbReply::TransferComplete {
            completion_code: 1,
            ..
        })
    )
}

/// Issue `GET_MAX_LUN` (class control-IN). A STALL means a single LUN.
#[cfg(not(test))]
fn get_max_lun(usb_ep: u32, notice: &AttachNotice) -> u8 {
    let iface = notice.interface_num;
    let setup = [0xA1, 0xFE, 0x00, 0x00, iface, 0x00, 0x01, 0x00];
    match usb_call(
        usb_ep,
        &UsbRequest::ControlRequest {
            slot_id: notice.slot_id,
            setup,
            length: 1,
        },
    ) {
        Some(UsbReply::ControlData {
            data,
            completion_code: 1,
        }) if !data.is_empty() => data[0],
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// Decimal log helpers
// ---------------------------------------------------------------------------

#[cfg(not(test))]
fn write_u32_dec(mut n: u32) {
    let mut buf = [0u8; 10];
    let mut i = buf.len();
    if n == 0 {
        write_str(STDOUT_FILENO, "0");
        return;
    }
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    // SAFETY: buf[i..] is ASCII digits.
    write_str(STDOUT_FILENO, unsafe {
        core::str::from_utf8_unchecked(&buf[i..])
    });
}

#[cfg(not(test))]
fn write_u8_dec(n: u8) {
    write_u32_dec(n as u32);
}

// ---------------------------------------------------------------------------
// UAS transport detection (D.3)
// ---------------------------------------------------------------------------

/// Scan a raw USB configuration descriptor blob and return `true` if it
/// contains a Mass Storage interface (`bInterfaceClass == 0x08`) with the
/// UAS protocol (`bInterfaceProtocol == 0x62`).
///
/// Interface descriptor layout (USB 2.0 §9.6.5):
/// - byte 0: bLength
/// - byte 1: bDescriptorType  (0x04 = Interface)
/// - byte 2: bInterfaceNumber
/// - byte 3: bAlternateSetting
/// - byte 4: bNumEndpoints
/// - byte 5: bInterfaceClass
/// - byte 6: bInterfaceSubClass
/// - byte 7: bInterfaceProtocol
/// - byte 8: iInterface
///
/// This is host-testable: no USB hardware is required.
pub fn find_uas_interface(config: &[u8]) -> bool {
    let mut i = 0usize;
    while i < config.len() {
        // Each descriptor starts with bLength (byte 0) and bDescriptorType
        // (byte 1). Skip any zero-length descriptor to avoid infinite loops.
        let len = config[i] as usize;
        if len < 2 || i + len > config.len() {
            break;
        }
        let desc_type = config[i + 1];
        // 0x04 = Interface Descriptor
        if desc_type == 0x04 && len >= 9 {
            let class = config[i + 5];
            let protocol = config[i + 7];
            if class == CLASS_MASS_STORAGE && protocol == PROTOCOL_UAS {
                return true;
            }
        }
        i += len;
    }
    false
}

/// Select the transport for a bound mass-storage device and log the choice.
///
/// Fetches the raw configuration descriptor via `GetDescriptors`, scans for
/// a UAS interface, and returns `true` for UAS, `false` for BOT.
///
/// Falls back to BOT on any IPC failure or if the device only advertises BOT
/// (`bInterfaceProtocol == 0x50`).
#[cfg(not(test))]
fn select_transport(usb_ep: u32, notice: &AttachNotice) -> bool {
    let config_blob = match usb_call(
        usb_ep,
        &UsbRequest::GetDescriptors {
            slot_id: notice.slot_id,
        },
    ) {
        Some(UsbReply::Descriptors { config, .. }) => config,
        _ => {
            // Cannot fetch descriptors — fall back to BOT.
            write_str(
                STDOUT_FILENO,
                "usb-storage: GetDescriptors failed — defaulting to transport=bot\n",
            );
            return false;
        }
    };

    if find_uas_interface(&config_blob) {
        write_str(STDOUT_FILENO, "usb-storage: transport=uas\n");
        // TODO(92a D.3): Drive SCSI commands over UAS IUs (CommandIu on
        // bulk-OUT command pipe, SenseIu/ResponseIu on bulk-IN status pipe,
        // data over the data pipes, Tag == Stream ID). For now fall back to
        // BOT so the block server functions on QEMU `usb-storage` devices
        // while UAS bring-up on `usb-uas` hardware is deferred.
        write_str(
            STDOUT_FILENO,
            "usb-storage: UAS selected but falling back to BOT for block-server loop\n",
        );
        true
    } else {
        // BOT — check the protocol byte for logging; accept any non-UAS device.
        if notice.interface_protocol == PROTOCOL_BOT {
            write_str(STDOUT_FILENO, "usb-storage: transport=bot\n");
        } else {
            write_str(
                STDOUT_FILENO,
                "usb-storage: transport=bot (unknown protocol)\n",
            );
        }
        false
    }
}

// ---------------------------------------------------------------------------
// Real-FS vs scratch detection (D.4 safety gate)
// ---------------------------------------------------------------------------

/// Return `true` if LBA 0 / LBA 2 of the attached device contain
/// filesystem signatures that should NOT be overwritten.
///
/// Checks:
/// 1. MBR signature: bytes 510–511 of LBA 0 == `0x55 0xAA`.
/// 2. ext2 superblock magic: bytes 56–57 of the 1024-byte superblock
///    (which lives at disk offset 1024 = LBA 2, bytes 56–57) == `0x53 0xEF`.
///
/// On any read failure (transport error, small read) returns `false` (safe
/// default: run the self-test only if we are sure there is no FS).
#[cfg(not(test))]
fn detect_real_fs(usb_ep: u32, notice: &AttachNotice) -> bool {
    // Read LBA 0 (512 bytes).
    let lba0 = match bot_command(usb_ep, notice, &cdb_read10(0, 1), true, 512) {
        Some((data, 0)) if data.len() >= 512 => data,
        _ => return false,
    };

    // Check MBR signature at bytes 510–511.
    if lba0[510] == 0x55 && lba0[511] == 0xAA {
        return true;
    }

    // Read LBA 2 (512 bytes) to reach the ext2 superblock area.
    // The ext2 superblock starts at disk offset 1024 = LBA 2 byte 0.
    // Magic is at superblock offset 56, i.e. LBA 2 byte 56.
    let lba2 = match bot_command(usb_ep, notice, &cdb_read10(2, 1), true, 512) {
        Some((data, 0)) if data.len() >= 512 => data,
        _ => return false,
    };

    // ext2 magic: 0x53EF at offset 56–57 in the superblock (little-endian).
    if lba2[56] == 0x53 && lba2[57] == 0xEF {
        return true;
    }

    false
}

// ---------------------------------------------------------------------------
// Bare-metal de-risk: big, photo-legible PASS/FAIL banner
// ---------------------------------------------------------------------------
//
// On bare metal there is no serial console — the result is read off a phone
// photo of the framebuffer. A multi-line bordered banner with a distinct fill
// character per outcome (`=` PASS / `#` FAIL) stays recognisable even in a
// blurry photo, where a single buried sentinel line would not.

/// PASS banner — emitted once the device answers GET_MAX_LUN → TEST UNIT READY
/// → INQUIRY → READ CAPACITY, i.e. it is fully reachable as USB mass storage.
#[cfg(not(test))]
fn print_storage_pass_banner(blocks: u32, bsize: u32) {
    if !PROBE_BANNER {
        return;
    }
    write_str(
        STDOUT_FILENO,
        "\n==================================================\n",
    );
    write_str(
        STDOUT_FILENO,
        "==  USB MASS STORAGE:  PASS  -  usb0 reachable\n",
    );
    write_str(STDOUT_FILENO, "==  blocks=");
    write_u32_dec(blocks);
    write_str(STDOUT_FILENO, "  bsize=");
    write_u32_dec(bsize);
    write_str(
        STDOUT_FILENO,
        "\n==================================================\n\n",
    );
}

/// FAIL banner — emitted when the discovery walk times out with no
/// mass-storage device (e.g. the boot stick did not re-enumerate via xHCI).
#[cfg(not(test))]
fn print_storage_fail_banner() {
    if !PROBE_BANNER {
        return;
    }
    write_str(
        STDOUT_FILENO,
        "\n##################################################\n",
    );
    write_str(
        STDOUT_FILENO,
        "##  USB MASS STORAGE:  FAIL  -  no device found\n",
    );
    write_str(
        STDOUT_FILENO,
        "##  boot stick NOT reachable as USB mass storage\n",
    );
    write_str(
        STDOUT_FILENO,
        "##################################################\n\n",
    );
}

// ---------------------------------------------------------------------------
// Bind + probe one mass-storage device
// ---------------------------------------------------------------------------

/// Drive the GET_MAX_LUN → TEST UNIT READY → INQUIRY → READ CAPACITY
/// sequence against a bound mass-storage device. Returns `Some(capacity)`
/// on a successful capacity read, `None` on failure.
#[cfg(not(test))]
fn probe_device(usb_ep: u32, notice: &AttachNotice) -> Option<ReadCapacity10> {
    let max_lun = get_max_lun(usb_ep, notice);
    write_str(STDOUT_FILENO, "usb-storage: max_lun=");
    write_u8_dec(max_lun);
    write_str(STDOUT_FILENO, "\n");

    // TEST UNIT READY.
    let mut bot_ok = false;
    for _ in 0..5 {
        match bot_command(usb_ep, notice, &cdb_test_unit_ready(), false, 0) {
            Some((_, status)) => {
                bot_ok = true;
                if status == 0 {
                    break;
                }
                let _ = syscall_lib::nanosleep_for(0, 20_000_000);
            }
            None => break,
        }
    }
    if bot_ok {
        write_str(STDOUT_FILENO, "USB_STORAGE:bot-ok\n");
    } else {
        write_str(STDOUT_FILENO, "usb-storage: BOT round-trip failed\n");
        return None;
    }

    // INQUIRY.
    let (inq_data, inq_status) = match bot_command(
        usb_ep,
        notice,
        &cdb_inquiry(INQUIRY_LEN as u8),
        true,
        INQUIRY_LEN,
    ) {
        Some(r) => r,
        None => {
            write_str(STDOUT_FILENO, "usb-storage: INQUIRY transport failed\n");
            return None;
        }
    };
    if inq_status != 0 {
        write_str(STDOUT_FILENO, "usb-storage: INQUIRY CSW status nonzero\n");
        return None;
    }
    if let Some(inq) = InquiryData::parse(&inq_data) {
        write_str(STDOUT_FILENO, "usb-storage: INQUIRY type=");
        write_u8_dec(inq.peripheral_device_type);
        write_str(
            STDOUT_FILENO,
            if inq.removable {
                " removable"
            } else {
                " fixed"
            },
        );
        write_str(STDOUT_FILENO, "\n");
        if inq.peripheral_device_type != SCSI_TYPE_DIRECT_ACCESS {
            write_str(
                STDOUT_FILENO,
                "usb-storage: not a direct-access block device — skipping\n",
            );
            return None;
        }
    }

    // READ CAPACITY(10).
    let (cap_data, cap_status) = match bot_command(
        usb_ep,
        notice,
        &cdb_read_capacity10(),
        true,
        READ_CAPACITY10_LEN,
    ) {
        Some(r) => r,
        None => {
            write_str(
                STDOUT_FILENO,
                "usb-storage: READ CAPACITY transport failed\n",
            );
            return None;
        }
    };
    if cap_status != 0 {
        write_str(
            STDOUT_FILENO,
            "usb-storage: READ CAPACITY CSW status nonzero\n",
        );
        return None;
    }
    let cap = ReadCapacity10::parse(&cap_data)?;
    let blocks = cap.last_lba.wrapping_add(1);
    write_str(STDOUT_FILENO, "USB_MASS_STORAGE:ready blocks=");
    write_u32_dec(blocks);
    write_str(STDOUT_FILENO, " bsize=");
    write_u32_dec(cap.block_size);
    write_str(STDOUT_FILENO, "\n");
    // Reaching here means BOT + INQUIRY + READ CAPACITY all succeeded: the
    // device is fully reachable as mass storage. Paint the photo-legible PASS
    // banner (bare-metal de-risk).
    print_storage_pass_banner(blocks, cap.block_size);

    Some(cap)
}

// ---------------------------------------------------------------------------
// Destructive self-test (scratch devices only)
// ---------------------------------------------------------------------------

/// Run the WRITE(10)/READ(10) self-test on scratch/blank media.
///
/// Should only be called after `detect_real_fs` returned `false`.
#[cfg(not(test))]
fn run_scratch_self_test(usb_ep: u32, notice: &AttachNotice, cap: &ReadCapacity10) {
    let blocks = cap.last_lba.wrapping_add(1);
    if cap.block_size as usize != 512 || blocks <= SCRATCH_LBA {
        return;
    }
    let mut pattern = [0u8; 512];
    for (i, b) in pattern.iter_mut().enumerate() {
        *b = (i as u8) ^ 0x5A;
    }
    match bot_command_write(usb_ep, notice, &cdb_write10(SCRATCH_LBA, 1), &pattern) {
        Some(0) => {}
        _ => {
            write_str(STDOUT_FILENO, "usb-storage: WRITE(10) self-test failed\n");
            return;
        }
    }
    match bot_command(usb_ep, notice, &cdb_read10(SCRATCH_LBA, 1), true, 512) {
        Some((rd, 0)) if rd.len() == 512 && rd[..] == pattern[..] => {
            write_str(STDOUT_FILENO, "USB_STORAGE:rw-ok\n");
        }
        _ => {
            write_str(
                STDOUT_FILENO,
                "usb-storage: READ(10) self-test verify mismatch\n",
            );
            return;
        }
    }
    // Phase 92a H.4 / D.5 — zero-copy DMA over a shared-memory region.
    if blocks > SCRATCH_LBA + 24 {
        shm_dma_self_test(usb_ep, notice);
    }
}

/// Submit a **zero-copy** bulk data stage over a shared-memory region and block
/// for completion (Phase 92a H.4 `SubmitShmTransfer`). The xHCI server maps the
/// region into the device's IOMMU domain and programs one TRB straight at it —
/// no inline `USB_MSG_MAX` copy. Returns the transferred byte count.
#[cfg(not(test))]
fn submit_shm(
    usb_ep: u32,
    slot_id: u8,
    dci: u8,
    shm_id: u32,
    len: u32,
    dir_in: bool,
) -> Option<usize> {
    match usb_call(
        usb_ep,
        &UsbRequest::SubmitShmTransfer {
            slot_id,
            dci,
            shm_id,
            len,
            dir_in,
        },
    ) {
        Some(UsbReply::TransferComplete {
            transferred,
            completion_code: 1,
        }) => Some(transferred),
        _ => None,
    }
}

/// BOT WRITE(10) whose data-OUT stage is a single zero-copy shm transfer.
#[cfg(not(test))]
fn bot_write_shm(
    usb_ep: u32,
    notice: &AttachNotice,
    cdb: &[u8],
    shm_id: u32,
    len: u32,
) -> Option<u8> {
    let slot_id = notice.slot_id;
    let tag = next_tag();
    let cbw = Cbw::new(tag, len, false, 0, cdb);
    if !bulk_out(usb_ep, slot_id, notice.bulk_out_dci, &cbw.encode()) {
        return None;
    }
    submit_shm(usb_ep, slot_id, notice.bulk_out_dci, shm_id, len, false)?;
    let csw = Csw::parse(&submit_bulk_in(
        usb_ep,
        slot_id,
        notice.bulk_in_dci,
        CSW_LEN as u16,
    )?)?;
    if csw.tag != tag {
        return None;
    }
    Some(csw.status)
}

/// BOT READ(10) whose data-IN stage is a single zero-copy shm transfer.
#[cfg(not(test))]
fn bot_read_shm(
    usb_ep: u32,
    notice: &AttachNotice,
    cdb: &[u8],
    shm_id: u32,
    len: u32,
) -> Option<u8> {
    let slot_id = notice.slot_id;
    let tag = next_tag();
    let cbw = Cbw::new(tag, len, true, 0, cdb);
    if !bulk_out(usb_ep, slot_id, notice.bulk_out_dci, &cbw.encode()) {
        return None;
    }
    submit_shm(usb_ep, slot_id, notice.bulk_in_dci, shm_id, len, true)?;
    let csw = Csw::parse(&submit_bulk_in(
        usb_ep,
        slot_id,
        notice.bulk_in_dci,
        CSW_LEN as u16,
    )?)?;
    if csw.tag != tag {
        return None;
    }
    Some(csw.status)
}

/// Zero-copy DMA self-test (Phase 92a H.4 / D.5): WRITE(10) then READ(10) a
/// **16-sector (8192-byte)** payload — larger than the ~4092-byte inline budget
/// — through a single shared-memory region the xHCI device DMAs into/out of
/// directly, and verify the round-trip byte-identical. Proves the zero-copy
/// `SubmitShmTransfer` path moves a >`USB_MSG_MAX` transfer in one descriptor.
/// Emits `USB_STORAGE:shm-dma-ok`.
#[cfg(not(test))]
fn shm_dma_self_test(usb_ep: u32, notice: &AttachNotice) {
    const SECTORS: u16 = 16;
    const NBYTES: usize = SECTORS as usize * 512;
    const TEST_LBA: u32 = SCRATCH_LBA + 8;
    let shm_id = syscall_lib::shm_create(NBYTES);
    if shm_id == 0 {
        write_str(STDOUT_FILENO, "usb-storage: shm_create failed\n");
        return;
    }
    let va = syscall_lib::shm_map(shm_id);
    if va == u64::MAX || va == 0 {
        write_str(STDOUT_FILENO, "usb-storage: shm_map failed\n");
        return;
    }
    let buf = va as *mut u8;
    // Fill the shared buffer with a recognizable pattern.
    for i in 0..NBYTES {
        // SAFETY: the shm region is NBYTES bytes mapped writable at `va`.
        unsafe { buf.add(i).write_volatile((i as u8) ^ 0x3C) };
    }
    // WRITE(10) the shm to the device (zero-copy data-OUT — device reads the shm).
    if bot_write_shm(
        usb_ep,
        notice,
        &cdb_write10(TEST_LBA, SECTORS),
        shm_id,
        NBYTES as u32,
    ) != Some(0)
    {
        write_str(STDOUT_FILENO, "usb-storage: shm WRITE failed\n");
        return;
    }
    // Clear the buffer, then READ(10) back into it (zero-copy data-IN).
    for i in 0..NBYTES {
        // SAFETY: as above.
        unsafe { buf.add(i).write_volatile(0) };
    }
    if bot_read_shm(
        usb_ep,
        notice,
        &cdb_read10(TEST_LBA, SECTORS),
        shm_id,
        NBYTES as u32,
    ) != Some(0)
    {
        write_str(STDOUT_FILENO, "usb-storage: shm READ failed\n");
        return;
    }
    // Verify the device DMA'd the original pattern back into the shared buffer.
    for i in 0..NBYTES {
        // SAFETY: as above.
        if unsafe { buf.add(i).read_volatile() } != ((i as u8) ^ 0x3C) {
            write_str(STDOUT_FILENO, "usb-storage: shm DMA verify mismatch\n");
            return;
        }
    }
    write_str(STDOUT_FILENO, "USB_STORAGE:shm-dma-ok\n");
}

// ---------------------------------------------------------------------------
// Detach cleanup (C.4)
// ---------------------------------------------------------------------------

/// Release a bound device slot. Called when a detach is observed (on the
/// discovery/rebind path or mid-serve). Logs the event so the caller can exit
/// or attempt re-discovery.
#[cfg(not(test))]
fn release_device(notice: &AttachNotice) {
    write_str(STDOUT_FILENO, "usb-storage: device detached slot=");
    write_u8_dec(notice.slot_id);
    write_str(STDOUT_FILENO, " — released\n");
}

/// Absolute CLOCK_MONOTONIC nanoseconds — the deadline base for
/// `ipc_recv_msg_timeout` (C.4 detach reconcile). Returns 0 if the clock read
/// fails (yielding an immediate timeout, which is self-correcting).
#[cfg(not(test))]
fn monotonic_ns() -> u64 {
    let (sec, nsec) = syscall_lib::clock_gettime(syscall_lib::CLOCK_MONOTONIC);
    if sec < 0 {
        return 0;
    }
    (sec as u64)
        .saturating_mul(1_000_000_000)
        .saturating_add(nsec as u64)
}

/// C.4: re-read the device's discovery-time `NextAttach` `cursor` and report
/// whether the device is gone. A `attached:false` entry, a missing entry, or a
/// transport error all count as detached (mirrors `usb-net`'s `device_detached`).
#[cfg(not(test))]
fn device_detached(usb_ep: u32, cursor: u8) -> bool {
    match usb_call(usb_ep, &UsbRequest::NextAttach { cursor }) {
        Some(UsbReply::Attach {
            notice: Some(notice),
        }) => !notice.attached,
        _ => true,
    }
}

/// Phase 106 hardening: require TWO consecutive detach verdicts ~300 ms
/// apart before tearing down. When this daemon serves the ROOT filesystem
/// (USB-root boot), a single failed `NextAttach` round-trip — the xHCI
/// server mid-recovery after a slow bulk transfer, a transient IPC hiccup —
/// must not unmount and exit: that turns a one-shot glitch into a dead
/// system. A real hot-unplug is permanent and passes both probes.
#[cfg(not(test))]
fn device_detached_confirmed(usb_ep: u32, cursor: u8) -> bool {
    if !device_detached(usb_ep, cursor) {
        return false;
    }
    let _ = syscall_lib::nanosleep_for(0, 300_000_000);
    device_detached(usb_ep, cursor)
}

// ---------------------------------------------------------------------------
// Block-server IPC loop (D.4)
// ---------------------------------------------------------------------------

/// Create a Phase 50 IPC endpoint and register it under `name`.
/// Returns the capability handle on success, `u64::MAX` on failure.
#[cfg(not(test))]
fn create_service_endpoint(name: &str) -> Option<u32> {
    let ep = syscall_lib::create_endpoint();
    if ep == u64::MAX {
        return None;
    }
    let ep_u32 = match u32::try_from(ep) {
        Ok(v) => v,
        Err(_) => return None,
    };
    let rc = syscall_lib::ipc_register_service(ep_u32, name);
    if rc == u64::MAX {
        return None;
    }
    Some(ep_u32)
}

/// Build a `BlkReplyHeader` for an Ok read reply.
#[cfg(not(test))]
fn ok_read_reply(cmd_id: u64, bytes: u32) -> BlkReplyHeader {
    BlkReplyHeader {
        cmd_id,
        status: BlockDriverError::Ok,
        bytes,
    }
}

/// Build a `BlkReplyHeader` for an error reply.
#[cfg(not(test))]
fn err_reply(cmd_id: u64) -> BlkReplyHeader {
    BlkReplyHeader {
        cmd_id,
        status: BlockDriverError::IoError,
        bytes: 0,
    }
}

/// Build a `BlkReplyHeader` for an Ok write/flush/status reply with no data.
#[cfg(not(test))]
fn ok_empty_reply(cmd_id: u64) -> BlkReplyHeader {
    BlkReplyHeader {
        cmd_id,
        status: BlockDriverError::Ok,
        bytes: 0,
    }
}

/// SCSI SYNCHRONIZE CACHE(10) CDB (opcode 0x35, 10 bytes, no data stage).
/// Requests the device to write its volatile cache to the medium.
#[cfg(not(test))]
fn cdb_sync_cache10() -> [u8; 10] {
    [0x35, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]
}

/// USB Mass Storage **BOT Reset Recovery** (BOT spec §5.3.4) plus the xHCI
/// controller-side endpoint recovery — the answer to the install-copy
/// `transport-fail` cascade (Phase 106).
///
/// The failure it unwinds: one bulk transfer exceeds the xHCI server's
/// completion-wait budget (host stalls of >5 s were observed under
/// battery-load TCG while QEMU's disk backends fought the host page cache)
/// and is abandoned mid-BOT-exchange. The device is now phase-desynced (it
/// still owes data / a CSW), so it STALLs the next CBW — and the STALL halts
/// the xHCI endpoint. Without recovery every later ≥8-sector transfer on the
/// device fails, including the kernel's own rootfs reads.
///
/// Sequence:
/// 1. `RecoverEndpoint` on both bulk pipes — the xHCI half (Stop/Reset
///    Endpoint + Set TR Dequeue + stale-event sweep).
/// 2. **Bulk-Only Mass Storage Reset** (class request `0xFF` to the
///    interface) — returns the device's BOT state machine to CBW-wait.
/// 3. `CLEAR_FEATURE(ENDPOINT_HALT)` on bulk-IN then bulk-OUT — clears the
///    device-side stall state + data toggles (pairing with Reset Endpoint's
///    TSP=0 on the controller side).
///
/// Returns `true` when every step succeeded; the caller then retries the
/// failed BOT command exactly once. Only TRANSPORT failures recover — a CSW
/// with a bad status is the device *answering* (BOT framing intact), so it
/// passes through as a normal command failure.
#[cfg(not(test))]
fn bot_recover(usb_ep: u32, notice: &AttachNotice) -> bool {
    write_str(STDOUT_FILENO, "usb-storage: BOT reset recovery start\n");
    let slot = notice.slot_id;
    let recover_ep = |dci: u8| -> bool {
        matches!(
            usb_call(usb_ep, &UsbRequest::RecoverEndpoint { slot_id: slot, dci }),
            Some(UsbReply::TransferComplete {
                completion_code: 1,
                ..
            })
        )
    };
    let control = |setup: [u8; 8]| -> bool {
        matches!(
            usb_call(
                usb_ep,
                &UsbRequest::ControlRequest {
                    slot_id: slot,
                    setup,
                    length: 0,
                },
            ),
            Some(UsbReply::ControlData {
                completion_code: 1,
                ..
            })
        )
    };

    let ep_recovered = recover_ep(notice.bulk_in_dci) & recover_ep(notice.bulk_out_dci);
    // Bulk-Only Mass Storage Reset: bmRequestType 0x21 (H2D, class,
    // interface), bRequest 0xFF, wIndex = interface, no data stage.
    let bot_reset = control([0x21, 0xFF, 0, 0, notice.interface_num, 0, 0, 0]);
    // CLEAR_FEATURE(ENDPOINT_HALT): bmRequestType 0x02 (H2D, standard,
    // endpoint), bRequest 1, wValue 0 (ENDPOINT_HALT), wIndex = endpoint
    // address. DCI = ep*2 + dir, so the endpoint number is dci >> 1 and the
    // IN address carries bit 7.
    let clear_in = control([0x02, 0x01, 0, 0, 0x80 | (notice.bulk_in_dci >> 1), 0, 0, 0]);
    let clear_out = control([0x02, 0x01, 0, 0, notice.bulk_out_dci >> 1, 0, 0, 0]);

    let ok = ep_recovered && bot_reset && clear_in && clear_out;
    write_str(
        STDOUT_FILENO,
        if ok {
            "usb-storage: BOT reset recovery ok — retrying command\n"
        } else {
            "usb-storage: BOT reset recovery FAILED\n"
        },
    );
    ok
}

/// Handle a `BLK_READ` by issuing chunked BOT READ(10) commands.
///
/// Multi-sector spans go through the zero-copy shm path when the bounce
/// buffer is available (`MAX_SHM_SECTORS`-sized SCSI commands, one TRB per
/// data stage); a missing bounce buffer or a ≤[`MAX_BOT_SECTORS`] tail uses
/// the inline `SubmitBulkIn` chunking.
///
/// Assembles all sector data into a single buffer, then stages it as a
/// combined bulk payload (header + data) via `ipc_store_reply_bulk`.
/// Returns the `BlkReplyHeader` with `bytes = sectors * 512` on success.
#[cfg(not(test))]
fn handle_bot_read(
    usb_ep: u32,
    notice: &AttachNotice,
    cmd_id: u64,
    lba: u64,
    sector_count: u32,
    shm: Option<&ShmBounce>,
) -> BlkReplyHeader {
    let mut all_data: Vec<u8> = Vec::with_capacity(sector_count as usize * 512);
    let mut remaining = sector_count;
    let mut current_lba = lba;

    while remaining > 0 {
        // Zero-copy stage for anything the inline path would have to split.
        if let Some(bounce) = shm
            && remaining > MAX_BOT_SECTORS as u32
        {
            let chunk = remaining.min(MAX_SHM_SECTORS);
            let byte_count = chunk * 512;
            let lba32 = current_lba as u32;
            let mut retried = false;
            loop {
                match bot_read_shm(
                    usb_ep,
                    notice,
                    &cdb_read10(lba32, chunk as u16),
                    bounce.id,
                    byte_count,
                ) {
                    Some(0) => {
                        // SAFETY: the bounce region is MAX_SHM_SECTORS × 512
                        // bytes mapped at `va`, and byte_count ≤ that. The xHCI
                        // server's completion reply happens-before this read, so
                        // the DMA'd stage is visible.
                        let stage =
                            unsafe { core::slice::from_raw_parts(bounce.va, byte_count as usize) };
                        all_data.extend_from_slice(stage);
                        break;
                    }
                    Some(status) => {
                        write_str(STDOUT_FILENO, "usb-storage: BLK_READ shm csw-status=");
                        write_u8_dec(status);
                        write_str(STDOUT_FILENO, " lba=");
                        write_u32_dec(lba32);
                        write_str(STDOUT_FILENO, " sectors=");
                        write_u32_dec(chunk);
                        write_str(STDOUT_FILENO, "\n");
                        return err_reply(cmd_id);
                    }
                    None => {
                        write_str(
                            STDOUT_FILENO,
                            "usb-storage: BLK_READ shm transport-fail lba=",
                        );
                        write_u32_dec(lba32);
                        write_str(STDOUT_FILENO, " sectors=");
                        write_u32_dec(chunk);
                        write_str(STDOUT_FILENO, "\n");
                        // Phase 106: one transport failure used to poison the
                        // pipe for good — reset-recover and retry this SCSI
                        // command exactly once.
                        if !retried && bot_recover(usb_ep, notice) {
                            retried = true;
                            continue;
                        }
                        return err_reply(cmd_id);
                    }
                }
            }
            remaining -= chunk;
            current_lba += chunk as u64;
            continue;
        }

        let chunk = remaining.min(MAX_BOT_SECTORS as u32) as u16;
        let byte_count = chunk * 512;
        let lba32 = current_lba as u32; // BOT READ(10) uses 32-bit LBA.
        let mut retried = false;
        loop {
            match bot_command(usb_ep, notice, &cdb_read10(lba32, chunk), true, byte_count) {
                Some((data, 0)) if data.len() == byte_count as usize => {
                    all_data.extend_from_slice(&data);
                    break;
                }
                // Bare-metal diagnostic: name the failure shape + LBA so the boot log
                // says WHY READ(10) failed instead of a bare "BOT error".
                //   short-read  → CSW passed but the bulk-IN data phase came up short
                //   csw-status  → device reported command-failed(1)/phase-error(2)
                //   transport   → CBW/CSW/bulk-IN transfer itself failed (None)
                Some((data, 0)) => {
                    write_str(STDOUT_FILENO, "usb-storage: BLK_READ BOT short-read lba=");
                    write_u32_dec(lba32);
                    write_str(STDOUT_FILENO, " got=");
                    write_u32_dec(data.len() as u32);
                    write_str(STDOUT_FILENO, " want=");
                    write_u32_dec(byte_count as u32);
                    write_str(STDOUT_FILENO, "\n");
                    return err_reply(cmd_id);
                }
                Some((_, status)) => {
                    write_str(STDOUT_FILENO, "usb-storage: BLK_READ BOT csw-status=");
                    write_u8_dec(status);
                    write_str(STDOUT_FILENO, " lba=");
                    write_u32_dec(lba32);
                    write_str(STDOUT_FILENO, " sectors=");
                    write_u8_dec(chunk as u8);
                    write_str(STDOUT_FILENO, "\n");
                    return err_reply(cmd_id);
                }
                None => {
                    write_str(
                        STDOUT_FILENO,
                        "usb-storage: BLK_READ BOT transport-fail lba=",
                    );
                    write_u32_dec(lba32);
                    write_str(STDOUT_FILENO, " sectors=");
                    write_u8_dec(chunk as u8);
                    write_str(STDOUT_FILENO, "\n");
                    // Phase 106: reset-recover + retry once (transport only).
                    if !retried && bot_recover(usb_ep, notice) {
                        retried = true;
                        continue;
                    }
                    return err_reply(cmd_id);
                }
            }
        }
        remaining -= chunk as u32;
        current_lba += chunk as u64;
    }

    let total_bytes = all_data.len() as u32;

    // Stage combined reply: header first, then bulk sector data.
    // The kernel BlockReply protocol carries bulk immediately after the header.
    let hdr = ok_read_reply(cmd_id, total_bytes);
    let header_bytes = encode_blk_reply(hdr, 0);
    let mut combined = Vec::with_capacity(header_bytes.len() + all_data.len());
    combined.extend_from_slice(&header_bytes);
    combined.extend_from_slice(&all_data);

    let rc = syscall_lib::ipc_store_reply_bulk(&combined);
    if rc == u64::MAX {
        write_str(
            STDOUT_FILENO,
            "usb-storage: ipc_store_reply_bulk (read) failed\n",
        );
        return err_reply(cmd_id);
    }

    // The combined (header + bulk) buffer has been staged via ipc_store_reply_bulk
    // above. Return a sentinel (bytes == u32::MAX) that run_block_server_loop
    // recognises as "bulk already staged — skip the second store_reply_bulk
    // call and go straight to ipc_reply." ipc_store_reply_bulk replaces the
    // staged buffer on each call, so without the sentinel the loop's header-
    // only encode would silently overwrite the sector payload we just staged.
    BlkReplyHeader {
        cmd_id,
        status: BlockDriverError::Ok,
        bytes: u32::MAX, // Sentinel: combined header+bulk already staged.
    }
}

/// Handle a `BLK_WRITE` by issuing chunked BOT WRITE(10) commands.
///
/// Mirrors [`handle_bot_read`]: multi-sector spans stage their data through
/// the shm bounce buffer (one `SubmitShmTransfer` data-OUT per SCSI command)
/// when it is available; the inline `SubmitBulkOut` chunking covers the
/// no-bounce fallback and ≤[`MAX_BOT_SECTORS`] tails.
#[cfg(not(test))]
fn handle_bot_write(
    usb_ep: u32,
    notice: &AttachNotice,
    cmd_id: u64,
    lba: u64,
    sector_count: u32,
    payload: &[u8],
    shm: Option<&ShmBounce>,
) -> BlkReplyHeader {
    let expected_bytes = sector_count as usize * 512;
    if payload.len() < expected_bytes {
        write_str(STDOUT_FILENO, "usb-storage: BLK_WRITE payload too short\n");
        return err_reply(cmd_id);
    }

    let mut remaining = sector_count;
    let mut current_lba = lba;
    let mut offset = 0usize;

    while remaining > 0 {
        if let Some(bounce) = shm
            && remaining > MAX_BOT_SECTORS as u32
        {
            let chunk = remaining.min(MAX_SHM_SECTORS);
            let byte_count = chunk as usize * 512;
            let lba32 = current_lba as u32;
            // SAFETY: the bounce region is MAX_SHM_SECTORS × 512 bytes mapped
            // writable at `va`, byte_count ≤ that, and the payload slice was
            // length-checked above. The copy completes before the IPC submit,
            // which is the device's happens-before edge.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    payload[offset..offset + byte_count].as_ptr(),
                    bounce.va,
                    byte_count,
                );
            }
            let mut retried = false;
            loop {
                match bot_write_shm(
                    usb_ep,
                    notice,
                    &cdb_write10(lba32, chunk as u16),
                    bounce.id,
                    byte_count as u32,
                ) {
                    Some(0) => break,
                    Some(status) => {
                        write_str(STDOUT_FILENO, "usb-storage: BLK_WRITE shm csw-status=");
                        write_u8_dec(status);
                        write_str(STDOUT_FILENO, " lba=");
                        write_u32_dec(lba32);
                        write_str(STDOUT_FILENO, "\n");
                        return err_reply(cmd_id);
                    }
                    None => {
                        write_str(
                            STDOUT_FILENO,
                            "usb-storage: BLK_WRITE shm transport-fail lba=",
                        );
                        write_u32_dec(lba32);
                        write_str(STDOUT_FILENO, "\n");
                        // Phase 106: reset-recover + retry once. The bounce
                        // buffer still holds this chunk's payload, so the
                        // retried WRITE(10) is byte-identical (idempotent).
                        if !retried && bot_recover(usb_ep, notice) {
                            retried = true;
                            continue;
                        }
                        return err_reply(cmd_id);
                    }
                }
            }
            remaining -= chunk;
            current_lba += chunk as u64;
            offset += byte_count;
            continue;
        }

        let chunk = remaining.min(MAX_BOT_SECTORS as u32) as u16;
        let byte_count = chunk as usize * 512;
        let lba32 = current_lba as u32;
        let chunk_payload = &payload[offset..offset + byte_count];
        let mut retried = false;
        loop {
            match bot_command_write(usb_ep, notice, &cdb_write10(lba32, chunk), chunk_payload) {
                Some(0) => break,
                Some(status) => {
                    write_str(STDOUT_FILENO, "usb-storage: BLK_WRITE BOT csw-status=");
                    write_u8_dec(status);
                    write_str(STDOUT_FILENO, " lba=");
                    write_u32_dec(lba32);
                    write_str(STDOUT_FILENO, "\n");
                    return err_reply(cmd_id);
                }
                None => {
                    write_str(
                        STDOUT_FILENO,
                        "usb-storage: BLK_WRITE BOT transport-fail lba=",
                    );
                    write_u32_dec(lba32);
                    write_str(STDOUT_FILENO, "\n");
                    // Phase 106: reset-recover + retry once (transport only;
                    // the payload slice is unchanged, so the retry is
                    // idempotent).
                    if !retried && bot_recover(usb_ep, notice) {
                        retried = true;
                        continue;
                    }
                    return err_reply(cmd_id);
                }
            }
        }
        remaining -= chunk as u32;
        current_lba += chunk as u64;
        offset += byte_count;
    }

    ok_empty_reply(cmd_id)
}

/// Handle a `BLK_FLUSH` by issuing SCSI SYNCHRONIZE CACHE(10).
///
/// Many BOT devices don't have a volatile cache, and some will fail the
/// command. We treat a CSW status 1 (failed) as non-fatal and return Ok
/// (the device simply has no cache to flush).
#[cfg(not(test))]
fn handle_bot_flush(usb_ep: u32, notice: &AttachNotice, cmd_id: u64) -> BlkReplyHeader {
    match bot_command(usb_ep, notice, &cdb_sync_cache10(), false, 0) {
        Some((_, 0)) | Some((_, 1)) => {
            // Status 0 = cache flushed; status 1 = no cache / not supported.
            ok_empty_reply(cmd_id)
        }
        _ => {
            // Transport error — return Ok anyway; a missing flush is not fatal
            // for a removable device whose caller cannot recover from it.
            ok_empty_reply(cmd_id)
        }
    }
}

// ---------------------------------------------------------------------------
// Entry
// ---------------------------------------------------------------------------

#[cfg(not(test))]
fn program_main(_args: &[&str]) -> i32 {
    write_str(STDOUT_FILENO, BOOT_LOG_MARKER);

    if !syscall_lib::ipc_wait_service(USB_SERVICE_NAME, 10_000) {
        write_str(
            STDOUT_FILENO,
            "usb-storage: 'usb' service never appeared — exiting cleanly\n",
        );
        return 0;
    }
    let Some(usb_ep) = lookup(USB_SERVICE_NAME) else {
        write_str(
            STDOUT_FILENO,
            "usb-storage: 'usb' lookup failed — exiting\n",
        );
        return 0;
    };

    // D.4: discover up to MAX_STICKS mass-storage devices (NextAttach walk;
    // tier-2 hub-enumerated devices arrive late, so it re-walks for ~30 s).
    let devices = discover_storage_devices(usb_ep);
    if devices.is_empty() {
        write_str(
            STDOUT_FILENO,
            "usb-storage: no mass-storage device attached — exiting cleanly\n",
        );
        print_storage_fail_banner();
        return 0;
    }

    // Phase 106 single-daemon guard: exactly one usb-storage process may
    // drive BOT traffic. On a USB-root boot, init's bootstrap fork already
    // serves the boot stick as `usb0.block`, and the service manager's
    // `usb_storage` daemon still starts afterwards — its probe (GET_MAX_LUN /
    // TEST UNIT READY / INQUIRY) would interleave raw BOT commands on the
    // SAME bulk pipes the live instance is mid-transfer on, corrupting both
    // streams (observed killing the installer's image copy at ~12%). The
    // kernel registry drops a dead owner's entries (`ipc/cleanup.rs`), so a
    // live `usb{k}.block` name means a live daemon: skip such devices BEFORE
    // any device traffic, and exit 0 (`restart=on-failure` must not respawn
    // us into the same collision) when every discovered device is already
    // served.
    let claimed = |k: u32| lookup(&service_name_for(k)).is_some();

    // Phase 106: one shared bounce region serves every device's multi-sector
    // transfers (both loops are single-threaded, so requests never overlap).
    let shm_bounce = setup_shm_bounce();

    if devices.len() == 1 {
        // Single-device path: the efficient blocking-with-timeout loop
        // (usb0.block; C.4-validated by usb-unmount-smoke). No
        // multi-device idle-poll latency on the common single-stick case.
        if claimed(0) {
            write_str(
                STDOUT_FILENO,
                "usb-storage: usb0.block already served — exiting cleanly (single-daemon)\n",
            );
            return 0;
        }
        let (notice, cursor) = devices[0];
        let (blk_ep, uas) = match prepare_and_register(usb_ep, &notice, 0) {
            Some(v) => v,
            // A lost registration race is the concurrent-daemon case again —
            // exit clean so on-failure restart does not re-probe a pipe
            // another daemon is actively serving.
            None if claimed(0) => return 0,
            None => return 1,
        };
        // `cursor` lets the loop re-query NextAttach for a C.4 hot-unplug.
        run_block_server_loop(usb_ep, blk_ep, &notice, uas, cursor, shm_bounce.as_ref());
        return 0;
    }

    // D.4 multi-device path: prepare + register each stick (usb0.block,
    // usb1.block, …) and serve them all from one event loop.
    write_str(STDOUT_FILENO, "usb-storage: ");
    write_u8_dec(devices.len() as u8);
    write_str(STDOUT_FILENO, " mass-storage devices — multi-device mode\n");
    let mut active: Vec<StorageDevice> = Vec::new();
    let mut skipped_claimed = false;
    for (k, (notice, cursor)) in devices.into_iter().enumerate() {
        if claimed(k as u32) {
            write_str(STDOUT_FILENO, "usb-storage: usb");
            write_u8_dec(k as u8);
            write_str(STDOUT_FILENO, ".block already served — skipping device\n");
            skipped_claimed = true;
            continue;
        }
        match prepare_and_register(usb_ep, &notice, k as u32) {
            Some((blk_ep, uas)) => active.push(StorageDevice {
                notice,
                cursor,
                index: k as u32,
                blk_ep,
                uas,
            }),
            None => {
                write_str(
                    STDOUT_FILENO,
                    "usb-storage: skipping a device (prepare failed)\n",
                );
            }
        }
    }
    if active.is_empty() {
        // Nothing left to serve. If devices were skipped because another
        // daemon owns them, that is the expected single-daemon posture —
        // exit clean (no restart). Only genuine prepare failures restart.
        return if skipped_claimed { 0 } else { 1 };
    }
    run_multi_block_server_loop(usb_ep, active, shm_bounce.as_ref());

    0
}

// ---------------------------------------------------------------------------
// Multi-device support (D.4)
// ---------------------------------------------------------------------------

/// Maximum concurrent USB sticks the daemon serves. The kernel `blk::remote`
/// registry holds `MAX_REMOTE_BLOCK`=4 devices; slot 0 is the root disk, so
/// slots 1..=3 (→ `usb0`/`usb1`/`usb2`, mounted `/mnt/usb0`..`/mnt/usb2`) are
/// available for USB mass storage.
#[cfg(not(test))]
const MAX_STICKS: usize = 3;

/// One bound mass-storage device served by the multi-device loop.
#[cfg(not(test))]
struct StorageDevice {
    /// The device's `AttachNotice` (bulk DCIs + slot for BOT transfers).
    notice: AttachNotice,
    /// `NextAttach` cursor where it was discovered (for the C.4 detach probe).
    cursor: u8,
    /// 0-based stick index → `usb{index}.block` / `/mnt/usb{index}`.
    index: u32,
    /// The registered block-service endpoint.
    blk_ep: u32,
    /// UAS vs BOT transport selection (BOT is the live path; D.3).
    uas: bool,
}

/// `usb{index}.block` service name for a stick index.
#[cfg(not(test))]
fn service_name_for(index: u32) -> alloc::string::String {
    alloc::format!("usb{index}.block")
}

/// NUL-terminated `/mnt/usb{index}` mount prefix for the `umount` syscall.
/// `index < MAX_STICKS` (single digit), so a one-byte append suffices.
#[cfg(not(test))]
fn mount_prefix_cstr(index: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity(11);
    v.extend_from_slice(b"/mnt/usb");
    v.push(b'0' + (index as u8));
    v.push(0);
    v
}

/// Serve one decoded block request (shared by the single- and multi-device
/// loops). Decodes the request, dispatches to the BOT handlers, and stages +
/// sends the reply. For `BLK_READ` the combined (header+bulk) buffer is staged
/// inside `handle_bot_read` and signalled via the `bytes == u32::MAX` sentinel
/// so the second store call is skipped.
#[cfg(not(test))]
fn serve_block_request(
    usb_ep: u32,
    notice: &AttachNotice,
    uas: bool,
    msg: &IpcMessage,
    recv_buf: &[u8],
    shm: Option<&ShmBounce>,
) {
    use kernel_core::driver_ipc::block::BLK_REQUEST_HEADER_SIZE;

    let reply_cap = msg.data[3] as u32;
    if reply_cap == 0 {
        // Notification or fire-and-forget — no reply expected.
        return;
    }
    let real_len = (msg.data[1] as usize).min(recv_buf.len());

    let (reply_header, already_staged) = match decode_blk_request(&recv_buf[..real_len]) {
        Ok((req_hdr, _payload_grant)) => {
            let write_payload = if real_len > BLK_REQUEST_HEADER_SIZE {
                &recv_buf[BLK_REQUEST_HEADER_SIZE..real_len]
            } else {
                &[][..]
            };

            // TODO(92a D.3): UAS transport — when `uas` is true and the live UAS
            // IU datapath lands, route SCSI over CommandIu/SenseIu/ResponseIu
            // instead of the BOT helpers below. BOT is the live transport today.
            let _ = uas;

            match req_hdr.kind {
                BLK_READ => {
                    let hdr = handle_bot_read(
                        usb_ep,
                        notice,
                        req_hdr.cmd_id,
                        req_hdr.lba,
                        req_hdr.sector_count,
                        shm,
                    );
                    let already = hdr.bytes == u32::MAX;
                    (hdr, already)
                }
                BLK_WRITE => {
                    let hdr = handle_bot_write(
                        usb_ep,
                        notice,
                        req_hdr.cmd_id,
                        req_hdr.lba,
                        req_hdr.sector_count,
                        write_payload,
                        shm,
                    );
                    (hdr, false)
                }
                BLK_FLUSH => {
                    let hdr = handle_bot_flush(usb_ep, notice, req_hdr.cmd_id);
                    (hdr, false)
                }
                BLK_STATUS => (ok_empty_reply(req_hdr.cmd_id), false),
                _ => (
                    BlkReplyHeader {
                        cmd_id: req_hdr.cmd_id,
                        status: BlockDriverError::InvalidRequest,
                        bytes: 0,
                    },
                    false,
                ),
            }
        }
        Err(_) => (
            BlkReplyHeader {
                cmd_id: 0,
                status: BlockDriverError::InvalidRequest,
                bytes: 0,
            },
            false,
        ),
    };

    if !already_staged {
        let reply_bytes = encode_blk_reply(reply_header, 0);
        let rc_bulk = syscall_lib::ipc_store_reply_bulk(&reply_bytes);
        if rc_bulk == u64::MAX {
            write_str(STDOUT_FILENO, "usb-storage: ipc_store_reply_bulk failed\n");
        }
    }

    let rc_reply = syscall_lib::ipc_reply(reply_cap, 0, 0);
    if rc_reply == u64::MAX {
        write_str(STDOUT_FILENO, "usb-storage: ipc_reply failed\n");
    }
}

/// Walk `NextAttach` and collect up to [`MAX_STICKS`] attached mass-storage
/// devices (each with a bulk IN+OUT pair) plus the cursor each was found at.
/// Re-walks every ~200 ms for up to ~30 s; once a non-empty set is stable for a
/// few walks it returns, so co-present sticks that enumerate slightly apart are
/// all caught while a lone stick is not delayed by waiting for a phantom second.
#[cfg(not(test))]
fn discover_storage_devices(usb_ep: u32) -> Vec<(AttachNotice, u8)> {
    const POLL_INTERVAL_MS: u32 = 200;
    const MAX_POLLS: u32 = 150; // 150 × 200 ms = 30 s
    const STABLE_WALKS: u32 = 3; // ~600 ms with no growth ⇒ set is complete

    let mut best: Vec<(AttachNotice, u8)> = Vec::new();
    let mut stable = 0u32;

    for _ in 0..MAX_POLLS {
        let mut current: Vec<(AttachNotice, u8)> = Vec::new();
        let mut cursor = 0u8;
        loop {
            match usb_call(usb_ep, &UsbRequest::NextAttach { cursor }) {
                Some(UsbReply::Attach {
                    notice: Some(notice),
                }) => {
                    if notice.attached
                        && notice.interface_class == CLASS_MASS_STORAGE
                        && notice.bulk_in_dci != 0
                        && notice.bulk_out_dci != 0
                    {
                        current.push((notice, cursor));
                        if current.len() >= MAX_STICKS {
                            break;
                        }
                    }
                    cursor = cursor.saturating_add(1);
                }
                Some(UsbReply::Attach { notice: None }) | None => break,
                _ => cursor = cursor.saturating_add(1),
            }
        }

        if current.len() > best.len() {
            best = current;
            stable = 0;
        } else if !best.is_empty() {
            stable += 1;
            if stable >= STABLE_WALKS {
                break;
            }
        }

        let _ = syscall_lib::nanosleep_for(0, POLL_INTERVAL_MS * 1_000_000);
    }

    for (notice, _) in &best {
        write_str(STDOUT_FILENO, "usb-storage: bound mass-storage slot=");
        write_u8_dec(notice.slot_id);
        write_str(STDOUT_FILENO, " in_dci=");
        write_u8_dec(notice.bulk_in_dci);
        write_str(STDOUT_FILENO, " out_dci=");
        write_u8_dec(notice.bulk_out_dci);
        write_str(STDOUT_FILENO, "\n");
    }
    best
}

/// Probe a discovered device, run the scratch self-test only on blank media, and
/// register its `usb{index}.block` backend. Returns `(blk_ep, uas)` on success.
#[cfg(not(test))]
fn prepare_and_register(usb_ep: u32, notice: &AttachNotice, index: u32) -> Option<(u32, bool)> {
    // (D.3) Transport selection: fetch config descriptor, scan for UAS.
    let uas = select_transport(usb_ep, notice);

    // Probe: GET_MAX_LUN → TEST UNIT READY (bot-ok) → INQUIRY → READ CAPACITY.
    let cap = match probe_device(usb_ep, notice) {
        Some(c) => c,
        None => {
            write_str(STDOUT_FILENO, "usb-storage: probe failed\n");
            return None;
        }
    };

    // (D.4 safety gate) Detect a real filesystem before any destructive write.
    if detect_real_fs(usb_ep, notice) {
        write_str(
            STDOUT_FILENO,
            "usb-storage: real-fs detected — skipping destructive self-test\n",
        );
    } else {
        write_str(
            STDOUT_FILENO,
            "usb-storage: scratch device — running rw self-test\n",
        );
        run_scratch_self_test(usb_ep, notice, &cap);
    }

    // Register the block backend as usb{index}.block.
    let svc = service_name_for(index);
    let blk_ep = match create_service_endpoint(&svc) {
        Some(ep) => ep,
        None => {
            write_str(STDOUT_FILENO, "usb-storage: failed to register ");
            write_str(STDOUT_FILENO, &svc);
            write_str(STDOUT_FILENO, "\n");
            return None;
        }
    };
    write_str(STDOUT_FILENO, "usb-storage: registered ");
    write_str(STDOUT_FILENO, &svc);
    write_str(STDOUT_FILENO, "\n");
    Some((blk_ep, uas))
}

/// Multi-device block-server loop (D.4): serves N≥2 sticks from one process.
///
/// m3OS's single-threaded userspace (the native `BrkAllocator` is not
/// thread-safe) means N devices cannot each block on their own endpoint in a
/// thread; instead this is the single-event-loop pattern (the analog of Track F
/// multi-controller servicing): round-robin `ipc_try_recv_msg` across every
/// device's block endpoint, serving any pending request immediately, and when a
/// full round is idle, run the C.4 detach reconcile across all devices and sleep
/// briefly. The single-device case keeps the efficient blocking
/// `run_block_server_loop` (no poll latency); this path trades a small idle-poll
/// latency for serving multiple sticks without threads.
#[cfg(not(test))]
fn run_multi_block_server_loop(
    usb_ep: u32,
    mut devices: Vec<StorageDevice>,
    shm: Option<&ShmBounce>,
) {
    use kernel_core::driver_ipc::block::BLK_REQUEST_HEADER_SIZE;
    use kernel_core::driver_ipc::block::MAX_SECTORS_PER_REQUEST;

    let recv_cap = BLK_REQUEST_HEADER_SIZE + (MAX_SECTORS_PER_REQUEST as usize) * 512;
    let mut recv_buf = alloc::vec![0u8; recv_cap];
    let mut last_detach_ns = monotonic_ns();

    loop {
        // Drain any pending request on each device's endpoint (non-blocking).
        let mut served_any = false;
        let mut i = 0;
        while i < devices.len() {
            let ep = devices[i].blk_ep;
            let mut msg = IpcMessage::new(0);
            let rc = syscall_lib::ipc_try_recv_msg(ep, &mut msg, &mut recv_buf);
            if rc != u64::MAX {
                serve_block_request(
                    usb_ep,
                    &devices[i].notice,
                    devices[i].uas,
                    &msg,
                    &recv_buf,
                    shm,
                );
                served_any = true;
            }
            i += 1;
        }
        if served_any {
            // Stay in the fast path while any device is busy.
            continue;
        }

        // Idle round: C.4 detach reconcile across all devices (rate-limited).
        let now = monotonic_ns();
        if now.saturating_sub(last_detach_ns) >= DETACH_POLL_INTERVAL_MS * 1_000_000 {
            last_detach_ns = now;
            let mut j = 0;
            while j < devices.len() {
                if device_detached_confirmed(usb_ep, devices[j].cursor) {
                    let index = devices[j].index;
                    let prefix = mount_prefix_cstr(index);
                    let rc_um = syscall_lib::umount(&prefix);
                    write_str(
                        STDOUT_FILENO,
                        if rc_um == 0 {
                            "USB_STORAGE:detached-unmounted /mnt/usb"
                        } else {
                            "USB_STORAGE:detached (no live mount) /mnt/usb"
                        },
                    );
                    write_u8_dec(index as u8);
                    write_str(STDOUT_FILENO, "\n");
                    release_device(&devices[j].notice);
                    devices.remove(j);
                    // Do not advance `j`: the next device shifted into this slot.
                    continue;
                }
                j += 1;
            }
            if devices.is_empty() {
                write_str(
                    STDOUT_FILENO,
                    "usb-storage: all devices detached — exiting\n",
                );
                return;
            }
        }

        // Brief idle sleep to avoid a busy-spin while no device has traffic.
        let _ = syscall_lib::nanosleep_for(0, 1_000_000);
    }
}

/// Block-server dispatch loop (single device).
///
/// Mirrors the `BlockServer::handle_next` contract via [`serve_block_request`]:
/// recv → decode_blk_request → dispatch → encode_blk_reply → ipc_store_reply_bulk
/// → ipc_reply. For `BLK_READ` the combined (header+bulk) buffer is staged inside
/// `handle_bot_read` and signalled via the `bytes == u32::MAX` sentinel so the
/// loop skips the second store call.
///
/// C.4: the recv is bounded by `ipc_recv_msg_timeout` (≈`DETACH_POLL_INTERVAL`).
/// On a deadline expiry (no block request in the window) the loop re-queries
/// `NextAttach` at the device's discovery `cursor`; if the device was
/// hot-unplugged it unmounts `/mnt/usb0` (freeing the kernel `blk::remote`
/// slot) and returns, so a removed stick no longer leaves a stale mount. An
/// active filesystem keeps the timeout from firing, so steady-state I/O is
/// unaffected.
#[cfg(not(test))]
fn run_block_server_loop(
    usb_ep: u32,
    blk_ep: u32,
    notice: &AttachNotice,
    uas: bool,
    cursor: u8,
    shm: Option<&ShmBounce>,
) {
    use kernel_core::driver_ipc::block::BLK_REQUEST_HEADER_SIZE;
    use kernel_core::driver_ipc::block::MAX_SECTORS_PER_REQUEST;

    let recv_cap = BLK_REQUEST_HEADER_SIZE + (MAX_SECTORS_PER_REQUEST as usize) * 512;
    let mut recv_buf = alloc::vec![0u8; recv_cap];
    let mut consecutive_errors: u32 = 0;

    loop {
        let mut msg = IpcMessage::new(0);
        let deadline = monotonic_ns().saturating_add(DETACH_POLL_INTERVAL_MS * 1_000_000);
        let rc = syscall_lib::ipc_recv_msg_timeout(blk_ep, &mut msg, &mut recv_buf, deadline);

        if rc == NEG_ETIMEDOUT {
            // C.4: idle window elapsed — has the device been hot-unplugged?
            if device_detached_confirmed(usb_ep, cursor) {
                let rc_um = syscall_lib::umount(MOUNT_PREFIX_CSTR);
                if rc_um == 0 {
                    write_str(STDOUT_FILENO, "USB_STORAGE:detached-unmounted /mnt/usb0\n");
                } else {
                    // The device was registered but never mounted (or already
                    // unmounted) — benign; still tear down and exit.
                    write_str(
                        STDOUT_FILENO,
                        "USB_STORAGE:detached (no live mount) /mnt/usb0\n",
                    );
                }
                release_device(notice);
                return;
            }
            continue;
        }
        if rc == u64::MAX {
            consecutive_errors += 1;
            write_str(
                STDOUT_FILENO,
                "usb-storage: ipc_recv_msg error — continuing\n",
            );
            if consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                write_str(
                    STDOUT_FILENO,
                    "usb-storage: too many consecutive recv errors — exiting for restart\n",
                );
                return;
            }
            continue;
        }
        consecutive_errors = 0;
        serve_block_request(usb_ep, notice, uas, &msg, &recv_buf, shm);
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // find_uas_interface tests (D.3 host-testable helper)
    // -----------------------------------------------------------------------

    /// Build a minimal 9-byte Interface Descriptor for testing.
    fn interface_desc(class: u8, protocol: u8) -> Vec<u8> {
        vec![
            9,    // bLength
            0x04, // bDescriptorType = Interface
            0,    // bInterfaceNumber
            0,    // bAlternateSetting
            2,    // bNumEndpoints
            class, 0x06, // bInterfaceSubClass (SCSI)
            protocol, 0, // iInterface
        ]
    }

    /// A minimal Configuration Descriptor header (9 bytes) followed by an
    /// interface descriptor.
    fn config_with_interface(class: u8, protocol: u8) -> Vec<u8> {
        let iface = interface_desc(class, protocol);
        let total_len = (9 + iface.len()) as u16;
        let mut cfg = vec![
            9,    // bLength
            0x02, // bDescriptorType = Configuration
            total_len as u8,
            (total_len >> 8) as u8,
            1,    // bNumInterfaces
            1,    // bConfigurationValue
            0,    // iConfiguration
            0x80, // bmAttributes
            50,   // bMaxPower
        ];
        cfg.extend_from_slice(&iface);
        cfg
    }

    /// Config descriptor with a UAS mass-storage interface → returns true.
    #[test]
    fn find_uas_interface_detects_uas() {
        let config = config_with_interface(CLASS_MASS_STORAGE, PROTOCOL_UAS);
        assert!(find_uas_interface(&config));
    }

    /// Config descriptor with a BOT mass-storage interface → returns false.
    #[test]
    fn find_uas_interface_rejects_bot() {
        let config = config_with_interface(CLASS_MASS_STORAGE, PROTOCOL_BOT);
        assert!(!find_uas_interface(&config));
    }

    /// Config descriptor with a non-mass-storage UAS-protocol interface
    /// (e.g. HID with protocol 0x62) → returns false (class mismatch).
    #[test]
    fn find_uas_interface_requires_mass_storage_class() {
        let config = config_with_interface(0x03 /* HID */, PROTOCOL_UAS);
        assert!(!find_uas_interface(&config));
    }

    /// Empty config descriptor → returns false without panicking.
    #[test]
    fn find_uas_interface_empty_returns_false() {
        assert!(!find_uas_interface(&[]));
    }

    /// A config descriptor with multiple interfaces: one BOT, one UAS →
    /// returns true (UAS is present).
    #[test]
    fn find_uas_interface_multiple_interfaces_finds_uas() {
        let bot_iface = interface_desc(CLASS_MASS_STORAGE, PROTOCOL_BOT);
        let uas_iface = interface_desc(CLASS_MASS_STORAGE, PROTOCOL_UAS);
        let total_len = (9 + bot_iface.len() + uas_iface.len()) as u16;
        let mut config = vec![
            9,    // bLength
            0x02, // bDescriptorType = Configuration
            total_len as u8,
            (total_len >> 8) as u8,
            2, // bNumInterfaces
            1, // bConfigurationValue
            0,
            0x80,
            50,
        ];
        config.extend_from_slice(&bot_iface);
        config.extend_from_slice(&uas_iface);
        assert!(find_uas_interface(&config));
    }

    /// A truncated / corrupt descriptor (bLength > remaining bytes) terminates
    /// gracefully and returns false.
    #[test]
    fn find_uas_interface_truncated_descriptor_returns_false() {
        // bLength = 9 but only 5 bytes in the buffer.
        let config = vec![9, 0x04, 0, 0, 2];
        assert!(!find_uas_interface(&config));
    }

    /// A zero-length descriptor terminates the scan immediately.
    #[test]
    fn find_uas_interface_zero_length_terminates() {
        let config = vec![0, 0x04, 0, 0, 2, CLASS_MASS_STORAGE, 0, PROTOCOL_UAS, 0];
        assert!(!find_uas_interface(&config));
    }
}
