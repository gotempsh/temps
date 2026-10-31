---
name: temps-add-handler
description: Add or change an HTTP endpoint in a Temps crate - RequireAuth plus permission and project-scope guards, typed utoipa DTOs, a service call (never the database), an exhaustive From<DomainError> for Problem, audit logging for writes, router and ApiDoc registration, handler tests, and regenerating the web and CLI OpenAPI clients. Use for any new or modified route, request/response shape or permission check.
---
<!--
SPDX-FileCopyrightText: 2024-2026 Temps Contributors
SPDX-License-Identifier: MIT OR Apache-2.0
-->

# Add an HTTP handler

Handlers are the thin HTTP layer: authenticate, authorise, call one service,
audit the write, map the result. Everything else lives in the service.

Reference crate: `crates/temps-flags` (small, complete, follows every rule):

| Piece | File |
|---|---|
| Domain error enum + `From<sea_orm::DbErr>` | `crates/temps-flags/src/error.rs` |
| Service (business logic, mock-DB tests) | `crates/temps-flags/src/services/flag_service.rs` |
| DTOs + handler state | `crates/temps-flags/src/handlers/types.rs` |
| Handlers, routes, `ApiDoc`, `From<FlagError> for Problem`, guard tests | `crates/temps-flags/src/handlers/handler.rs` |
| Audit event types | `crates/temps-flags/src/handlers/audit.rs` |
| Plugin wiring | `crates/temps-flags/src/plugin.rs` |

Read first: `AGENTS.md` (Errors, Architecture), `REVIEW.md`
→ "Authorization and scope" and "Secrets must not reach logs".

## 1. Service first

If the behaviour does not exist in a service yet, add it there: a method on the
crate's service struct returning `Result<T, DomainError>` (a `thiserror` enum
whose variants carry the IDs involved). No `anyhow` in services. The handler
never imports `sea_orm` or touches an entity; `scripts/rule_drift.py` counts
`handler_db_access` for any `use sea_orm` / `Entity::find` in `handlers/` or
`handlers.rs`.

## 2. DTOs

In the crate's `handlers/types.rs` (or next to the handler):

- Request and response structs derive `Serialize`/`Deserialize` and
  `utoipa::ToSchema`; query structs derive `utoipa::IntoParams`.
- Never return `Json<serde_json::Value>`; always a typed struct.
- Convert models to responses with `From`/a constructor; do not serialise entities
  directly.
- Dates are `DateTime<Utc>` (ISO 8601 with `Z`).
- **Secrets**: a stored key, token or password is never returned. Return a flag
  such as `has_secret: bool` (`crates/temps-webhooks/src/handlers.rs`) or `***`.
  The only exception is a credential minted by this request (a new API key, a
  reset password): returned once, with `Cache-Control: no-store`, never logged or
  audited; see `reset_user_password` in `crates/temps-auth/src/handlers.rs`.
- **Lists** are paginated: `page` (default 1), `page_size` (default 20, max 100),
  sorted `created_at` DESC unless the domain has a better natural order. Clamp in
  one shared helper, as `normalize_pagination` in the flags service does, so the
  handler's `total_pages` matches what the service served.

## 3. The handler

Shape, from `create_flag` in `crates/temps-flags/src/handlers/handler.rs`:

```rust
#[utoipa::path(
    tag = "Feature Flags",
    post,
    path = "/projects/{project_id}/flags",
    params(("project_id" = i32, Path, description = "Project ID")),
    request_body = CreateFlagRequest,
    responses(
        (status = 201, description = "Flag created", body = FlagResponse),
        (status = 400, description = "Validation error"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 409, description = "Flag key already exists"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_flag(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<FlagsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
    Json(request): Json<CreateFlagRequest>,
) -> Result<impl IntoResponse, Problem> {
    project_scope_guard!(auth, project_id);
    project_permission_guard!(auth, FlagsWrite, project_id, &state.project_access_checker);

    let flag = state.flag_service.create(project_id, /* typed input */).await?;

    let audit = FeatureFlagCreatedAudit { context: audit_context(&auth, &metadata), /* ids, values */ };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log for feature flag creation: {}", e);
    }

    Ok((StatusCode::CREATED, Json(FlagResponse::new(flag, Vec::new()))))
}
```

Guards (macros in `crates/temps-auth/src/permission_guard.rs`, imported from
`temps_auth`):

- `permission_guard!(auth, SomePermission)`: instance-wide permission. Every
  handler taking `RequireAuth` needs a guard (`unguarded_require_auth` metric).
- Handlers that take a `project_id` also need `project_scope_guard!(auth,
  project_id)` (deployment tokens) **and** either `project_access_guard!` or
  `project_permission_guard!` with the state's `project_access_checker`. The
  test `every_project_scope_guard_has_access_guard_companion` in temps-auth fails
  if the second is missing.
- Use `deny_deployment_token!(auth)` for by-id reads with no project in scope.
- Load rows scoped by the authorised `project_id`, never by a child ID alone.
- A new permission is a new variant in `crates/temps-auth/src/permissions.rs`
  (enum, string form, parse, and the role grant lists), named
  `{Domain}{Read|Write|Create|Delete}`.

## 4. Error mapping

In the handler module, `impl From<DomainError> for Problem` with **one explicit
arm per variant, no `_ =>`** (counted as `problem_catch_all`). 404 for not
found, 400 for validation, 409 for conflicts, 500 for internal, each with
`.with_title(..)` and `.with_detail(error.to_string())`. See `impl From<FlagError>
for Problem` in the flags handler. Inline errors use `temps_core::error_builder`.

## 5. Audit every write

POST, PUT, PATCH and DELETE log an audit event. Pattern (`crates/temps-flags/src/handlers/audit.rs`):

- One `#[derive(Debug, Clone, Serialize)]` struct per operation with
  `pub context: AuditContext` (`temps_core::AuditContext`: `user_id`,
  `ip_address`, `user_agent`) plus the IDs and old/new values.
- `impl temps_core::AuditOperation` with an `UPPER_SNAKE` operation type
  (`"FEATURE_FLAG_CREATED"`); the flags crate uses a small macro for this.
- Build the context from `auth.user_id()` and the `RequestMetadata` extension.
- Call `state.audit_service.create_audit_log(&audit)` after the service succeeds;
  log a failure with `error!` and still return success.
- Never put a secret value in an audit event.

## 6. Register it

- Route in the crate's `configure_routes()` with `{param}` syntax
  (`/projects/{project_id}/flags/{key}`), never `:param`. Paths are relative; the
  `/api` prefix is applied when the application is built.
- Add the handler to `paths(...)` and every new DTO to `components(schemas(...))`
  in the crate's `#[derive(OpenApi)]` doc (`FlagsApiDoc`).
- A new crate also needs a plugin (see the `temps-add-plugin` skill) whose
  `openapi_schema()` returns that doc.
- If the AI assistant should be able to call the operation, its `operation_id`
  must be added to the read or write allowlist (`ai_read_allowlist` /
  `ai_write_allowlist` in `crates/temps-cli/src/commands/serve/console.rs`).
  That is a security decision: read responses must not contain decrypted secrets.
- Check sibling paths that reach the same data (MCP tools, CLI, background jobs)
  for the same permission and scope checks.

## 7. Tests

Every handler gets tests for 401, 403, success and error mapping:

- **Guards (403) by calling the handler function directly** with an
  `AuthContext` and a `MockDatabase`-backed service: `mod tests` in
  `crates/temps-flags/src/handlers/handler.rs` (`reader_cannot_archive_a_flag`,
  `deployment_token_cannot_read_another_projects_flags`,
  `write_handlers_reject_a_read_only_api_key`). A `NoopAuditLogger` keeps the
  audit crate out of the test.
- **401 and full request/response through the router** with `tower::ServiceExt::oneshot`:
  `crates/temps-deployments/src/handlers/wireguard_mesh.rs`
  (`every_mesh_endpoint_requires_authentication`, a `send` helper that inserts the
  `AuthContext` extension only when given, and a `RecordingAuditLogger` that lets
  a test assert the audit event was written).
- **Error mapping**: assert each domain error maps to the intended status
  (`problem_status` in the same wireguard file).
- **Service**: happy path, not found, validation and DB error with `MockDatabase`
  (flags service tests).

## 8. Regenerate the OpenAPI clients

Any change to routes, DTOs or schemas changes the generated clients. Rebuild and
restart the server (`start-temps` skill), then:

```bash
cd web && bun run openapi-ts                 # web/src/api/client from the live server
cd apps/temps-cli && bun run spec:update && bun run generate:api
git diff --numstat -- apps/temps-cli/openapi.json   # a few hundred lines, never tens of thousands
```

Never `curl ... > apps/temps-cli/openapi.json`: the committed file is sorted and
indented and `bun run spec:check` (pre-commit and CI) rejects anything else.
Never add a plugin-only route to `apps/temps-cli/openapi.json`. Commit the
regenerated files, then `bun run lint` in `web/` and `bun run typecheck` in
`apps/temps-cli/`.

## Verification

```bash
cargo check --lib -p <crate>
cargo test --lib -p <crate> <test_name>
cargo test --lib -p <crate>
cargo clippy -p <crate> --all-targets -- -D warnings
cargo test --lib -p temps-auth every_project_scope_guard_has_access_guard_companion
python3 scripts/rule_drift.py
python3 scripts/source_attribution.py check
cd apps/temps-cli && bun run spec:check
```

New files get the SPDX header (`python3 scripts/source_attribution.py annotate
<file>`). Commit with `git commit -s -m "feat(<crate>): <what the endpoint does>"`.

## Checklist

- [ ] `RequireAuth` + permission guard; project scope + access guard when a `project_id` is involved
- [ ] Reads scoped to the authorised project
- [ ] Service call only, no `sea_orm` in the handler
- [ ] Typed DTOs with `ToSchema`/`IntoParams`; no `serde_json::Value` response
- [ ] Exhaustive `From<DomainError> for Problem`
- [ ] Audit event for every write, without secrets
- [ ] Secrets masked; pagination defaults 20/100
- [ ] Route uses `{param}`; handler and schemas in `ApiDoc`
- [ ] 401/403/success/error tests run and pass
- [ ] Both OpenAPI clients regenerated and committed
