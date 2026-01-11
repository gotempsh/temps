//! Upstream configuration for environment routing
//!
//! This module defines type-safe structures for managing backend upstreams
//! that handle traffic routing in the proxy layer.

use sea_orm::FromJsonQueryResult;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Upstream backend configuration
///
/// Represents a single backend server address that can handle traffic.
/// Multiple upstreams enable load balancing across replicas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Upstream {
    /// Backend address in format "host:port" (e.g., "127.0.0.1:8080")
    #[schema(example = "127.0.0.1:8080")]
    pub address: String,
}

impl Upstream {
    /// Create a new upstream with the given address
    pub fn new(address: String) -> Self {
        Self { address }
    }

    /// Validate the upstream address format
    pub fn validate(&self) -> Result<(), String> {
        if self.address.is_empty() {
            return Err("Upstream address cannot be empty".to_string());
        }

        // Basic validation for host:port format
        if !self.address.contains(':') {
            return Err(format!(
                "Upstream address '{}' must be in format 'host:port'",
                self.address
            ));
        }

        // Split and validate port
        let parts: Vec<&str> = self.address.split(':').collect();
        if parts.len() != 2 {
            return Err(format!(
                "Upstream address '{}' must contain exactly one colon",
                self.address
            ));
        }

        let port_str = parts[1];
        match port_str.parse::<u16>() {
            Ok(port) if port > 0 => Ok(()),
            _ => Err(format!(
                "Invalid port '{}' in upstream address '{}'",
                port_str, self.address
            )),
        }
    }
}

/// List of upstream backends for load balancing
///
/// This is the typed structure stored in the environments.upstreams JSONB column.
/// The proxy uses these addresses for round-robin load balancing across replicas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, FromJsonQueryResult)]
#[serde(transparent)]
pub struct UpstreamList {
    /// List of backend upstreams
    pub upstreams: Vec<Upstream>,
}

impl UpstreamList {
    /// Create an empty upstream list
    pub fn new() -> Self {
        Self {
            upstreams: Vec::new(),
        }
    }

    /// Create upstream list from a vector of upstreams
    pub fn from_upstreams(upstreams: Vec<Upstream>) -> Self {
        Self { upstreams }
    }

    /// Create upstream list from address strings
    pub fn from_addresses(addresses: Vec<String>) -> Self {
        Self {
            upstreams: addresses.into_iter().map(Upstream::new).collect(),
        }
    }

    /// Add an upstream to the list
    pub fn add(&mut self, upstream: Upstream) {
        self.upstreams.push(upstream);
    }

    /// Get all upstream addresses as strings
    pub fn addresses(&self) -> Vec<String> {
        self.upstreams.iter().map(|u| u.address.clone()).collect()
    }

    /// Check if the list is empty
    pub fn is_empty(&self) -> bool {
        self.upstreams.is_empty()
    }

    /// Get the number of upstreams
    pub fn len(&self) -> usize {
        self.upstreams.len()
    }

    /// Validate all upstreams in the list
    pub fn validate(&self) -> Result<(), String> {
        for (i, upstream) in self.upstreams.iter().enumerate() {
            upstream
                .validate()
                .map_err(|e| format!("Upstream {}: {}", i, e))?;
        }
        Ok(())
    }
}

impl Default for UpstreamList {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_upstream_validation() {
        let valid = Upstream::new("127.0.0.1:8080".to_string());
        assert!(valid.validate().is_ok());

        let valid_domain = Upstream::new("localhost:3000".to_string());
        assert!(valid_domain.validate().is_ok());

        let empty = Upstream::new("".to_string());
        assert!(empty.validate().is_err());

        let no_port = Upstream::new("127.0.0.1".to_string());
        assert!(no_port.validate().is_err());

        let invalid_port = Upstream::new("127.0.0.1:abc".to_string());
        assert!(invalid_port.validate().is_err());

        let zero_port = Upstream::new("127.0.0.1:0".to_string());
        assert!(zero_port.validate().is_err());
    }

    #[test]
    fn test_upstream_list() {
        let mut list = UpstreamList::new();
        assert!(list.is_empty());
        assert_eq!(list.len(), 0);

        list.add(Upstream::new("127.0.0.1:8080".to_string()));
        list.add(Upstream::new("127.0.0.1:8081".to_string()));

        assert!(!list.is_empty());
        assert_eq!(list.len(), 2);
        assert_eq!(
            list.addresses(),
            vec!["127.0.0.1:8080".to_string(), "127.0.0.1:8081".to_string()]
        );
    }

    #[test]
    fn test_upstream_list_from_addresses() {
        let addresses = vec!["127.0.0.1:8080".to_string(), "127.0.0.1:8081".to_string()];
        let list = UpstreamList::from_addresses(addresses.clone());

        assert_eq!(list.len(), 2);
        assert_eq!(list.addresses(), addresses);
    }

    #[test]
    fn test_upstream_list_validation() {
        let mut list = UpstreamList::new();
        list.add(Upstream::new("127.0.0.1:8080".to_string()));
        list.add(Upstream::new("localhost:3000".to_string()));
        assert!(list.validate().is_ok());

        let mut invalid_list = UpstreamList::new();
        invalid_list.add(Upstream::new("127.0.0.1:8080".to_string()));
        invalid_list.add(Upstream::new("invalid".to_string()));
        assert!(invalid_list.validate().is_err());
    }

    #[test]
    fn test_serialization() {
        let list = UpstreamList::from_addresses(vec![
            "127.0.0.1:8080".to_string(),
            "127.0.0.1:8081".to_string(),
        ]);

        let json = serde_json::to_value(&list).unwrap();
        let deserialized: UpstreamList = serde_json::from_value(json).unwrap();

        assert_eq!(list, deserialized);
    }

    #[test]
    fn test_empty_list_serialization() {
        let list = UpstreamList::new();
        let json = serde_json::to_value(&list).unwrap();

        // Should serialize as empty array
        assert_eq!(json, serde_json::json!([]));

        let deserialized: UpstreamList = serde_json::from_value(json).unwrap();
        assert!(deserialized.is_empty());
    }
}
