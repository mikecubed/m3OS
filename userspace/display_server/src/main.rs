//! Phase 56 — userspace display server (compositor).
//!
//! This binary owns presentation: it claims the primary framebuffer from
//! the kernel via the Phase 47/56 syscall surface, registers itself in the
//! service registry as `"display"`, and idles on its IPC endpoint so init's
//! supervisor sees a healthy daemon.
//!
//! Tracks landed in this PR:
//!   * **C.1** — crate scaffolding + four-place new-binary wiring.
//!   * **C.2** — framebuffer acquisition through the [`KernelFramebufferOwner`]
//!     impl of the `kernel-core::display::fb_owner::FramebufferOwner` trait.
//!
//! Tracks deferred to follow-up PRs (foundation in `kernel-core`):
//!   * **C.3 / C.4** — surface state machine + damage-tracked composer.
//!   * **C.5** — AF_UNIX / IPC client-protocol dispatcher.
//!   * **C.6** — `gfx-demo` reference client.
//!   * **B.2 / B.3 / B.4** — kernel-side wiring for mouse, frame-tick,
//!     and surface-buffer transport (pure-logic cores already in
//!     `kernel-core::input::mouse`, `kernel-core::display::frame_tick`,
//!     `kernel-core::display::buffer`).
#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

mod animation;
mod borders;
mod client;
mod compose;
mod config;
mod control;
mod decoration;
mod fb;
mod input;
mod keybind;
mod surface;
mod workspace;

use core::alloc::Layout;
use kernel_core::display::fb_owner::FramebufferOwner;
use kernel_core::display::protocol::{Rect, ServerMessage, SurfaceId, SurfaceRole};
use kernel_core::display::stats::FrameStatsRing;
use kernel_core::input::bind_table::{BindTable, GrabState};
use kernel_core::input::dispatch::SurfaceGeometry;
use syscall_lib::IpcMessage;
use syscall_lib::STDOUT_FILENO;
use syscall_lib::heap::BrkAllocator;

use crate::client::{FatalReason, InboundFrame, dispatch};
use crate::compose::{ComposeContext, fill_background, run_compose_filtered};
use crate::workspace::WorkspaceLayoutAdapter;

/// Phase 57d follow-up — per-second diagnostic counters for the SHM
/// transport bring-up. Sampled and logged once per second from the
/// compose path so a hung surface, a stuck compose tick, or a dropped
/// Damage event produces visible signal in the boot transcript
/// instead of silent emptiness. Ripped once SHM is stable.
static DIAG_COMPOSES_RUN: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static DIAG_FB_WRITES: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static DIAG_PROTOCOL_VIOLATION_LOG_BUDGET: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(16);
use crate::control::{
    ControlSubscriptions, DebugCrashPolicy, null_subscriber_sender, publish_bind_triggered,
    publish_focus_changed, publish_surface_created, publish_surface_destroyed, record_frame_sample,
};
use crate::fb::KernelFramebufferOwner;
use crate::input::{InputEffect, InputWiring};
use crate::surface::SurfaceRegistry;

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "display_server: alloc error\n");
    syscall_lib::exit(99)
}

/// Phase 56 startup background colour (encoded BGRA8888 / RGBA8888 — both
/// formats happen to render this byte order as a uniform deep teal). The
/// expected startup pixel value is `0x002B_5A4B`, recorded here so manual
/// smoke validation knows what to expect on `cargo xtask run-gui --fresh`.
///
/// `pub` so `compose::clear_rect_to_background` (the cursor-trail
/// damage clear) writes the same value as the initial fill — otherwise
/// the cursor leaves opaque-black squares wherever it has been on the
/// teal background.
pub const BG_PIXEL: u32 = 0x002B_5A4Bu32;

syscall_lib::entry_point_with_env!(program_main);

/// Phase 56 Track F.2 — debug-crash gate. The dispatcher consults this
/// once per `ControlCommand::DebugCrash` it decodes; production boots
/// leave it disabled so a hostile client cannot crash the compositor.
/// The gate is set from the env var
/// `M3OS_DISPLAY_SERVER_DEBUG_CRASH=1` read once at startup. The init
/// daemon passes the env var through only when `/etc/m3os-smoke-test-mode`
/// is present (see `userspace/init/src/main.rs::ENV_DISPLAY_SERVER_DEBUG_CRASH`).
const ENV_DEBUG_CRASH: &str = "M3OS_DISPLAY_SERVER_DEBUG_CRASH";

/// Phase 56 close-out (G.1 regression) — env var name. Same gating
/// pattern as `ENV_DEBUG_CRASH`: production boots leave this unset and
/// the dispatcher shadows `ReadBackPixel` to `UnknownVerb`.
const ENV_READBACK: &str = "M3OS_DISPLAY_SERVER_READBACK";

/// Phase 56 close-out (G.2 regression) — env var name for the
/// synthetic-key-injection gate.
const ENV_INJECT_KEY: &str = "M3OS_DISPLAY_SERVER_INJECT_KEY";

/// Read the debug-crash gate from the process environment. Matches a
/// strict `M3OS_DISPLAY_SERVER_DEBUG_CRASH=1` so a typo or alternate
/// truthy spelling stays disabled.
fn debug_crash_policy_from_env(env: &[&str]) -> DebugCrashPolicy {
    for entry in env {
        if let Some(value) = entry.strip_prefix(ENV_DEBUG_CRASH).and_then(|rest| {
            // Match exactly `KEY=value` (single `=`).
            rest.strip_prefix('=')
        }) && value == "1"
        {
            return DebugCrashPolicy::enabled();
        }
    }
    DebugCrashPolicy::disabled()
}

/// Phase 56 close-out (G.1 regression) — read the readback gate from
/// the process environment. Same shape as `debug_crash_policy_from_env`.
fn readback_policy_from_env(env: &[&str]) -> control::ReadBackPolicy {
    for entry in env {
        if let Some(value) = entry
            .strip_prefix(ENV_READBACK)
            .and_then(|rest| rest.strip_prefix('='))
            && value == "1"
        {
            return control::ReadBackPolicy::enabled();
        }
    }
    control::ReadBackPolicy::disabled()
}

/// Phase 56 close-out (G.2 regression) — read the inject-key gate.
fn inject_key_policy_from_env(env: &[&str]) -> control::InjectKeyPolicy {
    for entry in env {
        if let Some(value) = entry
            .strip_prefix(ENV_INJECT_KEY)
            .and_then(|rest| rest.strip_prefix('='))
            && value == "1"
        {
            return control::InjectKeyPolicy::enabled();
        }
    }
    control::InjectKeyPolicy::disabled()
}

fn program_main(_args: &[&str], env: &[&str]) -> i32 {
    syscall_lib::write_str(
        STDOUT_FILENO,
        "display_server: starting (Phase 56 — C.1+C.2)\n",
    );

    // Phase 56 Track F.2 — read the debug-crash gate once at startup.
    // The dispatcher consults this on every `ControlCommand::DebugCrash`;
    // disabled (the production default) shadows the verb back to
    // `UnknownVerb`.
    let debug_crash = debug_crash_policy_from_env(env);
    if debug_crash.is_enabled() {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "display_server: F.2 debug-crash verb ENABLED via M3OS_DISPLAY_SERVER_DEBUG_CRASH=1\n",
        );
    }

    // Phase 56 close-out (G.1) — read the readback gate. Same shape:
    // disabled in production, enabled by the multi-client-coexistence
    // regression's marker file.
    let readback = readback_policy_from_env(env);
    if readback.is_enabled() {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "display_server: G.1 ReadBackPixel verb ENABLED via M3OS_DISPLAY_SERVER_READBACK=1\n",
        );
    }

    // Phase 56 close-out (G.2) — read the inject-key gate.
    let inject_key_policy = inject_key_policy_from_env(env);
    if inject_key_policy.is_enabled() {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "display_server: G.2 InjectKey verb ENABLED via M3OS_DISPLAY_SERVER_INJECT_KEY=1\n",
        );
    }

    // ----- Service endpoints ----------------------------------------------
    let ep_handle = syscall_lib::create_endpoint();
    if ep_handle == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "display_server: failed to create endpoint\n");
        return 1;
    }
    let ep_handle = ep_handle as u32;

    // Phase 56 Track E.4 — second IPC endpoint for the control socket.
    // The endpoint is later registered as `"display-control"` so `m3ctl`
    // (and any future native bar / launcher client) can locate it via
    // `ipc_lookup_service`. The codec, dispatcher, subscription
    // registry, and runtime byte-flow are all wired: each loop
    // iteration `serve_one_control_request` non-blocking-recvs one
    // pending request via `SYS_IPC_TRY_RECV_MSG` and stages a reply
    // bulk via `ipc_store_reply_bulk` + `ipc_reply`. Subscription
    // *event* push (server → client) remains deferred (see
    // `control::publish_*` TODO markers) — it needs a separate
    // cap-transfer or polling design.
    let ctl_ep_handle = syscall_lib::create_endpoint();
    if ctl_ep_handle == u64::MAX {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "display_server: failed to create control endpoint\n",
        );
        return 1;
    }
    let ctl_ep_handle = ctl_ep_handle as u32;

    // ----- Framebuffer acquisition (C.2) ---------------------------------
    let mut owner = match acquire_framebuffer_with_backoff() {
        Ok(o) => o,
        Err(reason) => {
            syscall_lib::write_str(STDOUT_FILENO, "display_server: ");
            syscall_lib::write_str(STDOUT_FILENO, reason);
            syscall_lib::write_str(STDOUT_FILENO, "\n");
            return 1;
        }
    };
    let meta = owner.metadata();

    syscall_lib::write_str(STDOUT_FILENO, "display_server: framebuffer acquired\n");
    log_fb_meta(meta.width, meta.height, meta.stride_bytes);

    // Initial whole-screen wipe. The framebuffer still contains the
    // kernel framebuffer console's boot-log text at this point;
    // without an explicit fill, the surface- and cursor-blit passes
    // (which only paint mapped-surface and cursor regions) would leave
    // the boot text visible everywhere else, and the cursor-trail
    // damage path would create a teal trail wherever the mouse moved.
    // The first-frame branch in `run_compose` does the same thing for
    // any subsequent context reset; running it once here covers the
    // pre-first-frame interval where a slow client still hasn't
    // committed any surface.
    if let Err(_e) = fill_background(&mut owner) {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "display_server: initial background fill failed\n",
        );
    }

    // ----- Input wiring (D.3) --------------------------------------------
    let mut input_wiring = InputWiring::new();
    if input_wiring.kbd.is_connected() {
        syscall_lib::write_str(STDOUT_FILENO, "display_server: kbd service connected\n");
    } else {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "display_server: kbd service unavailable (continuing without keyboard)\n",
        );
    }
    if input_wiring.mouse.is_connected() {
        syscall_lib::write_str(STDOUT_FILENO, "display_server: mouse service connected\n");
    } else {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "display_server: mouse service unavailable (continuing without pointer)\n",
        );
    }

    // Per-frame input policy state held by `display_server` itself.
    // The dispatcher takes a borrow of these on every drain and never
    // owns them — that keeps the compositor's focus / bind / grab
    // tracking auditable in one place.
    //
    // Track E.4 — the bind table is now `mut` because the control
    // socket's `register-bind` / `unregister-bind` verbs mutate it.
    // The reference passed to `InputWiring::drain_one_pass` is still
    // a `&BindTable`; the mutability is purely for the control
    // dispatcher's use.
    let mut bind_table = BindTable::new();
    let mut grab_state = GrabState::new();
    let mut focused: Option<SurfaceId> = None;
    let mut pointer_position: (i32, i32) = (0, 0);

    // ----- Phase 72 — load `/etc/compositor.conf` --------------------------
    let mut compositor_config = config::CompositorConfig::defaults();
    if let Some(parsed) = load_compositor_config(config::CONFIG_PATH) {
        compositor_config = parsed;
    }
    syscall_lib::write_str(
        STDOUT_FILENO,
        "display_server: compositor.conf loaded; outer_gap=",
    );
    write_u32(compositor_config.gaps.outer as u32);
    syscall_lib::write_str(STDOUT_FILENO, " inner_gap=");
    write_u32(compositor_config.gaps.inner as u32);
    syscall_lib::write_str(STDOUT_FILENO, " border_w=");
    write_u32(compositor_config.borders.width as u32);
    syscall_lib::write_str(STDOUT_FILENO, "\n");

    // ----- Phase 72 — workspaces + keybind chord engine --------------------
    let mut workspace_mgr =
        workspace::WorkspaceManager::new(compositor_config.workspaces.defaults[0]);
    for i in 0..workspace::NUM_WORKSPACES {
        if let Some(ws) = workspace_mgr.workspace_mut(i) {
            ws.set_policy(compositor_config.workspaces.defaults[i]);
        }
    }
    let mut bind_stack = keybind::BindStack::new();
    // Apply user-supplied chord overrides from the config file.
    bind_stack.reload_default(&compositor_config.keybinds.user_chords);
    bind_stack.set_resize_step_px(compositor_config.keybinds.resize_step_px);

    // Track E.4 — control-socket subscription registry and frame-stats
    // ring. The registry is keyed by `ClientId`; Phase 56 uses a
    // single static `ClientId` because the in-process control endpoint
    // serves one connection at a time. The frame-stats ring fills as
    // the compose loop runs.
    let mut control_subs = ControlSubscriptions::new();
    let mut frame_stats = FrameStatsRing::new();
    let mut frame_index_counter: u64 = 0;
    // Snapshot of registered surface ids from the previous iteration.
    // Used to compute create / destroy deltas to publish on the
    // control-socket subscription registry — without rewriting
    // `client.rs` or `surface.rs`'s public APIs to surface lifecycle
    // hooks.
    let mut prev_surface_ids: alloc::vec::Vec<SurfaceId> = alloc::vec::Vec::new();

    // ----- Phase 56 single-threaded event loop (C.3 + C.4 + C.5 + D.3) ----
    //
    // The compositor multiplexes:
    //   * inbound IPC client messages (`ipc_recv_msg` on `ep_handle`)
    //   * the frame-tick (drained via `frame_tick_drain` syscall, B.3)
    //
    // Every iteration: receive one client message (`ipc_recv_msg` blocks
    // until traffic arrives), dispatch it, send the reply via the
    // reply capability the kernel staged in `header.data[3]` (Phase 56
    // close-out — the kernel now reports the reply-cap handle so
    // userspace doesn't have to guess; `data[3]` is reserved for this
    // purpose because `data[0..3]` are claimed by existing protocol
    // payloads in vfs_server / net_server / ramdisk), then drive one
    // compose pass
    // if a frame-tick has elapsed AND there is pending damage.
    //
    // Reply convention:
    //   * `RESP_OK` (= 0)        — message accepted, no further data
    //   * `RESP_FATAL` (= u64::MAX) — protocol violation; client should
    //                                 disconnect and reconnect
    //
    // The fuller server→client event channel (`Welcome`,
    // `SurfaceConfigured`, `BufferReleased`, ...) is currently logged
    // for diagnostic visibility but not yet transported back: per-client
    // out-of-band send caps land alongside Track D's input dispatcher
    // and Track E's control socket. For Phase 56's single-client demo
    // this keeps the call/reply contract intact (no deadlocked clients)
    // without prematurely committing to a multi-client wire.
    //
    // Frame-tick caveat: `ipc_recv_msg` blocks, so frame-tick-driven
    // composition only progresses while clients send traffic. `gfx-demo`
    // sends a fixed sequence at startup and then idles — that's enough
    // for Phase 56's protocol-reference smoke. A non-blocking
    // try-recv (or notification-bound recv) lands with the C.5 follow-up
    // when the input services start delivering events on this endpoint.
    const RESP_OK: u64 = 0;
    const RESP_FATAL: u64 = u64::MAX;
    let mut registry = SurfaceRegistry::new();
    let mut compose_ctx = ComposeContext::new();
    let mut bulk_buf = alloc::vec![0u8; client::MAX_BULK_BYTES];
    // Phase 72b — `[autostart]` execution latch. Flipped to `true`
    // after the first successful compose so the autostart entries
    // launch exactly once per compositor lifetime, matching
    // Hyprland's `exec-once` semantics. `m3ctl reload` does not
    // clear the latch — long-running respawn belongs in init or
    // session_manager.
    let mut autostart_ran = false;
    // Phase 72b (Track K.3) — last-known tile dimensions per Toplevel
    // surface. After each `arrange_current`, the loop diffs against
    // this map and emits `ServerMessage::SurfaceResized` to every
    // surface whose `(w, h)` changed. Term re-flows via its existing
    // Phase 69 handler (Screen::resize → TIOCSWINSZ → SIGWINCH).
    // DOOM consumes via Phase 72b Track K.4. Position-only changes
    // do not need notification — the client's surface buffer keeps
    // its intrinsic size and the compositor handles repositioning.
    let mut last_tile_dims: alloc::collections::BTreeMap<SurfaceId, (u32, u32)> =
        alloc::collections::BTreeMap::new();
    // Phase 57d follow-up — Tier 1 fullscreen-takeover gate. When a
    // control client (e.g. `/bin/fb-takeover`) sends `YieldFb`, the
    // server drops framebuffer ownership via `SYS_FB_YIELD` and sets
    // this flag; the compose loop then skips its frame-tick work
    // until `ReclaimFb` reverses the transition. Without the gate the
    // composer would race the takeover program on FB pages it no
    // longer owns.
    let mut fb_yielded = false;
    // Phase 57d follow-up — post-reclaim full-screen background fill.
    // Set to `true` by the reclaim handler so the next compose tick
    // calls `fill_background` before `run_compose`. Without this, any
    // pixels the takeover program drew outside our toplevel surfaces
    // (e.g. the doom HUD area when term's surface is centered and
    // smaller than the FB) keep showing through — the pure-logic
    // `run_compose` only paints surface and cursor regions, so the
    // background gutter would still display whatever doom left there.
    let mut needs_post_reclaim_fill = false;
    // Phase 57d follow-up — small ring of recently dispatched verb
    // frames so the next protocol-violation log can show whether the
    // failing bulk was preceded by a structurally similar frame, and
    // whether the offending opcode/body_len matches a frame seen one
    // or two iterations ago. Strictly diagnostic; rate-limited via the
    // same `DIAG_PROTOCOL_VIOLATION_LOG_BUDGET` as the headline log.
    let mut recent_frames = RecentFrames::new();
    // Phase 56 C.5 close-out — per-client outbound queue. The
    // dispatcher's `InputEffect::Outbound(ServerMessage::Key|Pointer)`
    // fires on focused-client key/pointer events; we accumulate the
    // resulting `ServerMessage`s here and the client drains them one
    // at a time via `LABEL_CLIENT_EVENT_PULL`. Capped at
    // `client::MAX_CLIENT_EVENT_QUEUE` per the Phase 56 resource
    // bounds; oldest is dropped when the cap is reached.
    // Phase 70 — each queued event carries the target `SurfaceId` so
    // the PULL handler can return only the events the calling client
    // is entitled to (i.e. its own focused-surface events). The
    // dispatcher's `InputEffect::Outbound` already names the target;
    // we just preserve that through to the wire.
    let mut client_event_queue: alloc::collections::VecDeque<(SurfaceId, ServerMessage)> =
        alloc::collections::VecDeque::with_capacity(client::MAX_CLIENT_EVENT_QUEUE);
    // Reusable encode buffer for the `LABEL_CLIENT_EVENT_PULL` reply
    // bulk. The largest `ServerMessage` body is `Pointer` at
    // `FRAME_HEADER_SIZE + POINTER_EVENT_WIRE_SIZE` ≈ 50 bytes; 128
    // bytes leaves ample headroom while staying well below
    // `MAX_BULK_BYTES`.
    let mut event_reply_buf = [0u8; 128];

    let reg = syscall_lib::ipc_register_service(ep_handle, "display");
    if reg == u64::MAX {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "display_server: failed to register 'display'\n",
        );
        return 1;
    }
    syscall_lib::write_str(STDOUT_FILENO, "display_server: registered as 'display'\n");

    // Phase 57d follow-up — separate service marker for "display has
    // grabbed graphical input". Registered lazily on the first Toplevel
    // map below. While unregistered, `stdin_feeder` keeps owning PS/2
    // keystrokes (the text-mode bridge); once a real graphical client
    // maps a Toplevel surface, `stdin_feeder` polls this name and
    // stands down so `display_server`'s focus dispatcher can deliver
    // typed `KeyEvent`s. Keeping the bootstrap split means clients
    // can still find `"display"` to send Hello/CreateSurface, while
    // the input-grab decision waits for an actual graphical surface.
    let mut input_owner_registered = false;

    let ctl_reg = syscall_lib::ipc_register_service(ctl_ep_handle, "display-control");
    if ctl_reg == u64::MAX {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "display_server: failed to register 'display-control'\n",
        );
        return 1;
    }

    loop {
        // 1. Try to receive one graphical-endpoint message non-blocking.
        //    Phase 56 close-out: switched from blocking `ipc_recv_msg`
        //    to `ipc_try_recv_msg` so this loop can multiplex the
        //    graphical endpoint, the control endpoint, the frame-tick
        //    drain, and the input pull-paths in a single thread. If
        //    `ipc_try_recv_msg` returns `u64::MAX`, no client traffic
        //    is queued; we fall through with `had_graphical = false`
        //    so the dispatch + reply block is skipped.
        let mut header = IpcMessage::new(0);
        let recv_ret = syscall_lib::ipc_try_recv_msg(ep_handle, &mut header, &mut bulk_buf);
        let had_graphical = recv_ret != u64::MAX;

        // Phase 56 C.5 close-out — `LABEL_CLIENT_EVENT_PULL` is an
        // out-of-band drain verb. Handle it here so it does not flow
        // through `dispatch`/`SurfaceRegistry`; the queue is a
        // main-loop concern and the verb's reply shape is not
        // `RESP_OK` / `RESP_FATAL`. We still let the rest of the loop
        // iterate so input drain, control-socket service, and the
        // compose pass are not starved by a pulling client.
        let pull_handled = had_graphical && header.label == client::LABEL_CLIENT_EVENT_PULL;
        if pull_handled {
            let reply_cap = header.data[3] as u32;
            // Phase 70 — `data[0]` carries a client-supplied surface
            // id and is used purely as a cooperative routing hint so
            // a well-behaved client only drains events targeted at its
            // own surface. This is NOT an access-control boundary:
            // any client that knows or guesses another surface id can
            // request it and receive that surface's queued events.
            // Real per-client ownership (separate outbound queues per
            // client identity) is deferred to Phase 71; until then,
            // the shared outbound queue + cooperative filter is the
            // minimum needed to let term + DOOM coexist without
            // racing for each other's key events.
            //
            // A value of `0` is the legacy wildcard (pre-Phase-70
            // clients pass `0`) and pops the next queued event
            // regardless of target — keeps old `m3ctl` / `gfx-demo`
            // callers working until they are updated.
            let requested = header.data[0] as u32;
            let pop_idx = if requested == 0 {
                if client_event_queue.is_empty() {
                    None
                } else {
                    Some(0usize)
                }
            } else {
                client_event_queue
                    .iter()
                    .position(|(target, _)| target.0 == requested)
            };
            let popped = pop_idx.and_then(|i| client_event_queue.remove(i));
            match popped {
                Some((_target, msg)) => match msg.encode(&mut event_reply_buf) {
                    Ok(n) => {
                        let _ = syscall_lib::ipc_store_reply_bulk(&event_reply_buf[..n]);
                        if reply_cap != 0 {
                            let _ = syscall_lib::ipc_reply(
                                reply_cap,
                                client::LABEL_CLIENT_EVENT_PULL,
                                0,
                            );
                        }
                    }
                    Err(_) => {
                        // Encode failure means the queued message was
                        // somehow malformed. Drop it, reply NONE so the
                        // caller recovers, log so a developer notices.
                        syscall_lib::write_str(
                            STDOUT_FILENO,
                            "display_server: outbound encode failed; dropping event\n",
                        );
                        if reply_cap != 0 {
                            let _ = syscall_lib::ipc_reply(
                                reply_cap,
                                client::LABEL_CLIENT_EVENT_NONE,
                                0,
                            );
                        }
                    }
                },
                None => {
                    if reply_cap != 0 {
                        let _ =
                            syscall_lib::ipc_reply(reply_cap, client::LABEL_CLIENT_EVENT_NONE, 0);
                    }
                }
            }
        }

        let outcome = if had_graphical && !pull_handled {
            let bulk_len = header.data[1] as usize;
            let bulk_slice = if bulk_len <= bulk_buf.len() {
                &bulk_buf[..bulk_len]
            } else {
                &[][..]
            };
            let outcome = dispatch(
                InboundFrame {
                    header,
                    bulk: bulk_slice,
                },
                &mut registry,
            );
            recent_frames.push(&header, bulk_slice, &outcome);
            outcome
        } else {
            client::DispatchOutcome::default()
        };
        if outcome.fatal {
            log_client_protocol_violation(outcome.fatal_reason, &header, &bulk_buf, &recent_frames);
        }
        if outcome.closed {
            // Phase 72b Track K.7 — Goodbye now carries the client's
            // own token, and the dispatcher returns it via
            // `outcome.closed_client_token`. We scope teardown to that
            // client's surfaces only, replacing the Phase 70
            // "preserve everything" workaround. Surfaces that other
            // clients still own keep their entries.
            if let Some(token) = outcome.closed_client_token {
                let destroyed = registry.destroy_client_surfaces(token);
                if !destroyed.is_empty() {
                    syscall_lib::write_str(STDOUT_FILENO, "display_server: client Goodbye token=");
                    write_u32(token);
                    syscall_lib::write_str(STDOUT_FILENO, " destroyed=");
                    write_u32(destroyed.len() as u32);
                    syscall_lib::write_str(STDOUT_FILENO, "\n");
                }
                for sid in &destroyed {
                    let _ = workspace_mgr.remove_surface(*sid);
                    last_tile_dims.remove(sid);
                    if focused == Some(*sid) {
                        focused = None;
                    }
                }
                // Re-publish focus after the teardown so subscribers
                // see the updated state.
                if !destroyed.is_empty() {
                    publish_focus_changed(&mut control_subs, focused, null_subscriber_sender);
                    // Phase 72b — clear stale pixels left by the
                    // disconnecting client. Same rationale as the
                    // `outcome.destroyed` branch above; the Goodbye
                    // path lands here when a client cleanly says
                    // farewell while the destroy branch covers
                    // explicit DestroySurface verbs.
                    compose_ctx.force_full_repaint();
                }
            } else {
                syscall_lib::write_str(
                    STDOUT_FILENO,
                    "display_server: client Goodbye without token (legacy v2 protocol; registry preserved)\n",
                );
            }
        }
        // Phase 72 — keep the workspace manager in sync with surface
        // lifecycle: every newly-rolled Toplevel joins the active
        // workspace; every destroyed surface leaves whichever
        // workspace currently holds it.
        for (sid, role) in outcome.created.iter() {
            if matches!(role, SurfaceRole::Toplevel) {
                workspace_mgr.insert_on_current(*sid);
            }
        }
        for sid in outcome.destroyed.iter() {
            let _ = workspace_mgr.remove_surface(*sid);
            // Phase 72b — drop the surface from the last-tile-dims
            // map so a future surface that lands at the same id does
            // not get spuriously diffed against the dead tile.
            last_tile_dims.remove(sid);
        }
        // Phase 72b — surfaces leaving the active arrangement leave
        // stale pixels in the framebuffer (term being closed, greeter
        // exiting after auth, etc.). Force a full repaint on the next
        // compose pass so the background is cleared and every live
        // surface re-blits cleanly. Cheap (one full FB clear) and
        // confined to the rare destroy path.
        if !outcome.destroyed.is_empty() {
            compose_ctx.force_full_repaint();
        }
        if let Some(focused_id) = focused
            && outcome.destroyed.iter().any(|id| *id == focused_id)
        {
            focused = None;
            publish_focus_changed(&mut control_subs, focused, null_subscriber_sender);
        }
        // Phase 70 — when focus was just dropped (a Toplevel destroy,
        // or any other reset path), re-pick a focus target from the
        // remaining toplevels so the keyboard does not get stuck
        // routing to nothing. Pick the lowest-id remaining toplevel
        // for determinism — `term`'s `SurfaceId(1)` always wins over a
        // PID-seeded DOOM surface, so closing DOOM hands focus back
        // to term automatically.
        //
        // Filter to `SurfaceRole::Toplevel` only: `surface_ids()`
        // returns every registered surface (Cursor, Layer, anything
        // role-less mid-handshake), and routing keyboard events to a
        // non-Toplevel surface would misroute them entirely. PR 179
        // round-3 review fix.
        if focused.is_none() {
            let fallback = registry
                .surface_ids()
                .into_iter()
                .filter(|id| matches!(registry.surface_role(*id), Some(SurfaceRole::Toplevel)))
                .min();
            if let Some(id) = fallback {
                focused = Some(id);
                publish_focus_changed(&mut control_subs, focused, null_subscriber_sender);
            }
        }
        // Phase 70 — auto-focus the most-recently-created Toplevel. The
        // Phase 56 baseline only granted focus when `focused.is_none()`,
        // which meant a newly-mapped Toplevel (DOOM created after term)
        // never received the keyboard focus. With the focus-aware
        // dispatcher routing `KeyEvent`s only to the focused surface,
        // every keystroke went to term while DOOM sat unfocused — the
        // user-visible symptom was "DOOM doesn't see my keys". Moving
        // focus to the new Toplevel matches the focus-on-create
        // convention every floating window manager uses (sway, i3,
        // Wayland weston, Windows). A future tiling-policy phase
        // (Phase 71) can override this with chord-driven focus.
        if let Some((surface_id, _)) = outcome
            .created
            .iter()
            .find(|(_, role)| matches!(role, SurfaceRole::Toplevel))
            && Some(*surface_id) != focused
        {
            focused = Some(*surface_id);
            publish_focus_changed(&mut control_subs, focused, null_subscriber_sender);
        }

        // First-Toplevel-map gate for the input-owner service. Any
        // Toplevel created in this iteration means a real graphical
        // client (`term`, a layer client wrapping a Toplevel, etc.)
        // is up. Once that's true, register the marker exactly once
        // so `stdin_feeder` can transition out of the PS/2-to-TTY
        // bridge. Re-using the graphical endpoint as the registered
        // endpoint is fine here: the marker is only consulted via
        // `ipc_service_exists` (cap-free probe), not used to send
        // IPC traffic, so there's no danger of confusing it with the
        // protocol traffic that flows over `"display"`.
        if !input_owner_registered
            && outcome
                .created
                .iter()
                .any(|(_, role)| matches!(role, SurfaceRole::Toplevel))
        {
            let rc = syscall_lib::ipc_register_service(ep_handle, "display.input-owner");
            if rc != u64::MAX {
                input_owner_registered = true;
                syscall_lib::write_str(
                    STDOUT_FILENO,
                    "display_server: registered as 'display.input-owner' (first Toplevel mapped)\n",
                );
            } else {
                syscall_lib::write_str(
                    STDOUT_FILENO,
                    "display_server: failed to register 'display.input-owner'\n",
                );
            }
        }

        // Track E.4 — diff the current registered surface ids against
        // the previous-iteration snapshot and publish SurfaceCreated /
        // SurfaceDestroyed events to control-socket subscribers. We
        // do this here (rather than in `client::dispatch`) so the
        // existing `DispatchOutcome` shape stays unchanged. The same
        // bound-`prev_surface_ids` snapshot flips to the empty list
        // when the client closes (above).
        let cur_surface_ids = registry.surface_ids();
        publish_surface_lifecycle_deltas(
            &mut control_subs,
            &registry,
            &prev_surface_ids,
            &cur_surface_ids,
        );
        // Watch the outbound queue for `SurfaceConfigured` — that's
        // the post-CreateSurface + SetSurfaceRole sequence emit. The
        // delta path above also catches it (set role makes the id
        // appear in `surface_ids`), but inspecting outbound covers
        // the case where the role was set *before* the dispatcher
        // populated `surface_ids` ordering; both paths converge on
        // the same SurfaceCreated event for any (id, role) pair.
        for msg in outcome.outbound.iter() {
            if let ServerMessage::SurfaceDestroyed { surface_id } = msg {
                publish_surface_destroyed(&mut control_subs, *surface_id, null_subscriber_sender);
            }
        }
        prev_surface_ids = cur_surface_ids;

        // 2. Reply to the caller so any `ipc_call*` request unblocks.
        //    Phase 56 close-out — the kernel populates `header.data[3]`
        //    with the reply-cap handle so userspace doesn't have to
        //    guess. Skip reply when there's no graphical message this
        //    iteration or no cap (fire-and-forget sender). Also skip
        //    when `LABEL_CLIENT_EVENT_PULL` was already handled above —
        //    that verb has its own reply shape and replying here would
        //    panic the kernel on a double-reply.
        if had_graphical && !pull_handled {
            let reply_label = if outcome.fatal { RESP_FATAL } else { RESP_OK };
            let reply_cap = header.data[3] as u32;
            if reply_cap != 0 {
                let _ = syscall_lib::ipc_reply(reply_cap, reply_label, 0);
            }
        }

        // 3. If a frame-tick has elapsed, drive one compose pass. The
        //    pure-logic `compose_frame` already calls
        //    `FramebufferOwner::present()` once at the end iff at least one
        //    write succeeded — no extra `owner.present()` here. Calling it
        //    twice would double-flush on any future backend that uses
        //    `present` as a real swap point (today `KernelFramebufferOwner`
        //    uses the trait's default no-op, so the duplicate was visible
        //    only to a reviewer reading the code).
        // Phase 57d follow-up — Tier 1 fullscreen-takeover gate.
        // While `fb_yielded` is set, the takeover program owns the
        // framebuffer. Skip the entire compose path: don't drain the
        // frame-tick (so the next reclaim sees a clean tick budget),
        // don't run `run_compose` (it would write to FB pages owned by
        // another process), don't emit `compose_micros` samples (those
        // would lie about composer health). The reclaim handler marks
        // every surface dirty so the post-reclaim tick redraws.
        let ticks = if fb_yielded {
            0
        } else {
            syscall_lib::frame_tick_drain()
        };
        if ticks > 0 {
            // Phase 57d follow-up — post-reclaim full-screen fill.
            // After Tier 1 fullscreen-takeover, the takeover program
            // (e.g. doom) drew over the entire FB. `run_compose`
            // below only paints surface and cursor regions; any
            // background gutter would still show doom's last frame.
            // Reset the cursor-damage tracking too so the cursor is
            // redrawn at the current position rather than computing
            // damage against a "previous" pointer that referred to
            // pre-yield FB state.
            if needs_post_reclaim_fill {
                if let Err(_e) = fill_background(&mut owner) {
                    syscall_lib::write_str(
                        STDOUT_FILENO,
                        "display_server: post-reclaim background fill failed\n",
                    );
                }
                compose_ctx = ComposeContext::new();
                needs_post_reclaim_fill = false;
            }

            // E.3 — gate has moved into `run_compose`. The composer
            // checks both `registry.has_damage()` AND pointer-motion
            // damage (via `cursor_damage`); a tick with no surface
            // damage but a moved cursor still composes one frame so
            // the cursor's old position is overpainted and the new
            // one shows up.
            //
            // Track E.4 — wrap the compose call with a monotonic
            // clock read on each side so we can record the
            // composition wall-time into the FrameStatsRing. This is
            // the "Engineering Discipline → Observability" sample the
            // `m3ctl frame-stats` verb returns.
            let start_us = monotonic_micros();
            // Phase 72 — workspace-aware compose. The adapter funnels
            // `LayoutPolicy::arrange` calls into the current
            // workspace's active tiling policy + outer/inner gaps.
            // The `include_toplevel` filter masks Toplevel surfaces
            // belonging to other workspaces; Layer / Cursor surfaces
            // remain visible across all workspaces.
            let active_ws_windows: alloc::vec::Vec<SurfaceId> =
                workspace_mgr.current().windows().to_vec();
            // Phase 72b Track K.3 — diff the current tile arrangement
            // against the last-known dims map and emit `SurfaceResized`
            // for every Toplevel whose dimensions changed. Position
            // changes are silent (the client's buffer keeps its intrinsic
            // size). Active workspace only: surfaces on inactive
            // workspaces don't receive resize events until they come
            // back into view (the next `switch_workspace` will diff and
            // re-emit if their new tile differs).
            let arrangement_for_diff: alloc::vec::Vec<(SurfaceId, Rect)> = workspace_mgr
                .arrange_current(
                    Rect {
                        x: 0,
                        y: 0,
                        w: meta.width,
                        h: meta.height,
                    },
                    compositor_config.gaps,
                );
            // Phase 72b — track whether the arrangement structure
            // changed this frame (any tile dim flipped or any surface
            // left the active set). On a change, the now-uncovered
            // FB regions — inner gaps that shifted, outer-gap edges
            // newly exposed by a shrunken tile, the entire
            // previously-occupied area when a workspace switch swaps
            // surface sets — keep stale pixels from the old frame
            // because compose_frame only writes inside damage rects
            // and the gap areas are in no damage list. Force a full
            // repaint when this happens so the background fill clears
            // the gaps before the surface pass re-blits the live tiles.
            let mut arrangement_changed = false;
            for (sid, rect) in arrangement_for_diff.iter() {
                let entry = last_tile_dims.entry(*sid).or_insert((0, 0));
                if entry.0 != rect.w || entry.1 != rect.h {
                    *entry = (rect.w, rect.h);
                    arrangement_changed = true;
                    if client_event_queue.len() == client::MAX_CLIENT_EVENT_QUEUE {
                        client_event_queue.pop_front();
                    }
                    client_event_queue.push_back((
                        *sid,
                        ServerMessage::SurfaceResized {
                            surface_id: *sid,
                            width: rect.w,
                            height: rect.h,
                        },
                    ));
                }
            }
            // Drop tracked dims for surfaces that left the active
            // arrangement (workspace switch, destroy, move). This keeps
            // the map bounded and prevents stale-comparison artifacts
            // when the surface returns under a different tile size.
            let active_set: alloc::collections::BTreeSet<SurfaceId> =
                arrangement_for_diff.iter().map(|(sid, _)| *sid).collect();
            let prev_tracked = last_tile_dims.len();
            last_tile_dims.retain(|sid, _| active_set.contains(sid));
            if last_tile_dims.len() != prev_tracked {
                arrangement_changed = true;
            }
            if arrangement_changed {
                compose_ctx.force_full_repaint();
            }
            let mut adapter = WorkspaceLayoutAdapter {
                manager: &mut workspace_mgr,
                gaps: compositor_config.gaps,
            };
            let compose_result = run_compose_filtered(
                &mut owner,
                &mut adapter,
                &mut registry,
                &mut compose_ctx,
                pointer_position,
                |id| active_ws_windows.contains(&id),
                Some(compositor_config.borders),
                focused,
            );
            let elapsed_us = monotonic_micros().saturating_sub(start_us);
            let compose_micros = if elapsed_us > u32::MAX as u64 {
                u32::MAX
            } else {
                elapsed_us as u32
            };
            // Phase 57d follow-up — boot-time compose visibility.
            // Log the first 5 entries so we see compose come up and
            // know which arm fired (ok0 / okN / err) on each.  The
            // every-60 steady-state log was removed in the Phase 57e
            // deferral cleanup (2026-05-07) — it generated thousands
            // of identical lines per boot for no diagnostic value
            // once the compose loop's liveness was no longer in
            // question.  The Err path still emits a dedicated log
            // line so a compose failure remains visible.
            let entry_count =
                DIAG_COMPOSES_RUN.fetch_add(1, core::sync::atomic::Ordering::Relaxed) + 1;
            let result_tag: &'static str = match &compose_result {
                Ok(0) => "ok0",
                Ok(_) => "okN",
                Err(_) => "err",
            };
            // Phase 72b — fire `[autostart]` after the first successful
            // compose so launched processes (term, future status bar)
            // have a live compositor + framebuffer to attach to. Latch
            // ensures one-shot semantics across the compositor lifetime.
            if !autostart_ran && compose_result.is_ok() {
                autostart_ran = true;
                run_autostart(&compositor_config.autostart.entries);
            }
            let writes_this = compose_result.as_ref().copied().unwrap_or(0);
            DIAG_FB_WRITES.fetch_add(writes_this as u64, core::sync::atomic::Ordering::Relaxed);
            if entry_count <= 5 {
                let total_writes = DIAG_FB_WRITES.load(core::sync::atomic::Ordering::Relaxed);
                let key_some =
                    input::DIAG_KEY_DRAINS_SOME.load(core::sync::atomic::Ordering::Relaxed);
                let key_none =
                    input::DIAG_KEY_DRAINS_NONE.load(core::sync::atomic::Ordering::Relaxed);
                let ptr_some =
                    input::DIAG_PTR_DRAINS_SOME.load(core::sync::atomic::Ordering::Relaxed);
                let ptr_none =
                    input::DIAG_PTR_DRAINS_NONE.load(core::sync::atomic::Ordering::Relaxed);
                syscall_lib::write_str(STDOUT_FILENO, "display_server: compose#");
                write_u32(entry_count as u32);
                syscall_lib::write_str(STDOUT_FILENO, " ");
                syscall_lib::write_str(STDOUT_FILENO, result_tag);
                syscall_lib::write_str(STDOUT_FILENO, " writes=");
                write_u32(writes_this as u32);
                syscall_lib::write_str(STDOUT_FILENO, " total=");
                write_u32(total_writes as u32);
                syscall_lib::write_str(STDOUT_FILENO, " keys=");
                write_u32(key_some as u32);
                syscall_lib::write_str(STDOUT_FILENO, "/");
                write_u32(key_none as u32);
                syscall_lib::write_str(STDOUT_FILENO, " ptrs=");
                write_u32(ptr_some as u32);
                syscall_lib::write_str(STDOUT_FILENO, "/");
                write_u32(ptr_none as u32);
                syscall_lib::write_str(STDOUT_FILENO, " pos=");
                write_u32(pointer_position.0.max(0) as u32);
                syscall_lib::write_str(STDOUT_FILENO, ",");
                write_u32(pointer_position.1.max(0) as u32);
                let mbytes = syscall_lib::ps2_diag_counter(0);
                let mpackets = syscall_lib::ps2_diag_counter(1);
                let mdrops = syscall_lib::ps2_diag_counter(2);
                let irq1 = syscall_lib::ps2_diag_counter(3);
                let irq12 = syscall_lib::ps2_diag_counter(4);
                syscall_lib::write_str(STDOUT_FILENO, " irq1=");
                write_u32(irq1 as u32);
                syscall_lib::write_str(STDOUT_FILENO, " irq12=");
                write_u32(irq12 as u32);
                syscall_lib::write_str(STDOUT_FILENO, " mbytes=");
                write_u32(mbytes as u32);
                syscall_lib::write_str(STDOUT_FILENO, " mpkts=");
                write_u32(mpackets as u32);
                syscall_lib::write_str(STDOUT_FILENO, " mdrops=");
                write_u32(mdrops as u32);
                syscall_lib::write_str(STDOUT_FILENO, "\n");
            }
            match compose_result {
                Ok(0) => {}
                Ok(_writes) => {
                    record_frame_sample(&mut frame_stats, frame_index_counter, compose_micros);
                    frame_index_counter = frame_index_counter.saturating_add(1);
                }
                Err(_) => {
                    syscall_lib::write_str(STDOUT_FILENO, "display_server: compose failed\n");
                }
            }
        }

        // 4. Drain input services (D.3). The dispatcher routes every
        //    drained event by current focus + bind-table + grab-state
        //    policy and emits `InputEffect`s the shim translates here:
        //      * `Outbound(ServerMessage::Key/Pointer)` → log for
        //        diagnostic visibility (the per-client send-cap
        //        channel is C.5 follow-up work; for now pushing onto
        //        an internal queue without a wire would just
        //        accumulate).
        //      * `BindTriggered { id }` → log; control-socket E.4
        //        will emit `BindTriggered` once that landing wires
        //        up.
        //      * `FocusChanged(id)` → update the local focus tracker.
        //      * `PointerEnter` / `PointerLeave` → log only; protocol
        //        does not yet carry hover events.
        //
        //    Surface geometry comes from the registry's compose plan.
        //    A surface that left the registry between two drains is
        //    invisible to the dispatcher next pass — the proptest
        //    invariant enforces no destroyed-surface delivery, but if
        //    the dispatcher's `hovered` still points at it, the
        //    `forget_hovered` path resets the tracker.
        let output_rect = Rect {
            x: 0,
            y: 0,
            w: meta.width,
            h: meta.height,
        };
        let compose_entries = registry.iter_compose(output_rect);
        let surface_geom: alloc::vec::Vec<SurfaceGeometry> = compose_entries
            .iter()
            .map(|e| SurfaceGeometry::toplevel(e.id, e.rect))
            .collect();
        // Reset hover tracking if the previously hovered surface is no
        // longer in the registry. The dispatcher cannot know this on
        // its own — the registry is the source of truth.
        if let Some(hov) = input_wiring.dispatcher.hovered()
            && !surface_geom.iter().any(|g| g.id == hov)
        {
            input_wiring.dispatcher.forget_hovered();
        }
        // Phase 72 — feed the active chord table from the bind stack
        // (default or transient resize mode). The legacy `bind_table`
        // is still mutated by the control-socket `RegisterBind`
        // verb so existing tests keep working, but Phase 72's chord
        // dispatch consumes the stack's active table directly: that
        // table is the union of the built-in chord set plus any
        // user-supplied chords parsed from `/etc/compositor.conf`.
        let _ = &bind_table; // legacy: still mutated by control socket
        let effects = input_wiring.drain_one_pass(
            focused,
            None, // active_exclusive_layer — E.2 wires this once Layer surfaces map
            pointer_position,
            &surface_geom,
            bind_stack.active_table(),
            &mut grab_state,
        );
        for effect in effects {
            match effect {
                InputEffect::Outbound(target, msg) => {
                    // E.3 seam: extract the pointer's `abs_position`
                    // from any `Pointer` message the dispatcher
                    // emitted, and forward it to the next compose
                    // call's cursor blit.
                    if let kernel_core::display::protocol::ServerMessage::Pointer(ev) = msg
                        && let Some(abs) = ev.abs_position
                    {
                        pointer_position = abs;
                    }
                    // Phase 56 C.5 close-out — push the dispatcher's
                    // `Outbound` message onto the per-client queue.
                    // The client drains it via `LABEL_CLIENT_EVENT_PULL`
                    // (handled at the top of the loop). When the queue
                    // is at the documented cap, drop the OLDEST event
                    // and emit a structured log line — preserving
                    // recent input is more useful than blocking forever
                    // because a client stopped pulling.
                    //
                    // Phase 70 — the target id is preserved alongside
                    // the message so the PULL handler can return only
                    // events the calling client owns.
                    if client_event_queue.len() >= client::MAX_CLIENT_EVENT_QUEUE {
                        client_event_queue.pop_front();
                        syscall_lib::write_str(
                            STDOUT_FILENO,
                            "display_server: outbound queue full; oldest dropped\n",
                        );
                    }
                    client_event_queue.push_back((target, msg));
                }
                InputEffect::BindTriggered { id } => {
                    syscall_lib::write_str(STDOUT_FILENO, "display_server: bind triggered id=");
                    write_u32(id);
                    syscall_lib::write_str(STDOUT_FILENO, "\n");
                    publish_bind_triggered(&mut control_subs, 0, id, null_subscriber_sender);
                    // Phase 72 — translate the raw `BindId` into a
                    // typed `KeybindAction` and execute it. Legacy
                    // ids registered by the control-socket
                    // `RegisterBind` verb (which writes to
                    // `bind_table`) have no action map entry; they
                    // surface only as the `BindTriggered` event for
                    // m3ctl subscribers and are otherwise ignored.
                    if let Some(action) = bind_stack.lookup_action_raw(id) {
                        dispatch_keybind_action(
                            action,
                            &mut workspace_mgr,
                            &mut bind_stack,
                            &mut focused,
                            &mut control_subs,
                            &compositor_config,
                            &mut client_event_queue,
                            &mut compose_ctx,
                        );
                    }
                }
                InputEffect::FocusChanged(id) => {
                    let prev = focused;
                    focused = Some(id);
                    if prev != focused {
                        publish_focus_changed(&mut control_subs, focused, null_subscriber_sender);
                        // Phase 72 review-resolution — borders use the
                        // focused id to pick active vs inactive colour
                        // every compose pass. Without a forced repaint
                        // here the no-damage short-circuit can keep the
                        // previous border colours on screen until some
                        // unrelated surface damages. Focus changes are
                        // user-initiated and infrequent, so paying one
                        // full repaint per focus transition is cheap.
                        // Also invalidates the cache so `focus_affects_
                        // geometry()` layouts (tabbed, fullscreen) pick
                        // the new visible surface on the next pass.
                        compose_ctx.force_full_repaint();
                        compose_ctx.invalidate_arrangement_cache();
                    }
                }
                InputEffect::PointerEnter(_id) | InputEffect::PointerLeave(_id) => {
                    // Phase 56 protocol does not yet carry hover events;
                    // log nothing here to keep the boot serial output
                    // quiet during normal operation.
                }
                InputEffect::CursorMoved(abs) => {
                    // Always-fired by the wiring's pointer drain; carries
                    // the compositor-maintained absolute position after
                    // integrating PS/2 dx/dy. The next compose pass picks
                    // this up via `cursor_motion` damage and re-blits the
                    // cursor at the new spot, including when the pointer
                    // is over no mapped surface (the Outbound branch only
                    // fires when a surface is under the cursor).
                    pointer_position = abs;
                }
            }
        }

        // Track E.4 — service one pending control-endpoint message
        // per iteration if any has arrived. Phase 56 close-out wires
        // the `SYS_IPC_TRY_RECV_MSG` non-blocking recv (kernel syscall
        // 0x1113) so the main loop can multiplex frame-tick driving
        // and control-endpoint serving without blocking.
        serve_one_control_request(
            ctl_ep_handle,
            &mut registry,
            &mut bind_table,
            &mut control_subs,
            &frame_stats,
            debug_crash,
            readback,
            inject_key_policy,
            &owner,
            &mut input_wiring,
            &mut fb_yielded,
            &mut needs_post_reclaim_fill,
            &mut workspace_mgr,
            &mut bind_stack,
            &mut focused,
            &mut compositor_config,
            &mut compose_ctx,
        );

        // Yield briefly when the iteration had no graphical traffic so
        // we don't busy-spin. 1 ms ≈ 1000 polls/sec — well below the
        // 60 Hz frame-tick rate but with enough headroom for control-
        // socket interactive latency.
        if !had_graphical {
            let _ = syscall_lib::nanosleep_for(0, 1_000_000);
        }
    }
}

/// Phase 56 close-out — try-recv one control-endpoint message and serve
/// it. Returns immediately if no client is waiting (so the main loop
/// stays responsive to frame ticks).
///
/// On a pending request:
/// 1. `ipc_try_recv_msg` drains the request label + bulk into local
///    buffers.
/// 2. `serve_control_iter` decodes the `ControlCommand`, dispatches
///    against compositor state, encodes the `ControlEvent` reply.
/// 3. `ipc_store_reply_bulk` stages the reply bytes.
/// 4. `ipc_reply` with `LABEL_CTL_REPLY` wakes the caller; the kernel
///    transfers the staged bulk to the caller's `pending_bulk` slot,
///    where `m3ctl` drains it via `ipc_take_pending_bulk`.
///
/// On any decode / dispatch error a structured `ControlEvent::Error`
/// reply is sent so clients always observe a well-formed frame.
#[allow(clippy::too_many_arguments)]
fn serve_one_control_request(
    ep_handle: u32,
    registry: &mut SurfaceRegistry,
    bind_table: &mut BindTable,
    subscriptions: &mut control::ControlSubscriptions,
    frame_stats: &FrameStatsRing,
    debug_crash: DebugCrashPolicy,
    readback: control::ReadBackPolicy,
    inject_key_policy: control::InjectKeyPolicy,
    fb_owner: &KernelFramebufferOwner,
    input_wiring: &mut InputWiring,
    fb_yielded: &mut bool,
    needs_post_reclaim_fill: &mut bool,
    workspace_mgr: &mut workspace::WorkspaceManager,
    bind_stack: &mut keybind::BindStack,
    focused: &mut Option<SurfaceId>,
    compositor_config: &mut config::CompositorConfig,
    compose_ctx: &mut compose::ComposeContext,
) {
    let mut header = syscall_lib::IpcMessage::new(0);
    let mut req_buf = [0u8; control::MAX_BULK_BYTES];
    let label = syscall_lib::ipc_try_recv_msg(ep_handle, &mut header, &mut req_buf);
    if label == u64::MAX {
        // No pending request OR transport error. Either way: skip
        // this iteration; the next frame-tick poll will retry.
        return;
    }
    // Phase 56 close-out — the kernel writes the reply-cap handle into
    // `header.data[3]`. Use it directly instead of guessing.
    let reply_cap = header.data[3] as u32;
    if label != control::LABEL_CTL_CMD {
        // Unknown label. Stage an error reply so the client can
        // observe the protocol violation.
        let mut reply_buf = [0u8; control::MAX_BULK_BYTES];
        let n = encode_event_or_drop(
            &kernel_core::display::control::ControlEvent::Error {
                code: kernel_core::display::control::ControlErrorCode::UnknownVerb,
            },
            &mut reply_buf,
        );
        if n > 0 {
            let _ = syscall_lib::ipc_store_reply_bulk(&reply_buf[..n]);
        }
        if reply_cap != 0 {
            let _ = syscall_lib::ipc_reply(reply_cap, control::LABEL_CTL_REPLY, 0);
        }
        return;
    }

    // Bulk size lives in the message's data[1] (set by ipc_send_with_bulk).
    let bulk_len = header.data[1] as usize;
    let bulk_len = bulk_len.min(req_buf.len());

    let mut reply_buf = [0u8; control::MAX_BULK_BYTES];

    // Phase 57d follow-up — Tier 1 fullscreen-takeover hooks. Peek at
    // the decoded command for `YieldFb` / `ReclaimFb` and route them
    // through `handle_fb_yield_request` / `handle_fb_reclaim_request`
    // directly. These have side effects (a kernel syscall plus
    // compose-loop state changes) the pure-logic `dispatch_command`
    // shouldn't carry, and the rest of the control surface is unaware
    // of the yielded state.
    use kernel_core::display::control::{ControlCommand, decode_command};
    let n = if let Ok((cmd, _)) = decode_command(&req_buf[..bulk_len]) {
        match cmd {
            ControlCommand::YieldFb => handle_fb_yield_request(fb_yielded, &mut reply_buf),
            ControlCommand::ReclaimFb => handle_fb_reclaim_request(
                registry,
                fb_yielded,
                needs_post_reclaim_fill,
                &mut reply_buf,
            ),
            ControlCommand::SetLayout { kind } => {
                // The policy state changed under the same id set; without
                // these two calls the arrangement cache and the no-damage
                // short-circuit would keep painting the previous policy's
                // partition until something else damaged the framebuffer.
                let n = handle_set_layout(workspace_mgr, kind, &mut reply_buf);
                compose_ctx.invalidate_arrangement_cache();
                compose_ctx.force_full_repaint();
                n
            }
            ControlCommand::SwitchWorkspace { n } => {
                let nbytes = handle_switch_workspace(
                    workspace_mgr,
                    focused,
                    subscriptions,
                    n,
                    &mut reply_buf,
                );
                // Workspace switch usually changes the active id set (so
                // the compose cache invalidates naturally), but the focus
                // and active-set transition still needs the no-damage
                // gate bypassed so borders + background recolour on the
                // next pass.
                compose_ctx.invalidate_arrangement_cache();
                compose_ctx.force_full_repaint();
                nbytes
            }
            ControlCommand::MoveToWorkspace { n, follow } => {
                let nbytes = handle_move_to_workspace(
                    workspace_mgr,
                    focused,
                    subscriptions,
                    n,
                    follow,
                    &mut reply_buf,
                );
                compose_ctx.invalidate_arrangement_cache();
                compose_ctx.force_full_repaint();
                nbytes
            }
            ControlCommand::Reload => {
                let n = handle_reload(workspace_mgr, bind_stack, compositor_config, &mut reply_buf);
                // Gaps / borders / per-workspace policy may all have
                // changed; recompute arrangement and repaint.
                compose_ctx.invalidate_arrangement_cache();
                compose_ctx.force_full_repaint();
                n
            }
            ControlCommand::QueryWindows => {
                let meta = fb_owner.metadata();
                handle_query_windows(
                    workspace_mgr,
                    *focused,
                    compositor_config.gaps,
                    meta.width,
                    meta.height,
                    &mut reply_buf,
                )
            }
            ControlCommand::QueryWorkspaces => {
                handle_query_workspaces(workspace_mgr, &mut reply_buf)
            }
            ControlCommand::SetMasterRatio { ratio_x100 } => {
                let n = handle_set_master_ratio(workspace_mgr, ratio_x100, &mut reply_buf);
                compose_ctx.invalidate_arrangement_cache();
                compose_ctx.force_full_repaint();
                n
            }
            ControlCommand::TileFullscreen => {
                let n = handle_tile_fullscreen(workspace_mgr, &mut reply_buf);
                compose_ctx.invalidate_arrangement_cache();
                compose_ctx.force_full_repaint();
                n
            }
            _ => {
                let client = control::ClientId(0);
                let pixel_reader =
                    |x: u32, y: u32| -> Option<u32> { fb_owner.read_pixel(x, y).ok() };
                let inject_key_sink =
                    |ev: kernel_core::input::events::KeyEvent| input_wiring.inject_key(ev);
                serve_control_iter(
                    &req_buf[..bulk_len],
                    client,
                    registry,
                    bind_table,
                    subscriptions,
                    frame_stats,
                    debug_crash,
                    readback,
                    inject_key_policy,
                    pixel_reader,
                    inject_key_sink,
                    &mut reply_buf,
                )
            }
        }
    } else {
        // Fall through to generic decoder so its error reply path stays
        // the single source of truth for malformed frames.
        let client = control::ClientId(0);
        let pixel_reader = |x: u32, y: u32| -> Option<u32> { fb_owner.read_pixel(x, y).ok() };
        let inject_key_sink =
            |ev: kernel_core::input::events::KeyEvent| input_wiring.inject_key(ev);
        serve_control_iter(
            &req_buf[..bulk_len],
            client,
            registry,
            bind_table,
            subscriptions,
            frame_stats,
            debug_crash,
            readback,
            inject_key_policy,
            pixel_reader,
            inject_key_sink,
            &mut reply_buf,
        )
    };
    if n > 0 {
        let _ = syscall_lib::ipc_store_reply_bulk(&reply_buf[..n]);
    }
    if reply_cap != 0 {
        let _ = syscall_lib::ipc_reply(reply_cap, control::LABEL_CTL_REPLY, 0);
    }
}

/// Phase 56 Track E.4 — single-iteration control-endpoint dispatch
/// helper. Decodes one `ControlCommand` from `bulk`, invokes the
/// dispatcher, and stages the encoded `ControlEvent` reply onto the
/// reply-bulk slot.
///
/// Returns `Ok(reply_bytes)` for the count of bytes staged; the caller
/// is responsible for the final `ipc_reply` with `LABEL_CTL_REPLY`.
/// On any codec or dispatch error, the helper still produces an
/// encoded `Error` event so the client always receives a reply.
///
/// The Phase 56 close-out wires this from `serve_one_control_request`
/// using the new `SYS_IPC_TRY_RECV_MSG` syscall to multiplex frame-tick
/// driving and control-endpoint serving in the same single-threaded loop.
fn serve_control_iter<F, I>(
    bulk: &[u8],
    client: control::ClientId,
    registry: &SurfaceRegistry,
    bind_table: &mut BindTable,
    subscriptions: &mut control::ControlSubscriptions,
    frame_stats: &FrameStatsRing,
    debug_crash: DebugCrashPolicy,
    readback: control::ReadBackPolicy,
    inject_key_policy: control::InjectKeyPolicy,
    pixel_reader: F,
    inject_key_sink: I,
    reply_buf: &mut [u8],
) -> usize
where
    F: FnOnce(u32, u32) -> Option<u32>,
    I: FnOnce(kernel_core::input::events::KeyEvent),
{
    use kernel_core::display::control::{
        ControlError, ControlErrorCode, ControlEvent, decode_command,
    };

    // Decode → dispatch. Any decode error is converted to an `Error`
    // event so the wire is always a valid frame.
    let cmd = match decode_command(bulk) {
        Ok((c, _)) => c,
        Err(ControlError::UnknownVerb { .. }) => {
            return encode_event_or_drop(
                &ControlEvent::Error {
                    code: ControlErrorCode::UnknownVerb,
                },
                reply_buf,
            );
        }
        Err(ControlError::MalformedFrame) => {
            return encode_event_or_drop(
                &ControlEvent::Error {
                    code: ControlErrorCode::MalformedFrame,
                },
                reply_buf,
            );
        }
        Err(ControlError::BadArgs { .. }) => {
            return encode_event_or_drop(
                &ControlEvent::Error {
                    code: ControlErrorCode::BadArgs,
                },
                reply_buf,
            );
        }
        Err(_) => {
            return encode_event_or_drop(
                &ControlEvent::Error {
                    code: ControlErrorCode::MalformedFrame,
                },
                reply_buf,
            );
        }
    };

    match control::dispatch_command(
        &cmd,
        client,
        registry,
        bind_table,
        subscriptions,
        frame_stats,
        debug_crash,
        readback,
        inject_key_policy,
        pixel_reader,
        inject_key_sink,
        reply_buf,
    ) {
        Ok(Some(n)) => n,
        Ok(None) => 0,
        Err(_) => encode_event_or_drop(
            &ControlEvent::Error {
                code: ControlErrorCode::MalformedFrame,
            },
            reply_buf,
        ),
    }
}

/// Best-effort encode of a `ControlEvent`. Returns the byte count on
/// success, or `0` if even the error event won't fit in `reply_buf`.
/// `0` lets the caller send a label-only reply so the client at
/// least observes a roundtrip.
#[allow(dead_code)]
fn encode_event_or_drop(
    evt: &kernel_core::display::control::ControlEvent,
    reply_buf: &mut [u8],
) -> usize {
    kernel_core::display::control::encode_event(evt, reply_buf).unwrap_or_default()
}

/// Phase 57d follow-up — Tier 1 fullscreen-takeover yield handler.
///
/// Drops framebuffer ownership via `SYS_FB_YIELD` so a fullscreen
/// program (e.g. doom launched via `/bin/fb-takeover`) can claim it.
/// Sets `fb_yielded = true` so the compose loop skips its frame-tick
/// work — without this gate the compositor would keep writing pixels
/// while another process draws, racing on the same physical FB pages.
/// Idempotent: a second yield while already yielded is a no-op
/// success.
fn handle_fb_yield_request(fb_yielded: &mut bool, reply_buf: &mut [u8]) -> usize {
    use kernel_core::display::control::{ControlErrorCode, ControlEvent};
    if *fb_yielded {
        return encode_event_or_drop(&ControlEvent::Ack, reply_buf);
    }
    let rc = syscall_lib::fb_yield();
    if rc != 0 {
        syscall_lib::write_str(STDOUT_FILENO, "display_server: fb_yield syscall failed\n");
        return encode_event_or_drop(
            &ControlEvent::Error {
                code: ControlErrorCode::ResourceExhausted,
            },
            reply_buf,
        );
    }
    *fb_yielded = true;
    syscall_lib::write_str(
        STDOUT_FILENO,
        "display_server: framebuffer yielded for takeover\n",
    );
    encode_event_or_drop(&ControlEvent::Ack, reply_buf)
}

/// Phase 57d follow-up — Tier 1 fullscreen-takeover reclaim handler.
///
/// Counterpart to [`handle_fb_yield_request`]. Re-acquires framebuffer
/// ownership via `SYS_FB_REACQUIRE` and clears the yielded gate. If
/// the takeover program exited normally the kernel's exit cleanup
/// already cleared the FB owner; if `FB_REACQUIRE` returns `EBUSY`
/// the reclaimer will retry shortly (the wrapper retries the verb on
/// `Internal`).
///
/// On success, marks every live surface dirty so the next compose
/// pass repaints the screen — between yield and reclaim the takeover
/// program drew over the FB and any cached compose state is now
/// stale. Also sets `needs_post_reclaim_fill` so the next compose
/// tick `fill_background`s the entire FB before running compose:
/// surface blits only paint surface and cursor regions, leaving any
/// background gutter (where doom drew but our toplevels don't cover)
/// showing whatever the takeover program left behind.
fn handle_fb_reclaim_request(
    registry: &mut SurfaceRegistry,
    fb_yielded: &mut bool,
    needs_post_reclaim_fill: &mut bool,
    reply_buf: &mut [u8],
) -> usize {
    use kernel_core::display::control::{ControlErrorCode, ControlEvent};
    if !*fb_yielded {
        return encode_event_or_drop(&ControlEvent::Ack, reply_buf);
    }
    let rc = syscall_lib::fb_reacquire();
    if rc != 0 {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "display_server: fb_reacquire syscall failed\n",
        );
        return encode_event_or_drop(
            &ControlEvent::Error {
                code: ControlErrorCode::ResourceExhausted,
            },
            reply_buf,
        );
    }
    registry.mark_all_dirty();
    *fb_yielded = false;
    *needs_post_reclaim_fill = true;
    syscall_lib::write_str(
        STDOUT_FILENO,
        "display_server: framebuffer reclaimed; full repaint queued\n",
    );
    encode_event_or_drop(&ControlEvent::Ack, reply_buf)
}

/// Try to acquire the framebuffer with bounded retry, in case another
/// short-lived process is still releasing ownership at boot.
fn acquire_framebuffer_with_backoff() -> Result<KernelFramebufferOwner, &'static str> {
    const MAX_ATTEMPTS: u32 = 8;
    const BACKOFF_NS: u32 = 5_000_000; // 5 ms

    for attempt in 0..MAX_ATTEMPTS {
        match KernelFramebufferOwner::acquire() {
            Ok(o) => return Ok(o),
            Err(fb::AcquireError::FbBusy) => {
                if attempt + 1 == MAX_ATTEMPTS {
                    return Err("framebuffer busy after retry budget");
                }
                syscall_lib::nanosleep_for(0, BACKOFF_NS);
            }
            Err(fb::AcquireError::FbInfoFailed) => return Err("FB info syscall failed"),
            Err(fb::AcquireError::FbMmapFailed) => return Err("FB mmap syscall failed"),
            Err(fb::AcquireError::UnsupportedPixelFormat) => {
                return Err("FB pixel format not supported");
            }
        }
    }
    Err("framebuffer busy after retry budget")
}

fn log_fb_meta(w: u32, h: u32, stride: u32) {
    syscall_lib::write_str(STDOUT_FILENO, "display_server: fb metadata: ");
    write_u32(w);
    syscall_lib::write_str(STDOUT_FILENO, "x");
    write_u32(h);
    syscall_lib::write_str(STDOUT_FILENO, " stride=");
    write_u32(stride);
    syscall_lib::write_str(STDOUT_FILENO, "\n");
}

fn log_client_protocol_violation(
    reason: Option<FatalReason>,
    header: &IpcMessage,
    bulk_buf: &[u8],
    recent: &RecentFrames,
) {
    if DIAG_PROTOCOL_VIOLATION_LOG_BUDGET
        .fetch_update(
            core::sync::atomic::Ordering::Relaxed,
            core::sync::atomic::Ordering::Relaxed,
            |remaining| remaining.checked_sub(1),
        )
        .is_err()
    {
        return;
    }
    let bulk_len = (header.data[1] as usize).min(bulk_buf.len());
    syscall_lib::write_str(
        STDOUT_FILENO,
        "display_server: client protocol violation reason=",
    );
    syscall_lib::write_str(STDOUT_FILENO, fatal_reason_name(reason));
    syscall_lib::write_str(STDOUT_FILENO, " label=");
    write_u32(header.label as u32);
    syscall_lib::write_str(STDOUT_FILENO, " bulk_len=");
    write_u32(bulk_len as u32);
    if bulk_len >= 4 {
        let body_len = u16::from_le_bytes([bulk_buf[0], bulk_buf[1]]);
        let opcode = u16::from_le_bytes([bulk_buf[2], bulk_buf[3]]);
        syscall_lib::write_str(STDOUT_FILENO, " body_len=");
        write_u32(body_len as u32);
        syscall_lib::write_str(STDOUT_FILENO, " opcode=");
        write_u32(opcode as u32);
    }
    syscall_lib::write_str(STDOUT_FILENO, " head16=");
    write_hex_prefix(bulk_buf, bulk_len, 16);
    syscall_lib::write_str(STDOUT_FILENO, "\n");
    recent.dump();
}

/// Phase 57d follow-up — small ring of recently dispatched verb frames.
/// Captures `(label, bulk_len, head8 bytes, outcome tag)` so a fatal
/// dispatch can show whether the offending bulk's bytes match a frame
/// the dispatcher accepted moments earlier (which would point at a
/// stale-bulk read on the receiver side, rather than a malformed frame
/// from the client).
const RECENT_FRAMES_CAP: usize = 8;

#[derive(Clone, Copy)]
struct RecentFrame {
    label: u64,
    bulk_len: u32,
    head: [u8; 8],
    head_len: u8,
    outcome: RecentOutcome,
}

#[derive(Clone, Copy)]
enum RecentOutcome {
    Ok,
    Closed,
    Fatal(Option<FatalReason>),
}

struct RecentFrames {
    entries: [Option<RecentFrame>; RECENT_FRAMES_CAP],
    next: usize,
}

impl RecentFrames {
    fn new() -> Self {
        Self {
            entries: [None; RECENT_FRAMES_CAP],
            next: 0,
        }
    }

    fn push(&mut self, header: &IpcMessage, bulk: &[u8], outcome: &client::DispatchOutcome) {
        let mut head = [0u8; 8];
        let head_len = bulk.len().min(head.len());
        head[..head_len].copy_from_slice(&bulk[..head_len]);
        let outcome_tag = if outcome.fatal {
            RecentOutcome::Fatal(outcome.fatal_reason)
        } else if outcome.closed {
            RecentOutcome::Closed
        } else {
            RecentOutcome::Ok
        };
        let entry = RecentFrame {
            label: header.label,
            bulk_len: bulk.len() as u32,
            head,
            head_len: head_len as u8,
            outcome: outcome_tag,
        };
        self.entries[self.next] = Some(entry);
        self.next = (self.next + 1) % RECENT_FRAMES_CAP;
    }

    /// Print the ring oldest-first with the most recent entry last so
    /// scanners see the failing frame at the bottom and its predecessor
    /// just above it.
    fn dump(&self) {
        let mut age = 0;
        for offset in 0..RECENT_FRAMES_CAP {
            let idx = (self.next + offset) % RECENT_FRAMES_CAP;
            if let Some(entry) = &self.entries[idx] {
                syscall_lib::write_str(STDOUT_FILENO, "display_server:   recent[-");
                write_u32((RECENT_FRAMES_CAP - 1 - age) as u32);
                syscall_lib::write_str(STDOUT_FILENO, "] label=");
                write_u32(entry.label as u32);
                syscall_lib::write_str(STDOUT_FILENO, " bulk_len=");
                write_u32(entry.bulk_len);
                syscall_lib::write_str(STDOUT_FILENO, " head=");
                write_hex_prefix(
                    &entry.head[..entry.head_len as usize],
                    entry.head_len as usize,
                    entry.head_len as usize,
                );
                syscall_lib::write_str(STDOUT_FILENO, " outcome=");
                syscall_lib::write_str(STDOUT_FILENO, recent_outcome_name(entry.outcome));
                syscall_lib::write_str(STDOUT_FILENO, "\n");
            }
            age += 1;
        }
    }
}

fn recent_outcome_name(outcome: RecentOutcome) -> &'static str {
    match outcome {
        RecentOutcome::Ok => "ok",
        RecentOutcome::Closed => "closed",
        RecentOutcome::Fatal(reason) => fatal_reason_name(reason),
    }
}

/// Print up to `max` bytes of `buf[..len]` as lowercase hex without
/// separators. `len` is capped at `buf.len()` defensively. Designed for
/// terse single-line log output — 16 bytes prints as 32 hex chars.
fn write_hex_prefix(buf: &[u8], len: usize, max: usize) {
    let n = len.min(buf.len()).min(max);
    let mut out = [0u8; 2];
    for byte in &buf[..n] {
        out[0] = hex_nibble(byte >> 4);
        out[1] = hex_nibble(byte & 0x0F);
        if let Ok(s) = core::str::from_utf8(&out) {
            syscall_lib::write_str(STDOUT_FILENO, s);
        }
    }
}

fn hex_nibble(v: u8) -> u8 {
    match v {
        0..=9 => b'0' + v,
        10..=15 => b'a' + (v - 10),
        _ => b'?',
    }
}

fn fatal_reason_name(reason: Option<FatalReason>) -> &'static str {
    match reason {
        Some(FatalReason::BulkTooLarge) => "bulk-too-large",
        Some(FatalReason::PixelHeaderTooShort) => "pixel-header-too-short",
        Some(FatalReason::PixelSizeMismatch) => "pixel-size-mismatch",
        Some(FatalReason::PendingBulkFull) => "pending-bulk-full",
        Some(FatalReason::ChunkHeaderTooShort) => "chunk-header-too-short",
        Some(FatalReason::ChunkDecode) => "chunk-decode",
        Some(FatalReason::ChunkBufferMismatch) => "chunk-buffer-mismatch",
        Some(FatalReason::ChunkReceive) => "chunk-receive",
        Some(FatalReason::VerbDecode) => "verb-decode",
        Some(FatalReason::ShmMapFailed) => "shm-map-failed",
        None => "unknown",
    }
}

fn write_u32(mut value: u32) {
    let mut buf = [0u8; 10];
    let mut idx = buf.len();
    if value == 0 {
        idx -= 1;
        buf[idx] = b'0';
    } else {
        while value != 0 {
            idx -= 1;
            buf[idx] = b'0' + (value % 10) as u8;
            value /= 10;
        }
    }
    if let Ok(s) = core::str::from_utf8(&buf[idx..]) {
        syscall_lib::write_str(STDOUT_FILENO, s);
    }
}

/// Map the kernel's reported pixel-format tag onto
/// `kernel-core::display::fb_owner::PixelFormat`.
pub(crate) fn pixel_format_from_kernel_tag(
    tag: u32,
) -> Option<kernel_core::display::fb_owner::PixelFormat> {
    use kernel_core::display::fb_owner::PixelFormat;
    match tag {
        0 => Some(PixelFormat::Rgba8888), // bootloader_api::PixelFormat::Rgb
        1 => Some(PixelFormat::Bgra8888), // bootloader_api::PixelFormat::Bgr
        _ => None,
    }
}

/// Read the monotonic clock and return the time as microseconds. Used by
/// the Track E.4 frame-stats wrapper around `run_compose`. Saturates
/// rather than panicking on overflow or syscall error so the compose
/// path stays panic-free.
fn monotonic_micros() -> u64 {
    let (sec, nsec) = syscall_lib::clock_gettime(syscall_lib::CLOCK_MONOTONIC);
    if sec < 0 {
        return 0;
    }
    let sec_us = (sec as u64).saturating_mul(1_000_000);
    let nsec_us = (nsec as u64) / 1_000;
    sec_us.saturating_add(nsec_us)
}

/// Phase 56 Track E.4 — diff the previous and current snapshot of
/// registered surface ids and publish `SurfaceCreated` /
/// `SurfaceDestroyed` events on the control-socket subscription
/// registry for every entry that changed.
///
/// Both snapshots are sorted ascending (the registry is a `BTreeMap`),
/// so the diff is a linear two-pointer walk. The function looks up
/// the role from the registry for any newly-appearing id; a
/// destroy-then-recreate within the same iteration is impossible
/// because the dispatcher processes one IPC message per loop pass.
fn publish_surface_lifecycle_deltas(
    subs: &mut crate::control::ControlSubscriptions,
    registry: &SurfaceRegistry,
    prev: &[SurfaceId],
    cur: &[SurfaceId],
) {
    let mut i = 0usize;
    let mut j = 0usize;
    while i < prev.len() && j < cur.len() {
        let p = prev[i];
        let c = cur[j];
        if p == c {
            i += 1;
            j += 1;
        } else if p.0 < c.0 {
            // `p` was destroyed.
            publish_surface_destroyed(subs, p, null_subscriber_sender);
            i += 1;
        } else {
            // `c` is new.
            publish_surface_created(subs, registry, c, null_subscriber_sender);
            j += 1;
        }
    }
    while i < prev.len() {
        publish_surface_destroyed(subs, prev[i], null_subscriber_sender);
        i += 1;
    }
    while j < cur.len() {
        publish_surface_created(subs, registry, cur[j], null_subscriber_sender);
        j += 1;
    }
}

/// Phase 72 — load `/etc/compositor.conf` from the ext2 data disk.
/// Returns `Some(cfg)` on a clean parse; logs a one-liner and returns
/// `None` on any read or parse failure (the caller falls back to
/// `CompositorConfig::defaults()`).
fn load_compositor_config(path: &str) -> Option<config::CompositorConfig> {
    use alloc::string::String;
    use alloc::vec::Vec;
    let mut path_buf = alloc::vec::Vec::with_capacity(path.len() + 1);
    path_buf.extend_from_slice(path.as_bytes());
    path_buf.push(0);
    let fd = syscall_lib::open(&path_buf, 0, 0);
    if fd < 0 {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "display_server: compositor.conf not found; using defaults\n",
        );
        return None;
    }
    let fd_i32 = fd as i32;
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let mut chunk = [0u8; 1024];
    loop {
        let n = syscall_lib::read(fd_i32, &mut chunk);
        if n <= 0 {
            break;
        }
        let n_usize = n as usize;
        buf.extend_from_slice(&chunk[..n_usize]);
        if n_usize < chunk.len() {
            break;
        }
    }
    let _ = syscall_lib::close(fd_i32);
    let text = match core::str::from_utf8(&buf) {
        Ok(s) => String::from(s),
        Err(_) => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "display_server: compositor.conf not utf-8; using defaults\n",
            );
            return None;
        }
    };
    match config::CompositorConfig::parse_with_warnings(&text) {
        Ok((cfg, warnings)) => {
            for w in &warnings {
                match w {
                    config::ConfigWarning::UnknownSection { .. } => {
                        syscall_lib::write_str(
                            STDOUT_FILENO,
                            "display_server: compositor.conf: ignoring unknown section\n",
                        );
                    }
                    config::ConfigWarning::UnknownKey { .. } => {
                        syscall_lib::write_str(
                            STDOUT_FILENO,
                            "display_server: compositor.conf: ignoring unknown key\n",
                        );
                    }
                }
            }
            Some(cfg)
        }
        Err(_e) => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "display_server: compositor.conf parse error; using defaults\n",
            );
            None
        }
    }
}

/// Phase 72b — authenticated session descriptor read from
/// `/run/m3os-current-session` (written by greeter on successful
/// auth). When present, `SpawnTerm` and `[autostart]` switch into
/// this UID/GID before `execve` so spawned terms run as the
/// authenticated user rather than display_server's own UID. When
/// absent (skip-login / autologin mode), the spawn paths fall back
/// to direct execve and the child inherits display_server's UID.
struct SessionState {
    uid: u32,
    gid: u32,
    user: alloc::string::String,
    home: alloc::string::String,
}

const SESSION_STATE_PATH: &[u8] = b"/run/m3os-current-session\0";

fn read_session_state() -> Option<SessionState> {
    let fd = syscall_lib::open(SESSION_STATE_PATH, 0 /* O_RDONLY */, 0);
    if fd < 0 {
        return None;
    }
    let fd_i32 = fd as i32;
    let mut buf = [0u8; 512];
    let n = syscall_lib::read(fd_i32, &mut buf);
    let _ = syscall_lib::close(fd_i32);
    if n <= 0 {
        return None;
    }
    let bytes = &buf[..n as usize];
    let text = core::str::from_utf8(bytes).ok()?;
    let mut uid: Option<u32> = None;
    let mut gid: Option<u32> = None;
    let mut user = alloc::string::String::new();
    let mut home = alloc::string::String::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("uid=") {
            uid = v.parse().ok();
        } else if let Some(v) = line.strip_prefix("gid=") {
            gid = v.parse().ok();
        } else if let Some(v) = line.strip_prefix("user=") {
            user.push_str(v);
        } else if let Some(v) = line.strip_prefix("home=") {
            home.push_str(v);
        }
    }
    Some(SessionState {
        uid: uid?,
        gid: gid?,
        user,
        home,
    })
}

/// Phase 72 / 72b — `execve` a NUL-terminated path. Used by both
/// the `SUPER+RETURN` keybind handler and the `[autostart]`
/// config-driven launcher. `path_with_nul` and `argv0_with_nul` must
/// each end in a `\0` byte. When a `SessionState` is present, the
/// caller `setuid`/`setgid`s in the child before invoking this and
/// passes a populated `envp_buf`; otherwise envp is empty.
fn spawn_exec(path_with_nul: &[u8], argv0_with_nul: &[u8]) -> isize {
    let argv: [*const u8; 2] = [argv0_with_nul.as_ptr(), core::ptr::null()];
    let envp: [*const u8; 1] = [core::ptr::null()];
    syscall_lib::execve(path_with_nul, &argv, &envp)
}

/// Phase 72b — `execve` with the authenticated user's `USER=` and
/// `HOME=` envp populated. The forked child has already called
/// `setgid`/`setuid` to the user's IDs.
fn spawn_exec_with_session(
    path_with_nul: &[u8],
    argv0_with_nul: &[u8],
    session: &SessionState,
) -> isize {
    let user_var = build_env_var(b"USER=", session.user.as_bytes());
    let home_var = build_env_var(b"HOME=", session.home.as_bytes());
    let path_var = b"PATH=/usr/local/bin:/bin:/sbin:/usr/bin\0";
    let term_var = b"TERM=m3os-term\0";
    let argv: [*const u8; 2] = [argv0_with_nul.as_ptr(), core::ptr::null()];
    let envp: [*const u8; 5] = [
        user_var.as_ptr(),
        home_var.as_ptr(),
        path_var.as_ptr(),
        term_var.as_ptr(),
        core::ptr::null(),
    ];
    syscall_lib::execve(path_with_nul, &argv, &envp)
}

/// Build a NUL-terminated `KEY=value` byte vector for envp use.
fn build_env_var(prefix: &[u8], value: &[u8]) -> alloc::vec::Vec<u8> {
    let mut v = alloc::vec::Vec::with_capacity(prefix.len() + value.len() + 1);
    v.extend_from_slice(prefix);
    v.extend_from_slice(value);
    v.push(0);
    v
}

/// `spawn_term` runs in the just-forked child. It consults the
/// session state, drops privileges if a user is authenticated, and
/// finally `execve`s `/bin/term`. Returns only on execve failure.
fn spawn_term() -> isize {
    if let Some(session) = read_session_state() {
        if syscall_lib::setgid(session.gid) != 0 {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "display_server: setgid failed in spawned child; running as root\n",
            );
        }
        if syscall_lib::setuid(session.uid) != 0 {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "display_server: setuid failed in spawned child; running as root\n",
            );
        }
        spawn_exec_with_session(b"/bin/term\0", b"/bin/term\0", &session)
    } else {
        spawn_exec(b"/bin/term\0", b"/bin/term\0")
    }
}

/// Phase 72b — run every `[autostart] exec = <path>` entry from the
/// parsed config exactly once via `fork + execve`. Called after the
/// first compose frame completes so the launched processes have a
/// live compositor to connect to. `entries` is borrowed from
/// `CompositorConfig::autostart`; the function does not retain it.
///
/// `path` must be staged as a NUL-terminated string at execve time;
/// we allocate a `Vec<u8>` per entry for the `\0` suffix because the
/// kernel ABI requires it.
fn run_autostart(entries: &[alloc::string::String]) {
    use alloc::vec::Vec;
    for path in entries {
        let mut buf: Vec<u8> = Vec::with_capacity(path.len() + 1);
        buf.extend_from_slice(path.as_bytes());
        buf.push(0);
        let pid = syscall_lib::fork();
        if pid == 0 {
            // Phase 72b — same setuid/setgid path as `spawn_term`.
            // Autostart entries inherit the authenticated user's UID
            // when greeter has staged the session file; otherwise
            // (default-skip-login / autologin) the child runs as
            // display_server's own UID.
            if let Some(session) = read_session_state() {
                if syscall_lib::setgid(session.gid) != 0 {
                    syscall_lib::write_str(
                        STDOUT_FILENO,
                        "display_server: autostart setgid failed; running as root\n",
                    );
                }
                if syscall_lib::setuid(session.uid) != 0 {
                    syscall_lib::write_str(
                        STDOUT_FILENO,
                        "display_server: autostart setuid failed; running as root\n",
                    );
                }
                let _ = spawn_exec_with_session(&buf, &buf, &session);
            } else {
                let _ = spawn_exec(&buf, &buf);
            }
            syscall_lib::exit(99);
        }
        syscall_lib::write_str(STDOUT_FILENO, "display_server: autostart exec=");
        syscall_lib::write_str(STDOUT_FILENO, path);
        syscall_lib::write_str(STDOUT_FILENO, "\n");
    }
}

/// Phase 72 — execute a typed `KeybindAction` against the live
/// compositor state. Each arm is a small state-machine step the
/// main loop folds into its existing focus / publish helpers.
fn dispatch_keybind_action(
    action: keybind::KeybindAction,
    workspace_mgr: &mut workspace::WorkspaceManager,
    bind_stack: &mut keybind::BindStack,
    focused: &mut Option<SurfaceId>,
    control_subs: &mut control::ControlSubscriptions,
    cfg: &config::CompositorConfig,
    client_event_queue: &mut alloc::collections::VecDeque<(SurfaceId, ServerMessage)>,
    compose_ctx: &mut compose::ComposeContext,
) {
    use keybind::{BindMode, KeybindAction};
    match action {
        KeybindAction::SwitchWorkspace(n) => {
            let idx = (n.saturating_sub(1)) as usize;
            if let Ok(transition) = workspace_mgr.switch_workspace(idx) {
                if transition.switched {
                    syscall_lib::write_str(STDOUT_FILENO, "display_server: workspace switched to ");
                    write_u32(n as u32);
                    syscall_lib::write_str(STDOUT_FILENO, "\n");
                    // Pick focus from the new workspace; falls back
                    // to None if it is empty.
                    *focused = workspace_mgr.next_focus(None);
                    publish_focus_changed(control_subs, *focused, null_subscriber_sender);
                    compose_ctx.invalidate_arrangement_cache();
                    compose_ctx.force_full_repaint();
                }
            }
        }
        KeybindAction::MoveToWorkspace(n) => {
            if let Some(fid) = *focused {
                let idx = (n.saturating_sub(1)) as usize;
                let follow = cfg.workspaces.follow_on_move;
                if let Ok(transition) = workspace_mgr.move_to_workspace(fid, idx, follow) {
                    syscall_lib::write_str(STDOUT_FILENO, "display_server: moved focused to ");
                    write_u32(n as u32);
                    syscall_lib::write_str(STDOUT_FILENO, "\n");
                    if transition.switched {
                        *focused = workspace_mgr.next_focus(None);
                        publish_focus_changed(control_subs, *focused, null_subscriber_sender);
                    } else if !workspace_mgr.current().windows().contains(&fid) {
                        // Focused window left the active workspace.
                        *focused = workspace_mgr.next_focus(None);
                        publish_focus_changed(control_subs, *focused, null_subscriber_sender);
                    }
                    compose_ctx.invalidate_arrangement_cache();
                    compose_ctx.force_full_repaint();
                }
            }
        }
        KeybindAction::CycleFocus => {
            let next = workspace_mgr.next_focus(*focused);
            if next != *focused {
                *focused = next;
                publish_focus_changed(control_subs, *focused, null_subscriber_sender);
                // Repaint so border colours track the new focus, and
                // recompute arrangement for `focus_affects_geometry()`
                // policies (tabbed / fullscreen).
                compose_ctx.invalidate_arrangement_cache();
                compose_ctx.force_full_repaint();
            }
        }
        KeybindAction::SpawnTerm => {
            let pid = syscall_lib::fork();
            if pid == 0 {
                let _ = spawn_term();
                syscall_lib::exit(99);
            }
            syscall_lib::write_str(STDOUT_FILENO, "display_server: SUPER+RETURN spawned term\n");
        }
        KeybindAction::LaunchLauncher => {
            // Phase 73 — fork + execve the launcher. If a launcher is
            // already running, the existing process owns the chord
            // surface and a second fork is harmless: it exits after
            // failing to acquire the singleton service-registry slot
            // it tries to register. Cheap (one extra fork on
            // double-press) and keeps the code path symmetric with
            // `SpawnTerm`.
            let pid = syscall_lib::fork();
            if pid == 0 {
                if let Some(session) = read_session_state() {
                    let _ = syscall_lib::setgid(session.gid);
                    let _ = syscall_lib::setuid(session.uid);
                    let _ =
                        spawn_exec_with_session(b"/bin/launcher\0", b"/bin/launcher\0", &session);
                } else {
                    let _ = spawn_exec(b"/bin/launcher\0", b"/bin/launcher\0");
                }
                syscall_lib::exit(99);
            }
            syscall_lib::write_str(
                STDOUT_FILENO,
                "display_server: SUPER+SPACE spawned launcher\n",
            );
        }
        KeybindAction::KillFocused => {
            // Phase 72b Track K.6 — push `ServerMessage::CloseRequest`
            // to the focused surface's owning client. The client
            // decides how to shut down gracefully; the compositor
            // does not forcibly destroy surfaces or kill processes
            // here (escalation to SIGTERM/SIGKILL belongs in
            // session_manager / init).
            if let Some(fid) = *focused {
                syscall_lib::write_str(STDOUT_FILENO, "display_server: SUPER+Q close-request id=");
                write_u32(fid.0);
                syscall_lib::write_str(STDOUT_FILENO, "\n");
                if client_event_queue.len() == client::MAX_CLIENT_EVENT_QUEUE {
                    client_event_queue.pop_front();
                }
                client_event_queue
                    .push_back((fid, ServerMessage::CloseRequest { surface_id: fid }));
            }
        }
        KeybindAction::EnterResize => {
            bind_stack.push_mode(BindMode::Resize);
            syscall_lib::write_str(STDOUT_FILENO, "display_server: entered resize mode\n");
        }
        KeybindAction::ExitResize => {
            bind_stack.pop_to_default();
            syscall_lib::write_str(STDOUT_FILENO, "display_server: exited resize mode\n");
        }
        KeybindAction::ResizeFocused { direction, step } => {
            if let Some(fid) = *focused {
                let result = workspace_mgr
                    .current_mut()
                    .adjust_focused(fid, direction, step);
                if let Err(layout::LayoutError::Unsupported) = result {
                    // Phase 72b K.5 — surface the rejection instead of
                    // silently dropping H/J/K/L keypresses. The user
                    // entered resize mode against a layout that does
                    // not support per-tile resize (grid is uniform;
                    // tabbed shows only the focused tile; fullscreen
                    // is fullscreen). Log it and auto-exit resize
                    // mode so subsequent keystrokes reach the focused
                    // client instead of vanishing into the bind stack.
                    let policy_name = workspace_mgr.current().policy().as_name();
                    syscall_lib::write_str(
                        STDOUT_FILENO,
                        "display_server: resize not supported under '",
                    );
                    syscall_lib::write_str(STDOUT_FILENO, policy_name);
                    syscall_lib::write_str(STDOUT_FILENO, "'; exiting resize mode\n");
                    bind_stack.pop_to_default();
                }
                // Other variants (NoFocusedWindow etc.) keep resize
                // mode active — the user might press H/J/K/L after
                // focusing a window. Log a one-line warning so the
                // failure mode is visible.
                if let Err(other) = result.as_ref() {
                    if !matches!(other, layout::LayoutError::Unsupported) {
                        syscall_lib::write_str(
                            STDOUT_FILENO,
                            "display_server: resize keystroke ignored (no focused window?)\n",
                        );
                    }
                }
                // On a successful resize the policy state changed under
                // the same id set, so the arrangement cache would
                // otherwise keep painting the previous ratios.
                if result.is_ok() {
                    compose_ctx.invalidate_arrangement_cache();
                    compose_ctx.force_full_repaint();
                }
                let _ = result;
            } else {
                // No focused window — resize chord has nothing to act
                // on. Stay in resize mode; the user may focus a window
                // next.
                syscall_lib::write_str(
                    STDOUT_FILENO,
                    "display_server: resize keystroke ignored (no focused window)\n",
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Phase 72 — control-socket verb handlers
// ---------------------------------------------------------------------------

fn handle_set_layout(
    workspace_mgr: &mut workspace::WorkspaceManager,
    kind_byte: u8,
    reply_buf: &mut [u8],
) -> usize {
    use kernel_core::display::control::{ControlErrorCode, ControlEvent};
    let kind = match kind_byte {
        0 => layout::PolicyKind::MasterStack,
        1 => layout::PolicyKind::Dwindle,
        2 => layout::PolicyKind::Spiral,
        3 => layout::PolicyKind::Grid,
        4 => layout::PolicyKind::Tabbed,
        5 => layout::PolicyKind::Fullscreen,
        _ => {
            return encode_event_or_drop(
                &ControlEvent::Error {
                    code: ControlErrorCode::BadArgs,
                },
                reply_buf,
            );
        }
    };
    workspace_mgr.set_current_layout(kind);
    encode_event_or_drop(&ControlEvent::Ack, reply_buf)
}

fn handle_switch_workspace(
    workspace_mgr: &mut workspace::WorkspaceManager,
    focused: &mut Option<SurfaceId>,
    subs: &mut control::ControlSubscriptions,
    n: u8,
    reply_buf: &mut [u8],
) -> usize {
    use kernel_core::display::control::{ControlErrorCode, ControlEvent};
    if n == 0 || (n as usize) > workspace::NUM_WORKSPACES {
        return encode_event_or_drop(
            &ControlEvent::Error {
                code: ControlErrorCode::BadArgs,
            },
            reply_buf,
        );
    }
    let idx = (n - 1) as usize;
    match workspace_mgr.switch_workspace(idx) {
        Ok(transition) => {
            if transition.switched {
                *focused = workspace_mgr.next_focus(None);
                publish_focus_changed(subs, *focused, null_subscriber_sender);
                publish_to_subscribers_workspace(subs, n);
            }
            encode_event_or_drop(&ControlEvent::Ack, reply_buf)
        }
        Err(_) => encode_event_or_drop(
            &ControlEvent::Error {
                code: ControlErrorCode::BadArgs,
            },
            reply_buf,
        ),
    }
}

fn handle_move_to_workspace(
    workspace_mgr: &mut workspace::WorkspaceManager,
    focused: &mut Option<SurfaceId>,
    subs: &mut control::ControlSubscriptions,
    n: u8,
    follow: u8,
    reply_buf: &mut [u8],
) -> usize {
    use kernel_core::display::control::{ControlErrorCode, ControlEvent};
    if n == 0 || (n as usize) > workspace::NUM_WORKSPACES {
        return encode_event_or_drop(
            &ControlEvent::Error {
                code: ControlErrorCode::BadArgs,
            },
            reply_buf,
        );
    }
    let Some(fid) = *focused else {
        return encode_event_or_drop(
            &ControlEvent::Error {
                code: ControlErrorCode::UnknownSurface,
            },
            reply_buf,
        );
    };
    let idx = (n - 1) as usize;
    let follow_bool = follow != 0;
    match workspace_mgr.move_to_workspace(fid, idx, follow_bool) {
        Ok(transition) => {
            if transition.switched {
                *focused = workspace_mgr.next_focus(None);
                publish_focus_changed(subs, *focused, null_subscriber_sender);
                publish_to_subscribers_workspace(subs, n);
            } else if !workspace_mgr.current().windows().contains(&fid) {
                *focused = workspace_mgr.next_focus(None);
                publish_focus_changed(subs, *focused, null_subscriber_sender);
            }
            encode_event_or_drop(&ControlEvent::Ack, reply_buf)
        }
        Err(_) => encode_event_or_drop(
            &ControlEvent::Error {
                code: ControlErrorCode::BadArgs,
            },
            reply_buf,
        ),
    }
}

fn handle_reload(
    workspace_mgr: &mut workspace::WorkspaceManager,
    bind_stack: &mut keybind::BindStack,
    compositor_config: &mut config::CompositorConfig,
    reply_buf: &mut [u8],
) -> usize {
    use kernel_core::display::control::ControlEvent;
    if let Some(new_cfg) = load_compositor_config(config::CONFIG_PATH) {
        *compositor_config = new_cfg;
        bind_stack.reload_default(&compositor_config.keybinds.user_chords);
        bind_stack.set_resize_step_px(compositor_config.keybinds.resize_step_px);
        for i in 0..workspace::NUM_WORKSPACES {
            if let Some(ws) = workspace_mgr.workspace_mut(i) {
                ws.set_policy(compositor_config.workspaces.defaults[i]);
            }
        }
        syscall_lib::write_str(STDOUT_FILENO, "display_server: reloaded compositor.conf\n");
    } else {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "display_server: reload kept previous config\n",
        );
    }
    encode_event_or_drop(&ControlEvent::Ack, reply_buf)
}

fn handle_query_windows(
    workspace_mgr: &mut workspace::WorkspaceManager,
    focused: Option<SurfaceId>,
    gaps: layout::GapConfig,
    output_width: u32,
    output_height: u32,
    reply_buf: &mut [u8],
) -> usize {
    use kernel_core::display::control::{ControlEvent, WindowQueryEntry};
    use kernel_core::display::protocol::Rect as ProtoRect;
    // For each workspace that holds a window, run the layout and
    // collect the per-window rect. The output rect comes from the
    // framebuffer's actual metadata so the query view matches the
    // compositor's authoritative arrangement on every real display
    // size (instead of the previous hardcoded 1280x720 placeholder
    // that disagreed with the rest of the repo's 1280x800 baseline).
    let output = ProtoRect {
        x: 0,
        y: 0,
        w: output_width,
        h: output_height,
    };
    let mut entries: alloc::vec::Vec<WindowQueryEntry> = alloc::vec::Vec::new();
    for ws_idx in 0..workspace::NUM_WORKSPACES {
        let Some(ws) = workspace_mgr.workspace(ws_idx) else {
            continue;
        };
        if ws.is_empty() {
            continue;
        }
        // Re-tile the workspace into the real framebuffer rect so the
        // `m3ctl query windows` reply reports the rects that compose
        // would actually paint. `output` above is built from
        // `output_width` / `output_height` (the live framebuffer
        // metadata) — the previous synthetic 1280×720 fallback is gone.
        let ws_mut = workspace_mgr.workspace_mut(ws_idx).expect("idx checked");
        let inner = layout::apply_outer_gaps(output, gaps.outer);
        let rects = ws_mut.tile(inner, gaps);
        for (sid, rect) in rects {
            entries.push(WindowQueryEntry {
                surface_id: sid,
                workspace: (ws_idx + 1) as u8,
                rect,
                focused: focused == Some(sid),
            });
        }
    }
    encode_event_or_drop(&ControlEvent::WindowListReply { entries }, reply_buf)
}

fn handle_query_workspaces(
    workspace_mgr: &workspace::WorkspaceManager,
    reply_buf: &mut [u8],
) -> usize {
    use kernel_core::display::control::{ControlEvent, WorkspaceQueryEntry};
    let mut entries: alloc::vec::Vec<WorkspaceQueryEntry> =
        alloc::vec::Vec::with_capacity(workspace::NUM_WORKSPACES);
    for (idx, ws) in workspace_mgr.iter() {
        entries.push(WorkspaceQueryEntry {
            workspace: (idx + 1) as u8,
            policy_kind: policy_kind_to_byte(ws.policy()),
            window_count: ws.len() as u32,
            active: idx == workspace_mgr.current_index(),
        });
    }
    encode_event_or_drop(&ControlEvent::WorkspaceListReply { entries }, reply_buf)
}

fn handle_set_master_ratio(
    workspace_mgr: &mut workspace::WorkspaceManager,
    ratio_x100: u16,
    reply_buf: &mut [u8],
) -> usize {
    use kernel_core::display::control::ControlEvent;
    let ratio = (ratio_x100 as f32) / 100.0;
    // The PolicySet is per-workspace and the master-stack state is
    // inside it; mutating it requires a path through the workspace.
    // We do so here via a thin accessor.
    if let Some(ws) = workspace_mgr.workspace_mut(workspace_mgr.current_index()) {
        ws.set_master_ratio(ratio);
    }
    encode_event_or_drop(&ControlEvent::Ack, reply_buf)
}

fn handle_tile_fullscreen(
    workspace_mgr: &mut workspace::WorkspaceManager,
    reply_buf: &mut [u8],
) -> usize {
    use kernel_core::display::control::ControlEvent;
    let cur = workspace_mgr.current().policy();
    let new = if cur == layout::PolicyKind::Fullscreen {
        layout::PolicyKind::Dwindle
    } else {
        layout::PolicyKind::Fullscreen
    };
    workspace_mgr.set_current_layout(new);
    encode_event_or_drop(&ControlEvent::Ack, reply_buf)
}

fn policy_kind_to_byte(k: layout::PolicyKind) -> u8 {
    match k {
        layout::PolicyKind::MasterStack => 0,
        layout::PolicyKind::Dwindle => 1,
        layout::PolicyKind::Spiral => 2,
        layout::PolicyKind::Grid => 3,
        layout::PolicyKind::Tabbed => 4,
        layout::PolicyKind::Fullscreen => 5,
    }
}

/// Phase 72 — publish a `WorkspaceChanged` event to subscribers of
/// the `FocusChanged` kind (the existing subscribable slot the new
/// event variant reuses).
fn publish_to_subscribers_workspace(subs: &mut control::ControlSubscriptions, workspace_num: u8) {
    use kernel_core::display::control::ControlEvent;
    kernel_core::display::subscription::publish_to_subscribers(
        subs,
        ControlEvent::WorkspaceChanged {
            workspace: workspace_num,
        },
        null_subscriber_sender,
    );
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "display_server: PANIC\n");
    let _ = syscall_lib::framebuffer_release();
    syscall_lib::exit(101)
}
