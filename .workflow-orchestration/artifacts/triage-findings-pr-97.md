Created: 2026-04-12T06:21:00.857Z
Review surface: PR #97 against main
Current PR head: `78dc89be337840cf8032d49fc770ac0ef734d3a7`
Review thread state: 0 unresolved threads

## Purpose

This note captures unresolved findings uncovered during PR #97 review resolution and readiness work so they can be triaged separately from the completed thread fixes.

## Open findings

| ID | Priority | Category | Source | Status | Finding |
| --- | --- | --- | --- | --- | --- |
| `pr97-smoke-test-login-prompt` | P1 | CI / correctness | GitHub `check` job `70948837404` | Open | Smoke test times out after username entry waiting for either `Set password for` or `Password:`. |
| `pr97-codeql-check-failure` | P1 | Code scanning | GitHub `CodeQL` check `70948866195` | Open | The CodeQL check on the pushed PR head still fails even though the per-language analysis jobs completed. |
| `exit-group-async-sibling-teardown` | P2 | Kernel architecture / correctness | Local review during PR resolution | Open, pre-existing | `exit_group()` sibling teardown is still asynchronous and can reap sibling `Process` entries before those threads are definitely quiesced. |
| `gha-node20-deprecation-warning` | P3 | CI maintenance | GitHub Actions log warning | Open | GitHub Actions emitted a Node.js 20 deprecation warning for several workflow actions. |

## Details

### `pr97-smoke-test-login-prompt`

- **Source:** `check` job `70948837404`
- **URL:** `https://github.com/mikecubed/m3OS/actions/runs/24298965327/job/70948837404`
- **Observed behavior:** Attempt 3/3 booted QEMU, reached the login flow, sent the username with a backspace correction, then timed out at step 3 waiting for either `Set password for` or `Password:`.
- **Last serial output:** `rooo\b \bt`
- **Triage hypothesis:** likely in the login / TTY / prompt-detection path rather than in the already-fixed review-thread changes themselves.
- **Suggested next step:** reproduce with the same smoke-test flow, inspect the login prompt path around first-boot password setup vs normal password prompt, and compare serial output with the harness expectations.

### `pr97-codeql-check-failure`

- **Source:** GitHub `CodeQL` check `70948866195`
- **URL:** `https://github.com/mikecubed/m3OS/runs/70948866195`
- **Observed behavior:** the top-level CodeQL check failed on the pushed PR head while `Analyze (rust)`, `Analyze (c-cpp)`, and `Analyze (actions)` all completed successfully.
- **Triage note:** the failure detail was not surfaced in the current CLI/API output gathered during review resolution, so the exact alert still needs to be identified from the GitHub check/code scanning UI or from an auth path that exposes code scanning alerts.
- **Suggested next step:** inspect the check details directly in GitHub, capture the concrete alert location/rule, then decide whether it is a real regression, a still-open issue, or a stale result.

### `exit-group-async-sibling-teardown`

- **Source:** local substantive review during PR thread resolution
- **Observed behavior:** sibling threads killed by `exit_group()` are still reaped asynchronously; the current design can remove a sibling `Process` entry before the sibling is guaranteed to have stopped running on another core.
- **Why this stayed open:** this concern predates the PR-thread fixes and was not expanded in the scoped review-resolution batch.
- **Suggested next step:** triage as separate kernel lifecycle work; likely requires either synchronous sibling quiescing or delayed reaping until the scheduler confirms the sibling is off-core.

### `gha-node20-deprecation-warning`

- **Source:** tail of the `check` job log
- **Observed behavior:** GitHub warned that `actions/cache@v4`, `actions/checkout@v4`, and `actions/upload-artifact@v4` are still running on Node.js 20 and will be forced onto Node.js 24 by default starting June 2, 2026.
- **Suggested next step:** audit the workflow action versions and update them to Node 24-compatible releases before the deprecation window closes.

## Immediate triage order

1. `pr97-smoke-test-login-prompt`
2. `pr97-codeql-check-failure`
3. `exit-group-async-sibling-teardown`
4. `gha-node20-deprecation-warning`
