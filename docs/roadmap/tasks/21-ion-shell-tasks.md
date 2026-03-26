# Phase 21 — Ion Shell Integration: Task List

**Depends on:** Phase 20 ✅, Phase 19 ✅, Phase 12 ✅
**Goal:** See [Phase 21 roadmap doc](../21-ion-shell.md) for full details.

> Task breakdown for this phase is pending. It will be expanded before implementation begins,
> following the same track layout used in previous phases (independent tracks A, B, C…
> that can be worked in parallel, with explicit dependency annotations).

## Placeholder Track Layout

| Track | Scope | Dependencies |
|---|---|---|
| A | Cross-compile ion to x86_64-unknown-linux-musl; vendor binary | — |
| B | xtask image build integration (copy ion to ramdisk at /bin/ion) | A |
| C | Update userspace/init to execve /bin/ion with /bin/sh0 fallback | B |
| D | Validation: QEMU boot tests for script mode and cooked interactive mode | B, C |

_Expand this file into detailed tasks (P21-T001…) before starting implementation._

## Related

- [Phase 21 Design Doc](../21-ion-shell.md)
- [Phase 20 Design Doc](../20-userspace-init-shell.md)
- [docs/shell/alternative-shells.md](../../shell/alternative-shells.md)
