//! Docker-specific validation rules

use temps_import_types::{
    plan::VolumeType, ImportPlan, ImportValidationRule, ValidationLevel, ValidationResult,
    WorkloadSnapshot,
};

/// Docker validation rules
pub struct DockerValidationRules;

impl DockerValidationRules {
    /// Get all Docker validation rules
    pub fn all_rules() -> Vec<Box<dyn ImportValidationRule>> {
        vec![
            Box::new(ImageAccessibleRule),
            Box::new(PortConflictRule),
            Box::new(VolumePathRule),
        ]
    }
}

/// Validate that the Docker image is accessible
struct ImageAccessibleRule;

impl ImportValidationRule for ImageAccessibleRule {
    fn rule_id(&self) -> &str {
        "docker.image.accessible"
    }

    fn rule_name(&self) -> &str {
        "Docker Image Accessible"
    }

    fn level(&self) -> ValidationLevel {
        ValidationLevel::Critical
    }

    fn validate(&self, snapshot: &WorkloadSnapshot, plan: &ImportPlan) -> ValidationResult {
        // Check if the Docker image is specified
        let image = &plan.deployment.image;

        if image.is_empty() {
            return ValidationResult {
                rule_id: self.rule_id().to_string(),
                rule_name: self.rule_name().to_string(),
                level: self.level(),
                passed: false,
                message: "No Docker image specified in deployment configuration".to_string(),
                remediation: Some(
                    "Specify a valid Docker image name (e.g., nginx:latest, myapp:1.0)".to_string(),
                ),
                affected_resources: vec!["deployment.image".to_string()],
            };
        }

        // Check for basic image name validity
        let image_parts: Vec<&str> = image.split(':').collect();
        if image_parts.is_empty() || image_parts[0].is_empty() {
            return ValidationResult {
                rule_id: self.rule_id().to_string(),
                rule_name: self.rule_name().to_string(),
                level: self.level(),
                passed: false,
                message: format!("Invalid Docker image format: '{}'", image),
                remediation: Some(
                    "Use format: [registry/]image[:tag] (e.g., nginx:latest, docker.io/library/nginx:1.21)"
                        .to_string(),
                ),
                affected_resources: vec!["deployment.image".to_string()],
            };
        }

        // Warn about 'latest' tag usage
        if image.ends_with(":latest") || !image.contains(':') {
            return ValidationResult {
                rule_id: self.rule_id().to_string(),
                rule_name: self.rule_name().to_string(),
                level: self.level(),
                passed: true,
                message: format!(
                    "Image '{}' uses 'latest' tag or no tag. Consider using a specific version tag for reproducible deployments",
                    image
                ),
                remediation: Some(
                    "Specify an explicit version tag (e.g., nginx:1.21.3) instead of 'latest'"
                        .to_string(),
                ),
                affected_resources: vec!["deployment.image".to_string()],
            };
        }

        // Check if original snapshot had an image
        if snapshot.image.is_none() {
            return ValidationResult {
                rule_id: self.rule_id().to_string(),
                rule_name: self.rule_name().to_string(),
                level: self.level(),
                passed: false,
                message: "Container snapshot has no image information".to_string(),
                remediation: Some("Inspect the container to determine its image".to_string()),
                affected_resources: vec!["snapshot.image".to_string()],
            };
        }

        ValidationResult {
            rule_id: self.rule_id().to_string(),
            rule_name: self.rule_name().to_string(),
            level: self.level(),
            passed: true,
            message: format!("Image '{}' is properly specified", image),
            remediation: None,
            affected_resources: vec![],
        }
    }
}

/// Validate that ports don't conflict with existing deployments
struct PortConflictRule;

impl ImportValidationRule for PortConflictRule {
    fn rule_id(&self) -> &str {
        "docker.ports.conflict"
    }

    fn rule_name(&self) -> &str {
        "Port Conflict Check"
    }

    fn level(&self) -> ValidationLevel {
        ValidationLevel::Warning
    }

    fn validate(&self, _snapshot: &WorkloadSnapshot, plan: &ImportPlan) -> ValidationResult {
        use std::collections::HashSet;

        // Check for duplicate ports within the plan
        let mut seen_ports = HashSet::new();
        let mut duplicate_ports = Vec::new();

        for port_mapping in &plan.deployment.ports {
            if !seen_ports.insert(port_mapping.container_port) {
                duplicate_ports.push(port_mapping.container_port);
            }
        }

        if !duplicate_ports.is_empty() {
            return ValidationResult {
                rule_id: self.rule_id().to_string(),
                rule_name: self.rule_name().to_string(),
                level: self.level(),
                passed: false,
                message: format!(
                    "Duplicate port mappings detected: {}",
                    duplicate_ports
                        .iter()
                        .map(|p| p.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                remediation: Some(
                    "Remove duplicate port mappings from the deployment configuration".to_string(),
                ),
                affected_resources: duplicate_ports
                    .iter()
                    .map(|p| format!("deployment.ports.{}", p))
                    .collect(),
            };
        }

        // Check for standard privileged ports (< 1024)
        let privileged_ports: Vec<u16> = plan
            .deployment
            .ports
            .iter()
            .filter(|p| p.host_port.is_some() && p.host_port.unwrap() < 1024)
            .map(|p| p.host_port.unwrap())
            .collect();

        if !privileged_ports.is_empty() {
            return ValidationResult {
                rule_id: self.rule_id().to_string(),
                rule_name: self.rule_name().to_string(),
                level: self.level(),
                passed: true,
                message: format!(
                    "Host ports in privileged range (<1024) detected: {}. May require elevated permissions",
                    privileged_ports
                        .iter()
                        .map(|p| p.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                remediation: Some(
                    "Consider using non-privileged ports (>= 1024) or ensure the runtime has necessary permissions"
                        .to_string(),
                ),
                affected_resources: privileged_ports
                    .iter()
                    .map(|p| format!("deployment.ports.{}", p))
                    .collect(),
            };
        }

        // Check if there are any ports exposed
        if plan.deployment.ports.is_empty() {
            return ValidationResult {
                rule_id: self.rule_id().to_string(),
                rule_name: self.rule_name().to_string(),
                level: self.level(),
                passed: true,
                message: "No ports exposed. Container may not be accessible externally".to_string(),
                remediation: Some(
                    "If this is a web application, ensure at least one port is exposed".to_string(),
                ),
                affected_resources: vec!["deployment.ports".to_string()],
            };
        }

        ValidationResult {
            rule_id: self.rule_id().to_string(),
            rule_name: self.rule_name().to_string(),
            level: self.level(),
            passed: true,
            message: format!(
                "{} port(s) configured without conflicts",
                plan.deployment.ports.len()
            ),
            remediation: None,
            affected_resources: vec![],
        }
    }
}

/// Validate that volume paths are valid
struct VolumePathRule;

impl ImportValidationRule for VolumePathRule {
    fn rule_id(&self) -> &str {
        "docker.volumes.path"
    }

    fn rule_name(&self) -> &str {
        "Volume Path Validation"
    }

    fn level(&self) -> ValidationLevel {
        ValidationLevel::Warning
    }

    fn validate(&self, _snapshot: &WorkloadSnapshot, plan: &ImportPlan) -> ValidationResult {
        let mut bind_mounts = Vec::new();
        let mut invalid_paths = Vec::new();
        let mut absolute_paths = Vec::new();

        for volume in &plan.deployment.volumes {
            // Check for bind mounts
            if volume.volume_type == VolumeType::Bind {
                bind_mounts.push(volume.source.clone());

                // Check if source path looks like an absolute path
                if !volume.source.starts_with('/') && !volume.source.starts_with("\\") {
                    invalid_paths.push(volume.source.clone());
                }
            }

            // Check if destination is an absolute path
            if !volume.destination.starts_with('/') {
                absolute_paths.push(volume.destination.clone());
            }

            // Check for potentially problematic destination paths
            let dangerous_paths = ["/", "/bin", "/boot", "/dev", "/etc", "/lib", "/proc", "/root", "/sbin", "/sys", "/usr"];
            if dangerous_paths.contains(&volume.destination.as_str()) {
                return ValidationResult {
                    rule_id: self.rule_id().to_string(),
                    rule_name: self.rule_name().to_string(),
                    level: self.level(),
                    passed: false,
                    message: format!(
                        "Volume mounted to critical system path: '{}'. This can break the container",
                        volume.destination
                    ),
                    remediation: Some(
                        "Mount volumes to application-specific directories (e.g., /app/data, /var/lib/myapp)"
                            .to_string(),
                    ),
                    affected_resources: vec![format!("deployment.volumes.{}", volume.destination)],
                };
            }
        }

        // Warn about bind mounts
        if !bind_mounts.is_empty() {
            return ValidationResult {
                rule_id: self.rule_id().to_string(),
                rule_name: self.rule_name().to_string(),
                level: self.level(),
                passed: true,
                message: format!(
                    "{} bind mount(s) detected. These may not be available in the target environment: {}",
                    bind_mounts.len(),
                    bind_mounts.join(", ")
                ),
                remediation: Some(
                    "Consider using named volumes instead of bind mounts for better portability. Convert bind mounts to volumes or ensure the paths exist in the target environment"
                        .to_string(),
                ),
                affected_resources: bind_mounts
                    .iter()
                    .map(|p| format!("deployment.volumes.{}", p))
                    .collect(),
            };
        }

        // Check for invalid bind mount paths
        if !invalid_paths.is_empty() {
            return ValidationResult {
                rule_id: self.rule_id().to_string(),
                rule_name: self.rule_name().to_string(),
                level: self.level(),
                passed: false,
                message: format!(
                    "Bind mount source paths must be absolute: {}",
                    invalid_paths.join(", ")
                ),
                remediation: Some(
                    "Use absolute paths for bind mount sources (e.g., /host/data not ./data)"
                        .to_string(),
                ),
                affected_resources: invalid_paths
                    .iter()
                    .map(|p| format!("deployment.volumes.{}", p))
                    .collect(),
            };
        }

        // Check for non-absolute destination paths
        if !absolute_paths.is_empty() {
            return ValidationResult {
                rule_id: self.rule_id().to_string(),
                rule_name: self.rule_name().to_string(),
                level: self.level(),
                passed: false,
                message: format!(
                    "Volume destination paths must be absolute: {}",
                    absolute_paths.join(", ")
                ),
                remediation: Some(
                    "Use absolute paths for volume destinations (e.g., /app/data not data)"
                        .to_string(),
                ),
                affected_resources: absolute_paths
                    .iter()
                    .map(|p| format!("deployment.volumes.{}", p))
                    .collect(),
            };
        }

        // If no volumes, that's okay
        if plan.deployment.volumes.is_empty() {
            return ValidationResult {
                rule_id: self.rule_id().to_string(),
                rule_name: self.rule_name().to_string(),
                level: self.level(),
                passed: true,
                message: "No volumes configured. Container uses ephemeral storage only".to_string(),
                remediation: None,
                affected_resources: vec![],
            };
        }

        ValidationResult {
            rule_id: self.rule_id().to_string(),
            rule_name: self.rule_name().to_string(),
            level: self.level(),
            passed: true,
            message: format!(
                "{} volume(s) configured with valid paths",
                plan.deployment.volumes.len()
            ),
            remediation: None,
            affected_resources: vec![],
        }
    }
}
