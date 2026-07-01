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
//!
//! ## Track E residual (documented, deliberate)
//!
//! `OperationRegion` accesses during method evaluation go through
//! [`StubRegionSpace`]: reads yield zero, writes are dropped, and a
//! counter records the traffic. This matches the host-test mock exactly,
//! so namespace behavior in the VM is bit-identical to the CI-green host
//! tests. The PM1/GPE *event* path does NOT ride AML regions — it uses
//! the dedicated role-named PM syscalls. A real SystemIO/SystemMemory
//! backend lands with the Phase 103 EC work, which is its first honest
//! consumer.

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
    SYS_ACPI_PM_READ, SYS_ACPI_PM_WRITE, SYS_ACPI_SCI_SUBSCRIBE, SYS_ACPI_TABLE_GET,
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
/// Reply label for any failure (unknown label, bad request, no match).
const REPLY_ERR: u64 = u64::MAX;
/// `ACPI_STA` success replies are `REPLY_STA_BASE | sta`.
const REPLY_STA_BASE: u64 = 0x100;

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
// Region backend (Track E residual — see module doc)
// ---------------------------------------------------------------------

struct StubRegionSpace {
    reads: u64,
    writes: u64,
}

impl RegionSpace for StubRegionSpace {
    fn read(&mut self, _space: u8, _addr: u64, _width_bits: u32) -> Result<u64, AmlError> {
        self.reads += 1;
        Ok(0)
    }
    fn write(
        &mut self,
        _space: u8,
        _addr: u64,
        _width_bits: u32,
        _value: u64,
    ) -> Result<(), AmlError> {
        self.writes += 1;
        Ok(())
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
    regions: &mut StubRegionSpace,
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
    regions: &mut StubRegionSpace,
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
    regions: &mut StubRegionSpace,
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

/// Service pending PM1 fixed events: read status, report, clear (RW1C),
/// re-arm the enables the kernel ISR masked.
fn service_pm1() {
    let sts = pm_read(ACPI_PM_REG_PM1A_STS, 0);
    if sts <= 0 {
        return;
    }
    let sts = sts as u64;
    if sts & PM1_PWRBTN != 0 {
        announce(&format!("ACPI_SMOKE:power-button PM1_STS={sts:#x}\n"));
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
fn service_gpe(ns: &mut Namespace, regions: &mut StubRegionSpace, gpe0_half: usize) {
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
    // Drain any Notify() the GPE methods queued (Track D routing seam —
    // consumers subscribe in Phase 103; today the log is the subscriber).
    for (node, code) in core::mem::take(&mut ns.pending_notify) {
        let path = ns.full_path(node);
        announce(&format!("acpid: Notify({path}, {code:#x})\n"));
    }
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
    let mut regions = StubRegionSpace {
        reads: 0,
        writes: 0,
    };
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
    let mut msg = IpcMessage::new(0);
    let mut bulk = [0u8; MSG_BUF];
    loop {
        bulk.fill(0);
        let rc = syscall_lib::ipc_recv_msg(ep, &mut msg, &mut bulk);
        if rc == u64::MAX {
            continue;
        }
        if rc == u64::from(RECV_KIND_NOTIFICATION) && msg.label == 0 {
            let bits = msg.data[0];
            if bits & (1 << ACPI_SCI_BIT_PM1) != 0 {
                service_pm1();
            }
            if bits & (1 << ACPI_SCI_BIT_GPE) != 0 {
                service_gpe(&mut ns, &mut regions, gpe0_half);
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
            _ => {
                syscall_lib::ipc_reply(reply_cap, REPLY_ERR, 0);
            }
        }
    }
}

syscall_lib::entry_point!(program_main);
