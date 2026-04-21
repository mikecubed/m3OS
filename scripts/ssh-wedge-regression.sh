#!/usr/bin/env bash
# SSH-wedge regression — single-run harness.
#
# Guards against regression of the SCHEDULER.lock ISR-deadlock class of bug
# fixed in commit ac37270 (see docs/post-mortems/2026-04-21-scheduler-lock-
# isr-deadlock.md). Boots the guest once, waits for sshd to listen on port
# 22, fires a single `ssh -o BatchMode=yes` attempt, and classifies the
# outcome.
#
# Usage:
#   scripts/ssh-wedge-regression.sh <run-id>
#
# Optional environment:
#   ARTIFACT_DIR   — where to write .log/.ssh/.summary files
#                    (default: ${TMPDIR:-/tmp}).
#   BOOT_TIMEOUT_S — seconds to wait for sshd to listen (default: 90).
#   SSH_TIMEOUT_S  — seconds to wait for the ssh attempt (default: 30).
#   CONNECT_TIMEOUT_S — passed to ssh -o ConnectTimeout (default: 20).
#   SSH_PORT       — host port forwarded to guest:22 (default: 2222).
#
# Outputs <ARTIFACT_DIR>/ssh-wedge-<run-id>.{log,ssh,summary}. Prints the
# summary to stdout and exits non-zero if sshd never listened.
#
# Classification (from the summary file):
#   clean-auth-rejected  — ssh exit 255 + "Permission denied". The expected
#                          path under BatchMode=yes. Indicates a healthy
#                          kernel.
#   clean-login          — ssh exit 0. Only happens if a real keyring matches,
#                          which BatchMode=yes does not enable.
#   early-wedge          — ssh exit 255 + "Connection timed out during banner
#                          exchange". The failure mode the post-mortem's
#                          fix closes.
#   late-wedge           — ssh exit 124 + "Permanently added". Pre-fix tail
#                          mis-classification; no longer observed.
#   unknown              — anything else. Inspect the .ssh and .log files.

set -u

RUN_ID="${1:?run id required}"
ARTIFACT_DIR="${ARTIFACT_DIR:-${TMPDIR:-/tmp}}"
BOOT_TIMEOUT_S="${BOOT_TIMEOUT_S:-90}"
SSH_TIMEOUT_S="${SSH_TIMEOUT_S:-30}"
CONNECT_TIMEOUT_S="${CONNECT_TIMEOUT_S:-20}"
SSH_PORT="${SSH_PORT:-2222}"

# Repo root = dir containing this script's parent.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

mkdir -p "$ARTIFACT_DIR"
LOG="${ARTIFACT_DIR}/ssh-wedge-${RUN_ID}.log"
SSH_LOG="${ARTIFACT_DIR}/ssh-wedge-${RUN_ID}.ssh"
SUMMARY="${ARTIFACT_DIR}/ssh-wedge-${RUN_ID}.summary"

: > "$LOG"
: > "$SSH_LOG"

cd "$REPO_ROOT"

setsid cargo xtask run > "$LOG" 2>&1 &
QPID=$!

# Wait for sshd to listen.
for _ in $(seq 1 "$BOOT_TIMEOUT_S"); do
    if grep -q "sshd: listening on port 22" "$LOG"; then
        break
    fi
    sleep 1
done

if ! grep -q "sshd: listening on port 22" "$LOG"; then
    {
        echo "run=${RUN_ID}"
        echo "ssh_exit=-1"
        echo "class=boot-failed"
        echo "log=${LOG}"
        echo "ssh_log=${SSH_LOG}"
    } > "$SUMMARY"
    cat "$SUMMARY"
    kill -TERM -- -"$QPID" 2>/dev/null || true
    sleep 2
    kill -KILL -- -"$QPID" 2>/dev/null || true
    wait "$QPID" 2>/dev/null || true
    exit 2
fi

# Small post-listen settling delay — the sshd listener log fires before the
# accept loop is parked on the listen socket, and firing the ssh attempt
# immediately sometimes loses the first SYN to a hostfwd race.
sleep 2

set +e
timeout "$SSH_TIMEOUT_S" ssh \
    -o StrictHostKeyChecking=no \
    -o UserKnownHostsFile=/dev/null \
    -o BatchMode=yes \
    -o "ConnectTimeout=${CONNECT_TIMEOUT_S}" \
    -p "$SSH_PORT" user@127.0.0.1 'exit' > "$SSH_LOG" 2>&1
SSH_EXIT=$?
set -e

# Let sshd flush its session-teardown logs before we tear QEMU down.
sleep 3

kill -TERM -- -"$QPID" 2>/dev/null || true
sleep 2
kill -KILL -- -"$QPID" 2>/dev/null || true
wait "$QPID" 2>/dev/null || true

# Classify.
CLASS=unknown
if [ "$SSH_EXIT" = "0" ]; then
    CLASS="clean-login"
elif grep -q "Permission denied" "$SSH_LOG"; then
    CLASS="clean-auth-rejected"
elif grep -q "Connection timed out during banner exchange" "$SSH_LOG"; then
    CLASS="early-wedge"
elif [ "$SSH_EXIT" = "124" ] && grep -q "Permanently added" "$SSH_LOG"; then
    CLASS="late-wedge"
fi

{
    echo "run=${RUN_ID}"
    echo "ssh_exit=${SSH_EXIT}"
    echo "class=${CLASS}"
    echo "log=${LOG}"
    echo "ssh_log=${SSH_LOG}"
} > "$SUMMARY"

cat "$SUMMARY"
