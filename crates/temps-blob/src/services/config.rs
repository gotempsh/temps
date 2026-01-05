//! Blob Service configuration types

use rand::Rng;
use serde::{Deserialize, Serialize};

/// Default MinIO Docker image (use dated release for reproducibility)
pub const DEFAULT_MINIO_IMAGE: &str = "minio/minio:RELEASE.2025-09-07T16-13-09Z";
/// Default container name
pub const DEFAULT_CONTAINER_NAME: &str = "temps-blob-minio";
/// Default volume name
pub const DEFAULT_VOLUME_NAME: &str = "temps-blob-minio_data";
/// Default bucket name
pub const DEFAULT_BUCKET_NAME: &str = "temps-blobs";

/// User-provided configuration for Blob service (with defaults)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BlobInputConfig {
    /// Docker image to use (e.g., "minio/minio:RELEASE.2025-09-07T16-13-09Z")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docker_image: Option<String>,

    /// Host port for MinIO API (0 = auto-assign)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_port: Option<u16>,

    /// Host port for MinIO Console (0 = auto-assign)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub console_port: Option<u16>,

    /// Root user (access key)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_user: Option<String>,

    /// Root password (secret key) - will be encrypted in DB
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_password: Option<String>,
}

/// Internal configuration for Blob service (with generated/resolved values)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobConfig {
    /// Docker image name with tag
    pub docker_image: String,

    /// Host port for MinIO API (0 = auto-assign on creation)
    pub api_port: u16,

    /// Host port for MinIO Console (0 = auto-assign on creation)
    pub console_port: u16,

    /// Root user (access key)
    pub root_user: String,

    /// Root password (secret key)
    pub root_password: String,

    /// Container name
    pub container_name: String,

    /// Volume name for persistent data
    pub volume_name: String,

    /// Default bucket name
    pub bucket_name: String,
}

impl Default for BlobConfig {
    fn default() -> Self {
        Self {
            docker_image: DEFAULT_MINIO_IMAGE.to_string(),
            api_port: 0,     // Auto-assign
            console_port: 0, // Auto-assign
            root_user: generate_access_key(),
            root_password: generate_secret_key(),
            container_name: DEFAULT_CONTAINER_NAME.to_string(),
            volume_name: DEFAULT_VOLUME_NAME.to_string(),
            bucket_name: DEFAULT_BUCKET_NAME.to_string(),
        }
    }
}

impl From<BlobInputConfig> for BlobConfig {
    fn from(input: BlobInputConfig) -> Self {
        Self {
            docker_image: input
                .docker_image
                .unwrap_or_else(|| DEFAULT_MINIO_IMAGE.to_string()),
            api_port: input.api_port.unwrap_or(0),
            console_port: input.console_port.unwrap_or(0),
            root_user: input.root_user.unwrap_or_else(generate_access_key),
            root_password: input.root_password.unwrap_or_else(generate_secret_key),
            container_name: DEFAULT_CONTAINER_NAME.to_string(),
            volume_name: DEFAULT_VOLUME_NAME.to_string(),
            bucket_name: DEFAULT_BUCKET_NAME.to_string(),
        }
    }
}

impl BlobConfig {
    /// Extract the image name (without tag)
    pub fn image_name(&self) -> &str {
        self.docker_image
            .split(':')
            .next()
            .unwrap_or(&self.docker_image)
    }

    /// Extract the image tag
    pub fn image_tag(&self) -> &str {
        self.docker_image.split(':').nth(1).unwrap_or("latest")
    }

    /// Get the version from the image tag
    /// MinIO uses release dates: "RELEASE.2024-11-07T00-52-20Z" -> "2024-11-07"
    pub fn version(&self) -> String {
        let tag = self.image_tag();
        if tag.starts_with("RELEASE.") {
            // Extract date: RELEASE.2024-11-07T00-52-20Z -> 2024-11-07
            tag.strip_prefix("RELEASE.")
                .and_then(|s| s.split('T').next())
                .unwrap_or(tag)
                .to_string()
        } else {
            tag.to_string()
        }
    }
}

/// Generate a random access key (16 alphanumeric characters)
fn generate_access_key() -> String {
    let mut rng = rand::thread_rng();
    let charset: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    (0..16)
        .map(|_| charset[rng.gen_range(0..charset.len())] as char)
        .collect()
}

/// Generate a random secret key (32 alphanumeric characters)
fn generate_secret_key() -> String {
    let mut rng = rand::thread_rng();
    let charset: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    (0..32)
        .map(|_| charset[rng.gen_range(0..charset.len())] as char)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_blob_config_default() {
        let config = BlobConfig::default();
        assert_eq!(config.docker_image, DEFAULT_MINIO_IMAGE);
        assert_eq!(config.api_port, 0);
        assert_eq!(config.console_port, 0);
        assert_eq!(config.container_name, "temps-blob-minio");
        assert_eq!(config.bucket_name, "temps-blobs");
        // Root user and password should be generated
        assert!(!config.root_user.is_empty());
        assert!(!config.root_password.is_empty());
    }

    #[test]
    fn test_blob_input_config_to_config() {
        let input = BlobInputConfig {
            docker_image: Some("minio/minio:RELEASE.2024-12-01T00-00-00Z".to_string()),
            api_port: Some(9002),
            console_port: Some(9003),
            root_user: Some("myuser".to_string()),
            root_password: Some("mypassword".to_string()),
        };

        let config: BlobConfig = input.into();
        assert_eq!(
            config.docker_image,
            "minio/minio:RELEASE.2024-12-01T00-00-00Z"
        );
        assert_eq!(config.api_port, 9002);
        assert_eq!(config.console_port, 9003);
        assert_eq!(config.root_user, "myuser");
        assert_eq!(config.root_password, "mypassword");
    }

    #[test]
    fn test_blob_input_config_defaults() {
        let input = BlobInputConfig::default();
        let config: BlobConfig = input.into();

        assert_eq!(config.docker_image, DEFAULT_MINIO_IMAGE);
        assert_eq!(config.api_port, 0);
        // Generated credentials should not be empty
        assert!(!config.root_user.is_empty());
        assert!(!config.root_password.is_empty());
    }

    #[test]
    fn test_image_name_and_tag() {
        let config = BlobConfig {
            docker_image: "minio/minio:RELEASE.2024-11-07T00-52-20Z".to_string(),
            ..Default::default()
        };

        assert_eq!(config.image_name(), "minio/minio");
        assert_eq!(config.image_tag(), "RELEASE.2024-11-07T00-52-20Z");
    }

    #[test]
    fn test_version_extraction() {
        let tests = vec![
            ("minio/minio:RELEASE.2025-09-07T16-13-09Z", "2025-09-07"),
            ("minio/minio:RELEASE.2024-11-07T00-52-20Z", "2024-11-07"),
            ("minio/minio:latest", "latest"),
        ];

        for (image, expected_version) in tests {
            let config = BlobConfig {
                docker_image: image.to_string(),
                ..Default::default()
            };
            assert_eq!(config.version(), expected_version, "Failed for {}", image);
        }
    }

    #[test]
    fn test_generate_access_key() {
        let key = generate_access_key();
        assert_eq!(key.len(), 16);
        assert!(key.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn test_generate_secret_key() {
        let key = generate_secret_key();
        assert_eq!(key.len(), 32);
        assert!(key.chars().all(|c| c.is_ascii_alphanumeric()));
    }
}
