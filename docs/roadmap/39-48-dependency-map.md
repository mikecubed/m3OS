# Remaining Phase Dependency Map (39-48)

This page trims the roadmap down to the still-planned phases starting at Phase 39.
The diagrams intentionally **exclude every phase before 39**, even when an earlier
phase is a real prerequisite, so the remaining internal dependency structure is easier
to see.

## Direct remaining-phase dependencies

```mermaid
flowchart TD
    P39["39<br/>Unix Domain Sockets"]
    P40["40<br/>Threading"]
    P41["41<br/>Expanded Coreutils"]
    P42["42<br/>Crypto Primitives"]
    P43["43<br/>SSH Server"]
    P44["44<br/>Rust Cross-Compilation"]
    P45["45<br/>Ports System"]
    P46["46<br/>System Services"]
    P47["47<br/>DOOM"]
    P48["48<br/>Mouse Input"]

    P42 --> P43
    P41 --> P45
    P39 --> P46
    P47 -. optional .-> P48
```

## Parallel work waves

```mermaid
flowchart LR
    subgraph W1["Wave 1: no blockers inside phases 39-48"]
        P39["39 Unix Domain Sockets"]
        P40["40 Threading"]
        P41["41 Expanded Coreutils"]
        P42["42 Crypto Primitives"]
        P44["44 Rust Cross-Compilation"]
        P47["47 DOOM"]
        P48["48 Mouse Input"]
    end

    subgraph W2["Wave 2: unlocked by Wave 1 work"]
        P43["43 SSH Server"]
        P45["45 Ports System"]
        P46["46 System Services"]
    end

    P42 --> P43
    P41 --> P45
    P39 --> P46
    P47 -. optional .-> P48
```

## Phase-by-phase parallelization view

| Phase | Remaining-phase blockers | Can start in parallel now? | Notes |
|---|---|---:|---|
| 39 | None | Yes | Main dependency inside the remaining set for Phase 46. |
| 40 | None | Yes | Isolated from the rest of 39-48 in the current roadmap. |
| 41 | None | Yes | Only blocks Phase 45 inside the remaining set. |
| 42 | None | Yes | Sole remaining prerequisite for Phase 43. |
| 43 | 42 | No | Can run once the crypto layer is ready. |
| 44 | None | Yes | Independent of the other remaining phases. |
| 45 | 41 | No | Depends on the broader coreutils expansion. |
| 46 | 39 | No | The roadmap links this through Unix-domain-socket-backed logging. |
| 47 | None | Yes | Independent showcase phase. |
| 48 | None required | Yes | Optional value increase if paired with Phase 47 for DOOM mouse aiming. |

## Practical parallel tracks

If the goal is to maximize parallel work after Phase 38, the cleanest split is:

1. **Kernel IPC/services track:** Phase 39, then Phase 46.
2. **Threading track:** Phase 40 by itself.
3. **Tooling track:** Phase 41, then Phase 45.
4. **Security/remote access track:** Phase 42, then Phase 43.
5. **Rust toolchain track:** Phase 44 by itself.
6. **Showcase/input track:** Phase 47 and Phase 48 can proceed independently, with optional integration afterward.

That means the best immediate parallel starters are **39, 40, 41, 42, 44, 47, and 48**.
