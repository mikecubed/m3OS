//! Phase 56 Track C.6 — protocol-reference client for `display_server`.
//!
//! This binary is a *minimal* visual-smoke client. It does not aim to be a
//! real application; its sole purpose is to prove that the Phase 56
//! `display_server` daemon (Track C) actually composites client-supplied
//! surfaces by walking the protocol verbs end-to-end:
//!
//!   1. Look up the `"display"` service in the IPC registry (with bounded
//!      retry — `display_server` may still be coming up at boot).
//!   2. Send `Hello { protocol_version, capabilities = 0 }`.
//!   3. Allocate a 32 × 32 BGRA8888 surface buffer (the largest size that
//!      fits the kernel's `MAX_BULK_LEN` of 4096 bytes — see
//!      `userspace/lib/surface_buffer/`).
//!   4. Walk the surface lifecycle: `CreateSurface`, `SetSurfaceRole`
//!      (Toplevel), `AttachBuffer`, `CommitSurface`.
//!   5. Idle — the demo stays alive so a developer can manually inspect the
//!      composited output via `cargo xtask run-gui`.
//!
//! # Engineering discipline
//!
//! Per the Phase 56 task doc: this binary contains **no** `unwrap` /
//! `expect` / `panic!` calls outside the documented init-failure points.
//! Every fallible call is checked and reported via `syscall_lib::write_str`.
//!
//! # Pixel transport
//!
//! True per-buffer pixel transport (Track B.4 + composer wiring in C.4) is
//! still landing in follow-up PRs. This client therefore stops after the
//! protocol-verb round-trip and *does not* push pixel bytes across IPC; the
//! background colour `0x00FF_8800` (orange) documented below is the value
//! the surface buffer is filled with, ready to ship the moment the bulk
//! transport is enabled.
#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use core::alloc::Layout;

use kernel_core::display::protocol::{
    BufferId, ClientMessage, PROTOCOL_VERSION, SurfaceId, SurfaceRole,
};
use surface_buffer::{PixelFormat, SurfaceBuffer, SurfaceBufferId};
use syscall_lib::STDOUT_FILENO;
use syscall_lib::heap::BrkAllocator;

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "gfx-demo: alloc error\n");
    syscall_lib::exit(99)
}

/// Demo-surface fill colour, BGRA8888 little-endian. Documented in the
/// banner so manual smoke validation knows what to expect when the C.4
/// composer wiring lands.
const DEMO_FILL_BGRA: u32 = 0x00FF_8800;

/// IPC labels. Must match the `display_server::client` dispatcher:
/// protocol-verb messages travel on label `1` (`LABEL_VERB`), pixel-bulk
/// messages on label `2` (`LABEL_PIXELS`). A future cleanup will lift
/// these constants into a shared crate so the demo and the server can
/// import a single source of truth.
const LABEL_PROTOCOL: u64 = 1;

/// Stack buffer for `ClientMessage::encode`. The largest Phase 56
/// client-message body is `SetSurfaceRole(Layer)` at ~24 bytes; a 128-byte
/// stack buffer leaves ample headroom while staying well below
/// `MAX_FRAME_BODY_LEN` (= 4096).
const ENCODE_BUF_LEN: usize = 128;

syscall_lib::entry_point!(program_main);

fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(
        STDOUT_FILENO,
        "gfx-demo: starting (Phase 56 — C.6 protocol-reference client)\n",
    );
    syscall_lib::write_str(
        STDOUT_FILENO,
        "gfx-demo: surface fill = 0x00FF8800 (orange, BGRA8888 LE)\n",
    );

    // ----- Service lookup with bounded retry -----------------------------
    let server_handle = match lookup_display_with_backoff() {
        Some(h) => h,
        None => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "gfx-demo: display service not available after retry budget\n",
            );
            return 1;
        }
    };
    syscall_lib::write_str(STDOUT_FILENO, "gfx-demo: connected to 'display'\n");

    // ----- Protocol round-trip -------------------------------------------
    let mut buf = [0u8; ENCODE_BUF_LEN];

    // 1. Hello.
    let hello = ClientMessage::Hello {
        protocol_version: PROTOCOL_VERSION,
        capabilities: 0,
    };
    if !send_message(server_handle, &hello, &mut buf, "Hello") {
        return 1;
    }

    // 2. Allocate the surface buffer. SurfaceBuffer::new is fallible —
    //    handle every variant rather than unwrapping.
    let mut surface_buf =
        match SurfaceBuffer::new(SurfaceBufferId(1), 32, 32, PixelFormat::Bgra8888) {
            Ok(b) => b,
            Err(_) => {
                syscall_lib::write_str(STDOUT_FILENO, "gfx-demo: SurfaceBuffer::new failed\n");
                return 1;
            }
        };
    surface_buf.fill(DEMO_FILL_BGRA);
    syscall_lib::write_str(
        STDOUT_FILENO,
        "gfx-demo: allocated 32x32 BGRA buffer, filled with orange\n",
    );

    // 3. CreateSurface.
    let create = ClientMessage::CreateSurface {
        surface_id: SurfaceId(1),
    };
    if !send_message(server_handle, &create, &mut buf, "CreateSurface") {
        return 1;
    }

    // 4. SetSurfaceRole(Toplevel).
    let role = ClientMessage::SetSurfaceRole {
        surface_id: SurfaceId(1),
        role: SurfaceRole::Toplevel,
    };
    if !send_message(server_handle, &role, &mut buf, "SetSurfaceRole(Toplevel)") {
        return 1;
    }

    // 5. AttachBuffer.
    let attach = ClientMessage::AttachBuffer {
        surface_id: SurfaceId(1),
        buffer_id: BufferId(1),
    };
    if !send_message(server_handle, &attach, &mut buf, "AttachBuffer") {
        return 1;
    }

    // 6. CommitSurface — final verb that asks the compositor to flip.
    let commit = ClientMessage::CommitSurface {
        surface_id: SurfaceId(1),
    };
    if !send_message(server_handle, &commit, &mut buf, "CommitSurface") {
        return 1;
    }

    syscall_lib::write_str(
        STDOUT_FILENO,
        "gfx-demo: protocol round-trip complete; idling for inspection\n",
    );

    // ----- Idle loop ------------------------------------------------------
    //
    // ~10 minutes of 1-second sleeps so the demo stays alive long enough
    // for a human running `cargo xtask run-gui` to inspect the framebuffer.
    // The exact duration is not load-bearing — the goal is "stays up
    // indefinitely from the smoke-test operator's perspective".
    for _ in 0..600u32 {
        let _ = syscall_lib::nanosleep_for(1, 0);
    }

    0
}

/// Look up the `"display"` service with a bounded retry loop. Mirrors the
/// `display_server` framebuffer-acquisition retry shape so two daemons
/// racing at boot do not produce flaky failures.
fn lookup_display_with_backoff() -> Option<u32> {
    const MAX_ATTEMPTS: u32 = 8;
    const BACKOFF_NS: u32 = 5_000_000; // 5 ms

    for attempt in 0..MAX_ATTEMPTS {
        let raw = syscall_lib::ipc_lookup_service("display");
        if raw != u64::MAX {
            return Some(raw as u32);
        }
        if attempt + 1 == MAX_ATTEMPTS {
            return None;
        }
        let _ = syscall_lib::nanosleep_for(0, BACKOFF_NS);
    }
    None
}

/// Encode `msg` into `buf` and send it to `server_handle` via
/// `ipc_call_buf`. Logs success/failure with the supplied step name.
/// Returns `true` on success, `false` on encode or IPC failure.
fn send_message(server_handle: u32, msg: &ClientMessage, buf: &mut [u8], step: &str) -> bool {
    let len = match msg.encode(buf) {
        Ok(n) => n,
        Err(_) => {
            syscall_lib::write_str(STDOUT_FILENO, "gfx-demo: encode failed for ");
            syscall_lib::write_str(STDOUT_FILENO, step);
            syscall_lib::write_str(STDOUT_FILENO, "\n");
            return false;
        }
    };

    let reply = syscall_lib::ipc_call_buf(server_handle, LABEL_PROTOCOL, 0, &buf[..len]);
    if reply == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "gfx-demo: ipc_call_buf failed for ");
        syscall_lib::write_str(STDOUT_FILENO, step);
        syscall_lib::write_str(STDOUT_FILENO, "\n");
        return false;
    }

    syscall_lib::write_str(STDOUT_FILENO, "gfx-demo: sent ");
    syscall_lib::write_str(STDOUT_FILENO, step);
    syscall_lib::write_str(STDOUT_FILENO, " ok\n");
    true
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "gfx-demo: PANIC\n");
    syscall_lib::exit(101)
}
