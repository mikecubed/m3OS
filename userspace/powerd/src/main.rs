//! `powerd` — Phase 103's ring-3 power policy daemon.
//!
//! The consumer that turns Phase 101's ACPI substrate into laptop power
//! state. Per the 101 split decision the AML interpreter lives in
//! `acpid`, so powerd is a pure IPC client of the `"acpi"` service:
//! `ACPI_FIND_BY_HID` locates the `PNP0C0A` battery and `ACPI0003` AC
//! adapter, `ACPI_EVAL` evaluates `_BST`/`_BIF`/`_PSR`/`_TMP` on demand,
//! `ACPI_LIST_TZ` enumerates thermal zones, and `ACPI_SUBSCRIBE`
//! registers this daemon for `Notify`/fixed-event pushes (powerd is the
//! first production consumer of the D.5/E.4 event push). The decode +
//! percentage math is `kernel_core::power::battery`, thermal decode +
//! trip classification `kernel_core::power::thermal`, and the governor
//! state machine `kernel_core::power::governor` (all host-tested); the
//! client protocol is `kernel_core::power::control`.
//!
//! **Track E division of labor** (charter correction, mirrors slice 1):
//! the governor *policy* ticks here in ring 3 (userspace-first rule) —
//! a ~1 s `ipc_recv_msg_timeout` idle wake samples the kernel's
//! cumulative CPU times via `SYS_POWER_CPUFREQ_STATUS`, folds the load
//! delta and the Track C thermal cap through `Governor::next`, and
//! applies the target via `SYS_POWER_SET_PERF`. Ring 0 keeps only the
//! HWP MSR mechanism behind those two syscalls; on QEMU (no HWP) the
//! apply is a successful no-op so this loop is platform-independent.
//!
//! One endpoint, two names: registered as both `"power"` (the query
//! service `m3ctl` / the settings panel call) and `"powerd.events"`
//! (the name acpid resolves to push events), so a single recv loop
//! multiplexes queries, events, and governor ticks by label.
//!
//! On a platform with no battery/AC devices (every VM and desktop) the
//! daemon still serves [`PowerStatusWire::no_battery`] — the
//! `power-smoke` QEMU arm asserts exactly this posture. Serial
//! sentinels:
//!
//! - `POWERD:ready battery=<none|path> ac=<assumed-online|path>
//!   zones=<n> mech=<none|hwp> backlight=<none|path> sleep=<none|s3+s4…>`
//! - `POWERD:suspend rejected reason=resume-path-unimplemented
//!   firmware=<bits>` — a `POWER_SUSPEND` request failed closed (Track
//!   F: no S3 resume path yet; refusing beats never waking up).
//! - `POWERD:lid action=lock (suspend fails closed)` — lid-close routed
//!   to the lockscreen fallback (no QEMU lid model; HW-validated).
//! - `POWERD:backlight set=<pct>%` — a `POWER_SET_BRIGHTNESS` request
//!   applied a level through `_BCM` (Track B; never fires on q35).
//! - `POWERD:event path=<asl-path> code=<c>` — an acpid event arrived.
//! - `POWERD:governor mode=<m> target=<t> load=<l>%` — first tick, then
//!   only when the target changes (a QEMU boot settles to the floor in
//!   a handful of lines).
//! - `POWERD:thermal state=<passive|critical> temp_dc=<t>` — a zone
//!   crossed a trip point (never fires on zone-less QEMU).
//! - `POWERD:poweroff reason=<power-button|thermal-critical>` — Track
//!   D.3 routing fired: a forked child execs `/bin/shutdown` (SIGTERM to
//!   init → service teardown → `sys_reboot(POWER_OFF)` → kernel sync +
//!   the ACPI S5 write acpid registered at boot). The child survives
//!   init's teardown because it is not a supervised service.

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
use kernel_core::power::backlight::{self, BACKLIGHT_UNKNOWN, BclLevels};
use kernel_core::power::battery::{self, BatteryInfo};
use kernel_core::power::control::{
    AcState, CpufreqMech, PERCENT_UNKNOWN, POWER_SERVICE_NAME, POWER_SET_BRIGHTNESS, POWER_STATUS,
    POWER_SUSPEND, PowerStatusWire, SLEEP_S0IX, SLEEP_S3, SLEEP_S4, TEMP_UNKNOWN_DECI_C,
    ThermalWire,
};
use kernel_core::power::governor::{Governor, GovernorMode, PERF_MAX, PERF_MIN};
use kernel_core::power::syscalls::{
    CPUFREQ_MECH_HWP, CPUFREQ_STATUS_WIRE_LEN, CpufreqStatusWire, SYS_POWER_CPUFREQ_STATUS,
    SYS_POWER_ENTER_SLEEP, SYS_POWER_SET_PERF,
};
use kernel_core::power::thermal::{
    ThermalState, TripPoints, classify, deci_celsius_from_decikelvin, decode_temp_dk,
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
const ACPI_LIST_TZ: u64 = 8;
const ACPI_EVAL_ARG: u64 = 9;
const ACPI_LIST_BACKLIGHT: u64 = 10;
const ACPI_SLEEP_STATES: u64 = 11;
/// `ACPI_SLEEP_STATES` replies carry the bits in the label:
/// `REPLY_SLEEP_BASE | bits` (the acpid `_STA` shape).
const REPLY_SLEEP_BASE: u64 = 0x200;

/// The event-endpoint name acpid resolves for its push handle.
const EVENTS_SERVICE_NAME: &str = "powerd.events";

const REPLY_BUF: usize = 4096;

/// Governor tick period (the recv-timeout idle wake).
const GOVERNOR_TICK_NS: u64 = 1_000_000_000;
/// `ipc_recv_msg_timeout` deadline-expiry return (`-ETIMEDOUT`).
const NEG_ETIMEDOUT: u64 = (-110_i64) as u64;

/// One text-request round-trip to acpid; `Some(reply bulk)` on label-0.
fn acpi_call(handle: u32, label: u64, text: &str) -> Option<Vec<u8>> {
    let reply = syscall_lib::ipc_call_buf(handle, label, text.len() as u64, text.as_bytes());
    if reply != 0 {
        return None;
    }
    take_reply_bulk()
}

fn take_reply_bulk() -> Option<Vec<u8>> {
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

/// A body-less list verb (`ACPI_LIST_TZ`/`ACPI_LIST_BACKLIGHT`; plain
/// `ipc_call` — the slice-1 `send_with_bulk:bad_len` lesson):
/// newline-joined full paths.
fn acpi_list(handle: u32, label: u64) -> Vec<String> {
    if syscall_lib::ipc_call(handle, label, 0) != 0 {
        return Vec::new();
    }
    let Some(bulk) = take_reply_bulk() else {
        return Vec::new();
    };
    let Ok(text) = core::str::from_utf8(&bulk) else {
        return Vec::new();
    };
    text.split('\n')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

/// `ACPI_SLEEP_STATES` (F.1): the firmware's declared sleep support as
/// `SLEEP_*` bits, carried in the reply label.
fn sleep_states(handle: u32) -> u8 {
    let reply = syscall_lib::ipc_call(handle, ACPI_SLEEP_STATES, 0);
    if reply & !0xFF != REPLY_SLEEP_BASE {
        return 0;
    }
    (reply & 0xFF) as u8
}

/// Render sleep bits for the ready line / logs (`none` or `s3+s4`…).
fn sleep_str(bits: u8) -> String {
    if bits == 0 {
        return String::from("none");
    }
    let mut s = String::new();
    for (bit, name) in [(SLEEP_S3, "s3"), (SLEEP_S4, "s4"), (SLEEP_S0IX, "s0ix")] {
        if bits & bit != 0 {
            if !s.is_empty() {
                s.push('+');
            }
            s.push_str(name);
        }
    }
    s
}

/// `ACPI_EVAL_ARG`: evaluate a one-argument method (`_BCM(level)`).
/// Bulk = path ++ 8-byte LE arg; `data0` = path length.
fn eval_arg(handle: u32, path: &str, arg: u64) -> Option<AmlValue> {
    let mut req = Vec::with_capacity(path.len() + 8);
    req.extend_from_slice(path.as_bytes());
    req.extend_from_slice(&arg.to_le_bytes());
    let reply = syscall_lib::ipc_call_buf(handle, ACPI_EVAL_ARG, path.len() as u64, &req);
    if reply != 0 {
        return None;
    }
    let bulk = take_reply_bulk()?;
    wire::decode(&bulk).ok().map(|(v, _)| v)
}

// ---------------------------------------------------------------------
// Track E — kernel cpufreq syscalls
// ---------------------------------------------------------------------

/// `SYS_POWER_CPUFREQ_STATUS` — probed mechanism + cumulative CPU times.
fn cpufreq_status() -> Option<CpufreqStatusWire> {
    let mut buf = [0u8; CPUFREQ_STATUS_WIRE_LEN];
    // SAFETY: m3OS-native read-only syscall; the kernel writes at most
    // `buf.len()` bytes and returns the count (or a negative errno).
    let n = unsafe {
        syscall_lib::syscall2(
            SYS_POWER_CPUFREQ_STATUS,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        )
    };
    if (n as i64) < 0 {
        return None;
    }
    CpufreqStatusWire::decode(&buf[..(n as usize).min(buf.len())])
}

/// `SYS_POWER_SET_PERF` — apply a governor target (root-gated; a
/// successful no-op on mechanism-less platforms).
fn set_perf(target: u8) {
    // SAFETY: single-integer m3OS-native syscall, no memory arguments.
    let rc = unsafe { syscall_lib::syscall1(SYS_POWER_SET_PERF, target as u64) };
    if (rc as i64) < 0 {
        announce("powerd: WARNING SYS_POWER_SET_PERF failed\n");
    }
}

// ---------------------------------------------------------------------
// Power state
// ---------------------------------------------------------------------

/// A thermal zone with its boot-evaluated (static) trip points.
struct ThermalZone {
    path: String,
    trips: TripPoints,
}

/// A backlight output with its boot-decoded (static) `_BCL` level list.
struct Backlight {
    path: String,
    levels: BclLevels,
}

fn severity(s: ThermalState) -> u8 {
    match s {
        ThermalState::Normal => 0,
        ThermalState::Passive => 1,
        ThermalState::Critical => 2,
    }
}

struct PowerDevices {
    acpi: Option<u32>,
    battery_path: Option<String>,
    ac_path: Option<String>,
    /// `_BIF`/`_BIX` info cached once (static data).
    battery_info: Option<BatteryInfo>,
    /// Thermal zones with trips cached once (`_CRT`/`_PSV` are static;
    /// only `_TMP` is live).
    zones: Vec<ThermalZone>,
    /// The first backlight-capable display output (`_BCL` carrier),
    /// with its level list cached once; `None` on QEMU/desktop.
    backlight: Option<Backlight>,
    /// Firmware-declared sleep support (F.1 discovery), `SLEEP_*` bits.
    sleep_bits: u8,
}

impl PowerDevices {
    /// Live thermal sample across every zone: hottest temperature and
    /// the worst classified state. `(TEMP_UNKNOWN, NoZones)` on a
    /// zone-less platform (QEMU q35) or when every `_TMP` read fails.
    fn thermal_sample(&self) -> (i16, ThermalWire) {
        let Some(acpi) = self.acpi else {
            return (TEMP_UNKNOWN_DECI_C, ThermalWire::NoZones);
        };
        let mut hottest: Option<i64> = None;
        let mut worst: Option<ThermalState> = None;
        for zone in &self.zones {
            let Some(tmp_dk) =
                eval(acpi, &format!("{}._TMP", zone.path)).and_then(|v| decode_temp_dk(&v))
            else {
                continue;
            };
            let dc = deci_celsius_from_decikelvin(tmp_dk);
            hottest = Some(hottest.map_or(dc, |h: i64| h.max(dc)));
            let state = classify(tmp_dk, &zone.trips);
            worst = Some(match worst {
                Some(w) if severity(w) >= severity(state) => w,
                _ => state,
            });
        }
        match (hottest, worst) {
            (Some(dc), Some(state)) => (
                dc.clamp(i16::MIN as i64 + 1, i16::MAX as i64) as i16,
                ThermalWire::from_state(state),
            ),
            _ => (TEMP_UNKNOWN_DECI_C, ThermalWire::NoZones),
        }
    }

    /// Current backlight level as a percent (live `_BQC` read mapped
    /// through the cached `_BCL` list), or `BACKLIGHT_UNKNOWN`.
    fn backlight_percent(&self) -> u8 {
        let (Some(acpi), Some(bl)) = (self.acpi, self.backlight.as_ref()) else {
            return BACKLIGHT_UNKNOWN;
        };
        match eval(acpi, &format!("{}._BQC", bl.path)).and_then(|v| backlight::decode_bqc(&v)) {
            Some(level) => bl.levels.level_to_percent(level),
            None => BACKLIGHT_UNKNOWN,
        }
    }

    /// Apply a brightness percent: snap to the nearest `_BCL` level and
    /// evaluate `_BCM(level)`. Returns the applied percent, or `None`
    /// when there is no backlight device or the evaluation failed.
    fn set_brightness(&self, pct: u8) -> Option<u8> {
        let (acpi, bl) = (self.acpi?, self.backlight.as_ref()?);
        let level = bl.levels.nearest_level(pct.min(100));
        eval_arg(acpi, &format!("{}._BCM", bl.path), level as u64)?;
        Some(bl.levels.level_to_percent(level))
    }

    /// Evaluate the live snapshot. Every query re-reads `_BST`/`_PSR`/
    /// `_TMP`/`_BQC` — no staleness, and the VM case costs nothing (no
    /// devices). Governor fields (`governor`/`mech`/`perf`) are the
    /// caller's — the main loop owns that state and overlays it.
    fn status(&self) -> PowerStatusWire {
        let (temp_deci_c, thermal) = self.thermal_sample();
        let base = PowerStatusWire {
            temp_deci_c,
            thermal,
            backlight_pct: self.backlight_percent(),
            sleep_bits: self.sleep_bits,
            ..PowerStatusWire::no_battery()
        };
        let Some(acpi) = self.acpi else {
            return base;
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
            return PowerStatusWire { ac, ..base };
        };
        let bst = eval(acpi, &format!("{bat_path}._BST")).and_then(|v| battery::decode_bst(&v));
        match (bst, self.battery_info.as_ref()) {
            (Some(status), Some(info)) => PowerStatusWire {
                battery_present: true,
                percent: battery::percent(&status, info).unwrap_or(PERCENT_UNKNOWN),
                ac,
                state: status.state,
                rate: status.present_rate,
                ..base
            },
            _ => PowerStatusWire {
                battery_present: true,
                percent: PERCENT_UNKNOWN,
                ac,
                ..base
            },
        }
    }
}

/// Map the worst thermal state onto the governor's cap: passive halves
/// the scale, critical pins the floor (ACPI passive cooling §11.4).
fn thermal_cap(thermal: ThermalWire) -> Option<u8> {
    match thermal {
        ThermalWire::Passive => Some(PERF_MAX / 2),
        ThermalWire::Critical => Some(PERF_MIN),
        ThermalWire::NoZones | ThermalWire::Normal => None,
    }
}

fn monotonic_now_ns() -> u64 {
    let (sec, nsec) = syscall_lib::clock_gettime(syscall_lib::CLOCK_MONOTONIC);
    (sec.max(0) as u64)
        .saturating_mul(1_000_000_000)
        .saturating_add(nsec.max(0) as u64)
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
        zones: Vec::new(),
        backlight: None,
        sleep_bits: 0,
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
        // Sleep discovery (Track F.1): which states the firmware declares
        // is static; whether suspend *runs* is the F.3 resume path's call.
        devices.sleep_bits = sleep_states(handle);
        // Backlight (Track B): the `_BCL` level list is static — decode
        // once; only `_BQC` is live. First `_BCL` carrier wins (laptops
        // expose one panel; multi-output brightness is a residual).
        devices.backlight = acpi_list(handle, ACPI_LIST_BACKLIGHT)
            .into_iter()
            .find_map(|path| {
                let levels = eval(handle, &format!("{path}._BCL"))
                    .and_then(|v| backlight::decode_bcl(&v))?;
                Some(Backlight { path, levels })
            });
        // Thermal zones: trips are static — evaluate once here; only
        // `_TMP` is re-read per sample.
        for path in acpi_list(handle, ACPI_LIST_TZ) {
            let trips = TripPoints {
                critical_dk: eval(handle, &format!("{path}._CRT")).and_then(|v| decode_temp_dk(&v)),
                passive_dk: eval(handle, &format!("{path}._PSV")).and_then(|v| decode_temp_dk(&v)),
            };
            devices.zones.push(ThermalZone { path, trips });
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

    // ---- Track E: governor state ----------------------------------------
    let mut governor = Governor::new(GovernorMode::Conservative);
    let mut prev_snapshot = cpufreq_status();
    let mech = match prev_snapshot {
        Some(s) if s.mechanism == CPUFREQ_MECH_HWP => CpufreqMech::Hwp,
        _ => CpufreqMech::None,
    };
    let mut last_announced_target: Option<u8> = None;
    let mut last_thermal = ThermalWire::NoZones;
    let mut poweroff_started = false;
    let mut last_brightness: Option<u8> = None;

    announce(&format!(
        "POWERD:ready battery={} ac={} zones={} mech={} backlight={} sleep={}\n",
        devices.battery_path.as_deref().unwrap_or("none"),
        devices.ac_path.as_deref().unwrap_or("assumed-online"),
        devices.zones.len(),
        mech.as_str(),
        devices
            .backlight
            .as_ref()
            .map_or("none", |b| b.path.as_str()),
        sleep_str(devices.sleep_bits),
    ));

    // ---- Serve ----------------------------------------------------------
    let mut msg = IpcMessage::new(0);
    let mut bulk = [0u8; REPLY_BUF];
    loop {
        bulk.fill(0);
        let deadline_ns = monotonic_now_ns().saturating_add(GOVERNOR_TICK_NS);
        let rc = syscall_lib::ipc_recv_msg_timeout(ep, &mut msg, &mut bulk, deadline_ns);
        if rc == NEG_ETIMEDOUT {
            // Governor tick: load delta + thermal cap → target → kernel.
            let Some(snap) = cpufreq_status() else {
                continue;
            };
            let load = prev_snapshot
                .and_then(|prev| snap.load_pct_since(&prev))
                .unwrap_or(0);
            prev_snapshot = Some(snap);
            let (temp_dc, thermal) = devices.thermal_sample();
            if severity_wire(thermal) > severity_wire(ThermalWire::Normal)
                && thermal != last_thermal
            {
                announce(&format!(
                    "POWERD:thermal state={} temp_dc={temp_dc}\n",
                    thermal.as_str()
                ));
            }
            last_thermal = thermal;
            // D.3 thermal-runaway safety: a zone at/above `_CRT` initiates
            // the graceful poweroff (never fires on zone-less QEMU).
            if thermal == ThermalWire::Critical {
                initiate_poweroff("thermal-critical", &mut poweroff_started);
            }
            let target = governor.next(load, thermal_cap(thermal));
            set_perf(target);
            if last_announced_target != Some(target) {
                announce(&format!(
                    "POWERD:governor mode={} target={target} load={load}%\n",
                    governor.mode.as_str()
                ));
                last_announced_target = Some(target);
            }
            continue;
        }
        if rc == u64::MAX {
            continue;
        }
        if rc == ACPI_EVENT {
            // Pushed event from acpid (no reply cap — fire-and-forget).
            let code = msg.data[0];
            let len = bulk.iter().position(|&b| b == 0).unwrap_or(bulk.len());
            let path = core::str::from_utf8(&bulk[..len]).unwrap_or("<non-utf8>");
            announce(&format!("POWERD:event path={path} code={code:#x}\n"));
            // D.3 routing: the power button (fixed-feature pseudo-path or a
            // control-method PNP0C0C notify) initiates graceful poweroff.
            if code == 0x80 && (path == "\\FIXED.PWRBTN" || path.ends_with("PWRB")) {
                initiate_poweroff("power-button", &mut poweroff_started);
            }
            // Track F lid routing (charter D.3): suspend fails closed
            // until the F.3 resume path exists, so lid-close falls back
            // to locking the session (`PNP0C0D` lid devices are named
            // LID0/LID_ across firmwares; no QEMU model — HW-validated).
            if code == 0x80 && path.contains("LID") {
                announce("POWERD:lid action=lock (suspend fails closed)\n");
                lock_session();
            }
            continue;
        }
        let Some(reply_cap) = msg.reply_cap_handle() else {
            continue;
        };
        if rc == u64::from(POWER_STATUS) {
            let status = PowerStatusWire {
                governor: governor.mode,
                mech,
                perf: governor.current(),
                ..devices.status()
            };
            let encoded = status.encode();
            syscall_lib::ipc_store_reply_bulk(&encoded);
            syscall_lib::ipc_reply(reply_cap, 0, encoded.len() as u64);
        } else if rc == u64::from(POWER_SUSPEND) {
            // Track F.3: attempt a real S3. The kernel enter-sleep path
            // returns -ENOSYS when \_S3/FACS were never registered (S3
            // disabled or hardware-reduced firmware) — the fail-closed
            // posture survives on exactly the platforms that need it.
            if let Some(acpi) = devices.acpi {
                // \_PTS(3): prepare-to-sleep (optional method — a failed
                // evaluation on firmware without it is not an error).
                let _ = eval_arg(acpi, "\\_PTS", 3);
            }
            announce("POWERD:suspending\n");
            // SAFETY: argument-less m3OS-native syscall (root-gated).
            let rc2 = unsafe { syscall_lib::syscall1(SYS_POWER_ENTER_SLEEP, 0) } as i64;
            if rc2 == 0 {
                // We are on the other side of a suspend/resume cycle.
                //
                // Drain the wake-side event burst BEFORE calling acpid:
                // the wake itself sets PWRBTN_STS, and acpid is blocked
                // pushing that event at our endpoint — it cannot serve
                // a \_WAK call until the push is received (found live:
                // a clean cross-daemon deadlock, acpid retrying its push
                // while powerd waited on acpid's reply). Wake-window
                // button events are artifacts of the wake, NOT poweroff
                // requests — announce and drop.
                for _ in 0..8 {
                    let deadline = monotonic_now_ns().saturating_add(150_000_000);
                    let rc3 = syscall_lib::ipc_recv_msg_timeout(ep, &mut msg, &mut bulk, deadline);
                    if rc3 == NEG_ETIMEDOUT || rc3 == u64::MAX {
                        break;
                    }
                    if rc3 == ACPI_EVENT {
                        let code = msg.data[0];
                        let len = bulk.iter().position(|&b| b == 0).unwrap_or(bulk.len());
                        let path = core::str::from_utf8(&bulk[..len]).unwrap_or("<non-utf8>");
                        announce(&format!(
                            "POWERD:wake-event path={path} code={code:#x} (wake artifact, dropped)\n"
                        ));
                    } else if let Some(rc) = msg.reply_cap_handle() {
                        // A queued query from before/during the suspend —
                        // refuse it cleanly rather than servicing out of
                        // order (the caller retries).
                        syscall_lib::ipc_reply(rc, u64::MAX, 0);
                    }
                }
                if let Some(acpi) = devices.acpi {
                    let _ = eval_arg(acpi, "\\_WAK", 3);
                }
                // B.3 resume hook: re-apply the last-set brightness (the
                // panel controller lost it with the platform reset).
                if let Some(pct) = last_brightness {
                    let _ = devices.set_brightness(pct);
                }
                announce("POWERD:resume\n");
                syscall_lib::ipc_reply(reply_cap, 0, 0);
            } else {
                announce(&format!(
                    "POWERD:suspend rejected rc={rc2} firmware={}\n",
                    sleep_str(devices.sleep_bits)
                ));
                syscall_lib::ipc_reply(reply_cap, u64::MAX, devices.sleep_bits as u64);
            }
        } else if rc == u64::from(POWER_SET_BRIGHTNESS) {
            // Track B: pct in data0 → nearest _BCL level → _BCM. The
            // last-set level re-applies on resume with Track F.
            match devices.set_brightness(msg.data[0].min(100) as u8) {
                Some(applied) => {
                    last_brightness = Some(applied);
                    announce(&format!("POWERD:backlight set={applied}%\n"));
                    syscall_lib::ipc_reply(reply_cap, 0, applied as u64);
                }
                None => {
                    syscall_lib::ipc_reply(reply_cap, u64::MAX, 0);
                }
            }
        } else {
            syscall_lib::ipc_reply(reply_cap, u64::MAX, 0);
        }
    }
}

/// [`ThermalWire`] ordered by severity (`NoZones` and `Normal` both
/// benign).
fn severity_wire(t: ThermalWire) -> u8 {
    match t {
        ThermalWire::NoZones | ThermalWire::Normal => 0,
        ThermalWire::Passive => 1,
        ThermalWire::Critical => 2,
    }
}

/// Track F lid fallback: lock the session (the `m3ctl lock` shape — a
/// forked child execs `/bin/lockscreen`).
fn lock_session() {
    let pid = syscall_lib::fork();
    if pid == 0 {
        let path: &[u8] = b"/bin/lockscreen\0";
        let argv: [*const u8; 2] = [path.as_ptr(), core::ptr::null()];
        let envp: [*const u8; 1] = [core::ptr::null()];
        syscall_lib::execve(path, &argv, &envp);
        syscall_lib::exit(1);
    }
}

/// Phase 103 D.3 — initiate the graceful poweroff chain, once. Spawns
/// `/bin/shutdown` (SIGTERM to init → reverse-dependency service stop →
/// `sys_reboot(POWER_OFF)` → kernel sync + ACPI S5). The work happens in
/// a forked child because powerd itself is a supervised service: init's
/// teardown SIGTERMs this daemon mid-shutdown, while the orphaned child
/// (not a service) survives to fire the final reboot syscall.
fn initiate_poweroff(reason: &str, started: &mut bool) {
    if *started {
        return;
    }
    *started = true;
    announce(&format!("POWERD:poweroff reason={reason}\n"));
    let pid = syscall_lib::fork();
    if pid == 0 {
        // Child: exec the existing shutdown coreutil (owns the
        // kill(1)+grace+reboot sequence).
        let path: &[u8] = b"/bin/shutdown\0";
        let argv: [*const u8; 2] = [path.as_ptr(), core::ptr::null()];
        let envp: [*const u8; 1] = [core::ptr::null()];
        syscall_lib::execve(path, &argv, &envp);
        syscall_lib::exit(1); // exec failed
    } else if pid < 0 {
        announce("powerd: WARNING poweroff fork failed\n");
        *started = false;
    }
}

syscall_lib::entry_point!(program_main);
