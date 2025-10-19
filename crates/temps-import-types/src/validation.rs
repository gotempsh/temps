//! Validation types for pre-flight checks

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Result of a validation check
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ValidationResult {
    /// Rule that was checked
    pub rule_id: String,
    /// Human-readable rule name
    pub rule_name: String,
    /// Validation level
    pub level: ValidationLevel,
    /// Whether the validation passed
    pub passed: bool,
    /// Message describing the result
    pub message: String,
    /// Suggested remediation (if failed)
    pub remediation: Option<String>,
    /// Affected resources/fields
    pub affected_resources: Vec<String>,
}

/// Validation severity level
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ValidationLevel {
    /// Informational only
    Info,
    /// Warning - may cause issues
    Warning,
    /// Error - will likely cause issues
    Error,
    /// Critical - will definitely fail
    Critical,
}

/// Complete validation report
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ValidationReport {
    /// All validation results
    pub results: Vec<ValidationResult>,
    /// Overall status
    pub overall_status: ValidationStatus,
    /// Summary statistics
    pub summary: ValidationSummary,
}

impl ValidationReport {
    /// Create a new empty validation report
    pub fn new() -> Self {
        Self {
            results: Vec::new(),
            overall_status: ValidationStatus::Passed,
            summary: ValidationSummary::default(),
        }
    }

    /// Add a validation result
    pub fn add_result(&mut self, result: ValidationResult) {
        // Update summary
        match result.level {
            ValidationLevel::Info => self.summary.info_count += 1,
            ValidationLevel::Warning => self.summary.warning_count += 1,
            ValidationLevel::Error => self.summary.error_count += 1,
            ValidationLevel::Critical => self.summary.critical_count += 1,
        }

        if !result.passed {
            self.summary.failed_count += 1;
        } else {
            self.summary.passed_count += 1;
        }

        // Update overall status
        if !result.passed && result.level >= ValidationLevel::Critical {
            self.overall_status = ValidationStatus::Failed;
        } else if !result.passed
            && result.level >= ValidationLevel::Error
            && self.overall_status != ValidationStatus::Failed
        {
            self.overall_status = ValidationStatus::FailedWithWarnings;
        } else if !result.passed
            && result.level >= ValidationLevel::Warning
            && self.overall_status == ValidationStatus::Passed
        {
            self.overall_status = ValidationStatus::PassedWithWarnings;
        }

        self.results.push(result);
    }

    /// Check if validation passed (no critical/error failures)
    pub fn can_proceed(&self) -> bool {
        matches!(
            self.overall_status,
            ValidationStatus::Passed | ValidationStatus::PassedWithWarnings
        )
    }
}

impl Default for ValidationReport {
    fn default() -> Self {
        Self::new()
    }
}

/// Overall validation status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ValidationStatus {
    /// All validations passed
    Passed,
    /// Passed with warnings
    PassedWithWarnings,
    /// Failed with some errors
    FailedWithWarnings,
    /// Failed with critical errors
    Failed,
}

/// Validation summary statistics
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct ValidationSummary {
    /// Total validations run
    pub total_count: usize,
    /// Validations that passed
    pub passed_count: usize,
    /// Validations that failed
    pub failed_count: usize,
    /// Info-level results
    pub info_count: usize,
    /// Warning-level results
    pub warning_count: usize,
    /// Error-level results
    pub error_count: usize,
    /// Critical-level results
    pub critical_count: usize,
}

/// Trait for validation rules
pub trait ImportValidationRule: Send + Sync {
    /// Unique identifier for this rule
    fn rule_id(&self) -> &str;

    /// Human-readable rule name
    fn rule_name(&self) -> &str;

    /// Validation level
    fn level(&self) -> ValidationLevel;

    /// Run the validation check
    fn validate(
        &self,
        snapshot: &crate::snapshot::WorkloadSnapshot,
        plan: &crate::plan::ImportPlan,
    ) -> ValidationResult;
}
