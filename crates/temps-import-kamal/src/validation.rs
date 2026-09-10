// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pre-flight validation rules for Kamal imports

use temps_import_types::{
    ImportPlan, ImportValidationRule, ValidationLevel, ValidationResult, WorkloadSnapshot,
};

#[cfg(test)]
pub(crate) mod fixtures {
    use std::collections::HashMap;
    use temps_import_types::plan::{
        DeploymentConfiguration, DeploymentStrategy, EnvironmentConfiguration, EnvironmentVariable,
        NetworkConfiguration, NetworkMode, PlanComplexity, PlanMetadata, ProjectConfiguration,
        ProjectType, ResourceCounts, ResourceLimits, RiskLevel,
    };
    use temps_import_types::{
        ImportPlan, MigrationSummary, NetworkInfo, ResourceInfo, WorkloadId, WorkloadSnapshot,
        WorkloadStatus, WorkloadType,
    };

    pub fn snapshot(image: Option<&str>, host_count: usize) -> WorkloadSnapshot {
        let hosts: Vec<String> = (0..host_count).map(|i| format!("host-{i}")).collect();
        WorkloadSnapshot {
            id: WorkloadId::new("lab-shop"),
            name: Some("lab-shop".to_string()),
            workload_type: WorkloadType::Container,
            status: WorkloadStatus::Running,
            image: image.map(|i| i.to_string()),
            command: None,
            entrypoint: None,
            working_dir: None,
            env: HashMap::new(),
            ports: HashMap::new(),
            volumes: vec![],
            network: NetworkInfo {
                mode: NetworkMode::Bridge,
                networks: vec![],
                hostname: None,
                domain_name: None,
            },
            resources: ResourceInfo::default(),
            labels: HashMap::new(),
            health_check: None,
            restart_policy: None,
            created_at: chrono::Utc::now(),
            source_metadata: serde_json::json!({ "hosts": hosts }),
        }
    }

    /// A minimal, otherwise-empty plan carrying the given env vars —
    /// everything else is a dummy value the validation rules under test
    /// don't read.
    pub fn plan_with_env_vars(env_vars: Vec<EnvironmentVariable>) -> ImportPlan {
        ImportPlan {
            version: "1".to_string(),
            source: "kamal".to_string(),
            source_id: "lab-shop".to_string(),
            project: ProjectConfiguration {
                name: "lab-shop".to_string(),
                slug: "lab-shop".to_string(),
                project_type: ProjectType::Docker,
                is_web_app: true,
            },
            environment: EnvironmentConfiguration {
                name: "production".to_string(),
                subdomain: "lab-shop".to_string(),
                resources: ResourceLimits {
                    cpu_limit: None,
                    memory_limit: None,
                    cpu_request: None,
                    memory_request: None,
                },
            },
            deployment: DeploymentConfiguration {
                image: "traefik/whoami:latest".to_string(),
                build: None,
                strategy: DeploymentStrategy::Replace,
                env_vars,
                ports: vec![],
                volumes: vec![],
                network: NetworkConfiguration {
                    mode: NetworkMode::Bridge,
                    hostname: None,
                    dns_servers: vec![],
                },
                resources: ResourceLimits {
                    cpu_limit: None,
                    memory_limit: None,
                    cpu_request: None,
                    memory_request: None,
                },
                command: None,
                entrypoint: None,
                working_dir: None,
                health_check: None,
                git: None,
            },
            services: vec![],
            domains: vec![],
            additional_deployments: vec![],
            steps: vec![],
            summary: MigrationSummary {
                headline: "test".to_string(),
                overall_risk: RiskLevel::Low,
                resource_counts: ResourceCounts::default(),
                critical_warnings: vec![],
                manual_actions_required: vec![],
                unsupported_features: vec![],
            },
            metadata: PlanMetadata {
                generated_at: chrono::Utc::now(),
                generator_version: "test".to_string(),
                complexity: PlanComplexity::Low,
                warnings: vec![],
            },
            cost_analysis: None,
        }
    }

    pub fn env_var(key: &str, is_secret: bool) -> EnvironmentVariable {
        EnvironmentVariable {
            key: key.to_string(),
            value: if is_secret {
                String::new()
            } else {
                "value".to_string()
            },
            is_secret,
            source_description: None,
        }
    }
}

pub struct KamalValidationRules;

impl KamalValidationRules {
    pub fn all_rules() -> Vec<Box<dyn ImportValidationRule>> {
        vec![
            Box::new(ImagePresentRule),
            Box::new(SecretPlaceholderRule),
            Box::new(MultiHostRule),
        ]
    }
}

/// The config must name an image — Kamal cannot deploy without one.
struct ImagePresentRule;

impl ImportValidationRule for ImagePresentRule {
    fn rule_id(&self) -> &str {
        "kamal-image-present"
    }
    fn rule_name(&self) -> &str {
        "Image reference present"
    }
    fn level(&self) -> ValidationLevel {
        ValidationLevel::Critical
    }
    fn validate(&self, snapshot: &WorkloadSnapshot, _plan: &ImportPlan) -> ValidationResult {
        let passed = snapshot.image.as_deref().is_some_and(|i| !i.is_empty());
        ValidationResult {
            rule_id: self.rule_id().to_string(),
            rule_name: self.rule_name().to_string(),
            level: self.level(),
            passed,
            message: if passed {
                format!(
                    "Deploys image '{}'",
                    snapshot.image.as_deref().unwrap_or_default()
                )
            } else {
                "deploy.yml has no 'image' — nothing to deploy".to_string()
            },
            remediation: (!passed).then(|| "Add the 'image:' key to config/deploy.yml".to_string()),
            affected_resources: vec![snapshot.id.to_string()],
        }
    }
}

/// Secrets arrive as empty placeholders — the app will not boot until the
/// operator fills them in, so surface it before execution rather than after.
struct SecretPlaceholderRule;

impl ImportValidationRule for SecretPlaceholderRule {
    fn rule_id(&self) -> &str {
        "kamal-secret-placeholders"
    }
    fn rule_name(&self) -> &str {
        "Secret values must be supplied manually"
    }
    fn level(&self) -> ValidationLevel {
        ValidationLevel::Warning
    }
    fn validate(&self, _snapshot: &WorkloadSnapshot, plan: &ImportPlan) -> ValidationResult {
        let secrets: Vec<&str> = plan
            .deployment
            .env_vars
            .iter()
            .filter(|v| v.is_secret)
            .map(|v| v.key.as_str())
            .collect();
        let passed = secrets.is_empty();
        ValidationResult {
            rule_id: self.rule_id().to_string(),
            rule_name: self.rule_name().to_string(),
            level: self.level(),
            passed,
            message: if passed {
                "No secret references in deploy.yml".to_string()
            } else {
                format!(
                    "{} secret(s) are referenced by name only and import empty: {}",
                    secrets.len(),
                    secrets.join(", ")
                )
            },
            remediation: (!passed).then(|| {
                "Kamal resolves these from .kamal/secrets at deploy time. Copy each value into the temps environment before deploying.".to_string()
            }),
            affected_resources: secrets.iter().map(|s| s.to_string()).collect(),
        }
    }
}

/// Kamal spreads a role across many hosts; temps runs one container per
/// environment, so multi-host roles need a conscious decision.
struct MultiHostRule;

impl ImportValidationRule for MultiHostRule {
    fn rule_id(&self) -> &str {
        "kamal-multi-host"
    }
    fn rule_name(&self) -> &str {
        "Host count"
    }
    fn level(&self) -> ValidationLevel {
        ValidationLevel::Warning
    }
    fn validate(&self, snapshot: &WorkloadSnapshot, _plan: &ImportPlan) -> ValidationResult {
        let hosts = snapshot
            .source_metadata
            .get("hosts")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        let passed = hosts <= 1;
        ValidationResult {
            rule_id: self.rule_id().to_string(),
            rule_name: self.rule_name().to_string(),
            level: self.level(),
            passed,
            message: if passed {
                "Single-host role".to_string()
            } else {
                format!(
                    "Role runs on {} hosts — temps deploys one container per environment",
                    hosts
                )
            },
            remediation: (!passed).then(|| {
                "One container usually suffices on temps; for horizontal scale, use temps worker nodes".to_string()
            }),
            affected_resources: vec![snapshot.id.to_string()],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    #[test]
    fn image_present_rule_passes_with_an_image() {
        let result = ImagePresentRule.validate(
            &snapshot(Some("traefik/whoami"), 0),
            &plan_with_env_vars(vec![]),
        );
        assert!(result.passed);
    }

    #[test]
    fn image_present_rule_fails_without_an_image() {
        let result = ImagePresentRule.validate(&snapshot(None, 0), &plan_with_env_vars(vec![]));
        assert!(!result.passed);
        assert!(result.remediation.is_some());
    }

    #[test]
    fn secret_placeholder_rule_passes_with_no_secrets() {
        let plan = plan_with_env_vars(vec![env_var("PLAN", false)]);
        let result = SecretPlaceholderRule.validate(&snapshot(None, 0), &plan);
        assert!(result.passed);
    }

    #[test]
    fn secret_placeholder_rule_warns_and_names_secret_keys() {
        let plan = plan_with_env_vars(vec![
            env_var("PLAN", false),
            env_var("DATABASE_PASSWORD", true),
        ]);
        let result = SecretPlaceholderRule.validate(&snapshot(None, 0), &plan);
        assert!(!result.passed);
        assert!(result.message.contains("DATABASE_PASSWORD"));
        assert!(!result.message.contains("PLAN"));
        assert_eq!(
            result.affected_resources,
            vec!["DATABASE_PASSWORD".to_string()]
        );
    }

    #[test]
    fn multi_host_rule_passes_for_a_single_host() {
        let result = MultiHostRule.validate(&snapshot(None, 1), &plan_with_env_vars(vec![]));
        assert!(result.passed);
    }

    #[test]
    fn multi_host_rule_passes_for_zero_hosts() {
        let result = MultiHostRule.validate(&snapshot(None, 0), &plan_with_env_vars(vec![]));
        assert!(result.passed);
    }

    #[test]
    fn multi_host_rule_warns_for_multiple_hosts() {
        let result = MultiHostRule.validate(&snapshot(None, 3), &plan_with_env_vars(vec![]));
        assert!(!result.passed);
        assert!(result.message.contains('3'));
    }
}
