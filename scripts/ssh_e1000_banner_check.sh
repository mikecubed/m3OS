#!/usr/bin/env bash
# SSH e1000 banner regression — single-run harness.
#
# Boots m3OS with the ring-3 e1000 driver enabled, waits for sshd to listen,
# captures the first 64 bytes of the server banner from host port 2222, and
# then performs a single BatchMode ssh handshake to confirm the connection
# reaches the authentication boundary without wedging.
#
# Usage:
#   scripts/ssh_e1000_banner_check.sh <run-id> [--timeout <secs>] [--display]
#
# Optional environment:
#   ARTIFACT_DIR       — where to write .log/.ssh/.banner/.summary files
#                        (default: <repo>/target/ssh-e1000-banner-check).
#   BANNER_TIMEOUT_S   — seconds to wait for the first banner bytes (default: 5).
#   SSH_TIMEOUT_S      — seconds to wait for the ssh attempt (default: 5).
#   CONNECT_TIMEOUT_S  — passed to ssh -o ConnectTimeout (default: 5).
#   SSH_PORT           — host port forwarded to guest:22 (default: 2222).
#
# Outputs <ARTIFACT_DIR>/ssh-e1000-banner-<run-id>.{log,ssh,banner,summary}.
# Prints the summary to stdout and exits non-zero on failure.

set -eu

RUN_ID=""
BOOT_TIMEOUT_S=30
DISPLAY=0

while [ "$#" -gt 0 ]; do
    case "$1" in
        --timeout)
            shift
            BOOT_TIMEOUT_S="${1:?--timeout requires a value}"
            ;;
        --display)
            DISPLAY=1
            ;;
        --help)
            echo "Usage: scripts/ssh_e1000_banner_check.sh <run-id> [--timeout <secs>] [--display]"
            exit 0
            ;;
        --*)
            echo "ssh_e1000_banner_check.sh: unknown option: $1" >&2
            exit 64
            ;;
        *)
            if [ -n "$RUN_ID" ]; then
                echo "ssh_e1000_banner_check.sh: unexpected extra argument: $1" >&2
                exit 64
            fi
            RUN_ID="$1"
            ;;
    esac
    shift
done

if [ -z "$RUN_ID" ]; then
    echo "ssh_e1000_banner_check.sh: run id required" >&2
    exit 64
fi

BANNER_TIMEOUT_S="${BANNER_TIMEOUT_S:-5}"
SSH_TIMEOUT_S="${SSH_TIMEOUT_S:-5}"
CONNECT_TIMEOUT_S="${CONNECT_TIMEOUT_S:-5}"
SSH_PORT="${SSH_PORT:-2222}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ARTIFACT_DIR="${ARTIFACT_DIR:-$REPO_ROOT/target/ssh-e1000-banner-check}"

mkdir -p "$ARTIFACT_DIR"
LOG="${ARTIFACT_DIR}/ssh-e1000-banner-${RUN_ID}.log"
SSH_LOG="${ARTIFACT_DIR}/ssh-e1000-banner-${RUN_ID}.ssh"
BANNER_LOG="${ARTIFACT_DIR}/ssh-e1000-banner-${RUN_ID}.banner"
SUMMARY="${ARTIFACT_DIR}/ssh-e1000-banner-${RUN_ID}.summary"
KNOWN_HOSTS="${ARTIFACT_DIR}/ssh-e1000-banner-${RUN_ID}.known_hosts"

: > "$LOG"
: > "$SSH_LOG"
: > "$BANNER_LOG"
: > "$KNOWN_HOSTS"

cd "$REPO_ROOT"

QPID=""
cleanup() {
    if [ -n "$QPID" ]; then
        kill -TERM -- "-$QPID" 2>/dev/null || true
        sleep 2
        kill -KILL -- "-$QPID" 2>/dev/null || true
        wait "$QPID" 2>/dev/null || true
        QPID=""
    fi
}
trap cleanup EXIT

sshd_listening() {
    grep -q "sshd: listening on port 22" "$LOG" || grep -q "sshd: listening on :22" "$LOG"
}

if [ "$DISPLAY" -eq 1 ]; then
    setsid cargo xtask run-gui --device e1000 > "$LOG" 2>&1 &
else
    setsid cargo xtask run --device e1000 > "$LOG" 2>&1 &
fi
QPID=$!

for _ in $(seq 1 "$BOOT_TIMEOUT_S"); do
    if sshd_listening; then
        break
    fi
    sleep 1
done

if ! sshd_listening; then
    {
        echo "run=${RUN_ID}"
        echo "banner_exit=-1"
        echo "ssh_exit=-1"
        echo "class=boot-failed"
        echo "log=${LOG}"
        echo "ssh_log=${SSH_LOG}"
        echo "banner_log=${BANNER_LOG}"
    } > "$SUMMARY"
    cat "$SUMMARY"
    exit 2
fi

sleep 1

set +e
python - "$SSH_PORT" "$BANNER_TIMEOUT_S" "$BANNER_LOG" <<'PY'
import pathlib
import socket
import sys

port = int(sys.argv[1])
timeout = float(sys.argv[2])
out_path = pathlib.Path(sys.argv[3])
sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
sock.settimeout(timeout)
try:
    sock.connect(("127.0.0.1", port))
    chunks = []
    size = 0
    while size < 64:
        chunk = sock.recv(64 - size)
        if not chunk:
            break
        chunks.append(chunk)
        size += len(chunk)
        if b"\n" in chunk:
            break
    data = b"".join(chunks)
except socket.timeout:
    sys.exit(3)
except OSError as exc:
    out_path.write_text(f"connect-error={exc}\n", encoding="utf-8")
    sys.exit(4)
finally:
    sock.close()

out_path.write_bytes(data)
if not data:
    sys.exit(3)
sys.exit(0)
PY
BANNER_EXIT=$?
set -e

if [ "$BANNER_EXIT" -eq 3 ]; then
    {
        echo "run=${RUN_ID}"
        echo "banner_exit=${BANNER_EXIT}"
        echo "ssh_exit=-1"
        echo "class=banner-timeout"
        echo "log=${LOG}"
        echo "ssh_log=${SSH_LOG}"
        echo "banner_log=${BANNER_LOG}"
    } > "$SUMMARY"
    cat "$SUMMARY"
    exit 3
fi

if [ "$BANNER_EXIT" -ne 0 ]; then
    {
        echo "run=${RUN_ID}"
        echo "banner_exit=${BANNER_EXIT}"
        echo "ssh_exit=-1"
        echo "class=banner-connect-failed"
        echo "log=${LOG}"
        echo "ssh_log=${SSH_LOG}"
        echo "banner_log=${BANNER_LOG}"
    } > "$SUMMARY"
    cat "$SUMMARY"
    exit 4
fi

if ! LC_ALL=C grep -aq "SSH-2.0-" "$BANNER_LOG"; then
    {
        echo "run=${RUN_ID}"
        echo "banner_exit=${BANNER_EXIT}"
        echo "ssh_exit=-1"
        echo "class=invalid-banner-prefix"
        echo "log=${LOG}"
        echo "ssh_log=${SSH_LOG}"
        echo "banner_log=${BANNER_LOG}"
    } > "$SUMMARY"
    cat "$SUMMARY"
    exit 4
fi

set +e
timeout "$SSH_TIMEOUT_S" ssh \
    -o StrictHostKeyChecking=no \
    -o UserKnownHostsFile="$KNOWN_HOSTS" \
    -o BatchMode=yes \
    -o PreferredAuthentications=none \
    -o PubkeyAuthentication=no \
    -o PasswordAuthentication=no \
    -o KbdInteractiveAuthentication=no \
    -o "ConnectTimeout=${CONNECT_TIMEOUT_S}" \
    -p "$SSH_PORT" root@127.0.0.1 'exit' > "$SSH_LOG" 2>&1
SSH_EXIT=$?
set -e

sleep 1

CLASS="unknown"
EXIT_CODE=5
if [ "$SSH_EXIT" = "0" ]; then
    CLASS="clean-login"
    EXIT_CODE=0
elif grep -q "Permission denied" "$SSH_LOG" \
    || grep -q "No more authentication methods to try" "$SSH_LOG" \
    || grep -q "Authentications that can continue" "$SSH_LOG"; then
    CLASS="clean-auth-rejected"
    EXIT_CODE=0
elif grep -q "Connection timed out during banner exchange" "$SSH_LOG"; then
    CLASS="ssh-banner-timeout"
elif [ "$SSH_EXIT" = "124" ]; then
    CLASS="ssh-timeout"
else
    CLASS="ssh-failed"
fi

{
    echo "run=${RUN_ID}"
    echo "banner_exit=${BANNER_EXIT}"
    echo "ssh_exit=${SSH_EXIT}"
    echo "class=${CLASS}"
    echo "log=${LOG}"
    echo "ssh_log=${SSH_LOG}"
    echo "banner_log=${BANNER_LOG}"
} > "$SUMMARY"

cat "$SUMMARY"
exit "$EXIT_CODE"
