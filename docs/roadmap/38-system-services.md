# Phase 38 - System Services

## Milestone Goal

The OS has a service manager, system logging, and scheduled task execution. Services
(sshd, telnetd, etc.) are managed by a unified init/service system that handles
startup ordering, restart on failure, and clean shutdown. The OS behaves like a real
server that can be administered remotely.

## Learning Goals

- Understand how init systems (SysV init, systemd, runit) manage service lifecycles.
- Learn how syslog provides centralized logging for all system services.
- See how cron enables scheduled automation.
- Understand daemon processes: double-fork, setsid, and why they exist.

## Feature Scope

### Service Manager (enhanced init)

Extend the existing PID 1 init to manage services:

**Service Definition** (`/etc/services.d/sshd.conf`):
```
name=sshd
command=/sbin/sshd
type=daemon
restart=always
depends=network
```

**Service Operations:**
- `service start <name>` — start a service
- `service stop <name>` — stop a service (send SIGTERM, then SIGKILL)
- `service restart <name>` — stop then start
- `service status <name>` — show running/stopped, PID, uptime
- `service list` — show all services and their status

**Lifecycle Management:**
- Start services in dependency order at boot.
- Restart services that exit unexpectedly (if `restart=always`).
- Handle `SIGCHLD` to detect service exits.
- Clean shutdown: stop all services in reverse dependency order on `shutdown`/`reboot`.

### System Logging (`syslogd`)

- Accept log messages from services via a Unix domain socket (`/dev/log`) or
  a simple kernel ring buffer interface.
- Write logs to `/var/log/messages` (persistent) and `/var/log/kern.log` (kernel messages).
- Log format: `timestamp hostname service[pid]: message`
- `logger` command to send log messages from the shell.
- `tail -f /var/log/messages` for live log monitoring.

### Scheduled Tasks (`crond`)

A minimal cron daemon:
- Read `/etc/crontab` and `/var/spool/cron/<user>` crontab files.
- Standard cron format: `minute hour day month weekday command`
- Execute commands at scheduled times.
- Special strings: `@reboot`, `@hourly`, `@daily`
- `crontab -e` to edit the current user's crontab (uses `$EDITOR`).
- `crontab -l` to list scheduled jobs.

### System Administration Commands

- **`shutdown`** — initiate clean system shutdown (stop services, sync disks, halt)
- **`reboot`** — clean restart
- **`hostname`** — get/set the system hostname
- **`date`** — display/set system date and time
- **`who` / `w`** — show logged-in users (reads utmp/wtmp or PTY table)
- **`last`** — show recent logins (reads wtmp log)

### Kernel Support (if needed)

- `clock_gettime` improvements for accurate scheduling.
- Kernel shutdown path: sync filesystems, stop drivers, halt/reboot CPU.
- `reboot()` syscall.
- Unix domain sockets (for syslog) — or use pipes/files as simpler alternative.

## Prerequisites

| Phase | Why needed |
|---|---|
| Phase 27 (User Accounts) | Service ownership, user crontabs |
| Phase 29 (PTY) | who/w reads PTY session info |
| Phase 30 (Telnet) / Phase 35 (SSH) | Remote services to manage |
| Phase 24 (Persistent Storage) | Log files, crontabs persist |
| Phase 19 (Signals) | SIGTERM/SIGKILL for service management |

## Implementation Outline

1. Enhance init to read service definitions from `/etc/services.d/`.
2. Implement dependency ordering (topological sort).
3. Implement the `service` command.
4. Implement service restart monitoring (SIGCHLD handler).
5. Write `syslogd`: listen on `/dev/log` (or a named pipe), write to log files.
6. Write the `logger` command.
7. Write `crond`: parse crontab format, sleep until next job, execute.
8. Write `crontab` command.
9. Write `shutdown`, `reboot`, `hostname`, `date`, `who` utilities.
10. Implement kernel reboot/shutdown syscalls.
11. Test full lifecycle: boot → services start → cron runs → remote login → shutdown.

## Acceptance Criteria

- Services defined in `/etc/services.d/` start automatically at boot in dependency order.
- `service status sshd` shows the service is running with its PID.
- Killing a `restart=always` service causes it to be automatically restarted.
- `service stop telnetd` cleanly stops the telnet server.
- Log messages from services appear in `/var/log/messages`.
- `logger "test message"` writes to the system log.
- A cron job scheduled for every minute executes on time.
- `shutdown` cleanly stops all services and halts the system.
- `reboot` cleanly restarts the system.
- `who` shows currently logged-in users.

## Companion Task List

- [Phase 38 Task List](./tasks/38-system-services-tasks.md)

## How Real OS Implementations Differ

Real service managers:
- systemd: unit files, socket activation, cgroups, journal, timer units — enormous scope
- runit: simple supervision tree, process 1/2/3 stages
- OpenRC: dependency-based init with parallel startup
- launchd (macOS): combines init, cron, inetd, and more

Our approach is closest to runit or early SysV init: simple service definitions,
dependency ordering, and basic supervision. This is enough to understand the concepts
without the complexity of systemd.

Real cron implementations (Vixie cron, dcron) handle:
- Mailto on job output
- Timezone-aware scheduling
- Randomized delay
- Access control (cron.allow, cron.deny)

Our cron is minimal: parse the format, execute at the right time.

## Deferred Until Later

- Socket activation (systemd-style)
- Service sandboxing (cgroups, namespaces)
- Journal (structured logging)
- Log rotation
- Remote syslog
- NTP time synchronization
- systemd-compatible unit files
- Runlevels / targets
