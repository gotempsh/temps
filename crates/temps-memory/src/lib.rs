// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Evergreen workflow memory primitives for Temps.
//!
//! This crate owns the contract for "memory" — the persistent,
//! per-workflow notepad that AI processes can read from and write to
//! across runs. It defines:
//!
//! - [`WorkflowMemoryProvider`] — the trait the agent executor calls at
//!   prompt-build time to load relevant facts.
//! - [`WorkflowMemoryFact`] / [`WorkflowMemoryError`] — plain data the
//!   trait hands around. No DB, no HTTP, no Sea-ORM here.
//! - [`MEMORY_SCRIPT`] + [`memory_install_command`] — the bash script
//!   that runs inside sandboxes, so AI harnesses can `memory write "..."`
//!   without a Rust client.
//!
//! ## Why this crate exists
//!
//! Memory is used by two subsystems:
//!
//! 1. **Agents** — `temps-agents::executor` reads memory to build prompts.
//! 2. **Sandboxes** — the bash script below runs inside every workflow
//!    sandbox so the AI can remember things between runs.
//!
//! No in-tree implementation of [`WorkflowMemoryProvider`] currently exists
//! (the previous one lived in the now-removed `temps-workspace` crate). The
//! agent executor handles a missing provider gracefully — runs proceed
//! without memory injection.
//!
//! ## Not in this crate
//!
//! - The Sea-ORM entity — lives in `temps-entities::workflow_memory`.
//! - The migration — lives in `temps-migrations`.
//!
//! Those are deliberately kept where they are because they pull in
//! heavy DB + HTTP deps that would poison this crate's consumers.

use async_trait::async_trait;

// ── Plain data ──────────────────────────────────────────────────────────────

/// One memory fact as returned by a provider. Mirrors the
/// `temps_entities::workflow_memory::Model` shape but lives here so the
/// agent executor never needs to depend on the entities crate just to
/// read memory.
///
/// Fields are intentionally narrow — only what a prompt-builder needs.
/// Richer data (tags, source_run_ids, timestamps) stays on the DB side
/// and is surfaced through the HTTP API for UI consumers.
#[derive(Debug, Clone)]
pub struct WorkflowMemoryFact {
    pub id: i64,
    pub fact: String,
    pub confidence: f32,
    pub times_used: i32,
}

/// Errors from a memory provider. Kept opaque so individual
/// implementations (Postgres-backed, in-memory for tests, a future
/// remote backend) can use their own typed errors internally without
/// forcing consumers to depend on a shared error enum.
#[derive(Debug, thiserror::Error)]
#[error("workflow memory error: {message}")]
pub struct WorkflowMemoryError {
    pub message: String,
}

impl WorkflowMemoryError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

// ── Trait ───────────────────────────────────────────────────────────────────

/// Input for creating a memory fact. Parallels the `POST /memory` body
/// but lives here so in-process callers don't need to construct JSON.
#[derive(Debug, Clone)]
pub struct WriteFactRequest {
    pub fact: String,
    pub tags: Vec<String>,
    /// Optional confidence override. `None` means "use the provider's
    /// default" (typically 0.7).
    pub confidence: Option<f32>,
    /// Optional: the run id that produced this fact. Helps later
    /// auditing — "which run wrote this?" — without forcing callers
    /// that don't have a run id to make one up.
    pub source_run_id: Option<i64>,
}

/// Trait the agent executor uses to read workflow memory before
/// spawning an AI harness. No in-tree implementation currently registers
/// one; the executor falls back to running without memory when absent.
///
/// Required methods are **read-only** (`load_for_trigger`,
/// `render_for_prompt`). Write methods (`write_fact`, `supersede_fact`,
/// `search_facts`) are provided with default implementations that
/// return [`WorkflowMemoryError`] `"not supported by this provider"` —
/// this lets lightweight consumers (in-memory fakes for tests,
/// read-only caches) implement only what they need.
///
/// Implementations **must** enforce scoping by `(project_id, agent_id)`.
/// A memory leak across workflows is a correctness bug, not a
/// convenience issue.
#[async_trait]
pub trait WorkflowMemoryProvider: Send + Sync {
    /// Load the most relevant memory facts for an upcoming run.
    ///
    /// `relevant_tags` is a list of tags derived from the trigger
    /// context (e.g. `["error_group_id:42", "file:src/api/login.ts"]`).
    /// The provider should return facts matching any of those tags plus
    /// a few high-confidence general facts as a fallback, so a fresh
    /// workflow with no tagged matches still gets useful context.
    async fn load_for_trigger(
        &self,
        project_id: i32,
        agent_id: i32,
        relevant_tags: Vec<String>,
        limit: usize,
    ) -> Result<Vec<WorkflowMemoryFact>, WorkflowMemoryError>;

    /// Render a list of facts as a markdown section to prepend to a
    /// prompt. Returns an empty string for empty input — the caller
    /// should never inject the section header on its own, since that
    /// would produce prompts with a header and no body.
    fn render_for_prompt(&self, facts: &[WorkflowMemoryFact]) -> String;

    /// Write a new fact. Default: returns unsupported — override in
    /// backends that own writes. Bash-script writes go through the
    /// HTTP API, not this trait, so in-process writers are rare and
    /// mostly used for migrations, imports, and tests.
    async fn write_fact(
        &self,
        _project_id: i32,
        _agent_id: i32,
        _request: WriteFactRequest,
    ) -> Result<WorkflowMemoryFact, WorkflowMemoryError> {
        Err(WorkflowMemoryError::new(
            "write_fact not supported by this WorkflowMemoryProvider",
        ))
    }

    /// Replace an existing fact with an updated version. Used when the
    /// AI discovers a previously-recorded fact is wrong or stale — the
    /// old fact is marked superseded (not deleted) so the audit trail
    /// survives. Default: unsupported.
    async fn supersede_fact(
        &self,
        _project_id: i32,
        _agent_id: i32,
        _fact_id: i64,
        _replacement: WriteFactRequest,
    ) -> Result<WorkflowMemoryFact, WorkflowMemoryError> {
        Err(WorkflowMemoryError::new(
            "supersede_fact not supported by this WorkflowMemoryProvider",
        ))
    }

    /// Full-text search across facts. Default: unsupported. Distinct
    /// from `load_for_trigger` because search is a user-driven lookup
    /// (`memory search "oauth"`), whereas `load_for_trigger` is an
    /// automatic pre-run load keyed on tags.
    async fn search_facts(
        &self,
        _project_id: i32,
        _agent_id: i32,
        _query: &str,
        _limit: usize,
    ) -> Result<Vec<WorkflowMemoryFact>, WorkflowMemoryError> {
        Err(WorkflowMemoryError::new(
            "search_facts not supported by this WorkflowMemoryProvider",
        ))
    }
}

// ── Bash memory script (installed into sandboxes) ───────────────────────────

/// Path inside the sandbox where the memory script is installed.
///
/// Lives under `/home/temps/` (the named home volume), NOT `/workspace`.
/// `/workspace` is bind-mounted from the host repo — anything written there
/// shows up as a tracked file in the user's working tree and can leak into
/// PRs. `/home/temps` is private to the sandbox.
pub const MEMORY_SCRIPT_PATH: &str = "/home/temps/.temps/bin/memory";

/// Directory that must be on `$PATH` inside the sandbox so the AI can
/// type `memory write "..."` without the full path.
pub const MEMORY_SCRIPT_DIR: &str = "/home/temps/.temps/bin";

/// Bash memory client shipped into every workflow sandbox. Reads scope
/// from env vars at runtime, so the same binary works for any workflow.
///
/// **Required env vars:**
/// - `TEMPS_API_URL` — base URL of the Temps API (defaults to
///   `http://host.docker.internal:3000` for local Docker).
/// - `TEMPS_API_TOKEN` — deployment token scoped to the project.
/// - `TEMPS_PROJECT_ID` — project this sandbox is running for.
/// - `TEMPS_WORKFLOW_SLUG` — the workflow's slug (so memory is scoped
///   per workflow, not per project).
pub const MEMORY_SCRIPT: &str = r#"#!/usr/bin/env bash
# Auto-generated by Temps. Do not edit.
# Workflow memory access for the current Temps workflow.
#
# Usage:
#   memory write "<fact>" [--tags tag1,tag2]
#   memory search "<query>"
#   memory list
#   memory supersede <id> --by "<new fact>"
#
# All operations are scoped to the current workflow via TEMPS_WORKFLOW_SLUG
# and the project via TEMPS_PROJECT_ID.

set -euo pipefail

TEMPS_API_URL="${TEMPS_API_URL:-http://host.docker.internal:3000}"
TEMPS_API_TOKEN="${TEMPS_API_TOKEN:?TEMPS_API_TOKEN not set}"
PROJECT_ID="${TEMPS_PROJECT_ID:?TEMPS_PROJECT_ID not set}"
WORKFLOW_SLUG="${TEMPS_WORKFLOW_SLUG:?TEMPS_WORKFLOW_SLUG not set}"

API_BASE="${TEMPS_API_URL}/api/v1/projects/${PROJECT_ID}/workflows/${WORKFLOW_SLUG}/memory"

CMD="${1:-help}"
shift || true

# Convert a comma-separated tag string to a JSON array via jq.
tags_to_json() {
  if [[ -z "${1:-}" ]]; then
    echo "[]"
  else
    echo "$1" | jq -R 'split(",") | map(select(length > 0))'
  fi
}

case "$CMD" in
  write)
    FACT="${1:-}"
    if [[ -z "$FACT" ]]; then
      echo "usage: memory write \"<fact>\" [--tags tag1,tag2]" >&2
      exit 1
    fi
    shift

    TAGS_JSON="[]"
    if [[ "${1:-}" == "--tags" ]]; then
      shift
      TAGS_JSON=$(tags_to_json "${1:-}")
      shift || true
    fi

    BODY=$(jq -n --arg f "$FACT" --argjson t "$TAGS_JSON" '{fact: $f, tags: $t}')
    RESPONSE=$(curl -sf -X POST "$API_BASE" \
      -H "Authorization: Bearer $TEMPS_API_TOKEN" \
      -H "Content-Type: application/json" \
      -d "$BODY")
    ID=$(echo "$RESPONSE" | jq -r '.id')
    echo "Saved fact #$ID"
    ;;

  search)
    QUERY="${1:-}"
    if [[ -z "$QUERY" ]]; then
      echo "usage: memory search \"<query>\"" >&2
      exit 1
    fi
    curl -sf -G "$API_BASE/search" \
      -H "Authorization: Bearer $TEMPS_API_TOKEN" \
      --data-urlencode "q=$QUERY" \
      --data-urlencode "limit=10" \
    | jq -r '.facts[] | "[\(.id)] (conf=\(.confidence), used=\(.times_used)) \(.fact)"'
    ;;

  list)
    curl -sf "$API_BASE?limit=50" \
      -H "Authorization: Bearer $TEMPS_API_TOKEN" \
    | jq -r '.facts[] | "[\(.id)] (conf=\(.confidence), used=\(.times_used)) \(.fact)"'
    ;;

  supersede)
    ID="${1:-}"
    if [[ -z "$ID" || "${2:-}" != "--by" || -z "${3:-}" ]]; then
      echo "usage: memory supersede <id> --by \"<new fact>\"" >&2
      exit 1
    fi
    NEW_FACT="$3"
    BODY=$(jq -n --arg f "$NEW_FACT" '{new_fact: $f, new_tags: []}')
    curl -sf -X POST "$API_BASE/$ID/supersede" \
      -H "Authorization: Bearer $TEMPS_API_TOKEN" \
      -H "Content-Type: application/json" \
      -d "$BODY"
    echo
    ;;

  help|*)
    cat <<'EOF'
Temps Memory — persistent notes for this workflow.

Usage:
  memory write "<fact>" [--tags tag1,tag2,tag3]
      Save a new finding. Tags help future runs find it.

  memory search "<query>"
      Find past findings matching the query.

  memory list
      Show all current facts for this workflow.

  memory supersede <id> --by "<new fact>"
      Replace an outdated fact with a new one.

Examples:
  memory write "OAuth fails when state cookie missing" --tags error_group_id:42,file:src/api/auth/callback.ts
  memory search "oauth"
  memory list
EOF
    ;;
esac
"#;

/// Build the shell command (suitable for `docker exec`) that installs
/// the memory script in a sandbox container.
///
/// The command is a single `sh -c` so it's atomic from the caller's
/// perspective: either the script is fully installed and executable,
/// or nothing changed. Partial installs (mkdir without write) would
/// surface as broken `memory write` calls inside the AI harness, and
/// that's a hard failure mode to debug after the fact.
pub fn memory_install_command() -> Vec<String> {
    // Delimiter must not appear in the script body or the heredoc
    // terminates early. The random suffix is stable across runs so the
    // install command itself is deterministic (helpful for caching).
    let delimiter = "TEMPS_MEMORY_SCRIPT_EOF_X9K2P7";
    let script = format!(
        "mkdir -p {dir} && \
         cat > {path} <<'{delim}'\n{contents}\n{delim}\n\
         chmod +x {path}",
        dir = MEMORY_SCRIPT_DIR,
        path = MEMORY_SCRIPT_PATH,
        delim = delimiter,
        contents = MEMORY_SCRIPT,
    );

    vec!["sh".to_string(), "-c".to_string(), script]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_script_has_required_subcommands() {
        assert!(MEMORY_SCRIPT.contains("write)"));
        assert!(MEMORY_SCRIPT.contains("search)"));
        assert!(MEMORY_SCRIPT.contains("list)"));
        assert!(MEMORY_SCRIPT.contains("supersede)"));
        assert!(MEMORY_SCRIPT.contains("help|*)"));
    }

    #[test]
    fn memory_script_reads_required_env_vars() {
        assert!(MEMORY_SCRIPT.contains("TEMPS_API_URL"));
        assert!(MEMORY_SCRIPT.contains("TEMPS_API_TOKEN"));
        assert!(MEMORY_SCRIPT.contains("TEMPS_PROJECT_ID"));
        assert!(MEMORY_SCRIPT.contains("TEMPS_WORKFLOW_SLUG"));
    }

    #[test]
    fn memory_script_uses_strict_bash() {
        assert!(MEMORY_SCRIPT.contains("set -euo pipefail"));
    }

    #[test]
    fn memory_script_targets_versioned_api() {
        // The bash memory client must hit /api/v1/... — any drift here
        // silently breaks sandbox writes if the legacy unversioned path
        // is ever deprecated. Pinned in a test so the next contributor
        // to touch API_BASE gets an immediate compile-time-ish failure.
        assert!(MEMORY_SCRIPT.contains("/api/v1/projects/"));
        assert!(
            !MEMORY_SCRIPT.contains("/api/projects/${PROJECT_ID}"),
            "script still points at the legacy unversioned path",
        );
    }

    #[test]
    fn memory_script_no_cli_dependency() {
        // The script must use curl, NOT a temps CLI binary — the sandbox
        // can't assume temps is on $PATH.
        assert!(MEMORY_SCRIPT.contains("curl"));
        assert!(!MEMORY_SCRIPT.contains("temps memory"));
    }

    #[test]
    fn install_command_structure() {
        let cmd = memory_install_command();
        assert_eq!(cmd[0], "sh");
        assert_eq!(cmd[1], "-c");
        let body = &cmd[2];
        assert!(body.contains("mkdir -p /home/temps/.temps/bin"));
        assert!(body.contains("chmod +x /home/temps/.temps/bin/memory"));
        assert!(body.contains(MEMORY_SCRIPT));
    }

    #[test]
    fn install_command_heredoc_delimiter_unique() {
        // Delimiter must appear exactly twice (open + close) and not
        // elsewhere in the heredoc body, or the heredoc terminates early.
        let cmd = memory_install_command();
        let body = &cmd[2];
        let delim = "TEMPS_MEMORY_SCRIPT_EOF_X9K2P7";
        let count = body.matches(delim).count();
        assert_eq!(
            count, 2,
            "delimiter must appear exactly twice (open + close)"
        );
        assert!(!MEMORY_SCRIPT.contains(delim));
    }

    #[test]
    fn workflow_memory_error_construction() {
        let err = WorkflowMemoryError::new("test failure");
        assert_eq!(err.message, "test failure");
        assert!(err.to_string().contains("test failure"));
    }

    #[test]
    fn default_write_methods_return_unsupported() {
        // Guarantees lightweight/test providers don't accidentally silently
        // succeed on writes they never implemented. The default impls must
        // return an error with "not supported" — callers use this text to
        // distinguish a real backend failure from a capability gap.
        struct ReadOnlyProvider;

        #[async_trait]
        impl WorkflowMemoryProvider for ReadOnlyProvider {
            async fn load_for_trigger(
                &self,
                _: i32,
                _: i32,
                _: Vec<String>,
                _: usize,
            ) -> Result<Vec<WorkflowMemoryFact>, WorkflowMemoryError> {
                Ok(vec![])
            }
            fn render_for_prompt(&self, _: &[WorkflowMemoryFact]) -> String {
                String::new()
            }
        }

        let p = ReadOnlyProvider;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let write = p
                .write_fact(
                    1,
                    1,
                    WriteFactRequest {
                        fact: "x".into(),
                        tags: vec![],
                        confidence: None,
                        source_run_id: None,
                    },
                )
                .await;
            assert!(write.is_err());
            assert!(write.unwrap_err().to_string().contains("not supported"));

            let supersede = p
                .supersede_fact(
                    1,
                    1,
                    42,
                    WriteFactRequest {
                        fact: "x".into(),
                        tags: vec![],
                        confidence: None,
                        source_run_id: None,
                    },
                )
                .await;
            assert!(supersede.is_err());
            assert!(supersede.unwrap_err().to_string().contains("not supported"));

            let search = p.search_facts(1, 1, "q", 10).await;
            assert!(search.is_err());
            assert!(search.unwrap_err().to_string().contains("not supported"));
        });
    }

    #[test]
    fn workflow_memory_fact_clone() {
        // The fact type needs to be cheap to clone for prompt rendering,
        // which happens per-agent-invocation.
        let fact = WorkflowMemoryFact {
            id: 1,
            fact: "test fact".to_string(),
            confidence: 0.8,
            times_used: 3,
        };
        let cloned = fact.clone();
        assert_eq!(cloned.id, 1);
        assert_eq!(cloned.fact, "test fact");
        assert_eq!(cloned.confidence, 0.8);
        assert_eq!(cloned.times_used, 3);
    }
}
