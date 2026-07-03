//! `powerd` — Phase 103's ring-3 power policy daemon.
//!
//! The consumer that turns Phase 101's ACPI substrate into laptop power
//! state. Per the 101 split decision the AML interpreter lives in
//! `acpid`, so powerd is a pure IPC client of the `"acpi"` service:
//! `ACPI_FIND_BY_HID` locates the `PNP0C0A` battery and `ACPI0003` AC
//! adapter, `ACPI_EVAL` evaluates `_BST`/`_BIF`/`_PSR` on demand, and
//! `ACPI_SUBSCRIBE` registers this daemon for `Notify`/fixed-event
//! pushes (powerd is the first production consumer of the D.5/E.4 event
//! push). The decode + percentage math is `kernel_core::power::battery`
//! (host-tested); the client protocol is `kernel_core::power::control`.
//!
//! One endpoint, two names: registered as both `"power"` (the query
//! service `m3ctl` / the settings panel call) and `"powerd.events"`
//! (the name acpid resolves to push events), so a single blocking recv
//! loop multiplexes queries and events by label.
//!
//! On a platform with no battery/AC devices (every VM and desktop) the
//! daemon still serves [`PowerStatusWire::no_battery`] — the
//! `power-smoke` QEMU arm asserts exactly this posture. Serial
//! sentinels:
//!
//! - `POWERD:ready battery=<none|path> ac=<assumed-online|path>`
//! - `POWERD:event path=<asl-path> code=<c>` — an acpid event arrived.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::alloc::Layout;

use kernel_core::acpi::aml::object::AmlValue;
use kernel_core::acpi::aml::wire;
use kernel_core::power::battery::{self, BatteryInfo};
use kernel_core::power::control::{
    AcState, PERCENT_UNKNOWN, POWER_SERVICE_NAME, POWER_STATUS, PowerStatusWire,
};
use syscall_lib::heap::BrkAllocator;
use syscall_lib::{IpcMessage, STDOUT_FILENO, write_str};

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    write_str(STDOUT_FILENO, "powerd: alloc error\n");
    syscall_lib::exit(99)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "powerd: PANIC\n");
    syscall_lib::exit(101)
}

/// Serial + stdout (the smoke oracle + dmesg on hardware).
fn announce(msg: &str) {
    write_str(STDOUT_FILENO, msg);
    syscall_lib::serial_print(msg);
}

// ---------------------------------------------------------------------
// acpid client (protocol constants mirror userspace/drivers/acpid)
// ---------------------------------------------------------------------

const ACPI_SERVICE_NAME: &str = "acpi";
const ACPI_FIND_BY_HID: u64 = 2;
const ACPI_SUBSCRIBE: u64 = 5;
const ACPI_EVENT: u64 = 6;
const ACPI_EVAL: u64 = 7;

/// The event-endpoint name acpid resolves for its push handle.
const EVENTS_SERVICE_NAME: &str = "powerd.events";

const REPLY_BUF: usize = 4096;

/// One text-request round-trip to acpid; `Some(reply bulk)` on label-0.
fn acpi_call(handle: u32, label: u64, text: &str) -> Option<Vec<u8>> {
    let reply = syscall_lib::ipc_call_buf(handle, label, text.len() as u64, text.as_bytes());
    if reply != 0 {
        return None;
    }
    let mut buf = alloc::vec![0u8; REPLY_BUF];
    let n = syscall_lib::ipc_take_pending_bulk(&mut buf);
    if n == u64::MAX {
        return None;
    }
    buf.truncate((n as usize).min(REPLY_BUF));
    Some(buf)
}

fn find_by_hid(handle: u32, hid: &str) -> Option<String> {
    let bulk = acpi_call(handle, ACPI_FIND_BY_HID, hid)?;
    core::str::from_utf8(&bulk).ok().map(String::from)
}

fn eval(handle: u32, path: &str) -> Option<AmlValue> {
    let bulk = acpi_call(handle, ACPI_EVAL, path)?;
    wire::decode(&bulk).ok().map(|(v, _)| v)
}

// ---------------------------------------------------------------------
// Power state
// ---------------------------------------------------------------------

struct PowerDevices {
    acpi: Option<u32>,
    battery_path: Option<String>,
    ac_path: Option<String>,
    /// `_BIF`/`_BIX` info cached once (static data).
    battery_info: Option<BatteryInfo>,
}

impl PowerDevices {
    /// Evaluate the live snapshot. Every query re-reads `_BST`/`_PSR` —
    /// no staleness, and the VM case costs nothing (no devices).
    fn status(&self) -> PowerStatusWire {
        let Some(acpi) = self.acpi else {
            return PowerStatusWire::no_battery();
        };
        let ac = match self.ac_path.as_deref() {
            Some(path) => {
                match eval(acpi, &format!("{path}._PSR")).and_then(|v| battery::decode_psr(&v)) {
                    Some(true) => AcState::Online,
                    Some(false) => AcState::Offline,
                    None => AcState::AssumedOnline,
                }
            }
            None => AcState::AssumedOnline,
        };
        let Some(bat_path) = self.battery_path.as_deref() else {
            return PowerStatusWire {
                ac,
                ..PowerStatusWire::no_battery()
            };
        };
        let bst = eval(acpi, &format!("{bat_path}._BST")).and_then(|v| battery::decode_bst(&v));
        match (bst, self.battery_info.as_ref()) {
            (Some(status), Some(info)) => PowerStatusWire {
                battery_present: true,
                percent: battery::percent(&status, info).unwrap_or(PERCENT_UNKNOWN),
                ac,
                state: status.state,
                rate: status.present_rate,
            },
            _ => PowerStatusWire {
                battery_present: true,
                percent: PERCENT_UNKNOWN,
                ac,
                state: 0,
                rate: 0,
            },
        }
    }
}

fn program_main(_args: &[&str]) -> i32 {
    announce("powerd: starting\n");

    // ---- Our endpoint: one queue, two registered names ------------------
    let ep = syscall_lib::create_endpoint();
    if ep == u64::MAX {
        announce("powerd: create_endpoint failed\n");
        return 1;
    }
    let ep = ep as u32;
    if syscall_lib::ipc_register_service(ep, POWER_SERVICE_NAME) != 0 {
        announce("powerd: service registration failed\n");
        return 1;
    }
    if syscall_lib::ipc_register_service(ep, EVENTS_SERVICE_NAME) != 0 {
        announce("powerd: events registration failed\n");
        return 1;
    }

    // ---- Discover the power devices through acpid -----------------------
    // acpid starts alongside us (`depends=acpid` orders it first, but the
    // service registration can still race) — retry ~10 s before falling
    // back to the no-ACPI posture.
    let mut acpi = u64::MAX;
    for _ in 0..100 {
        acpi = syscall_lib::ipc_lookup_service(ACPI_SERVICE_NAME);
        if acpi != u64::MAX {
            break;
        }
        let _ = syscall_lib::nanosleep_for(0, 100_000_000);
    }
    let acpi = u32::try_from(acpi).ok();

    let mut devices = PowerDevices {
        acpi,
        battery_path: None,
        ac_path: None,
        battery_info: None,
    };
    if let Some(handle) = acpi {
        // PNP0C0A = Control Method Battery, ACPI0003 = AC adapter.
        devices.battery_path = find_by_hid(handle, "PNP0C0A");
        devices.ac_path = find_by_hid(handle, "ACPI0003");
        if let Some(bat) = devices.battery_path.as_deref() {
            // Static info: prefer the extended `_BIX`, fall back to `_BIF`.
            devices.battery_info = eval(handle, &format!("{bat}._BIX"))
                .and_then(|v| battery::decode_bix(&v))
                .or_else(|| {
                    eval(handle, &format!("{bat}._BIF")).and_then(|v| battery::decode_bif(&v))
                });
        }
        // Subscribe for Notify/fixed-event pushes (battery/AC/lid/button).
        let sub = syscall_lib::ipc_call_buf(
            handle,
            ACPI_SUBSCRIBE,
            EVENTS_SERVICE_NAME.len() as u64,
            EVENTS_SERVICE_NAME.as_bytes(),
        );
        if sub != 0 {
            announce("powerd: WARNING acpid event subscribe failed\n");
        }
    } else {
        announce("powerd: no acpi service — serving no-battery state\n");
    }

    announce(&format!(
        "POWERD:ready battery={} ac={}\n",
        devices.battery_path.as_deref().unwrap_or("none"),
        devices.ac_path.as_deref().unwrap_or("assumed-online"),
    ));

    // ---- Serve ----------------------------------------------------------
    let mut msg = IpcMessage::new(0);
    let mut bulk = [0u8; REPLY_BUF];
    loop {
        bulk.fill(0);
        let rc = syscall_lib::ipc_recv_msg(ep, &mut msg, &mut bulk);
        if rc == u64::MAX {
            continue;
        }
        if rc == ACPI_EVENT {
            // Pushed event from acpid (no reply cap — fire-and-forget).
            let code = msg.data[0];
            let len = bulk.iter().position(|&b| b == 0).unwrap_or(bulk.len());
            let path = core::str::from_utf8(&bulk[..len]).unwrap_or("<non-utf8>");
            announce(&format!("POWERD:event path={path} code={code:#x}\n"));
            // Policy routing (lid → suspend, button → session) lands with
            // Track D.3; the log is the slice-1 consumer.
            continue;
        }
        let Some(reply_cap) = msg.reply_cap_handle() else {
            continue;
        };
        if rc == u64::from(POWER_STATUS) {
            let status = devices.status();
            let encoded = status.encode();
            syscall_lib::ipc_store_reply_bulk(&encoded);
            syscall_lib::ipc_reply(reply_cap, 0, encoded.len() as u64);
        } else {
            syscall_lib::ipc_reply(reply_cap, u64::MAX, 0);
        }
    }
}

syscall_lib::entry_point!(program_main);
