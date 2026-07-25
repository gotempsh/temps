//! Pre-flight validation rules for CapRover imports

use temps_import_types::{
    DataImplicationSeverity, ImportPlan, ImportValidationRule, ValidationLevel, ValidationResult,
    WorkloadSnapshot,
};

pub struct CaproverValidationRules;

impl CaproverValidationRules {
    pub fn all_rules() -> Vec<Box<dyn ImportValidationRule>> {
        vec![
            Box::new(SourcePresentRule),
            Box::new(DatabaseReachabilityRule),
            Box::new(GeneratedDomainRule),
        ]
    }
}

/// The workload must have either a registry image or a git repository —
/// otherwise temps has nothing to deploy.
struct SourcePresentRule;

impl ImportValidationRule for SourcePresentRule {
    fn rule_id(&self) -> &str {
        "caprover-source-present"
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
            .get("repo")
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
                .then(|| "Check the application's source configuration in CapRover".to_string()),
            affected_resources: vec![snapshot.id.to_string()],
        }
    }
}

/// Databases that are not publicly reachable can't have their data copied
/// from outside the CapRover server.
struct DatabaseReachabilityRule;

impl ImportValidationRule for DatabaseReachabilityRule {
    fn rule_id(&self) -> &str {
        "caprover-database-reachability"
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
                    "{} database(s) are not publicly reachable: {} — their data needs a manual dump on the CapRover server",
                    unreachable.len(),
                    unreachable.join(", ")
                )
            },
            remediation: (!passed).then(|| {
                "Publish host ports on these database apps in CapRover before migrating, or plan a docker-exec dump on the source server".to_string()
            }),
            affected_resources: unreachable.iter().map(|s| s.to_string()).collect(),
        }
    }
}

/// CapRover-generated wildcard-DNS domains are skipped, which is expected —
/// surface it so the user isn't surprised their sslip.io URL changes.
struct GeneratedDomainRule;

impl ImportValidationRule for GeneratedDomainRule {
    fn rule_id(&self) -> &str {
        "caprover-generated-domains"
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
        let passed = true; // informational — never blocks
        ValidationResult {
            rule_id: self.rule_id().to_string(),
            rule_name: self.rule_name().to_string(),
            level: self.level(),
            passed,
            message: if skipped.is_empty() {
                "No CapRover-generated domains to skip".to_string()
            } else {
                format!(
                    "{} CapRover-generated domain(s) will be skipped (temps assigns its own): {}",
                    skipped.len(),
                    skipped.join(", ")
                )
            },
            remediation: None,
            affected_resources: skipped.iter().map(|s| s.to_string()).collect(),
        }
    }
}
