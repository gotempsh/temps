//! OTel collector sidecar injection for deployed containers.
//!
//! Automatically injects an OTel collector sidecar into every deployed stack.
//! Users opt out, not in. The collector:
//! - Forwards telemetry to the Temps ingest endpoint
//! - Collects host metrics (CPU, memory, disk, network)
//! - Collects container-level metrics per service
//! - Supports passthrough for apps that self-instrument with OTel SDKs
//!
//! The sidecar runs as a separate container on the same Docker network
//! as the application container, configured via environment variables.

use std::collections::HashMap;

/// Configuration for the OTel collector sidecar.
#[derive(Debug, Clone)]
pub struct SidecarConfig {
    /// Docker image for the OTel collector.
    pub collector_image: String,
    /// Temps ingest endpoint URL.
    pub ingest_endpoint: String,
    /// Whether sidecar injection is enabled globally.
    pub enabled: bool,
    /// Batch processor timeout in seconds.
    pub batch_timeout_secs: u32,
    /// Batch processor max queue size.
    pub batch_max_queue_size: u32,
}

impl Default for SidecarConfig {
    fn default() -> Self {
        Self {
            collector_image: "otel/opentelemetry-collector-contrib:0.96.0".to_string(),
            ingest_endpoint: "http://host.docker.internal:3000/api/otel".to_string(),
            enabled: true,
            batch_timeout_secs: 5,
            batch_max_queue_size: 1000,
        }
    }
}

/// Generates the OTel collector configuration YAML for a specific project.
pub fn generate_collector_config(
    api_key: &str,
    ingest_endpoint: &str,
    config: &SidecarConfig,
) -> String {
    format!(
        r#"receivers:
  otlp:
    protocols:
      grpc:
        endpoint: 0.0.0.0:4317
      http:
        endpoint: 0.0.0.0:4318
  hostmetrics:
    collection_interval: 15s
    scrapers:
      cpu: {{}}
      memory: {{}}
      disk: {{}}
      network: {{}}
      load: {{}}
      filesystem: {{}}
  docker_stats:
    collection_interval: 15s
    endpoint: unix:///var/run/docker.sock

processors:
  batch:
    timeout: {batch_timeout}s
    send_batch_max_size: {batch_max_queue}
  filter/noise:
    metrics:
      exclude:
        match_type: regexp
        metric_names:
          - "system\\.cpu\\.time"
          - "system\\.filesystem\\.inodes.*"
          - "system\\.paging.*"
  metricstransform:
    transforms:
      - include: ".*"
        match_type: regexp
        action: update

exporters:
  otlphttp:
    endpoint: {endpoint}
    headers:
      authorization: "Bearer {api_key}"
    compression: zstd

extensions:
  health_check:
    endpoint: 0.0.0.0:13133

service:
  extensions: [health_check]
  pipelines:
    metrics:
      receivers: [otlp, hostmetrics, docker_stats]
      processors: [filter/noise, metricstransform, batch]
      exporters: [otlphttp]
    traces:
      receivers: [otlp]
      processors: [batch]
      exporters: [otlphttp]
    logs:
      receivers: [otlp]
      processors: [batch]
      exporters: [otlphttp]
"#,
        batch_timeout = config.batch_timeout_secs,
        batch_max_queue = config.batch_max_queue_size,
        endpoint = ingest_endpoint,
        api_key = api_key,
    )
}

/// Compute environment variables to inject into the application container
/// so it can send telemetry to the sidecar collector.
pub fn app_env_vars(sidecar_container_name: &str) -> HashMap<String, String> {
    let mut vars = HashMap::new();

    // Standard OTel SDK environment variables
    vars.insert(
        "OTEL_EXPORTER_OTLP_ENDPOINT".to_string(),
        format!("http://{}:4317", sidecar_container_name),
    );
    vars.insert(
        "OTEL_EXPORTER_OTLP_PROTOCOL".to_string(),
        "grpc".to_string(),
    );
    vars.insert("OTEL_TRACES_EXPORTER".to_string(), "otlp".to_string());
    vars.insert("OTEL_METRICS_EXPORTER".to_string(), "otlp".to_string());
    vars.insert("OTEL_LOGS_EXPORTER".to_string(), "otlp".to_string());

    vars
}

/// Generate the sidecar container name for a given deployment.
pub fn sidecar_container_name(project_name: &str, environment_name: &str) -> String {
    format!("temps-otel-{}-{}", project_name, environment_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_collector_config() {
        let config = SidecarConfig::default();
        let yaml =
            generate_collector_config("tk_test123", "http://localhost:3000/api/otel", &config);

        assert!(yaml.contains("otlp:"));
        assert!(yaml.contains("hostmetrics:"));
        assert!(yaml.contains("docker_stats:"));
        assert!(yaml.contains("Bearer tk_test123"));
        assert!(yaml.contains("http://localhost:3000/api/otel"));
        assert!(yaml.contains("zstd"));
        assert!(yaml.contains("filter/noise"));
    }

    #[test]
    fn test_app_env_vars() {
        let vars = app_env_vars("temps-otel-myapp-prod");
        assert_eq!(
            vars.get("OTEL_EXPORTER_OTLP_ENDPOINT").unwrap(),
            "http://temps-otel-myapp-prod:4317"
        );
        assert_eq!(vars.get("OTEL_EXPORTER_OTLP_PROTOCOL").unwrap(), "grpc");
    }

    #[test]
    fn test_sidecar_container_name() {
        assert_eq!(
            sidecar_container_name("myapp", "production"),
            "temps-otel-myapp-production"
        );
    }
}
