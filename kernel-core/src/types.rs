/// Unique task identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskId(pub u64);

/// Index into the global endpoint registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndpointId(pub u8);

/// Index into the global notification registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotifId(pub u8);

/// MAC address as 6 bytes.
pub type MacAddr = [u8; 6];

/// IPv4 address as 4 bytes.
pub type Ipv4Addr = [u8; 4];
