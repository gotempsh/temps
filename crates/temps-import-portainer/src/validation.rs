//! Pre-flight validation rules for Portainer imports

use temps_import_types::{
    DataImplicationSeverity, ImportPlan, ImportValidationRule, ValidationLevel, ValidationResult,
    WorkloadSnapshot,
};

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
