#!/usr/bin/env bash
# count_kernel_lines.sh — Reproducible kernel LOC audit for Phase 55b Track F.5
#
# Reports:
#   1. Raw LOC and code-only LOC for all kernel/src/**/*.rs files (current HEAD)
#   2. Phase 55 close baseline (commit 4539724, measured at phase close)
#   3. Net kernel LOC change since Phase 55 close (two views: total and driver-only)
#   4. Before/after counts for the two deleted drivers and their facades
#
# Usage: ./scripts/count_kernel_lines.sh
# Run from the repository root.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
KERNEL_SRC="${REPO_ROOT}/kernel/src"

# Phase 55 close commit (feat/phase-55a, v0.55.1 tag — last commit before 55b work)
PHASE55_COMMIT="4539724"

# ── Hardcoded Phase 55 close baselines (measured from ${PHASE55_COMMIT}) ─────
# nvme.rs: 1314 lines  (git show 4539724:kernel/src/blk/nvme.rs | wc -l)
# e1000.rs: 801 lines  (git show 4539724:kernel/src/net/e1000.rs | wc -l)
# These are the files deleted by D.5 and E.5 respectively.
PHASE55_NVME_LOC=1314
PHASE55_E1000_LOC=801
PHASE55_DELETED_COMBINED=$((PHASE55_NVME_LOC + PHASE55_E1000_LOC))   # 2115

# Total kernel/src raw and code-only LOC at Phase 55 close (measured from 4539724):
# find kernel/src -name '*.rs' | xargs wc -l | tail -1  =>  51252
# grep -v -E '^\s*(//|$)'                               =>  36223
PHASE55_TOTAL_LOC=51252
PHASE55_CODE_ONLY_LOC=36223

# Phase 55b kernel diff summary (git diff 4539724..HEAD --stat -- 'kernel/src/**'):
#   4152 insertions, 2235 deletions
#   Breakdown of insertions not related to driver deletion:
#     syscall/device_host.rs  +2204  (new device-host ABI layer)
#     main.rs                 +748   (device-host init, capability wiring)
#     pci/bar.rs              +306   (BAR access helpers for ring-3 MMIO)
#     net/remote.rs           +310   (RemoteNic facade)
#     blk/remote.rs           +208   (RemoteBlockDevice facade)
#     remaining               +376   (smaller changes across 12 files)
PHASE55B_KERNEL_INSERTIONS=4152
PHASE55B_KERNEL_DELETIONS=2235

# ── helpers ──────────────────────────────────────────────────────────────────

count_raw() {
    find "${KERNEL_SRC}" -name '*.rs' -print0 \
        | xargs -0 wc -l \
        | tail -1 \
        | awk '{print $1}'
}

count_code_only() {
    # Lines that are NOT blank and NOT single-line comments (//)
    find "${KERNEL_SRC}" -name '*.rs' -print0 \
        | xargs -0 cat \
        | grep -v -E '^\s*(//|$)' \
        | wc -l
}

count_file_raw() {
    local file="$1"
    if [[ -f "${file}" ]]; then
        wc -l < "${file}"
    else
        echo "0"
    fi
}

# ── measurements ─────────────────────────────────────────────────────────────

echo "================================================================"
echo "  m3OS kernel LOC audit — Phase 55b Track F.5"
echo "  $(date -u '+%Y-%m-%d %H:%M UTC')   HEAD: $(git rev-parse --short HEAD)"
echo "================================================================"
echo ""

echo "── Phase 55 close baseline (commit ${PHASE55_COMMIT}) ──────────────"
echo "  nvme.rs   (deleted by D.5): ${PHASE55_NVME_LOC} raw lines"
echo "  e1000.rs  (deleted by E.5): ${PHASE55_E1000_LOC} raw lines"
echo "  Combined deleted drivers   : ${PHASE55_DELETED_COMBINED} raw lines"
echo "  Total kernel/src raw LOC   : ${PHASE55_TOTAL_LOC}"
echo "  Total kernel/src code-only : ${PHASE55_CODE_ONLY_LOC}"
echo ""

echo "── Facade files at HEAD ─────────────────────────────────────────"
REMOTE_BLK="${KERNEL_SRC}/blk/remote.rs"
REMOTE_NET="${KERNEL_SRC}/net/remote.rs"

FACADE_BLK_LOC=$(count_file_raw "${REMOTE_BLK}")
FACADE_NET_LOC=$(count_file_raw "${REMOTE_NET}")
FACADE_COMBINED=$((FACADE_BLK_LOC + FACADE_NET_LOC))

echo "  kernel/src/blk/remote.rs   : ${FACADE_BLK_LOC} lines"
echo "  kernel/src/net/remote.rs   : ${FACADE_NET_LOC} lines"
echo "  Combined facades           : ${FACADE_COMBINED} lines"
echo ""

FACADE_TARGET=300
if [[ ${FACADE_COMBINED} -lt ${FACADE_TARGET} ]]; then
    echo "  [PASS] Combined facades (${FACADE_COMBINED}) meet the < ${FACADE_TARGET} line target."
else
    FACADE_OVERAGE=$((FACADE_COMBINED - FACADE_TARGET))
    echo "  [SCOPE NOTE] Task-doc target for combined facades: < ${FACADE_TARGET} lines."
    echo "  Actual: ${FACADE_COMBINED} lines — target MISSED by ${FACADE_OVERAGE} lines."
    echo "  net/remote.rs (${FACADE_NET_LOC} L) grew beyond ~150-line estimate due to"
    echo "  RX-routing dispatch and link-state machinery required for correct IPC semantics."
fi
echo ""

echo "── Current HEAD counts ──────────────────────────────────────────"
CURRENT_RAW=$(count_raw)
CURRENT_CODE=$(count_code_only)
echo "  kernel/src raw LOC   : ${CURRENT_RAW}"
echo "  kernel/src code-only : ${CURRENT_CODE}"
echo ""

echo "── Net kernel LOC change since Phase 55 close ───────────────────"
NET_RAW=$((CURRENT_RAW - PHASE55_TOTAL_LOC))
NET_CODE=$((CURRENT_CODE - PHASE55_CODE_ONLY_LOC))

# Driver-isolation view: deleted driver code vs replacement facades only
DRIVER_NET=$((FACADE_COMBINED - PHASE55_DELETED_COMBINED))

if [[ ${NET_RAW} -le 0 ]]; then
    echo "  Total raw delta       : ${NET_RAW}  (all kernel/src changes)"
else
    echo "  Total raw delta       : +${NET_RAW}  (all kernel/src changes)"
fi

if [[ ${NET_CODE} -le 0 ]]; then
    echo "  Total code-only delta : ${NET_CODE}  (excl. blank lines + // comments)"
else
    echo "  Total code-only delta : +${NET_CODE}  (excl. blank lines + // comments)"
fi

echo ""
echo "  Driver-isolation delta: ${DRIVER_NET}"
echo "    = facades (${FACADE_COMBINED} L) − deleted drivers (${PHASE55_DELETED_COMBINED} L)"
echo "    Phase 55b also added ${PHASE55B_KERNEL_INSERTIONS} L of new kernel infrastructure"
echo "    (device-host syscall layer, BAR helpers, init wiring) and removed"
echo "    ${PHASE55B_KERNEL_DELETIONS} L of old code — net total kernel delta is +${NET_RAW}."
echo ""

LOC_TARGET=-1800
if [[ ${NET_RAW} -le ${LOC_TARGET} ]]; then
    echo "  [PASS] Total raw delta (${NET_RAW}) meets the ≤ ${LOC_TARGET} target."
else
    echo "  [WARN] Total raw delta (+${NET_RAW}) does NOT meet the ≤ ${LOC_TARGET} target."
    echo "         The kernel grew because Phase 55b added the device-host ABI layer"
    echo "         (~2204 L syscall/device_host.rs) which is new ring-0 infrastructure."
    echo "         Driver-isolation view: ${DRIVER_NET} lines (drivers removed vs facades added)."
fi
echo ""

echo "── Deletion commit auditability ─────────────────────────────────"
echo "  Task-doc criterion: both nvme.rs and e1000.rs deleted in ONE change set."
echo "  Actual: deletions landed in two separate commits:"
D5_LOG=$(git log --oneline --all | grep 'feat(55b-d5)' | head -1)
E5_LOG=$(git log --oneline --all | grep 'feat(55b-e5)' | head -1)
echo "    D.5 (NVMe):  ${D5_LOG:-<not found in log>}"
echo "    E.5 (e1000): ${E5_LOG:-<not found in log>}"
echo "  [DEVIATION] Single-changeset criterion NOT met."
echo "  Separate commits preserve independent D.5/E.5 track history but break"
echo "  the single-audit-point criterion. Both commits are on the same branch."
echo ""

echo "── Top 30 kernel/src files by raw LOC (HEAD) ────────────────────"
find "${KERNEL_SRC}" -name '*.rs' -print0 \
    | xargs -0 wc -l \
    | sort -rn \
    | head -31
echo "================================================================"
