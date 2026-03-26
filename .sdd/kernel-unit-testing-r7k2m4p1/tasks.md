# Tasks: Kernel Unit Testing Infrastructure

**Input**: Design documents from `.sdd/kernel-unit-testing-r7k2m4p1/`
**Prerequisites**: plan.md (required), spec.md (required for user stories)

## Format: `- [ ] T### [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2, US3)
- Include exact file paths in descriptions

## Phase 1: Setup — kernel-core Crate Scaffolding

**Purpose**: Create the `kernel-core` crate and wire it into the workspace

- [x] T001 Create `kernel-core/Cargo.toml` with `no_std` + `alloc` + `spin` dependencies
- [x] T002 Create `kernel-core/src/lib.rs` with `#![no_std]`, `extern crate alloc`, and module declarations (initially empty stubs)
- [x] T003 Add `kernel-core` to workspace members in `Cargo.toml`
- [x] T004 Add `kernel-core` as a dependency in `kernel/Cargo.toml`

**Checkpoint**: `cargo build -p kernel-core --target x86_64-unknown-linux-gnu` compiles (empty crate)

---

## Phase 2: Extract Shared Types

**Purpose**: Define foundational ID and address types in `kernel-core` so dependent modules can reference them

**Note**: `TaskId`, `EndpointId`, and `NotifId` are **newtype structs** (not type aliases) with `#[derive(Debug, Clone, Copy, PartialEq, Eq)]`. They must be moved as-is.

- [x] T005 Create `kernel-core/src/types.rs` with newtype structs: `TaskId(pub u64)`, `EndpointId(pub u8)`, `NotifId(pub u8)`, and type aliases: `MacAddr = [u8; 6]`, `Ipv4Addr = [u8; 4]`
- [x] T006 Re-export all shared types from `kernel-core/src/lib.rs`
- [x] T007 Update `kernel/src/task/mod.rs` to use `pub use kernel_core::types::TaskId` instead of local definition
- [x] T008 Update `kernel/src/ipc/endpoint.rs` to use `pub use kernel_core::types::EndpointId` instead of local definition
- [x] T009 Update `kernel/src/ipc/notification.rs` to use `kernel_core::types::NotifId` instead of local definition
- [x] T010 Update `kernel/src/ipc/mod.rs` re-exports to pull `EndpointId`, `NotifId` from kernel-core (via endpoint/notification re-exports)
- [x] T011 Update `kernel/src/net/arp.rs` to use `pub use kernel_core::types::Ipv4Addr` instead of local definition
- [x] T012 Update `kernel/src/net/virtio_net.rs` to use `pub use kernel_core::types::MacAddr` instead of local definition
- [x] T013 Update `kernel/src/net/ethernet.rs` to import `MacAddr` from new location
- [x] T014 Update remaining net modules (`udp.rs`, `config.rs`, `tcp.rs`) that import `Ipv4Addr` from `super::arp`

**Checkpoint**: `cargo xtask check` passes — kernel compiles with shared types from `kernel-core`

---

## Phase 3: User Story 1 — Extract Pure-Logic Modules (Priority: P1) MVP

**Goal**: Move pure-logic code into `kernel-core` and make kernel re-export it, enabling host-side `cargo test`

**Independent Test**: `cargo test -p kernel-core --target x86_64-unknown-linux-gnu` runs all unit tests

### 3a: Move Modules to kernel-core + Update Kernel Re-exports (per-module pairs)

Each task moves the pure-logic code to kernel-core AND updates the kernel file to re-export from kernel-core in a single step, so the build never breaks mid-task.

- [x] T015 [P] [US1] **Pipe**: Move `Pipe` struct + methods to `kernel-core/src/pipe.rs`; update `kernel/src/pipe.rs` to re-export `Pipe` and keep `PIPE_TABLE` global + public functions (`create_pipe`, `pipe_read`, `pipe_write`, etc.)
- [x] T016 [P] [US1] **Message**: Move `Message` struct to `kernel-core/src/ipc/message.rs`; update `kernel/src/ipc/message.rs` to re-export
- [x] T017 [P] [US1] **Capability**: Move `Capability`, `CapError`, `CapHandle`, `CapabilityTable` to `kernel-core/src/ipc/capability.rs` (uses `TaskId`, `EndpointId`, `NotifId` from `kernel_core::types`); update `kernel/src/ipc/capability.rs` to re-export
- [x] T018 [P] [US1] **Registry**: Refactor `kernel/src/ipc/registry.rs` — extract `Registry` struct with instance methods (`register(&mut self, ...)`, `lookup(&self, ...)`) + `RegistryError` into `kernel-core/src/ipc/registry.rs`; keep global `REGISTRY: Mutex<Registry>` + module-level `register()`/`lookup()` wrappers in kernel
- [x] T019 [P] [US1] **Ethernet**: Move `EthernetFrame`, `parse()`, `build()`, ethertype constants, `MAC_BROADCAST` to `kernel-core/src/net/ethernet.rs`; update `kernel/src/net/ethernet.rs` to re-export
- [x] T020 [P] [US1] **ARP**: Move `ArpPacket`, `parse()`, `build()` to `kernel-core/src/net/arp.rs`; leave `send_request()`, `resolve()`, ARP cache in `kernel/src/net/arp.rs` with re-exports
- [x] T021 [P] [US1] **IPv4**: Move `Ipv4Header`, `parse()`, `build()`, `checksum()`, protocol constants (`PROTO_ICMP`, `PROTO_UDP`, `PROTO_TCP`) to `kernel-core/src/net/ipv4.rs`; leave `send()`, `handle_ipv4()` in kernel with re-exports
- [x] T022 [P] [US1] **UDP**: Move `UdpHeader`, `parse()`, `build()` to `kernel-core/src/net/udp.rs`; also move `UdpBindings` struct with instance methods (`bind`, `enqueue`, `dequeue`) as testable pure logic; keep global `UDP_BINDINGS` static + `send()`/`recv()`/`handle_udp()` in kernel with re-exports
- [x] T023 [P] [US1] **ICMP**: Move `IcmpHeader`, `parse()`, `build()` to `kernel-core/src/net/icmp.rs`; note: `build()` calls `ipv4::checksum()` so `kernel-core/src/net/icmp.rs` must `use super::ipv4::checksum`; leave `handle_icmp()`, `ping()` in kernel with re-exports
- [x] T024 [P] [US1] **TCP**: Move `TcpHeader`, `parse()`, `build()`, `tcp_checksum()`, TCP flag constants to `kernel-core/src/net/tcp.rs`; note: `tcp_checksum()` calls `ipv4::checksum()` — same cross-module pattern as ICMP; leave connection state machine + `handle_tcp()` in kernel with re-exports
- [x] T025 [P] [US1] **Tmpfs**: Move `Tmpfs`, `TmpfsError`, `TmpfsStat`, `MAX_FILE_SIZE`, all `Tmpfs` methods to `kernel-core/src/fs/tmpfs.rs`; leave global `TMPFS: Mutex<Tmpfs>` in kernel with re-exports
- [x] T026 [US1] Create/finalize `kernel-core/src/ipc/mod.rs`, `kernel-core/src/net/mod.rs`, `kernel-core/src/fs/mod.rs` with appropriate re-exports; update `kernel-core/src/lib.rs` module declarations

### 3b: Verify No Regression

- [x] T027 [US1] Run `cargo xtask check` — kernel must compile cleanly with all re-exports
- [x] T028 [US1] Run `cargo xtask run` — verify kernel boots and runs normally in QEMU (no regression)

**Checkpoint**: kernel-core compiles on host, kernel builds for bare-metal using re-exports, boot works

### 3c: Write Host-Side Unit Tests

- [x] T029 [P] [US1] Write Pipe tests in `kernel-core/src/pipe.rs` — read/write, wraparound, empty/full, partial read, partial write, zero-length ops (target: 6+ tests)
- [x] T030 [P] [US1] Write Message tests in `kernel-core/src/ipc/message.rs` — `new()`, `with1()`, `with2()`, default (target: 4+ tests)
- [x] T031 [P] [US1] Write CapabilityTable tests in `kernel-core/src/ipc/capability.rs` — insert, get, remove, invalid handle, wrong type, table full, insert_at, default (target: 8+ tests)
- [x] T032 [P] [US1] Write Registry tests in `kernel-core/src/ipc/registry.rs` — register, lookup, duplicate name, name too long, registry full (target: 5+ tests)
- [x] T033 [P] [US1] Write IPv4 tests in `kernel-core/src/net/ipv4.rs` — parse valid packet, parse too-short, parse wrong version, checksum RFC vectors, build round-trip (target: 5+ tests)
- [x] T034 [P] [US1] Write UDP tests in `kernel-core/src/net/udp.rs` — parse valid, parse too-short, build round-trip, payload truncation, UdpBindings bind/enqueue/dequeue/duplicate-bind/full-queue (target: 7+ tests)
- [x] T035 [P] [US1] Write ICMP tests in `kernel-core/src/net/icmp.rs` — parse valid, parse too-short, build with checksum verification, echo request/reply type codes (target: 4+ tests)
- [x] T036 [P] [US1] Write Ethernet tests in `kernel-core/src/net/ethernet.rs` — parse valid frame, parse too-short, build round-trip (target: 3+ tests)
- [x] T037 [P] [US1] Write ARP tests in `kernel-core/src/net/arp.rs` — parse valid, parse wrong hw/proto type, parse too-short, build round-trip (target: 4+ tests)
- [x] T038 [P] [US1] Write TCP tests in `kernel-core/src/net/tcp.rs` — parse valid, parse too-short, build round-trip, tcp_checksum verification, flag constants (target: 5+ tests)
- [x] T039 [P] [US1] Write Tmpfs tests in `kernel-core/src/fs/tmpfs.rs` — create file, read/write, stat, unlink, mkdir, rmdir, list_dir, rename, truncate, nested paths, error cases (target: 10+ tests)

### 3d: Integrate Host Tests into Build System

- [x] T040 [US1] Update `cargo xtask check` in `xtask/src/main.rs` to also run `cargo test -p kernel-core --target x86_64-unknown-linux-gnu`
- [x] T041 [US1] Run full `cargo xtask check` and verify all host-side tests pass

**Checkpoint**: US1 complete — 30+ host-side unit tests pass via `cargo xtask check`, kernel still boots

---

## Phase 4: User Story 2 — In-QEMU Test Harness (Priority: P2)

**Goal**: Implement the QEMU-based test harness from docs/09-testing.md

**Independent Test**: `cargo xtask test` boots a test kernel in QEMU, runs tests, and exits with pass/fail

### 4a: Kernel Test Harness Module

- [ ] T042 [US2] Create `kernel/src/testing.rs` with `QemuExitCode`, `exit_qemu()`, `Testable` trait, `test_runner()`, `test_panic_handler()`
- [ ] T043 [US2] Update `kernel/src/main.rs` with conditional `#![cfg_attr(test, ...)]` attributes for `custom_test_frameworks` and `test_runner`
- [ ] T044 [US2] Add conditional `#[cfg(test)]` test panic handler in `kernel/src/main.rs` that calls `testing::test_panic_handler()`

### 4b: Xtask Test Subcommand

- [ ] T045 [US2] Add `test` subcommand to `xtask/src/main.rs` — build kernel with `--tests`, launch QEMU with ISA debug exit device, read exit code
- [ ] T046 [US2] Add `--test <name>` flag to run a single integration test binary
- [ ] T047 [US2] Add `--timeout <seconds>` flag (default 60s) — kill QEMU if it exceeds the timeout
- [ ] T048 [US2] Update `usage()` in `xtask/src/main.rs` to document the `test` subcommand

### 4c: Sample Integration Test

- [ ] T049 [US2] Write a basic boot integration test as a `#[test_case]` in `kernel/src/main.rs` (or `kernel/tests/basic_boot.rs`) that verifies serial output works and exits successfully
- [ ] T050 [US2] Verify `cargo xtask test` runs the sample test and QEMU exits with success code (0x21)
- [ ] T051 [US2] Verify a panicking test causes QEMU to exit with failure code (0x23) and prints the panic message

**Checkpoint**: US2 complete — `cargo xtask test` works with at least one integration test

---

## Phase 5: User Story 3 — Documentation & Conventions (Priority: P3)

**Goal**: Document the testing patterns so future developers can add tests without guidance

- [ ] T052 [P] [US3] Update `docs/09-testing.md` to document both host-side (`cargo test -p kernel-core`) and QEMU testing workflows with concrete examples
- [ ] T053 [P] [US3] Update `CLAUDE.md` / `AGENTS.md` build commands section to include `cargo xtask test` and host test commands
- [ ] T054 [US3] Update workspace `Cargo.toml` comments if needed to reflect new `kernel-core` member

**Checkpoint**: US3 complete — documentation matches implementation

---

## Phase 6: Polish & Validation

- [ ] T055 Run full validation: `cargo xtask check` (includes host tests), `cargo xtask test` (QEMU tests), `cargo xtask run` (normal boot)
- [ ] T056 Verify SC-001: count host-side tests (must be >= 30)
- [ ] T057 Verify SC-002: measure host test execution time (must be < 10 seconds)

---

## Dependencies & Execution Order

```text
Phase 1 (Setup: T001–T004)
  └─→ Phase 2 (Shared Types: T005–T014)
        └─→ Phase 3a (Move + Re-export per-module: T015–T026)  ←── [P] tasks parallel
              └─→ Phase 3b (Verify: T027–T028)
                    ├─→ Phase 3c (Write Tests: T029–T039)  ←── all [P] tasks parallel
                    │     └─→ Phase 3d (Integrate: T040–T041)
                    │
                    └─→ Phase 4a (QEMU Harness: T042–T044)  ←── parallel with 3c
                          └─→ Phase 4b (Xtask Command: T045–T048)
                                └─→ Phase 4c (Sample Test: T049–T051)
                                      └─→ Phase 5 (Docs: T052–T054)  ←── [P] tasks parallel
                                            └─→ Phase 6 (Polish: T055–T057)
```

### Parallel Execution Opportunities

- **Phase 3a**: T015–T025 are independent per-module extractions — run all in parallel
- **Phase 3c**: T029–T039 are independent test files — run all in parallel
- **Phase 3c + Phase 4a**: QEMU harness work (T042–T044) can start in parallel with writing host tests (T029–T039) since they touch different files
- **Phase 5**: T052–T053 are independent doc updates — run in parallel

### Suggested MVP Scope

**User Story 1 only (Phases 1–3)**: This delivers the highest-value outcome — 30+ fast host-side unit tests covering all pure-logic modules. User Stories 2 and 3 can be tackled in a follow-up session.

---

## Review Notes

Issues identified and resolved during review:

1. **Newtype structs, not aliases**: `TaskId(u64)`, `EndpointId(u8)`, `NotifId(u8)` are structs with derive attributes — moved as-is, not simplified to type aliases
2. **Per-module task pairing**: Phase 3a merges move + re-export into single per-module tasks (was split into separate 3a/3b batches which would cause broken intermediate builds)
3. **TCP extraction added**: `TcpHeader`, `parse()`, `build()`, `tcp_checksum()` are pure logic — added as T024 (move) and T038 (tests)
4. **UdpBindings included**: `UdpBindings` struct with `bind`/`enqueue`/`dequeue` moved to kernel-core per plan; tests added in T034
5. **Cross-module deps documented**: ICMP `build()` → `ipv4::checksum()` and TCP `tcp_checksum()` → `ipv4::checksum()` noted in T023/T024
6. **EndpointId/NotifId origin**: These are defined in `endpoint.rs` and `notification.rs` (not `ipc/mod.rs`) — T008/T009 updated to target the correct files
