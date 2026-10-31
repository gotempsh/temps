# Plan: building project edge rules

**Design:** [ADR-071](../adr/071-project-edge-rules.md)
**Issue:** #1685
**Branch so far:** `feat/issue-1685-project-level-ingress-routing-rules` (ADR + console UI preview backed by browser storage)

## Goal

Ship edge rules as the request-level firewall: an operator, a script or an
agent can describe "if a request looks like this, then do that" for a project.
The proxy enforces it within a microsecond budget. Every surface (console, API,
CLI, MCP tools, skill docs) is available on day one.

**Done means:**

- Rules created in the console, the CLI or through MCP are enforced by the
  proxy within about a second of saving.
- The console tester, `temps edge-rules test` and the `evaluate_edge_rules`
  tool give the same answer as the proxy for the same request.
- Every write is audited.
- A bad rule can be found and switched off from any surface.

## Where we are

| Piece | State |
|---|---|
| Design (ADR-071) | Proposed. Covers the data model, expression AST, actions, evaluation semantics, pipeline placement, hot-path budget, multi-node behaviour, API, CLI, MCP tools and skills |
| Console UI | Built as a preview: list, if/then editor, request tester, examples. It saves to `localStorage` through `web/src/components/project/settings/edge-rules/store.ts` and is not enforced |
| UI evaluator | `evaluate.ts` (TypeScript) implements the ADR's semantics: path normalization, the ambiguous-path rule, `none` groups, missing values, redirect templating. 58 tests |
| Backend, proxy, CLI, MCP, skills | Nothing yet |
| Attack-mode challenge forgery (ADR §6) | Known and unfixed. Blocks the `challenge` action |

## Principles for the build

1. **One engine, two languages, one test suite.** The proxy evaluates rules in
   Rust. The console tester evaluates them in TypeScript, so it works on unsaved
   drafts. Both read the same conformance fixtures
   (`crates/temps-edge-rules/tests/conformance/*.json`: rule set + request +
   expected decision and trace). If the two disagree, CI fails. The API's
   `/evaluate` uses the Rust engine, so the CLI and MCP get the proxy's exact
   answer.
2. **The engine is a pure library.** No database, no axum, no tokio in the
   evaluator. Parse, validate, compile and evaluate are plain functions over
   plain data. This keeps it fast, fuzzable and usable from the proxy, the API
   and the tests.
3. **Validation lives in one place.** The service validates with the engine's
   validator. The API, CLI and MCP send JSON and get typed errors back. The
   console keeps its client-side checks for instant feedback, but the server is
   authoritative, and the conformance fixtures include invalid rules with
   expected error messages.
4. **Safe by default.** New rules from examples, the CLI and MCP start in
   `log_only`. The service refuses to save a rule that would block the console
   host or the ACME path. Exemptions are hard-coded in the proxy, not
   configurable.
5. **Hot path rules from CLAUDE.md apply.**
   - No locks, I/O or allocation per request beyond what the matchers need.
   - Compiled programs live in an `ArcSwap`.
   - Counters are atomics, flushed on an interval.
   - The log attribution rides the existing batched proxy-log writer.

## Work plan

Six pull requests. Each one compiles, is tested, and leaves the product
consistent: nothing is half-exposed. The first two can be built in parallel.

### PR 1: fix challenge forgery (ADR §6, phase 0)

Independent security fix to today's attack mode, and a prerequisite for the
`challenge` action.

- **Signed challenges:** `/_temps/captcha/verify` stops trusting the
  client-supplied `challenge`, `environment_id` and `identifier`. The challenge
  is issued HMAC-signed (project, environment, expiry, nonce) and verified
  server-side.
- **Signed clearance cookie:** clearance becomes a signed, expiring cookie
  scoped to the project and environment, carrying the rule revision. A JA4
  fingerprint alone no longer clears a client.
- **Key:** the signing key is stored encrypted via `EncryptionService`, not in
  an environment variable.
- **Tests:**
  - A forged challenge is rejected.
  - A replayed nonce is rejected.
  - A cookie from another project is rejected.
  - An expired cookie is rejected.
  - The happy path still clears.
- **Files:** `crates/temps-proxy/src/handler/captcha.rs`,
  `crates/temps-proxy/src/service/challenge_service.rs`.

### PR 2: engine crate and data model

**New crate `crates/temps-edge-rules`** (pure, no I/O):

| Module | Contents |
|---|---|
| `model.rs` | Serde types for the AST (`Expression`, `Condition`, `Group`, `Field`, `Operator`), `Action`, `Mode`. Exactly the JSON the ADR specifies and the UI already produces |
| `validate.rs` | All ADR limits and rules (100 rules, 32 conditions, depth 4, list sizes, value bytes, glob wildcards, regex size, redirect target safety). Returns typed `RuleIssue { target, message }` with node IDs so clients can highlight the field |
| `normalize.rs` | Path normalization (dot segments, unreserved-escape decoding, `//` collapsing) and the ambiguity flag |
| `compile.rs` | AST → `CompiledProgram`: globs → matchers, regex via `regex` with `size_limit`, CIDR lists → prefix trie, lowercase folding done once |
| `eval.rs` | `RequestFacts<'a>` (borrowed from the request) → `Decision` plus an optional trace (only for `/evaluate`, never on the hot path) |
| `error.rs` | `EdgeRuleError` (thiserror, with rule IDs and positions in every message) |

**Data model:**
- Migration `m2026MMDD_000001_create_project_edge_rules`:
  - Table, deferrable unique `(project_id, position)`, indexes.
  - Trigger `pg_notify('edge_rules_changed', project_id)`.
  - Nullable `proxy_logs.edge_rule_id` and `edge_rule_mode` columns.
- Entity `crates/temps-entities/src/project_edge_rules.rs`.

**Tests:**
- Unit tests in each module.
- Conformance fixtures, seeded by porting the 58 UI test cases.
- A proptest that random valid ASTs round-trip through serde and never panic in
  compile or eval.
- A cargo-fuzz target on the path normalizer.
- Criterion benchmarks: a 100-rule worst-case program evaluates in under 5 µs
  per request (ADR §7 budget).

**Web change:** `evaluate.test.ts` also runs the shared conformance fixtures.

### PR 3: service, API and console on the API

- **Service** `EdgeRuleService` in `crates/temps-proxy/src/service/`, next to
  `ip_access_control_service.rs`:
  - CRUD with `revision` checks (409 on mismatch).
  - Atomic reorder in one transaction.
  - Duplicate.
  - `evaluate(project_id, sample)` using the engine.
  - Self-lockout guard: refuses an enforced rule that would block the console
    host or `/.well-known/acme-challenge/`.
  - Typed `EdgeRuleServiceError`.
- **Handlers** `crates/temps-proxy/src/handler/edge_rules.rs`, under
  `/projects/{project_id}/edge-rules`:
  - `GET /`, `POST /`, `GET /{rule_id}`, `PATCH /{rule_id}`, `DELETE /{rule_id}`
  - `PUT /order`
  - `POST /evaluate`
- **Handler requirements:**
  - `permission_guard!` with new `EdgeRulesRead`/`EdgeRulesWrite` permissions.
  - Audit entries carrying the before/after rule.
  - Registered in `ApiDoc`.
- **Clients:**
  - Regenerate `apps/temps-cli/openapi.json` (`bun run spec:update`).
  - Regenerate both generated clients.
- **Console:**
  - Swap `store.ts` from `localStorage` to the generated client. The hooks keep
    their names and shapes, so pages don't change.
  - The tester calls `/evaluate` for saved rules and keeps local evaluation for
    unsaved drafts (the fixtures guarantee they agree).
  - Remove the preview callout.
- **Tests:**
  - Service tests for success, not-found, validation, revision conflict,
    reorder and the lockout guard.
  - Handler tests for 401, 403, 2xx and Problem Details.
  - Console Bun tests for the store mapping.

### PR 4: enforcement in the proxy

- **Loader:**
  - Per-project `ArcSwap<Option<Arc<CompiledProgram>>>` keyed by project ID,
    in a map that is itself swapped only when projects change.
  - Reloads one project on `edge_rules_changed`.
  - In-process broadcast on the single-node path (same pattern as
    `route_reload_subscriber.rs`).
  - 60 s reconcile using `max(updated_at)` per project.
- **Placement in `request_filter`:**
  - After project resolution, `RequestPolicyGate` and `ProjectIpGate`, and
    before rate limiting, attack mode and the password wall (ADR §5).
  - ACME and `/_temps/*` paths are exempt before evaluation.
- **Actions:** block (status + body), allow (skip the remaining edge rules
  only), challenge (on the PR 1 clearance), redirect (templated `Location`),
  log.
- **`log_only`:** records the would-be decision and continues.
- **Attribution:**
  - `edge_rule_id` and mode written through the existing
    `proxy_log_batch_writer`.
  - Per-rule match counters as atomics, flushed every 10 s into a small
    `project_edge_rule_stats` rollup.
  - Prometheus metrics with bounded labels (action and mode, never the rule
    ID).
- **Fail-safe:** if a program fails to compile at load (it was validated on
  write, so this means a bug or a downgrade), the proxy logs an error, keeps
  the last good program, and exposes the failure on the rules list. It never
  fails open silently.
- **Workers:** phase-1 behaviour from ADR §8, falling back to the control plane
  for projects with rules. The worker-local engine comes later.
- **Tests:**
  - Proxy integration tests per action and mode.
  - Exemptions hold.
  - Reload after a change.
  - Last-good on a bad program.
  - Counters increment.
  - Benchmarks re-run with the real facts extraction.

### PR 5: CLI, MCP tools, skills (ADR §9.1)

- **CLI** `apps/temps-cli/src/commands/edge-rules/`:
  - Commands: `list`, `get`, `create`, `update`, `delete`, `move`, `enable`,
    `disable`, `enforce`, `log-only`, `test`, `export`, `apply`.
  - Flag shortcuts for simple rules. `--file` for anything nested.
  - Readable rule descriptions that match the console wording.
  - Tests next to each command, plus the `docs.test.ts` / `docs-examples.test.ts`
    coverage the CLI already enforces.
- **MCP** `crates/temps-mcp-server/src/tools/edge_rules.rs`, registered in the
  `networking` group:
  - Read tools: `list_edge_rules`, `get_edge_rule`, `evaluate_edge_rules`.
  - Write proposals: `create_edge_rule` (always `log_only`),
    `update_edge_rule`, `delete_edge_rule`, `reorder_edge_rules`,
    `set_edge_rule_mode`.
  - Proposals carry the plain-words description and the before/after.
  - Add `edge-rules` to the `networking` categories in
    `apps/temps-cli/src/commands/mcp/groups.ts`.
- **Skills:**
  - `skills/temps-cli/SKILL.md`: the commands.
  - `skills/temps/SKILL.md` and `skills/temps-best-practices/SKILL.md`: recipes
    for blocking probes, restricting a path to an office network, and debugging
    a blocked request.
- **Parity test:** a test enumerates the edge-rule API operations from the
  OpenAPI spec and asserts each has a CLI command and an MCP tool.

### PR 6: console finishing

- Match counts and a 24 h sparkline per rule, from the stats rollup, so an
  operator can tell when a `log_only` rule is safe to enforce.
- Proxy logs: an "Edge rule" column and filter, linking a blocked request to
  its rule.
- GeoIP onboarding state for country, continent and ASN conditions when the
  database is not installed (ADR §10).
- Drag-to-reorder, if the up/down buttons prove too slow in practice.
- Component tests (`.test.tsx`) for the editor and list.

## Sequencing

```
PR 1 (challenge fix) ─────────────────────────┐
PR 2 (engine + table) ── PR 3 (API + console) ─┴─ PR 4 (proxy) ── PR 5 (CLI/MCP/skills) ── PR 6 (polish)
```

- PR 1 and PR 2 start together.
- PR 5 can start as soon as PR 3 merges, in parallel with PR 4. It can only be
  released after PR 4, because a CLI that creates rules the proxy ignores would
  mislead.
- Until PR 4 lands, the console keeps a "not enforced yet" notice, now driven
  by a capability flag from the API rather than hard-coded.

## Verification

| Check | How |
|---|---|
| Engine semantics | Conformance fixtures pass in Rust and TypeScript |
| Hot-path budget | Criterion: 100-rule worst case under 5 µs; proxy load test shows no measurable p99 change with 0 rules |
| End-to-end | On a local slot (start-temps skill): deploy a sample app, add each example rule, confirm the block, redirect and challenge with curl and a browser, and confirm `log_only` changes nothing but logs |
| Lockout safety | Try to save a rule blocking the console host or ACME path: refused with a clear error. Enforced rules never affect `/_temps/*` |
| Surfaces agree | The same sample request through the console tester, `temps edge-rules test`, `evaluate_edge_rules` and a real request gives the same decision |
| Audit | Every create, update, delete, reorder and mode change appears with before/after, including MCP-originated ones |

## Risks

| Risk | Mitigation |
|---|---|
| A rule locks the operator out of their own app | Defaults to `log_only`; lockout guard for console and ACME; every surface can disable a rule; the audit log shows who did it |
| The two evaluators drift | Shared conformance fixtures in CI. The Rust engine is the reference |
| Regex or glob cost on the hot path | Linear-time `regex` with `size_limit`, wildcard cap, compile once per change, benchmarks in CI |
| Missed reload notification leaves stale rules | Reconcile loop plus in-process broadcast; revision shown in logs makes staleness visible |
| Multi-node gaps | Phase 1 routes rule-bearing projects through the control plane (ADR §8); documented in the UI and skills |
| Header-based rules on spoofable headers | Editor and CLI warn (already in the UI); docs steer to Client IP |

## Decisions (settled 2026-10-31)

1. **Everything ships in the first release.** All five actions (block, allow,
   challenge, redirect, log), so the challenge-forgery fix (PR 1) lands in the
   same release.
2. **Saving applies at once.** No draft/publish step for now. A draft concept
   may come later (ADR adjustment A1), but it is out of scope.
3. **Paths ignore case by default.** New path conditions start with
   `case_insensitive: true`; the operator can turn it off per condition.
4. **New permissions.** `EdgeRulesRead` and `EdgeRulesWrite`, not reused from
   any existing permission.

The work lands as one pull request, built in the order above, with one commit
per step.
