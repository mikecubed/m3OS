# Service Model Maturity

**Aligned Roadmap Phase:** Phase 51
**Status:** In Progress
**Source Ref:** phase-51

## Overview

Phase 51 hardens the Phase 46 service manager into a trusted lifecycle model.
Where Phase 46 introduced data-driven service definitions, dependency ordering,
restart policies, syslog, cron, and admin commands, Phase 51 strengthens every
layer so that later extracted ring-3 services can join the same supervision
framework without inventing their own conventions. The key changes are: a
stabilized service-definition contract with privilege and timeout fields, a
validated state machine with enforced transition guards, restart backoff with crash
classification, deterministic shutdown ordering with per-service timeouts and
orphan reaping, syslog integration for init itself, and a hardened admin control
path.

## What This Doc Covers

- Service-definition contract (fields: name, command, type, restart, max_restart,
  depends, user, stop_timeout)
- Service state machine with enforced transition guards
- Restart backoff (1s, 2s, 5s cap; reset after 10s uptime)
- Crash classification (clean exit, error exit, signal death)
- Shutdown ordering (reverse dependency order, per-service timeout, orphan reaping)
- Logging model (init routes lifecycle events through syslog)
- Admin surface (service list/status/start/stop/restart/enable/disable, control
  file hardening)
- Directory scan for service configs (replacing KNOWN_CONFIGS)

## Core Implementation

### Service-Definition Contract

Phase 46 introduced `key=value` service configs in `/etc/services.d/`. Phase 51
stabilizes this format as the contract that all managed services must follow.

**Fields:**

| Field | Type | Default | Description |
|---|---|---|---|
| `name` | string | required | Unique service identifier |
| `command` | path | required | Executable path |
| `type` | `daemon` or `oneshot` | `daemon` | Whether init waits for exit or supervises |
| `restart` | `always`, `on-failure`, `never` | `always` | When to restart after exit |
| `max_restart` | integer | 10 | Maximum consecutive restart attempts before permanent stop |
| `depends` | comma-separated names | none | Services that must be running before this one starts |
| `user` | UID (numeric) | 0 (root) | Privilege level for the service process |
| `stop_timeout` | seconds | 5 | How long to wait between SIGTERM and SIGKILL during shutdown |

Phase 51 also replaces the hardcoded `KNOWN_CONFIGS` array with a directory scan
of `/etc/services.d/*.conf`, so new services can be added by dropping a config
file without recompiling init.

### Service State Machine

The state machine from Phase 46 is retained with a `try_transition` validation
method. Invalid transitions (such as `PermanentlyStopped` to `Starting`) are
rejected — `start_service` and `stop_service` return early when the guard
fails, preventing duplicate instances or stop attempts on already-stopped
services.

```
NeverStarted --> Starting --> Running --> Stopping --> Stopped(exit_code)
                                                          |
                                                    PermanentlyStopped
```

Every state transition is logged and triggers an immediate status-file update
so operators and tooling always see the current state. The status file now
includes the exit code or signal number for stopped services and a timestamp
of the last state change.

### Restart Backoff

Phase 46 used a flat 1-second delay between restarts. Phase 51 introduces
progressive backoff:

| Consecutive restart | Delay |
|---|---|
| 1 | 1 second |
| 2 | 2 seconds |
| 3+ | 5 seconds (cap) |

The restart counter and delay reset to their initial values when a service runs
successfully for at least 10 seconds before exiting. This prevents a
crash-looping service from consuming resources at a constant rate while still
recovering quickly from transient failures.

### Crash Classification

Phase 51 distinguishes three exit conditions for restart decisions:

| Condition | Detection | Restart on `on-failure` |
|---|---|---|
| Clean exit | exit code 0 | No |
| Error exit | exit code != 0 | Yes |
| Signal death | terminated by signal | Yes |

Exit classification is logged with the service name so operators can distinguish
intentional stops from crashes. The `restart=on-failure` policy uses this
classification to avoid restarting services that exited cleanly.

### Shutdown Ordering

Shutdown walks the dependency graph in reverse topological order: a service is
stopped only after all services that depend on it have already stopped. Phase 51
adds three improvements over Phase 46:

1. **Per-service timeout.** Each service can declare its own `stop_timeout`
   instead of using the global 5-second default. Services with open connections
   (e.g., sshd) can request longer timeouts.

2. **Progress logging.** Each service stop is logged with the service name,
   the action taken (SIGTERM sent, SIGKILL forced, stopped cleanly), and
   elapsed time. Total shutdown duration is logged at completion.

3. **Orphan reaping.** After all managed services are stopped, a final
   `waitpid(-1, WNOHANG)` loop reaps any remaining orphaned children before
   calling `sys_reboot`. A global timeout (15 seconds) prevents infinite waits.

### Logging Model

Phase 46 had syslogd receiving messages from daemons but init logged only to
stdout/serial. Phase 51 connects init to syslog:

- After syslogd reaches Running state, init opens a DGRAM socket to `/dev/log`.
- Service lifecycle events (start, stop, restart, crash, permanent stop) are
  sent as syslog messages with facility `daemon`.
- If syslog is unavailable (e.g., during early boot), init falls back to serial
  output.
- Syslog messages from init carry tag `init` for filtering.

Kernel diagnostic messages from `/dev/kmsg` continue to appear in
`/var/log/kern.log` and are also forwarded to `/var/log/messages` with `kern`
facility prefix for unified viewing.

### Admin Surface

The `service` command gains richer output and new subcommands:

| Subcommand | Behavior |
|---|---|
| `service list` | Summary table: name, state, PID, restart count |
| `service status <name>` | Detail: name, state, PID, restart count, last exit code/signal, last state-change timestamp |
| `service start <name>` | Start a stopped service (root only) |
| `service stop <name>` | Stop a running service (root only) |
| `service restart <name>` | Stop then start (root only) |
| `service enable <name>` | Remove `.disabled` marker; service starts at next boot |
| `service disable <name>` | Create `.disabled` marker; service skipped at boot |

The init control path (`/var/run/init.cmd`) is hardened: the file is created
with mode 0600 inside root-owned `/var/run`, and the `service` command checks
root privilege before writing.

## Key Files

| File | Purpose |
|---|---|
| `userspace/init/src/main.rs` | Service manager: parser, dep graph, lifecycle, restart backoff, shutdown, control path |
| `userspace/syslogd/src/main.rs` | Syslog daemon: init log integration, facility coverage |
| `userspace/coreutils-rs/src/service.rs` | `service` command: list, status, start, stop, restart, enable, disable |
| `kernel/initrd/etc/services.d/*.conf` | Service definition files with new fields |

## How This Phase Differs From Later Work

- This phase stabilizes the service contract and supervision semantics for
  the shipped daemon set. Later Phase 52 extracts the first core services into
  supervised ring-3 processes that join this framework.
- The control path uses a file-based interface (`/var/run/init.cmd`). Later
  phases may replace this with IPC-based service control via the registry.
- No socket activation, health probes, or watchdog support. These are deferred
  to later phases if needed.
- Service sandboxing (cgroups, namespaces, capability confinement) is not part
  of this phase.

## Related Roadmap Docs

- [Phase 51 roadmap doc](./roadmap/51-service-model-maturity.md)
- [Phase 51 task doc](./roadmap/tasks/51-service-model-maturity-tasks.md)
- [Phase 46 learning doc](./46-system-services.md) (baseline this phase extends)

## Deferred or Later-Phase Topics

- Socket activation and readiness protocols (sd_notify-style)
- Advanced service sandboxing and capability confinement
- IPC-based service control replacing the file-based command interface
- Rich health probes, watchdogs, and multi-instance orchestration
- Structured journaling and long-term log retention policy
- Runlevels and targets
