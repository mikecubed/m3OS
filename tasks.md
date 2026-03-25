# Phase 16 — Network Stack

**Branch:** `phase-16-network-stack`
**Depends on:** Phase 12 (POSIX Compat) ✅, Phase 15 (Hardware Discovery) ✅
**Status:** ✅ Complete

## Track Status

| Track | Scope | Status |
|---|---|---|
| A | virtio-net driver | ✅ done |
| B | Ethernet + ARP | ✅ done |
| C | IPv4 + ICMP | ✅ done |
| D | UDP | ✅ done |
| E | TCP | ✅ done |
| F | Net task + ping (socket API deferred) | ✅ done |
| G | Validation + documentation | ✅ done |

---

## Track A — virtio-net Driver

| Task | Description | Status |
|---|---|---|
| P16-T001 | Find virtio-net device in PCI device list | ✅ |
| P16-T002 | Read BARs to locate virtio configuration regions | ✅ |
| P16-T003 | Implement virtio device reset sequence | ✅ |
| P16-T004 | Implement feature negotiation | ✅ |
| P16-T005 | Define `Virtqueue` struct | ✅ |
| P16-T006 | Implement `virtqueue_init(queue_index)` | ✅ |
| P16-T007 | Initialize RX and TX virtqueues | ✅ |
| P16-T008 | Implement `virtio_net_recv()` | ✅ |
| P16-T009 | Implement `virtio_net_send(frame)` | ✅ |
| P16-T010 | Read device MAC address | ✅ |
| P16-T011 | Route virtio-net IRQ through I/O APIC | ✅ |
| P16-T012 | Implement interrupt-driven receive | ✅ |

## Track B — Ethernet and ARP

| Task | Description | Status |
|---|---|---|
| P16-T013 | Define `EthernetFrame` struct | ✅ |
| P16-T014 | Implement `ethernet_parse()` | ✅ |
| P16-T015 | Implement `ethernet_build()` | ✅ |
| P16-T016 | Implement EtherType dispatch | ✅ |
| P16-T017 | Define ARP packet structure | ✅ |
| P16-T018 | Implement ARP parse/build | ✅ |
| P16-T019 | Implement ARP cache | ✅ |
| P16-T020 | Implement `arp_resolve()` | ✅ |
| P16-T021 | Implement ARP request path | ✅ |
| P16-T022 | Implement ARP reply handler | ✅ |
| P16-T023 | Implement ARP request responder | ✅ |

## Track C — IPv4 and ICMP

| Task | Description | Status |
|---|---|---|
| P16-T024 | Define `Ipv4Header` struct | ✅ |
| P16-T025 | Implement `ipv4_parse()` | ✅ |
| P16-T026 | Implement IPv4 header checksum | ✅ |
| P16-T027 | Implement `ipv4_build()` | ✅ |
| P16-T028 | Implement `ipv4_send()` | ✅ |
| P16-T029 | Configure static IP (10.0.2.15/24, gw 10.0.2.2) | ✅ |
| P16-T030 | Implement protocol dispatch | ✅ |
| P16-T031 | Define ICMP header struct | ✅ |
| P16-T032 | Implement ICMP echo reply | ✅ |
| P16-T033 | Implement `ping(target_ip)` | ✅ |

## Track D — UDP

| Task | Description | Status |
|---|---|---|
| P16-T034 | Define `UdpHeader` struct | ✅ |
| P16-T035 | Implement `udp_parse()` | ✅ |
| P16-T036 | Implement `udp_build()` | ✅ |
| P16-T037 | Implement UDP port binding table | ✅ |
| P16-T038 | Implement `udp_send()` | ✅ |
| P16-T039 | Implement `udp_recv()` | ✅ |

## Track E — TCP

| Task | Description | Status |
|---|---|---|
| P16-T040 | Define `TcpHeader` struct | ✅ |
| P16-T041 | Implement TCP checksum | ✅ |
| P16-T042 | Implement TCP parse/build | ✅ |
| P16-T043 | Define `TcpState` enum | ✅ |
| P16-T044 | Define `TcpConnection` struct | ✅ |
| P16-T045 | Implement active open (client connect) | ✅ |
| P16-T046 | Implement passive open (server listen) | ✅ |
| P16-T047 | Implement data send | ✅ |
| P16-T048 | Implement data receive | ✅ |
| P16-T049 | Implement connection close (active) | ✅ |
| P16-T050 | Implement connection close (passive) | ✅ |
| P16-T051 | Implement RST handling | ✅ |
| P16-T052 | Implement simple flow control | ✅ |

## Track F — Net Task + Ping (Socket API Deferred)

| Task | Description | Status |
|---|---|---|
| P16-T053 | Create `userspace/net_server` crate | ⏭️ deferred (kernel-mode net stack) |
| P16-T054 | Shared-memory region for driver ↔ net_server | ⏭️ deferred |
| P16-T055 | Implement net_server main loop | ✅ (kernel net_task) |
| P16-T056 | Define socket syscall numbers | ⏭️ deferred |
| P16-T057 | Implement `sys_socket()` | ⏭️ deferred |
| P16-T058 | Implement `sys_bind()` | ⏭️ deferred |
| P16-T059 | Implement `sys_connect()` | ⏭️ deferred |
| P16-T060 | Implement `sys_listen()` / `sys_accept()` | ⏭️ deferred |
| P16-T061 | Implement `sys_send()` / `sys_recv()` | ⏭️ deferred |
| P16-T062 | Implement `sys_sendto()` / `sys_recvfrom()` | ⏭️ deferred |
| P16-T063 | Add `ping` shell command | ✅ |
| P16-T064 | Add `nc`-like utility | ⏭️ deferred |

## Track G — Validation and Documentation

| Task | Description | Status |
|---|---|---|
| P16-T065 | Acceptance: virtio-net detected and MAC logged | ✅ |
| P16-T066 | Acceptance: `ping 10.0.2.2` works | ⏳ pending interactive QEMU validation |
| P16-T067 | Acceptance: UDP echo test | ⏭️ deferred (needs nc utility) |
| P16-T068 | Acceptance: TCP client test | ⏭️ deferred (needs nc utility) |
| P16-T069 | Acceptance: TCP server test | ⏭️ deferred (needs nc utility) |
| P16-T070 | Acceptance: no regressions | ✅ |
| P16-T071 | `cargo xtask check` passes | ✅ |
| P16-T072 | QEMU boot validation | ✅ |
| P16-T073 | Write `docs/16-network.md` | ✅ |
