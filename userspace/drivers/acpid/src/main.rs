//! `acpid` — the ring-3 ACPI daemon (Phase 101 Tracks D/E).
//!
//! Hosts the AML interpreter *outside* ring 0 (the Track E split): at
//! start it fetches the FACP/DSDT/SSDT blobs through the read-only
//! `SYS_ACPI_TABLE_GET` accessor, builds the
//! [`kernel_core::acpi::namespace::Namespace`], registers the `"acpi"`
//! IPC query service, subscribes the SCI, performs the ACPI-enable
//! handshake, and dispatches PM1/GPE events. The kernel keeps only the
//! level-SCI hardware ack (demux + enable-mask in the ISR); every policy
//! decision — which `_Lxx` method to run, what a power-button press
//! means — happens here.
//!
//! ## IPC protocol (service name `"acpi"`)
//!
//! - [`ACPI_FIND_BY_HID`]: `data[0]` = HID text length, bulk = HID text
//!   (`"DLL0945"`, `"PNP0C0D"`, …). Reply label 0 with bulk = the full
//!   ASL path of the first present match, or [`REPLY_ERR`].
//! - [`ACPI_GET_CRS`]: `data[0]` = path length, bulk = ASL path from
//!   `FindByHid`. Reply label 0 with bulk = the raw `_CRS` resource
//!   template (decode with `kernel_core::acpi::resource::decode_crs`),
//!   or [`REPLY_ERR`].
//! - [`ACPI_STA`]: same request shape; the `_STA` value rides the reply
//!   label offset past the error sentinel — reply label =
//!   [`REPLY_STA_BASE`]` | sta` (sta ≤ 0xFF), or [`REPLY_ERR`].
//! - [`ACPI_SUBSCRIBE`] (Phase 101 D.5/E.4): `data[0]` = length, bulk =
//!   the subscriber's REGISTERED event-service name; acpid resolves it
//!   via `ipc_lookup_service` for its own send handle. (Not a cap
//!   transfer: `grant_task_cap` is move-semantics, so transferring the
//!   endpoint cap would strip the subscriber's only receive handle —
//!   the registry hands out independent send handles while the owner
//!   keeps receiving, the established m3OS push idiom.) Reply label 0
//!   on success, [`REPLY_ERR`] when the table is full or the name does
//!   not resolve. Thereafter every routed event is PUSHED to the
//!   subscriber's endpoint as an [`ACPI_EVENT`]-labelled message:
//!   `data0` = the notify code, bulk = the source ASL path. Two event
//!   sources ride this: AML `Notify(dev, code)` drained after GPE method
//!   evaluation (the real D.5 path), and the PM1 fixed power button,
//!   which has no AML device on QEMU and is pushed with the pseudo-path
//!   [`FIXED_PWRBTN_PATH`] + code `0x80`. A dead subscriber (send
//!   failure) is dropped. acpid's serve loop deliberately stays on
//!   `ipc_recv_with_caps` — it exercises the (fixed) bound-notification
//!   classification of that kernel path on every SCI event.
//!
//! ## Region backend (Phase 101 E.3 — real since this slice)
//!
//! `OperationRegion` accesses during method evaluation go through
//! [`SyscallRegionSpace`]: `SystemIO` (space 1) rides
//! `SYS_ACPI_IO_READ/WRITE` (raw port, `/drivers/`-gated) and
//! `SystemMemory` (space 0) rides `SYS_ACPI_MEM_READ/WRITE` (kernel
//! linear physical map). 64-bit field chunks split into two 32-bit
//! accesses. `PCI_Config` (space 2) and `EmbeddedControl` (space 3)
//! return [`AmlError::RegionAccess`] — PCI regions need the enclosing
//! device's `_ADR`/`_SEG`/`_BBN` context the interpreter does not yet
//! thread through (documented residual), and the EC transport is the
//! Phase 103 work. Boot runs two self-probes through this backend (a
//! `SystemIO` read of the FADT's PM1a status port and a `SystemMemory`
//! read of the DSDT signature) so `acpi-smoke` proves the syscall path
//! end-to-end without depending on which methods the DSDT evaluates.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::alloc::Layout;

use kernel_core::acpi::aml::object::{AmlError, RegionSpace};
use kernel_core::acpi::namespace::Namespace;
use kernel_core::device_host::syscalls::{
    ACPI_PM_REG_GPE0_STS, ACPI_PM_REG_PM1A_CNT, ACPI_PM_REG_PM1A_EN, ACPI_PM_REG_PM1A_STS,
    ACPI_PM_REG_SMI_CMD, ACPI_SCI_BIT_GPE, ACPI_SCI_BIT_PM1, NOTIFICATION_SENTINEL_NEW,
    SYS_ACPI_IO_READ, SYS_ACPI_IO_WRITE, SYS_ACPI_MEM_READ, SYS_ACPI_MEM_WRITE, SYS_ACPI_PM_READ,
    SYS_ACPI_PM_WRITE, SYS_ACPI_SCI_SUBSCRIBE, SYS_ACPI_TABLE_GET,
};
use kernel_core::ipc::wake_kind::RECV_KIND_NOTIFICATION;
use syscall_lib::heap::BrkAllocator;
use syscall_lib::{IpcMessage, STDOUT_FILENO, write_str};

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    write_str(STDOUT_FILENO, "acpid: alloc error\n");
    syscall_lib::exit(99)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "acpid: PANIC\n");
    syscall_lib::exit(101)
}

/// Mirror diagnostics into the kernel dmesg ring (`[userspace] …`) so a
/// bare-metal boot is observable over `dmesg` via SSH — the ring-3 fd 1
/// is invisible there (the Phase 100 usbhub lesson).
fn klog(msg: &str) {
    syscall_lib::serial_print(msg);
}

/// Emit to BOTH the console (serial gates) and dmesg (bare metal).
fn announce(msg: &str) {
    write_str(STDOUT_FILENO, msg);
    klog(msg);
}

// ---------------------------------------------------------------------
// IPC protocol
// ---------------------------------------------------------------------

const ACPI_SERVICE_NAME: &str = "acpi";
/// Request labels start at 2: label 1 is `RECV_KIND_NOTIFICATION`'s wake
/// value, and keeping them disjoint makes traces unambiguous.
const ACPI_FIND_BY_HID: u64 = 2;
const ACPI_GET_CRS: u64 = 3;
const ACPI_STA: u64 = 4;
/// Phase 101 D.5/E.4 — register an event subscriber (cap-transfer).
const ACPI_SUBSCRIBE: u64 = 5;
/// Label on every event message pushed to a subscriber's endpoint.
const ACPI_EVENT: u64 = 6;
/// Reply label for any failure (unknown label, bad request, no match).
const REPLY_ERR: u64 = u64::MAX;
/// `ACPI_STA` success replies are `REPLY_STA_BASE | sta`.
const REPLY_STA_BASE: u64 = 0x100;

/// Bounded subscriber table (Phase 102/103 clients + smoke harnesses).
const MAX_SUBSCRIBERS: usize = 8;
/// Pseudo-path pushed for the PM1 fixed power button (which has no AML
/// device node on QEMU's q35; control-method buttons Notify their own
/// PNP0C0C device instead and arrive with a real path).
const FIXED_PWRBTN_PATH: &str = "\\FIXED.PWRBTN";
/// Conventional "device status change" notify code (also what a
/// control-method power button sends).
const NOTIFY_STATUS_CHANGE: u64 = 0x80;

/// Largest request/reply payload: a namespace path or `_CRS` template.
const MSG_BUF: usize = 1024;

// ---------------------------------------------------------------------
// PM1 fixed-event bits (ACPI 6.5 §4.8.4.1.1)
// ---------------------------------------------------------------------

const PM1_PWRBTN: u64 = 1 << 8;
const PM1_CNT_SCI_EN: u64 = 1 << 0;

// ---------------------------------------------------------------------
// Raw platform-ACPI syscalls
// ---------------------------------------------------------------------

fn sys_table_get(sig: &[u8; 4], index: usize, buf: &mut [u8]) -> isize {
    unsafe {
        syscall_lib::syscall4(
            SYS_ACPI_TABLE_GET,
            sig.as_ptr() as u64,
            index as u64,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        ) as isize
    }
}

fn pm_read(sel: u64, idx: u64) -> isize {
    unsafe { syscall_lib::syscall2(SYS_ACPI_PM_READ, sel, idx) as isize }
}

fn pm_write(sel: u64, idx: u64, value: u64) -> isize {
    unsafe { syscall_lib::syscall3(SYS_ACPI_PM_WRITE, sel, idx, value) as isize }
}

fn sci_subscribe() -> isize {
    unsafe {
        syscall_lib::syscall1(SYS_ACPI_SCI_SUBSCRIBE, NOTIFICATION_SENTINEL_NEW as u64) as isize
    }
}

/// Fetch a whole table: size query, then a sized copy.
fn fetch_table(sig: &[u8; 4], index: usize) -> Option<Vec<u8>> {
    let len = sys_table_get(sig, index, &mut []);
    if len <= 0 {
        return None;
    }
    let mut buf = alloc::vec![0u8; len as usize];
    let again = sys_table_get(sig, index, &mut buf);
    if again != len {
        return None;
    }
    Some(buf)
}

// ---------------------------------------------------------------------
// Region backend (Phase 101 E.3 — real syscall-backed; see module doc)
// ---------------------------------------------------------------------

fn acpi_io_read(port: u64, width_bytes: u64) -> isize {
    unsafe { syscall_lib::syscall2(SYS_ACPI_IO_READ, port, width_bytes) as isize }
}

fn acpi_io_write(port: u64, width_bytes: u64, value: u64) -> isize {
    unsafe { syscall_lib::syscall3(SYS_ACPI_IO_WRITE, port, width_bytes, value) as isize }
}

fn acpi_mem_read(phys: u64, width_bytes: u64) -> isize {
    unsafe { syscall_lib::syscall2(SYS_ACPI_MEM_READ, phys, width_bytes) as isize }
}

fn acpi_mem_write(phys: u64, width_bytes: u64, value: u64) -> isize {
    unsafe { syscall_lib::syscall3(SYS_ACPI_MEM_WRITE, phys, width_bytes, value) as isize }
}

/// AML region-space bytes (ACPI 6.5 §19.6.102) this backend serves.
const SPACE_SYSTEM_MEMORY: u8 = 0;
const SPACE_SYSTEM_IO: u8 = 1;

/// The real `RegionSpace`: `SystemIO`/`SystemMemory` route to the
/// `/drivers/`-gated E.3 syscalls; 64-bit field chunks split into two
/// 32-bit accesses (the syscall value rides a positive `isize`).
/// `PCI_Config`/`EmbeddedControl` are documented residuals →
/// [`AmlError::RegionAccess`]. Counters feed the boot log so regressions
/// in region traffic are visible on serial.
struct SyscallRegionSpace {
    io_reads: u64,
    io_writes: u64,
    mem_reads: u64,
    mem_writes: u64,
}

impl SyscallRegionSpace {
    fn new() -> Self {
        Self {
            io_reads: 0,
            io_writes: 0,
            mem_reads: 0,
            mem_writes: 0,
        }
    }

    fn read_one(space: u8, addr: u64, width_bytes: u64) -> Result<u64, AmlError> {
        let rc = match space {
            SPACE_SYSTEM_IO => acpi_io_read(addr, width_bytes),
            SPACE_SYSTEM_MEMORY => acpi_mem_read(addr, width_bytes),
            _ => return Err(AmlError::RegionAccess),
        };
        if rc < 0 {
            return Err(AmlError::RegionAccess);
        }
        Ok(rc as u64)
    }

    fn write_one(space: u8, addr: u64, width_bytes: u64, value: u64) -> Result<(), AmlError> {
        let rc = match space {
            SPACE_SYSTEM_IO => acpi_io_write(addr, width_bytes, value),
            SPACE_SYSTEM_MEMORY => acpi_mem_write(addr, width_bytes, value),
            _ => return Err(AmlError::RegionAccess),
        };
        if rc < 0 {
            return Err(AmlError::RegionAccess);
        }
        Ok(())
    }
}

impl RegionSpace for SyscallRegionSpace {
    fn read(&mut self, space: u8, addr: u64, width_bits: u32) -> Result<u64, AmlError> {
        match space {
            SPACE_SYSTEM_IO => self.io_reads += 1,
            SPACE_SYSTEM_MEMORY => self.mem_reads += 1,
            _ => {}
        }
        match width_bits {
            8 | 16 | 32 => Self::read_one(space, addr, u64::from(width_bits / 8)),
            64 => {
                let lo = Self::read_one(space, addr, 4)?;
                let hi = Self::read_one(space, addr + 4, 4)?;
                Ok(lo | (hi << 32))
            }
            _ => Err(AmlError::RegionAccess),
        }
    }

    fn write(&mut self, space: u8, addr: u64, width_bits: u32, value: u64) -> Result<(), AmlError> {
        match space {
            SPACE_SYSTEM_IO => self.io_writes += 1,
            SPACE_SYSTEM_MEMORY => self.mem_writes += 1,
            _ => {}
        }
        match width_bits {
            8 | 16 | 32 => Self::write_one(space, addr, u64::from(width_bits / 8), value),
            64 => {
                Self::write_one(space, addr, 4, value & 0xFFFF_FFFF)?;
                Self::write_one(space, addr + 4, 4, value >> 32)
            }
            _ => Err(AmlError::RegionAccess),
        }
    }

    fn sleep_ms(&mut self, ms: u64) {
        if ms > 0 {
            let _ = syscall_lib::nanosleep_for(ms / 1000, ((ms % 1000) * 1_000_000) as u32);
        }
    }
}

// ---------------------------------------------------------------------
// Request handlers
// ---------------------------------------------------------------------

fn req_str(msg: &IpcMessage, bulk: &[u8]) -> Option<String> {
    let len = msg.data[0] as usize;
    if len == 0 || len > bulk.len() {
        return None;
    }
    let mut s = String::with_capacity(len);
    for &b in &bulk[..len] {
        if !b.is_ascii() || b == 0 {
            return None;
        }
        s.push(b as char);
    }
    Some(s)
}

fn handle_find_by_hid(
    ns: &mut Namespace,
    regions: &mut SyscallRegionSpace,
    msg: &IpcMessage,
    bulk: &[u8],
    reply_cap: u32,
) {
    let Some(hid) = req_str(msg, bulk) else {
        syscall_lib::ipc_reply(reply_cap, REPLY_ERR, 0);
        return;
    };
    let hits = ns.find_by_hid(regions, &hid);
    match hits.first() {
        Some(&node) => {
            let path = ns.full_path(node);
            syscall_lib::ipc_store_reply_bulk(path.as_bytes());
            syscall_lib::ipc_reply(reply_cap, 0, path.len() as u64);
        }
        None => {
            syscall_lib::ipc_reply(reply_cap, REPLY_ERR, 0);
        }
    }
}

fn handle_get_crs(
    ns: &mut Namespace,
    regions: &mut SyscallRegionSpace,
    msg: &IpcMessage,
    bulk: &[u8],
    reply_cap: u32,
) {
    let node = req_str(msg, bulk).and_then(|p| ns.resolve_str(&p));
    let Some(node) = node else {
        syscall_lib::ipc_reply(reply_cap, REPLY_ERR, 0);
        return;
    };
    match ns.crs_bytes(regions, node) {
        Ok(crs) => {
            syscall_lib::ipc_store_reply_bulk(&crs);
            syscall_lib::ipc_reply(reply_cap, 0, crs.len() as u64);
        }
        Err(_) => {
            syscall_lib::ipc_reply(reply_cap, REPLY_ERR, 0);
        }
    }
}

fn handle_sta(
    ns: &mut Namespace,
    regions: &mut SyscallRegionSpace,
    msg: &IpcMessage,
    bulk: &[u8],
    reply_cap: u32,
) {
    let node = req_str(msg, bulk).and_then(|p| ns.resolve_str(&p));
    match node {
        Some(node) => {
            let sta = ns.sta(regions, node) & 0xFF;
            syscall_lib::ipc_reply(reply_cap, REPLY_STA_BASE | sta, sta);
        }
        None => {
            syscall_lib::ipc_reply(reply_cap, REPLY_ERR, 0);
        }
    }
}

// ---------------------------------------------------------------------
// SCI event dispatch
// ---------------------------------------------------------------------

/// An event subscriber: the transferred endpoint cap + an ASL-path
/// prefix filter (empty = wildcard).
struct Subscriber {
    cap: u32,
    prefix: String,
}

/// Push `(path, code)` to every matching subscriber (Phase 101 D.5).
/// A failed send means the subscriber's endpoint died — drop it.
fn route_notify(subscribers: &mut Vec<Subscriber>, path: &str, code: u64) {
    subscribers.retain(|sub| {
        if !path.as_bytes().starts_with(sub.prefix.as_bytes()) {
            return true;
        }
        let rc = syscall_lib::ipc_send_buf(sub.cap, ACPI_EVENT, code, path.as_bytes());
        if rc == u64::MAX {
            announce("acpid: dropping dead event subscriber\n");
            false
        } else {
            true
        }
    });
}

/// Service pending PM1 fixed events: read status, report, clear (RW1C),
/// re-arm the enables the kernel ISR masked, and push the fixed
/// power-button event to subscribers (D.5 — fixed events have no AML
/// `Notify`, so they ride the pseudo-path).
fn service_pm1(subscribers: &mut Vec<Subscriber>) {
    let sts = pm_read(ACPI_PM_REG_PM1A_STS, 0);
    if sts <= 0 {
        return;
    }
    let sts = sts as u64;
    if sts & PM1_PWRBTN != 0 {
        announce(&format!("ACPI_SMOKE:power-button PM1_STS={sts:#x}\n"));
        route_notify(subscribers, FIXED_PWRBTN_PATH, NOTIFY_STATUS_CHANGE);
    }
    // Write-1-clear exactly the bits we observed.
    let _ = pm_write(ACPI_PM_REG_PM1A_STS, 0, sts);
    // Re-arm the power button (the ISR masked pending enables).
    let en = pm_read(ACPI_PM_REG_PM1A_EN, 0).max(0) as u64;
    let _ = pm_write(ACPI_PM_REG_PM1A_EN, 0, en | PM1_PWRBTN);
}

/// Service pending GPEs: evaluate `\_GPE._Lxx`/`._Exx` for each asserted
/// bit, then clear status. Masked enables stay masked — `acpid` never
/// armed them, so re-arming blind could storm a GPE nobody handles.
fn service_gpe(
    ns: &mut Namespace,
    regions: &mut SyscallRegionSpace,
    gpe0_half: usize,
    subscribers: &mut Vec<Subscriber>,
) {
    for byte in 0..gpe0_half {
        let sts = pm_read(ACPI_PM_REG_GPE0_STS, byte as u64);
        if sts <= 0 {
            continue;
        }
        let sts = sts as u64;
        for bit in 0..8u64 {
            if sts & (1 << bit) == 0 {
                continue;
            }
            let gpe = byte as u64 * 8 + bit;
            let hex = |n: u64| -> char {
                if n < 10 {
                    (b'0' + n as u8) as char
                } else {
                    (b'A' + (n - 10) as u8) as char
                }
            };
            for prefix in ["_L", "_E"] {
                let method = format!("\\_GPE.{prefix}{}{}", hex(gpe >> 4), hex(gpe & 0xF));
                if ns.resolve_str(&method).is_some() {
                    let outcome = match ns.evaluate(regions, &method) {
                        Ok(_) => "ok",
                        Err(_) => "err",
                    };
                    announce(&format!("acpid: GPE {gpe:#x} -> {method} {outcome}\n"));
                    break;
                }
            }
        }
        let _ = pm_write(ACPI_PM_REG_GPE0_STS, byte as u64, sts);
    }
    // Drain any Notify() the GPE methods queued and route each to the
    // subscribed clients (Phase 101 D.5 — the log stays as the always-on
    // trace alongside the IPC push).
    for (node, code) in core::mem::take(&mut ns.pending_notify) {
        let path = ns.full_path(node);
        announce(&format!("acpid: Notify({path}, {code:#x})\n"));
        route_notify(subscribers, &path, code);
    }
}

/// `ACPI_SUBSCRIBE` handler (Phase 101 E.4): the bulk carries the
/// subscriber's REGISTERED event-service name (`data[0]` = length);
/// acpid resolves it via `ipc_lookup_service` to obtain its own send
/// handle. (A raw cap transfer cannot express this subscription:
/// `grant_task_cap` is move-semantics — the subscriber would lose its
/// only receive handle and orphan the endpoint. The registry hands out
/// independent send handles while the owner keeps receiving, which is
/// the established m3OS push idiom.)
fn handle_subscribe(
    subscribers: &mut Vec<Subscriber>,
    msg: &IpcMessage,
    bulk: &[u8],
    reply_cap: u32,
) {
    if subscribers.len() >= MAX_SUBSCRIBERS {
        syscall_lib::ipc_reply(reply_cap, REPLY_ERR, 0);
        return;
    }
    let Some(name) = req_str(msg, bulk) else {
        syscall_lib::ipc_reply(reply_cap, REPLY_ERR, 0);
        return;
    };
    let handle = syscall_lib::ipc_lookup_service(&name);
    let Ok(cap) = u32::try_from(handle) else {
        syscall_lib::ipc_reply(reply_cap, REPLY_ERR, 0);
        return;
    };
    subscribers.push(Subscriber {
        cap,
        prefix: String::new(),
    });
    announce(&format!(
        "acpid: subscriber added (service={name}, total={})\n",
        subscribers.len()
    ));
    syscall_lib::ipc_reply(reply_cap, 0, 0);
}

/// The ACPI-mode handshake (§16.3.1): if `SCI_EN` is clear, write the
/// FADT's `ACPI_ENABLE` value to `SMI_CMD` and poll until firmware hands
/// the SCI over. QEMU's PM device flips it on the first write; on real
/// firmware this can take milliseconds.
fn enable_acpi_mode(facp: &[u8]) {
    let cnt = pm_read(ACPI_PM_REG_PM1A_CNT, 0);
    if cnt >= 0 && (cnt as u64) & PM1_CNT_SCI_EN != 0 {
        announce("acpid: SCI_EN already set\n");
        return;
    }
    let acpi_enable = facp.get(52).copied().unwrap_or(0);
    if acpi_enable == 0 {
        announce("acpid: no ACPI_ENABLE handshake (assuming SCI owned)\n");
        return;
    }
    let _ = pm_write(ACPI_PM_REG_SMI_CMD, 0, acpi_enable as u64);
    for _ in 0..100 {
        let cnt = pm_read(ACPI_PM_REG_PM1A_CNT, 0);
        if cnt >= 0 && (cnt as u64) & PM1_CNT_SCI_EN != 0 {
            announce("acpid: SCI_EN set (ACPI mode entered)\n");
            return;
        }
        let _ = syscall_lib::nanosleep_for(0, 10_000_000);
    }
    announce("acpid: WARNING SCI_EN never set after ACPI_ENABLE\n");
}

fn program_main(_args: &[&str]) -> i32 {
    announce("acpid: starting\n");

    // ---- Fetch tables -------------------------------------------------
    let Some(facp) = fetch_table(b"FACP", 0) else {
        announce("acpid: no FACP — ACPI unavailable, exiting\n");
        return 0;
    };
    let gpe0_half = facp.get(92).copied().unwrap_or(0) as usize / 2;
    let Some(dsdt) = fetch_table(b"DSDT", 0) else {
        announce("acpid: no DSDT — ACPI unavailable, exiting\n");
        return 0;
    };

    // ---- Build the namespace ------------------------------------------
    let mut regions = SyscallRegionSpace::new();
    let mut ns = Namespace::new();
    let mut tables = 1usize;
    let mut skipped = 0usize;
    match ns.load_table(&dsdt, &mut regions) {
        Ok(summary) => skipped += summary.skipped.len(),
        Err(_) => {
            announce("acpid: DSDT load FAILED\n");
            return 1;
        }
    }
    for i in 0..16 {
        let Some(ssdt) = fetch_table(b"SSDT", i) else {
            break;
        };
        if let Ok(summary) = ns.load_table(&ssdt, &mut regions) {
            tables += 1;
            skipped += summary.skipped.len();
        }
    }
    let devices = ns.devices().len();
    announce(&format!(
        "ACPI_SMOKE:namespace-built nodes={} devices={devices} tables={tables} skipped={skipped}\n",
        ns.len()
    ));

    // ---- E.3 self-probes -------------------------------------------------
    // Prove the real RegionSpace syscall path end-to-end at every boot,
    // independent of which methods the DSDT happens to evaluate:
    //  1. SystemIO — read the FADT's PM1a event/status port (a harmless
    //     status read; FADT byte offset 56 = PM1a_EVT_BLK, u32 LE).
    //  2. SystemMemory — read the DSDT's first 4 bytes at its physical
    //     address (FADT offset 140 = X_DSDT u64, falling back to offset
    //     40 = DSDT u32) and require the literal "DSDT" signature.
    let pm1a_port = facp
        .get(56..60)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as u64)
        .unwrap_or(0);
    if pm1a_port != 0 {
        match regions.read(SPACE_SYSTEM_IO, pm1a_port, 16) {
            Ok(v) => announce(&format!(
                "ACPI_SMOKE:regionspace-io ok port={pm1a_port:#x} val={v:#x}\n"
            )),
            Err(_) => announce("ACPI_SMOKE:regionspace-io FAILED\n"),
        }
    }
    let dsdt_phys = facp
        .get(140..148)
        .map(|b| u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
        .filter(|&x| x != 0)
        .or_else(|| {
            facp.get(40..44)
                .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as u64)
                .filter(|&x| x != 0)
        });
    if let Some(phys) = dsdt_phys {
        match regions.read(SPACE_SYSTEM_MEMORY, phys, 32) {
            Ok(v) if v == u64::from(u32::from_le_bytes(*b"DSDT")) => {
                announce("ACPI_SMOKE:regionspace-mem ok sig=DSDT\n");
            }
            Ok(v) => announce(&format!("ACPI_SMOKE:regionspace-mem MISMATCH val={v:#x}\n")),
            Err(_) => announce("ACPI_SMOKE:regionspace-mem FAILED\n"),
        }
    }

    // ---- Register the query service ------------------------------------
    let ep = syscall_lib::create_endpoint();
    if ep == u64::MAX {
        announce("acpid: create_endpoint failed\n");
        return 1;
    }
    let ep = ep as u32;
    if syscall_lib::ipc_register_service(ep, ACPI_SERVICE_NAME) != 0 {
        announce("acpid: service registration failed\n");
        return 1;
    }

    // ---- SCI + ACPI mode ----------------------------------------------
    let notif = sci_subscribe();
    if notif < 0 {
        // No SCI on this platform (or FADT absent): still serve queries.
        announce(&format!("acpid: SCI subscribe unavailable ({notif})\n"));
    } else {
        let notif = notif as u32;
        if syscall_lib::sys_notif_bind(notif, ep) != 0 {
            announce("acpid: notif bind failed\n");
            return 1;
        }
        enable_acpi_mode(&facp);
        // Clear stale PM1 status, then arm the power button.
        let stale = pm_read(ACPI_PM_REG_PM1A_STS, 0).max(0) as u64;
        if stale != 0 {
            let _ = pm_write(ACPI_PM_REG_PM1A_STS, 0, stale);
        }
        let en = pm_read(ACPI_PM_REG_PM1A_EN, 0).max(0) as u64;
        let _ = pm_write(ACPI_PM_REG_PM1A_EN, 0, en | PM1_PWRBTN);
        announce("ACPI_SMOKE:sci-armed\n");
    }

    // ---- Serve ----------------------------------------------------------
    // `ipc_recv_with_caps` (not plain `recv_msg`): the E.4 Subscribe verb
    // transfers the subscriber's endpoint cap in `cap_slots[0]`.
    let mut subscribers: Vec<Subscriber> = Vec::new();
    let mut msg = IpcMessage::new(0);
    let mut bulk = [0u8; MSG_BUF];
    loop {
        bulk.fill(0);
        let rc = syscall_lib::ipc_recv_with_caps(ep, &mut msg, &mut bulk);
        if rc == u64::MAX {
            continue;
        }
        if rc == u64::from(RECV_KIND_NOTIFICATION) && msg.label == 0 {
            let bits = msg.data[0];
            if bits & (1 << ACPI_SCI_BIT_PM1) != 0 {
                service_pm1(&mut subscribers);
            }
            if bits & (1 << ACPI_SCI_BIT_GPE) != 0 {
                service_gpe(&mut ns, &mut regions, gpe0_half, &mut subscribers);
            }
            continue;
        }
        let Some(reply_cap) = msg.reply_cap_handle() else {
            continue;
        };
        match rc {
            ACPI_FIND_BY_HID => handle_find_by_hid(&mut ns, &mut regions, &msg, &bulk, reply_cap),
            ACPI_GET_CRS => handle_get_crs(&mut ns, &mut regions, &msg, &bulk, reply_cap),
            ACPI_STA => handle_sta(&mut ns, &mut regions, &msg, &bulk, reply_cap),
            ACPI_SUBSCRIBE => handle_subscribe(&mut subscribers, &msg, &bulk, reply_cap),
            _ => {
                syscall_lib::ipc_reply(reply_cap, REPLY_ERR, 0);
            }
        }
    }
}

syscall_lib::entry_point!(program_main);
