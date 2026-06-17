#!/usr/bin/env bash
# m3os-logsink.sh — receive m3OS syslog output on a second machine.
#
# Mode 1 (default): binds a UDP listener on BIND_PORT (default 514) and
# appends every received datagram to LOG_FILE (default ./m3os-console.log).
# Use this during bare-metal driver bring-up before networking is fully up,
# by pointing m3OS syslogd at THIS_HOST:BIND_PORT.
#
# Mode 2 (--ssh user@host): SSH-tails the remote syslog and appends to the
# same log file.  Use this once m3OS sshd is reachable.
#
# Usage:
#   m3os-logsink.sh [--port <N>] [--log <file>] [--ssh <user@host>]
#                   [--remote-cmd <cmd>] [--help]
#
# Environment:
#   BIND_PORT   UDP port to listen on (default: 514)
#   LOG_FILE    Path to append log lines to (default: ./m3os-console.log)
#
# Requirements (Mode 1): socat (preferred) or nc with -ul support.
# Requirements (Mode 2): ssh in PATH.
#
# Note: ports below 1024 require root.  Run as root or use --port 5140
# (or any unprivileged port) and configure m3OS syslogd accordingly.
set -euo pipefail

# ── defaults ────────────────────────────────────────────────────────────────
BIND_PORT="${BIND_PORT:-514}"
LOG_FILE="${LOG_FILE:-./m3os-console.log}"
SSH_TARGET=""
REMOTE_CMD="tail -f /var/log/messages"

# ── argument parsing ─────────────────────────────────────────────────────────
usage() {
    cat <<'EOF'
Usage: m3os-logsink.sh [OPTIONS]

Collect m3OS serial/syslog output on a second machine.

Mode 1 (default — UDP listener):
  Binds a UDP socket and appends every received datagram to a log file.
  Point m3OS syslogd at THIS_HOST:BIND_PORT (e.g. "syslogd -R 192.168.1.5").

  Options:
    --port  N       UDP port to bind (default: $BIND_PORT, or BIND_PORT env)
    --log   FILE    Log file to append to (default: $LOG_FILE, or LOG_FILE env)

Mode 2 (SSH tail):
  Opens an SSH connection and tails a remote log, appending to the same file.
  Use once m3OS sshd is reachable.

  Options:
    --ssh   USER@HOST   SSH target
    --remote-cmd CMD    Remote command to run (default: "tail -f /var/log/messages")
    --log   FILE        Log file to append to

General:
    --help              Show this help and exit

Environment variables:
    BIND_PORT   Overrides the default UDP bind port (Mode 1)
    LOG_FILE    Overrides the default log file path

Requirements:
    Mode 1: socat (preferred) or nc with -ul support
    Mode 2: ssh in PATH

Note: binding ports < 1024 requires root.
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --port)
            BIND_PORT="$2"; shift 2 ;;
        --port=*)
            BIND_PORT="${1#--port=}"; shift ;;
        --log)
            LOG_FILE="$2"; shift 2 ;;
        --log=*)
            LOG_FILE="${1#--log=}"; shift ;;
        --ssh)
            SSH_TARGET="$2"; shift 2 ;;
        --ssh=*)
            SSH_TARGET="${1#--ssh=}"; shift ;;
        --remote-cmd)
            REMOTE_CMD="$2"; shift 2 ;;
        --remote-cmd=*)
            REMOTE_CMD="${1#--remote-cmd=}"; shift ;;
        --help|-h)
            usage; exit 0 ;;
        *)
            echo "error: unknown argument: $1" >&2
            echo "Run with --help for usage." >&2
            exit 1 ;;
    esac
done

# ── cleanup trap ─────────────────────────────────────────────────────────────
cleanup() {
    echo ""
    echo "[m3os-logsink] stopping (INT/TERM received). Log is at: $LOG_FILE"
    exit 0
}
trap cleanup INT TERM

# ── Mode 2: SSH tail ─────────────────────────────────────────────────────────
if [[ -n "$SSH_TARGET" ]]; then
    echo "[m3os-logsink] Mode 2 — SSH tail"
    echo "[m3os-logsink]   target : $SSH_TARGET"
    echo "[m3os-logsink]   command: $REMOTE_CMD"
    echo "[m3os-logsink]   log    : $LOG_FILE"
    echo ""
    echo "[m3os-logsink] Press Ctrl+C to stop."
    # tee -a so we also see output on the terminal.
    ssh "$SSH_TARGET" "$REMOTE_CMD" | tee -a "$LOG_FILE"
    exit 0
fi

# ── Mode 1: UDP listener ─────────────────────────────────────────────────────
echo "[m3os-logsink] Mode 1 — UDP listener"
echo "[m3os-logsink]   bind port: $BIND_PORT"
echo "[m3os-logsink]   log file : $LOG_FILE"
echo ""
echo "[m3os-logsink] Point m3OS syslogd at THIS_HOST:${BIND_PORT}"
echo "[m3os-logsink]   e.g. add 'syslogd -R <this-host-ip>' to m3OS /etc/rc.local"
echo "              or pass -R <ip>:${BIND_PORT} to syslogd in your m3OS image."
echo "[m3os-logsink] Press Ctrl+C to stop."
echo ""

if command -v socat > /dev/null 2>&1; then
    echo "[m3os-logsink] using socat (preferred)"
    # Each datagram is written as a line; socat adds a newline.
    socat -u "UDP-RECVFROM:${BIND_PORT},fork" STDOUT | tee -a "$LOG_FILE"
elif command -v nc > /dev/null 2>&1; then
    echo "[m3os-logsink] socat not found — falling back to nc -ul"
    # nc -ul only reads a single datagram on many implementations; wrap in a
    # loop so we keep listening.  GNU netcat and ncat both support this idiom.
    while true; do
        nc -ul "$BIND_PORT" | tee -a "$LOG_FILE"
    done
else
    echo "error: neither 'socat' nor 'nc' (netcat) found in PATH." >&2
    echo "       Install one of:" >&2
    echo "         Arch/Manjaro : sudo pacman -S socat" >&2
    echo "         Debian/Ubuntu: sudo apt install socat" >&2
    echo "         Fedora       : sudo dnf install socat" >&2
    exit 1
fi
