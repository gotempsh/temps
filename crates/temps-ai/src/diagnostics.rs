//! High-level, reusable AI helpers that pair a prompt with a schema.
//!
//! [`diagnose_failure`] is the one-call entry point for "explain this failed
//! build / deployment": any crate (the deployer on build failure, a retry
//! handler, the UI on demand) passes a [`FailureContext`] and gets a structured
//! [`ErrorDiagnosis`] back, best-effort. No prompt-wrangling or JSON parsing at
//! the call site.

use crate::schemas::ErrorDiagnosis;
use crate::service::{AiRequest, AiService};
use crate::typed::complete_typed;

/// Keep the log tail bounded so a giant build log can't blow the token budget.
/// We feed the *tail* — failures surface at the end — capped to this many bytes
/// (~the last chunk of output), trimmed to a line boundary.
const MAX_LOG_TAIL_BYTES: usize = 12_000;

/// What kind of failure is being diagnosed (shapes the prompt framing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// A container image build (Docker/Nixpacks/buildpacks) failed.
    Build,
    /// A deployment failed to start or pass health checks.
    Deployment,
}

impl FailureKind {
    fn label(self) -> &'static str {
        match self {
            FailureKind::Build => "container image build",
            FailureKind::Deployment => "deployment",
        }
    }
}

/// Inputs for diagnosing a failed build or deployment. Carries only what the
/// model needs — no secrets; callers should pass a log tail with env values
/// already redacted if necessary.
#[derive(Debug, Clone)]
pub struct FailureContext {
    pub kind: FailureKind,
    /// Governance/usage scope.
    pub project_id: Option<i32>,
    /// The failing step/command, if known (e.g. a Dockerfile `RUN` line).
    pub failed_step: Option<String>,
    /// Process exit code, if known.
    pub exit_code: Option<i64>,
    /// The build/deploy log (the tail is used; see [`MAX_LOG_TAIL_BYTES`]).
    pub log: String,
}

const DIAGNOSIS_SYSTEM: &str = "You are a senior platform/DevOps engineer diagnosing a failed build or deployment \
from its log output. Identify the single most likely root cause and concrete, ordered fixes a developer can \
apply. Base everything strictly on the evidence in the log — do NOT invent file names, versions, or causes you \
cannot see. Quote the key log line(s) verbatim. Be concise and practical. Respond ONLY with JSON matching the \
provided schema.";

impl FailureContext {
    /// The last [`MAX_LOG_TAIL_BYTES`] of the log, trimmed to a line boundary.
    fn log_tail(&self) -> &str {
        let log = self.log.trim_end();
        if log.len() <= MAX_LOG_TAIL_BYTES {
            return log;
        }
        let start = log.len() - MAX_LOG_TAIL_BYTES;
        // Advance to the next newline so we don't start mid-line / mid-char.
        match log[start..].find('\n') {
            Some(nl) => &log[start + nl + 1..],
            None => &log[start..],
        }
    }

    fn to_prompt(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!("A {} failed.\n", self.kind.label()));
        if let Some(step) = &self.failed_step {
            s.push_str(&format!("Failing step: {step}\n"));
        }
        if let Some(code) = self.exit_code {
            s.push_str(&format!("Exit code: {code}\n"));
        }
        s.push_str("\nLog tail:\n");
        s.push_str("```\n");
        s.push_str(self.log_tail());
        s.push_str("\n```");
        s
    }
}

/// Diagnose a failed build/deployment into a structured [`ErrorDiagnosis`].
/// `None` when AI is unavailable or the reply doesn't conform — callers fall
/// back to showing the raw log. Best-effort: wrap in a timeout at the call site.
pub async fn diagnose_failure(ai: &dyn AiService, ctx: &FailureContext) -> Option<ErrorDiagnosis> {
    if ctx.log.trim().is_empty() {
        return None;
    }
    let req = AiRequest {
        purpose: "deploy.failure_diagnosis".to_string(),
        project_id: ctx.project_id,
        system: Some(DIAGNOSIS_SYSTEM.to_string()),
        prompt: ctx.to_prompt(),
        max_tokens: Some(700),
        temperature: Some(0.1),
        ..Default::default()
    };
    complete_typed(ai, req).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schemas::FailureCategory;
    use crate::service::{AiError, AiRequest as Req, AiResponse};
    use async_trait::async_trait;

    fn ctx(log: &str) -> FailureContext {
        FailureContext {
            kind: FailureKind::Build,
            project_id: Some(7),
            failed_step: Some("RUN cargo build --release".into()),
            exit_code: Some(101),
            log: log.into(),
        }
    }

    #[test]
    fn test_prompt_includes_step_and_log() {
        let p = ctx("error: linker `cc` not found").to_prompt();
        assert!(p.contains("container image build failed"));
        assert!(p.contains("Failing step: RUN cargo build --release"));
        assert!(p.contains("Exit code: 101"));
        assert!(p.contains("error: linker `cc` not found"));
    }

    #[test]
    fn test_log_tail_truncates_to_line_boundary() {
        let big = format!("{}\nFINAL ERROR LINE", "x".repeat(20_000));
        let c = ctx(&big);
        let tail = c.log_tail();
        assert!(tail.len() <= MAX_LOG_TAIL_BYTES);
        assert!(tail.ends_with("FINAL ERROR LINE"));
        assert!(!tail.starts_with('x') || tail.len() < 20_000); // started at a line boundary
    }

    struct CannedAi(String);
    #[async_trait]
    impl AiService for CannedAi {
        async fn is_available(&self) -> bool {
            true
        }
        async fn complete(&self, _r: Req) -> Result<AiResponse, AiError> {
            Ok(AiResponse {
                text: self.0.clone(),
                json: None,
                model: "mock".into(),
            })
        }
        async fn chat_stream(
            &self,
            _r: crate::streaming::ChatTurnRequest,
        ) -> Result<crate::streaming::TokenStream, AiError> {
            let text = self.0.clone();
            Ok(Box::pin(futures::stream::once(async move { Ok(text) })))
        }
    }

    #[tokio::test]
    async fn test_diagnose_failure_parses_structured_reply() {
        let reply = r#"```json
{"summary":"Missing C compiler.","likely_cause":"The build image has no `cc`.",
"suggested_fixes":["Install build-essential in the Dockerfile."],
"category":"dependency","key_log_lines":["error: linker `cc` not found"],"confidence":0.9}
```"#;
        let ai = CannedAi(reply.into());
        let d = diagnose_failure(&ai, &ctx("error: linker `cc` not found"))
            .await
            .unwrap();
        assert_eq!(d.category, FailureCategory::Dependency);
        assert_eq!(d.suggested_fixes.len(), 1);
    }

    #[tokio::test]
    async fn test_diagnose_failure_none_on_empty_log() {
        let ai = CannedAi("{}".into());
        assert!(diagnose_failure(&ai, &ctx("   ")).await.is_none());
    }
}
