#!/usr/bin/env bash
# Like ssh_full_session_test.sh, but disconnects via "exit" instead of Ctrl-D
# to isolate whether logout works at all vs whether Ctrl-D specifically is broken.

set -u
DRIVER="${1:?driver required}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARTIFACT_DIR="${ARTIFACT_DIR:-/tmp/m3os-ssh-exit-${DRIVER}}"
mkdir -p "$ARTIFACT_DIR"
LOG="$ARTIFACT_DIR/qemu.log"
SESSION_LOG="$ARTIFACT_DIR/ssh-session.log"
SSH_PORT="${SSH_PORT:-2222}"
BOOT_TIMEOUT_S=45

cd /home/mikecubed/projects/ostest-wt-int-phase55c

case "$DRIVER" in
    e1000) QEMU_ARGS="--device e1000" ;;
    virtio) QEMU_ARGS="" ;;
    *) echo "Unknown driver: $DRIVER" >&2; exit 64 ;;
esac

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

for _ in $(seq 1 "$BOOT_TIMEOUT_S"); do
    if grep -q "sshd: listening on" "$LOG"; then break; fi
    sleep 1
done

if ! grep -q "sshd: listening on" "$LOG"; then
    echo "RESULT=boot-failed" >&2
    exit 2
fi
sleep 2

python3 - "$SSH_PORT" "$SESSION_LOG" "$2" <<'PY'
import os, pty, select, signal, sys, time
port, session_log_path, mode = sys.argv[1], sys.argv[2], sys.argv[3]
session_log = open(session_log_path, "w")
def log(msg):
    session_log.write(msg + "\n"); session_log.flush()
    print(msg, file=sys.stderr, flush=True)

pid, fd = pty.fork()
if pid == 0:
    os.execvp("ssh", ["ssh", "-o", "StrictHostKeyChecking=no",
                      "-o", "UserKnownHostsFile=/dev/null",
                      "-o", "ConnectTimeout=10", "-p", port,
                      "root@127.0.0.1"])
    os._exit(127)

def read_until(needle, timeout=10):
    deadline = time.time() + timeout
    buf = b""
    while time.time() < deadline:
        r, _, _ = select.select([fd], [], [], 0.5)
        if fd in r:
            try: chunk = os.read(fd, 4096)
            except OSError: break
            if not chunk: break
            buf += chunk
            session_log.write(chunk.decode("utf-8", errors="replace")); session_log.flush()
            if needle.encode() in buf: return True, buf
    return False, buf

def write(s): os.write(fd, s.encode())

ok, _ = read_until("password:", 15)
if not ok: log("FAIL: no password prompt"); sys.exit(3)
write("root\n")
ok, _ = read_until("#", 60)
if not ok: log("FAIL: no shell prompt"); sys.exit(4)

# Disconnect using requested mode
if mode == "exit":
    log("sending 'exit\\n'")
    write("exit\n")
elif mode == "ctrld":
    log("sending Ctrl-D")
    os.write(fd, b"\x04")
elif mode == "exit-ctrld":
    log("sending 'exit\\n' then Ctrl-D as backup")
    write("exit\n")
    time.sleep(1)
    os.write(fd, b"\x04")
else:
    log(f"unknown mode {mode}"); sys.exit(99)

deadline = time.time() + 15
while time.time() < deadline:
    pid_done, status = os.waitpid(pid, os.WNOHANG)
    if pid_done == pid:
        try:
            while True:
                r, _, _ = select.select([fd], [], [], 0.1)
                if fd not in r: break
                chunk = os.read(fd, 4096)
                if not chunk: break
                session_log.write(chunk.decode("utf-8", errors="replace"))
        except OSError: pass
        code = os.WEXITSTATUS(status) if os.WIFEXITED(status) else -1
        log(f"ssh exited with status {code}")
        sys.exit(0 if code == 0 else 8)
    r, _, _ = select.select([fd], [], [], 0.5)
    if fd in r:
        try:
            chunk = os.read(fd, 4096)
            if chunk: session_log.write(chunk.decode("utf-8", errors="replace")); session_log.flush()
        except OSError: pass

log(f"FAIL: ssh client hung after {mode}")
os.kill(pid, signal.SIGKILL)
sys.exit(10)
PY
exit $?
