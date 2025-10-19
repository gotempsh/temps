use bollard::Docker;
use tracing::{info, error, debug};
use std::net::IpAddr;
use std::sync::Arc;
use parking_lot::RwLock;
use std::time::{Duration, Instant};

use crate::types::{
    PlatformInfo, PublicIpInfo, PrivateIpInfo, NetworkInterface, ServerMode
};

/// Cached network information
#[derive(Debug, Clone)]
struct CachedNetworkInfo {
    pub public_ip: Option<String>,
    pub private_ip: Option<String>,
    pub last_updated: Instant,
}

#[derive(Clone)]
pub struct PlatformInfoService {
    docker: Arc<Docker>,
    network_cache: Arc<RwLock<Option<CachedNetworkInfo>>>,
    cache_duration: Duration,
}

impl PlatformInfoService {
    pub fn new(docker: Arc<Docker>) -> Self {
        Self {
            docker,
            network_cache: Arc::new(RwLock::new(None)),
            cache_duration: Duration::from_secs(600), // Cache for 10 minutes
        }
    }

    pub async fn get_platform_info(&self) -> anyhow::Result<PlatformInfo> {
        info!("Getting platform info from Docker");

        let info = self.docker.info().await?;

        // Get OS type and architecture from Docker info
        let os_type = info.os_type.unwrap_or_else(|| "unknown".to_string());
        let architecture = info.architecture.unwrap_or_else(|| "unknown".to_string());

        // Construct platform string in the format "os/arch"
        let platform = format!("{}/{}", os_type.to_lowercase(), architecture.to_lowercase());

        Ok(PlatformInfo {
            os_type,
            architecture,
            platforms: vec![platform],
        })
    }

    pub async fn get_public_ip(&self) -> PublicIpInfo {
        // Check cache first
        if let Some(cached) = self.get_cached_network_info() {
            if let Some(public_ip) = cached.public_ip {
                return PublicIpInfo {
                    ip: Some(public_ip),
                    source: Some("cache".to_string()),
                    error: None,
                };
            }
        }

        info!("Fetching public IP address from external service");

        // Try multiple services to get the public IP
        let services = vec![
            "https://api.ipify.org?format=json",
            "https://ipinfo.io/json",
            "https://api.myip.com",
        ];

        for service in services {
            match reqwest::get(service).await {
                Ok(response) => {
                    if let Ok(json) = response.json::<serde_json::Value>().await {
                        // Extract IP from different response formats
                        let ip = if let Some(ip) = json.get("ip") {
                            ip.as_str().map(String::from)
                        } else if let Some(origin) = json.get("origin") {
                            origin.as_str().map(String::from)
                        } else {
                            None
                        };

                        if let Some(ip_address) = ip {
                            // Update cache with the new IP
                            self.update_cache_public_ip(Some(ip_address.clone()));

                            return PublicIpInfo {
                                ip: Some(ip_address),
                                source: Some(service.to_string()),
                                error: None,
                            };
                        }
                    }
                }
                Err(e) => {
                    error!("Failed to get IP from {}: {}", service, e);
                    continue;
                }
            }
        }

        // If all services fail, return an error
        PublicIpInfo {
            ip: None,
            source: None,
            error: Some("Unable to determine public IP address".to_string()),
        }
    }

    pub async fn get_private_ip(&self) -> anyhow::Result<PrivateIpInfo> {
        // Check cache first for primary IP
        if let Some(cached) = self.get_cached_network_info() {
            if cached.private_ip.is_some() {
                debug!("Using cached private IP");
                // Still need to return full PrivateIpInfo, so fetch interfaces
                // but use cached primary IP
            }
        }

        info!("Getting private IP address");

        // Get all network interfaces
        let interfaces = get_if_addrs::get_if_addrs()?;

        // Collect all non-loopback IPv4 and IPv6 addresses
        let mut ipv4_addresses = Vec::new();
        let mut ipv6_addresses = Vec::new();

        for interface in interfaces {
            // Skip loopback interfaces
            if interface.is_loopback() {
                continue;
            }

            match interface.ip() {
                IpAddr::V4(addr) => {
                    // Check if it's a private IP address (RFC 1918)
                    let octets = addr.octets();
                    let is_private =
                        (octets[0] == 10) || // 10.0.0.0/8
                        (octets[0] == 172 && (octets[1] >= 16 && octets[1] <= 31)) || // 172.16.0.0/12
                        (octets[0] == 192 && octets[1] == 168); // 192.168.0.0/16

                    ipv4_addresses.push(NetworkInterface {
                        address: addr.to_string(),
                        interface: interface.name,
                        is_private: Some(is_private),
                        is_link_local: None,
                        is_unique_local: None,
                    });
                }
                IpAddr::V6(addr) => {
                    // Check if it's a link-local or unique local address
                    let is_link_local = addr.segments()[0] == 0xfe80;
                    let is_unique_local = (addr.segments()[0] & 0xfe00) == 0xfc00;

                    ipv6_addresses.push(NetworkInterface {
                        address: addr.to_string(),
                        interface: interface.name,
                        is_private: None,
                        is_link_local: Some(is_link_local),
                        is_unique_local: Some(is_unique_local),
                    });
                }
            }
        }

        // Find the most likely private IP (prefer 192.168.x.x, then 10.x.x.x, then 172.16-31.x.x)
        let primary_private_ip = ipv4_addresses.iter()
            .filter(|iface| iface.is_private.unwrap_or(false))
            .min_by_key(|iface| {
                let ip = &iface.address;
                if ip.starts_with("192.168.") { 0 }
                else if ip.starts_with("10.") { 1 }
                else { 2 }
            })
            .map(|iface| iface.address.clone());

        // Cache the primary IP
        if let Some(ref primary_ip) = primary_private_ip {
            self.update_cache_private_ip(Some(primary_ip.clone()));
        }

        Ok(PrivateIpInfo {
            primary_ip: primary_private_ip,
            ipv4_addresses,
            ipv6_addresses,
        })
    }

    /// Get the server mode from request headers
    pub async fn get_server_mode_from_headers(&self, headers: &axum::http::HeaderMap) -> ServerMode {
        // Check for Cloudflare headers first
        let has_cf_ray = headers.get("cf-ray").is_some();
        let has_cf_connecting_ip = headers.get("cf-connecting-ip").is_some();

        if has_cf_ray || has_cf_connecting_ip {
            return ServerMode::CloudflareTunnel;
        }

        // Get the Host header to check what address was used to access the API
        let host_header = headers.get("host")
            .and_then(|h| h.to_str().ok())
            .map(|h| h.to_string());

        // Check if accessing via localhost or local IP
        let is_local_access = host_header.as_ref()
            .map(|host| {
                let host_without_port = host.split(':').next().unwrap_or(host);

                host_without_port == "localhost" ||
                host_without_port == "127.0.0.1" ||
                host_without_port == "::1" ||
                is_private_ip(host_without_port)
            })
            .unwrap_or(false);

        if is_local_access {
            return ServerMode::Local;
        }

        // For non-local, non-Cloudflare access, determine if NAT or Direct
        let cached_info = self.get_cached_network_info();

        if let Some(info) = cached_info {
            self.determine_mode_from_ips(&info.public_ip, &info.private_ip)
        } else {
            // Default to Direct if we can't determine
            ServerMode::Direct
        }
    }

    /// Get cached network information or None if expired
    fn get_cached_network_info(&self) -> Option<CachedNetworkInfo> {
        let cache_read = self.network_cache.read();
        if let Some(ref cached) = *cache_read {
            if cached.last_updated.elapsed() < self.cache_duration {
                debug!("Using cached network info");
                return Some(cached.clone());
            }
        }
        None
    }

    /// Update cached public IP
    fn update_cache_public_ip(&self, public_ip: Option<String>) {
        let mut cache_write = self.network_cache.write();
        if let Some(ref mut cached) = *cache_write {
            cached.public_ip = public_ip;
            cached.last_updated = Instant::now();
        } else {
            *cache_write = Some(CachedNetworkInfo {
                public_ip,
                private_ip: None,
                last_updated: Instant::now(),
            });
        }
    }

    /// Update cached private IP
    fn update_cache_private_ip(&self, private_ip: Option<String>) {
        let mut cache_write = self.network_cache.write();
        if let Some(ref mut cached) = *cache_write {
            cached.private_ip = private_ip;
            cached.last_updated = Instant::now();
        } else {
            *cache_write = Some(CachedNetworkInfo {
                public_ip: None,
                private_ip,
                last_updated: Instant::now(),
            });
        }
    }

    /// Force refresh the cached information
    pub async fn refresh_network_cache(&self) -> anyhow::Result<()> {
        info!("Refreshing network cache");

        // Clear existing cache
        {
            let mut cache_write = self.network_cache.write();
            *cache_write = None;
        }

        // Fetch new info
        let _ = self.get_public_ip().await;
        let _ = self.get_private_ip().await;

        Ok(())
    }

    /// Get just the cached public IP without fetching
    pub fn get_cached_public_ip(&self) -> Option<String> {
        self.get_cached_network_info()
            .and_then(|info| info.public_ip)
    }

    /// Get just the cached private IP without fetching
    pub fn get_cached_private_ip(&self) -> Option<String> {
        self.get_cached_network_info()
            .and_then(|info| info.private_ip)
    }

    /// Get public IP with automatic cache refresh if needed
    pub async fn get_public_ip_with_fallback(&self) -> Option<String> {
        // Try cache first
        if let Some(ip) = self.get_cached_public_ip() {
            return Some(ip);
        }

        // Cache miss or expired - fetch fresh data
        debug!("Public IP not in cache, fetching...");
        let ip_info = self.get_public_ip().await;
        ip_info.ip
    }

    /// Get private IP with automatic cache refresh if needed
    pub async fn get_private_ip_with_fallback(&self) -> Option<String> {
        // Try cache first
        if let Some(ip) = self.get_cached_private_ip() {
            return Some(ip);
        }

        // Cache miss or expired - fetch fresh data
        debug!("Private IP not in cache, fetching...");
        match self.get_private_ip().await {
            Ok(ip_info) => ip_info.primary_ip,
            Err(e) => {
                error!("Failed to get private IP: {}", e);
                None
            }
        }
    }

    /// Determine server mode from IP addresses
    fn determine_mode_from_ips(&self, public_ip: &Option<String>, private_ip: &Option<String>) -> ServerMode {
        match (public_ip, private_ip) {
            (Some(pub_ip), Some(priv_ip)) if pub_ip != priv_ip && is_private_ip(priv_ip) => {
                ServerMode::Nat
            }
            (Some(_), _) => ServerMode::Direct,
            _ => ServerMode::Local, // No public IP means local only
        }
    }
}

/// Check if an IP address is in a private range (RFC1918)
fn is_private_ip(ip: &str) -> bool {
    if let Ok(addr) = ip.parse::<std::net::IpAddr>() {
        match addr {
            std::net::IpAddr::V4(ipv4) => {
                ipv4.is_private() ||
                ipv4.is_loopback() ||
                ipv4.is_link_local()
            }
            std::net::IpAddr::V6(ipv6) => {
                ipv6.is_loopback() ||
                ipv6.is_unspecified() ||
                // Check for link-local (fe80::/10)
                (ipv6.segments()[0] & 0xffc0) == 0xfe80 ||
                // Check for unique local (fc00::/7)
                (ipv6.segments()[0] & 0xfe00) == 0xfc00
            }
        }
    } else {
        false
    }
}