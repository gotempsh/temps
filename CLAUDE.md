# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

# Temps - Architecture & Development Guidelines

## Critical Rules

### 🚫 NEVER
- Access database directly from HTTP handlers - ALWAYS use services
- Return untyped JSON (`serde_json::Value`) - ALWAYS use typed structs
- Expose sensitive data (API keys, tokens) in responses - ALWAYS mask them
- Create N+1 queries - ALWAYS use JOINs for related data
- Leave the project in non-compilable state
- Use `#[tokio::main]` when integrating with pingora
- **Use plain text logging** - ALWAYS use structured logging with `append_structured_log()` or helper methods (`log_info`, `log_success`, `log_warning`, `log_error`)
- **Create markdown documentation files (*.md) unless explicitly requested** - No README files, no documentation files unless the user asks

### ✅ ALWAYS
- Run `cargo check --lib` after every modification
- **New functionality must compile without warnings** - Fix all warnings before considering work complete
- **Write tests for all new functionality AND verify they run successfully** - Tests that don't run are worthless
- **Use structured logging with explicit log levels** - All logs must use `append_structured_log(log_id, LogLevel, message)` or helper methods
- **Use Conventional Commits** - Format: `type(scope): description` (e.g., `feat: add user authentication`, `fix(api): handle null responses`)
  - Types: `feat`, `fix`, `docs`, `style`, `refactor`, `test`, `chore`, `perf`, `ci`, `build`, `revert`
  - CHANGELOG is auto-generated from commits using `git-cliff`
- Use services for all business logic
- Implement pagination (default: 20, max: 100) and sorting (default: created_at DESC)
- Use typed error handling with proper propagation
- Follow the three-layer architecture pattern
- Keep tests for a class/service in the same file, not in separate test files
- Return dates in ISO 8601 format with `Z` suffix for UTC times
- Use `permission_check!` macro for authorization in handlers

## Commit Message Format

Follow [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>[optional scope]: <description>

[optional body]

[optional footer(s)]
```

**Types:**
- `feat:` - New feature (→ Added in CHANGELOG)
- `fix:` - Bug fix (→ Fixed in CHANGELOG)
- `docs:` - Documentation only
- `style:` - Code style (formatting, no logic change)
- `refactor:` - Code refactoring
- `perf:` - Performance improvement
- `test:` - Adding tests
- `build:` - Build system changes
- `ci:` - CI configuration
- `chore:` - Other changes (dependencies, tooling)
- `revert:` - Revert previous commit

**Examples:**
```bash
feat(auth): add JWT token refresh
fix(api): handle null response from external service
docs: update installation instructions
chore(deps): update rust dependencies
```

**Generate CHANGELOG:**
```bash
# Install git-cliff
cargo install git-cliff

# Generate changelog for unreleased changes
git cliff --unreleased --prepend CHANGELOG.md

# Generate changelog for specific version
git cliff --tag v1.0.0 --prepend CHANGELOG.md
```

## Development Setup

### Initial Setup

**One-time setup for git hooks:**
```bash
./scripts/setup-hooks.sh
```

This script will:
- Detect or prompt you to choose between `prek` (Rust-based) or `pre-commit` (Python-based)
- Install the chosen framework if not already installed
- Configure git hooks that run automatically on commit

The hooks include:
- Conventional Commits validator
- Changelog format validator
- Code formatting (cargo fmt)
- Linting (cargo clippy)
- YAML validation
- Trailing whitespace fixes

**Add to your shell profile (~/.zshrc or ~/.bashrc):**
```bash
# If using pre-commit (Python-based)
export PATH="$HOME/Library/Python/3.9/bin:$PATH"  # macOS
export PATH="$HOME/.local/bin:$PATH"              # Linux

# If using prek (Rust-based)
export PATH="$HOME/.cargo/bin:$PATH"
```

**Run hooks manually:**
```bash
# If using prek
prek run --all-files

# If using pre-commit
pre-commit run --all-files
```

### Pre-commit Hooks

Hooks run **automatically** on `git commit`:
- ✅ Validates commit message format (Conventional Commits)
- ✅ Runs `cargo fmt`
- ✅ Runs `cargo clippy`
- ✅ Validates CHANGELOG.md format
- ✅ Checks YAML files
- ✅ Fixes trailing whitespace

**Manual run:**
```bash
pre-commit run --all-files
```

**Skip hooks (not recommended):**
```bash
git commit --no-verify
```

## Build & Test Commands

### Build Commands
```bash
# Check library compilation (fast, run after every change)
cargo check --lib

# Check specific crate
cargo check --lib -p temps-deployer

# Build main binary (use in background)
cargo build --bin temps

# Build with optimizations
cargo build --release --bin temps
```

### Web Build Integration

The web UI is automatically built during `cargo build` via `temps-cli/build.rs` and placed in `temps-cli/dist/`.

**Development (Debug) Mode - Web build SKIPPED by default:**
```bash
# Fast Rust development (skips web build)
cargo build

# Force web build in debug mode
FORCE_WEB_BUILD=1 cargo build
```

**Release Mode - Web build INCLUDED automatically:**
```bash
# Build with web UI (automatic)
cargo build --release

# Skip web build even in release
SKIP_WEB_BUILD=1 cargo build --release
```

**Manual web build:**
```bash
cd web
bun install
bun run build

# Output to temps-cli/dist
RSBUILD_OUTPUT_PATH=../crates/temps-cli/dist bun run build
```

**Important Notes:**
- In debug mode, a **placeholder** `dist/` directory is created with a development HTML page
- This prevents build failures since `include_dir!("dist")` requires the directory to exist
- Tests and debug builds work without building the full web UI
- The placeholder shows instructions for building the full UI when accessed

### Test Commands
```bash
# Run unit tests (no external dependencies)
cargo test --lib

# Run specific test
cargo test test_name

# Run integration tests (requires Docker)
cargo test --features integration-tests

# Run tests for specific crate
cargo test -p temps-deployments

# Run ignored tests (Docker-dependent)
cargo test -- --ignored
```


## Workspace Architecture

This is a **Cargo workspace** with 30+ crates organized by domain:

### Core Infrastructure
- **temps-core**: Core types, utilities, and shared abstractions (CookieCrypto, EncryptionService)
- **temps-database**: Database connection pooling and configuration
- **temps-entities**: Sea-ORM database entities (auto-generated)
- **temps-migrations**: Database schema migrations
- **temps-routes**: HTTP route definitions and handler registration

### Services & Domains
- **temps-analytics**: Core analytics service
- **temps-analytics-funnels**: Funnel tracking and analysis
- **temps-analytics-session-replay**: Session replay functionality
- **temps-auth**: Authentication and permission system
- **temps-deployer**: Docker/container deployment runtime (Bollard-based)
- **temps-deployments**: Deployment orchestration and workflow management
- **temps-git**: Git provider integrations (GitHub, GitLab)
- **temps-logs**: Container log streaming and aggregation
- **temps-proxy**: Reverse proxy with TLS/ACME support (Pingora-based)
- **temps-providers**: External service providers (PostgreSQL, Redis, S3/MinIO)
- **temps-queue**: Job queue management
- **temps-monitoring**: Status page and uptime monitoring
- **temps-embeddings**: Vector embeddings for error grouping

### CLI & Operations
- **temps-cli**: Command-line interface
- **temps-backup**: Backup and restore operations
- **temps-config**: Configuration management
- **temps-audit**: Audit logging

## Logging

### Structured Logging (JSONL Format)

**CRITICAL**: All logs must use structured logging. Plain text logging via `append_to_log()` has been removed.

#### Log Format

All logs are stored in **JSONL (JSON Lines)** format with the following structure:

```json
{"level":"info","message":"Starting deployment","timestamp":"2025-01-25T12:34:56.789Z","line":1}
{"level":"success","message":"Deployment complete","timestamp":"2025-01-25T12:35:12.456Z","line":2}
```

#### Log Levels

- `info` - Informational messages (default)
- `success` - Successful operations (✅)
- `warning` - Warnings or non-critical issues (⏳, ⚠️)
- `error` - Errors or failures (❌)

#### Usage

```rust
use temps_logs::{LogLevel, LogService};

// Basic usage - explicit level
log_service
    .append_structured_log(log_id, LogLevel::Info, "Starting deployment")
    .await?;

// Helper methods (recommended)
log_service.log_info(log_id, "Processing...").await?;
log_service.log_success(log_id, "Deployment complete").await?;
log_service.log_warning(log_id, "Retrying connection...").await?;
log_service.log_error(log_id, "Failed to connect").await?;
```

#### Automatic Level Detection

When streaming logs (e.g., Docker build output), use level detection:

```rust
fn detect_log_level(message: &str) -> LogLevel {
    if message.contains("✅") || message.contains("Complete") || message.contains("success") {
        LogLevel::Success
    } else if message.contains("❌") || message.contains("Failed") || message.contains("Error") {
        LogLevel::Error
    } else if message.contains("⏳") || message.contains("Waiting") || message.contains("warning") {
        LogLevel::Warning
    } else {
        LogLevel::Info
    }
}

// Use in callbacks
let level = detect_log_level(&line);
log_service.append_structured_log(log_id, level, line).await?;
```

#### Reading Logs

```rust
// Get structured logs
let logs: Vec<LogEntry> = log_service.get_structured_logs(log_id).await?;

// Search logs
let results = log_service.search_structured_logs(log_id, "error").await?;

// Filter by level
let errors = log_service
    .filter_structured_logs_by_level(log_id, LogLevel::Error)
    .await?;
```

#### Migration from Plain Text Logging

The `append_to_log()` method has been **removed**. All code must use structured logging:

```rust
// ❌ REMOVED - Will not compile
log_service.append_to_log(log_id, "message\n").await?;

// ✅ CORRECT - Use structured logging
log_service.append_structured_log(log_id, LogLevel::Info, "message").await?;

// ✅ BETTER - Use helper methods
log_service.log_info(log_id, "message").await?;
```

**Why removed?**
- Enforces consistent JSONL format across all logs
- Prevents accidental plain text logs
- Requires explicit log level selection
- Enables frontend to display logs with proper icons, colors, and formatting

## Architecture

### Three-Layer Architecture
```
HTTP Layer (Handlers) → Service Layer → Data Access Layer (Sea-ORM)
```

**HTTP Layer**: Request/response handling, validation, OpenAPI docs
**Service Layer**: Business logic, orchestration, transactions
**Data Access Layer**: Database queries via Sea-ORM entities

### Service Pattern

Services live in individual crates or in `src/services/` for the main binary.

```rust
// In crate or src/services/example_service.rs
pub struct ExampleService {
    db: Arc<DatabaseConnection>,
}

impl ExampleService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    pub async fn get_something(&self, id: i32) -> Result<Model, ServiceError> {
        // Business logic here
    }
}
```


### Handler Pattern

```rust
#[utoipa::path(
    post,
    path = "/examples",
    request_body = CreateExampleRequest,
    responses(
        (status = 201, body = ExampleResponse),
        (status = 403, description = "Insufficient permissions"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_example(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Json(request): Json<CreateExampleRequest>,
) -> Result<Json<ExampleResponse>, Problem> {
    // Check permissions first
    permission_check!(auth, Permission::ExamplesCreate);

    // Only call services, never access DB
    let result = state.services.example_service.create(request).await?;
    Ok(Json(ExampleResponse::from(result)))
}
```

### Type Separation
- **Database Layer**: Sea-ORM entities in `temps-entities` or `src/entities/`
- **Service Layer**: Domain models in service crates/files
- **HTTP Layer**: Request/Response DTOs with `#[derive(Serialize, Deserialize, ToSchema)]`

## Developing HTTP Handlers - Complete Guide

This section provides a comprehensive guide for creating new HTTP handlers following the established patterns in the codebase.

### Handler Development Checklist

When creating a new handler, follow this order:

1. **Define Request/Response Types** with `#[derive(Serialize, Deserialize, ToSchema)]`
2. **Create Service Methods** for business logic (never access DB from handlers)
3. **Define Permissions** for the new operations (if not existing)
4. **Implement Handler Function** with proper authentication and authorization
5. **Add Audit Logging** for all write operations (create, update, delete)
6. **Register in ApiDoc** (paths and schemas)
7. **Register Routes** in `configure_routes()`
8. **Test** and ensure no warnings

### Step-by-Step Handler Development

#### 1. Define Request/Response Types

```rust
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

// Request types
#[derive(Deserialize, ToSchema, Clone)]
pub struct CreateResourceRequest {
    /// Name of the resource
    #[schema(example = "My Resource")]
    pub name: String,
    /// Optional description
    pub description: Option<String>,
}

#[derive(Deserialize, ToSchema, Clone)]
pub struct UpdateResourceRequest {
    /// Optional new name
    pub name: Option<String>,
    /// Optional new description
    pub description: Option<String>,
}

// Response types
#[derive(Serialize, ToSchema)]
pub struct ResourceResponse {
    pub id: i32,
    pub name: String,
    pub description: Option<String>,
    #[schema(example = "2025-10-12T12:15:47.609192Z")]
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[schema(example = "2025-10-12T12:15:47.609192Z")]
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

// Implement From trait for entity to response conversion
impl From<temps_entities::resources::Model> for ResourceResponse {
    fn from(resource: temps_entities::resources::Model) -> Self {
        Self {
            id: resource.id,
            name: resource.name,
            description: resource.description,
            created_at: resource.created_at,
            updated_at: resource.updated_at,
        }
    }
}
```

#### 2. Service Layer (No DB Access in Handlers!)

Handlers must **NEVER** access the database directly. All business logic goes in services:

```rust
// In service file
pub async fn create_resource(&self, request: CreateResourceRequest) -> Result<Model, ServiceError> {
    // Business logic here
    // Validation
    // Database operations via Sea-ORM
    // Error handling
}

pub async fn update_resource(&self, id: i32, request: UpdateResourceRequest) -> Result<Model, ServiceError> {
    // Update logic
}

pub async fn delete_resource(&self, id: i32) -> Result<(), ServiceError> {
    // Deletion logic (consider soft delete)
}
```

#### 3. Define Permissions

For new operations, add permissions to `temps-auth` permission enum:

```rust
// In temps-auth/src/lib.rs or appropriate location
pub enum Permission {
    // Existing permissions...

    // Resource management
    ResourcesRead,     // For GET operations
    ResourcesWrite,    // For PATCH/PUT operations
    ResourcesCreate,   // For POST operations
    ResourcesDelete,   // For DELETE operations
}
```

**Permission Naming Convention**: `{Domain}{Operation}` where Operation is `Read`, `Write`, `Create`, or `Delete`.

#### 4. Implement Handler Functions

**READ Handler (List)**:
```rust
use temps_auth::{permission_guard, RequireAuth};
use temps_core::problemdetails::Problem;

#[utoipa::path(
    tag = "Resources",
    get,
    path = "/resources",
    responses(
        (status = 200, description = "List of resources", body = Vec<ResourceResponse>),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn list_resources(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ResourcesRead);

    let resources = app_state
        .services
        .resource_service
        .list_resources()
        .await?;

    let responses: Vec<ResourceResponse> = resources.into_iter().map(Into::into).collect();
    Ok(Json(responses))
}
```

**READ Handler (Get by ID)**:
```rust
#[utoipa::path(
    tag = "Resources",
    get,
    path = "/resources/{id}",
    responses(
        (status = 200, description = "Resource details", body = ResourceResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Resource not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn get_resource(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ResourcesRead);

    let resource = app_state
        .services
        .resource_service
        .get_resource(id)
        .await?;

    Ok(Json(ResourceResponse::from(resource)))
}
```

**CREATE Handler (with Audit Log)**:
```rust
use temps_core::RequestMetadata;
use tracing::error;

#[utoipa::path(
    tag = "Resources",
    post,
    path = "/resources",
    request_body = CreateResourceRequest,
    responses(
        (status = 201, description = "Resource created", body = ResourceResponse),
        (status = 400, description = "Invalid request", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn create_resource(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<CreateResourceRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ResourcesCreate);

    let resource = app_state
        .services
        .resource_service
        .create_resource(request.clone())
        .await?;

    // CRITICAL: Add audit log for write operations
    let audit = ResourceCreatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        resource_id: resource.id,
        name: resource.name.clone(),
    };

    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok((StatusCode::CREATED, Json(ResourceResponse::from(resource))))
}
```

**UPDATE Handler (with Audit Log)**:
```rust
#[utoipa::path(
    tag = "Resources",
    patch,
    path = "/resources/{id}",
    request_body = UpdateResourceRequest,
    responses(
        (status = 200, description = "Resource updated", body = ResourceResponse),
        (status = 400, description = "Invalid request", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Resource not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn update_resource(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<UpdateResourceRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ResourcesWrite);

    let resource = app_state
        .services
        .resource_service
        .update_resource(id, request.clone())
        .await?;

    // Track which fields were updated
    let mut updated_fields = HashMap::new();
    if request.name.is_some() {
        updated_fields.insert("name".to_string(), "updated".to_string());
    }
    if request.description.is_some() {
        updated_fields.insert("description".to_string(), "updated".to_string());
    }

    let audit = ResourceUpdatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        resource_id: resource.id,
        name: resource.name.clone(),
        updated_fields,
    };

    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok(Json(ResourceResponse::from(resource)))
}
```

**DELETE Handler (with Audit Log)**:
```rust
#[utoipa::path(
    tag = "Resources",
    delete,
    path = "/resources/{id}",
    responses(
        (status = 204, description = "Resource deleted"),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Resource not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn delete_resource(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ResourcesDelete);

    // Get resource details BEFORE deletion for audit log
    let resource = app_state
        .services
        .resource_service
        .get_resource(id)
        .await?;

    app_state
        .services
        .resource_service
        .delete_resource(id)
        .await?;

    let audit = ResourceDeletedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        resource_id: resource.id,
        name: resource.name,
    };

    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok(StatusCode::NO_CONTENT)
}
```

#### 5. Audit Logging for Write Operations

**CRITICAL**: All write operations (CREATE, UPDATE, DELETE) must include audit logging.

Create audit types in your handler module:

```rust
// In handlers/audit.rs or at top of handler file
use temps_core::{AuditContext, AuditOperation};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct ResourceCreatedAudit {
    pub context: AuditContext,
    pub resource_id: i32,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResourceUpdatedAudit {
    pub context: AuditContext,
    pub resource_id: i32,
    pub name: String,
    pub updated_fields: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResourceDeletedAudit {
    pub context: AuditContext,
    pub resource_id: i32,
    pub name: String,
}

// Implement AuditOperation trait for each audit type
impl AuditOperation for ResourceCreatedAudit {
    fn operation_type(&self) -> String {
        "RESOURCE_CREATED".to_string()
    }

    fn user_id(&self) -> i32 {
        self.context.user_id
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}

// Repeat for ResourceUpdatedAudit and ResourceDeletedAudit
```

#### 6. Register in ApiDoc

Add your handler functions and types to the OpenAPI documentation:

```rust
#[derive(OpenApi)]
#[openapi(
    paths(
        list_resources,
        create_resource,
        get_resource,
        update_resource,
        delete_resource,
    ),
    components(
        schemas(
            CreateResourceRequest,
            UpdateResourceRequest,
            ResourceResponse,
        )
    ),
    info(
        title = "Resources API",
        description = "API endpoints for managing resources",
        version = "1.0.0"
    ),
    tags(
        (name = "Resources", description = "Resource management endpoints")
    )
)]
pub struct ResourceApiDoc;
```

#### 7. Configure Routes

Register your handlers in the router:

```rust
pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/resources", get(list_resources).post(create_resource))
        .route("/resources/{id}",
            get(get_resource)
                .patch(update_resource)
                .delete(delete_resource)
        )
}
```

### Handler Development Rules

**ALWAYS**:
- Use `RequireAuth(auth): RequireAuth` for authentication
- Use `permission_guard!(auth, PermissionName)` for authorization
- Use `State(app_state): State<Arc<AppState>>` for service access
- Use `Extension(metadata): Extension<RequestMetadata>` for audit logging
- Call services only - never access database directly
- Add audit logs for all write operations (CREATE, UPDATE, DELETE)
- Return `Result<impl IntoResponse, Problem>` for error handling
- Include all HTTP status codes in OpenAPI documentation
- Convert entities to response DTOs using `From` trait
- Clone request data if needed for audit logs

**NEVER**:
- Access database directly in handlers
- Skip permission checks
- Skip audit logging for write operations
- Use `unwrap()` or `expect()` in handlers
- Return database entities directly (always use DTOs)
- Expose sensitive data in responses (mask API keys, tokens, etc.)

### Route Parameter Format

**CRITICAL**: Route parameters in Axum routes and OpenAPI annotations must use curly braces `{param}`, NOT colons `:param`.

```rust
// ✅ CORRECT - Use curly braces in both route and OpenAPI
pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/projects/{id}", get(get_project))           // ← {id}
        .route("/users/{user_id}/posts/{post_id}", get(...)) // ← {user_id}, {post_id}
}

#[utoipa::path(
    get,
    path = "/projects/{id}",  // ← Must match route definition
    ...
)]

// ❌ INCORRECT - Do NOT use colons
.route("/projects/:id", get(get_project))  // ← Wrong!
path = "/projects/:id"                      // ← Wrong!
```

**Why?**
- Axum supports both `:param` and `{param}` syntax in routes
- OpenAPI specification only supports `{param}` format
- Using `{param}` everywhere ensures consistency and correct OpenAPI documentation

### Common Handler Patterns

**Problem Details Error Mapping**:

The codebase uses RFC 7807 Problem Details for HTTP error responses via `temps_core::problemdetails`. All service errors should be converted to `Problem` type.

**Complete Error Mapping Example**:
```rust
use temps_core::problemdetails::{self, Problem};
use axum::http::StatusCode;

impl From<ServiceError> for Problem {
    fn from(error: ServiceError) -> Self {
        match error {
            // 404 Not Found - Resource doesn't exist
            ServiceError::NotFound(msg) => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Resource Not Found")
                .with_detail(msg),

            // 400 Bad Request - Invalid input, validation failures
            ServiceError::Validation(msg) => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Validation Error")
                .with_detail(msg),

            // 400 Bad Request - Schedule/configuration errors
            ServiceError::Schedule(msg) => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Schedule Error")
                .with_detail(msg),

            // 500 Internal Server Error - Database failures
            ServiceError::Database(e) => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Database Error")
                .with_detail(e.to_string()),

            ServiceError::DatabaseConnectionError(msg) =>
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Database Connection Error")
                    .with_detail(msg),

            // 500 Internal Server Error - External service failures (S3, Redis, etc.)
            ServiceError::S3(e) => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("S3 Storage Error")
                .with_detail(e.to_string()),

            ServiceError::ExternalService(msg) =>
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("External Service Error")
                    .with_detail(msg),

            // 500 Internal Server Error - Configuration issues
            ServiceError::Configuration(msg) =>
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Configuration Error")
                    .with_detail(msg),

            // 500 Internal Server Error - Operation failures
            ServiceError::Operation(msg) =>
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Operation Failed")
                    .with_detail(msg),

            // 500 Internal Server Error - Catch-all
            ServiceError::Internal(msg) =>
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(msg),

            // Default case
            _ => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Internal Server Error")
                .with_detail("An unexpected error occurred"),
        }
    }
}
```

**Using Problem Details in Handlers**:
```rust
// Automatic conversion via ? operator
async fn get_resource(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ResourcesRead);

    // ServiceError automatically converts to Problem via From trait
    let resource = app_state
        .services
        .resource_service
        .get_resource(id)
        .await?;  // ? operator uses From<ServiceError> for Problem

    Ok(Json(ResourceResponse::from(resource)))
}

// Manual conversion with custom details
async fn complex_operation(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ResourcesWrite);

    match app_state.services.resource_service.complex_operation(id).await {
        Ok(result) => Ok(Json(result)),
        Err(ServiceError::NotFound(_)) => {
            // Custom problem with additional context
            Err(problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Resource Not Found")
                .with_detail(format!("Resource with ID {} does not exist", id))
                .with_instance(format!("/resources/{}", id)))
        }
        Err(e) => Err(Problem::from(e)),  // Use default mapping
    }
}

// Using error_builder helper
async fn delete_resource(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ResourcesDelete);

    let result = app_state
        .services
        .resource_service
        .delete_resource(id)
        .await?;

    if !result {
        // Using error_builder for common cases
        return Err(temps_core::error_builder::not_found()
            .title("Resource Not Found")
            .detail(format!("Resource with ID {} not found", id))
            .build());
    }

    Ok(StatusCode::NO_CONTENT)
}
```

**Problem Details Response Format**:
```json
{
  "type": "about:blank",
  "title": "Resource Not Found",
  "status": 404,
  "detail": "Resource with ID 123 does not exist",
  "instance": "/resources/123"
}
```

**Service Error Definition Pattern**:
```rust
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ServiceError {
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("Database connection error: {0}")]
    DatabaseConnectionError(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Schedule error: {0}")]
    Schedule(String),

    #[error("Configuration error: {0}")]
    Configuration(String),

    #[error("External service error: {0}")]
    ExternalService(String),

    #[error("S3 error: {0}")]
    S3(String),

    #[error("Operation failed: {0}")]
    Operation(String),

    #[error("Internal error: {0}")]
    Internal(String),
}
```

**Pagination in Handlers**:
```rust
#[derive(Deserialize, ToSchema)]
pub struct ListQuery {
    #[schema(example = 1)]
    pub page: Option<u64>,
    #[schema(example = 20)]
    pub page_size: Option<u64>,
}

async fn list_resources(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Query(query): Query<ListQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ResourcesRead);

    let (resources, total) = app_state
        .services
        .resource_service
        .list_resources(query.page, query.page_size)
        .await?;

    // Return with pagination metadata
    Ok(Json(json!({
        "data": resources.into_iter().map(ResourceResponse::from).collect::<Vec<_>>(),
        "total": total,
        "page": query.page.unwrap_or(1),
        "page_size": query.page_size.unwrap_or(20),
    })))
}
```

### Testing Handlers

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use axum_test::TestServer;

    #[tokio::test]
    async fn test_create_resource() {
        let app = setup_test_app().await;
        let server = TestServer::new(app).unwrap();

        let response = server
            .post("/resources")
            .add_header("Authorization", "Bearer test-token")
            .json(&json!({
                "name": "Test Resource",
                "description": "Test description"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::CREATED);
    }
}
```

## Permission System

Use the **permission guard macros** from `temps-auth`:

```rust
use temps_auth::{permission_check, Permission, RequireAuth};

// In handler
pub async fn delete_provider(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(provider_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::GitProvidersDelete);

    state.service.delete_provider(provider_id).await?;
    Ok(StatusCode::NO_CONTENT)
}
```

**Alternative syntax**:
```rust
permission_guard!(auth, ApiKeysDelete);  // Without Permission:: prefix
```

## Common Patterns

### Pagination & Sorting
```rust
pub async fn list_examples(
    &self,
    page: Option<u64>,
    page_size: Option<u64>,
    sort_by: Option<String>,
    sort_order: Option<String>,
) -> Result<(Vec<Example>, u64), ServiceError> {
    let page = page.unwrap_or(1);
    let page_size = std::cmp::min(page_size.unwrap_or(20), 100);

    let mut query = Example::find()
        .order_by_desc(example::Column::CreatedAt); // Default sorting

    let paginator = query.paginate(self.db.as_ref(), page_size);
    let total = paginator.num_items().await?;
    let items = paginator.fetch_page(page - 1).await?;

    Ok((items, total))
}
```

### Error Handling

#### Error Type Definition
```rust
#[derive(Error, Debug)]
pub enum ServiceError {
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),
    #[error("Not found")]
    NotFound,
    #[error("Validation error: {0}")]
    Validation(String),
}
```

#### Error Context: Use map_err, NOT .context()

**CRITICAL**: Never use `anyhow`'s `.context()` method. Always use `.map_err()` to preserve underlying error details.

**Why?**
- `.context()` wraps the error and can hide important details (like database constraint violations, connection errors, etc.)
- `.map_err()` allows you to construct custom error variants while preserving the original error information
- Typed errors with `thiserror` provide better error handling than generic `anyhow::Error`

```rust
// ❌ BAD - Using .context() loses error details
let config = std::fs::read_to_string(&path)
    .context("Failed to read config file")?;

let user = User::find_by_id(id)
    .one(db)
    .await
    .context("Failed to fetch user")?;

// ✅ GOOD - Using map_err preserves error details
let config = std::fs::read_to_string(&path)
    .map_err(|e| ServiceError::Configuration(format!("Failed to read config file at {}: {}", path.display(), e)))?;

let user = User::find_by_id(id)
    .one(db)
    .await
    .map_err(|e| ServiceError::Database(format!("Failed to fetch user {}: {}", id, e)))?;

// ✅ EVEN BETTER - Use thiserror's #[from] for automatic conversion
#[derive(Error, Debug)]
pub enum ServiceError {
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),  // Automatic conversion with full error details

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Configuration error: {0}")]
    Configuration(String),
}

// Then just use ? operator - error details are preserved
let user = User::find_by_id(id).one(db).await?;  // Automatically converts to ServiceError::Database
```

**Best Practices:**
1. Define typed error enums with `thiserror::Error`
2. Use `#[from]` for automatic conversion of common error types
3. Use `.map_err()` when you need to add context or convert to custom variants
4. Never use `anyhow::Error` in service layer - use typed errors
5. Include relevant context (IDs, paths, etc.) in error messages
6. Preserve the original error with `{e}` or by wrapping it

**Example Service Error Pattern:**
```rust
use thiserror::Error;

#[derive(Error, Debug)]
pub enum BackupError {
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("S3 error: {0}")]
    S3(String),

    #[error("Backup not found: {0}")]
    NotFound(String),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

impl BackupService {
    pub async fn create_backup(&self, request: CreateBackupRequest) -> Result<Backup, BackupError> {
        // Database errors automatically convert via #[from]
        let backup = backup::ActiveModel {
            name: Set(request.name.clone()),
            // ...
        }
        .insert(self.db.as_ref())
        .await?;  // Automatically becomes BackupError::Database

        // S3 errors need explicit mapping
        self.s3_client
            .put_object()
            .send()
            .await
            .map_err(|e| BackupError::S3(format!("Failed to upload backup {}: {}", backup.id, e)))?;

        Ok(backup)
    }
}
```

### Sensitive Data Protection
```rust
impl From<Settings> for SettingsResponse {
    fn from(settings: Settings) -> Self {
        Self {
            // Mask sensitive fields
            api_key: settings.api_key.as_ref().map(|_| "***".to_string()),
            // Non-sensitive fields
            project_name: settings.project_name,
        }
    }
}
```

### Prevent N+1 Queries
```rust
// ❌ BAD
let sessions = find().all(db).await?;
for session in sessions {
    let visitor = find_by_id(session.visitor_id).one(db).await?; // N+1!
}

// ✅ GOOD - Single query with JOIN
let sessions_with_data = session::Entity::find()
    .inner_join(visitor::Entity)
    .all(db).await?;
```

## Date & Time Formatting

**CRITICAL**: All datetime fields in API responses MUST be in ISO 8601 format with `Z` suffix for UTC times.

**Correct format**: `2025-10-12T12:15:47.609192Z`
**Incorrect format**: `2025-10-12T12:15:47` (missing Z suffix)

### For UTC DateTimes
```rust
use chrono::Utc;

#[derive(Serialize)]
pub struct Response {
    // DateTime<Utc> automatically serializes with 'Z' suffix
    pub created_at: chrono::DateTime<Utc>,
}
```

### SQL Time Bucketing Pattern
When using `time_bucket()` or `time_bucket_gapfill()` with GROUP BY:

```sql
-- ❌ BAD - Cannot cast in SELECT and GROUP BY together
SELECT time_bucket('1 hour', checked_at)::timestamptz AS bucket
FROM events
GROUP BY bucket  -- ERROR: no top level time_bucket in group by clause

-- ✅ GOOD - Use subquery: GROUP BY uncast, then cast in outer SELECT
SELECT bucket::timestamptz as bucket, count
FROM (
    SELECT time_bucket('1 hour', checked_at) AS bucket, COUNT(*) as count
    FROM events
    GROUP BY bucket
) sub
ORDER BY bucket ASC
```

**Key Rule**: Never cast `time_bucket()` in the same query level where you GROUP BY it. Always use subqueries or cast after grouping.

### Database Type Mapping
| Rust Type | SQL Type | Serialization Format |
|-----------|----------|---------------------|
| `DateTime<Utc>` | `TIMESTAMPTZ` | `2025-10-12T12:15:47.609Z` ✅ |
| `NaiveDateTime` | `TIMESTAMP` | `2025-10-12T12:15:47` ❌ (needs custom serializer) |

**Best Practice**: Prefer `DateTime<Utc>` over `NaiveDateTime` for API responses.

## Docker/Bollard Integration

When working with Docker operations (in `temps-deployer`, `temps-providers`, `temps-logs`):

### Modern Bollard API Usage
The codebase uses **Bollard 0.19+** with OpenAPI-generated types:

```rust
use bollard::{
    query_parameters::{
        CreateContainerOptions, InspectContainerOptions, ListContainersOptions,
        LogsOptions, RemoveContainerOptions, StartContainerOptions,
    },
    Docker,
};

// ✅ Use builder pattern for container creation
let container = docker.create_container(
    Some(
        CreateContainerOptionsBuilder::new()
            .name(&container_name)
            .build(),
    ),
    ContainerCreateBody {
        image: Some(image_name.to_string()),
        cmd: Some(vec!["/bin/sh".to_string()]),
        ..Default::default()
    },
).await?;

// ✅ LogsOptions without generic parameter
let logs = docker.logs(
    container_id,
    Some(LogsOptions {
        stdout: true,
        stderr: true,
        follow: true,
        ..Default::default()
    }),
).await;

// ✅ Boolean fields are plain bool, not Option<bool>
docker.remove_container(
    container_id,
    Some(RemoveContainerOptions {
        force: true,  // Not Some(true)
        ..Default::default()
    }),
).await?;
```

**Key Changes from Old API**:
- `bollard::container::*` → `bollard::query_parameters::*` and `bollard::models::*`
- `Config` → `ContainerCreateBody`
- `CreateContainerOptions` → Use builder pattern
- Remove generic parameters from options types
- Boolean fields use plain `bool` instead of `Option<bool>`
- Network options: `CreateNetworkOptions` → `NetworkCreateRequest`

## CLI Commands

### Available Commands
- **`serve`**: Start the full HTTP API server with all services
- **`proxy`**: Start only the proxy server (same parameters as serve)

### Command Structure Pattern
```rust
#[derive(Args)]
pub struct ServeCommand {
    #[arg(long, default_value = "127.0.0.1:3000", env = "TEMPS_ADDRESS")]
    pub address: String,

    #[arg(long, env = "TEMPS_TLS_ADDRESS")]
    pub tls_address: Option<String>,

    #[arg(long, env = "TEMPS_DATABASE_URL")]
    pub database_url: String,

    #[arg(long, env = "TEMPS_DATA_DIR")]
    pub data_dir: Option<PathBuf>,

    #[arg(long, env = "TEMPS_CONSOLE_ADDRESS")]
    pub console_address: Option<String>,
}
```

### Command Execute Pattern (Pingora Integration)
```rust
impl YourCommand {
    pub fn execute(self) -> anyhow::Result<()> {  // ← SYNC, not async
        // Setup data directory, crypto services, etc.

        // For database connections, create runtime locally
        let rt = tokio::runtime::Runtime::new()?;
        let db = rt.block_on(database_connection())?;

        // Create shutdown signal with resource cleanup and timeout
        let shutdown_signal = Box::new(CtrlCShutdownSignal::new(Duration::from_secs(30)))
            as Box<dyn ProxyShutdownSignal>;

        // Start pingora (which handles its own runtime)
        match setup_pingora_server(db, config, crypto, shutdown_signal) {
            Ok(_) => {
                info!("Server started successfully");
                Ok(()) // Pingora takes over from here
            },
            Err(e) => Err(e)
        }
    }
}
```

**CRITICAL Pingora Rules**:
- Never use `#[tokio::main]` when integrating with pingora
- Command `execute()` methods must be **synchronous**
- Use local tokio runtime only for specific async operations (like DB connections)
- Let pingora handle the main runtime after startup

## Data Directory & Configuration

### Data Directory Pattern
```rust
// Command line argument with env var fallback and default
#[arg(long, env = "TEMPS_DATA_DIR")]
pub data_dir: Option<PathBuf>,

// Get data directory with sensible default
fn get_data_dir(&self) -> anyhow::Result<PathBuf> {
    if let Some(data_dir) = &self.data_dir {
        Ok(data_dir.clone())
    } else {
        // Default to ~/.temps
        let home = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
        Ok(home.join(".temps"))
    }
}
```

### File Creation Pattern
```rust
// Create files only if they don't exist (no overwrite)
let file_path = data_dir.join("filename");
if !file_path.exists() {
    let content = generate_secure_content();
    fs::write(&file_path, content)?;
    debug!("Created {} at {}", "filename", file_path.display());
}
```

## Cryptography Services

Available services from `temps-core`:
- **`CookieCrypto`**: For encrypting/decrypting session tokens and cookie data
- **`EncryptionService`**: For general-purpose application data encryption

### Setup Pattern
```rust
fn setup_encryption_service(&self, data_dir: &PathBuf) -> anyhow::Result<Arc<EncryptionService>> {
    let encryption_key_path = data_dir.join("encryption_key");
    let encryption_key_hex = fs::read_to_string(&encryption_key_path)
        .map_err(|e| anyhow::anyhow!("Failed to read encryption key: {}", e))?;
    let encryption_key_hex = encryption_key_hex.trim();

    let encryption_service = EncryptionService::new(encryption_key_hex)
        .map_err(|e| anyhow::anyhow!("Failed to create encryption service: {}", e))?;

    Ok(Arc::new(encryption_service))
}

fn setup_cookie_crypto(&self, data_dir: &PathBuf) -> anyhow::Result<Arc<CookieCrypto>> {
    let encryption_key_path = data_dir.join("encryption_key");
    let encryption_key_hex = fs::read_to_string(&encryption_key_path)
        .map_err(|e| anyhow::anyhow!("Failed to read encryption key: {}", e))?;
    let encryption_key_hex = encryption_key_hex.trim();

    // Parse hex key to bytes for CookieCrypto
    let key_bytes = hex::decode(encryption_key_hex)
        .map_err(|e| anyhow::anyhow!("Failed to decode encryption key: {}", e))?;

    if key_bytes.len() != 32 {
        return Err(anyhow::anyhow!("Encryption key must be exactly 32 bytes"));
    }

    let mut key_array = [0u8; 32];
    key_array.copy_from_slice(&key_bytes);
    let cookie_crypto = CookieCrypto::new(&key_array);

    Ok(Arc::new(cookie_crypto))
}
```

### Key Generation
```rust
// Generate 32-byte encryption key as hex string
let mut rng = rand::thread_rng();
let key: [u8; 32] = rng.gen();
let hex_key = hex::encode(key);

// Generate auth secret with custom charset
let secret: String = (0..64)
    .map(|_| {
        let charset = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-%";
        charset[rng.gen_range(0..charset.len())] as char
    })
    .collect();
```

## Database

### PostgreSQL with TimescaleDB
- Use Sea-ORM for all operations
- PostgreSQL functions allowed: `NOW()`, `INTERVAL`, `to_char()`, `time_bucket()`, `time_bucket_gapfill()`
- Use `DatabaseBackend::Postgres` for raw queries
- Parameter binding: `$1`, `$2`, etc.
- **ALWAYS cast `time_bucket()` results to `timestamptz`** for proper UTC serialization

### Migrations
```rust
// Use .into() for Expr conversions
.col_expr(Column::UpdatedAt, Expr::current_timestamp().into())
```

## Logging Levels

- **ERROR**: Critical failures (DB connection, auth failures)
- **WARN**: Important but non-critical (rate limits, retries)
- **INFO**: Business events (user actions, deployments, server startup)
- **DEBUG**: Technical details (file creation, service initialization, migrations, bootstrapping)
- **TRACE**: Diagnostic info (SQL queries, request details)

### Logging Guidelines
- Use **DEBUG** for file creation and service initialization messages
- Use **INFO** for important operational milestones (server start, data directory location)
- Always include file paths in debug messages when creating files
- Keep INFO logs focused on business-relevant events

## Environment Variables

Minimal required:
- `TEMPS_ADDRESS` (default: `127.0.0.1:3000`)
- `TEMPS_DATABASE_URL`

Optional:
- `TEMPS_TLS_ADDRESS`
- `TEMPS_CONSOLE_ADDRESS`
- `TEMPS_DATA_DIR` (default: `~/.temps`)
- `TEMPS_LOG_LEVEL`

All environment variables use the `TEMPS_` prefix.

## OpenAPI Documentation

All routes must include OpenAPI definitions using `utoipa`:

```rust
#[utoipa::path(
    post,
    path = "/examples",
    request_body = CreateExampleRequest,
    responses(
        (status = 201, description = "Success response", body = ExampleResponse),
        (status = 400, description = "Bad request"),
        (status = 403, description = "Insufficient permissions")
    ),
    tag = "Examples",
    security(("bearer_auth" = []))
)]
```

Register in `ApiDoc` in `src/admin/server.rs`:
- Add handler function to `paths()`
- Add DTOs to `components(schemas())`

## Quick Reference Checklists

### Service Checklist
- [ ] Check existing services before creating new ones
- [ ] Extend existing services when possible
- [ ] Use `Arc<>` for all services
- [ ] Handle errors with typed enums
- [ ] Never access DB from handlers

### Handler Checklist
- [ ] Use typed request/response structs
- [ ] Add OpenAPI documentation
- [ ] Return proper HTTP status codes
- [ ] Use `permission_check!` for authorization
- [ ] Only call services, never DB
- [ ] Implement pagination & sorting

### Database Checklist
- [ ] Use Sea-ORM for queries
- [ ] Prevent N+1 with JOINs
- [ ] Add indexes for frequently queried fields
- [ ] Use transactions for multi-step operations
- [ ] Implement soft deletes when appropriate
- [ ] Cast `time_bucket()` results to `timestamptz`

## Build Process Rules

**CRITICAL**: Only execute `cargo build` or `cargo check` if you are at least 99% confident that the code will compile successfully.

This means:
- Carefully reviewed all recent code changes for syntax, type, and dependency errors
- Ensured that all imports, modules, and features referenced exist
- Validated that code follows Rust best practices and project architecture
- Confirmed that any new/modified files are properly included in project structure
- Checked that all required dependencies are present in `Cargo.toml`
- Verified that code generation or macro usage is correct

**Rationale**: Building is resource-intensive. Avoid unnecessary builds.

**Exception**: Testing for compilation errors as part of debugging (document the reason).

### Warning-Free Compilation

**All new functionality must compile without warnings.** Before considering any feature complete:

1. Run `cargo check --lib` on affected crates
2. Fix all compiler warnings (unused variables, dead code, deprecated APIs, etc.)
3. Verify no new warnings were introduced

**Common warnings to fix**:
- Unused variables: Add `_` prefix or remove if truly unnecessary
- Unused imports: Remove them
- Dead code: Remove unused functions/structs or add `#[allow(dead_code)]` with justification
- Deprecated APIs: Update to modern alternatives (e.g., Bollard API migrations)
- Missing documentation: Add doc comments for public APIs

**Check warnings with**:
```bash
cargo check --lib -p crate-name 2>&1 | grep "warning"
```

Only acceptable warnings are pre-existing warnings in unrelated crates - never introduce new warnings.

## Testing Requirements

**CRITICAL**: All new functionality must have tests that are **written AND verified to run successfully**. Tests that don't run or that you haven't verified are worthless.

### Testing Checklist for New Functionality

1. **Write Tests** - Create appropriate test coverage
2. **Run Tests** - Execute the tests to verify they work
3. **Verify Success** - Ensure all tests pass
4. **Check Coverage** - Confirm critical paths are tested

### Test Types

#### Unit Tests (No External Dependencies)
Located in the same file as the code being tested using `#[cfg(test)]` modules.

```rust
// In src/services/example_service.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_create_example() {
        // Test logic here
        let result = some_function();
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_validation_error() {
        let result = validate_input("invalid");
        assert!(result.is_err());
    }
}
```

**Run unit tests**:
```bash
# Run all unit tests in a crate
cargo test --lib -p temps-backup

# Run specific test
cargo test --lib -p temps-backup test_create_example

# Run with output
cargo test --lib -p temps-backup -- --nocapture
```

#### Integration Tests (Require External Services)
Located in `tests/` directory, require Docker or other services.

```rust
// In tests/integration/service_test.rs
#[tokio::test]
#[ignore] // Ignored by default, run explicitly
async fn test_integration_with_docker() {
    // Setup test database/services
    let db = setup_test_db().await;

    // Test logic
    let result = service.operation(&db).await;

    assert!(result.is_ok());
}
```

**Run integration tests**:
```bash
# Run integration tests (requires Docker)
cargo test --features integration-tests -p temps-deployments

# Run ignored tests explicitly
cargo test --features integration-tests -- --ignored

# Run specific integration test
cargo test --features integration-tests test_integration_with_docker
```

### Test Verification Process

**MANDATORY STEPS** after writing tests:

1. **Run the test immediately after writing it**:
```bash
cargo test --lib -p your-crate test_your_new_function
```

2. **Verify the output shows the test passed**:
```
running 1 test
test tests::test_your_new_function ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

3. **If the test fails, fix it before proceeding** - Don't commit failing tests

4. **Run all tests in the crate to ensure no regressions**:
```bash
cargo test --lib -p your-crate
```

### Test Structure Pattern

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // Helper function for test setup
    fn setup() -> TestContext {
        TestContext {
            // Test fixtures
        }
    }

    #[tokio::test]
    async fn test_successful_case() {
        // Arrange
        let context = setup();
        let input = create_test_input();

        // Act
        let result = service.method(input).await;

        // Assert
        assert!(result.is_ok());
        let value = result.unwrap();
        assert_eq!(value.field, expected_value);
    }

    #[tokio::test]
    async fn test_error_case() {
        // Arrange
        let context = setup();
        let invalid_input = create_invalid_input();

        // Act
        let result = service.method(invalid_input).await;

        // Assert
        assert!(result.is_err());
        match result.unwrap_err() {
            ServiceError::Validation(msg) => {
                assert!(msg.contains("expected error"));
            }
            _ => panic!("Unexpected error type"),
        }
    }

    #[tokio::test]
    async fn test_edge_case() {
        // Test boundary conditions, empty inputs, etc.
    }
}
```

### What to Test

**Service Methods**:
- ✅ Happy path (successful operations)
- ✅ Error cases (validation failures, not found, etc.)
- ✅ Edge cases (empty input, boundary values)
- ✅ Business logic validation

**Handlers**:
- ✅ Authentication (unauthorized access)
- ✅ Authorization (insufficient permissions)
- ✅ Request validation (invalid input)
- ✅ Success responses
- ✅ Error responses (proper status codes)

**Database Operations**:
- ✅ CRUD operations
- ✅ Queries with filters
- ✅ Pagination
- ✅ Transactions

### Mock Database Pattern

For unit tests that need database access, use Sea-ORM's mock database:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    #[tokio::test]
    async fn test_create_with_mock_db() {
        // Create mock database
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![
                vec![example::Model {
                    id: 1,
                    name: "Test".to_string(),
                    created_at: Utc::now(),
                }],
            ])
            .into_connection();

        let service = ExampleService::new(Arc::new(db));
        let result = service.create("Test").await;

        assert!(result.is_ok());
    }
}
```

### Testing Audit Logs

For handlers with audit logging, verify audit logs are created:

```rust
#[tokio::test]
async fn test_create_with_audit_log() {
    let app_state = setup_test_app_state().await;

    // Perform operation
    let result = create_resource(
        RequireAuth(test_auth()),
        State(app_state.clone()),
        Extension(test_metadata()),
        Json(test_request()),
    ).await;

    assert!(result.is_ok());

    // Verify audit log was created
    let audit_logs = app_state.audit_service.get_recent_logs(1).await.unwrap();
    assert_eq!(audit_logs.len(), 1);
    assert_eq!(audit_logs[0].operation_type, "RESOURCE_CREATED");
}
```

### Common Test Failures

**Test not found**:
- Ensure test function has `#[tokio::test]` or `#[test]` attribute
- Check test module has `#[cfg(test)]` attribute
- Verify test function name starts with `test_`

**Tests compile but don't run**:
- Check if test is marked `#[ignore]` (needs explicit flag to run)
- Verify you're running tests in the correct crate with `-p crate-name`

**Database connection errors in tests**:
- Use mock database for unit tests
- Ensure Docker is running for integration tests
- Check test has `#[ignore]` if it requires external services

### Test Output Validation

Always verify test output looks correct:

```bash
# Good output - tests passing
running 5 tests
test tests::test_create ... ok
test tests::test_update ... ok
test tests::test_delete ... ok
test tests::test_validation_error ... ok
test tests::test_not_found ... ok

test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured

# Bad output - tests failing
running 5 tests
test tests::test_create ... FAILED
test tests::test_update ... ok

failures:
    tests::test_create

# Bad output - tests ignored unexpectedly
running 5 tests
test tests::test_create ... ignored

test result: ok. 4 passed; 0 failed; 1 ignored; 0 measured
```

### Testing Rules

**ALWAYS**:
- Write tests for all new functions and handlers
- Run tests immediately after writing them
- Verify all tests pass before committing
- Test both success and error cases
- Keep tests in same file as code (for services)
- Use descriptive test names (`test_create_with_valid_input`)

**NEVER**:
- Commit code without verifying tests run
- Assume tests work without running them
- Skip testing error cases
- Leave failing tests to "fix later"
- Use production database for tests
- Hardcode sensitive data in tests

### Test Documentation

Document complex test scenarios:

```rust
/// Tests that creating a resource with duplicate name returns validation error
#[tokio::test]
async fn test_create_duplicate_name_returns_validation_error() {
    // Arrange: Create first resource
    let db = setup_test_db().await;
    let service = ExampleService::new(Arc::new(db));
    service.create("duplicate").await.unwrap();

    // Act: Try to create second resource with same name
    let result = service.create("duplicate").await;

    // Assert: Should return validation error
    assert!(matches!(result, Err(ServiceError::Validation(_))));
}
```

# Claude Development Guidelines for Temps UI

## Important Development Rules

### User Feedback
**ALWAYS provide visual feedback for user actions.** Every action that a user takes should have immediate visual feedback:

- **Success actions**: Show a success message/alert when operations complete successfully
- **Error actions**: Show an error message/alert when operations fail
- **Loading states**: Show loading indicators during async operations
- **State changes**: Reflect state changes immediately in the UI

#### Implementation Options:
1. **Toast notifications** (if available)
2. **Alert components** (for important messages)
3. **Inline feedback** (for form validation)
4. **Loading spinners** (for async operations)
5. **Success/error badges** (for status indicators)

#### Examples:
```tsx
// Good - provides feedback
const syncMutation = useMutation({
  onSuccess: () => {
    setFeedback({ type: 'success', message: 'Repositories synced successfully!' })
  },
  onError: (error) => {
    setFeedback({ type: 'error', message: 'Failed to sync repositories. Please try again.' })
  }
})

// Bad - no user feedback
const syncMutation = useMutation({
  onSuccess: () => {
    console.log('Success')
  },
  onError: (error) => {
    console.error('Error', error)
  }
})
```

### Git Provider Integration
- Always check for existing git providers before showing provider selection
- Show timestamps for when providers were connected
- Auto-update project names based on selected repository
- Use card-based UI for selections instead of dropdowns where possible

### Repository Management
- Implement search with debouncing (250-300ms)
- Use server-side filtering when API supports it
- Show visual feedback while searching (spinning icon)
- Display repository metadata (language, stars, forks, last updated)

### Component Reusability
- Create reusable components that work in different contexts (onboarding, settings, etc.)
- Support both inline and dialog/modal modes
- Make components configurable with props

### React Component Best Practices

#### Avoid IFEs (Immediately Invoked Function Expressions) in JSX

**NEVER use auto-callable functions (IFEs) in React components.** Instead, extract logic into separate components, helper functions, or use proper React patterns.

**Why?**
- Reduces code readability and maintainability
- Makes components harder to test
- Hides logic that should be in reusable components or helper functions
- Creates unnecessary complexity in JSX

**Examples:**

```tsx
// ❌ BAD - Using IIFE in JSX
function MyComponent({ headers }) {
  return (
    <div>
      {(() => {
        try {
          const parsed = typeof headers === 'string'
            ? JSON.parse(headers)
            : headers
          return Object.entries(parsed).map(([key, value]) => (
            <div key={key}>
              <span>{key}</span>
              <span>{Array.isArray(value) ? value.join(', ') : String(value)}</span>
            </div>
          ))
        } catch (e) {
          return <p>Failed to parse headers</p>
        }
      })()}
    </div>
  )
}

// ✅ GOOD - Extract to separate component
interface HeadersDisplayProps {
  headers: string | Record<string, unknown> | null | undefined
  emptyMessage?: string
}

function HeadersDisplay({ headers, emptyMessage = 'Failed to parse headers' }: HeadersDisplayProps) {
  if (!headers) return null

  try {
    const parsed = typeof headers === 'string' ? JSON.parse(headers) : headers
    const entries = Object.entries(parsed)

    if (entries.length === 0) {
      return <p className="text-sm text-muted-foreground">No headers available</p>
    }

    return (
      <div className="space-y-3">
        {entries.map(([key, value]) => (
          <div key={key} className="border-b pb-2 last:border-0">
            <div className="flex flex-col space-y-1">
              <span className="text-sm font-medium">{key}</span>
              <span className="text-sm text-muted-foreground font-mono break-all">
                {Array.isArray(value) ? value.join(', ') : String(value)}
              </span>
            </div>
          </div>
        ))}
      </div>
    )
  } catch (_error) {
    return <p className="text-sm text-muted-foreground">{emptyMessage}</p>
  }
}

// Usage
function MyComponent({ headers }) {
  return (
    <div>
      <HeadersDisplay headers={headers} />
    </div>
  )
}
```

```tsx
// ❌ BAD - Complex logic in IIFE
function EventList({ events }) {
  return (
    <div>
      {events.map(event => (
        <div key={event.id}>
          {(() => {
            const data = event.data
            if (isMetaEventData(data)) {
              if (data.href) return data.href
            }
            if (isIncrementalSnapshotData(data)) {
              if (data.source !== undefined) {
                return INCREMENTAL_TYPES[data.source]
              }
            }
            return JSON.stringify(data)
          })()}
        </div>
      ))}
    </div>
  )
}

// ✅ GOOD - Extract to helper function
function formatEventData(event: Event): string {
  const data = event.data

  if (isMetaEventData(data) && data.href) {
    return data.href
  }

  if (isIncrementalSnapshotData(data) && data.source !== undefined) {
    return INCREMENTAL_TYPES[data.source]
  }

  return JSON.stringify(data)
}

function EventList({ events }) {
  return (
    <div>
      {events.map(event => (
        <div key={event.id}>
          {formatEventData(event)}
        </div>
      ))}
    </div>
  )
}
```

**When IFEs are acceptable:**
- Simple type casting that TypeScript requires (but prefer helper functions)
- Very simple transformations that don't warrant a separate function (1-2 lines max)

```tsx
// Acceptable for simple type casting (but still prefer helper functions)
<pre>
  {(() => {
    const data = event.data
    const formatted: string = typeof data === 'object' && data !== null
      ? JSON.stringify(data, null, 2)
      : String(data)
    return formatted
  })()}
</pre>
```

#### Use Mutation/Query States Instead of Manual State Variables

**ALWAYS use the built-in state from React Query mutations and queries** (`isPending`, `isLoading`, `isError`, etc.) instead of managing loading states manually with `useState`.

**Why?**
- Eliminates redundant state management
- Prevents state sync issues
- Reduces boilerplate code
- Automatically tracks the actual operation state
- More reliable and less error-prone

**Examples:**

```tsx
// ❌ BAD - Manual loading state management
function DomainDetail() {
  const [isCompletingDns, setIsCompletingDns] = useState(false)

  const finalizeOrder = useMutation({
    ...finalizeOrderMutation(),
    onSuccess: () => {
      toast.success('DNS challenge verified!')
    },
  })

  const handleCompleteDns = async () => {
    try {
      setIsCompletingDns(true)
      await finalizeOrder.mutateAsync({ path: { domain_id: domain.id } })
    } catch (error) {
      // Error handled
    } finally {
      setIsCompletingDns(false)
    }
  }

  return (
    <Button
      onClick={handleCompleteDns}
      disabled={isCompletingDns}
    >
      {isCompletingDns ? (
        <>
          <Loader2 className="animate-spin" />
          Verifying...
        </>
      ) : (
        'Verify DNS'
      )}
    </Button>
  )
}

// ✅ GOOD - Use mutation's isPending state
function DomainDetail() {
  const finalizeOrder = useMutation({
    ...finalizeOrderMutation(),
    onSuccess: () => {
      toast.success('DNS challenge verified!')
    },
  })

  const handleCompleteDns = async () => {
    try {
      await finalizeOrder.mutateAsync({ path: { domain_id: domain.id } })
    } catch (error) {
      // Error handled in onError
    }
  }

  return (
    <Button
      onClick={handleCompleteDns}
      disabled={finalizeOrder.isPending}
    >
      {finalizeOrder.isPending ? (
        <>
          <Loader2 className="animate-spin" />
          Verifying...
        </>
      ) : (
        'Verify DNS'
      )}
    </Button>
  )
}
```

```tsx
// ❌ BAD - Manual loading state for queries
function ProjectList() {
  const [isLoadingProjects, setIsLoadingProjects] = useState(true)

  const { data: projects } = useQuery({
    ...getProjectsOptions(),
    onSuccess: () => setIsLoadingProjects(false),
    onError: () => setIsLoadingProjects(false),
  })

  if (isLoadingProjects) return <Spinner />

  return <div>{/* render projects */}</div>
}

// ✅ GOOD - Use query's isLoading state
function ProjectList() {
  const { data: projects, isLoading } = useQuery({
    ...getProjectsOptions(),
  })

  if (isLoading) return <Spinner />

  return <div>{/* render projects */}</div>
}
```

**Available states from React Query:**
- **Mutations**: `isPending`, `isSuccess`, `isError`, `error`, `data`
- **Queries**: `isLoading`, `isFetching`, `isError`, `isSuccess`, `error`, `data`

**When to use each:**
- `isPending` (mutations): Operation is currently running
- `isLoading` (queries): Initial data load (no cached data exists)
- `isFetching` (queries): Data is being fetched (may have cached data)
- `isError`: Operation failed, check `error` for details
- `isSuccess`: Operation completed successfully

### UI/UX Guidelines
- Use shadcn/ui components with proper theme colors for light/dark mode
- Prefer wider layouts for complex forms (max-w-7xl instead of max-w-5xl)
- **Layout Rule**: When using max-width constraints (max-w-*), always center the content with `mx-auto` or use full width. Never leave restricted width content aligned to the left
- Use cards for one-click selections instead of two-click dropdowns
- Show loading states for all async operations
- Display proper error boundaries and fallbacks
- **Copy Buttons**: Always use the `CopyButton` component from `@/components/ui/copy-button` for copy-to-clipboard functionality. This component provides built-in state management, tooltips, and visual feedback.
  ```tsx
  import { CopyButton } from '@/components/ui/copy-button'

  // Icon-only button (compact)
  <CopyButton
    value={textToCopy}
    className="h-8 w-8 p-0 hover:bg-accent hover:text-accent-foreground rounded-md"
  />

  // Button with label
  <CopyButton
    value={textToCopy}
    className="h-8 px-3 rounded-md border border-input bg-background hover:bg-accent hover:text-accent-foreground"
  >
    Copy
  </CopyButton>
  ```
  **Never** create custom copy handlers with manual state management and toast notifications - use the `CopyButton` component instead.

### Form Management
- **ALWAYS use React Hook Form with Zod validation** for all forms
- Define a Zod schema for form validation
- Use `useForm` with `zodResolver` for type-safe form handling
- Leverage `react-hook-form`'s built-in state management instead of manual `useState`
- Use default values to set initial form state (including data from API)

#### Example:
```tsx
import { zodResolver } from '@hookform/resolvers/zod'
import { useForm } from 'react-hook-form'
import { z } from 'zod'

const formSchema = z.object({
  branch: z.string().min(1, 'Branch is required'),
  environmentId: z.number({ required_error: 'Environment is required' }),
})

type FormValues = z.infer<typeof formSchema>

function MyComponent() {
  const form = useForm<FormValues>({
    resolver: zodResolver(formSchema),
    defaultValues: {
      branch: data?.main_branch || '',
      environmentId: environments?.[0]?.id,
    },
  })

  const onSubmit = (values: FormValues) => {
    // Handle form submission with validated data
  }

  return (
    <Form {...form}>
      <form onSubmit={form.handleSubmit(onSubmit)}>
        {/* Form fields */}
      </form>
    </Form>
  )
}
```

### API Integration
- Always handle loading, error, and success states
- Use React Query for data fetching and caching
- Implement proper error handling with user-friendly messages
- Invalidate queries after mutations to keep data fresh

## Project Structure
- `/src/components/` - Reusable React components
- `/src/pages/` - Page components
- `/src/api/client/` - Generated API client
- `/src/hooks/` - Custom React hooks
- `/src/contexts/` - React contexts
- `/src/lib/` - Utility functions

## Key Technologies
- React with TypeScript
- Tanstack Query (React Query)
- shadcn/ui components
- Tailwind CSS
- Rsbuild for bundling

## Package Management
- **Use `bun` for package installations** - All packages should be installed using `bun add <package>` instead of npm or yarn
- Example: `bun add <package-name>` for dependencies
- Example: `bun add -D <package-name>` for dev dependencies

## Testing with Playwright

### Local Development Login Credentials
When testing with Playwright on localhost:3000, use these credentials:

- **Email**: `dviejo@kfs.es`
- **Password**: `@vAvQL78%HfL0&vX`

### Example Login Flow
```typescript
// Navigate to login page
await page.goto('http://localhost:3000');

// Fill in credentials
await page.getByRole('textbox', { name: 'Email' }).fill('dviejo@kfs.es');
await page.getByRole('textbox', { name: 'Password' }).fill('@vAvQL78%HfL0&vX');

// Submit login
await page.getByRole('button', { name: 'Sign in' }).click();

// Wait for navigation to dashboard
await page.waitForURL('**/dashboard');
```

**Note**: These credentials are for local development only and should not be used in production environments.
