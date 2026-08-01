//! WireGuard mesh networking for Temps multi-node deployments.
//!
//! Uses `defguard_wireguard_rs` for embedded userspace WireGuard — no external
//! `wireguard-tools` package or kernel module required. The WireGuard protocol
//! runs in-process via boringtun (Cloudflare's Rust implementation).

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum WireGuardError {
    #[error("WireGuard operation failed: {operation} — {reason}")]
    OperationFailed { operation: String, reason: String },

    #[error("WireGuard interface error: {0}")]
    InterfaceError(String),

    #[error("No available IP addresses in subnet {subnet}")]
    SubnetExhausted { subnet: String },

    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Interface {interface} already exists")]
    InterfaceExists { interface: String },

    #[error("Peer with public key {public_key} already configured")]
    PeerExists { public_key: String },
}

/// A WireGuard peer (another node in the mesh).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireGuardPeer {
    /// Base64-encoded public key
    pub public_key: String,
    /// External endpoint, e.g. "203.0.113.50:51820"
    pub endpoint: String,
    /// Allowed IPs for this peer, e.g. "10.100.0.2/32"
    pub allowed_ips: String,
}

/// Keypair generated for a node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireGuardKeypair {
    pub private_key: String,
    pub public_key: String,
}

/// Manages a WireGuard interface for the Temps mesh network.
///
/// Uses embedded userspace WireGuard via defguard/boringtun — no external
/// `wg` or `ip` CLI tools required.
#[derive(Debug)]
pub struct WireGuardManager {
    /// Interface name, e.g. "wg0"
    interface: String,
    /// Subnet prefix, e.g. "10.100.0"
    subnet_prefix: String,
    /// Subnet mask bits
    subnet_mask: u8,
    /// WireGuard listen port
    listen_port: u16,
}

impl WireGuardManager {
    /// Create a new manager for the given interface and subnet.
    ///
    /// Default subnet: 10.100.0.0/24, listen port: 51820.
    pub fn new(interface: &str, subnet: &str, listen_port: u16) -> Result<Self, WireGuardError> {
        let parts: Vec<&str> = subnet.split('/').collect();
        if parts.len() != 2 {
            return Err(WireGuardError::InvalidConfig(format!(
                "Invalid subnet format '{}', expected CIDR notation like '10.100.0.0/24'",
                subnet
            )));
        }

        let ip_parts: Vec<&str> = parts[0].split('.').collect();
        if ip_parts.len() != 4 {
            return Err(WireGuardError::InvalidConfig(format!(
                "Invalid IP in subnet: {}",
                parts[0]
            )));
        }

        let mask: u8 = parts[1].parse().map_err(|_| {
            WireGuardError::InvalidConfig(format!("Invalid subnet mask: {}", parts[1]))
        })?;

        let subnet_prefix = format!("{}.{}.{}", ip_parts[0], ip_parts[1], ip_parts[2]);

        Ok(Self {
            interface: interface.to_string(),
            subnet_prefix,
            subnet_mask: mask,
            listen_port,
        })
    }

    /// Create with default settings (wg0, 10.100.0.0/24, port 51820).
    pub fn default_config() -> Result<Self, WireGuardError> {
        let subnet = std::env::var("TEMPS_WG_SUBNET").unwrap_or_else(|_| "10.100.0.0/24".into());
        let port: u16 = std::env::var("TEMPS_WG_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(51820);
        Self::new("wg0", &subnet, port)
    }

    /// Check if WireGuard is available.
    ///
    /// With embedded userspace WireGuard this always succeeds — no external tools needed.
    pub async fn check_available(&self) -> Result<(), WireGuardError> {
        Ok(())
    }

    /// Generate a new WireGuard keypair using pure Rust cryptography.
    ///
    /// Uses x25519-dalek for Curve25519 key generation — no `wg genkey` needed.
    pub async fn generate_keypair(&self) -> Result<WireGuardKeypair, WireGuardError> {
        let secret = temps_core::ecies::generate_x25519_static_secret().map_err(|error| {
            WireGuardError::OperationFailed {
                operation: "generate X25519 keypair".to_string(),
                reason: error.to_string(),
            }
        })?;
        let public = x25519_dalek::PublicKey::from(&secret);

        let private_key = BASE64.encode(secret.as_bytes());
        let public_key = BASE64.encode(public.as_bytes());

        Ok(WireGuardKeypair {
            private_key,
            public_key,
        })
    }

    /// Initialize the WireGuard interface with the given IP address and private key.
    ///
    /// Creates a userspace WireGuard interface via defguard/boringtun.
    /// No external `wg` or `ip` CLI tools are needed.
    pub async fn init_interface(
        &self,
        ip: Ipv4Addr,
        private_key: &str,
    ) -> Result<(), WireGuardError> {
        use defguard_wireguard_rs::{
            InterfaceConfiguration, Userspace, WGApi, WireguardInterfaceApi,
        };
        use std::str::FromStr;

        let mut wgapi = WGApi::<Userspace>::new(self.interface.clone()).map_err(|e| {
            WireGuardError::InterfaceError(format!(
                "Failed to create WireGuard API for {}: {}",
                self.interface, e
            ))
        })?;

        // Create the userspace WireGuard interface (TUN device via boringtun)
        wgapi.create_interface().map_err(|e| {
            WireGuardError::InterfaceError(format!(
                "Failed to create interface {}: {}",
                self.interface, e
            ))
        })?;

        // Configure the interface with private key, port, and address
        let addr_str = format!("{}/{}", ip, self.subnet_mask);
        let address = defguard_wireguard_rs::net::IpAddrMask::from_str(&addr_str).map_err(|e| {
            WireGuardError::InvalidConfig(format!("Invalid address {}: {}", addr_str, e))
        })?;

        let config = InterfaceConfiguration {
            name: self.interface.clone(),
            prvkey: private_key.to_string(),
            addresses: vec![address],
            port: self.listen_port,
            peers: Vec::new(),
            mtu: None,
            fwmark: None,
        };

        wgapi.configure_interface(&config).map_err(|e| {
            WireGuardError::InterfaceError(format!(
                "Failed to configure interface {}: {}",
                self.interface, e
            ))
        })?;

        tracing::info!(
            interface = %self.interface,
            ip = %ip,
            port = %self.listen_port,
            "WireGuard interface initialized (embedded userspace)"
        );

        Ok(())
    }

    /// Add a peer to the WireGuard interface.
    pub async fn add_peer(&self, peer: &WireGuardPeer) -> Result<(), WireGuardError> {
        use defguard_wireguard_rs::{Userspace, WGApi, WireguardInterfaceApi};

        let wgapi = WGApi::<Userspace>::new(self.interface.clone()).map_err(|e| {
            WireGuardError::InterfaceError(format!(
                "Failed to create WireGuard API for {}: {}",
                self.interface, e
            ))
        })?;

        // Parse the base64 public key into a Key
        let key: defguard_wireguard_rs::key::Key =
            peer.public_key.as_str().try_into().map_err(|e| {
                WireGuardError::InvalidConfig(format!(
                    "Invalid peer public key '{}': {:?}",
                    peer.public_key, e
                ))
            })?;

        let mut wg_peer = defguard_wireguard_rs::peer::Peer::new(key);

        // Parse endpoint
        if !peer.endpoint.is_empty() {
            wg_peer.set_endpoint(&peer.endpoint).map_err(|e| {
                WireGuardError::InvalidConfig(format!(
                    "Invalid peer endpoint '{}': {}",
                    peer.endpoint, e
                ))
            })?;
        }

        // Parse allowed IPs
        if let Ok(addr_mask) = peer
            .allowed_ips
            .parse::<defguard_wireguard_rs::net::IpAddrMask>()
        {
            wg_peer.allowed_ips.push(addr_mask);
        }

        // Enable persistent keepalive for NAT traversal
        wg_peer.persistent_keepalive_interval = Some(25);

        wgapi
            .configure_peer(&wg_peer)
            .map_err(|e| WireGuardError::OperationFailed {
                operation: format!("add peer {}", peer.public_key),
                reason: format!("{}", e),
            })?;

        tracing::info!(
            interface = %self.interface,
            peer_key = %peer.public_key,
            endpoint = %peer.endpoint,
            allowed_ips = %peer.allowed_ips,
            "WireGuard peer added"
        );

        Ok(())
    }

    /// Remove a peer from the WireGuard interface.
    pub async fn remove_peer(&self, public_key: &str) -> Result<(), WireGuardError> {
        use defguard_wireguard_rs::{Userspace, WGApi, WireguardInterfaceApi};

        let wgapi = WGApi::<Userspace>::new(self.interface.clone()).map_err(|e| {
            WireGuardError::InterfaceError(format!(
                "Failed to create WireGuard API for {}: {}",
                self.interface, e
            ))
        })?;

        let key: defguard_wireguard_rs::key::Key = public_key.try_into().map_err(|e| {
            WireGuardError::InvalidConfig(format!("Invalid public key '{}': {:?}", public_key, e))
        })?;

        wgapi
            .remove_peer(&key)
            .map_err(|e| WireGuardError::OperationFailed {
                operation: format!("remove peer {}", public_key),
                reason: format!("{}", e),
            })?;

        tracing::info!(
            interface = %self.interface,
            peer_key = %public_key,
            "WireGuard peer removed"
        );

        Ok(())
    }

    /// Tear down the WireGuard interface.
    pub async fn destroy_interface(&self) -> Result<(), WireGuardError> {
        use defguard_wireguard_rs::{Userspace, WGApi, WireguardInterfaceApi};

        let wgapi = WGApi::<Userspace>::new(self.interface.clone()).map_err(|e| {
            WireGuardError::InterfaceError(format!(
                "Failed to create WireGuard API for {}: {}",
                self.interface, e
            ))
        })?;

        wgapi.remove_interface().map_err(|e| {
            WireGuardError::InterfaceError(format!(
                "Failed to remove interface {}: {}",
                self.interface, e
            ))
        })?;

        tracing::info!(
            interface = %self.interface,
            "WireGuard interface destroyed"
        );

        Ok(())
    }

    /// Get the next available IP in the subnet, given a list of already-assigned IPs.
    ///
    /// Skips .0 (network) and .255 (broadcast). Starts from .1.
    pub fn next_available_ip(&self, existing: &[Ipv4Addr]) -> Result<Ipv4Addr, WireGuardError> {
        for last_octet in 1..255u8 {
            let candidate: Ipv4Addr = format!("{}.{}", self.subnet_prefix, last_octet)
                .parse()
                .map_err(|_| {
                    WireGuardError::InvalidConfig(format!(
                        "Failed to construct IP from prefix {} and octet {}",
                        self.subnet_prefix, last_octet
                    ))
                })?;

            if !existing.contains(&candidate) {
                return Ok(candidate);
            }
        }

        Err(WireGuardError::SubnetExhausted {
            subnet: format!("{}.0/{}", self.subnet_prefix, self.subnet_mask),
        })
    }

    /// Get the interface name.
    pub fn interface(&self) -> &str {
        &self.interface
    }

    /// Get the listen port.
    pub fn listen_port(&self) -> u16 {
        self.listen_port
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wireguard_manager_creation() {
        let manager = WireGuardManager::new("wg0", "10.100.0.0/24", 51820).unwrap();
        assert_eq!(manager.interface(), "wg0");
        assert_eq!(manager.listen_port(), 51820);
        assert_eq!(manager.subnet_prefix, "10.100.0");
        assert_eq!(manager.subnet_mask, 24);
    }

    #[test]
    fn test_wireguard_manager_invalid_subnet() {
        let result = WireGuardManager::new("wg0", "invalid", 51820);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            WireGuardError::InvalidConfig(_)
        ));
    }

    #[test]
    fn test_next_available_ip_empty() {
        let manager = WireGuardManager::new("wg0", "10.100.0.0/24", 51820).unwrap();
        let ip = manager.next_available_ip(&[]).unwrap();
        assert_eq!(ip, Ipv4Addr::new(10, 100, 0, 1));
    }

    #[test]
    fn test_next_available_ip_skips_existing() {
        let manager = WireGuardManager::new("wg0", "10.100.0.0/24", 51820).unwrap();
        let existing = vec![Ipv4Addr::new(10, 100, 0, 1), Ipv4Addr::new(10, 100, 0, 2)];
        let ip = manager.next_available_ip(&existing).unwrap();
        assert_eq!(ip, Ipv4Addr::new(10, 100, 0, 3));
    }

    #[test]
    fn test_next_available_ip_exhausted() {
        let manager = WireGuardManager::new("wg0", "10.100.0.0/24", 51820).unwrap();
        let existing: Vec<Ipv4Addr> = (1..255u8).map(|i| Ipv4Addr::new(10, 100, 0, i)).collect();
        let result = manager.next_available_ip(&existing);
        assert!(matches!(
            result.unwrap_err(),
            WireGuardError::SubnetExhausted { .. }
        ));
    }

    #[test]
    fn test_wireguard_peer_serialization() {
        let peer = WireGuardPeer {
            public_key: "abc123def456".to_string(),
            endpoint: "203.0.113.50:51820".to_string(),
            allowed_ips: "10.100.0.2/32".to_string(),
        };

        let json = serde_json::to_string(&peer).unwrap();
        let deserialized: WireGuardPeer = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.public_key, peer.public_key);
        assert_eq!(deserialized.endpoint, peer.endpoint);
        assert_eq!(deserialized.allowed_ips, peer.allowed_ips);
    }

    #[tokio::test]
    async fn test_generate_keypair_produces_valid_keys() {
        let manager = WireGuardManager::new("wg0", "10.100.0.0/24", 51820).unwrap();
        let keypair = manager.generate_keypair().await.unwrap();

        // Keys should be valid base64
        let private_bytes = BASE64.decode(&keypair.private_key).unwrap();
        let public_bytes = BASE64.decode(&keypair.public_key).unwrap();

        // Keys should be 32 bytes (Curve25519)
        assert_eq!(private_bytes.len(), 32);
        assert_eq!(public_bytes.len(), 32);

        // Public key should derive from private key
        let secret = x25519_dalek::StaticSecret::from(
            <[u8; 32]>::try_from(private_bytes.as_slice()).unwrap(),
        );
        let expected_public = x25519_dalek::PublicKey::from(&secret);
        assert_eq!(public_bytes, expected_public.as_bytes());
    }

    #[tokio::test]
    async fn test_generate_keypair_unique() {
        let manager = WireGuardManager::new("wg0", "10.100.0.0/24", 51820).unwrap();
        let kp1 = manager.generate_keypair().await.unwrap();
        let kp2 = manager.generate_keypair().await.unwrap();

        // Two keypairs should be different
        assert_ne!(kp1.private_key, kp2.private_key);
        assert_ne!(kp1.public_key, kp2.public_key);
    }

    #[tokio::test]
    async fn test_check_available_always_succeeds() {
        let manager = WireGuardManager::new("wg0", "10.100.0.0/24", 51820).unwrap();
        // Embedded WireGuard is always available
        assert!(manager.check_available().await.is_ok());
    }
}
