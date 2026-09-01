// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use once_cell::sync::Lazy;

pub const DOCKER_LABEL_PREFIX: &str = "temps.";

/// Docker network name - configurable via TEMPS_NETWORK_NAME environment variable
/// Defaults to "temps-app-network" if not set
pub static NETWORK_NAME: Lazy<String> = Lazy::new(|| {
    std::env::var("TEMPS_NETWORK_NAME").unwrap_or_else(|_| "temps-app-network".to_string())
});

/// Deployment mode - determines how services communicate
/// - "baremetal" (default): Services are accessed via localhost with exposed ports
/// - "docker": Services are accessed via container names on the Docker network (internal ports)
///
/// Set via DEPLOYMENT_MODE environment variable
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeploymentMode {
    /// Baremetal mode: Use localhost and exposed host ports for service communication
    Baremetal,
    /// Docker mode: Use container names and internal ports for service communication
    Docker,
}

impl DeploymentMode {
    /// Get the current deployment mode from environment variable
    pub fn current() -> Self {
        match std::env::var("DEPLOYMENT_MODE")
            .unwrap_or_else(|_| "baremetal".to_string())
            .to_lowercase()
            .as_str()
        {
            "docker" => DeploymentMode::Docker,
            _ => DeploymentMode::Baremetal,
        }
    }

    /// Check if running in Docker mode
    pub fn is_docker() -> bool {
        Self::current() == DeploymentMode::Docker
    }

    /// Check if running in Baremetal mode
    pub fn is_baremetal() -> bool {
        Self::current() == DeploymentMode::Baremetal
    }

    /// Get the effective host and port for accessing a container
    ///
    /// In Docker mode: Returns (container_name, container_port) for container-to-container communication
    /// In Baremetal mode: Returns ("127.0.0.1", host_port) for host-based access (IPv4 to avoid IPv6 issues)
    ///
    /// # Arguments
    /// * `container_name` - The Docker container name
    /// * `container_port` - The internal container port (e.g., 3000, 8080)
    /// * `host_port` - The exposed host port for baremetal access
    ///
    /// # Returns
    /// A tuple of (host, port) appropriate for the current deployment mode
    pub fn get_effective_host_port(
        container_name: &str,
        container_port: u16,
        host_port: u16,
    ) -> (String, u16) {
        if Self::is_docker() {
            (container_name.to_string(), container_port)
        } else {
            // Use 127.0.0.1 instead of localhost to avoid IPv6 resolution issues
            // Pingora may try ::1 first when resolving "localhost", but apps typically
            // only listen on 127.0.0.1
            ("127.0.0.1".to_string(), host_port)
        }
    }

    /// Build a URL for accessing a container based on deployment mode
    ///
    /// In Docker mode: Returns http://{container_name}:{container_port}{path}
    /// In Baremetal mode: Returns http://localhost:{host_port}{path}
    ///
    /// # Arguments
    /// * `container_name` - The Docker container name
    /// * `container_port` - The internal container port
    /// * `host_port` - The exposed host port for baremetal access
    /// * `path` - Optional path to append (should start with '/')
    pub fn build_container_url(
        container_name: &str,
        container_port: u16,
        host_port: u16,
        path: Option<&str>,
    ) -> String {
        let (host, port) = Self::get_effective_host_port(container_name, container_port, host_port);
        format!("http://{}:{}{}", host, port, path.unwrap_or(""))
    }
}

impl std::fmt::Display for DeploymentMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeploymentMode::Baremetal => write!(f, "baremetal"),
            DeploymentMode::Docker => write!(f, "docker"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::sync::Mutex;

    /// Mutex to serialize tests that mutate the DEPLOYMENT_MODE environment variable.
    /// Without this, parallel test execution causes race conditions where one test's
    /// env::set_var is visible to another test that expects a different value.
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    // SAFETY: env::set_var/remove_var are unsafe in Rust 1.93+ because they are not
    // thread-safe. We use ENV_MUTEX to guarantee exclusive access across all tests
    // that mutate environment variables, making the unsafe calls sound.

    #[test]
    fn test_deployment_mode_default_is_baremetal() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        // Clear the environment variable to test default behavior
        unsafe { env::remove_var("DEPLOYMENT_MODE") };

        // Default should be baremetal
        let mode = DeploymentMode::current();
        assert_eq!(mode, DeploymentMode::Baremetal);
        assert!(DeploymentMode::is_baremetal());
        assert!(!DeploymentMode::is_docker());
    }

    #[test]
    fn test_deployment_mode_docker() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { env::set_var("DEPLOYMENT_MODE", "docker") };

        let mode = DeploymentMode::current();
        assert_eq!(mode, DeploymentMode::Docker);
        assert!(DeploymentMode::is_docker());
        assert!(!DeploymentMode::is_baremetal());

        unsafe { env::remove_var("DEPLOYMENT_MODE") };
    }

    #[test]
    fn test_deployment_mode_case_insensitive() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        for value in ["Docker", "DOCKER", "dOcKeR"] {
            unsafe { env::set_var("DEPLOYMENT_MODE", value) };
            assert_eq!(
                DeploymentMode::current(),
                DeploymentMode::Docker,
                "Failed for value: {}",
                value
            );
        }

        unsafe { env::remove_var("DEPLOYMENT_MODE") };
    }

    #[test]
    fn test_deployment_mode_invalid_defaults_to_baremetal() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        for value in ["invalid", "kubernetes", "swarm", ""] {
            unsafe { env::set_var("DEPLOYMENT_MODE", value) };
            assert_eq!(
                DeploymentMode::current(),
                DeploymentMode::Baremetal,
                "Failed for value: {}",
                value
            );
        }

        unsafe { env::remove_var("DEPLOYMENT_MODE") };
    }

    #[test]
    fn test_deployment_mode_display() {
        assert_eq!(format!("{}", DeploymentMode::Baremetal), "baremetal");
        assert_eq!(format!("{}", DeploymentMode::Docker), "docker");
    }

    #[test]
    fn test_deployment_mode_equality() {
        assert_eq!(DeploymentMode::Baremetal, DeploymentMode::Baremetal);
        assert_eq!(DeploymentMode::Docker, DeploymentMode::Docker);
        assert_ne!(DeploymentMode::Baremetal, DeploymentMode::Docker);
    }

    #[test]
    fn test_deployment_mode_clone() {
        let mode = DeploymentMode::Docker;
        let cloned = mode;
        assert_eq!(mode, cloned);
    }

    #[test]
    fn test_deployment_mode_debug() {
        assert_eq!(format!("{:?}", DeploymentMode::Baremetal), "Baremetal");
        assert_eq!(format!("{:?}", DeploymentMode::Docker), "Docker");
    }

    #[test]
    fn test_get_effective_host_port_baremetal() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { env::remove_var("DEPLOYMENT_MODE") };

        let (host, port) = DeploymentMode::get_effective_host_port("my-container", 3000, 49152);
        assert_eq!(host, "127.0.0.1"); // IPv4 to avoid IPv6 resolution issues
        assert_eq!(port, 49152);
    }

    #[test]
    fn test_get_effective_host_port_docker() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { env::set_var("DEPLOYMENT_MODE", "docker") };

        let (host, port) = DeploymentMode::get_effective_host_port("my-container", 3000, 49152);
        assert_eq!(host, "my-container");
        assert_eq!(port, 3000);

        unsafe { env::remove_var("DEPLOYMENT_MODE") };
    }

    #[test]
    fn test_build_container_url_baremetal() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { env::remove_var("DEPLOYMENT_MODE") };

        let url = DeploymentMode::build_container_url("my-container", 3000, 49152, Some("/health"));
        assert_eq!(url, "http://127.0.0.1:49152/health");

        let url_no_path = DeploymentMode::build_container_url("my-container", 3000, 49152, None);
        assert_eq!(url_no_path, "http://127.0.0.1:49152");
    }

    #[test]
    fn test_build_container_url_docker() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { env::set_var("DEPLOYMENT_MODE", "docker") };

        let url = DeploymentMode::build_container_url("my-container", 3000, 49152, Some("/health"));
        assert_eq!(url, "http://my-container:3000/health");

        let url_no_path = DeploymentMode::build_container_url("my-container", 3000, 49152, None);
        assert_eq!(url_no_path, "http://my-container:3000");

        unsafe { env::remove_var("DEPLOYMENT_MODE") };
    }
}
