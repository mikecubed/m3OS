//! `audio_server` binary entry point — Phase 57 Track D.1 / H.2.
//!
//! Run-time flow: write the boot marker, attempt to claim the sentinel
//! AC'97 BDF, create the command endpoint, register it under `audio.cmd`
//! (always — even when hardware is absent so `session_manager` does not
//! text-fallback), subscribe to the audio IRQ via
//! [`irq::subscribe_and_bind`], emit the `AUDIO_SMOKE:server:READY`
//! sentinel, and enter [`irq::run_io_loop`] — a non-returning IRQ / IPC
//! dispatch loop.
//!
//! When the AC'97 controller is absent (QEMU without `-device AC97`), the
//! server falls through to [`run_stub_loop`], a no-op IPC loop that
//! discards all client requests and replies with `SubmitAck{0}`.  This
//! keeps `audio.cmd` registered and alive so `session_manager` can
//! complete its boot sequence without triggering text-fallback.
//!
//! Track D.1 lands the scaffold; Tracks D.2..D.5 land the real backend,
//! stream registry, IRQ multiplex, and single-client policy.  Track H.2
//! lands this stub-mode path.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]
#![cfg_attr(not(test), feature(alloc_error_handler))]

extern crate alloc;
#[cfg(test)]
extern crate std;

#[cfg(not(test))]
use core::alloc::Layout;

#[cfg(not(test))]
use audio_server::{
    BOOT_LOG_MARKER, SENTINEL_BUS, SENTINEL_DEVICE, SENTINEL_FUNCTION, SERVER_READY_SENTINEL,
    SERVICE_NAME, client::ClientRegistry, device::Ac97Backend, irq, stream::StreamRegistry,
    stub::stub_reply_for,
};

#[cfg(not(test))]
use driver_runtime::{DeviceCapKey, DeviceHandle, ipc::EndpointCap};
#[cfg(not(test))]
use syscall_lib::STDOUT_FILENO;
#[cfg(not(test))]
use syscall_lib::heap::BrkAllocator;

#[cfg(not(test))]
#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[cfg(not(test))]
#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "audio_server: alloc error\n");
    syscall_lib::exit(99)
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "audio_server: PANIC\n");
    syscall_lib::exit(101)
}

#[cfg(not(test))]
syscall_lib::entry_point!(program_main);

#[cfg(not(test))]
fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, BOOT_LOG_MARKER);

    // Phase H.2: probe first, but do NOT exit on absence — the IPC
    // endpoint is registered unconditionally below so that
    // `session_manager`'s `await_ready("audio_server")` probe
    // (which looks up "audio.cmd") succeeds even when no AC'97
    // hardware is present in the machine.
    let key = DeviceCapKey::new(0, SENTINEL_BUS, SENTINEL_DEVICE, SENTINEL_FUNCTION);
    let device_opt = DeviceHandle::claim(key).ok();

    // Register the command endpoint regardless of hardware presence.
    let ep = syscall_lib::create_endpoint();
    if ep == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "audio_server: endpoint create failed\n");
        return 4;
    }
    let ep_u32 = match u32::try_from(ep) {
        Ok(id) => id,
        Err(_) => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "audio_server: endpoint id out of u32 range\n",
            );
            return 6;
        }
    };
    let rc = syscall_lib::ipc_register_service(ep_u32, SERVICE_NAME);
    if rc == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "audio_server: service register failed\n");
        return 5;
    }

    let endpoint = EndpointCap::new(ep_u32);

    // Branch on whether we have real hardware.
    let device = match device_opt {
        Some(d) => d,
        None => {
            // Track H.2: no AC'97 hardware detected. Register the
            // stub loop so `session_manager` can proceed to the
            // graphical session.  Log a single warning and enter the
            // no-op IPC server.
            syscall_lib::write_str(
                STDOUT_FILENO,
                "audio_server: WARNING — no AC'97 device found; running in stub mode (silent)\n",
            );
            syscall_lib::write_str(STDOUT_FILENO, SERVER_READY_SENTINEL);
            return run_stub_loop(endpoint);
        }
    };

    let mut backend = match Ac97Backend::init(device) {
        Ok(b) => b,
        Err(_) => {
            syscall_lib::write_str(STDOUT_FILENO, "audio_server: AC'97 init failed\n");
            return 3;
        }
    };

    let irq_notif = match irq::subscribe_and_bind(backend.device(), endpoint) {
        Ok(n) => n,
        Err(_) => {
            syscall_lib::write_str(STDOUT_FILENO, "audio_server: IRQ bind failed\n");
            return 7;
        }
    };

    syscall_lib::write_str(STDOUT_FILENO, SERVER_READY_SENTINEL);

    let mut streams = StreamRegistry::new();
    let mut clients = ClientRegistry::new();
    irq::run_io_loop(
        &mut backend,
        &mut streams,
        &mut clients,
        endpoint,
        irq_notif,
    )
}

/// Stub IPC loop — runs when no AC'97 hardware was found.
///
/// Accepts any incoming client message, discards the PCM payload, and
/// replies with a `SubmitAck { frames_consumed: 0 }` so callers that
/// poll for consumed-frame progress see a non-error reply.  `Open`
/// receives `Opened { stream_id: 0 }`; `Close` receives `Closed`.
/// Any other message receives `SubmitAck { 0 }` (silent discard).
///
/// This function never returns — it loops forever so `audio.cmd`
/// stays registered in the kernel's service table for the lifetime
/// of the boot session.
#[cfg(not(test))]
fn run_stub_loop(endpoint: EndpointCap) -> i32 {
    use driver_runtime::ipc::{IpcBackend, RecvResult};
    use kernel_core::audio::{AudioError, ClientMessage, ServerMessage};

    let mut transport = driver_runtime::ipc::SyscallBackend;
    loop {
        let result = match transport.recv(endpoint) {
            Ok(r) => r,
            Err(_) => {
                syscall_lib::write_str(
                    STDOUT_FILENO,
                    "audio_server: stub: recv error — continuing\n",
                );
                continue;
            }
        };
        let frame = match result {
            RecvResult::Notification(_bits) => {
                // Spurious notification in stub mode — ignore.
                continue;
            }
            RecvResult::Message(f) => f,
        };

        // Decode and dispatch through stub_reply_for (pure function,
        // host-tested in `stub.rs`).  All PCM data is discarded —
        // there is no AC'97 device to push it to.
        let reply: ServerMessage = match ClientMessage::decode(&frame.bulk) {
            Ok((msg, _)) => stub_reply_for(&msg),
            Err(_) => {
                // Decode error — return a benign error rather than
                // panicking.
                ServerMessage::SubmitError(AudioError::InvalidArgument)
            }
        };

        let mut buf = [0u8; 64];
        if let Ok(n) = reply.encode(&mut buf) {
            let _ = transport.store_reply_bulk(&buf[..n]);
        }
        let _ = transport.reply(frame.label, 0);
    }
}
