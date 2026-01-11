use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Information about a public IP address lookup
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PublicIpInfo {
    /// The public IP address, if successfully retrieved
    pub ip: Option<String>,
    /// The source service that provided the IP
    pub source: Option<String>,
    /// Error message if IP lookup failed
    pub error: Option<String>,
}

/// Information about a network interface
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NetworkInterface {
    /// IP address of the interface
    pub address: String,
    /// Name of the network interface
    pub interface: String,
    /// Whether this is a private IP address (RFC 1918)
    pub is_private: Option<bool>,
    /// Whether this is a link-local address (IPv6)
    pub is_link_local: Option<bool>,
    /// Whether this is a unique local address (IPv6)
    pub is_unique_local: Option<bool>,
}

/// Information about private/local IP addresses
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PrivateIpInfo {
    /// The primary private IP address (most likely to be useful)
    pub primary_ip: Option<String>,
    /// All IPv4 addresses found on non-loopback interfaces
    pub ipv4_addresses: Vec<NetworkInterface>,
    /// All IPv6 addresses found on non-loopback interfaces
    pub ipv6_addresses: Vec<NetworkInterface>,
}
