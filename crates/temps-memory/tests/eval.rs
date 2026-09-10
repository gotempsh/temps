// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Evergreen eval harness for `WorkflowMemoryProvider`.
//!
//! This test suite pins the *behavioral contract* of any memory provider
//! implementation — it doesn't care how facts are stored (Postgres, SQLite,
//! in-memory HashMap). The point: any PR that adds a new backend must pass
//! this harness, and any PR that changes the semantics of existing
//! providers must update the harness (which shows up in review).
//!
//! Scenarios mirror the real workloads we care about:
//! - **tag routing:** facts tagged for a specific trigger outrank generic
//!   ones.
//! - **tenancy:** a fact written for project A never leaks into project B,
//!   and the same for agent_id within a project.
//! - **supersede chain:** when a fact is superseded, the chain is
//!   navigable (old fact has `superseded_by` set, new fact wins lookups).
//! - **search fallback:** an unimplemented search method returns the
//!   documented "not supported" error rather than silently succeeding.
//! - **empty-input rendering:** `render_for_prompt` on zero facts returns
//!   an empty string (callers depend on this to avoid injecting empty
//!   headers into prompts).
//!
//! The harness lives in the `temps-memory` crate because the trait
//! contract belongs here. Any future provider implementation should
//! depend on this file in its own test suite to prove it obeys the same
//! contract.

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Mutex;

use temps_memory::{
    WorkflowMemoryError, WorkflowMemoryFact, WorkflowMemoryProvider, WriteFactRequest,
};

// ── Reference provider used by the harness ─────────────────────────────────
//
// This is an in-memory implementation of `WorkflowMemoryProvider`. Not a
// production backend — just a simple, correct reference that the harness
// exercises. If a behavior is wrong here, the harness itself catches it.

#[derive(Default)]
struct InMemoryProvider {
    // ((project_id, agent_id)) -> facts
    // Kept behind a Mutex so the harness can exercise `write_fact` through
    // the shared trait reference without touching `&mut self`.
    store: Mutex<HashMap<(i32, i32), Vec<StoredFact>>>,
    next_id: Mutex<i64>,
}

struct StoredFact {
    id: i64,
    fact: String,
    tags: Vec<String>,
    confidence: f32,
    times_used: i32,
    superseded_by: Option<i64>,
}

impl InMemoryProvider {
    fn new() -> Self {
        Self {
            store: Mutex::new(HashMap::new()),
            next_id: Mutex::new(1),
        }
    }

    fn alloc_id(&self) -> i64 {
        let mut id = self.next_id.lock().unwrap();
        let out = *id;
        *id += 1;
        out
    }
}

#[async_trait]
impl WorkflowMemoryProvider for InMemoryProvider {
    async fn load_for_trigger(
        &self,
        project_id: i32,
        agent_id: i32,
        relevant_tags: Vec<String>,
        limit: usize,
    ) -> Result<Vec<WorkflowMemoryFact>, WorkflowMemoryError> {
        let store = self.store.lock().unwrap();
        let empty = Vec::new();
        let facts = store.get(&(project_id, agent_id)).unwrap_or(&empty);

        // Rank: tag-matching first (by count of matching tags DESC), then
        // by confidence DESC. Ties broken by id ASC so results are stable.
        let mut scored: Vec<(usize, &StoredFact)> = facts
            .iter()
            .filter(|f| f.superseded_by.is_none())
            .map(|f| {
                let matches = f.tags.iter().filter(|t| relevant_tags.contains(t)).count();
                (matches, f)
            })
            .collect();
        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| b.1.confidence.partial_cmp(&a.1.confidence).unwrap())
                .then_with(|| a.1.id.cmp(&b.1.id))
        });

        Ok(scored
            .into_iter()
            .take(limit)
            .map(|(_, f)| WorkflowMemoryFact {
                id: f.id,
                fact: f.fact.clone(),
                confidence: f.confidence,
                times_used: f.times_used,
            })
            .collect())
    }

    fn render_for_prompt(&self, facts: &[WorkflowMemoryFact]) -> String {
        if facts.is_empty() {
            return String::new();
        }
        let mut out = String::from("## Memory\n\n");
        for f in facts {
            out.push_str(&format!("- {}\n", f.fact));
        }
        out
    }

    async fn write_fact(
        &self,
        project_id: i32,
        agent_id: i32,
        request: WriteFactRequest,
    ) -> Result<WorkflowMemoryFact, WorkflowMemoryError> {
        let id = self.alloc_id();
        let confidence = request.confidence.unwrap_or(0.7);
        let stored = StoredFact {
            id,
            fact: request.fact.clone(),
            tags: request.tags,
            confidence,
            times_used: 0,
            superseded_by: None,
        };
        let mut store = self.store.lock().unwrap();
        store
            .entry((project_id, agent_id))
            .or_default()
            .push(stored);
        Ok(WorkflowMemoryFact {
            id,
            fact: request.fact,
            confidence,
            times_used: 0,
        })
    }

    async fn supersede_fact(
        &self,
        project_id: i32,
        agent_id: i32,
        fact_id: i64,
        replacement: WriteFactRequest,
    ) -> Result<WorkflowMemoryFact, WorkflowMemoryError> {
        let new_id = self.alloc_id();
        let confidence = replacement.confidence.unwrap_or(0.7);
        let mut store = self.store.lock().unwrap();
        let bucket = store.entry((project_id, agent_id)).or_default();

        let old = bucket.iter_mut().find(|f| f.id == fact_id).ok_or_else(|| {
            WorkflowMemoryError::new(format!(
                "supersede: fact {fact_id} not found for project={project_id} agent={agent_id}"
            ))
        })?;
        old.superseded_by = Some(new_id);

        bucket.push(StoredFact {
            id: new_id,
            fact: replacement.fact.clone(),
            tags: replacement.tags,
            confidence,
            times_used: 0,
            superseded_by: None,
        });

        Ok(WorkflowMemoryFact {
            id: new_id,
            fact: replacement.fact,
            confidence,
            times_used: 0,
        })
    }

    // Intentionally inherits the default `search_facts` impl — the harness
    // asserts that "search not implemented" surfaces as the documented
    // error, not as an empty result set.
}

// ── Scenario helpers ───────────────────────────────────────────────────────

fn req(fact: &str, tags: &[&str]) -> WriteFactRequest {
    WriteFactRequest {
        fact: fact.into(),
        tags: tags.iter().map(|s| s.to_string()).collect(),
        confidence: None,
        source_run_id: None,
    }
}

// ── The harness itself ─────────────────────────────────────────────────────

/// Run the full evergreen suite against a provider. Backends can call this
/// from their own test module to prove they obey the contract.
async fn run_eval_suite(p: &impl WorkflowMemoryProvider) {
    scenario_tag_routing(p).await;
    scenario_tenancy_isolation(p).await;
    scenario_supersede_chain(p).await;
    scenario_search_fallback(p).await;
    scenario_empty_prompt_render(p).await;
}

async fn scenario_tag_routing(p: &impl WorkflowMemoryProvider) {
    // Two facts, one tag-relevant, one generic. Tag-relevant should win.
    p.write_fact(1, 1, req("generic: always use semicolons", &["style"]))
        .await
        .unwrap();
    p.write_fact(
        1,
        1,
        req(
            "oauth fails when state cookie missing",
            &["error_group:42", "file:auth.ts"],
        ),
    )
    .await
    .unwrap();

    let facts = p
        .load_for_trigger(1, 1, vec!["error_group:42".into()], 10)
        .await
        .unwrap();
    assert!(
        facts
            .first()
            .map(|f| &f.fact)
            .is_some_and(|f| f.contains("oauth")),
        "tag-matching fact must rank first; got: {facts:?}",
    );
}

async fn scenario_tenancy_isolation(p: &impl WorkflowMemoryProvider) {
    // Write to (project=2, agent=1). Must not appear under any other scope.
    p.write_fact(2, 1, req("project 2 fact", &[]))
        .await
        .unwrap();

    let wrong_project = p.load_for_trigger(3, 1, vec![], 10).await.unwrap();
    assert!(
        wrong_project.iter().all(|f| f.fact != "project 2 fact"),
        "fact leaked across project boundary: {wrong_project:?}",
    );

    let wrong_agent = p.load_for_trigger(2, 9, vec![], 10).await.unwrap();
    assert!(
        wrong_agent.iter().all(|f| f.fact != "project 2 fact"),
        "fact leaked across agent boundary: {wrong_agent:?}",
    );
}

async fn scenario_supersede_chain(p: &impl WorkflowMemoryProvider) {
    let old = p
        .write_fact(10, 10, req("db url is postgres://old", &["db"]))
        .await
        .unwrap();
    let new = p
        .supersede_fact(10, 10, old.id, req("db url is postgres://new", &["db"]))
        .await
        .unwrap();

    let facts = p
        .load_for_trigger(10, 10, vec!["db".into()], 10)
        .await
        .unwrap();
    // The superseded fact should not appear in the ranked load. The new
    // one should, and it should be the winner.
    assert!(
        facts.iter().any(|f| f.id == new.id),
        "new fact missing from load; got: {facts:?}",
    );
    assert!(
        facts.iter().all(|f| f.id != old.id),
        "old fact still served after supersede; got: {facts:?}",
    );
}

async fn scenario_search_fallback(_p: &impl WorkflowMemoryProvider) {
    // Specifically assert the default-impl `search_facts` returns the
    // documented error. We use a fresh provider so we're sure we're
    // hitting the default (the test above uses a provider that may have
    // overridden it in a future change).
    let bare: Box<dyn WorkflowMemoryProvider> = Box::new(BareReadOnly);
    let err = bare.search_facts(1, 1, "anything", 10).await.unwrap_err();
    assert!(
        err.to_string().contains("not supported"),
        "default search_facts should surface 'not supported'; got: {err}",
    );
}

async fn scenario_empty_prompt_render(p: &impl WorkflowMemoryProvider) {
    // Empty input must not produce a header. Callers assume "if
    // render_for_prompt returns empty, don't inject a memory section".
    assert_eq!(p.render_for_prompt(&[]), "");
}

/// Minimal provider that implements only the required methods, used to
/// assert default-impl behavior for optional trait methods.
struct BareReadOnly;

#[async_trait]
impl WorkflowMemoryProvider for BareReadOnly {
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

// ── Entry point ────────────────────────────────────────────────────────────

#[tokio::test]
async fn eval_in_memory_reference_provider() {
    // Runs the full behavioral contract against the in-memory reference
    // provider. If this ever fails, it means either the harness or the
    // reference provider has drifted — both live in this file so a PR
    // touching one is visible alongside the other.
    let p = InMemoryProvider::new();
    run_eval_suite(&p).await;
}
