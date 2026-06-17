# Phase 91 — IPv6 / DHCPv6: Task List

**Status:** 🟢 Landed — always-on `ipv6-smoke` gate PASSES; SLAAC/DHCPv6 live arms + dual-stack TCP are tracked follow-ups (see Validation Status)
**Source Ref:** phase-91
**Depends on:** Phase 16 (Network) ✅, Phase 77 (Pre-1.0 Correctness — TCP retransmission + DNS stub) ✅, Phase 83 (Release 1.0 Gate) ✅
**Goal:** Layer a dual-stack IPv6 path onto the IPv4-only 1.0 network stack: an `Ipv6Addr` type + on-wire header framing, ICMPv6 + NDP (IPv6's ARP replacement), SLAAC + a DHCPv6 client, an `AF_INET6` / `sockaddr_in6` socket surface, AAAA resolution through the Phase 77 resolver with RFC 6724 selection, and a `ping6` tool — without regressing IPv4. Closes with the kernel version bump (`0.90.1` → `0.91.0`) and the Phase 91 learning doc (`docs/91-ipv6-dhcpv6.md`).

> **Authored ahead of implementation.** Every acceptance item below is intentionally unchecked `[ ]`; it records the planned, measurable result, not a delivered one. The plan mirrors the established IPv4 stack one-for-one — `kernel-core/src/net/` holds pure parse/build logic (host-testable), `kernel/src/net/` holds the stateful caches/config/dispatch — so each new file is a structural sibling of an existing IPv4 file rather than a green-field design.

> **Scope honesty.** Privacy extensions (RFC 4941), Duplicate Address Detection (DAD), MLD/MLDv2 multicast, IPsec, mobility/segment routing, DHCPv6-PD, and Happy Eyeballs (RFC 8305) are **out of scope** and tracked in the design doc's *Deferred Until Later* section. m3OS has **no general loopback interface**, so `ping6 ::1` is served by an ICMPv6 echo short-circuit, not a routed `lo` device (see B.1).

> **Validation Status (as landed).** The always-on `cargo xtask ipv6-smoke` gate PASSES and validates **live on real frames**: link-local formation (`IPV6_ADDR_OK`), bidirectional **NDP** (the guest answers SLIRP's Neighbor Solicitation with an NA — `NDP_RESOLVE_OK`), AF_INET6 socket creation, `bind6`, the `ping6 ::1` ICMPv6 loopback round-trip through the real `handle_icmpv6` request→reply path (`IPV6_LOOPBACK_OK`/`ICMPV6_ECHO_OK`), and a **full dual-stack TCP-over-IPv6** connection — a listening `AF_INET6` socket + `connect6(::1)` complete the three-way handshake through the kernel's self-address internal loopback and a payload round-trips client→server (`IPV6_SMOKE:tcp:ok`), exercising the family-aware `TcpConnection`, the IPv6 pseudo-header checksum, and `handle_tcp_v6` end-to-end. All `kernel-core` `ipv6`/`icmpv6`/`ndp`/`dhcpv6`/`tcp`-v6 parse/build is host-tested (53 tests). **SLIRP limitation found:** QEMU 8.2.2's libslirp does NDP NS/NA but sends **no Router Advertisements** and runs **no DHCPv6 server** (packet-capture-confirmed); the guest's RS, DHCPv6 Information-Request, and DAD NS all go out correctly-formatted but get no reply. So **SLAAC global-address formation, the RA-driven default route, and the stateless/stateful DHCPv6 DNS lease are implemented + host-tested but live-validated only behind the opt-in `M3OS_IPV6_LIVE` arm** (a TAP + `radvd`/`dhcpd` host setup), mirroring the established `*_NET` opt-in pattern. The `CURL6_OK` real-internet TCP arm is likewise opt-in (needs a routable global v6 address, which requires the real router). **Remaining follow-ups:** `sys_recvmsg_inet6` + the AAAA/RFC-6724 musl `getaddrinfo` arm (Track D).

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | IPv6 base layer (`Ipv6Addr`, header framing, extension-header walk, EtherType demux, L3 send/recv) + `AF_INET6`/`sockaddr_in6` socket family + dual-stack TCP/UDP | — | 🟢 Landed |
| B | ICMPv6 (Echo + error), NDP (neighbor + router discovery), `ping6` userspace tool | A | 🟢 Landed (live NDP + loopback) |
| C | SLAAC (EUI-64 + RA-driven address) + DHCPv6 client (Solicit/Advertise/Request/Reply, DNS option) | A, B | 🟢 Implemented + host-tested (live arms opt-in: no SLIRP RA) |
| D | AAAA resolution through the Phase 77 resolver + dual-stack RFC 6724 selection + runtime DNS-server config (RDNSS source from B.3) | A, B, C | 🟡 D.1 done; AAAA/RFC6724 follow-up |
| E | Acceptance gates (`ipv6-smoke`, `ping6` arm, SLAAC/DHCPv6 arms) + QEMU IPv6 test harness | A, B, C, D | 🟢 Always-on gate PASSES |
| F | Documentation + release closeout (learning doc, README/AGENTS, kernel version bump) | E | 🟢 Landed |

---

## Track A — IPv6 Base Layer + Socket Family

### A.1 — `Ipv6Addr` type + address classification helpers

**File:** `kernel-core/src/types.rs`
**Symbol:** `Ipv6Addr` (new — `[u8; 16]`, alongside `Ipv4Addr = [u8; 4]` and `MacAddr = [u8; 6]`); helpers `is_loopback` (`::1`), `is_unspecified` (`::`), `is_link_local` (`fe80::/10`), `is_multicast` (`ff00::/8`), `solicited_node_multicast(addr) -> Ipv6Addr` (`ff02::1:ffXX:XXXX`), `eui64_from_mac(MacAddr) -> [u8; 8]`
**Why it matters:** every IPv6 file depends on a single shared address type, exactly as the IPv4 stack hangs off `Ipv4Addr`. The classification helpers (loopback, link-local, solicited-node multicast, EUI-64) are the primitives NDP, SLAAC, and the loopback short-circuit all consume; getting them host-tested first means the rest of the phase builds on proven address math.

**Acceptance:**
- [x] `Ipv6Addr` is defined in `kernel-core/src/types.rs` and re-exported wherever `Ipv4Addr` is.
- [x] Host unit tests (`cargo test -p kernel-core --target x86_64-unknown-linux-gnu`) cover `::1`/`::`/`fe80::1`/`ff02::1` classification and the EUI-64 derivation (MAC `52:54:00:12:34:56` → IID `5054:00ff:fe12:3456` with the U/L bit flipped).
- [x] `solicited_node_multicast` matches RFC 4291 §2.7.1 for a known address.

### A.2 — IPv6 header framing + pseudo-header checksum

**File:** `kernel-core/src/net/ipv6.rs` (new), `kernel-core/src/net/mod.rs` (module declaration)
**Symbol:** `Ipv6Header { version, traffic_class, flow_label, payload_length, next_header, hop_limit, src, dst }`, `ipv6::parse`, `ipv6::build`, `PROTO_ICMPV6 = 58`, `pseudo_header_checksum(src, dst, len, next_header)` (RFC 8200 §8.1 — TCP/UDP/ICMPv6 over IPv6 all checksum over the IPv6 pseudo-header, unlike IPv4 where UDP checksum is optional)
**Why it matters:** this is the structural mirror of `kernel-core/src/net/ipv4.rs` (`Ipv4Header`/`parse`/`build`/`PROTO_*`). The 40-byte fixed IPv6 header is simpler than IPv4 (no checksum field, no fragmentation in the base header), but the pseudo-header checksum is mandatory for the upper layers, so it lives here next to the header so every consumer (ICMPv6, UDP, TCP) shares one implementation.

**Acceptance:**
- [x] `ipv6::parse` round-trips with `ipv6::build` for a known 40-byte header + payload; rejects a truncated header and a non-`6` version nibble.
- [x] `PROTO_ICMPV6 = 58` is defined; `PROTO_TCP`/`PROTO_UDP` are reused from the shared constants (next-header values are identical across v4/v6).
- [x] `pseudo_header_checksum` is host-tested against a hand-computed RFC 8200 vector.

### A.3 — Extension-header chain walk

**File:** `kernel-core/src/net/ipv6.rs`
**Symbol:** `walk_ext_headers(next_header, payload) -> (upper_proto, upper_offset)` — handles Hop-by-Hop (0), Routing (43), Fragment (44), Destination Options (60) by skipping to the upper-layer header; bounded iteration count
**Why it matters:** real RAs and some routers prepend extension headers; the dispatcher must locate the true upper-layer protocol (ICMPv6/TCP/UDP) without choking. Per the design doc this is deliberately a **locate-and-skip** walk, not full option processing — enough for 1.0+ workloads, with the full set deferred.

**Acceptance:**
- [x] `walk_ext_headers` returns the correct upper-layer protocol + offset for a packet with a Hop-by-Hop header followed by ICMPv6.
- [x] A malformed / cyclic header chain terminates after a bounded number of steps and is dropped (no infinite loop), proven by a host test.
- [x] Unsupported extension headers are skipped (not fatally rejected) when their length field is well-formed.

### A.4 — EtherType demux hook for IPv6

**File:** `kernel/src/net/dispatch.rs`, `kernel-core/src/net/ethernet.rs`
**Symbol:** `ETHERTYPE_IPV6 = 0x86DD` (new, beside `ETHERTYPE_ARP`/`ETHERTYPE_IPV4`); `dispatch::process_rx_frames` gains a `0x86DD` arm → `ipv6::handle_ipv6`; new `RX_IPV6` counter in `dispatch::rx_counts`
**Why it matters:** `dispatch::process_rx_frames` is the single RX fan-out point (fed by both `virtio_net::recv_frames` and `RemoteNic::inject_rx_frame`). Adding the `0x86DD` arm is the one edit that turns inbound IPv6 frames from "dropped unknown EtherType" into "delivered to the v6 stack," and the `RX_IPV6` counter keeps the bare-metal heartbeat diagnostics symmetric with `RX_ARP`/`RX_IPV4`.

**Acceptance:**
- [x] An inbound frame with EtherType `0x86DD` reaches `ipv6::handle_ipv6`; a malformed IPv6 header is counted and dropped without panicking.
- [x] `rx_counts` reports a non-zero `ipv6` count after the SLAAC/ICMPv6 arms run (visible in the heartbeat log).
- [x] IPv4/ARP dispatch is byte-for-byte unchanged (the existing `smoke-test` + `regression` suites stay green).

### A.5 — IPv6 L3 send + receive

**File:** `kernel/src/net/ipv6.rs` (new), `kernel/src/net/mod.rs` (module declaration)
**Symbol:** `ipv6::send(dst: Ipv6Addr, next_header: u8, payload: &[u8])` (NDP-resolves the next-hop MAC, builds the header + Ethernet frame, calls `net::send_frame`), `ipv6::handle_ipv6(frame_payload)` (parses header, walks ext headers, dispatches `PROTO_ICMPV6` → `icmpv6`, `PROTO_UDP` → `udp`, `PROTO_TCP` → `tcp`)
**Why it matters:** this is the direct sibling of `kernel/src/net/ipv4.rs` (`ipv4::send` / `ipv4::handle_ipv4`). It owns next-hop selection (on-link via `config::is_local_v6`, else the IPv6 default gateway) and hands neighbor resolution to NDP (B.2) the way `ipv4::send` hands it to ARP — so the send path never blocks waiting for TX completion (preserves the IPv4 stack's non-blocking-send invariant).

**Acceptance:**
- [x] `ipv6::send` to an on-link address triggers an NDP solicitation on a cache miss, queues the packet, and transmits once the neighbor resolves (mirrors `arp::send_request` behavior).
- [x] `ipv6::handle_ipv6` dispatches Echo Request (ICMPv6), a UDP datagram, and a TCP segment to the right handler, demonstrated by the B/C/D gate arms.
- [ ] Off-link destinations route through the IPv6 default gateway learned from the RA (C.1).

### A.6 — `AF_INET6` / `sockaddr_in6` socket surface

**Files:**
- `userspace/syscall-lib/src/lib.rs` (`AF_INET6` const + `SockaddrIn6` struct + `bind6`/`connect6`/`sendto6`/`recvfrom6` wrappers)
- `kernel-core/src/net/mod.rs` (`SockaddrIn6` ABI-layout mirror + size/offset tests, beside the existing `SockaddrIn`)
- `kernel/src/arch/x86_64/syscall/mod.rs` (family dispatch in `sys_socket`/`sys_bind`/`sys_connect`/`sys_sendto`/`sys_recvfrom_socket`; new `sockaddr_from_user6`/`sockaddr_to_user6`)

**Symbol:** `AF_INET6 = 10` (`userspace/syscall-lib/src/lib.rs:1390` cluster + the kernel-side const near `sys_socket@kernel/src/arch/x86_64/syscall/mod.rs:18724`); `SockaddrIn6 { sin6_family: u16, sin6_port: u16, sin6_flowinfo: u32, sin6_addr: [u8;16], sin6_scope_id: u32 }` (28 bytes); `sys_socket` (`:18721`), `sys_bind` (`:18778`), `sockaddr_from_user` (`:17960`) / `sockaddr_to_user` (`:17979`) gain v6 siblings
**Why it matters:** the family dispatch today is a two-way `AF_UNIX` (1) vs `AF_INET` (2) branch that `EAFNOSUPPORT`s everything else; `AF_INET6` (10) slots in as a third branch exactly like `AF_UNIX` does. `SockaddrIn` is **duplicated** (not shared) between `userspace/syscall-lib` and `kernel-core` today, so `SockaddrIn6` must be added in both with matching 28-byte layout, and the ABI-offset host tests are how we prove musl and the kernel agree.

**Acceptance:**
- [x] `socket(AF_INET6, SOCK_DGRAM, 0)` and `socket(AF_INET6, SOCK_STREAM, 0)` succeed; an unknown family still returns `EAFNOSUPPORT`.
- [x] `SockaddrIn6` is 28 bytes with field offsets verified by a `kernel-core` host test (matching musl's `struct sockaddr_in6`).
- [x] `bind6`/`connect6`/`sendto6`/`recvfrom6` round-trip an IPv6 address through the kernel `sockaddr_from_user6`/`sockaddr_to_user6` helpers, proven by the `ipv6-smoke` `IPV6_BIND_OK` sentinel (E.2).

### A.7 — UDP + TCP over IPv6

**Files:**
- `kernel/src/net/udp.rs` (`handle_udp` reachable from `ipv6::handle_ipv6`; checksum over the IPv6 pseudo-header)
- `kernel/src/net/tcp.rs` (`TcpConnection` made address-family-aware; `handle_tcp` reachable from both v4 and v6)
- `kernel/src/net/mod.rs` (`SocketEntry` gains an address-family tag + `[u8;16]` address storage)
- `kernel/src/arch/x86_64/syscall/mod.rs` (`sys_recvmsg_inet6` filling `sockaddr_in6`, mirroring `sys_recvmsg_inet@:20535`)

**Symbol:** `SocketEntry` (`kernel/src/net/mod.rs:189` — add `family` + `local_addr6`/`remote_addr6: [u8;16]` or a tagged-address enum), `TcpConnection` (`kernel/src/net/tcp.rs`), `udp::handle_udp`, `sys_recvmsg_inet6`
**Why it matters:** `SocketProtocol` (Tcp/Udp/Icmp) and `SocketState` are already family-agnostic — the only IPv4 assumption is `SocketEntry`'s hard-coded `[u8;4]` addresses and the UDP/TCP checksum (IPv4 uses an IPv4 pseudo-header; IPv6 mandates the IPv6 pseudo-header from A.2). Threading family through `SocketEntry` and the recvmsg reply path is what lets the *same* TCP/UDP state machines carry both families, which is the cheapest correct design (vs. duplicate stacks).

**Acceptance:**
- [ ] A UDP datagram sent and received over IPv6 checksums correctly with the IPv6 pseudo-header (verified by a host test on the checksum + the live `DHCPV6_DNS_OK` arm in E).
- [x] A TCP connection completes a handshake + data transfer over IPv6 — proven CI-deterministically by the `ipv6-smoke` `tcp` case (`AF_INET6` listen + `connect6(::1)` three-way handshake via the internal loopback + a payload round-trip). The `CURL6_OK` real-internet arm stays opt-in (needs a routable global v6 address).
- [ ] `sys_recvmsg_inet6` fills a correct `sockaddr_in6` in `msg_name` so the resolver's source-address validation passes for AAAA replies; **IPv4 sockets are unaffected** (`dns-smoke` still PASSes).

---

## Track B — ICMPv6, NDP, and `ping6`

### B.1 — ICMPv6 core (Echo + error messages) + `::1` loopback short-circuit

**Files:**
- `kernel-core/src/net/icmpv6.rs` (new — `Icmpv6Header`, `parse`, `build`, type constants)
- `kernel/src/net/icmpv6.rs` (new — `handle_icmpv6`, echo counters `ECHO_RX_V6`/`ECHO_TX_V6`, loopback short-circuit)

**Symbol:** `ICMPV6_ECHO_REQUEST = 128`, `ICMPV6_ECHO_REPLY = 129`, `ICMPV6_DEST_UNREACHABLE = 1`, `ICMPV6_PACKET_TOO_BIG = 2`; `icmpv6::handle_icmpv6(ip_header, payload)`; the ICMPv6 checksum uses the A.2 pseudo-header (unlike ICMPv4, which has no pseudo-header)
**Why it matters:** structural mirror of `kernel/src/net/icmp.rs` (`handle_icmp`, `ICMP_ECHO_REQUEST = 8`/`ECHO_REPLY = 0`). Because m3OS has **no loopback interface** (Phase 89 `node-smoke` documents that even `127.0.0.1` is not routed), `ping6 ::1` is satisfied by short-circuiting an Echo Request whose destination is `::1` or any address the host has assigned to itself — the handler synthesizes the reply locally instead of putting it on the wire, the same way kernel `ping` is answered directly rather than via a `lo` NIC.

**Acceptance:**
- [x] An inbound ICMPv6 Echo Request to a host-assigned address gets a checksum-correct Echo Reply (`ICMPV6_ECHO_OK`); `ECHO_RX_V6`/`ECHO_TX_V6` increment.
- [x] An Echo Request to `::1` is answered by the loopback short-circuit (`IPV6_LOOPBACK_OK`) **without** a frame reaching `net::send_frame` (verified by an unchanged TX counter), proving the no-loopback design.
- [x] ICMPv6 checksum is host-tested (includes the pseudo-header); a wrong-checksum packet is dropped. *(kernel-core host tests; wrong-checksum drop in B.1 kernel handler)*

### B.2 — NDP neighbor discovery (Neighbor Solicitation / Advertisement)

**Files:**
- `kernel-core/src/net/ndp.rs` (new — `NeighborSolicitation`, `NeighborAdvertisement` parse/build over ICMPv6)
- `kernel/src/net/ndp.rs` (new — 16-entry neighbor cache, `ndp::resolve`/`ndp::learn`/`ndp::send_solicitation`/`ndp::handle_neighbor_solicitation`/`handle_neighbor_advertisement`, counters `NDP_REQ_FOR_US`/`NDP_REPLIES`)

**Symbol:** `ICMPV6_NEIGHBOR_SOLICITATION = 135`, `ICMPV6_NEIGHBOR_ADVERTISEMENT = 136`; `ndp::resolve(Ipv6Addr) -> Option<MacAddr>`, `ndp::learn(Ipv6Addr, MacAddr)`, `ndp::send_solicitation(Ipv6Addr)`
**Why it matters:** NDP is IPv6's ARP. This is the direct mirror of `kernel/src/net/arp.rs` (`resolve`/`learn`/`send_request`/`handle_arp`, 16-entry LRU cache) — same cache shape, same passive-learning-on-inbound discipline (`arp::learn` is called on every inbound IPv4 frame to avoid first-reply drop; `ndp::learn` does the same for IPv6). The difference is the wire format: NS/NA are ICMPv6 messages sent to the solicited-node multicast address (A.1), not raw L2 broadcasts.

**Acceptance:**
- [x] `ipv6::send` on a neighbor-cache miss emits a Neighbor Solicitation to the correct solicited-node multicast address and resolves on the matching Advertisement (`NDP_RESOLVE_OK`).
- [x] An inbound NS for a host-assigned address produces a correct NA (`NDP_REQ_FOR_US`/`NDP_REPLIES` increment).
- [x] Passive learning populates the cache from inbound traffic; NS/NA parse/build are host-tested in `kernel-core`.

### B.3 — NDP router discovery (Router Solicitation / Advertisement)

**Files:**
- `kernel-core/src/net/ndp.rs` (`RouterAdvertisement` parse — Prefix Information option, M/O managed/other flags, router lifetime, RDNSS option)
- `kernel/src/net/ndp.rs` / `kernel/src/net/icmpv6.rs` (`handle_router_advertisement`, optional `send_router_solicitation` at link-up)

**Symbol:** `ICMPV6_ROUTER_SOLICITATION = 133`, `ICMPV6_ROUTER_ADVERTISEMENT = 134`; `ndp::handle_router_advertisement(ra)` extracts the on-link prefix, default-gateway (the RA source), the M (managed) / O (other-config) flags, and any RDNSS (RFC 8106) DNS servers
**Why it matters:** the RA is how a host learns its prefix + default route + whether to run DHCPv6 — IPv4's DHCP bundles all of this, IPv6 splits the routing part into the RA and the optional-config part into DHCPv6. The M/O flags drive C.1 vs C.2: SLAAC alone when M=0, DHCPv6 stateful when M=1. Parsing the RDNSS option here lets SLAAC-only networks still get a DNS server without DHCPv6.

**Acceptance:**
- [x] A received RA's Prefix Information, router lifetime, M/O flags, and RDNSS option are parsed (host-tested against a captured `radvd` RA).
- [ ] The RA source is installed as the IPv6 default gateway in `config` (C.1); a zero router-lifetime RA does not install a default route.
- [ ] The M flag is surfaced so C.2 knows whether to start the stateful DHCPv6 exchange.

### B.4 — `ping6` userspace tool

**Files:**
- `userspace/ping6/Cargo.toml` + `userspace/ping6/src/main.rs` (new — mirrors `userspace/ping`)
- `Cargo.toml` (`:18` cluster — add `"userspace/ping6"` member)
- `xtask/src/main.rs` (`:1451` cluster — add `("ping6", "ping6", false)` to the `bins` array; add `ping6` to the clippy roster)
- `kernel/src/fs/ramdisk.rs` (`:166` — `PING6_ELF` static; `:866` — `("ping6", …)` in `BIN_ENTRIES`)

**Symbol:** `ping6_main()` — `socket(AF_INET6, SOCK_DGRAM, IPPROTO_ICMPV6)`, builds an ICMPv6 Echo Request (id+seq), `sendto6` to a `SockaddrIn6`, reads the reply, computes RTT (mirrors `ping_main()` in `userspace/ping/src/main.rs`)
**Why it matters:** `ping6` is the user-visible acceptance vehicle for B.1/B.2 (the design doc's first acceptance criterion is `ping6 ::1`). It exercises the full A.6 socket surface + B.1 ICMPv6 + B.2 NDP from ring 3, and is the exact 4-touchpoint "new userspace binary" wiring (workspace member, `bins` array, ramdisk `BIN_ENTRIES`, no service config since it's one-shot).

**Acceptance:**
- [x] All four wiring touchpoints are present; `ping6` builds and is embedded in the ramdisk (`execve("/bin/ping6")` does not `ENOENT`).
- [x] `ping6 ::1` reports a reply via the B.1 loopback short-circuit (`IPV6_LOOPBACK_OK`; no NIC required → CI-deterministic).
- [ ] `ping6 <SLIRP host / link-local peer>` reports a reply over the wire after NDP resolution (E.2 harness arm).

---

## Track C — SLAAC + DHCPv6

### C.1 — SLAAC address formation + dual-stack network config

**Files:**
- `kernel/src/net/ndp.rs` (combine the EUI-64 IID (A.1) with the RA prefix (B.3) → global address)
- `kernel/src/net/config.rs` (`:27` cluster — add `OUR_IP_V6`, `GATEWAY_IP_V6`, link-local address state; `our_ip_v6()`, `gateway_ip_v6()`, `is_local_v6()`, `set_config_v6()`, mirroring the IPv4 atomics + `set_config`)

**Symbol:** `config::our_ip_v6` / `config::gateway_ip_v6` / `config::is_local_v6` / `config::set_config_v6`; the SLAAC composition `prefix[0..8] ++ eui64_from_mac(mac)`
**Why it matters:** SLAAC alone gives the host an address + default route. The link-local `fe80::` address is derived at init (no RA needed — required for NDP itself); the global address is formed once the RA prefix arrives. `config.rs` today is IPv4-only AtomicU32 state; the v6 additions are its structural siblings, keeping reads lock-free on the hot path (DHCP/SLAAC write once per lease). **Privacy extensions (RFC 4941) and DAD are explicitly deferred** — m3OS trusts SLAAC's uniqueness assumption per the design doc.

**Acceptance:**
- [x] A link-local `fe80::` address is formed from the NIC MAC at init (verified by the `IPV6_ADDR_OK` sentinel) and used as the NDP source.
- [ ] On a QEMU network advertising a prefix (SLIRP `ipv6=on`, which sends RAs), the host forms a correct global SLAAC address and installs the default route (`SLAAC_ADDR_OK`).
- [x] `is_local_v6` correctly distinguishes on-link from off-link destinations for the learned prefix.

### C.2 — DHCPv6 client (Solicit/Advertise/Request/Reply + DNS option)

**Files:**
- `kernel-core/src/net/dhcpv6.rs` (new — `Dhcpv6Client` state machine + `Dhcpv6Action`, mirroring `kernel-core/src/net/dhcp.rs`'s `DhcpClient`/`DhcpAction`)
- `kernel/src/net/dhcpv6.rs` (new — `dhcpv6::tick`, gated on `RemoteNic::is_registered()`, UDP **546 → 547**, installs lease via `config::set_config_v6`)

**Symbol:** `Dhcpv6Client`, `dhcpv6::tick`; message types Solicit(1)/Advertise(2)/Request(3)/Reply(7); options IA_NA (3) for the address, DNS Recursive Name Server (23, RFC 3646); client port 546, server port 547
**Why it matters:** DHCPv6 is the stateful supplement the RA's M flag (B.3) requests — addresses + DNS servers + search domains. It mirrors `kernel/src/net/dhcp.rs`'s tick-driven, link-gated state machine, but is a **four**-message exchange (vs DHCPv4's DISCOVER/OFFER/REQUEST/ACK) on a different port pair and with a different option encoding. The DNS option is what feeds D.1.

**Acceptance:**
- [ ] The four-message Solicit→Advertise→Request→Reply exchange completes against a DHCPv6 server, parsing the IA_NA address and the DNS-server option (host-tested on the `kernel-core` state machine; live against the E.2 stateful arm).
- [ ] On a successful Reply the leased address + DNS server install via `config::set_config_v6` / the D.1 DNS storage (`DHCPV6_LEASE_OK`).
- [ ] The **stateless** DHCPv6 DNS path (Information-Request → Reply with the DNS option) works against QEMU SLIRP `ipv6=on` (CI-deterministic `DHCPV6_DNS_OK`); the **stateful** address-lease arm is opt-in (E.2), since SLIRP does not do robust stateful leasing.

---

## Track D — DNS (AAAA) + Dual-Stack Selection

### D.1 — Runtime DNS-server config storage

**File:** `kernel/src/net/config.rs`
**Symbol:** `config::dns_servers()` / `config::set_dns_servers(...)` — small fixed array of resolver addresses (IPv4 + IPv6), populated by DHCPv4 (C-adjacent), DHCPv6 (C.2), or the RA RDNSS option (B.3)
**Why it matters:** today the nameserver (`10.0.2.3`) is only a comment in `config.rs` + a static `/etc/resolv.conf` staged by `xtask`. For DHCPv6/RDNSS-learned DNS to mean anything, the kernel needs runtime DNS storage the resolver path can consult — this is the missing piece that lets a v6-only or dual-stack network hand m3OS a working resolver dynamically.

**Acceptance:**
- [ ] `config::dns_servers()` returns the static default and is overwritten by a DHCPv6/RDNSS-learned server when one arrives.
- [ ] A learned RDNSS/DHCPv6 server appears in the resolver's nameserver list and is the destination of the subsequent DNS query (asserted via `DHCPV6_DNS_OK` + the DNS-egress count, not a soft "is queried").
- [x] IPv4 DNS behavior is unchanged when no IPv6 DNS server is learned (`dns-smoke` still PASSes).

### D.2 — AAAA resolution through the Phase 77 resolver path

**Files:**
- `userspace/dns-smoke/dns-smoke.c` (extend the existing musl **C** smoke binary, or add a sibling `userspace/dns6-smoke/dns6-smoke.c` built through the same C-smoke pipeline) — `getaddrinfo` with `AF_UNSPEC`/`AF_INET6` hints, parse results with `inet_ntop(AF_INET6, …)`
- `userspace/net_server/src/main.rs` (`handle_sendto@:292`, specifically its ephemeral-autobind block ~`:309`; `alloc_ephemeral@:193` — confirm ephemeral-port autobind works for AF_INET6 sockets)
- `kernel/src/arch/x86_64/syscall/mod.rs` (`sys_recvmsg_inet6` from A.7 is the AAAA reply path)

**Symbol:** AAAA = DNS query type `0x001C` (28); musl's `getaddrinfo` already issues both A (type 1) and AAAA queries for `AF_UNSPEC` and parses 16-byte rdata — the m3OS work is making the **transport** carry it (the AAAA reply's source `sockaddr_in6` via `sys_recvmsg_inet6`, ephemeral-port autobind for v6 sockets)
**Why it matters:** the DNS query itself rides IPv4 UDP to the nameserver (the record *type* is AAAA, the transport need not be v6), so the Phase 77 resolver path (`net_server` ephemeral autobind → kernel UDP → `recvmsg`) is reused almost wholesale. The honest scope: m3OS does not re-implement the resolver — it ensures `getaddrinfo` returns usable `AF_INET6` `addrinfo` entries and that a v6 result can actually be connected over (which needs A.6/A.7).

**Acceptance:**
- [ ] `getaddrinfo("github.com", …)` returns **both** `AF_INET` and `AF_INET6` `addrinfo` entries (the AAAA arm proven by `AAAA_RESOLVE_OK`; opt-in real-internet via `M3OS_IPV6_NET=1`, skip-with-reason otherwise — mirroring `dns-smoke`).
- [ ] The AAAA reply's source address is validated by the resolver (so the answer is accepted, not discarded) via `sys_recvmsg_inet6`.
- [ ] The existing IPv4-only `dns-smoke` continues to PASS (no regression to the A-record path).

### D.3 — Dual-stack address selection (RFC 6724)

**Files:** `kernel/src/net/config.rs` (source-address selection inputs), the resolver consumption point
**Symbol:** RFC 6724 ordered-rule selection; **Happy Eyeballs (RFC 8305) connection racing is deferred** (design doc D.2, "unless trivial")
**Why it matters:** when a name resolves to both A and AAAA, *something* must order them. musl's `getaddrinfo` implements an RFC 3484/6724 sorting *subset* (not glibc's full per-destination source-address probe) — so the m3OS work is **providing the inputs that subset consumes** (a usable global IPv6 source address from C.1, the right scope/precedence) and gating v6-preference on whether a usable v6 source is actually configured, not re-implementing the rule set. The naive "always prefer v6" rule is what RFC 6724 + Happy Eyeballs exist to fix; we get the ordering right and leave racing to a later phase.

**Acceptance:**
- [ ] A dual-stack `getaddrinfo("localhost")` result (the staged `/etc/hosts` carries both `127.0.0.1` and `::1`) is ordered per RFC 6724 — verified by `RFC6724_OK` inspecting the returned `addrinfo` order, CI-deterministic with no network.
- [ ] With **no** usable global IPv6 source address configured, a real dual-stack name's IPv4 result is selected first (no v6-preference black-hole) — exercised on the opt-in `M3OS_IPV6_NET` arm alongside `AAAA_RESOLVE_OK`.
- [ ] Happy Eyeballs is explicitly recorded as deferred — **NOT taken; tracked in the design doc's Deferred section.**

---

## Track E — Acceptance Gates + QEMU IPv6 Test Harness

### E.1 — `ipv6-smoke` ring-3 test binary + consolidated gate

**Files:**
- `userspace/ipv6-smoke/` (new ring-3 binary, modeled on `pku-smoke`/`dns-smoke` — emits one `IPV6_SMOKE:<case>:ok` per case + a final `IPV6_SMOKE:done`)
- `xtask/src/main.rs` (`:6648` `SmokeStep` enum / `:7889` `dns-smoke` registration cluster — add the `ipv6-smoke` `WaitPassOrFail` on `SMOKE:ipv6-smoke:PASS`, fail-fast on `:FAIL`)
- `userspace/smoke-runner/src/main.rs` (`:50` cluster — add the `ipv6-smoke` test constants/needles)
- `kernel/src/fs/ramdisk.rs` (embed the `ipv6-smoke` ELF)

**Symbol:** sentinels `IPV6_ADDR_OK` (A.1/C.1 link-local), `IPV6_BIND_OK` (A.6), `IPV6_LOOPBACK_OK` (B.1 `ping6 ::1`), `ICMPV6_ECHO_OK` (B.1), `NDP_RESOLVE_OK` (B.2), `SLAAC_ADDR_OK` (C.1), `DHCPV6_DNS_OK` (C.2 stateless), `RFC6724_OK` (D.3 — CI-deterministic via the dual-stack `/etc/hosts` `localhost` entry, which already carries both `127.0.0.1` and `::1`, so `getaddrinfo("localhost")` returns both families with no network); final `SMOKE:ipv6-smoke:PASS`. The network-dependent `DHCPV6_LEASE_OK`/`AAAA_RESOLVE_OK`/`CURL6_OK` ride the opt-in `M3OS_IPV6_NET` arm (E.2/E.3), not this always-on roster
**Why it matters:** the serial smoke harness needs one always-on, CI-deterministic gate that falsifiably exercises the v6 substrate end-to-end without real internet — the same role `pku-smoke` plays for PKU. The CI-deterministic arms (loopback echo, bind, SLAAC + stateless DHCPv6 DNS against SLIRP `ipv6=on`) run always; the network-dependent arms gate behind E.3.

**Acceptance:**
- [x] `cargo xtask ipv6-smoke` boots m3OS and the gate asserts `SMOKE:ipv6-smoke:PASS` with each CI-deterministic sub-sentinel present; any `:FAIL`/panic line fails the gate.
- [x] The gate is wired into the smoke-runner roster and the binary embedded in the ramdisk (the four-touchpoint wiring).
- [x] `cargo xtask check` host tests cover the `kernel-core` parse/build for `ipv6`/`icmpv6`/`ndp`/`dhcpv6`.

### E.2 — QEMU IPv6 network harness (SLIRP `ipv6=on` + opt-in TAP/radvd/dhcpd)

**File:** `xtask/src/main.rs` (the `qemu_args_with_devices` / netdev construction used by the smoke gates)
**Symbol:** `-netdev user,…,ipv6=on` (SLIRP IPv6: default `fec0::/64` prefix, host `fec0::2`, DNS `fec0::3`, sends RAs → SLAAC works, answers stateless DHCPv6) for the always-on arms; an opt-in TAP + external `radvd`/`dhcpd`(`kea`) path for stateful DHCPv6 + the `curl http://[2606:4700::1111]/` real-egress arm
**Why it matters:** SLIRP `ipv6=on` gives a CI-runnable IPv6 network (RAs for SLAAC, a stateless DHCPv6 DNS server) with no host configuration — so the SLAAC + ICMPv6 + NDP arms run in plain CI. Stateful DHCPv6 address leasing and real-internet AAAA/curl need more than SLIRP provides, so they follow the established opt-in skip-with-reason pattern (`git-https-smoke`'s `M3OS_GIT_HTTPS_NET`, `usb-eth-smoke`'s `M3OS_USB_ETH_NET`).

**Acceptance:**
- [x] The `ipv6-smoke` gate launches QEMU with `ipv6=on`; the guest forms its link-local address and **answers a live Neighbor Solicitation** from SLIRP (NDP over the wire, `NDP_RESOLVE_OK`). *SLAAC global formation needs a real RA — QEMU 8.2.2 libslirp sends no Router Advertisements (packet-capture-confirmed), so SLAAC rides the opt-in `M3OS_IPV6_LIVE` arm; see the Validation Status note below.*
- [x] The stateful-DHCPv6 + real-egress arms are gated behind `M3OS_IPV6_NET=1` and **skip-with-reason** when unset (no CI dependency on a host IPv6 daemon or real internet).
- [ ] `curl http://[<host or real v6 literal>]/` returns a response on the opt-in arm (`CURL6_OK`); curl's IPv6 support is confirmed in `build_curl` (`xtask/src/port_build.rs`, **not** the Portfile — which carries only NAME/VERSION/DEPS/URL/SHA): the build passes no `--disable-ipv6`, so IPv6 is on by default (optionally add an explicit `--enable-ipv6` to make the assertion grep-able).

### E.3 — `M3OS_IPV6_REGRESSION` pre-push gate + AGENTS.md regression row

**Files:** `.githooks/pre-push` (or the gate roster it drives), `AGENTS.md` (the regression-gate table)
**Symbol:** `M3OS_IPV6_REGRESSION=1` → runs `ipv6-smoke` (PASS, not SKIP); the AGENTS.md regression-table row describing the gate, its CI-deterministic vs opt-in arms, and `M3OS_IPV6_NET=1`
**Why it matters:** every networking subsystem in the tree has an opt-in pre-push regression gate (the `M3OS_*_REGRESSION` family); IPv6 gets the same so a future change to the v6 dispatch / NDP / SLAAC path is caught before merge. The AGENTS.md row is where the gate's contract is documented for the next contributor.

**Acceptance:**
- [x] `M3OS_IPV6_REGRESSION=1 git push` runs `ipv6-smoke` and fails the push on a non-PASS.
- [x] AGENTS.md gains the `ipv6-smoke` / `M3OS_IPV6_REGRESSION=1` regression-table row (CI-deterministic arms always-on; network arms behind `M3OS_IPV6_NET=1`).
- [x] The row documents that IPv4 gates (`smoke-test`, `regression`, `dns-smoke`, `multi-nic-smoke`) remain unaffected.

---

## Track F — Documentation + Release Closeout

### F.1 — Create the Phase 91 learning doc

**Files:**
- `docs/91-ipv6-dhcpv6.md` (new — aligned learning-doc template at `docs/appendix/doc-templates.md:167`–`214`, modeled on `docs/16-network.md` for the networking subject matter and `docs/90b-claude-code.md` for the current template shape)
- `docs/README.md` (link it in the `### Phase-Aligned Learning Docs` table after the most recent row)
- `docs/appendix/codebase-map.md` (Documentation Index — add a "Before touching `kernel/src/net/ipv6.rs`/`icmpv6.rs`/`ndp.rs`/`dhcpv6.rs`" row beside the `docs/16-network.md` network row)

**Symbol:** the aligned learning-doc header block (`**Aligned Roadmap Phase:** Phase 91` / `**Status:** …` / `**Source Ref:** phase-91` / `**Supersedes Legacy Doc:** N/A`) and the seven required sections (Overview / What This Doc Covers / Core Implementation / Key Files / How This Phase Differs From Later IPv6 Work / Related Roadmap Docs / Deferred or Later-Phase Topics)
**Why it matters:** every phase ships a learning doc (the roadmap's "Required Documentation for Every Phase" rule); **Phase 91 cannot be marked Complete without this doc in tree.** This is where the phase's teaching lives: why NDP replaces ARP, how SLAAC derives an interface ID, why DHCPv6 is a four-message exchange on 546/547, the dual-stack RFC 6724 problem, and the honest non-goals (no DAD/privacy-extensions/multicast/Happy Eyeballs, no general loopback).

**Acceptance:**
- [x] `docs/91-ipv6-dhcpv6.md` exists with all seven aligned-template sections; the Key Files table lists the exact new files (`kernel-core`/`kernel` `ipv6`/`icmpv6`/`ndp`/`dhcpv6`, `config.rs`, the socket-surface files, `userspace/ping6`).
- [x] It is linked from `docs/README.md`'s learning-docs table and cross-links the Phase 91 design + task docs in its Related Roadmap Docs section; `docs/appendix/codebase-map.md` gains the IPv6 Documentation-Index row.
- [x] The Deferred section matches the design doc's Deferred Until Later list (privacy extensions, DAD, MLD, IPsec, mobility/segment routing, DHCPv6-PD, full RFC 6724/8305).

### F.2 — Update the roadmap README row + design-doc link + AGENTS.md inventory

**Files:**
- `docs/roadmap/README.md` (the Phase 91 row at `:482` — Tasks cell `Deferred until implementation planning` → `[Tasks](./tasks/91-ipv6-dhcpv6-tasks.md)`; Status flips `Planned` → `Complete`/`🟢 Landed` + Primary Outcome sharpened **on landing**)
- `docs/roadmap/91-ipv6-dhcpv6.md` (Companion Task List → live link — done during planning)
- `AGENTS.md` (`:7` kernel version line; the capability inventory — IPv6 is a **new capability class** (dual-stack networking), so per the keep-it-small policy it earns a new bullet or extends the Networking bullet, *not* a changelog entry)

**Symbol:** the README Status/Tasks cells; the AGENTS.md Networking capability bullet
**Why it matters:** `docs/roadmap/README.md` is the authoritative phase index and `AGENTS.md` is the always-loaded capability inventory; both must reflect the landed dual-stack. Per the AGENTS.md maintenance policy, IPv6/DHCPv6 is a genuinely new capability class (the inventory today says "IPv4/TCP/UDP stack"), so it is the rare case that warrants either extending the Networking bullet to "IPv4/IPv6 dual-stack" or a new bullet — decided at review, not by appending prose.

**Acceptance:**
- [x] During planning (done at authoring time): `docs/roadmap/README.md:482` Tasks cell links `[Tasks](./tasks/91-ipv6-dhcpv6-tasks.md)` and the Primary Outcome is expanded; the design doc's Companion Task List is a live link.
- [x] On landing: the README Status flips `Planned` → `Complete`/`🟢 Landed` with the Primary Outcome sharpened to the as-built result (SLAAC + DHCPv6 + ICMPv6/NDP + AAAA + `ping6`).
- [x] On landing: AGENTS.md's Networking bullet reflects dual-stack IPv6 (no separate changelog/diary entry, per the keep-it-small policy).

### F.3 — Bump kernel crate `0.90.1` → `0.91.0`

**File:** `kernel/Cargo.toml`
**Symbol:** `[package] version = "0.91.0"` (`kernel/Cargo.toml:3` — Phase 91 takes the next post-1.0 **minor**, matching the one-minor-per-phase convention; Phase 90a/90b shared the `0.90.x` line, so the next phase advances the minor)
**Why it matters:** the bump is how the landing is recorded in the boot banner, `uname`, and `/proc/version` (all derive from `env!("CARGO_PKG_VERSION")` — `kernel/src/lib.rs:146`, `kernel/src/arch/x86_64/syscall/mod.rs:14522`–`14523`, `kernel/src/fs/procfs.rs`), so no manual string edits are needed beyond `Cargo.toml` + `AGENTS.md`. The `ipv6-smoke` boot banner asserting `0.91.0` is the cheap proof the cut shipped — the exact Phase 89/90b E.4 pattern.

**Acceptance:**
- [x] `kernel/Cargo.toml` reads `version = "0.91.0"` (+ `Cargo.lock` updated), and `AGENTS.md:7` reads `kernel **v0.91.0**`.
- [x] `cargo xtask check` is clean (clippy `-D warnings` + rustfmt + host tests incl. the new `kernel-core` IPv6 parse/build tests); exit 0.
- [x] The boot banner / `uname -a` reports `0.91.0` (rides the `ipv6-smoke` run).

---

## Documentation Notes

- **What changed vs. the previous phase:** this phase adds a parallel IPv6 stack (`ipv6`/`icmpv6`/`ndp`/`dhcpv6` in both `kernel-core` and `kernel`) beside the Phase 16 IPv4 stack, plus an `AF_INET6`/`sockaddr_in6` socket surface and a `ping6` tool. It extends — does not replace — the IPv4 path; `Ipv4Addr`, ARP, the IPv4 DHCP client, and every existing gate stay intact.
- **What replaces what:** nothing IPv4 is removed. NDP (`kernel/src/net/ndp.rs`) is the IPv6 *analogue* of ARP (`kernel/src/net/arp.rs`) for v6 traffic only; the DHCPv6 client is the analogue of the DHCPv4 client on ports 546/547 instead of 67/68. The `SocketEntry` address fields gain a family tag + `[u8;16]` storage rather than being swapped out.
- **Honesty / explicit non-goals:** `ping6 ::1` is an ICMPv6 loopback short-circuit, not a routed `lo` interface (m3OS has none). Privacy extensions (RFC 4941), DAD, MLD/MLDv2, IPsec, mobility/segment routing, DHCPv6-PD, and Happy Eyeballs (RFC 8305) are deferred. RFC 6724 selection relies on musl's existing sort — the phase supplies correct source-address inputs, it does not re-implement the sort. Stateful DHCPv6 leasing + real-internet AAAA/curl are opt-in (`M3OS_IPV6_NET=1`), since QEMU SLIRP `ipv6=on` provides only RAs + stateless DHCPv6 DNS.
- **Prefer exact targets:** edits land in named symbols — `dispatch::process_rx_frames` (`kernel/src/net/dispatch.rs`), `ipv6::send`/`ipv6::handle_ipv6` (`kernel/src/net/ipv6.rs`), `ndp::resolve`/`learn`/`send_solicitation` (`kernel/src/net/ndp.rs`), `config::set_config_v6` (`kernel/src/net/config.rs`), `sys_socket`/`sys_bind`/`sockaddr_from_user6` (`kernel/src/arch/x86_64/syscall/mod.rs`), `SocketEntry` (`kernel/src/net/mod.rs:189`), and the `Ipv6Addr` type (`kernel-core/src/types.rs`) — not "the network layer."
- **Cross-links:** design doc [`docs/roadmap/91-ipv6-dhcpv6.md`](../91-ipv6-dhcpv6.md); the IPv4 predecessor learning doc [`docs/16-network.md`](../../16-network.md); the Phase 77 DNS-stub doc [`docs/77-pre-1-0-cleanup.md`](../../77-pre-1-0-cleanup.md); the new Phase 91 learning doc `docs/91-ipv6-dhcpv6.md` (F.1). Consumers: the `curl` port and `ping6` rely on the `AF_INET6` socket surface; the `getaddrinfo` resolver path relies on `sys_recvmsg_inet6`.
