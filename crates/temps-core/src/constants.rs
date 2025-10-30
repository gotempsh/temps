use once_cell::sync::Lazy;

pub const DOCKER_LABEL_PREFIX: &str = "temps.";

/// Docker network name - configurable via TEMPS_NETWORK_NAME environment variable
/// Defaults to "temps-app-network" if not set
pub static NETWORK_NAME: Lazy<String> = Lazy::new(|| {
    std::env::var("TEMPS_NETWORK_NAME").unwrap_or_else(|_| "temps-app-network".to_string())
});
