//! Reusable structured-output schemas.
//!
//! Types here derive `serde::{Serialize, Deserialize}` + `schemars::JsonSchema`
//! so they can be used as the `T` in [`crate::complete_typed`] by any crate, and
//! serialized back out to the API/UI. Keep them small and flat — wide/deeply
//! nested schemas degrade structured-output reliability.

use serde::{Deserialize, Serialize};

/// Best-fit category for a diagnosed build/deployment failure. A constrained
/// enum (rather than a free string) so the schema pins the model to known
/// buckets and consumers can branch/colour on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FailureCategory {
    /// Bad/invalid Dockerfile instruction, base image, or build context.
    Dockerfile,
    /// Missing/incompatible/unresolved dependency or package.
    Dependency,
    /// Source compilation / type / lint error.
    Compilation,
    /// Misconfiguration: env vars, ports, build args, paths.
    Configuration,
    /// Network failure: registry, package mirror, DNS, timeout.
    Network,
    /// Killed for exceeding a memory limit (OOM).
    OutOfMemory,
    /// Permission / ownership / access denied.
    Permissions,
    /// Other resource limit: disk, CPU time, file descriptors.
    Resource,
    /// App started but crashed/failed health checks at runtime.
    Runtime,
    /// Couldn't be classified from the available evidence.
    Unknown,
}

/// A structured diagnosis of a failed build or deployment — produced by the AI
/// foundation from a log tail, consumed by the deploy UI / error tracking.
///
/// Reusable across producers (the deployer on build/deploy failure) and
/// consumers (rendering the diagnosis, attaching it to a deployment, surfacing
/// it in error tracking).
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ErrorDiagnosis {
    /// One-sentence, plain-English summary of what went wrong.
    pub summary: String,
    /// The single most likely root cause.
    pub likely_cause: String,
    /// Ordered, concrete remediation steps the developer can act on.
    pub suggested_fixes: Vec<String>,
    /// Best-fit category.
    pub category: FailureCategory,
    /// The key line(s) from the log that evidence the diagnosis, copied
    /// verbatim so the developer can locate them.
    #[serde(default)]
    pub key_log_lines: Vec<String>,
    /// Confidence in the diagnosis, 0.0–1.0.
    pub confidence: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_diagnosis_roundtrips() {
        let json = serde_json::json!({
            "summary": "Build failed: missing dependency 'libpq'.",
            "likely_cause": "The base image lacks libpq-dev required by the pg crate.",
            "suggested_fixes": ["Add `apt-get install -y libpq-dev` before the build step."],
            "category": "dependency",
            "key_log_lines": ["error: could not find system library 'libpq'"],
            "confidence": 0.82
        });
        let d: ErrorDiagnosis = serde_json::from_value(json).unwrap();
        assert_eq!(d.category, FailureCategory::Dependency);
        assert_eq!(d.suggested_fixes.len(), 1);
        assert!((d.confidence - 0.82).abs() < 1e-6);
    }

    #[test]
    fn test_schema_is_derivable() {
        // The schema must build (used as the `T` in complete_typed).
        let schema = serde_json::to_value(schemars::schema_for!(ErrorDiagnosis)).unwrap();
        assert_eq!(schema["title"], "ErrorDiagnosis");
        // The category enum is constrained in the schema.
        let props = &schema["properties"];
        assert!(props.get("category").is_some());
    }
}
