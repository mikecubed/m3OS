# IPv6 and DHCPv6

**Aligned Roadmap Phase:** Phase 91
**Status:** Complete
**Source Ref:** phase-91
**Supersedes Legacy Doc:** N/A

## Overview

Phase 91 layers a **dual-stack IPv6 path** onto the previously IPv4-only network
stack (Phase 16). It is additive, not a rewrite: every existing IPv4 code path
keeps working untouched, and the new IPv6 code lives beside it — a parallel
header format, a parallel neighbor-discovery protocol, a parallel auto-config
client, and a third socket address family threaded through the *same* socket
table the IPv4 and AF_UNIX sockets already use. The headline deliverable is the
always-on `cargo xtask ipv6-smoke` gate, which **PASSES** and validates the new
stack *live on real frames*: the guest forms its link-local address from its
MAC, answers QEMU SLIRP's Neighbor Solicitation with a Neighbor Advertisement
(packet-capture-confirmed), creates `AF_INET6` sockets, binds them with a
28-byte `sockaddr_in6`, and round-trips `ping6 ::1` through the ICMPv6 loopback
short-circuit. The pieces that a SLIRP host cannot exercise — SLAAC global
address formation and DHCPv6 DNS — are fully implemented and host-tested (the
new kernel-core IPv6 modules — `ipv6`/`icmpv6`/`ndp`/`dhcpv6` — carry 46 unit
tests, with further v6 coverage in `udp`/`tcp` `build_v6` and the address
helpers) but live-validate only behind
the opt-in `M3OS_IPV6_LIVE` arm against a real router, because QEMU 8.2.2's
libslirp sends no Router Advertisements and runs no DHCPv6 server. This is the
honest boundary of the phase: the base layer, ICMPv6/NDP, the AF_INET6 socket
surface, and the loopback path are proven on the wire in CI; the
router-dependent autoconfig paths are proven by host tests plus an opt-in live
arm.

## What This Doc Covers

- The IPv6 base layer: the `Ipv6Addr` type and its classification/derivation
  helpers, the 40-byte fixed header, the mandatory pseudo-header checksum, and
  the extension-header walk — and how each deliberately differs from IPv4.
- The `AF_INET6` socket surface: a third family branch in the socket syscalls
  and a family-tagged `SocketEntry` so one socket table carries v4 and v6.
- ICMPv6 and NDP (Neighbor Discovery): why NDP replaces ARP, runs *over*
  ICMPv6 to the solicited-node multicast address, and what the four NDP message
  types do — plus the `ping6 ::1` internal-loopback trick.
- SLAAC and the DHCPv6 client: why IPv6 splits host configuration across Router
  Advertisements and a four-message DHCPv6 exchange, and how the RA's M/O flags
  decide which path applies.
- AAAA DNS resolution and dual-stack address selection, and why the AAAA
  *record type* rides the existing IPv4 UDP transport.
- The validation story: what `ipv6-smoke` proves on real frames, what the SLIRP
  limitation forces behind `M3OS_IPV6_LIVE`, and which dual-stack pieces are
  deferred.

## Core Implementation

### Track A — the IPv6 base layer and AF_INET6 sockets

**The address type.** An IPv6 address is `pub type Ipv6Addr = [u8; 16]` in
`kernel-core/src/types.rs`. Around it sit the classification helpers a stack
needs at every decision point: `ipv6_is_loopback` (`::1`), `ipv6_is_unspecified`
(`::`, the all-zeros "no source yet" address), `ipv6_is_link_local`
(`fe80::/10`), and `ipv6_is_multicast` (`ff00::/8`). On top of those are the
*derivation* helpers that make IPv6 autoconfiguration possible without a server:

- `eui64_from_mac` — Modified EUI-64. It expands a 48-bit MAC into a 64-bit
  interface identifier by inserting the bytes `ff:fe` in the middle and flipping
  the **U/L (universal/local) bit** (bit 1 of the first byte). This is the
  bridge between an L2 MAC and an L3 interface ID.
- `link_local_from_mac` — prepends the `fe80::/64` prefix to that EUI-64 to
  produce the link-local address the host can use *before any router or DHCP
  server has spoken*. Every IPv6 interface needs a link-local address just to
  run NDP, so this is computed at init.
- `solicited_node_multicast` — derives `ff02::1:ffXX:XXXX` from an address by
  appending its low 24 bits to the solicited-node prefix. This is the multicast
  group NDP uses instead of L2 broadcast (see Track B).
- `slaac_address` — combines a router-supplied prefix with the EUI-64 interface
  ID to form a global, routable address (see Track C).

**The header (`kernel-core/src/net/ipv6.rs`).** The IPv6 header is **40 bytes,
fixed** — contrast IPv4's variable 20+ bytes with its options. Three IPv4
features are deliberately *absent*: there is **no header checksum** (IPv6 leans
on L2 and the transport checksum instead), there is **no in-header
fragmentation** (fragmentation moves into an optional extension header and is
sender-only), and there are no options in the base header (they move to
extension headers too). What IPv6 *adds* is the **pseudo-header checksum** (RFC
8200 §8.1): the upper-layer checksum for ICMPv6, UDP, and TCP over IPv6 must
cover a pseudo-header built from the source/destination addresses, the payload
length, and the next-header value. Critically, this checksum is **mandatory**
over IPv6 — including for UDP, where over IPv4 it is optional and m3OS's IPv4
UDP simply sets it to zero. The module also implements a bounded
**locate-and-skip** walk over the extension-header chain (HOPOPT / Routing /
Fragment / Destination-Options): it follows the `next_header` links far enough
to find the real upper-layer protocol, with a hard iteration cap so a malformed
or adversarial chain cannot loop the parser.

**Dispatch.** `kernel/src/net/dispatch.rs` gains an EtherType `0x86DD` arm
(`ethernet::ETHERTYPE_IPV6`) beside the existing `0x0800` (IPv4) and `0x0806`
(ARP) arms; it routes the frame into `ipv6::handle_ipv6`, the v6 analogue of the
IPv4 ingress path.

**The AF_INET6 socket surface.** `AF_INET6` is family **10**, and its address
structure `sockaddr_in6` is **28 bytes** (vs `sockaddr_in`'s 16). The kernel
adds a *third* family branch — beside AF_UNIX and AF_INET — to `sys_socket`,
`bind`, `connect`, `sendto`, and `recvfrom` in
`kernel/src/arch/x86_64/syscall/mod.rs`, with `sockaddr_from_user6` /
`sockaddr_to_user6` translating the 28-byte structure to and from userspace. The
key design choice is reuse: rather than a separate v6 socket table, the existing
`SocketEntry` gains a `family` tag and a `[u8; 16]` address field, so the *same*
table holds both AF_INET and AF_INET6 sockets and the dispatch logic branches on
the tag. Userspace gets the matching ABI in `syscall-lib` — `AF_INET6`,
`SockaddrIn6`, and `bind6`/`connect6`/`sendto6`/`recvfrom6` wrappers — and the
`SockaddrIn6` layout is duplicated (not shared) between kernel-core and
syscall-lib, with offset tests on both sides asserting the 28-byte musl-matching
shape.

### Track B — ICMPv6 and NDP (why NDP replaces ARP)

**ICMPv6 (`icmpv6.rs`).** ICMPv6 is to IPv6 what ICMP is to IPv4, but it carries
*more*: alongside Echo Request/Reply it is the transport for all of NDP. Its
checksum uses the v6 pseudo-header (ICMPv4 has no pseudo-header at all), so the
ICMPv6 module and the NDP module both build on the Track A checksum primitive.

**NDP (`ndp.rs`, RFC 4861) — IPv6's ARP, restructured.** IPv4 uses ARP, an
L2.5 protocol, to answer "what MAC owns this IPv4 address?" by **broadcasting**
on the wire. IPv6 abolishes ARP and does the same job with Neighbor Discovery,
which runs *over ICMPv6* and sends its queries to the target's
**solicited-node multicast** address (`ff02::1:ffXX:XXXX`) rather than L2
broadcast — so only hosts whose address shares those low 24 bits are interrupted,
not the whole segment. NDP defines four message types, which split ARP's single
job into address resolution *and* router/prefix discovery:

- **Neighbor Solicitation (135) / Neighbor Advertisement (136)** — the direct
  ARP-request/ARP-reply replacement: "who has this address, and what is your
  link-layer address?"
- **Router Solicitation (133) / Router Advertisement (134)** — router and prefix
  discovery, which IPv4 does *not* have an L3 protocol for. The RA carries the
  on-link **prefix** (for SLAAC), the **default route**, the **M/O flags** that
  steer autoconfiguration (see Track C), and **RDNSS** DNS-server options.

m3OS keeps a **16-entry neighbor cache** mirroring the existing 16-entry ARP
cache, and learns entries passively from the NS/NA traffic it sees, exactly as
the ARP cache learns from ARP replies.

**`ping6 ::1` is an internal loopback.** m3OS has **no `lo` device** — there is
no general loopback interface, and even IPv4 `127.0.0.1` is not routed; the RX
path only sees frames that arrive from a NIC. So `ping6 ::1` cannot work by
routing. Instead, `ipv6::send_from` detects a self-addressed packet (one
destined for `::1` or any address the host has assigned itself) and **feeds it
back into the RX path** without touching the wire. The smoke gate proves this is
genuinely loopback-only by asserting the TX counter does not advance — no frame
reached `net::send_frame`.

### Track C — SLAAC and DHCPv6 (why IPv6 splits config across RA and DHCPv6)

IPv4 hands a host everything in one DHCP exchange: address, gateway, DNS, lease.
IPv6 deliberately splits this. The **RA carries the routing information**
(prefix + default route); **DHCPv6 carries the supplemental config** (DNS, etc.);
and the RA's **M (Managed) and O (Other) flags** tell the host which path to run:

- **SLAAC (StateLess Address AutoConfiguration).** The host forms its own
  addresses with no server: the **link-local** address from EUI-64 at init
  (needed before NDP can even run), and a **global** address as RA-prefix ++
  EUI-64 once an RA arrives. SLAAC alone gives address + default route.
- **DHCPv6 (`dhcpv6.rs`, RFC 8415)** runs when the RA's flags request it, and
  supplies what SLAAC cannot. It is a **four-message** exchange —
  **Solicit / Advertise / Request / Reply** — on UDP **port 546 → 547** to the
  all-DHCP-servers multicast `ff02::1:2`. Contrast DHCPv4's DISCOVER / OFFER /
  REQUEST / ACK on ports 67/68. The client identifies itself with a **DUID-LL**,
  requests an address via **IA_NA / IAADDR**, and parses **DNS_SERVERS**
  (RFC 3646). A **stateless Information-Request** variant fetches DNS only
  (no address) when the O flag is set but the M flag is not.

The DHCPv6 client mirrors the DHCPv4 client idiom introduced in PR #237, but
with one important difference: it runs on the **virtio/SLIRP path and is not
RemoteNic-gated**. IPv4's DHCP client has a static-IP fallback to lean on; IPv6
has none, so the autoconfig path must be unconditionally available.

### Track D — DNS (AAAA) and dual-stack selection

DNS gains **AAAA** record support. Two design points are worth internalizing:

1. **Runtime DNS-server storage** lives in `kernel/src/net/config.rs`
   (`dns_servers` / `add_dns_server`), populated by the RA's RDNSS option and by
   DHCPv6's DNS_SERVERS option. This is the v6 counterpart to where IPv4's DHCP
   lease deposits its DNS server.
2. **AAAA resolution rides the existing IPv4 UDP transport.** A AAAA query is an
   ordinary DNS query whose *record type* is AAAA; the transport carrying it does
   not itself need to be IPv6. So m3OS resolves IPv6 addresses over the v4 UDP
   path it already had — the record type and the transport are orthogonal.

**Dual-stack selection (RFC 6724)** is handled mostly by the prebuilt musl,
which already applies an RFC 3484/6724 sorting *subset* to `getaddrinfo`
results. m3OS does not re-implement the rule set; its job is to **supply the
source-address/scope inputs** that subset consumes (and to gate v6-preference on
whether a usable global v6 source is actually configured). The naive
"always prefer v6" rule famously degraded user experience in the early 2010s;
Happy Eyeballs (RFC 8305) was the eventual fix and is deferred here.

### Validation honesty

The always-on `cargo xtask ipv6-smoke` gate **PASSES** and validates these
arms *live on real frames* through QEMU SLIRP with `ipv6=on`: link-local
formation, **bidirectional NDP** (the guest answers SLIRP's Neighbor
Solicitation with an NA — packet-capture-confirmed), AF_INET6 socket creation,
`bind6`, and `ping6 ::1` via the loopback short-circuit. It ends on
`SMOKE:ipv6-smoke:PASS` and fails fast on any `:FAIL` / `IPV6_SMOKE:panic`.

The **SLIRP limitation** discovered during bring-up is the key caveat to teach:
QEMU 8.2.2's libslirp answers NDP Neighbor Solicitations but **sends no Router
Advertisements and runs no DHCPv6 server** (packet-capture-confirmed). Because
SLAAC's global-address formation depends on an RA prefix and DHCPv6 depends on a
server, those two arms are **implemented and host-tested** (the 46 kernel-core
unit tests across the new `ipv6`/`icmpv6`/`ndp`/`dhcpv6` modules, plus the
`udp`/`tcp` `build_v6` + RA-decision tests) but can only live-validate behind the
opt-in **`M3OS_IPV6_LIVE`** arm, which requires a real IPv6 router. That arm
attaches the guest to a real LAN via `M3OS_IPV6_TAP=<ifname>` — a TAP bridged to
a segment that has a router — instead of SLIRP. **SLAAC was demonstrated
end-to-end against a real home router** this way: the guest received the
router's Router Advertisement, formed a real `/64` global address, and the full
`ipv6-smoke` gate PASSed. The real-router run also surfaced two robustness
items, now landed: an **RFC 4861 Router-Solicitation retransmit** (up to three,
~4 s apart, until a global address is configured, so a single dropped RA does
not strand the host) and concise **RA-reception diagnostics**. Per-run
acquisition is nonetheless best-effort — bounded by the router's RA cadence and
the deferred **MLD** (without MLD the guest never formally joins the all-nodes
group `ff02::1`, so multicast RA delivery across a bridge is not guaranteed) —
which is why these arms stay opt-in and skip-with-reason in CI rather than
silently passing.

## Key Files

| File | Purpose |
|---|---|
| `kernel-core/src/types.rs` | `Ipv6Addr` type + classification helpers (loopback/unspecified/link-local/multicast) + `eui64_from_mac`, `solicited_node_multicast`, `link_local_from_mac`, `slaac_address` |
| `kernel-core/src/net/ipv6.rs` | 40-byte fixed header parse/build, pseudo-header checksum, bounded extension-header locate-and-skip walk |
| `kernel-core/src/net/icmpv6.rs` | ICMPv6 framing + pseudo-header checksum, Echo + NDP message carriage |
| `kernel-core/src/net/ndp.rs` | NDP (RFC 4861) message framing: NS/NA (135/136), RS/RA (133/134), prefix/route/RDNSS/M-O parsing |
| `kernel-core/src/net/dhcpv6.rs` | DHCPv6 (RFC 8415) Solicit/Advertise/Request/Reply + Information-Request, DUID-LL, IA_NA/IAADDR, DNS_SERVERS |
| `kernel-core/src/net/ethernet.rs` | `ETHERTYPE_IPV6` (`0x86DD`) constant |
| `kernel-core/src/net/mod.rs` | `SockaddrIn6` (28-byte musl-matching layout) + offset tests |
| `kernel/src/net/ipv6.rs` | In-kernel IPv6 ingress (`handle_ipv6`), egress (`send_from`), and the `::1`/own-address loopback short-circuit |
| `kernel/src/net/icmpv6.rs` | In-kernel ICMPv6 Echo handling + NDP message I/O |
| `kernel/src/net/ndp.rs` | 16-entry neighbor cache + passive learning + NS/NA responder + RS/RA handling |
| `kernel/src/net/dhcpv6.rs` | In-kernel DHCPv6 client state machine over UDP 546→547 to `ff02::1:2` |
| `kernel/src/net/config.rs` | v6 interface state (link-local/global addresses) + runtime `dns_servers`/`add_dns_server` |
| `kernel/src/net/dispatch.rs` | EtherType `0x86DD` dispatch arm → `ipv6::handle_ipv6` |
| `kernel/src/net/udp.rs` | UDP-over-IPv6 path (pseudo-header checksum, v6 send/recv) |
| `kernel/src/net/mod.rs` | `SocketEntry` family tag + `[u8;16]` address fields so one socket table carries both families |
| `kernel/src/arch/x86_64/syscall/mod.rs` | AF_INET6 (10) branch in `sys_socket`/`bind`/`connect`/`sendto`/`recvfrom`; `sockaddr_from_user6`/`sockaddr_to_user6` |
| `kernel/src/lib.rs` | `v6_tick` periodic driver (NDP/DHCPv6 timers) wired into the net tick |
| `userspace/syscall-lib` | Userspace ABI: `AF_INET6`, `SockaddrIn6`, `bind6`/`connect6`/`sendto6`/`recvfrom6` |
| `userspace/ping6/` | `ping6` userspace tool (loopback + manual-peer echo) |
| `userspace/ipv6-smoke/` | Ramdisk acceptance binary driving the `ipv6-smoke` gate |

## How This Phase Differs From Later IPv6 Work

- **This phase introduces** the dual-stack base layer: the IPv6 header, ICMPv6,
  NDP neighbor resolution, SLAAC + a DHCPv6 client, AAAA DNS, and the AF_INET6
  socket surface — enough for link-local + global addressing, neighbor
  discovery, UDP/ICMPv6/**TCP** over IPv6, `ping6`, and dual-stack `getaddrinfo`.
- **Full dual-stack TCP over IPv6 is included.** `handle_tcp_v6` + a
  family-aware `TcpConnection` (v6 addresses, IPv6 pseudo-header checksum, v6
  send path) complete a real three-way handshake + data transfer; the always-on
  `ipv6-smoke` `tcp` case proves it over the `::1` internal loopback. The
  `CURL6_OK` real-internet variant stays opt-in (it needs a routable *global*
  v6 address, which requires a real router SLIRP does not provide).
- **Address autoconfiguration is trust-on-first-use here.** This phase forms
  addresses from EUI-64 only and trusts SLAAC's uniqueness assumption — there is
  no **DAD** (Duplicate Address Detection) and no **privacy extensions**
  (RFC 4941 randomized IIDs); a later phase adds those.
- **Selection is delegated, not owned.** RFC 6724 selection rides musl's
  existing sort with m3OS supplying inputs; the full RFC 6724/8305 (Happy
  Eyeballs) behavior is later work.

## Related Roadmap Docs

- [Phase 91 design doc](./roadmap/91-ipv6-dhcpv6.md)
- [Phase 91 task list](./roadmap/tasks/91-ipv6-dhcpv6-tasks.md)
- [Phase 16 — IPv4 network stack (predecessor)](./16-network.md)
- [Phase 77 — pre-1.0 cleanup / DNS-resolver stub (extended here with AAAA)](./77-pre-1-0-cleanup.md)

## Deferred or Later-Phase Topics

- **Privacy extensions** (RFC 4941, randomized interface IDs).
- **DAD** (Duplicate Address Detection) before claiming an address.
- **MLD / MLDv2** multicast group management.
- **IPsec** (AH / ESP) for IPv6.
- **IPv6 mobility, NPTv6, segment routing.**
- **DHCPv6-PD** (prefix delegation, for routers).
- **RFC 8305 (Happy Eyeballs)** connection racing — this phase supplies inputs
  to musl's RFC 6724 sorting subset (now validated by `dns6-smoke`) but does not
  race connections.
- **Live SLAAC / DHCPv6 over a real router** and the `CURL6_OK` real-internet
  TCP arm — implemented + host-tested, but live-validate only behind the opt-in
  `M3OS_IPV6_LIVE` arm (QEMU's libslirp sends no RAs / runs no DHCPv6 server).
- **The AAAA + RFC 6724 musl `getaddrinfo` arm** end-to-end validation.
- **Live SLAAC / DHCPv6 validation** — needs a real IPv6 router; QEMU 8.2.2's
  SLIRP sends no Router Advertisements and runs no DHCPv6 server, so these run
  only behind the opt-in `M3OS_IPV6_LIVE` arm.
