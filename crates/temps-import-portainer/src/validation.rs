// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pre-flight validation rules for Portainer imports

use temps_import_types::{
    DataImplicationSeverity, ImportPlan, ImportValidationRule, ValidationLevel, ValidationResult,
    WorkloadSnapshot,
};

#[cfg(test)]
pub(crate) mod fixtures {
    use std::collections::HashMap;
    use temps_import_types::plan::{
        DeploymentConfiguration, DeploymentStrategy, EnvironmentConfiguration,
        NetworkConfiguration, NetworkMode, PlanComplexity, PlanMetadata, ProjectConfiguration,
        ProjectType, ResourceCounts, ResourceLimits, RiskLevel, ServiceAction, ServicePlan,
    };
    use temps_import_types::{
        ImportPlan, MigrationSummary, NetworkInfo, ResourceInfo, WorkloadId, WorkloadSnapshot,
        WorkloadStatus, WorkloadType,
    };

    pub fn snapshot(image: Option<&str>, compose_service: Option<&str>) -> WorkloadSnapshot {
        let source_metadata = match compose_service {
            Some(s) => serde_json::json!({ "compose_service": s }),
            None => serde_json::json!({}),
        };
        WorkloadSnapshot {
            id: WorkloadId::new("shop"),
            name: Some("shop".to_string()),
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
            source_metadata,
        }
    }

    /// A minimal, otherwise-empty plan with the given services — everything
    /// else is a dummy value the validation rules under test don't read.
    pub fn plan_with(services: Vec<ServicePlan>) -> ImportPlan {
        ImportPlan {
            version: "1".to_string(),
            source: "portainer".to_string(),
            source_id: "shop".to_string(),
            project: ProjectConfiguration {
                name: "shop".to_string(),
                slug: "shop".to_string(),
                project_type: ProjectType::Docker,
                is_web_app: true,
            },
            environment: EnvironmentConfiguration {
                name: "production".to_string(),
                subdomain: "shop".to_string(),
                resources: ResourceLimits {
                    cpu_limit: None,
                    memory_limit: None,
                    cpu_request: None,
                    memory_request: None,
                },
            },
            deployment: DeploymentConfiguration {
                image: "nginx:latest".to_string(),
                build: None,
                strategy: DeploymentStrategy::Replace,
                env_vars: vec![],
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
            services,
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

    pub fn service(name: &str, unreachable: bool) -> ServicePlan {
        ServicePlan {
            name: name.to_string(),
            service_type: "postgres".to_string(),
            version: None,
            parameters: HashMap::new(),
            env_var_mappings: HashMap::new(),
            action: ServiceAction::Create,
            action_description: String::new(),
            data_implications: if unreachable {
                vec![temps_import_types::DataImplication {
                    severity: temps_import_types::DataImplicationSeverity::DataNotMigrated,
                    message: "not reachable".to_string(),
                    recommended_action: None,
                }]
            } else {
                vec![]
            },
        }
    }
}

pub struct PortainerValidationRules;

impl PortainerValidationRules {
    pub fn all_rules() -> Vec<Box<dyn ImportValidationRule>> {
        vec![
            Box::new(SourcePresentRule),
            Box::new(DatabaseReachabilityRule),
        ]
    }
}

/// The workload must have either a registry image or a git repository —
/// otherwise temps has nothing to deploy.
struct SourcePresentRule;

impl ImportValidationRule for SourcePresentRule {
    fn rule_id(&self) -> &str {
        "portainer-source-present"
    }
    fn rule_name(&self) -> &str {
        "Deployable source present"
    }
    fn level(&self) -> ValidationLevel {
        ValidationLevel::Critical
    }
    fn validate(&self, snapshot: &WorkloadSnapshot, _plan: &ImportPlan) -> ValidationResult {
        let has_image = snapshot.image.as_deref().is_some_and(|i| !i.is_empty());
        let has_git = snapshot
            .source_metadata
            .get("compose_service")
            .and_then(|v| v.as_str())
            .is_some_and(|r| !r.is_empty());
        let passed = has_image || has_git;
        ValidationResult {
            rule_id: self.rule_id().to_string(),
            rule_name: self.rule_name().to_string(),
            level: self.level(),
            passed,
            message: if has_image {
                format!(
                    "Application deploys image '{}'",
                    snapshot.image.as_deref().unwrap_or_default()
                )
            } else if has_git {
                "Application builds from a git repository".to_string()
            } else {
                format!(
                    "Application '{}' has neither a container image nor a git repository — nothing to deploy",
                    snapshot.id
                )
            },
            remediation: (!passed)
                .then(|| "Check the application's source configuration in Portainer".to_string()),
            affected_resources: vec![snapshot.id.to_string()],
        }
    }
}

/// Databases that are not publicly reachable can't have their data copied
/// from outside the Portainer server.
struct DatabaseReachabilityRule;

impl ImportValidationRule for DatabaseReachabilityRule {
    fn rule_id(&self) -> &str {
        "portainer-database-reachability"
    }
    fn rule_name(&self) -> &str {
        "Database data reachability"
    }
    fn level(&self) -> ValidationLevel {
        ValidationLevel::Warning
    }
    fn validate(&self, _snapshot: &WorkloadSnapshot, plan: &ImportPlan) -> ValidationResult {
        let unreachable: Vec<&str> = plan
            .services
            .iter()
            .filter(|s| {
                s.data_implications
                    .iter()
                    .any(|i| i.severity == DataImplicationSeverity::DataNotMigrated)
            })
            .map(|s| s.name.as_str())
            .collect();
        let passed = unreachable.is_empty();
        ValidationResult {
            rule_id: self.rule_id().to_string(),
            rule_name: self.rule_name().to_string(),
            level: self.level(),
            passed,
            message: if passed {
                "All databases are reachable for dump/restore (or there are none)".to_string()
            } else {
                format!(
                    "{} database(s) are not publicly reachable: {} — their data needs a manual dump on the Portainer server",
                    unreachable.len(),
                    unreachable.join(", ")
                )
            },
            remediation: (!passed).then(|| {
                "Publish the database ports in Portainer before migrating, or plan a docker-exec dump on the source server".to_string()
            }),
            affected_resources: unreachable.iter().map(|s| s.to_string()).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    #[test]
    fn source_present_rule_passes_for_an_image() {
        let result =
            SourcePresentRule.validate(&snapshot(Some("nginx:latest"), None), &plan_with(vec![]));
        assert!(result.passed);
    }

    #[test]
    fn source_present_rule_passes_for_a_compose_service() {
        let result = SourcePresentRule.validate(&snapshot(None, Some("web")), &plan_with(vec![]));
        assert!(result.passed);
    }

    #[test]
    fn source_present_rule_fails_with_neither_image_nor_compose_service() {
        let result = SourcePresentRule.validate(&snapshot(None, None), &plan_with(vec![]));
        assert!(!result.passed);
        assert!(result.remediation.is_some());
    }

    #[test]
    fn database_reachability_rule_passes_when_all_services_reachable() {
        let plan = plan_with(vec![service("db", false)]);
        let result = DatabaseReachabilityRule.validate(&snapshot(None, None), &plan);
        assert!(result.passed);
    }

    #[test]
    fn database_reachability_rule_fails_and_names_unreachable_services() {
        let plan = plan_with(vec![service("db", true), service("cache", false)]);
        let result = DatabaseReachabilityRule.validate(&snapshot(None, None), &plan);
        assert!(!result.passed);
        assert!(result.message.contains("db"));
        assert!(!result.message.contains("cache"));
        assert_eq!(result.affected_resources, vec!["db".to_string()]);
    }
}
