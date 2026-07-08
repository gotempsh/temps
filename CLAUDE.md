# CLAUDE.md

Guidance for Claude Code when working with the Temps codebase.
**Core philosophy: code that works safely, and when it fails, explains why comprehensively.**

## Critical Rules

### NEVER
- Access database directly from HTTP handlers -- ALWAYS use services
- Return untyped JSON (`serde_json::Value`) -- ALWAYS use typed structs
- Use `.context()` from anyhow -- ALWAYS use `.map_err()` with typed errors
- Use `.unwrap()` or `.expect()` in production code -- ALWAYS use `?` or explicit error handling
- Use `anyhow::Result` in service layer -- ALWAYS use typed error enums with `thiserror`
- Expose sensitive data (API keys, tokens) in responses -- ALWAYS mask them
- Create N+1 queries -- ALWAYS use JOINs for related data
- Leave the project in non-compilable state
- Use `#[tokio::main]` when integrating with pingora
- Use plain text logging -- ALWAYS use structured JSONL logging
- Create markdown documentation files unless explicitly requested
- Mark Docker tests with `#[ignore]` -- they MUST skip gracefully at runtime instead
- Create error types with generic messages -- ALWAYS include IDs, names, and operation context
- Expose internal dependencies via public accessors (e.g. `service.db()`) -- pass dependencies directly via constructor or AppState
- Use `Option<T>` for dependencies that are required -- use `Arc<T>` and fail at startup if missing
- Use `get_service` for required dependencies in plugins -- use `require_service` which fails fast with a clear error
- Add new runtime configuration as environment variables -- environment variables for configuration are forbidden. ALWAYS model it as a column on the relevant entity row (e.g. `oidc_providers.trust_idp_email`, not `TEMPS_OIDC_SKIP_EMAIL_VERIFIED`) so the admin can change it per-record at runtime via the API/UI, gets audit logging for free, and operators don't have to restart the binary to change a single tenant's behaviour. If the value is sensitive (credentials, tokens, private keys), the column MUST be encrypted at rest via `EncryptionService`, never stored as plaintext -- this applies even where env vars might otherwise seem tempting for secrets (e.g. a Vault CA bundle or auth token: store it encrypted on the provider row, not as `TEMPS_VAULT_CA_BUNDLE`). The only legitimate exception is bootstrap-time config needed before a database connection exists (e.g. `DATABASE_URL`, `TEMPS_DATA_DIR`, `--license-path`)

### ALWAYS
- Run `cargo check --lib` after every modification
- New functionality must compile without warnings
- Write tests for all new functionality AND verify they run successfully
- Use structured logging with explicit log levels
- Use Conventional Commits: `type(scope): description`
- Use services for all business logic
- Implement pagination (default: 20, max: 100) and sorting (default: `created_at` DESC)
- Use typed error handling with proper propagation
- Follow the three-layer architecture pattern
- Keep tests in the same file as the code they test
- Return dates in ISO 8601 format with `Z` suffix
- Use `permission_guard!` macro for authorization in handlers
- Add audit logging for all write operations (CREATE, UPDATE, DELETE)
- Include contextual information (IDs, resource names, paths) in every error message
- Give each component its own copy of shared dependencies (Arc clones) -- never share via accessor methods
- Use `require_service` in plugins for dependencies the app can't function without
- Let the user configure and control their setup -- show status, give instructions, don't do things silently on their behalf
- Design new features to be scalable on a small resource footprint -- see [Scalability & Efficiency](#scalability--efficiency)

---

## Error Handling as First-Class Architecture

Error handling is the most critical aspect of this codebase. Every error must be typed, contextual, and traceable from origin to HTTP response.

### Error Propagation Chain

```
sea_orm::DbErr / std::io::Error / external error
    |
    v  (From<T> impl or map_err)
Domain Error Enum (per crate: BackupError, DeploymentError, etc.)
    |
    v  (From<DomainError> for Problem, defined in handler module)
Problem (RFC 7807 ProblemDetails)
    |
    v  (IntoResponse)
HTTP JSON response: application/problem+json
```

Every crate owns its error types. Errors flow upward through typed conversions, never through string coercion or anyhow wrapping.

### Defining Error Types

Every domain crate defines its own error enum. Error messages MUST include contextual identifiers.

```rust
use thiserror::Error;

#[derive(Error, Debug)]
pub enum BackupError {
    // GOOD: includes service_id, operation context, and original error
    #[error("Failed to encrypt parameter '{param_name}' for service {service_id}: {reason}")]
    EncryptionFailed { service_id: i32, param_name: String, reason: String },

    #[error("Backup {backup_id} not found in project {project_id}")]
    NotFound { backup_id: i32, project_id: i32 },

    #[error("Cannot delete service {service_id}: still linked to {project_count} project(s)")]
    HasLinkedProjects { service_id: i32, project_count: usize },

    #[error("Validation error: {message}")]
    Validation { message: String },

    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("S3 upload failed for backup {backup_id}: {reason}")]
    S3 { backup_id: i32, reason: String },
}
```

**Error type design rules:**
- Use structured fields (`{ service_id: i32, reason: String }`) over bare strings when the error has identifiers
- Use `#[from]` for automatic conversion of common error types (`sea_orm::DbErr`, `std::io::Error`)
- Use `#[error("...")]` messages that a developer can grep for and immediately understand what happened
- Every `NotFound` variant must include the ID that was searched for
- Every operation failure must include what was being operated on

**BAD error variants** (do not create these):
```rust
// These tell you NOTHING when they appear in logs
#[error("Database error")]         // Which database? What operation?
NotFound,                           // What wasn't found?
#[error("Operation failed")]       // Which operation?
#[error("Internal error: {0}")]    // Lazy catch-all
```

### Converting Database Errors

Every domain error implements `From<sea_orm::DbErr>` with semantic mapping:

```rust
impl From<sea_orm::DbErr> for BackupError {
    fn from(error: sea_orm::DbErr) -> Self {
        match error {
            sea_orm::DbErr::RecordNotFound(msg) => BackupError::NotFound {
                backup_id: 0, // When ID is lost, use the DbErr message
                project_id: 0,
            },
            sea_orm::DbErr::RecordNotInserted => BackupError::Validation {
                message: format!("Duplicate record: {}", error),
            },
            _ => BackupError::Database(error),
        }
    }
}
```

### Error Context: map_err over .context()

**CRITICAL**: Never use anyhow's `.context()`. It wraps and hides the original error type.

```rust
// BAD -- .context() loses the original error type
let data = fs::read(&path).context("Failed to read file")?;

// GOOD -- map_err preserves error details and adds context
let data = fs::read(&path)
    .map_err(|e| BackupError::Io {
        path: path.display().to_string(),
        reason: format!("Failed to read backup file: {}", e),
    })?;

// GOOD -- #[from] for automatic conversion when no extra context needed
let user = User::find_by_id(id).one(db).await?;  // DbErr -> DomainError via From
```

### Converting Domain Errors to HTTP Responses

Each handler module defines `From<DomainError> for Problem`. This is where errors become HTTP responses:

```rust
use temps_core::problemdetails::{self, Problem};
use axum::http::StatusCode;

impl From<BackupError> for Problem {
    fn from(error: BackupError) -> Self {
        match error {
            BackupError::NotFound { .. } =>
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Backup Not Found")
                    .with_detail(error.to_string()),

            BackupError::Validation { .. } =>
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Validation Error")
                    .with_detail(error.to_string()),

            BackupError::HasLinkedProjects { .. } =>
                problemdetails::new(StatusCode::CONFLICT)
                    .with_title("Resource In Use")
                    .with_detail(error.to_string()),

            BackupError::Database(_) | BackupError::Io(_) | BackupError::S3 { .. } =>
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(error.to_string()),

            BackupError::EncryptionFailed { .. } =>
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Encryption Error")
                    .with_detail(error.to_string()),
        }
    }
}
```

**Rules:**
- Every match arm must be explicit -- no catch-all `_ =>` arms
- Map to correct HTTP status codes: 404 for not found, 400 for validation, 409 for conflicts, 500 for internal
- Use `.with_detail(error.to_string())` to surface the contextual error message from `#[error("...")]`

### ErrorBuilder for Inline Errors

For errors constructed directly in handlers (not from service errors):

```rust
use temps_core::error_builder;

// Pre-built factories
Err(error_builder::not_found()
    .title("Project Not Found")
    .detail(format!("Project {} does not exist", project_id))
    .build())

Err(error_builder::bad_request()
    .title("Invalid Configuration")
    .detail("Branch name cannot be empty")
    .build())

// Full ErrorBuilder with structured metadata
Err(ErrorBuilder::new(StatusCode::FORBIDDEN)
    .type_("https://temps.sh/probs/insufficient-permissions")
    .title("Insufficient Permissions")
    .detail(format!("Requires {} permission", Permission::BackupsDelete))
    .value("required_permission", Permission::BackupsDelete.to_string())
    .value("user_role", auth.effective_role.to_string())
    .build())
```

### Problem Details Response Format (RFC 7807)

All error responses follow this JSON structure:

```json
{
  "type": "about:blank",
  "title": "Backup Not Found",
  "status": 404,
  "detail": "Backup 42 not found in project 7",
  "instance": "/backups/42"
}
```

---

## Scalability & Efficiency

Temps is a single binary that operators run on small machines (the reference deployment is a Hetzner cpx22: 3 vCPU / 4 GB RAM) while the proxy path may serve very high request rates. **Every new feature must be designed to scale on a small number of resources and be as efficient as possible.** This is a requirement for new functionality, not an optimization to defer.

### Hot path vs. control plane

Classify every piece of new code. The bar differs by an order of magnitude:

- **Hot path** (per-request in `temps-proxy`/`temps-edge`, per-event in analytics/error ingest, per-sample in metrics collection): assume 100k+ ops/s. No locks, no allocations you can avoid, no I/O.
- **Control plane** (handlers, services, background jobs): normal service rules apply, but still bounded memory and no unbounded fan-out.

### Hot-path rules

- **No synchronous I/O and no per-operation network/DB/disk calls.** Never write a row, send an HTTP request, or log to an external store per request. Aggregate in memory (atomics, sharded counters) and flush on an interval, or push into a bounded channel consumed by a background batcher.
- **No `Mutex`/`RwLock` on the request path.** Prefer atomics (`AtomicU64`, prometheus-style counters/histograms which are atomic internally), `arc-swap`/`ArcSwap` for rarely-updated shared config (already the pattern for route tables), thread-locals, or sharded state. A single contended lock at 100k req/s serializes all cores.
- **Bounded channels with explicit overflow policy.** Any `mpsc` fed by the hot path must be bounded, and the send must be `try_send` with a documented drop/degrade policy (count drops in a metric). An unbounded channel is a memory leak with a delay; a blocking send is a proxy stall.
- **Bounded label/key cardinality.** Metrics, caches, and maps keyed by request attributes must use bounded dimensions (status class, not full URL; project ID only if project count is bounded). Unbounded cardinality is an OOM on a 4 GB box.
- **Avoid per-request allocation and serialization** where practical: reuse buffers, prefer `&str`/`Bytes` over `String` clones, don't `serde_json::to_string` on the request path.

### Everywhere rules

- **Constant memory over per-item memory.** Streaming/chunked processing for anything unbounded (log tails, backups, exports). Never load an unbounded set into a `Vec`.
- **Batch writes.** Inserts into ClickHouse/TimescaleDB/Postgres from high-volume producers must be batched with a max-size + max-age flush, never row-at-a-time.
- **Pull over push for telemetry.** Prefer scrape/interval collection (existing `MetricsScraper` pattern) over per-event emission.
- **Background loops must be O(changes), not O(total).** Reconciliation/polling loops should query deltas (updated_at cursors, NOTIFY) rather than rescanning entire tables each tick.
- **Justify it in the PR.** For any feature touching the hot path or a high-volume data flow, the PR description must state the expected load, the memory bound, and what happens at saturation (drop, degrade, backpressure).

---

## Resilience Patterns

### Retry with Exponential Backoff

Use `RetryConfig` from `temps-core` for operations that can transiently fail:

```rust
use temps_core::retry::RetryConfig;

let config = RetryConfig::new(3)
    .with_base_delay(Duration::from_secs(1))
    .with_max_delay(Duration::from_secs(10));

let result = config.retry(|| async {
    client.call().await
}).await;
```

**When to retry:**
- HTTP calls to external APIs (GitHub, GitLab)
- Database connections after transient failures
- Container operations that may be temporarily unavailable

**When NOT to retry:**
- Validation errors (they won't change on retry)
- Authentication failures
- Not-found errors

### Timeout Handling

Always set explicit timeouts on external operations:

```rust
// HTTP clients
reqwest::Client::builder()
    .timeout(Duration::from_secs(30))
    .build()?;

// Database operations with tokio timeout
let result = tokio::time::timeout(
    Duration::from_secs(5),
    redis_client.get_connection(),
).await
.map_err(|_| ServiceError::ExternalService {
    service: "redis".into(),
    message: "Connection timed out after 5s".into(),
})?;

// Health check loops with bounded waiting
let max_wait = Duration::from_secs(300);
let start = Instant::now();
loop {
    if start.elapsed() > max_wait {
        return Err(DeployerError::HealthCheckTimeout {
            container_id: container_id.to_string(),
            timeout_secs: 300,
        });
    }
    if health_check_passes().await { break; }
    tokio::time::sleep(Duration::from_secs(2)).await;
}
```

### Graceful Degradation

When a subsystem fails, degrade gracefully rather than crashing:

```rust
// Audit log failures should NOT fail the main operation
if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
    error!("Failed to create audit log: {}", e);
    // Continue -- the main operation succeeded
}

// Docker tests skip gracefully when Docker is unavailable
if docker.ping().await.is_err() {
    println!("Docker not available, skipping test");
    return;
}

// Geolocation degrades gracefully when database is missing
match geoip_service.lookup(ip) {
    Ok(location) => Some(location),
    Err(_) => None,  // Feature disabled, not an error
}
```

### Resource Cleanup

Always clean up resources, even on error paths:

```rust
// Use Drop for guaranteed cleanup
impl Drop for TestDatabase {
    fn drop(&mut self) {
        // Create dedicated thread for async cleanup
        let cleanup_thread = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all().build();
            if let Ok(rt) = rt {
                rt.block_on(async {
                    Self::cleanup_schema(&database_url, &schema).await;
                });
            }
        });
        let _ = cleanup_thread.join();
    }
}

// Explicit cleanup in error paths
async fn deploy_container(&self, ctx: &WorkflowContext) -> Result<(), WorkflowError> {
    let container_id = self.create_container(ctx).await?;

    if let Err(e) = self.start_container(&container_id).await {
        // Clean up the container we just created
        let _ = self.remove_container(&container_id).await;
        return Err(e);
    }

    Ok(())
}

// Abort background tasks on cleanup
async fn cleanup(&self) {
    let mut handle = self.log_stream_task.lock().unwrap();
    if let Some(h) = handle.take() {
        h.abort();
    }
}
```

### Transaction Safety

Use transactions for multi-step database operations. Sea-ORM automatically rolls back on drop:

```rust
let txn = self.db.begin().await?;

let environment = new_environment.insert(&txn).await?;
let domain = new_domain.insert(&txn).await?;

// Only commits if both inserts succeed
// Automatic rollback if txn is dropped without commit
txn.commit().await?;
```

---

## Testing for Safety

Tests must verify both success and failure paths. Error-case testing is as important as happy-path testing.

### Test Structure

Tests live in `#[cfg(test)] mod tests` at the bottom of each file:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_create_backup_success() {
        // Arrange
        let db = create_mock_db_with_results(vec![valid_backup_model()]);
        let service = BackupService::new(Arc::new(db));

        // Act
        let result = service.create(valid_request()).await;

        // Assert
        assert!(result.is_ok());
        let backup = result.unwrap();
        assert_eq!(backup.name, "test-backup");
    }

    #[tokio::test]
    async fn test_create_backup_duplicate_name_returns_validation_error() {
        let db = create_mock_db_with_error(DbErr::RecordNotInserted);
        let service = BackupService::new(Arc::new(db));

        let result = service.create(duplicate_request()).await;

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), BackupError::Validation { .. }));
    }

    #[tokio::test]
    async fn test_delete_backup_not_found() {
        let db = create_mock_db_with_results(vec![Vec::<backup::Model>::new()]);
        let service = BackupService::new(Arc::new(db));

        let result = service.delete(999).await;

        assert!(matches!(
            result.unwrap_err(),
            BackupError::NotFound { backup_id: 999, .. }
        ));
    }
}
```

### What to Test

**For every service method, test:**
1. Happy path (valid input -> expected output)
2. Not-found case (invalid ID -> `NotFound` error with correct ID)
3. Validation failures (bad input -> `Validation` error with descriptive message)
4. Database errors (mock DB failure -> appropriate error variant)
5. Edge cases (empty strings, zero values, boundary conditions)

**For every handler, test:**
1. Unauthorized access (no token -> 401)
2. Insufficient permissions (wrong role -> 403)
3. Success response (correct status code and body shape)
4. Error responses (correct Problem Details format)

### Mock Patterns

**Sea-ORM MockDatabase:**
```rust
let db = MockDatabase::new(DatabaseBackend::Postgres)
    .append_query_results(vec![
        vec![backup::Model { id: 1, name: "test".into(), .. }],
    ])
    .into_connection();
let service = BackupService::new(Arc::new(db));
```

**Trait-based mocks for external services:**
```rust
struct MockNotificationService;

#[async_trait]
impl NotificationService for MockNotificationService {
    async fn send_notification(&self, _: NotificationData) -> Result<(), NotificationError> {
        Ok(())
    }
}
```

**mockall for complex trait mocking:**
```rust
mock! {
    ContainerDeployer {}
    #[async_trait]
    impl ContainerDeployer for ContainerDeployer {
        async fn deploy_container(&self, req: DeployRequest) -> Result<DeployResult, DeployerError>;
        async fn stop_container(&self, id: &str) -> Result<(), DeployerError>;
    }
}
```

**TestDatabase for integration tests (shared Docker container):**
```rust
let test_db = TestDatabase::new().await;
// Each test gets isolated schema, automatic cleanup via Drop
```

### Test Verification

After writing any test, immediately run it:

```bash
cargo test --lib -p your-crate test_your_function_name
cargo test --lib -p your-crate  # Run all tests in crate to check for regressions
```

### Docker Tests

Docker-dependent tests MUST NOT use `#[ignore]`. Instead, detect Docker availability and skip gracefully:

```rust
#[tokio::test]
async fn test_postgres_upgrade() {
    let docker = Docker::connect_with_defaults();
    if docker.is_err() || docker.unwrap().ping().await.is_err() {
        println!("Docker not available, skipping");
        return;
    }
    // ... actual test
}
```

---

## Architecture

### Three-Layer Architecture

```
HTTP Layer (Handlers)  -->  Service Layer  -->  Data Access Layer (Sea-ORM)
```

- **Handlers**: Auth, permissions, request/response DTOs, audit logging, OpenAPI docs
- **Services**: Business logic, error types, validation, orchestration
- **Data Access**: Sea-ORM entities, queries, migrations

### Service Pattern

```rust
pub struct BackupService {
    db: Arc<DatabaseConnection>,
    encryption_service: Arc<EncryptionService>,
    notification_service: Arc<dyn NotificationService>,
}

impl BackupService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        encryption_service: Arc<EncryptionService>,
        notification_service: Arc<dyn NotificationService>,
    ) -> Self {
        Self { db, encryption_service, notification_service }
    }

    pub async fn create(&self, request: CreateBackupRequest) -> Result<Backup, BackupError> {
        // Validate input
        if request.name.is_empty() {
            return Err(BackupError::Validation {
                message: "Backup name cannot be empty".into(),
            });
        }

        // Database operations via Sea-ORM
        let model = backup::ActiveModel { name: Set(request.name), .. }
            .insert(self.db.as_ref())
            .await?;  // DbErr -> BackupError via From

        Ok(model)
    }
}
```

**Rules:**
- Dependencies injected via constructor as `Arc<T>` or `Arc<dyn Trait>`
- Return `Result<T, DomainError>`, never `anyhow::Result`
- Validate inputs at the start of each method
- Use `?` operator for error propagation through `From` impls

### Handler Pattern

One complete annotated example -- all handlers follow this shape:

```rust
#[utoipa::path(
    tag = "Backups",
    post,
    path = "/backups",
    request_body = CreateBackupRequest,
    responses(
        (status = 201, description = "Backup created", body = BackupResponse),
        (status = 400, description = "Validation error", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn create_backup(
    RequireAuth(auth): RequireAuth,                       // 1. Authentication
    State(app_state): State<Arc<AppState>>,                // 2. Service access
    Extension(metadata): Extension<RequestMetadata>,       // 3. Audit context
    Json(request): Json<CreateBackupRequest>,               // 4. Typed request body
) -> Result<impl IntoResponse, Problem> {                  // 5. Typed error response
    permission_guard!(auth, BackupsCreate);                 // 6. Authorization check

    let backup = app_state.services                        // 7. Call service, NEVER DB
        .backup_service
        .create(request.clone())
        .await?;                                            // 8. ? auto-converts to Problem

    // 9. Audit log (failure logged but doesn't fail the request)
    let audit = BackupCreatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        backup_id: backup.id,
        name: backup.name.clone(),
    };
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok((StatusCode::CREATED, Json(BackupResponse::from(backup))))  // 10. Typed response
}
```

**Handler rules:**
- Route parameters use `{param}` syntax, never `:param`
- Return `Result<impl IntoResponse, Problem>`
- Never access database directly -- only call services
- Never use `.unwrap()` or `.expect()`
- All write operations (POST, PATCH, DELETE) must include audit logging
- Convert entities to response DTOs via `From` trait
- Register all handlers in `ApiDoc` with `#[openapi(...)]`

### Permission System

```rust
// permission_guard! returns 403 with structured error if check fails
permission_guard!(auth, BackupsCreate);

// Equivalent with full path
permission_check!(auth, Permission::BackupsCreate);
```

Permission naming: `{Domain}{Operation}` where Operation is `Read`, `Write`, `Create`, or `Delete`.

### Plugin System

Crates register services via the plugin system using type-safe DI:

```rust
impl TempsPlugin for BackupPlugin {
    fn register_services(&self, ctx: &ServiceRegistrationContext) -> Result<()> {
        let db = ctx.require_service::<Arc<DatabaseConnection>>();
        let encryption = ctx.require_service::<Arc<EncryptionService>>();
        ctx.register_service(Arc::new(BackupService::new(db, encryption)));
        Ok(())
    }
}
```

Two-phase initialization: all services register first, then all services initialize (can cross-reference).

---

## Structured Logging

### Application Logging (tracing)

```rust
error!("Failed to connect to service {}: {}", service_id, e);  // Critical failures
warn!("Rate limit approaching for project {}", project_id);     // Non-critical
info!("Deployment {} completed for project {}", deploy_id, project_id);  // Business events
debug!("Initializing backup service with {} providers", count); // Technical details
```

**Rules:**
- ERROR: Database failures, auth failures, unrecoverable errors
- WARN: Rate limits, retries, degraded functionality
- INFO: Business events (deployments, backups, user actions)
- DEBUG: Service initialization, file creation, configuration
- Always include IDs and context in log messages

### Deployment Logging (JSONL)

Deployment/build logs use structured JSONL format via `temps-logs`:

```rust
log_service.log_info(log_id, "Building image...").await?;
log_service.log_success(log_id, "Build complete").await?;
log_service.log_warning(log_id, "Retrying connection...").await?;
log_service.log_error(log_id, "Build failed: missing Dockerfile").await?;
```

---

## Database

- **ORM**: Sea-ORM for all queries
- **Database**: PostgreSQL with TimescaleDB extension
- **Raw queries**: Use `DatabaseBackend::Postgres` with `$1`, `$2` parameter binding
- **Dates**: Always use `DateTime<Utc>` (serializes with `Z` suffix), never `NaiveDateTime`
- **Time bucketing**: Never cast `time_bucket()` in the same query level as GROUP BY -- use subqueries
- **Prevent N+1**: Use JOINs for related data, never loop queries

```rust
// BAD: N+1
for session in sessions {
    let visitor = Visitor::find_by_id(session.visitor_id).one(db).await?;
}

// GOOD: Single query
let sessions = Session::find()
    .inner_join(Visitor)
    .all(db).await?;
```

### Pagination

```rust
pub async fn list(&self, page: Option<u64>, page_size: Option<u64>) -> Result<(Vec<Model>, u64), Error> {
    let page = page.unwrap_or(1);
    let page_size = std::cmp::min(page_size.unwrap_or(20), 100);
    let paginator = Entity::find()
        .order_by_desc(Column::CreatedAt)
        .paginate(self.db.as_ref(), page_size);
    let total = paginator.num_items().await?;
    let items = paginator.fetch_page(page - 1).await?;
    Ok((items, total))
}
```

---

## Build & Test Commands

```bash
# Check compilation (fast, run after every change)
cargo check --lib
cargo check --lib -p temps-deployer          # Specific crate

# Run tests
cargo test --lib                             # All unit tests
cargo test --lib -p temps-backup             # Specific crate
cargo test --lib -p temps-backup test_name   # Specific test
cargo test --lib -p temps-backup -- --nocapture  # With output

# Build
cargo build --bin temps                      # Debug build (skips web UI)
cargo build --release --bin temps            # Release build (includes web UI)
FORCE_WEB_BUILD=1 cargo build               # Debug with web UI
```

**Build discipline**: Only run `cargo build`/`cargo check` when at least 99% confident the code will compile. Fix all warnings before considering work complete.

### Conventional Commits

```
feat(auth): add JWT token refresh
fix(backup): handle missing S3 bucket gracefully
refactor(deployments): extract workflow execution into service
test(providers): add Redis connection timeout tests
```

Types: `feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`, `revert`

**Every commit on a PR branch is checked**, not just the final one — the `Changelog` workflow validates the full `base..HEAD` range and fails the whole PR if any single commit's subject doesn't conform.

`git revert` defaults to a non-conventional subject (`Revert "original message"`). Never accept that default — always pass an explicit conventional message, e.g. `git revert --no-edit` then `git commit --amend -m "test: drop temporary failing test"`, or better, pass `-m` directly on the revert itself.

---

## Workspace Structure

51 crates organized by domain:

| Category | Crates |
|---|---|
| **Core** | `temps-core`, `temps-database`, `temps-entities`, `temps-migrations`, `temps-routes`, `temps-config` |
| **Auth** | `temps-auth` |
| **Deployment** | `temps-deployments`, `temps-deployer`, `temps-proxy`, `temps-queue` |
| **Source Control** | `temps-git` |
| **Domains/TLS** | `temps-domains`, `temps-dns` |
| **External Services** | `temps-providers`, `temps-query`, `temps-query-postgres`, `temps-query-s3`, `temps-query-redis`, `temps-query-mongodb` |
| **Analytics** | `temps-analytics`, `temps-analytics-events`, `temps-analytics-funnels`, `temps-analytics-session-replay`, `temps-analytics-performance` |
| **Operations** | `temps-backup`, `temps-logs`, `temps-monitoring`, `temps-audit`, `temps-notifications` |
| **Storage** | `temps-kv`, `temps-blob`, `temps-static-files` |
| **Error Tracking** | `temps-error-tracking`, `temps-embeddings` |
| **Other** | `temps-cli`, `temps-email`, `temps-webhooks`, `temps-geo`, `temps-presets`, `temps-import`, `temps-import-types`, `temps-import-docker`, `temps-vulnerability-scanner`, `temps-infra`, `temps-status-page`, `temps-captcha-wasm` |

---

## Sensitive Data Protection

```rust
// ALWAYS mask secrets in API responses
impl From<Settings> for SettingsResponse {
    fn from(settings: Settings) -> Self {
        Self {
            api_key: settings.api_key.as_ref().map(|_| "***".to_string()),
            project_name: settings.project_name,
        }
    }
}
```

- API keys, tokens, passwords: always mask with `***` in responses
- Encryption at rest via `EncryptionService` (AES-256-GCM)
- Session tokens via `CookieCrypto`
- S3 credentials encrypted before database storage

---

## Bollard (Docker) Integration

The codebase uses **Bollard 0.19+** with OpenAPI-generated types:

```rust
use bollard::query_parameters::*;
use bollard::models::*;

// Container creation uses builder pattern
let container = docker.create_container(
    Some(CreateContainerOptionsBuilder::new().name(&name).build()),
    ContainerCreateBody { image: Some(image.to_string()), ..Default::default() },
).await?;

// Boolean fields are plain bool (not Option<bool>)
docker.remove_container(id, Some(RemoveContainerOptions { force: true, ..Default::default() })).await?;
```

Key API changes from older Bollard: `bollard::container::*` -> `bollard::query_parameters::*`, `Config` -> `ContainerCreateBody`, boolean fields are plain `bool`.

---

## Pingora Integration

- Command `execute()` methods must be **synchronous** (not `#[tokio::main]`)
- Create local tokio runtime only for specific async operations (DB connections)
- Let pingora manage the main runtime after startup

---

## Frontend Guidelines (web/)

### Stack
- React + TypeScript, Tanstack Query, shadcn/ui, Tailwind CSS, Rsbuild
- Package manager: `bun` (not npm/yarn)

### Critical React Rules

**No IFEs in JSX** -- extract to helper functions or separate components:
```tsx
// BAD: {(() => { ... })()}
// GOOD: {formatData(data)} or <DataDisplay data={data} />
```

**No hooks after early returns** -- all hooks must be called before any conditional returns:
```tsx
// BAD
if (isLoading) return <Spinner />
useEffect(() => { ... }, [])  // Skipped when loading!

// GOOD
useEffect(() => { ... }, [])
if (isLoading) return <Spinner />
```

**No conditional mounting of stateful components** -- use `open` props instead:
```tsx
// BAD: {condition && <Dialog />}
// GOOD: <Dialog open={condition} />
```

### State Management
- Use React Query's `isPending`/`isLoading`/`isError` -- never manual `useState` for loading states
- Use React Hook Form with Zod validation for all forms
- Invalidate queries after mutations

### UI Rules
- Always provide visual feedback (toast/loading/error states) for every user action
- Use `CopyButton` component for copy-to-clipboard (never manual clipboard handlers)
- Center content with `mx-auto` when using `max-w-*` constraints
- Use cards for selections instead of dropdowns where practical
- **Skeletons over spinners for content loading** -- when a page, card, or list is waiting on data, render `Skeleton` placeholders that match the real layout. Never use centered `Loader2` spinners for content loading; the page should not visibly "collapse" then "expand" when data arrives. Inline button spinners on mutations (`verify.isPending`) are still fine since they indicate action execution, not content loading.

### Mobile Responsiveness
- **Tables**: wrap in `overflow-x-auto`; hide secondary columns with `hidden md:table-cell`
- **Filter bars**: `flex flex-col gap-2 sm:flex-row sm:flex-wrap`; selects use `w-full sm:w-[Npx]`
- **Grids**: `grid-cols-1` → `md:grid-cols-2` → `lg:grid-cols-3` (or `grid-cols-2 md:grid-cols-4` for stat cards)
- **Side panels**: `flex-col lg:flex-row`; panel uses `w-full lg:w-[Npx]`
- **Pagination**: compact `{page} / {totalPages}` on mobile; full "Showing X–Y of Z" `hidden sm:inline`
- **Headers**: `flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between`
- **Button text**: `hidden sm:inline` for labels next to icons; icon-only on mobile
- **Min-width**: add `min-w-[Npx]` on scrollable containers so content doesn't collapse

### Testing with Playwright

If you need to run or test deployment-related features locally, ask for the required credentials (such as Docker or Temps platform accounts/env vars) from a maintainer. Credentials are not stored in the repository for security reasons.


---

## Environment Variables

All use `TEMPS_` prefix:

| Variable | Default | Required |
|---|---|---|
| `TEMPS_DATABASE_URL` | -- | Yes |
| `TEMPS_ADDRESS` | `127.0.0.1:3000` | No |
| `TEMPS_TLS_ADDRESS` | -- | No |
| `TEMPS_CONSOLE_ADDRESS` | -- | No |
| `TEMPS_DATA_DIR` | `~/.temps` | No |
| `TEMPS_LOG_LEVEL` | -- | No |

---

## Quick Reference Checklists

### New Service Checklist
- [ ] Define typed error enum with contextual messages
- [ ] Implement `From<sea_orm::DbErr>` with semantic mapping
- [ ] Inject dependencies via constructor as `Arc<T>`
- [ ] Return `Result<T, DomainError>`, never `anyhow::Result`
- [ ] Validate inputs at method entry
- [ ] Write tests for success, not-found, validation, and edge cases
- [ ] Run tests and verify they pass

### New Handler Checklist
- [ ] `RequireAuth` + `permission_guard!`
- [ ] Call services only -- never access DB
- [ ] Implement `From<DomainError> for Problem` (exhaustive match, no `_ =>`)
- [ ] Add audit logging for write operations
- [ ] Register in `ApiDoc` (paths + schemas)
- [ ] Register routes with `{param}` syntax
- [ ] Add OpenAPI documentation with all response codes

### Error Handling Checklist
- [ ] Error enum uses structured fields with IDs and context
- [ ] `#[error("...")]` messages are grep-able and descriptive
- [ ] `From<DbErr>` maps `RecordNotFound` to domain `NotFound`
- [ ] `From<DomainError> for Problem` covers all variants explicitly
- [ ] No `.context()` usage -- only `.map_err()`
- [ ] No `.unwrap()` or `.expect()` in production paths
- [ ] Error messages include: what failed, what was being operated on, and why
