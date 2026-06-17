# Phase 91 - IPv6 / DHCPv6

**Status:** Planned (post-1.0)
**Source Ref:** phase-91
**Depends on:** Phase 16 (Network) ✅, Phase 77 (Pre-1.0 Correctness — TCP retransmission + DNS stub) ✅, Phase 83 (Release 1.0 Gate) ✅
**Builds on:** Adds IPv6 + DHCPv6 + IPv6-aware DNS to the IPv4-only 1.0 network stack
**Primary Components:** `kernel-core/src/net/ipv6.rs` + `kernel/src/net/ipv6.rs` (new), `kernel-core/src/net/icmpv6.rs` + `kernel/src/net/icmpv6.rs` (new), `kernel-core/src/net/ndp.rs` + `kernel/src/net/ndp.rs` (new — Neighbor Discovery Protocol), `kernel-core/src/net/dhcpv6.rs` + `kernel/src/net/dhcpv6.rs` (new), the EtherType demux in `kernel/src/net/dispatch.rs` and dual-stack config in `kernel/src/net/config.rs` (extended), the `AF_INET6` / `sockaddr_in6` socket surface in `userspace/syscall-lib/src/lib.rs` + `kernel/src/arch/x86_64/syscall/mod.rs` (extended), AAAA resolution through the Phase 77 resolver path, and a new `userspace/ping6`

## Milestone Goal

m3OS speaks IPv6 — receives a SLAAC address via Router Advertisement, optionally augments it with a DHCPv6-assigned address + DNS server, resolves AAAA records via the Phase 77 DNS resolver stub, and uses an IPv6 default route. The dual-stack policy follows RFC 6724 for address selection.

## Why This Phase Exists

Phase 74a §1 row 15 grades IPv6 / DHCPv6 absence as MEDIUM and explicitly accepts it as a 1.0 deferral. This phase makes good on the deferral after Phase 83 — IPv6 is increasingly the default in residential and enterprise networks, and a post-1.0 m3OS that cannot speak it is a measurably less useful tool.

## Learning Goals

- Understand how IPv6 addressing differs from IPv4: 128-bit addresses, link-local + global scope, no broadcast, NDP instead of ARP
- See how Stateless Address Autoconfiguration (SLAAC) derives an interface ID from MAC + Router Advertisement prefix
- Learn how DHCPv6 differs from DHCPv4 (UDP/546 + 547, four-message exchange — Solicit/Advertise/Request/Reply)
- Understand the dual-stack address-selection problem (RFC 6724) and why naive "prefer IPv6" hurts user experience on misconfigured networks
- See how ICMPv6 carries both error reporting (analogous to ICMPv4) and the NDP control plane (Neighbor Solicitation, Neighbor Advertisement, Router Solicitation, Router Advertisement)

## Feature Scope

### Track A — IPv6 base

- **A.1** — `Ipv6Addr` type + on-wire framing of the IPv6 header.
- **A.2** — Extension-header parser (Hop-by-Hop, Routing, Fragment, Destination Options) — only those needed for 1.0+ workloads; defer the full set.
- **A.3** — `sockaddr_in6` + `AF_INET6` socket family in the existing socket-layer code.

### Track B — ICMPv6 + NDP

- **B.1** — ICMPv6 Echo Request/Reply, Destination Unreachable, Packet Too Big.
- **B.2** — NDP: Neighbor Solicitation / Advertisement (link-layer address resolution — IPv6's ARP replacement).
- **B.3** — NDP: Router Solicitation / Advertisement — read the prefix and default-gateway info from received RAs to drive SLAAC.

### Track C — SLAAC + DHCPv6

- **C.1** — SLAAC: form the EUI-64 interface ID from the MAC, combine with the RA prefix, generate the global IPv6 address. Privacy extensions (RFC 4941 randomized IID) — deferred.
- **C.2** — DHCPv6 client: four-message Solicit/Advertise/Request/Reply exchange. Stateful address + DNS option (RFC 3646) parsing.

### Track D — DNS + dual-stack selection

- **D.1** — AAAA record support in the Phase 77 DNS resolver stub. `getaddrinfo` returns both A and AAAA addresses, address-selection per RFC 6724.
- **D.2** — Happy Eyeballs (RFC 8305) connect-attempt racing between v4 and v6 — deferred unless trivial.

## Important Components and How They Work

### NDP vs ARP

IPv4 uses ARP (Layer 2.5) to resolve "what MAC corresponds to this IPv4 address." IPv6 replaces this with NDP, layered over ICMPv6, with the same conceptual job but more structure: separate request/reply messages for neighbor discovery, router discovery, and prefix announcement. NDP is also how routers tell hosts "use this prefix for SLAAC and this gateway as the default route" — IPv4's DHCP carries this; IPv6 splits the routing-information part into RA and the optional-config part into DHCPv6.

### Stateless vs. Stateful autoconfig

SLAAC alone gives the host an address and a default route. DHCPv6 (when the RA's "managed" bit is set) adds DNS servers, NTP servers, search domains, etc. Modern networks usually run both — SLAAC for addressing, DHCPv6 for the supplemental config. The dual-stack policy must understand that the IPv4 DHCP lease and the IPv6 SLAAC/DHCPv6 exchange can both happen on the same interface concurrently.

### RFC 6724 address selection

When both endpoints support v4 and v6, the kernel must pick which family to connect over. RFC 6724 specifies a priority-based ordered rule set. The naive "always prefer v6" rule famously broke user experience in the early 2010s — Happy Eyeballs (RFC 8305) was the eventual fix, racing both connection attempts and using whichever completed first. m3OS's prebuilt musl already applies an RFC 3484/6724 sorting *subset* to `getaddrinfo` results (it does not do glibc's full per-destination source-address probe); the work in this phase is supplying the source-address/scope inputs that subset consumes — and gating v6-preference on whether a usable global v6 source is actually configured — not re-implementing the rule set.

### Loopback and the `::1` acceptance test

m3OS has **no general loopback interface** — there is no `lo` device, and even IPv4 `127.0.0.1` is not routed (Phase 89's `node-smoke` notes egress is proven over the real TCP path precisely because there is no loopback). The `kernel/src/net/dispatch.rs` RX path only sees frames that arrive from a NIC driver. So the `ping6 ::1` acceptance criterion cannot be satisfied by a routed loopback; instead the ICMPv6 echo handler short-circuits Echo Requests addressed to `::1` (and to any address the host has assigned to itself) by synthesizing the Echo Reply locally, without ever touching the wire. This mirrors how the existing userspace `ping` is served — there is no loopback NIC, the kernel answers directly.

## How This Builds on Earlier Phases

- Extends Phase 16's IPv4 network stack with the parallel IPv6 stack.
- Extends Phase 77's DNS resolver stub with AAAA record support.
- Extends the socket layer with `AF_INET6` (existing kernel infrastructure already supports family-tagged sockets).

## Implementation Outline

1. Land `Ipv6Addr` + header framing + the EtherType `0x86DD` demux hook in `dispatch.rs` + the `AF_INET6` / `sockaddr_in6` socket family in the existing socket-layer code.
2. Land ICMPv6 Echo (with an internal `::1` / own-address loopback short-circuit, since m3OS has no general loopback interface) so `ping6 ::1` works and `ping6 <peer>` works against a manually configured address.
3. Land NDP + SLAAC; verify automatic address acquisition on a QEMU network with a Router Advertisement Daemon (`radvd`).
4. Land DHCPv6 client; verify against a `dhcpd` IPv6 pool.
5. Wire AAAA in the resolver; verify `getaddrinfo("github.com", ...)` returns v6 + v4 addresses.
6. Implement / validate the RFC 6724 selection rules (musl sorts; ensure correct source-address inputs).
7. Bump kernel to the next post-1.0 minor version (`0.90.1` → `0.91.0`) and land the Phase 91 learning doc (`docs/91-ipv6-dhcpv6.md`).

## Acceptance Criteria

- `ping6 ::1` reports a reply via the ICMPv6 loopback short-circuit (m3OS has no `lo` device — verified by an unchanged TX counter proving no frame reached `net::send_frame`; the `IPV6_LOOPBACK_OK` sentinel).
- On a QEMU dual-stack network (SLIRP `ipv6=on`, which sends Router Advertisements) m3OS forms a global SLAAC address automatically (`SLAAC_ADDR_OK`); the stateful-DHCPv6 lease arm is opt-in against an external `radvd`/`dhcpd` (`M3OS_IPV6_NET=1`).
- `curl http://[<IPv6 literal, e.g. 2606:4700::1111>]/` returns an HTTP response over IPv6 (opt-in `M3OS_IPV6_NET=1`; `CURL6_OK`).
- `getaddrinfo("github.com", ...)` returns both IPv4 (A) and IPv6 (AAAA) addresses (`AAAA_RESOLVE_OK`); the result is ordered per RFC 6724 (`RFC6724_OK`, inspecting the returned `addrinfo` order).
- No regression in IPv4 — the `smoke-test`, `regression`, and `dns-smoke` gates stay green, including the Phase 77 retransmission + multi-slot work.

## Companion Task List

- [Phase 91 Task List](./tasks/91-ipv6-dhcpv6-tasks.md)

## How Real OS Implementations Differ

- Linux's IPv6 stack is enormous (full RFC 8200 coverage, IPsec, MIPv6, multicast routing, Segment Routing v6, ...). m3OS at this phase ships base IPv6, NDP, SLAAC, DHCPv6, AAAA — the minimum credible dual-stack.
- Real OSes implement privacy extensions (RFC 4941) by default; m3OS at this phase uses EUI-64 only (deferred).
- Production OSes participate in DAD (Duplicate Address Detection) before claiming an address; m3OS at this phase trusts SLAAC's uniqueness assumption.
- IPv6 multicast (MLD, MLDv2) — deferred.
- IPv6 mobility, NPTv6, segment routing — deferred.
- Happy Eyeballs (RFC 8305) and connection racing — deferred unless trivial.

## Deferred Until Later

- Privacy extensions (RFC 4941)
- DAD (Duplicate Address Detection)
- MLD / MLDv2 multicast group management
- IPSec / AH / ESP
- 6LoWPAN, mobility, segment routing
- DHCPv6-PD (prefix delegation, for routers)
- Full RFC 6724 + 8305 complete behavior — initial implementation may be coarse
