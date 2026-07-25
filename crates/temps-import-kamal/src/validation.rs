//! Pre-flight validation rules for Kamal imports

use temps_import_types::{
    ImportPlan, ImportValidationRule, ValidationLevel, ValidationResult, WorkloadSnapshot,
};

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
