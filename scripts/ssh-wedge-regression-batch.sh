#!/usr/bin/env bash
# SSH-wedge regression — batch runner.
#
# Runs `scripts/ssh-wedge-regression.sh` N times sequentially and tallies
# outcomes. A 15-run batch is sufficient to detect regression of the
# SCHEDULER.lock ISR-deadlock class (pre-fix early-wedge rate was 30–40 %,
# so a 15-run all-clean streak's false-positive probability is < 0.01 %).
# See docs/post-mortems/2026-04-21-scheduler-lock-isr-deadlock.md.
#
# Usage:
#   scripts/ssh-wedge-regression-batch.sh <count> <prefix>
#
# Environment variables from the single-run harness are honored
# (ARTIFACT_DIR, BOOT_TIMEOUT_S, SSH_TIMEOUT_S, CONNECT_TIMEOUT_S, SSH_PORT).
#
# Writes <ARTIFACT_DIR>/ssh-wedge-batch-<prefix>.results (one summary per
# run) and prints the class tally at the end. Exits non-zero if any run
# classified as a wedge (early-wedge, late-wedge, or boot-failed).

set -u

N="${1:?count required}"
PREFIX="${2:?prefix required}"
ARTIFACT_DIR="${ARTIFACT_DIR:-${TMPDIR:-/tmp}}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

mkdir -p "$ARTIFACT_DIR"
RESULTS="${ARTIFACT_DIR}/ssh-wedge-batch-${PREFIX}.results"
: > "$RESULTS"

for i in $(seq 1 "$N"); do
    RUN_ID="${PREFIX}${i}"
    echo "=== run $i / $N (id=$RUN_ID) ===" | tee -a "$RESULTS"
    bash "${SCRIPT_DIR}/ssh-wedge-regression.sh" "$RUN_ID" 2>&1 | tee -a "$RESULTS"
    echo "" | tee -a "$RESULTS"
    # Brief delay between runs so the hostfwd port fully closes.
    sleep 2
done

echo ""
echo "=== Summary ==="
echo "Total runs: $N"
echo "Classification breakdown:"
grep "^class=" "$RESULTS" | sort | uniq -c | sort -rn

# Exit non-zero if any run wedged.
WEDGE_COUNT="$(grep -cE '^class=(early-wedge|late-wedge|boot-failed|unknown)$' "$RESULTS" || true)"
if [ "$WEDGE_COUNT" -gt 0 ]; then
    echo ""
    echo "FAIL: ${WEDGE_COUNT} wedge(s) detected in ${N} runs"
    exit 1
fi
