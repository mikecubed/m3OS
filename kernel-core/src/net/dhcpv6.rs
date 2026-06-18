//! DHCPv6 client protocol logic — RFC 8415.
//!
//! This module is **transport-agnostic**: it builds raw DHCPv6 message bytes
//! and parses server replies, but never performs I/O. The kernel glue layer
//! (not this module) is responsible for sending the resulting bytes over a UDP
//! socket bound to the client port 546 and directed to the multicast address
//! [`ALL_DHCP_SERVERS`] (`ff02::1:2`) on the server port 547.
//!
//! Unlike BOOTP/DHCPv4, a DHCPv6 message has no fixed BOOTP header — just a
//! 1-byte message type, a 3-byte transaction ID, and a flat list of TLV
//! options (RFC 8415 §8). Options may themselves encapsulate sub-options (e.g.
//! IA_NA → IA Address).
//!
//! # Two exchange flavours
//!
//! ## Stateful (address lease, RFC 8415 §18)
//!
//! ```text
//! Client                          Server
//!   |--- SOLICIT (mcast) ---------->|
//!   |<-- ADVERTISE -----------------|
//!   |--- REQUEST (mcast) ---------->|
//!   |<-- REPLY ---------------------|
//! ```
//!
//! The client multicasts a SOLICIT, a server replies with ADVERTISE offering an
//! address (and the server's DUID). The client multicasts a REQUEST echoing the
//! server's DUID + the offered address, and the server confirms with REPLY.
//!
//! ## Stateless (DNS-only, RFC 8415 §19.1.2)
//!
//! ```text
//! Client                          Server
//!   |--- INFORMATION-REQUEST ------>|
//!   |<-- REPLY ---------------------|
//! ```
//!
//! No address is leased; the client just asks for configuration (DNS servers).
//! This is the path the kernel driver drives by default. It is **not**
//! CI-deterministic under QEMU SLIRP (`ipv6=on`), which runs no DHCPv6 server,
//! so it is host-tested here and live-validated only behind the opt-in
//! `M3OS_IPV6_LIVE` arm against a real router.
//!
//! # Usage
//!
//! ```rust,ignore
//! let mut client = Dhcpv6Client::new();
//! let mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
//! let solicit = client.start_solicit(mac, [0x12, 0x34, 0x56]);
//! // … send solicit to [ff02::1:2]:547 from port 546 …
//!
//! if let Some(reply) = parse_reply(&server_udp_payload) {
//!     match client.on_reply(reply) {
//!         Dhcpv6Action::SendRequest(req) => { /* send req to [ff02::1:2]:547 */ }
//!         Dhcpv6Action::Bound(cfg) => { /* apply cfg.address, cfg.dns_servers … */ }
//!         Dhcpv6Action::Ignore => {}
//!     }
//! }
//! ```

use alloc::vec::Vec;

use crate::types::{Ipv6Addr, MacAddr};

// ---------------------------------------------------------------------------
// DHCPv6 wire constants (RFC 8415)
// ---------------------------------------------------------------------------

// Message types (RFC 8415 §7.3).
const MSG_SOLICIT: u8 = 1;
const MSG_ADVERTISE: u8 = 2;
const MSG_REQUEST: u8 = 3;
const MSG_REPLY: u8 = 7;
const MSG_INFORMATION_REQUEST: u8 = 11;

// Option codes (RFC 8415 §21, RFC 3646).
const OPT_CLIENTID: u16 = 1;
const OPT_SERVERID: u16 = 2;
const OPT_IA_NA: u16 = 3;
const OPT_IAADDR: u16 = 5;
const OPT_ORO: u16 = 6;
const OPT_ELAPSED_TIME: u16 = 8;
const OPT_STATUS_CODE: u16 = 13;
const OPT_DNS_SERVERS: u16 = 23;

// DUID-LL (RFC 8415 §11.4).
const DUID_TYPE_LL: u16 = 0x0003;
const HW_TYPE_ETHERNET: u16 = 0x0001;

/// DHCPv6 client UDP port (RFC 8415 §7.2) — source port for outgoing messages.
pub const DHCPV6_CLIENT_PORT: u16 = 546;
/// DHCPv6 server UDP port (RFC 8415 §7.2) — destination for client messages.
pub const DHCPV6_SERVER_PORT: u16 = 547;

/// `ff02::1:2` — the All_DHCP_Relay_Agents_and_Servers multicast address
/// (RFC 8415 §7.1). Client messages are sent here.
pub const ALL_DHCP_SERVERS: Ipv6Addr = [0xff, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01, 0, 0x02];

/// Fixed IAID used for the single IA_NA we maintain. Derived from the MAC in
/// [`iaid`]; this constant length documents the field width.
const IAID_LEN: usize = 4;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// DHCPv6 server message type, decoded from the 1-byte message-type field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dhcpv6MsgType {
    /// ADVERTISE (2) — a server advertising an available address.
    Advertise,
    /// REPLY (7) — a server confirming a lease or answering an
    /// Information-Request.
    Reply,
    /// Any other message-type value (not acted upon by this client).
    Other(u8),
}

/// Parsed reply from a DHCPv6 server (an ADVERTISE or REPLY).
///
/// Obtained from [`parse_reply`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dhcpv6Reply {
    /// Decoded message type.
    pub msg_type: Dhcpv6MsgType,
    /// Transaction ID (3 bytes) echoed by the server.
    pub transaction_id: [u8; 3],
    /// Raw SERVERID (DUID) bytes — echoed verbatim in the REQUEST.
    pub server_id: Vec<u8>,
    /// Address from IA_NA → IA Address (option 5), if present (stateful).
    pub ia_addr: Option<Ipv6Addr>,
    /// Preferred lifetime (seconds) from the IA Address option.
    pub preferred_lifetime: u32,
    /// Valid lifetime (seconds) from the IA Address option.
    pub valid_lifetime: u32,
    /// DNS recursive name servers (option 23, RFC 3646).
    pub dns_servers: Vec<Ipv6Addr>,
}

/// Network configuration obtained after a successful DHCPv6 exchange.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dhcpv6Config {
    /// Leased address (stateful) or `None` (stateless DNS-only
    /// Information-Request).
    pub address: Option<Ipv6Addr>,
    /// DNS recursive name servers.
    pub dns_servers: Vec<Ipv6Addr>,
    /// Valid lifetime of the lease in seconds (0 for the stateless path).
    pub valid_lifetime: u32,
}

/// Action returned by [`Dhcpv6Client::on_reply`].
#[derive(Debug)]
pub enum Dhcpv6Action {
    /// Caller must send these REQUEST bytes to `[ff02::1:2]:547`.
    SendRequest(Vec<u8>),
    /// The exchange is complete; apply this configuration.
    Bound(Dhcpv6Config),
    /// The reply is not actionable in the current state.
    Ignore,
}

/// State of the DHCPv6 client state machine (RFC 8415 §18).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dhcpv6State {
    /// Idle — no exchange in progress.
    Init,
    /// SOLICIT sent; waiting for at least one ADVERTISE.
    Soliciting,
    /// REQUEST sent; waiting for a REPLY confirming the lease.
    Requesting,
    /// INFORMATION-REQUEST sent (stateless); waiting for a REPLY.
    InfoRequesting,
    /// Configuration obtained.
    Bound,
}

/// Transport-agnostic DHCPv6 client state machine.
///
/// Holds the current [`Dhcpv6State`] and the transaction ID (`xid`) used to
/// correlate messages. The caller supplies a random `xid` to the `start_*`
/// methods; it is stored and replayed in all subsequent messages of the same
/// exchange.
#[derive(Debug)]
pub struct Dhcpv6Client {
    /// Current state.
    pub state: Dhcpv6State,
    /// Active transaction ID, or `[0; 3]` when in [`Dhcpv6State::Init`].
    pub xid: [u8; 3],
    /// MAC address of the interface, stored from the first `start_*` call.
    mac: MacAddr,
}

impl Dhcpv6Client {
    /// Create a new client in the [`Dhcpv6State::Init`] state.
    pub const fn new() -> Self {
        Self {
            state: Dhcpv6State::Init,
            xid: [0u8; 3],
            mac: [0u8; 6],
        }
    }

    /// Begin the **stateful** exchange (address lease).
    ///
    /// Transitions to [`Dhcpv6State::Soliciting`] and returns the SOLICIT bytes
    /// ready to send to `[ff02::1:2]:547`. `xid` is the caller-supplied 3-byte
    /// transaction ID (use a random value from the kernel CSPRNG for real use).
    pub fn start_solicit(&mut self, mac: MacAddr, xid: [u8; 3]) -> Vec<u8> {
        self.mac = mac;
        self.xid = xid;
        self.state = Dhcpv6State::Soliciting;
        build_solicit(xid, mac)
    }

    /// Begin the **stateless** DNS-only exchange.
    ///
    /// Transitions to [`Dhcpv6State::InfoRequesting`] and returns the
    /// INFORMATION-REQUEST bytes. This is the path the kernel driver drives by
    /// default — it leases no address, only configuration (DNS servers). It is
    /// **not** CI-deterministic under QEMU SLIRP (no DHCPv6 server); see the
    /// module docs.
    pub fn start_information_request(&mut self, mac: MacAddr, xid: [u8; 3]) -> Vec<u8> {
        self.mac = mac;
        self.xid = xid;
        self.state = Dhcpv6State::InfoRequesting;
        build_information_request(xid, mac)
    }

    /// Feed a parsed reply into the state machine.
    ///
    /// - `Soliciting` + ADVERTISE → [`Dhcpv6Action::SendRequest`] (REQUEST
    ///   bytes echoing the server's DUID + offered address), state →
    ///   `Requesting`.
    /// - `Requesting` + REPLY → [`Dhcpv6Action::Bound`] (config with address +
    ///   DNS), state → `Bound`.
    /// - `InfoRequesting` + REPLY → [`Dhcpv6Action::Bound`] (config with
    ///   `address = None`, DNS only), state → `Bound`.
    /// - Anything else → [`Dhcpv6Action::Ignore`] (state unchanged).
    pub fn on_reply(&mut self, reply: Dhcpv6Reply) -> Dhcpv6Action {
        match self.state {
            Dhcpv6State::Soliciting => {
                if reply.msg_type != Dhcpv6MsgType::Advertise {
                    return Dhcpv6Action::Ignore;
                }
                // We need both a server DUID and an offered address to proceed.
                let ia_addr = match reply.ia_addr {
                    Some(a) => a,
                    None => return Dhcpv6Action::Ignore,
                };
                if reply.server_id.is_empty() {
                    return Dhcpv6Action::Ignore;
                }
                self.state = Dhcpv6State::Requesting;
                let req = build_request(self.xid, self.mac, &reply.server_id, ia_addr);
                Dhcpv6Action::SendRequest(req)
            }
            Dhcpv6State::Requesting => {
                if reply.msg_type != Dhcpv6MsgType::Reply {
                    return Dhcpv6Action::Ignore;
                }
                self.state = Dhcpv6State::Bound;
                Dhcpv6Action::Bound(Dhcpv6Config {
                    address: reply.ia_addr,
                    dns_servers: reply.dns_servers,
                    valid_lifetime: reply.valid_lifetime,
                })
            }
            Dhcpv6State::InfoRequesting => {
                if reply.msg_type != Dhcpv6MsgType::Reply {
                    return Dhcpv6Action::Ignore;
                }
                self.state = Dhcpv6State::Bound;
                // Stateless: no address, DNS only.
                Dhcpv6Action::Bound(Dhcpv6Config {
                    address: None,
                    dns_servers: reply.dns_servers,
                    valid_lifetime: 0,
                })
            }
            // In Init or Bound states, ignore unsolicited replies.
            Dhcpv6State::Init | Dhcpv6State::Bound => Dhcpv6Action::Ignore,
        }
    }

    /// Reset the client to [`Dhcpv6State::Init`] (e.g. after a lease expiry or a
    /// failed exchange).
    pub fn reset(&mut self) {
        self.state = Dhcpv6State::Init;
        self.xid = [0u8; 3];
    }
}

impl Default for Dhcpv6Client {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Message builders
// ---------------------------------------------------------------------------

/// Build a SOLICIT message (RFC 8415 §18.2.1, §16.2).
///
/// Options: CLIENTID (DUID-LL), ELAPSED_TIME (0), ORO (requesting DNS_SERVERS),
/// and an IA_NA with no encapsulated IA Address (the server chooses).
pub fn build_solicit(xid: [u8; 3], mac: MacAddr) -> Vec<u8> {
    let mut buf = Vec::with_capacity(64);
    buf.push(MSG_SOLICIT);
    buf.extend_from_slice(&xid);

    // CLIENTID (DUID-LL).
    push_option(&mut buf, OPT_CLIENTID, &duid_ll(mac));
    // ELAPSED_TIME = 0 (hundredths of a second since the exchange started).
    push_option(&mut buf, OPT_ELAPSED_TIME, &[0, 0]);
    // ORO: request DNS_SERVERS (23).
    push_option(&mut buf, OPT_ORO, &OPT_DNS_SERVERS.to_be_bytes());
    // IA_NA: IAID, T1=0, T2=0, no encapsulated options (server chooses).
    let mut ia_na = Vec::with_capacity(12);
    ia_na.extend_from_slice(&iaid(mac));
    ia_na.extend_from_slice(&0u32.to_be_bytes()); // T1
    ia_na.extend_from_slice(&0u32.to_be_bytes()); // T2
    push_option(&mut buf, OPT_IA_NA, &ia_na);

    buf
}

/// Build a REQUEST message (RFC 8415 §18.2.2, §16.3).
///
/// Sent after an ADVERTISE to confirm the offered address from a specific
/// server. Echoes the server's DUID (`server_id`) and the offered address.
///
/// Options: CLIENTID, SERVERID, ELAPSED_TIME (0), ORO (DNS_SERVERS), and an
/// IA_NA encapsulating an IA Address for `ia_addr` (preferred/valid lifetimes
/// 0 — the server fills the real values in the REPLY).
pub fn build_request(xid: [u8; 3], mac: MacAddr, server_id: &[u8], ia_addr: Ipv6Addr) -> Vec<u8> {
    let mut buf = Vec::with_capacity(96);
    buf.push(MSG_REQUEST);
    buf.extend_from_slice(&xid);

    push_option(&mut buf, OPT_CLIENTID, &duid_ll(mac));
    push_option(&mut buf, OPT_SERVERID, server_id);
    push_option(&mut buf, OPT_ELAPSED_TIME, &[0, 0]);
    push_option(&mut buf, OPT_ORO, &OPT_DNS_SERVERS.to_be_bytes());

    // IA_NA: IAID, T1=0, T2=0, encapsulating one IA Address.
    let mut iaaddr = Vec::with_capacity(24);
    iaaddr.extend_from_slice(&ia_addr);
    iaaddr.extend_from_slice(&0u32.to_be_bytes()); // preferred lifetime
    iaaddr.extend_from_slice(&0u32.to_be_bytes()); // valid lifetime

    let mut ia_na = Vec::with_capacity(40);
    ia_na.extend_from_slice(&iaid(mac));
    ia_na.extend_from_slice(&0u32.to_be_bytes()); // T1
    ia_na.extend_from_slice(&0u32.to_be_bytes()); // T2
    push_option(&mut ia_na, OPT_IAADDR, &iaaddr);
    push_option(&mut buf, OPT_IA_NA, &ia_na);

    buf
}

/// Build an INFORMATION-REQUEST message (RFC 8415 §18.2.6, §16.6).
///
/// The stateless DNS-only path: no IA_NA, just CLIENTID, ELAPSED_TIME (0) and
/// an ORO requesting DNS_SERVERS.
pub fn build_information_request(xid: [u8; 3], mac: MacAddr) -> Vec<u8> {
    let mut buf = Vec::with_capacity(32);
    buf.push(MSG_INFORMATION_REQUEST);
    buf.extend_from_slice(&xid);

    push_option(&mut buf, OPT_CLIENTID, &duid_ll(mac));
    push_option(&mut buf, OPT_ELAPSED_TIME, &[0, 0]);
    push_option(&mut buf, OPT_ORO, &OPT_DNS_SERVERS.to_be_bytes());

    buf
}

// ---------------------------------------------------------------------------
// Message parser
// ---------------------------------------------------------------------------

/// Parse a DHCPv6 server message (UDP body, no IP/UDP headers).
///
/// Returns `None` for any truncated, malformed, or non-ADVERTISE/REPLY message.
/// Never panics regardless of input length or content.
///
/// Walks the top-level options TLV list (RFC 8415 §21.1): each option is
/// `code(2) len(2) data(len)`, all big-endian, bounds-checked. Descends IA_NA
/// (option 3) into its encapsulated IA Address (option 5) for the leased
/// address + lifetimes, and collects DNS_SERVERS (option 23). Unknown options
/// are skipped by their declared length.
pub fn parse_reply(buf: &[u8]) -> Option<Dhcpv6Reply> {
    // Need at least the message type + 3-byte transaction ID.
    if buf.len() < 4 {
        return None;
    }

    let msg_type = match buf[0] {
        MSG_ADVERTISE => Dhcpv6MsgType::Advertise,
        MSG_REPLY => Dhcpv6MsgType::Reply,
        // Only ADVERTISE and REPLY are server replies this client acts upon.
        _ => return None,
    };

    let mut transaction_id = [0u8; 3];
    transaction_id.copy_from_slice(&buf[1..4]);

    let mut server_id: Vec<u8> = Vec::new();
    let mut ia_addr: Option<Ipv6Addr> = None;
    let mut preferred_lifetime: u32 = 0;
    let mut valid_lifetime: u32 = 0;
    let mut dns_servers: Vec<Ipv6Addr> = Vec::new();

    // Walk the top-level options starting after the 4-byte header.
    let opts = &buf[4..];
    let mut i = 0usize;
    while i < opts.len() {
        // Each option needs a 4-byte code+len header.
        if i + 4 > opts.len() {
            return None; // truncated header
        }
        let code = u16::from_be_bytes([opts[i], opts[i + 1]]);
        let len = u16::from_be_bytes([opts[i + 2], opts[i + 3]]) as usize;
        i += 4;
        if i + len > opts.len() {
            return None; // option data runs past the end
        }
        let data = &opts[i..i + len];
        i += len;

        match code {
            OPT_SERVERID => {
                server_id = data.to_vec();
            }
            OPT_IA_NA => {
                // IA_NA data: IAID(4) T1(4) T2(4) then encapsulated options.
                if data.len() < 12 {
                    return None; // truncated IA_NA header
                }
                let mut j = 12usize;
                while j < data.len() {
                    if j + 4 > data.len() {
                        return None; // truncated encapsulated header
                    }
                    let sub_code = u16::from_be_bytes([data[j], data[j + 1]]);
                    let sub_len = u16::from_be_bytes([data[j + 2], data[j + 3]]) as usize;
                    j += 4;
                    if j + sub_len > data.len() {
                        return None; // encapsulated data runs past the end
                    }
                    let sub = &data[j..j + sub_len];
                    j += sub_len;

                    match sub_code {
                        OPT_IAADDR => {
                            // IA Address: address(16) preferred(4) valid(4) [+subopts].
                            if sub.len() < 24 {
                                return None; // truncated IA Address
                            }
                            let mut addr = [0u8; 16];
                            addr.copy_from_slice(&sub[0..16]);
                            ia_addr = Some(addr);
                            preferred_lifetime =
                                u32::from_be_bytes([sub[16], sub[17], sub[18], sub[19]]);
                            valid_lifetime =
                                u32::from_be_bytes([sub[20], sub[21], sub[22], sub[23]]);
                        }
                        // A non-success STATUS_CODE here means the IA was not
                        // granted; we leave `ia_addr` unset so `on_reply` ignores
                        // the offer (success=0 carries no address anyway).
                        OPT_STATUS_CODE => {}
                        _ => {}
                    }
                }
            }
            OPT_DNS_SERVERS => {
                // One or more 16-byte IPv6 addresses (RFC 3646).
                let mut k = 0usize;
                while k + 16 <= data.len() {
                    let mut a = [0u8; 16];
                    a.copy_from_slice(&data[k..k + 16]);
                    dns_servers.push(a);
                    k += 16;
                }
            }
            // CLIENTID, ELAPSED_TIME, STATUS_CODE, ORO, and unknown options are
            // skipped (already advanced past by `len`).
            _ => {}
        }
    }

    Some(Dhcpv6Reply {
        msg_type,
        transaction_id,
        server_id,
        ia_addr,
        preferred_lifetime,
        valid_lifetime,
        dns_servers,
    })
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Append a DHCPv6 TLV option: `code(2 BE) len(2 BE) data`.
#[inline]
fn push_option(buf: &mut Vec<u8>, code: u16, data: &[u8]) {
    buf.extend_from_slice(&code.to_be_bytes());
    buf.extend_from_slice(&(data.len() as u16).to_be_bytes());
    buf.extend_from_slice(data);
}

/// Build a DUID-LL from a MAC (RFC 8415 §11.4): 10 bytes total —
/// `duid-type(2)=0x0003, hardware-type(2)=0x0001, link-layer-address(6=MAC)`.
fn duid_ll(mac: MacAddr) -> [u8; 10] {
    let mut duid = [0u8; 10];
    duid[0..2].copy_from_slice(&DUID_TYPE_LL.to_be_bytes());
    duid[2..4].copy_from_slice(&HW_TYPE_ETHERNET.to_be_bytes());
    duid[4..10].copy_from_slice(&mac);
    duid
}

/// Derive a stable 4-byte IAID for our single IA_NA from the low 4 bytes of the
/// MAC. Any deterministic per-interface value satisfies RFC 8415 §6.6.
fn iaid(mac: MacAddr) -> [u8; IAID_LEN] {
    [mac[2], mac[3], mac[4], mac[5]]
}

// ---------------------------------------------------------------------------
// Host-only tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const MAC: MacAddr = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];

    // -----------------------------------------------------------------------
    // Synthetic server-reply builders.
    // -----------------------------------------------------------------------

    /// Build a server message: type + xid + the given pre-encoded options bytes.
    fn make_msg(msg_type: u8, xid: [u8; 3], opts: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.push(msg_type);
        v.extend_from_slice(&xid);
        v.extend_from_slice(opts);
        v
    }

    /// Encode one top-level option (code, len, data) into a buffer.
    fn opt(code: u16, data: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&code.to_be_bytes());
        v.extend_from_slice(&(data.len() as u16).to_be_bytes());
        v.extend_from_slice(data);
        v
    }

    /// Encode an IA_NA option wrapping an IA Address with the given lifetimes.
    fn ia_na_with_addr(addr: Ipv6Addr, preferred: u32, valid: u32) -> Vec<u8> {
        let mut iaaddr = Vec::new();
        iaaddr.extend_from_slice(&addr);
        iaaddr.extend_from_slice(&preferred.to_be_bytes());
        iaaddr.extend_from_slice(&valid.to_be_bytes());

        let mut ia_na = Vec::new();
        ia_na.extend_from_slice(&[0xAB, 0xCD, 0xEF, 0x01]); // IAID
        ia_na.extend_from_slice(&0u32.to_be_bytes()); // T1
        ia_na.extend_from_slice(&0u32.to_be_bytes()); // T2
        ia_na.extend_from_slice(&opt(OPT_IAADDR, &iaaddr));
        opt(OPT_IA_NA, &ia_na)
    }

    /// Encode a DNS_SERVERS option from a list of addresses.
    fn dns_opt(servers: &[Ipv6Addr]) -> Vec<u8> {
        let mut data = Vec::new();
        for s in servers {
            data.extend_from_slice(s);
        }
        opt(OPT_DNS_SERVERS, &data)
    }

    // -----------------------------------------------------------------------
    // Test 1: SOLICIT byte layout.
    // -----------------------------------------------------------------------

    #[test]
    fn solicit_layout() {
        let xid = [0x12, 0x34, 0x56];
        let pkt = build_solicit(xid, MAC);

        // msg-type == SOLICIT(1), xid echoed at [1..4].
        assert_eq!(pkt[0], MSG_SOLICIT);
        assert_eq!(&pkt[1..4], &xid);

        // Walk options; assert CLIENTID with DUID-LL, an IA_NA, an ELAPSED_TIME.
        let mut have_clientid = false;
        let mut have_ia_na = false;
        let mut have_elapsed = false;
        let opts = &pkt[4..];
        let mut i = 0usize;
        while i + 4 <= opts.len() {
            let code = u16::from_be_bytes([opts[i], opts[i + 1]]);
            let len = u16::from_be_bytes([opts[i + 2], opts[i + 3]]) as usize;
            i += 4;
            let data = &opts[i..i + len];
            i += len;
            match code {
                OPT_CLIENTID => {
                    have_clientid = true;
                    // DUID-LL: 0x0003, 0x0001, MAC.
                    assert_eq!(data.len(), 10);
                    assert_eq!(&data[0..2], &DUID_TYPE_LL.to_be_bytes());
                    assert_eq!(&data[2..4], &HW_TYPE_ETHERNET.to_be_bytes());
                    assert_eq!(&data[4..10], &MAC);
                }
                OPT_IA_NA => have_ia_na = true,
                OPT_ELAPSED_TIME => have_elapsed = true,
                _ => {}
            }
        }
        assert_eq!(i, opts.len(), "options must consume the whole buffer");
        assert!(have_clientid, "SOLICIT must carry CLIENTID");
        assert!(have_ia_na, "SOLICIT must carry IA_NA");
        assert!(have_elapsed, "SOLICIT must carry ELAPSED_TIME");
    }

    // -----------------------------------------------------------------------
    // Test 2: INFORMATION-REQUEST layout.
    // -----------------------------------------------------------------------

    #[test]
    fn information_request_layout() {
        let xid = [0xAA, 0xBB, 0xCC];
        let pkt = build_information_request(xid, MAC);

        assert_eq!(pkt[0], MSG_INFORMATION_REQUEST);
        assert_eq!(&pkt[1..4], &xid);

        let mut have_clientid = false;
        let mut have_oro = false;
        let mut have_ia_na = false;
        let opts = &pkt[4..];
        let mut i = 0usize;
        while i + 4 <= opts.len() {
            let code = u16::from_be_bytes([opts[i], opts[i + 1]]);
            let len = u16::from_be_bytes([opts[i + 2], opts[i + 3]]) as usize;
            i += 4 + len;
            match code {
                OPT_CLIENTID => have_clientid = true,
                OPT_ORO => have_oro = true,
                OPT_IA_NA => have_ia_na = true,
                _ => {}
            }
        }
        assert!(have_clientid, "INFORMATION-REQUEST must carry CLIENTID");
        assert!(have_oro, "INFORMATION-REQUEST must carry ORO");
        assert!(!have_ia_na, "INFORMATION-REQUEST must NOT carry IA_NA");
    }

    // -----------------------------------------------------------------------
    // Test 3: parse a synthetic ADVERTISE.
    // -----------------------------------------------------------------------

    #[test]
    fn parse_advertise_round_trip() {
        let xid = [0x01, 0x02, 0x03];
        let server_duid = [0x00, 0x03, 0x00, 0x01, 0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01];
        let addr: Ipv6Addr = [
            0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x42,
        ];
        let dns1: Ipv6Addr = [
            0x20, 0x01, 0x48, 0x60, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x88, 0x88,
        ];

        let mut opts = Vec::new();
        opts.extend_from_slice(&opt(OPT_SERVERID, &server_duid));
        opts.extend_from_slice(&ia_na_with_addr(addr, 3600, 7200));
        opts.extend_from_slice(&dns_opt(&[dns1]));
        let raw = make_msg(MSG_ADVERTISE, xid, &opts);

        let reply = parse_reply(&raw).expect("should parse ADVERTISE");
        assert_eq!(reply.msg_type, Dhcpv6MsgType::Advertise);
        assert_eq!(reply.transaction_id, xid);
        assert_eq!(reply.server_id, server_duid.to_vec());
        assert_eq!(reply.ia_addr, Some(addr));
        assert_eq!(reply.preferred_lifetime, 3600);
        assert_eq!(reply.valid_lifetime, 7200);
        assert_eq!(reply.dns_servers, vec![dns1]);
    }

    // -----------------------------------------------------------------------
    // Test 4: parse a synthetic REPLY.
    // -----------------------------------------------------------------------

    #[test]
    fn parse_reply_round_trip() {
        let xid = [0x09, 0x08, 0x07];
        let server_duid = [0x00, 0x03, 0x00, 0x01, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66];
        let addr: Ipv6Addr = [0xfd, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x99];
        let dns1: Ipv6Addr = [
            0x20, 0x01, 0x48, 0x60, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x88, 0x44,
        ];
        let dns2: Ipv6Addr = [0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01];

        let mut opts = Vec::new();
        opts.extend_from_slice(&opt(OPT_SERVERID, &server_duid));
        opts.extend_from_slice(&ia_na_with_addr(addr, 1800, 3600));
        opts.extend_from_slice(&dns_opt(&[dns1, dns2]));
        let raw = make_msg(MSG_REPLY, xid, &opts);

        let reply = parse_reply(&raw).expect("should parse REPLY");
        assert_eq!(reply.msg_type, Dhcpv6MsgType::Reply);
        assert_eq!(reply.ia_addr, Some(addr));
        assert_eq!(reply.preferred_lifetime, 1800);
        assert_eq!(reply.valid_lifetime, 3600);
        assert_eq!(reply.dns_servers, vec![dns1, dns2]);
    }

    // -----------------------------------------------------------------------
    // Test 5: full stateful walk: SOLICIT → ADVERTISE → REQUEST → REPLY → Bound.
    // -----------------------------------------------------------------------

    #[test]
    fn stateful_full_walk() {
        let xid = [0xCA, 0xFE, 0x01];
        let server_duid = [0x00, 0x03, 0x00, 0x01, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
        let addr: Ipv6Addr = [
            0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x64,
        ];
        let dns1: Ipv6Addr = [
            0x20, 0x01, 0x48, 0x60, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x88, 0x88,
        ];

        let mut client = Dhcpv6Client::new();
        assert_eq!(client.state, Dhcpv6State::Init);

        // --- Step 1: start_solicit ---
        let solicit = client.start_solicit(MAC, xid);
        assert_eq!(client.state, Dhcpv6State::Soliciting);
        assert_eq!(client.xid, xid);
        assert_eq!(solicit[0], MSG_SOLICIT);

        // --- Step 2: receive ADVERTISE → SendRequest ---
        let mut adv_opts = Vec::new();
        adv_opts.extend_from_slice(&opt(OPT_SERVERID, &server_duid));
        adv_opts.extend_from_slice(&ia_na_with_addr(addr, 3600, 7200));
        adv_opts.extend_from_slice(&dns_opt(&[dns1]));
        let adv = parse_reply(&make_msg(MSG_ADVERTISE, xid, &adv_opts)).unwrap();

        let action = client.on_reply(adv);
        assert_eq!(client.state, Dhcpv6State::Requesting);
        let request = match action {
            Dhcpv6Action::SendRequest(b) => b,
            other => panic!("expected SendRequest, got {:?}", other),
        };

        // The REQUEST must be valid: type REQUEST, echo SERVERID, carry IA_NA
        // with an IAADDR for the offered address. Parse it back to assert.
        assert_eq!(request[0], MSG_REQUEST);
        assert_eq!(&request[1..4], &xid);
        // Re-parse the REQUEST (it's ADVERTISE/REPLY-shaped enough for the
        // walker only if the type matches — so walk it manually here).
        let mut saw_serverid = false;
        let mut req_ia_addr: Option<Ipv6Addr> = None;
        {
            let opts = &request[4..];
            let mut i = 0usize;
            while i + 4 <= opts.len() {
                let code = u16::from_be_bytes([opts[i], opts[i + 1]]);
                let len = u16::from_be_bytes([opts[i + 2], opts[i + 3]]) as usize;
                i += 4;
                let data = &opts[i..i + len];
                i += len;
                match code {
                    OPT_SERVERID => {
                        saw_serverid = true;
                        assert_eq!(data, &server_duid);
                    }
                    OPT_IA_NA => {
                        // Descend to the IAADDR sub-option.
                        let mut j = 12usize;
                        while j + 4 <= data.len() {
                            let sc = u16::from_be_bytes([data[j], data[j + 1]]);
                            let sl = u16::from_be_bytes([data[j + 2], data[j + 3]]) as usize;
                            j += 4;
                            let sd = &data[j..j + sl];
                            j += sl;
                            if sc == OPT_IAADDR {
                                let mut a = [0u8; 16];
                                a.copy_from_slice(&sd[0..16]);
                                req_ia_addr = Some(a);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        assert!(saw_serverid, "REQUEST must echo the SERVERID");
        assert_eq!(
            req_ia_addr,
            Some(addr),
            "REQUEST must carry the offered addr"
        );

        // --- Step 3: receive REPLY → Bound ---
        let mut rep_opts = Vec::new();
        rep_opts.extend_from_slice(&opt(OPT_SERVERID, &server_duid));
        rep_opts.extend_from_slice(&ia_na_with_addr(addr, 3600, 7200));
        rep_opts.extend_from_slice(&dns_opt(&[dns1]));
        let rep = parse_reply(&make_msg(MSG_REPLY, xid, &rep_opts)).unwrap();

        let action2 = client.on_reply(rep);
        assert_eq!(client.state, Dhcpv6State::Bound);
        let cfg = match action2 {
            Dhcpv6Action::Bound(c) => c,
            other => panic!("expected Bound, got {:?}", other),
        };
        assert_eq!(cfg.address, Some(addr));
        assert_eq!(cfg.dns_servers, vec![dns1]);
        assert_eq!(cfg.valid_lifetime, 7200);
    }

    // -----------------------------------------------------------------------
    // Test 6: stateless walk: INFORMATION-REQUEST → REPLY (DNS only) → Bound.
    // -----------------------------------------------------------------------

    #[test]
    fn stateless_walk() {
        let xid = [0x00, 0x00, 0x01];
        let dns1: Ipv6Addr = [
            0x20, 0x01, 0x48, 0x60, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x88, 0x88,
        ];
        let dns2: Ipv6Addr = [
            0x20, 0x01, 0x48, 0x60, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x88, 0x44,
        ];

        let mut client = Dhcpv6Client::new();
        let info = client.start_information_request(MAC, xid);
        assert_eq!(client.state, Dhcpv6State::InfoRequesting);
        assert_eq!(info[0], MSG_INFORMATION_REQUEST);

        // REPLY with only DNS_SERVERS, no IA_NA.
        let rep = parse_reply(&make_msg(MSG_REPLY, xid, &dns_opt(&[dns1, dns2]))).unwrap();
        let action = client.on_reply(rep);
        assert_eq!(client.state, Dhcpv6State::Bound);
        let cfg = match action {
            Dhcpv6Action::Bound(c) => c,
            other => panic!("expected Bound, got {:?}", other),
        };
        assert_eq!(cfg.address, None);
        assert_eq!(cfg.dns_servers, vec![dns1, dns2]);
        assert_eq!(cfg.valid_lifetime, 0);
    }

    // -----------------------------------------------------------------------
    // Test 7: truncation / malformed safety — never panic, always None.
    // -----------------------------------------------------------------------

    #[test]
    fn parse_empty_returns_none() {
        assert!(parse_reply(&[]).is_none());
    }

    #[test]
    fn parse_header_only_too_short_returns_none() {
        // 3 bytes: shorter than type + 3-byte xid.
        assert!(parse_reply(&[MSG_REPLY, 0x01, 0x02]).is_none());
    }

    #[test]
    fn parse_option_length_past_end_returns_none() {
        // A SERVERID option claiming len=100 but with no data.
        let mut raw = vec![MSG_REPLY, 0x01, 0x02, 0x03];
        raw.extend_from_slice(&OPT_SERVERID.to_be_bytes());
        raw.extend_from_slice(&100u16.to_be_bytes());
        // No data follows.
        assert!(parse_reply(&raw).is_none());
    }

    #[test]
    fn parse_truncated_option_header_returns_none() {
        // Two trailing bytes — not enough for a 4-byte option header.
        let raw = vec![MSG_REPLY, 0x01, 0x02, 0x03, 0x00, 0x02];
        assert!(parse_reply(&raw).is_none());
    }

    #[test]
    fn parse_non_advertise_or_reply_returns_none() {
        // A SOLICIT (1) is a client message, not a server reply.
        let raw = make_msg(MSG_SOLICIT, [0, 0, 0], &[]);
        assert!(parse_reply(&raw).is_none());
        // REQUEST (3) likewise.
        let raw = make_msg(MSG_REQUEST, [0, 0, 0], &[]);
        assert!(parse_reply(&raw).is_none());
    }

    #[test]
    fn parse_ia_na_encapsulated_length_past_end_returns_none() {
        // IA_NA whose encapsulated IAADDR claims more bytes than present.
        let mut ia_na = Vec::new();
        ia_na.extend_from_slice(&[0, 0, 0, 0]); // IAID
        ia_na.extend_from_slice(&0u32.to_be_bytes()); // T1
        ia_na.extend_from_slice(&0u32.to_be_bytes()); // T2
        ia_na.extend_from_slice(&OPT_IAADDR.to_be_bytes());
        ia_na.extend_from_slice(&200u16.to_be_bytes()); // claims 200 bytes
        // No sub-data.
        let raw = make_msg(MSG_REPLY, [0, 0, 0], &opt(OPT_IA_NA, &ia_na));
        assert!(parse_reply(&raw).is_none());
    }

    // -----------------------------------------------------------------------
    // Test 8: on_reply Ignore in wrong states.
    // -----------------------------------------------------------------------

    #[test]
    fn ignore_reply_while_soliciting() {
        let xid = [0x01, 0x02, 0x03];
        let mut client = Dhcpv6Client::new();
        client.start_solicit(MAC, xid);
        // A REPLY (not ADVERTISE) while Soliciting must be ignored.
        let rep = parse_reply(&make_msg(MSG_REPLY, xid, &[])).unwrap();
        assert!(matches!(client.on_reply(rep), Dhcpv6Action::Ignore));
        assert_eq!(client.state, Dhcpv6State::Soliciting);
    }

    #[test]
    fn ignore_advertise_while_bound() {
        let xid = [0xCA, 0xFE, 0x01];
        let server_duid = [0x00, 0x03, 0x00, 0x01, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
        let addr: Ipv6Addr = [
            0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x64,
        ];

        let mut client = Dhcpv6Client::new();
        client.start_solicit(MAC, xid);

        // Drive to Bound.
        let mut adv_opts = Vec::new();
        adv_opts.extend_from_slice(&opt(OPT_SERVERID, &server_duid));
        adv_opts.extend_from_slice(&ia_na_with_addr(addr, 3600, 7200));
        let adv = parse_reply(&make_msg(MSG_ADVERTISE, xid, &adv_opts)).unwrap();
        client.on_reply(adv);
        let rep = parse_reply(&make_msg(MSG_REPLY, xid, &adv_opts)).unwrap();
        client.on_reply(rep);
        assert_eq!(client.state, Dhcpv6State::Bound);

        // A fresh ADVERTISE while Bound must be ignored, state unchanged.
        let adv2 = parse_reply(&make_msg(MSG_ADVERTISE, xid, &adv_opts)).unwrap();
        assert!(matches!(client.on_reply(adv2), Dhcpv6Action::Ignore));
        assert_eq!(client.state, Dhcpv6State::Bound);
    }

    #[test]
    fn ignore_reply_in_init_state() {
        let mut client = Dhcpv6Client::new();
        assert_eq!(client.state, Dhcpv6State::Init);
        let rep = parse_reply(&make_msg(MSG_REPLY, [0, 0, 0], &[])).unwrap();
        assert!(matches!(client.on_reply(rep), Dhcpv6Action::Ignore));
        assert_eq!(client.state, Dhcpv6State::Init);
    }

    // -----------------------------------------------------------------------
    // Misc correctness.
    // -----------------------------------------------------------------------

    #[test]
    fn reset_returns_to_init() {
        let mut client = Dhcpv6Client::new();
        client.start_solicit(MAC, [0xFF, 0xEE, 0xDD]);
        assert_eq!(client.state, Dhcpv6State::Soliciting);
        client.reset();
        assert_eq!(client.state, Dhcpv6State::Init);
        assert_eq!(client.xid, [0, 0, 0]);
    }

    #[test]
    fn all_dhcp_servers_constant_is_ff02_1_2() {
        assert_eq!(
            ALL_DHCP_SERVERS,
            [0xff, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01, 0, 0x02]
        );
    }

    #[test]
    fn duid_ll_layout() {
        let d = duid_ll(MAC);
        assert_eq!(d.len(), 10);
        assert_eq!(&d[0..2], &[0x00, 0x03]);
        assert_eq!(&d[2..4], &[0x00, 0x01]);
        assert_eq!(&d[4..10], &MAC);
    }

    #[test]
    fn iaid_derived_from_mac() {
        assert_eq!(iaid(MAC), [0x00, 0x12, 0x34, 0x56]);
    }

    #[test]
    fn advertise_without_address_is_ignored() {
        // An ADVERTISE with a SERVERID but no IA_NA/address cannot be REQUESTed.
        let xid = [0x01, 0x02, 0x03];
        let server_duid = [0x00, 0x03, 0x00, 0x01, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
        let mut client = Dhcpv6Client::new();
        client.start_solicit(MAC, xid);
        let adv = parse_reply(&make_msg(
            MSG_ADVERTISE,
            xid,
            &opt(OPT_SERVERID, &server_duid),
        ))
        .unwrap();
        assert!(matches!(client.on_reply(adv), Dhcpv6Action::Ignore));
        assert_eq!(client.state, Dhcpv6State::Soliciting);
    }
}
