//! Pre-flight validation rules for Dokploy imports

use temps_import_types::{
    DataImplicationSeverity, ImportPlan, ImportValidationRule, ValidationLevel, ValidationResult,
    WorkloadSnapshot,
};

pub struct DokployValidationRules;

impl DokployValidationRules {
    pub fn all_rules() -> Vec<Box<dyn ImportValidationRule>> {
        vec![
            Box::new(SourcePresentRule),
            Box::new(DatabaseReachabilityRule),
            Box::new(GeneratedDomainRule),
        ]
    }
}

/// The workload must have either a registry image or a git source.
struct SourcePresentRule;

impl ImportValidationRule for SourcePresentRule {
    fn rule_id(&self) -> &str {
        "dokploy-source-present"
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
            .get("custom_git_url")
            .and_then(|v| v.as_str())
            .is_some_and(|r| !r.is_empty())
            || snapshot
                .source_metadata
                .get("source_type")
                .and_then(|v| v.as_str())
                .is_some_and(|s| matches!(s, "github" | "gitlab" | "bitbucket" | "gitea"));
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
                    "Application '{}' has neither a container image nor a git source — nothing to deploy",
                    snapshot.id
                )
            },
            remediation: (!passed)
                .then(|| "Check the application's source configuration in Dokploy".to_string()),
            affected_resources: vec![snapshot.id.to_string()],
        }
    }
}

/// Databases without an external port can't have their data copied from
/// outside the Dokploy server.
struct DatabaseReachabilityRule;

impl ImportValidationRule for DatabaseReachabilityRule {
    fn rule_id(&self) -> &str {
        "dokploy-database-reachability"
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
                    "{} database(s) have no external port: {} — their data needs a manual dump on the Dokploy server",
                    unreachable.len(),
                    unreachable.join(", ")
                )
            },
            remediation: (!passed).then(|| {
                "Set an external port on these databases in Dokploy before migrating, or plan a docker-exec dump on the source server".to_string()
            }),
            affected_resources: unreachable.iter().map(|s| s.to_string()).collect(),
        }
    }
}

/// Generated wildcard-DNS domains are skipped — surface it for transparency.
struct GeneratedDomainRule;

impl ImportValidationRule for GeneratedDomainRule {
    fn rule_id(&self) -> &str {
        "dokploy-generated-domains"
    }
    fn rule_name(&self) -> &str {
        "Generated domains"
    }
    fn level(&self) -> ValidationLevel {
        ValidationLevel::Info
    }
    fn validate(&self, _snapshot: &WorkloadSnapshot, plan: &ImportPlan) -> ValidationResult {
        let skipped: Vec<&str> = plan
            .domains
            .iter()
            .filter(|d| d.action == temps_import_types::DomainAction::Skip)
            .map(|d| d.domain.as_str())
            .collect();
        ValidationResult {
            rule_id: self.rule_id().to_string(),
            rule_name: self.rule_name().to_string(),
            level: self.level(),
            passed: true,
            message: if skipped.is_empty() {
                "No generated domains to skip".to_string()
            } else {
                format!(
                    "{} generated domain(s) will be skipped (temps assigns its own): {}",
                    skipped.len(),
                    skipped.join(", ")
                )
            },
            remediation: None,
            affected_resources: skipped.iter().map(|s| s.to_string()).collect(),
        }
    }
}
