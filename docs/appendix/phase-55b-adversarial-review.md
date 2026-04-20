# Phase 55b Adversarial Review (Codex)

Target: `feat/phase-55b-ring-3-driver-host` branch diff against `main`
Reviewer: Codex (GPT-5.4) via `/codex:adversarial-review`
Date: 2026-04-20
Verdict: **needs-attention**

## Summary

No-ship: this branch opens a hardware trust boundary to any userspace task, can
revoke unrelated device claims on a single allocation failure, and auto-wires
block I/O to an untrusted service name.

## Findings

### [critical] `sys_device_claim` has no authorization gate

Location: `kernel/src/syscall/device_host.rs:546-557`

The syscall explicitly leaves the Phase 48 credential check disabled
(`if false`), so any ring-3 process can claim PCI devices and obtain
`Capability::Device`. From there the same process can map MMIO, allocate DMA,
and subscribe IRQs through the new device-host syscalls. That is a direct
privilege-boundary break, not a missing enhancement.

Recommendation: Block `sys_device_claim` until a real policy check is enforced.
If the credential plumbing is not ready, fail closed here and only enable the
syscall for explicitly authorized driver processes.

### [high] Cap-table exhaustion tears down every claim for the PID and skips derived-resource cleanup

Location: `kernel/src/syscall/device_host.rs:604-620`

If inserting the new `Capability::Device` fails, the unwind path calls
`reg.release_for_pid(pid)` directly. That removes every claim owned by the
process, not just the one being created, so one ENOMEM on a new claim can
revoke unrelated devices already in use. Worse, this bypasses
`release_claims_for_pid`, so existing MMIO mappings, DMA allocations, and IRQ
bindings for those older claims are not torn down in the same path. The result
is partial teardown: live derived resources with their parent claims silently
gone.

Recommendation: Rollback only the just-inserted claim key. Reuse the full
cascade cleanup helpers when removing an existing claim, or add a single-key
variant that releases IRQ/MMIO/DMA state before dropping the claim handle.

### [high] Remote NVMe auto-registration trusts an unprivileged service name

Location: `kernel/src/blk/remote.rs:79-124`

`RemoteBlockDevice::is_registered()` now binds kernel block dispatch to
whichever endpoint is registered as `nvme.block`, with no verification that
the owner is an authorized driver or that it actually claimed the target
device. In this codebase, `ipc_register_service` accepts arbitrary userspace
registrations for non-private names, so any process that grabs `nvme.block`
first can receive kernel block I/O whenever virtio-blk is absent. That
exposes filesystem reads/writes to a spoofed service and makes boot/device
selection depend on a string registry entry instead of a capability-backed
trust check.

Recommendation: Do not auto-bind block dispatch from the global service
registry alone. Require an explicit privileged registration path, or verify
that the service owner is the supervised NVMe driver process and already
holds the expected device claim before switching block I/O to it.

## Next Steps

- Enforce a real authorization check in `sys_device_claim` before shipping the
  device-host ABI.
- Fix the claim rollback path so a failed new claim cannot revoke unrelated
  devices or leave MMIO/DMA/IRQ state behind.
- Replace service-name-only remote-driver activation with a capability/owner-
  validated registration handshake.

## Resolution

Date: 2026-04-20
Verdict: **resolved**

All three findings were triaged as valid and fixed on this branch. The fixes
mirror the same "supervised driver process" trust classification so the
device-host and remote-block entry points agree on who is allowed to drive
hardware.

### [critical] `sys_device_claim` — fixed

`kernel/src/syscall/device_host.rs`

Replaced the `if false` Phase 48 stub with a real authorization gate
(`is_authorized_driver_process`). A caller is accepted only when its
`Process::exec_path` starts with `/drivers/` (the same prefix `init` uses to
classify supervised driver services — see `init: driver.registered` events).
`exec_path` is written by the kernel on `execve`, so a ring-3 process cannot
forge it. Everything else gets `-EACCES`. Phase 48 credentials will later
replace this single lookup with a proper policy decision point.

### [high] Cap-table rollback — fixed

`kernel/src/syscall/device_host.rs`

Replaced the `reg.release_for_pid(pid)` rollback with a new
`release_single(pid, key)` helper that removes exactly the just-inserted
claim. Unrelated claims the same PID already holds — along with their
derived MMIO/DMA/IRQ state — are no longer disturbed by an ENOMEM on a
fresh claim. The full cascade (`release_claims_for_pid`) is still the
teardown path on process exit; it is not the rollback path.

### [high] Remote NVMe auto-registration — fixed

`kernel/src/blk/remote.rs`, `kernel-core/src/ipc/registry.rs`,
`kernel/src/ipc/registry.rs`, `kernel/src/task/scheduler.rs`

Added `Registry::lookup_with_owner` so the kernel can see who registered
a service. `is_registered()` now gates auto-binding on the owner's
`exec_path` starting with `/drivers/` (new helper `is_trusted_driver_task`
resolves the service's `owner_task_id` → PID → exec_path). A non-driver
process that grabs `nvme.block` first is ignored and logged; kernel-owned
registrations (`owner == 0`) remain trusted so the boot-time wiring path
still works. Explicit `register()` calls continue to bypass the gate by
writing `g.state` directly.

### Validation

- `cargo xtask check` — clippy clean, rustfmt clean, kernel-core + passwd +
  driver_runtime host tests pass.
- `cargo test -p kernel-core --target x86_64-unknown-linux-gnu` — all
  suites pass.
- `cargo xtask test` (QEMU harness) — `device_host B.2 cascade test`,
  `post_crash_handles_invalid_in_restarted_process`, and the rest of the
  in-kernel integration tests pass.
- `cargo xtask image` — release image builds cleanly.
