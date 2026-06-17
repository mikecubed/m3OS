//! DHCP client protocol logic — RFC 2131 / BOOTP.
//!
//! This module is **transport-agnostic**: it builds raw BOOTP/DHCP message
//! bytes and parses server replies, but never performs I/O. The kernel glue
//! layer (not this module) is responsible for sending the resulting bytes over
//! a UDP socket bound to port 68 and directed to the broadcast address
//! 255.255.255.255:67.
//!
//! # Protocol summary (RFC 2131 §3)
//!
//! ```text
//! Client                       Server
//!   |--- DHCPDISCOVER (bcast) --->|
//!   |<-- DHCPOFFER (unicast/bc) --|
//!   |--- DHCPREQUEST (bcast) ---->|
//!   |<-- DHCPACK / DHCPNAK -------|
//! ```
//!
//! 1. The client broadcasts a DHCPDISCOVER with a random transaction ID (`xid`).
//! 2. One or more servers reply with DHCPOFFER, each advertising a lease.
//! 3. The client picks one offer and broadcasts a DHCPREQUEST echoing the
//!    chosen server's IP (option 54) and the offered address (option 50).
//! 4. The server confirms with DHCPACK (or rejects with DHCPNAK).
//!
//! # Usage
//!
//! ```rust,ignore
//! let mut client = DhcpClient::new();
//! let mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
//! let discover_bytes = client.start(mac);
//! // … send discover_bytes as a UDP broadcast on port 67 …
//!
//! // On receipt of a server packet:
//! if let Some(reply) = parse_reply(&server_udp_payload) {
//!     match client.on_reply(reply) {
//!         DhcpAction::SendRequest(req_bytes) => { /* broadcast req_bytes */ }
//!         DhcpAction::Bound(cfg) => { /* apply cfg.ip, cfg.mask, cfg.gateway … */ }
//!         DhcpAction::Nak => { /* server rejected; restart */ }
//!         DhcpAction::Ignore => {}
//!     }
//! }
//! ```

use alloc::vec::Vec;

use crate::types::{Ipv4Addr, MacAddr};

// ---------------------------------------------------------------------------
// BOOTP / DHCP wire constants (RFC 2131, RFC 2132)
// ---------------------------------------------------------------------------

/// BOOTP op code: client-to-server request.
const BOOTREQUEST: u8 = 1;
/// BOOTP hardware address type: Ethernet (IEEE 802).
const HTYPE_ETHERNET: u8 = 1;
/// Ethernet hardware address length (6 bytes).
const HLEN_ETHERNET: u8 = 6;
/// Default BOOTP hops field for a directly-connected client.
const HOPS_ZERO: u8 = 0;
/// BOOTP "seconds elapsed" field — set to 0 for simplicity.
const SECS_ZERO: u16 = 0;
/// RFC 2131 §2: broadcast flag bit in the `flags` field.
const FLAGS_BROADCAST: u16 = 0x8000;

/// DHCP magic cookie (RFC 2131 §3): bytes 236–239 of the BOOTP packet.
///
/// Value: 99.130.83.99 (0x63825363) in network byte order.
pub const MAGIC_COOKIE: [u8; 4] = [0x63, 0x82, 0x53, 0x63];

/// Byte offset of the magic cookie within a raw DHCP message.
///
/// BOOTP fixed header is 236 bytes (op+htype+hlen+hops=4, xid=4, secs=2,
/// flags=2, ciaddr=4, yiaddr=4, siaddr=4, giaddr=4, chaddr=16, sname=64,
/// file=128 → total 236).
pub const MAGIC_COOKIE_OFFSET: usize = 236;

// DHCP option codes (RFC 2132).
const OPT_SUBNET_MASK: u8 = 1;
const OPT_ROUTER: u8 = 3;
const OPT_DNS: u8 = 6;
const OPT_LEASE_TIME: u8 = 51;
const OPT_MSG_TYPE: u8 = 53;
const OPT_SERVER_ID: u8 = 54;
const OPT_PARAM_REQUEST: u8 = 55;
const OPT_REQUESTED_IP: u8 = 50;
const OPT_END: u8 = 255;

// DHCP message type values (RFC 2132 §9.6).
const DHCP_DISCOVER: u8 = 1;
const DHCP_OFFER: u8 = 2;
const DHCP_REQUEST: u8 = 3;
const DHCP_ACK: u8 = 5;
const DHCP_NAK: u8 = 6;

// Well-known UDP ports for DHCP (RFC 2131 §4.1).
/// Server port — destination for client broadcasts.
pub const DHCP_SERVER_PORT: u16 = 67;
/// Client port — source port for outgoing messages.
pub const DHCP_CLIENT_PORT: u16 = 68;

/// Minimum DHCP packet length: 236 (BOOTP fixed) + 4 (cookie) + 1 (END).
const MIN_DHCP_LEN: usize = MAGIC_COOKIE_OFFSET + 4 + 1;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// DHCP message type decoded from option 53.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DhcpMsgType {
    /// DHCPOFFER (2) — server advertising a lease.
    Offer,
    /// DHCPACK (5) — server confirming the lease.
    Ack,
    /// DHCPNAK (6) — server rejecting the request.
    Nak,
    /// Any other message type value (not acted upon by this client).
    Other(u8),
}

/// Parsed reply from a DHCP server.
///
/// Obtained from [`parse_reply`].
#[derive(Debug, Clone)]
pub struct DhcpReply {
    /// Message type decoded from option 53.
    pub msg_type: DhcpMsgType,
    /// `yiaddr` — the IP address the server is offering/assigning.
    pub yiaddr: Ipv4Addr,
    /// Option 54 — server identifier (the server's IP address).
    pub server_id: Ipv4Addr,
    /// Option 1 — subnet mask.
    pub subnet_mask: Option<Ipv4Addr>,
    /// Option 3 — first router (default gateway).
    pub router: Option<Ipv4Addr>,
    /// Option 6 — first DNS server.
    pub dns: Option<Ipv4Addr>,
    /// Option 51 — lease duration in seconds.
    pub lease_secs: Option<u32>,
}

/// Network configuration obtained after a successful DHCP exchange.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DhcpConfig {
    /// Assigned IP address (`yiaddr`).
    pub ip: Ipv4Addr,
    /// Subnet mask (option 1), or `255.255.255.0` if not provided.
    pub mask: Ipv4Addr,
    /// Default gateway (option 3), or `0.0.0.0` if not provided.
    pub gateway: Ipv4Addr,
    /// DNS server (option 6), or `0.0.0.0` if not provided.
    pub dns: Ipv4Addr,
    /// Lease duration in seconds (option 51), or `None` if not provided.
    pub lease_secs: Option<u32>,
}

/// Action returned by [`DhcpClient::on_reply`].
#[derive(Debug)]
pub enum DhcpAction {
    /// Client has moved to Requesting; caller must broadcast these bytes on
    /// UDP port 67 (destination 255.255.255.255).
    SendRequest(Vec<u8>),
    /// Client is now Bound; `DhcpConfig` contains the assigned parameters.
    Bound(DhcpConfig),
    /// Server sent DHCPNAK — the request was rejected.
    ///
    /// The client should reset to [`DhcpState::Init`] and restart.
    Nak,
    /// Reply was received but is not actionable in the current state
    /// (e.g. an OFFER while already Bound, or an unknown message type).
    Ignore,
}

/// State of the DHCP client state machine (RFC 2131 §4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DhcpState {
    /// Idle — no exchange in progress.
    Init,
    /// DHCPDISCOVER sent; waiting for at least one DHCPOFFER.
    Selecting,
    /// DHCPREQUEST sent; waiting for DHCPACK or DHCPNAK.
    Requesting,
    /// Lease obtained and active.
    Bound,
}

/// Transport-agnostic DHCP client state machine.
///
/// Holds the current [`DhcpState`] and the transaction ID (`xid`) used to
/// correlate messages.  The caller is responsible for generating a random
/// `xid` — the one supplied to [`DhcpClient::start`] is stored and replayed
/// in all subsequent messages.
#[derive(Debug)]
pub struct DhcpClient {
    /// Current state.
    pub state: DhcpState,
    /// Active transaction ID, or 0 when in [`DhcpState::Init`].
    pub xid: u32,
    /// MAC address of the interface, stored from the first [`DhcpClient::start`] call.
    mac: MacAddr,
}

impl DhcpClient {
    /// Create a new client in the [`DhcpState::Init`] state.
    pub const fn new() -> Self {
        Self {
            state: DhcpState::Init,
            xid: 0,
            mac: [0u8; 6],
        }
    }

    /// Begin a new DHCP exchange.
    ///
    /// Transitions to [`DhcpState::Selecting`] and returns the DHCPDISCOVER
    /// bytes ready to send.  `xid` is the caller-supplied transaction ID (use
    /// a random value from the kernel CSPRNG for real use).
    ///
    /// # RFC 2131 §4.4.1
    ///
    /// The client generates a random xid, broadcasts a DHCPDISCOVER, and
    /// enters the SELECTING state.
    pub fn start(&mut self, mac: MacAddr, xid: u32) -> Vec<u8> {
        self.mac = mac;
        self.xid = xid;
        self.state = DhcpState::Selecting;
        build_discover(xid, mac)
    }

    /// Feed a parsed reply into the state machine.
    ///
    /// Returns the action the caller should take (send bytes, apply config,
    /// etc.).  Replies that do not match the current state or transaction are
    /// silently ignored via [`DhcpAction::Ignore`].
    ///
    /// # RFC 2131 §4.4.1 – §4.4.3
    pub fn on_reply(&mut self, reply: DhcpReply) -> DhcpAction {
        match self.state {
            DhcpState::Selecting => {
                if reply.msg_type != DhcpMsgType::Offer {
                    return DhcpAction::Ignore;
                }
                // Move to Requesting and emit a DHCPREQUEST.
                self.state = DhcpState::Requesting;
                let req = build_request(self.xid, self.mac, reply.yiaddr, reply.server_id);
                DhcpAction::SendRequest(req)
            }
            DhcpState::Requesting => match reply.msg_type {
                DhcpMsgType::Ack => {
                    self.state = DhcpState::Bound;
                    let cfg = DhcpConfig {
                        ip: reply.yiaddr,
                        mask: reply.subnet_mask.unwrap_or([255, 255, 255, 0]),
                        gateway: reply.router.unwrap_or([0, 0, 0, 0]),
                        dns: reply.dns.unwrap_or([0, 0, 0, 0]),
                        lease_secs: reply.lease_secs,
                    };
                    DhcpAction::Bound(cfg)
                }
                DhcpMsgType::Nak => {
                    self.state = DhcpState::Init;
                    self.xid = 0;
                    DhcpAction::Nak
                }
                _ => DhcpAction::Ignore,
            },
            // In Init or Bound states, ignore unsolicited replies.
            DhcpState::Init | DhcpState::Bound => DhcpAction::Ignore,
        }
    }

    /// Reset the client to [`DhcpState::Init`] (e.g. after a NAK or lease
    /// expiry).
    pub fn reset(&mut self) {
        self.state = DhcpState::Init;
        self.xid = 0;
    }
}

impl Default for DhcpClient {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Packet builders
// ---------------------------------------------------------------------------

/// Build a DHCPDISCOVER message (RFC 2131 §4.4.1, RFC 2132).
///
/// Returns a complete BOOTP/DHCP payload ready to be wrapped in a UDP
/// datagram sent from port 68 to 255.255.255.255:67.
///
/// Options included:
/// - Option 53 (DHCP Message Type) = 1 (DISCOVER)
/// - Option 55 (Parameter Request List): subnet mask (1), router (3), DNS (6)
/// - Option 255 (End)
pub fn build_discover(xid: u32, mac: MacAddr) -> Vec<u8> {
    let mut pkt = build_bootp_header(xid, mac);
    // Options: message type DISCOVER
    push_option_u8(&mut pkt, OPT_MSG_TYPE, DHCP_DISCOVER);
    // Options: parameter request list (subnet mask, router, DNS)
    pkt.extend_from_slice(&[OPT_PARAM_REQUEST, 3, OPT_SUBNET_MASK, OPT_ROUTER, OPT_DNS]);
    // End option
    pkt.push(OPT_END);
    pkt
}

/// Build a DHCPREQUEST message (RFC 2131 §4.3.2).
///
/// Sent after receiving a DHCPOFFER to confirm the client wants the offered
/// address from the specific server.
///
/// Options included:
/// - Option 53 (DHCP Message Type) = 3 (REQUEST)
/// - Option 50 (Requested IP Address) = `requested_ip`
/// - Option 54 (Server Identifier) = `server_id`
/// - Option 255 (End)
pub fn build_request(
    xid: u32,
    mac: MacAddr,
    requested_ip: Ipv4Addr,
    server_id: Ipv4Addr,
) -> Vec<u8> {
    let mut pkt = build_bootp_header(xid, mac);
    // Options: message type REQUEST
    push_option_u8(&mut pkt, OPT_MSG_TYPE, DHCP_REQUEST);
    // Option 50: requested IP
    push_option_ip(&mut pkt, OPT_REQUESTED_IP, requested_ip);
    // Option 54: server identifier
    push_option_ip(&mut pkt, OPT_SERVER_ID, server_id);
    // End option
    pkt.push(OPT_END);
    pkt
}

// ---------------------------------------------------------------------------
// Packet parser
// ---------------------------------------------------------------------------

/// Parse a raw DHCP reply payload (UDP body, not including IP/UDP headers).
///
/// Returns `None` for any truncated, malformed, or non-DHCP packet.  Never
/// panics regardless of input length or content.
///
/// # RFC 2131 §2
///
/// Validates the magic cookie at offset 236, then walks the options TLV
/// list.  Only the subset of options needed by the client state machine is
/// decoded; unknown options are skipped.
pub fn parse_reply(buf: &[u8]) -> Option<DhcpReply> {
    // Must be long enough for the fixed BOOTP header + magic cookie + END.
    if buf.len() < MIN_DHCP_LEN {
        return None;
    }

    // op field: 1 = BOOTREQUEST, 2 = BOOTREPLY.  We only accept replies.
    // (RFC 2131 §2: op=2 for server-to-client.)
    // We do not validate op here — some relay configurations forward the
    // original BOOTREQUEST value.  The caller can filter if needed.

    // Validate magic cookie (RFC 2131 §3, last 4 bytes of BOOTP fixed header).
    if buf[MAGIC_COOKIE_OFFSET..MAGIC_COOKIE_OFFSET + 4] != MAGIC_COOKIE {
        return None;
    }

    // Extract yiaddr (bytes 16–19, RFC 2131 §2 "Your IP Address").
    let mut yiaddr = [0u8; 4];
    yiaddr.copy_from_slice(&buf[16..20]);

    // Walk options starting at byte 240 (after magic cookie).
    let opts = &buf[MAGIC_COOKIE_OFFSET + 4..];

    let mut msg_type: Option<DhcpMsgType> = None;
    let mut server_id: Ipv4Addr = [0u8; 4];
    let mut subnet_mask: Option<Ipv4Addr> = None;
    let mut router: Option<Ipv4Addr> = None;
    let mut dns: Option<Ipv4Addr> = None;
    let mut lease_secs: Option<u32> = None;

    let mut i = 0usize;
    while i < opts.len() {
        let code = opts[i];
        i += 1;

        match code {
            // Pad byte (RFC 2132 §3.1) — no length follows.
            0 => continue,
            // End option — stop parsing.
            OPT_END => break,
            _ => {
                // All other options: need at least a length byte.
                if i >= opts.len() {
                    return None; // truncated
                }
                let len = opts[i] as usize;
                i += 1;
                // Guard against read past end.
                if i + len > opts.len() {
                    return None; // truncated
                }
                let data = &opts[i..i + len];
                i += len;

                match code {
                    OPT_MSG_TYPE if len == 1 => {
                        msg_type = Some(decode_msg_type(data[0]));
                    }
                    OPT_SUBNET_MASK if len == 4 => {
                        let mut a = [0u8; 4];
                        a.copy_from_slice(data);
                        subnet_mask = Some(a);
                    }
                    OPT_ROUTER if len >= 4 => {
                        let mut a = [0u8; 4];
                        a.copy_from_slice(&data[..4]);
                        router = Some(a);
                    }
                    OPT_DNS if len >= 4 => {
                        let mut a = [0u8; 4];
                        a.copy_from_slice(&data[..4]);
                        dns = Some(a);
                    }
                    OPT_LEASE_TIME if len == 4 => {
                        lease_secs = Some(u32::from_be_bytes([data[0], data[1], data[2], data[3]]));
                    }
                    OPT_SERVER_ID if len == 4 => {
                        server_id.copy_from_slice(data);
                    }
                    // Unknown or unexpected-length option — skip.
                    _ => {}
                }
            }
        }
    }

    // Option 53 (message type) is mandatory in DHCP (RFC 2131 §3.1).
    let msg_type = msg_type?;

    Some(DhcpReply {
        msg_type,
        yiaddr,
        server_id,
        subnet_mask,
        router,
        dns,
        lease_secs,
    })
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Build the 236-byte BOOTP fixed header followed by the 4-byte magic cookie.
///
/// Layout (RFC 2131 §2):
/// ```text
/// op(1) htype(1) hlen(1) hops(1) xid(4) secs(2) flags(2)
/// ciaddr(4) yiaddr(4) siaddr(4) giaddr(4) chaddr(16)
/// sname(64) file(128)
/// magic(4)
/// ```
fn build_bootp_header(xid: u32, mac: MacAddr) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(300);

    // op, htype, hlen, hops
    pkt.push(BOOTREQUEST);
    pkt.push(HTYPE_ETHERNET);
    pkt.push(HLEN_ETHERNET);
    pkt.push(HOPS_ZERO);

    // xid (transaction ID)
    pkt.extend_from_slice(&xid.to_be_bytes());

    // secs, flags (broadcast bit set so servers know to reply by broadcast)
    pkt.extend_from_slice(&SECS_ZERO.to_be_bytes());
    pkt.extend_from_slice(&FLAGS_BROADCAST.to_be_bytes());

    // ciaddr (client IP) — 0.0.0.0 in DISCOVER/initial REQUEST
    pkt.extend_from_slice(&[0u8; 4]);
    // yiaddr — 0.0.0.0
    pkt.extend_from_slice(&[0u8; 4]);
    // siaddr — 0.0.0.0
    pkt.extend_from_slice(&[0u8; 4]);
    // giaddr — 0.0.0.0
    pkt.extend_from_slice(&[0u8; 4]);

    // chaddr: 16 bytes, first 6 = MAC, rest = 0
    pkt.extend_from_slice(&mac);
    pkt.extend_from_slice(&[0u8; 10]);

    // sname: 64 bytes of zero
    pkt.extend_from_slice(&[0u8; 64]);

    // file: 128 bytes of zero
    pkt.extend_from_slice(&[0u8; 128]);

    // Magic cookie (RFC 2131 §3)
    pkt.extend_from_slice(&MAGIC_COOKIE);

    debug_assert_eq!(pkt.len(), MAGIC_COOKIE_OFFSET + 4);
    pkt
}

/// Append a 1-byte-value DHCP option (code, length=1, value).
#[inline]
fn push_option_u8(pkt: &mut Vec<u8>, code: u8, value: u8) {
    pkt.push(code);
    pkt.push(1);
    pkt.push(value);
}

/// Append a 4-byte-IPv4 DHCP option (code, length=4, addr).
#[inline]
fn push_option_ip(pkt: &mut Vec<u8>, code: u8, addr: Ipv4Addr) {
    pkt.push(code);
    pkt.push(4);
    pkt.extend_from_slice(&addr);
}

/// Map a raw DHCP message-type byte (option 53 value) to [`DhcpMsgType`].
#[inline]
fn decode_msg_type(v: u8) -> DhcpMsgType {
    match v {
        DHCP_OFFER => DhcpMsgType::Offer,
        DHCP_ACK => DhcpMsgType::Ack,
        DHCP_NAK => DhcpMsgType::Nak,
        other => DhcpMsgType::Other(other),
    }
}

// ---------------------------------------------------------------------------
// Host-only tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Helpers to build synthetic server replies for parse tests.
    // -----------------------------------------------------------------------

    /// Build a minimal server-side BOOTREPLY skeleton (all fixed fields set,
    /// options area starts immediately after the magic cookie).
    fn make_reply_skeleton(yiaddr: Ipv4Addr) -> Vec<u8> {
        let mut pkt = vec![0u8; MAGIC_COOKIE_OFFSET];
        // op = BOOTREPLY (2)
        pkt[0] = 2;
        // htype = Ethernet
        pkt[1] = HTYPE_ETHERNET;
        // hlen = 6
        pkt[2] = HLEN_ETHERNET;
        // yiaddr at offset 16
        pkt[16..20].copy_from_slice(&yiaddr);
        // Magic cookie
        pkt.extend_from_slice(&MAGIC_COOKIE);
        pkt
    }

    /// Append a 1-byte option to a packet buffer.
    fn append_opt_u8(buf: &mut Vec<u8>, code: u8, val: u8) {
        buf.push(code);
        buf.push(1);
        buf.push(val);
    }

    /// Append a 4-byte IPv4 option.
    fn append_opt_ip(buf: &mut Vec<u8>, code: u8, addr: Ipv4Addr) {
        buf.push(code);
        buf.push(4);
        buf.extend_from_slice(&addr);
    }

    /// Append a 4-byte u32 option (big-endian).
    fn append_opt_u32(buf: &mut Vec<u8>, code: u8, val: u32) {
        buf.push(code);
        buf.push(4);
        buf.extend_from_slice(&val.to_be_bytes());
    }

    fn build_offer(
        yiaddr: Ipv4Addr,
        server_id: Ipv4Addr,
        mask: Ipv4Addr,
        router: Ipv4Addr,
        dns: Ipv4Addr,
        lease: u32,
    ) -> Vec<u8> {
        let mut pkt = make_reply_skeleton(yiaddr);
        append_opt_u8(&mut pkt, OPT_MSG_TYPE, DHCP_OFFER);
        append_opt_ip(&mut pkt, OPT_SERVER_ID, server_id);
        append_opt_ip(&mut pkt, OPT_SUBNET_MASK, mask);
        append_opt_ip(&mut pkt, OPT_ROUTER, router);
        append_opt_ip(&mut pkt, OPT_DNS, dns);
        append_opt_u32(&mut pkt, OPT_LEASE_TIME, lease);
        pkt.push(OPT_END);
        pkt
    }

    fn build_ack(
        yiaddr: Ipv4Addr,
        server_id: Ipv4Addr,
        mask: Ipv4Addr,
        router: Ipv4Addr,
        dns: Ipv4Addr,
        lease: u32,
    ) -> Vec<u8> {
        let mut pkt = make_reply_skeleton(yiaddr);
        append_opt_u8(&mut pkt, OPT_MSG_TYPE, DHCP_ACK);
        append_opt_ip(&mut pkt, OPT_SERVER_ID, server_id);
        append_opt_ip(&mut pkt, OPT_SUBNET_MASK, mask);
        append_opt_ip(&mut pkt, OPT_ROUTER, router);
        append_opt_ip(&mut pkt, OPT_DNS, dns);
        append_opt_u32(&mut pkt, OPT_LEASE_TIME, lease);
        pkt.push(OPT_END);
        pkt
    }

    fn build_nak(server_id: Ipv4Addr) -> Vec<u8> {
        let mut pkt = make_reply_skeleton([0, 0, 0, 0]);
        append_opt_u8(&mut pkt, OPT_MSG_TYPE, DHCP_NAK);
        append_opt_ip(&mut pkt, OPT_SERVER_ID, server_id);
        pkt.push(OPT_END);
        pkt
    }

    // -----------------------------------------------------------------------
    // DISCOVER byte-layout tests
    // -----------------------------------------------------------------------

    #[test]
    fn discover_magic_cookie_at_offset_236() {
        let mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
        let pkt = build_discover(0xDEAD_BEEF, mac);
        assert!(pkt.len() > MAGIC_COOKIE_OFFSET + 4, "packet too short");
        assert_eq!(
            &pkt[MAGIC_COOKIE_OFFSET..MAGIC_COOKIE_OFFSET + 4],
            &MAGIC_COOKIE,
            "magic cookie wrong"
        );
    }

    #[test]
    fn discover_op_and_htype() {
        let pkt = build_discover(1, [0xAA; 6]);
        assert_eq!(pkt[0], BOOTREQUEST, "op must be BOOTREQUEST");
        assert_eq!(pkt[1], HTYPE_ETHERNET, "htype must be Ethernet");
        assert_eq!(pkt[2], HLEN_ETHERNET, "hlen must be 6");
        assert_eq!(pkt[3], HOPS_ZERO, "hops must be 0");
    }

    #[test]
    fn discover_xid_encoded_big_endian() {
        let xid: u32 = 0x1234_5678;
        let pkt = build_discover(xid, [0; 6]);
        // xid starts at offset 4
        assert_eq!(&pkt[4..8], &xid.to_be_bytes());
    }

    #[test]
    fn discover_broadcast_flag_set() {
        let pkt = build_discover(1, [0; 6]);
        // flags at offset 10
        let flags = u16::from_be_bytes([pkt[10], pkt[11]]);
        assert_eq!(
            flags & FLAGS_BROADCAST,
            FLAGS_BROADCAST,
            "broadcast flag must be set"
        );
    }

    #[test]
    fn discover_chaddr_contains_mac() {
        let mac = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06];
        let pkt = build_discover(1, mac);
        // chaddr at offset 28
        assert_eq!(&pkt[28..34], &mac);
        // Remaining 10 chaddr bytes must be zero
        assert_eq!(&pkt[34..44], &[0u8; 10]);
    }

    #[test]
    fn discover_options_contain_msg_type_and_param_list() {
        let pkt = build_discover(0, [0; 6]);
        let opts = &pkt[MAGIC_COOKIE_OFFSET + 4..];

        // Expect: 53 01 01  (DISCOVER)  followed by  55 03 01 03 06  (PRL)
        assert!(opts.len() >= 8, "options too short");
        assert_eq!(opts[0], OPT_MSG_TYPE);
        assert_eq!(opts[1], 1);
        assert_eq!(opts[2], DHCP_DISCOVER);
        assert_eq!(opts[3], OPT_PARAM_REQUEST);
        assert_eq!(opts[4], 3);
        assert_eq!(opts[5], OPT_SUBNET_MASK);
        assert_eq!(opts[6], OPT_ROUTER);
        assert_eq!(opts[7], OPT_DNS);
        // Last byte must be END
        assert_eq!(*opts.last().unwrap(), OPT_END);
    }

    // -----------------------------------------------------------------------
    // REQUEST byte-layout tests
    // -----------------------------------------------------------------------

    #[test]
    fn request_magic_cookie_at_offset_236() {
        let mac = [0xAA; 6];
        let req_ip = [192, 168, 1, 100];
        let srv_id = [192, 168, 1, 1];
        let pkt = build_request(0x0000_0001, mac, req_ip, srv_id);
        assert_eq!(
            &pkt[MAGIC_COOKIE_OFFSET..MAGIC_COOKIE_OFFSET + 4],
            &MAGIC_COOKIE
        );
    }

    #[test]
    fn request_options_contain_required_options() {
        let mac = [0xBB; 6];
        let req_ip = [10, 0, 0, 50];
        let srv_id = [10, 0, 0, 1];
        let pkt = build_request(42, mac, req_ip, srv_id);
        let opts = &pkt[MAGIC_COOKIE_OFFSET + 4..];

        // Option 53 = REQUEST (3)
        assert_eq!(opts[0], OPT_MSG_TYPE);
        assert_eq!(opts[1], 1);
        assert_eq!(opts[2], DHCP_REQUEST);

        // Option 50: requested IP
        assert_eq!(opts[3], OPT_REQUESTED_IP);
        assert_eq!(opts[4], 4);
        assert_eq!(&opts[5..9], &req_ip);

        // Option 54: server ID
        assert_eq!(opts[9], OPT_SERVER_ID);
        assert_eq!(opts[10], 4);
        assert_eq!(&opts[11..15], &srv_id);

        // End
        assert_eq!(opts[15], OPT_END);
    }

    // -----------------------------------------------------------------------
    // parse_reply: valid OFFER
    // -----------------------------------------------------------------------

    #[test]
    fn parse_offer_round_trip() {
        let yiaddr = [10, 0, 2, 50];
        let server_id = [10, 0, 2, 1];
        let mask = [255, 255, 255, 0];
        let router = [10, 0, 2, 1];
        let dns = [8, 8, 8, 8];
        let lease = 86400u32;

        let raw = build_offer(yiaddr, server_id, mask, router, dns, lease);
        let reply = parse_reply(&raw).expect("should parse OFFER");

        assert_eq!(reply.msg_type, DhcpMsgType::Offer);
        assert_eq!(reply.yiaddr, yiaddr);
        assert_eq!(reply.server_id, server_id);
        assert_eq!(reply.subnet_mask, Some(mask));
        assert_eq!(reply.router, Some(router));
        assert_eq!(reply.dns, Some(dns));
        assert_eq!(reply.lease_secs, Some(lease));
    }

    // -----------------------------------------------------------------------
    // parse_reply: valid ACK
    // -----------------------------------------------------------------------

    #[test]
    fn parse_ack_round_trip() {
        let yiaddr = [192, 168, 0, 10];
        let server_id = [192, 168, 0, 1];
        let mask = [255, 255, 255, 0];
        let router = [192, 168, 0, 1];
        let dns = [1, 1, 1, 1];
        let lease = 3600u32;

        let raw = build_ack(yiaddr, server_id, mask, router, dns, lease);
        let reply = parse_reply(&raw).expect("should parse ACK");

        assert_eq!(reply.msg_type, DhcpMsgType::Ack);
        assert_eq!(reply.yiaddr, yiaddr);
        assert_eq!(reply.server_id, server_id);
        assert_eq!(reply.subnet_mask, Some(mask));
        assert_eq!(reply.router, Some(router));
        assert_eq!(reply.dns, Some(dns));
        assert_eq!(reply.lease_secs, Some(lease));
    }

    // -----------------------------------------------------------------------
    // parse_reply: valid NAK
    // -----------------------------------------------------------------------

    #[test]
    fn parse_nak_round_trip() {
        let server_id = [10, 0, 0, 1];
        let raw = build_nak(server_id);
        let reply = parse_reply(&raw).expect("should parse NAK");
        assert_eq!(reply.msg_type, DhcpMsgType::Nak);
        assert_eq!(reply.server_id, server_id);
    }

    // -----------------------------------------------------------------------
    // parse_reply: truncation safety (never panic)
    // -----------------------------------------------------------------------

    #[test]
    fn parse_reply_empty_returns_none() {
        assert!(parse_reply(&[]).is_none());
    }

    #[test]
    fn parse_reply_too_short_returns_none() {
        // Just under the minimum length
        let short = vec![0u8; MIN_DHCP_LEN - 1];
        assert!(parse_reply(&short).is_none());
    }

    #[test]
    fn parse_reply_wrong_magic_cookie_returns_none() {
        let mut pkt = make_reply_skeleton([10, 0, 0, 1]);
        // Bad cookie
        pkt.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        pkt.push(OPT_END);
        assert!(parse_reply(&pkt).is_none());
    }

    #[test]
    fn parse_reply_truncated_option_value_returns_none() {
        // Build a packet with an option that claims len=4 but only 2 bytes follow.
        let mut pkt = make_reply_skeleton([10, 0, 0, 1]);
        // Valid msg type so we get past that check.
        pkt.push(OPT_MSG_TYPE);
        pkt.push(1);
        pkt.push(DHCP_ACK);
        // Server-id claims 4 bytes but we only write 2.
        pkt.push(OPT_SERVER_ID);
        pkt.push(4);
        pkt.push(10);
        pkt.push(0);
        // No more bytes — truncated.
        assert!(parse_reply(&pkt).is_none());
    }

    #[test]
    fn parse_reply_truncated_length_byte_returns_none() {
        // Option code present but no length byte at all.
        let mut pkt = make_reply_skeleton([0; 4]);
        pkt.push(OPT_MSG_TYPE);
        // No length byte follows — the slice ends here.
        assert!(parse_reply(&pkt).is_none());
    }

    #[test]
    fn parse_reply_missing_msg_type_returns_none() {
        // A packet with only the END option (no option 53) should return None.
        let mut pkt = make_reply_skeleton([10, 0, 0, 5]);
        pkt.push(OPT_END);
        assert!(parse_reply(&pkt).is_none());
    }

    #[test]
    fn parse_reply_with_pad_bytes() {
        // Option 0 (pad) should be skipped silently.
        let mut pkt = make_reply_skeleton([10, 0, 0, 7]);
        // Three pad bytes before msg type.
        pkt.push(0); // pad
        pkt.push(0); // pad
        pkt.push(0); // pad
        append_opt_u8(&mut pkt, OPT_MSG_TYPE, DHCP_OFFER);
        append_opt_ip(&mut pkt, OPT_SERVER_ID, [10, 0, 0, 1]);
        pkt.push(OPT_END);
        let reply = parse_reply(&pkt).expect("pads should be ignored");
        assert_eq!(reply.msg_type, DhcpMsgType::Offer);
    }

    #[test]
    fn parse_reply_unknown_option_skipped() {
        // Option code 200 (unknown) with 3 bytes — should be silently skipped.
        let mut pkt = make_reply_skeleton([10, 0, 0, 9]);
        pkt.push(200u8); // unknown option code
        pkt.push(3);
        pkt.extend_from_slice(&[0xDE, 0xAD, 0xBE]);
        append_opt_u8(&mut pkt, OPT_MSG_TYPE, DHCP_ACK);
        append_opt_ip(&mut pkt, OPT_SERVER_ID, [10, 0, 0, 1]);
        pkt.push(OPT_END);
        let reply = parse_reply(&pkt).expect("unknown options should be skipped");
        assert_eq!(reply.msg_type, DhcpMsgType::Ack);
    }

    // -----------------------------------------------------------------------
    // State machine: full Init → Selecting → Requesting → Bound walk
    // -----------------------------------------------------------------------

    #[test]
    fn state_machine_full_walk_init_to_bound() {
        let mac = [0x52, 0x54, 0x00, 0xAB, 0xCD, 0xEF];
        let xid = 0xCAFE_BABE;
        let server_ip = [10, 0, 0, 1];
        let offered_ip = [10, 0, 0, 100];
        let mask = [255, 255, 255, 0];
        let router = [10, 0, 0, 1];
        let dns = [8, 8, 4, 4];
        let lease = 7200u32;

        let mut client = DhcpClient::new();
        assert_eq!(client.state, DhcpState::Init);

        // --- Step 1: start → DISCOVER ---
        let discover = client.start(mac, xid);
        assert_eq!(client.state, DhcpState::Selecting);
        assert_eq!(client.xid, xid);
        // Sanity: it's a valid discover
        assert_eq!(
            &discover[MAGIC_COOKIE_OFFSET..MAGIC_COOKIE_OFFSET + 4],
            &MAGIC_COOKIE
        );

        // --- Step 2: receive OFFER → REQUEST ---
        let offer_raw = build_offer(offered_ip, server_ip, mask, router, dns, lease);
        let offer = parse_reply(&offer_raw).unwrap();
        let action = client.on_reply(offer);
        assert_eq!(client.state, DhcpState::Requesting);

        let request = match action {
            DhcpAction::SendRequest(bytes) => bytes,
            other => panic!("expected SendRequest, got {:?}", other),
        };
        // REQUEST must contain the offered IP (option 50) and server ID (option 54).
        let opts = &request[MAGIC_COOKIE_OFFSET + 4..];
        // Find option 53 = REQUEST (3)
        assert_eq!(opts[0], OPT_MSG_TYPE);
        assert_eq!(opts[2], DHCP_REQUEST);
        // Find option 50: offered IP
        assert_eq!(opts[3], OPT_REQUESTED_IP);
        assert_eq!(&opts[5..9], &offered_ip);
        // Find option 54: server ID
        assert_eq!(opts[9], OPT_SERVER_ID);
        assert_eq!(&opts[11..15], &server_ip);

        // --- Step 3: receive ACK → Bound ---
        let ack_raw = build_ack(offered_ip, server_ip, mask, router, dns, lease);
        let ack = parse_reply(&ack_raw).unwrap();
        let action2 = client.on_reply(ack);
        assert_eq!(client.state, DhcpState::Bound);

        let cfg = match action2 {
            DhcpAction::Bound(c) => c,
            other => panic!("expected Bound, got {:?}", other),
        };
        assert_eq!(cfg.ip, offered_ip);
        assert_eq!(cfg.mask, mask);
        assert_eq!(cfg.gateway, router);
        assert_eq!(cfg.dns, dns);
        assert_eq!(cfg.lease_secs, Some(lease));
    }

    // -----------------------------------------------------------------------
    // State machine: NAK path
    // -----------------------------------------------------------------------

    #[test]
    fn state_machine_nak_resets_to_init() {
        let mac = [0x00, 0x11, 0x22, 0x33, 0x44, 0x55];
        let xid = 0x0000_0042;
        let server_ip = [172, 16, 0, 1];
        let offered_ip = [172, 16, 0, 50];
        let mask = [255, 255, 0, 0];

        let mut client = DhcpClient::new();
        client.start(mac, xid);

        // Receive OFFER to move to Requesting.
        let offer_raw = build_offer(
            offered_ip,
            server_ip,
            mask,
            [172, 16, 0, 1],
            [8, 8, 8, 8],
            3600,
        );
        let offer = parse_reply(&offer_raw).unwrap();
        let action = client.on_reply(offer);
        assert!(matches!(action, DhcpAction::SendRequest(_)));
        assert_eq!(client.state, DhcpState::Requesting);

        // Receive NAK → reset to Init.
        let nak_raw = build_nak(server_ip);
        let nak = parse_reply(&nak_raw).unwrap();
        let action2 = client.on_reply(nak);
        assert!(matches!(action2, DhcpAction::Nak));
        assert_eq!(client.state, DhcpState::Init);
        assert_eq!(client.xid, 0, "xid should be cleared after NAK");
    }

    // -----------------------------------------------------------------------
    // State machine: Ignore in wrong states
    // -----------------------------------------------------------------------

    #[test]
    fn state_machine_ignores_offer_in_bound_state() {
        let mac = [0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01];
        let xid = 1;
        let ip = [192, 168, 1, 42];
        let srv = [192, 168, 1, 1];
        let mask = [255, 255, 255, 0];

        let mut client = DhcpClient::new();
        client.start(mac, xid);

        // Drive to Bound.
        let offer_raw = build_offer(ip, srv, mask, srv, [8, 8, 8, 8], 600);
        let offer = parse_reply(&offer_raw).unwrap();
        client.on_reply(offer);
        let ack_raw = build_ack(ip, srv, mask, srv, [8, 8, 8, 8], 600);
        let ack = parse_reply(&ack_raw).unwrap();
        client.on_reply(ack);
        assert_eq!(client.state, DhcpState::Bound);

        // A new OFFER in Bound state should be ignored.
        let offer2_raw = build_offer(
            [1, 2, 3, 4],
            [5, 6, 7, 8],
            mask,
            [5, 6, 7, 8],
            [8, 8, 8, 8],
            600,
        );
        let offer2 = parse_reply(&offer2_raw).unwrap();
        let action = client.on_reply(offer2);
        assert!(matches!(action, DhcpAction::Ignore));
        assert_eq!(client.state, DhcpState::Bound, "state must not change");
    }

    #[test]
    fn state_machine_ignores_ack_in_selecting_state() {
        let mac = [0x11; 6];
        let mut client = DhcpClient::new();
        client.start(mac, 0xBEEF_DEAD);
        assert_eq!(client.state, DhcpState::Selecting);

        // An ACK arriving while Selecting should be ignored.
        let ack_raw = build_ack(
            [10, 0, 0, 5],
            [10, 0, 0, 1],
            [255, 255, 255, 0],
            [10, 0, 0, 1],
            [8, 8, 8, 8],
            3600,
        );
        let ack = parse_reply(&ack_raw).unwrap();
        let action = client.on_reply(ack);
        assert!(matches!(action, DhcpAction::Ignore));
        assert_eq!(client.state, DhcpState::Selecting);
    }

    #[test]
    fn state_machine_ignores_reply_in_init_state() {
        let mut client = DhcpClient::new();
        assert_eq!(client.state, DhcpState::Init);

        let offer_raw = build_offer(
            [10, 0, 0, 2],
            [10, 0, 0, 1],
            [255, 255, 255, 0],
            [10, 0, 0, 1],
            [8, 8, 8, 8],
            600,
        );
        let offer = parse_reply(&offer_raw).unwrap();
        let action = client.on_reply(offer);
        assert!(matches!(action, DhcpAction::Ignore));
    }

    // -----------------------------------------------------------------------
    // State machine: reset helper
    // -----------------------------------------------------------------------

    #[test]
    fn reset_returns_to_init() {
        let mut client = DhcpClient::new();
        client.start([0; 6], 0xFFFF_FFFF);
        assert_eq!(client.state, DhcpState::Selecting);
        client.reset();
        assert_eq!(client.state, DhcpState::Init);
        assert_eq!(client.xid, 0);
    }

    // -----------------------------------------------------------------------
    // Miscellaneous correctness checks
    // -----------------------------------------------------------------------

    #[test]
    fn discover_packet_length_is_at_least_min() {
        let pkt = build_discover(0, [0; 6]);
        assert!(
            pkt.len() >= MIN_DHCP_LEN,
            "discover must be at least {} bytes",
            MIN_DHCP_LEN
        );
    }

    #[test]
    fn request_packet_length_is_at_least_min() {
        let pkt = build_request(0, [0; 6], [0; 4], [0; 4]);
        assert!(pkt.len() >= MIN_DHCP_LEN);
    }

    #[test]
    fn dhcp_config_fallback_values_when_options_absent() {
        // A minimal ACK with only msg type + server ID — no mask/router/DNS/lease.
        let mut pkt = make_reply_skeleton([172, 31, 0, 5]);
        append_opt_u8(&mut pkt, OPT_MSG_TYPE, DHCP_ACK);
        append_opt_ip(&mut pkt, OPT_SERVER_ID, [172, 31, 0, 1]);
        pkt.push(OPT_END);
        let reply = parse_reply(&pkt).unwrap();

        let mut client = DhcpClient::new();
        client.start([0xCC; 6], 99);
        // Move to Requesting by feeding a dummy OFFER first.
        let offer_pkt = build_offer(
            reply.yiaddr,
            reply.server_id,
            [255, 255, 255, 0],
            [172, 31, 0, 1],
            [8, 8, 8, 8],
            600,
        );
        let offer = parse_reply(&offer_pkt).unwrap();
        client.on_reply(offer);

        let action = client.on_reply(reply);
        let cfg = match action {
            DhcpAction::Bound(c) => c,
            other => panic!("expected Bound, got {:?}", other),
        };
        // Fallback for missing options
        assert_eq!(cfg.mask, [255, 255, 255, 0]);
        assert_eq!(cfg.gateway, [0, 0, 0, 0]);
        assert_eq!(cfg.dns, [0, 0, 0, 0]);
        assert_eq!(cfg.lease_secs, None);
    }

    #[test]
    fn parse_other_msg_type() {
        // Message type 4 (DECLINE) — not handled by the SM but must parse cleanly.
        let mut pkt = make_reply_skeleton([0; 4]);
        append_opt_u8(&mut pkt, OPT_MSG_TYPE, 4);
        append_opt_ip(&mut pkt, OPT_SERVER_ID, [1, 2, 3, 4]);
        pkt.push(OPT_END);
        let reply = parse_reply(&pkt).unwrap();
        assert_eq!(reply.msg_type, DhcpMsgType::Other(4));
    }
}
