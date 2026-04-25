#!/usr/bin/env bash
# Boot m3OS with the chosen NIC driver, ssh in, run a command, send Ctrl-D,
# verify the session disconnects cleanly. Tests both ring-3 e1000 and
# virtio-net paths exercise the same plumbing as smoke-test plus a real
# interactive logout.
#
# Usage: ssh_full_session_test.sh <driver: e1000|virtio>

set -u

DRIVER="${1:?driver required (e1000 or virtio)}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARTIFACT_DIR="${ARTIFACT_DIR:-/tmp/m3os-ssh-full-${DRIVER}}"
mkdir -p "$ARTIFACT_DIR"
LOG="$ARTIFACT_DIR/qemu.log"
SESSION_LOG="$ARTIFACT_DIR/ssh-session.log"
SSH_PORT="${SSH_PORT:-2222}"
BOOT_TIMEOUT_S=45

cd /home/mikecubed/projects/ostest-wt-int-phase55c

case "$DRIVER" in
    e1000)
        QEMU_ARGS="--device e1000"
        ;;
    virtio)
        QEMU_ARGS=""
        ;;
    *)
        echo "Unknown driver: $DRIVER" >&2
        exit 64
        ;;
esac

echo "=== Booting m3OS with $DRIVER ===" >&2
QPID=""
cleanup() {
    if [ -n "$QPID" ]; then
        kill -TERM -- "-$QPID" 2>/dev/null || true
        sleep 2
        kill -KILL -- "-$QPID" 2>/dev/null || true
        wait "$QPID" 2>/dev/null || true
    fi
}
trap cleanup EXIT

setsid cargo xtask run $QEMU_ARGS > "$LOG" 2>&1 &
QPID=$!

# Wait for sshd to listen
for _ in $(seq 1 "$BOOT_TIMEOUT_S"); do
    if grep -q "sshd: listening on" "$LOG"; then
        break
    fi
    sleep 1
done

if ! grep -q "sshd: listening on" "$LOG"; then
    echo "RESULT=boot-failed" >&2
    tail -30 "$LOG" >&2
    exit 2
fi
echo "=== sshd listening, attempting interactive session ===" >&2
sleep 2

# Now drive an interactive session via Python+pexpect:
#   - ssh root@127.0.0.1 -p 2222
#   - send password "root"
#   - wait for prompt
#   - run `whoami`
#   - verify "root" output
#   - send Ctrl-D (EOF)
#   - expect ssh client to exit cleanly with status 0
python3 - "$SSH_PORT" "$SESSION_LOG" <<'PY'
import os
import pty
import select
import signal
import subprocess
import sys
import time

port, session_log_path = sys.argv[1], sys.argv[2]
session_log = open(session_log_path, "w")

def log(msg):
    session_log.write(msg + "\n")
    session_log.flush()
    print(msg, file=sys.stderr, flush=True)

# Use a pseudo-terminal so ssh's password prompt works.
pid, fd = pty.fork()
if pid == 0:
    os.execvp("ssh", [
        "ssh",
        "-o", "StrictHostKeyChecking=no",
        "-o", "UserKnownHostsFile=/dev/null",
        "-o", "ConnectTimeout=10",
        "-p", port,
        "root@127.0.0.1",
    ])
    os._exit(127)

def read_until(needle, timeout=10):
    deadline = time.time() + timeout
    buf = b""
    while time.time() < deadline:
        r, _, _ = select.select([fd], [], [], 0.5)
        if fd in r:
            try:
                chunk = os.read(fd, 4096)
            except OSError:
                break
            if not chunk:
                break
            buf += chunk
            session_log.write(chunk.decode("utf-8", errors="replace"))
            session_log.flush()
            if needle.encode() in buf:
                return True, buf
    return False, buf

def write(s):
    os.write(fd, s.encode())
    session_log.write(f"<<< {s!r}\n")
    session_log.flush()

# 1. Wait for password prompt
log("waiting for password prompt...")
ok, buf = read_until("password:", timeout=15)
if not ok:
    log(f"FAIL: no password prompt; buf={buf[-256:]!r}")
    sys.exit(3)

# 2. Send password
log("sending password 'root'")
write("root\n")

# 3. Wait for shell prompt
log("waiting for shell prompt...")
ok, buf = read_until("#", timeout=15)
if not ok:
    log(f"FAIL: no shell prompt; buf={buf[-256:]!r}")
    sys.exit(4)

# 4. Run whoami
log("running whoami")
write("/bin/whoami\n")
ok, buf = read_until("root", timeout=10)
if not ok:
    log(f"FAIL: whoami did not return 'root'; buf={buf[-256:]!r}")
    sys.exit(5)

# 5. Wait for prompt to return
ok, buf = read_until("#", timeout=10)
if not ok:
    log(f"FAIL: no prompt after whoami")
    sys.exit(6)

# 6. Run ls to make sure session is still responsive
log("running ls /bin to test responsiveness")
write("/bin/ls /bin\n")
ok, buf = read_until("#", timeout=10)
if not ok:
    log(f"FAIL: ls hung")
    sys.exit(7)

# 7. Send Ctrl-D (EOF) and expect clean disconnect
log("sending Ctrl-D (EOF)")
os.write(fd, b"\x04")  # ASCII EOT = Ctrl-D

# Wait for child to exit cleanly within 10 seconds
deadline = time.time() + 10
while time.time() < deadline:
    pid_done, status = os.waitpid(pid, os.WNOHANG)
    if pid_done == pid:
        # Drain remaining output
        try:
            while True:
                r, _, _ = select.select([fd], [], [], 0.1)
                if fd not in r: break
                chunk = os.read(fd, 4096)
                if not chunk: break
                session_log.write(chunk.decode("utf-8", errors="replace"))
        except OSError:
            pass
        if os.WIFEXITED(status):
            code = os.WEXITSTATUS(status)
            log(f"ssh client exited with status {code}")
            if code == 0:
                log("PASS: clean disconnect via Ctrl-D")
                sys.exit(0)
            else:
                log(f"FAIL: ssh client exited with non-zero status {code}")
                sys.exit(8)
        else:
            log(f"FAIL: ssh client did not exit normally; status={status}")
            sys.exit(9)
    # Read any remaining output while waiting
    r, _, _ = select.select([fd], [], [], 0.5)
    if fd in r:
        try:
            chunk = os.read(fd, 4096)
            if chunk:
                session_log.write(chunk.decode("utf-8", errors="replace"))
                session_log.flush()
        except OSError:
            pass

log("FAIL: ssh client did not exit within 10 seconds after Ctrl-D — HUNG")
os.kill(pid, signal.SIGKILL)
sys.exit(10)
PY

PY_EXIT=$?
echo "=== Python exit: $PY_EXIT ===" >&2
echo "=== Session log: $SESSION_LOG ===" >&2
exit $PY_EXIT
