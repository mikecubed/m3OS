#!/usr/bin/env bash
# SSH less-hang regression — single-run harness.
#
# Reproduces the user-reported "less over SSH hangs and q doesn't quit;
# kill -9 from a separate session doesn't kill it either" symptom.
#
# Boots m3OS with sshd listening on host port 2222, opens an interactive
# SSH session with pexpect, runs `less /etc/passwd`, asserts the first
# line renders, sends `q`, and asserts the shell prompt comes back
# within a bounded timeout.  If q hangs, opens a SECOND SSH session and
# tries `kill -9 <less-pid>` to verify the cross-session SIGKILL path.
#
# Usage:
#   scripts/ssh_less_hang_check.sh <run-id> [--timeout <secs>] [--display]
#
# Exit codes:
#   0   — `q` quit less within timeout AND second SSH ran clean
#   2   — boot/sshd-listen timeout
#   3   — first SSH could not authenticate
#   4   — `q` did NOT quit less (PRIMARY BUG repro)
#   5   — `kill -9` from second session did NOT kill less (SECONDARY BUG)
#   6   — python/pexpect missing

set -eu

RUN_ID=""
BOOT_TIMEOUT_S=60
DISPLAY=0
MODE="q"

while [ "$#" -gt 0 ]; do
    case "$1" in
        --timeout) shift; BOOT_TIMEOUT_S="${1:?--timeout requires a value}";;
        --display) DISPLAY=1;;
        --mode) shift; MODE="${1:?--mode requires a value}";;
        --help) echo "Usage: $0 <run-id> [--timeout <secs>] [--display] [--mode q|kill]"; exit 0;;
        --*) echo "$0: unknown option: $1" >&2; exit 64;;
        *) RUN_ID="$1";;
    esac
    shift
done

if [ -z "$RUN_ID" ]; then
    echo "$0: run id required" >&2; exit 64
fi

SSH_PORT="${SSH_PORT:-2222}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ARTIFACT_DIR="${ARTIFACT_DIR:-$REPO_ROOT/target/ssh-less-hang-check}"

mkdir -p "$ARTIFACT_DIR"
LOG="${ARTIFACT_DIR}/run-${RUN_ID}.log"
PEXPECT_LOG="${ARTIFACT_DIR}/run-${RUN_ID}.pexpect"
SUMMARY="${ARTIFACT_DIR}/run-${RUN_ID}.summary"
KNOWN_HOSTS="${ARTIFACT_DIR}/run-${RUN_ID}.known_hosts"

: > "$LOG"; : > "$PEXPECT_LOG"; : > "$KNOWN_HOSTS"

cd "$REPO_ROOT"

PYTHON_BIN="$(command -v python3 || true)"
if [ -z "$PYTHON_BIN" ]; then
    echo "class=missing-python" > "$SUMMARY"; cat "$SUMMARY"; exit 6
fi
if ! "$PYTHON_BIN" -c "import pexpect" 2>/dev/null; then
    echo "class=missing-pexpect" > "$SUMMARY"; cat "$SUMMARY"; exit 6
fi

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
    if sshd_listening; then break; fi
    sleep 1
done

if ! sshd_listening; then
    {
        echo "run=${RUN_ID}"
        echo "class=boot-failed"
        echo "log=${LOG}"
    } > "$SUMMARY"
    cat "$SUMMARY"; exit 2
fi

sleep 2

set +e
"$PYTHON_BIN" - "$SSH_PORT" "$PEXPECT_LOG" "$KNOWN_HOSTS" "$MODE" <<'PY'
import os, sys, pexpect, time

port = sys.argv[1]
log_path = sys.argv[2]
known_hosts = sys.argv[3]
mode = sys.argv[4] if len(sys.argv) > 4 else "q"

logf = open(log_path, "w", buffering=1)

def ssh_session(cmd_label):
    """Open an interactive root SSH session.  Returns a pexpect.spawn."""
    cmd = (
        f"ssh -o StrictHostKeyChecking=no "
        f"-o UserKnownHostsFile={known_hosts} "
        f"-o GlobalKnownHostsFile=/dev/null "
        f"-o ConnectTimeout=10 "
        f"-p {port} root@127.0.0.1"
    )
    child = pexpect.spawn(cmd, encoding="utf-8", timeout=30, logfile=logf)
    child.expect_exact("password:")
    child.sendline("root")
    # First-prompt detection: the m3OS shell prints `# ` (root) or
    # `root@m3os:/# `.  Send a newline to force a fresh prompt in case
    # the initial one was lost in sshd's diagnostic spew.
    time.sleep(1)
    child.sendline("")
    child.expect(r"# ", timeout=30)
    logf.write(f"\n=== [{cmd_label}] LOGGED IN ===\n")
    logf.flush()
    return child

# --- Phase 1: `q` quits less ---
PRIMARY_BUG = "q-does-not-quit-less"
SECONDARY_BUG = "sigkill-does-not-kill-less"
result_class = "unknown"
exit_code = 1

try:
    s1 = ssh_session("primary")
    s1.sendline("less /etc/passwd")
    # less renders the file; root: prefix appears in the first line.
    s1.expect("root:", timeout=15)
    logf.write("\n=== less rendered first line ===\n"); logf.flush()
    # Wait a beat so less has finished entering raw mode.
    time.sleep(0.5)

    if mode == "kill":
        # Explicit kill-and-recover path: don't send q, force-kill less
        # from a sibling session, then verify the primary session's
        # echo / line-buffering / prompt have recovered (termios
        # auto-recovery on foreground-process raw-mode exit).
        logf.write("\n=== mode=kill: skipping q, opening rescue session ===\n")
        logf.flush()
        s2 = ssh_session("rescue")
        s2.sendline("for d in /proc/[0-9]*; do echo $d $(cat $d/comm 2>/dev/null); done")
        s2.expect(r"# ", timeout=15)
        ps_text = s2.before
        logf.write(f"\n=== rescue /proc/*/comm output ===\n{ps_text}\n")
        logf.flush()
        less_pid = None
        for line in ps_text.splitlines():
            if " less" in line or line.endswith(" less"):
                parts = line.split()
                if parts and parts[0].startswith("/proc/"):
                    less_pid = parts[0].split("/")[2]
                    break
        if less_pid is None:
            result_class = "kill-mode:pid-not-found"
            exit_code = 5
        else:
            logf.write(f"\n=== rescue: kill -9 {less_pid} ===\n")
            s2.sendline(f"kill -9 {less_pid}")
            s2.expect(r"# ", timeout=10)
            time.sleep(2)
            # Confirm less is dead.
            s2.sendline(f"cat /proc/{less_pid}/comm 2>&1; echo CHECK_DONE")
            s2.expect("CHECK_DONE", timeout=10)
            check_out = s2.before
            logf.write(f"\n=== check after kill ===\n{check_out}\n")
            if "less" in check_out:
                result_class = "kill-mode:less-not-killed"
                exit_code = 5
            else:
                # less is dead; now verify primary session recovered.
                # Send a unique command and assert echo + prompt return.
                marker = "TERMIOS_RECOVERED_OK"
                s1.sendline(f"echo {marker}")
                try:
                    s1.expect(marker, timeout=10)
                    s1.expect(r"# ", timeout=5)
                    result_class = "kill-mode:session-recovered"
                    exit_code = 0
                    logf.write("\n=== PASS: termios auto-recovered after kill -9 ===\n")
                except pexpect.TIMEOUT:
                    result_class = "kill-mode:session-still-hung"
                    exit_code = 5
                    logf.write(f"\n=== FAIL: session still hung after kill; buffer={s1.buffer!r}\n")
        try:
            s2.sendline("exit"); time.sleep(0.5); s2.close(force=True)
        except Exception: pass
        try:
            s1.sendline("exit"); time.sleep(0.5); s1.close(force=True)
        except Exception: pass
        # kill-mode owns its assertion outcome; skip the q-path below.
        raise SystemExit(exit_code)

    # mode=q (default): assert q quits less cleanly.
    # Send q (no newline; less is in raw mode and reads single byte).
    s1.send("q")
    logf.write("\n=== sent q to less ===\n"); logf.flush()
    # Within 5s, the shell prompt should come back.
    try:
        s1.expect(r"# ", timeout=5)
        result_class = "less-quits-cleanly"
        exit_code = 0
        # Drop the session politely.
        s1.sendline("exit"); time.sleep(1); s1.close(force=True)
        logf.write("\n=== PASS: q quit less and shell prompt returned ===\n")
    except pexpect.TIMEOUT:
        logf.write("\n=== HANG: q did not return shell prompt within 5s ===\n")
        logf.flush()
        result_class = PRIMARY_BUG
        exit_code = 4
        # --- Phase 2: cross-session kill -9 ---
        try:
            # Open a second SSH session.
            s2 = ssh_session("rescue")
            # Use ps to find less's pid.  m3OS may not have pgrep.  Print
            # /proc/*/comm contents and match "less".  Fallback: scan
            # /proc/<pid>/cmdline.
            s2.sendline("for d in /proc/[0-9]*; do echo $d $(cat $d/comm 2>/dev/null); done")
            s2.expect(r"# ", timeout=10)
            ps_text = s2.before
            logf.write(f"\n=== /proc/*/comm output ===\n{ps_text}\n")
            logf.flush()
            less_pid = None
            for line in ps_text.splitlines():
                if " less" in line or line.endswith(" less"):
                    parts = line.split()
                    if parts:
                        path = parts[0]
                        if path.startswith("/proc/"):
                            less_pid = path.split("/")[2]
                            break
            if less_pid is None:
                logf.write("\n=== could not find less pid via /proc ===\n")
                result_class = SECONDARY_BUG + ":pid-not-found"
                exit_code = 5
            else:
                logf.write(f"\n=== killing less pid={less_pid} ===\n")
                logf.flush()
                s2.sendline(f"kill -9 {less_pid}")
                s2.expect(r"# ", timeout=5)
                # Confirm less is gone.
                time.sleep(1)
                s2.sendline(f"cat /proc/{less_pid}/status 2>&1; echo SENTINEL_DONE")
                s2.expect("SENTINEL_DONE", timeout=10)
                after_kill = s2.before
                logf.write(f"\n=== status after kill ===\n{after_kill}\n")
                logf.flush()
                if "No such file" in after_kill or "ENOENT" in after_kill or "does not exist" in after_kill:
                    result_class = "kill-killed-but-q-bug-remains"
                    # Primary bug still holds: q didn't quit but kill works.
                    exit_code = 4
                else:
                    # /proc/<pid>/status still readable → less still alive.
                    result_class = SECONDARY_BUG
                    exit_code = 5
            s2.sendline("exit"); time.sleep(1); s2.close(force=True)
        except Exception as e:
            logf.write(f"\n=== rescue session failed: {e!r} ===\n")
            result_class = SECONDARY_BUG + ":rescue-failed"
            exit_code = 5
        finally:
            try:
                s1.close(force=True)
            except Exception:
                pass
except pexpect.EOF as e:
    logf.write(f"\n=== unexpected EOF: {e!r} ===\n")
    result_class = "ssh-eof"
    exit_code = 3
except pexpect.TIMEOUT as e:
    logf.write(f"\n=== auth timeout: {e!r} ===\n")
    result_class = "auth-timeout"
    exit_code = 3
except Exception as e:
    logf.write(f"\n=== fatal: {e!r} ===\n")
    result_class = "fatal"
    exit_code = 7

logf.flush()
logf.close()

print(f"result_class={result_class}")
sys.exit(exit_code)
PY
PY_EXIT=$?
set -e

CLASS="$(tail -1 "$PEXPECT_LOG" 2>/dev/null || true)"

{
    echo "run=${RUN_ID}"
    echo "py_exit=${PY_EXIT}"
    echo "boot_log=${LOG}"
    echo "pexpect_log=${PEXPECT_LOG}"
} > "$SUMMARY"
cat "$SUMMARY"
exit "$PY_EXIT"
