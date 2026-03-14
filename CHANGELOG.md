# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- GenAI OTel tracing: collect and visualize AI conversations from OpenTelemetry `gen_ai.*` spans with support for Vercel AI SDK `ai.*` attribute fallbacks; includes conversation view, token usage aggregation, and tool call detail
- Deployment promotion: promote deployments between environments with environment protection settings (required reviewers, branch restrictions)
- On-demand scale-to-zero environments: environments sleep after configurable idle timeout and wake automatically on incoming HTTP requests via proxy integration
- AI usage analytics: per-model token tracking with agent/session context, BYOK vs platform key breakdown
- Vercel AI SDK tracing examples (Node.js) and Python GenAI tracing examples
- AI tracing documentation page
- Funnel card step pipeline: funnel list cards now show a horizontal pipeline of steps with completions count and conversion rate per step (e.g., `page_view 1,234 → signup 890 (72%)`) alongside the existing summary metrics

### Fixed
- GenAI trace token counts showed as zero: PostgreSQL `SUM(bigint)` returns `numeric` type, causing Sea-ORM `try_get::<Option<i64>>` to silently fail; added `::bigint` cast to all SUM expressions
- Funnel edit page always showed "Funnel Not Found": `EditFunnel` used `useParams()` to read `funnelId`, but no matching React Router `<Route>` with a `:funnelId` parameter was defined; `funnelId` was always `undefined`, so the funnel lookup always failed; now parsed from the URL and passed as a numeric prop
- Funnel card metrics never loaded (empty cards): `formatDateForAPI` produced `yyyy-MM-dd HH:mm:ss` format but the backend query string parser expects ISO 8601 (`YYYY-MM-DDTHH:MM:SSZ`); deserialization failed on every metrics request; changed to `date.toISOString()`

### Added
- Multi-node cluster support: distribute deployments across a control plane and multiple worker nodes connected via WireGuard private networking
- `temps-agent` crate: worker node agent with Docker runtime, token-based authentication, and deploy/status/stop/logs API endpoints
- `temps-wireguard` crate: WireGuard tunnel management for secure node-to-node networking
- `temps agent` CLI command to start a worker node agent
- `temps join` CLI command to register a worker node with the control plane (direct or relay mode)
- `temps node` CLI subcommand with `list`, `show`, `drain`, and `remove` operations for managing cluster nodes
- `--private-address` flag on `temps serve` to set the control plane's private/WireGuard IP for cross-node service connectivity
- Node scheduler with `LeastLoaded` (default) and `RoundRobin` scheduling strategies; resource-aware replica placement preferring nodes with lowest CPU+memory utilization; configurable max load threshold (default 90%) with graceful fallback
- Remote container deployer via agent HTTP API with health checks and log streaming support
- Cross-node service connectivity: environment variables are rewritten for remote containers so they reach external services (Postgres, Redis, MongoDB, S3, RustFS) on the control plane via private IP and host port instead of Docker container names
- Multi-node-aware route table: proxy resolves worker node private addresses for containers deployed on remote nodes, enabling traffic routing across the cluster with round-robin load balancing
- Node health check job that monitors worker heartbeats and marks stale nodes as offline
- Node drain operation: stops scheduling new containers on a node and migrates existing workloads to other nodes; CLI supports `--wait` with configurable timeout (default 600s)
- Node labels: persist in database, sent with every heartbeat, configurable via `--labels` CLI flag (comma-separated `key=value` pairs)
- Agent `/health` endpoint now returns real system metrics (CPU usage, memory used/total, disk used/total, running container count) via `sysinfo`
- Nodes management page in the web UI under Settings with resource usage visualization, per-node container listing with project/environment context, drain and remove operations with confirmation dialogs
- Database migrations: `nodes` table, `node_id` columns on `deployment_containers` and `deployment_config`, `alarms` table
- Alarm and monitoring system: `AlarmService` with support for container restarts, OOM kills, high CPU/memory, outages, and deployment failures; alarm cooldown mechanism to prevent duplicate rapid-fire alerts; integration with notification and job queue systems via `AlarmFiredJob` and `AlarmResolvedJob`
- `ContainerHealthMonitor`: periodic health checks on all active containers detecting restart count increases, OOM state changes, and resource threshold breaches
- Encryption at rest for environment variables: all values are now encrypted with AES-256-GCM (via `EncryptionService`) before being stored in the database; existing unencrypted rows are transparently decrypted at read time via an `is_encrypted` compatibility flag added in migration `m20260305_000003`; the `WorkflowPlanner` decrypts values before injecting them into deployment containers
- Container restart count tracking: container detail API now returns `restart_count` from Docker, surfacing container instability in the UI
- Downstream connection keepalive limit (Pingora 0.8.0): connections are closed after 1024 requests to prevent slow memory leaks from long-lived keep-alive connections
- Upstream write pending time diagnostics (Pingora 0.8.0): `X-Upstream-Write-Pending` response header exposes how long the upstream took to accept the request body; captured in proxy context for observability
- Preview environment flag support in environment variable settings UI

### Changed
- `EnvVarService` (in `temps-environments` and `temps-projects`) now requires `Arc<EncryptionService>` in its constructor; plugin registration injects it from the service registry
- Upgraded Pingora from 0.7.0 to 0.8.0; proxy service now uses `ProxyServiceBuilder` instead of `http_proxy_service()` for explicit `HttpServerOptions` configuration
- Security headers are now disabled by default for new installations; existing installations with saved settings are unaffected
- External service containers (Postgres, Redis, MongoDB, S3/MinIO, RustFS) now bind to `0.0.0.0` instead of `127.0.0.1`, making them reachable from worker nodes via the private network; only affects newly created containers

### Fixed
- **Duplicate live visitors**: proxy double-decrypted the visitor cookie — `ensure_visitor_session` decrypted the cookie and passed the plaintext UUID to `get_or_create_visitor`, which tried to decrypt it again; the second decryption always failed silently, causing a new visitor record on every returning page load; now passes the raw encrypted cookie directly
- Static deployment visitor duplication: `ensure_visitor_session` was called for every static file request (JS, CSS, images); concurrent first-visit requests without cookies each created separate visitors; now skips visitor creation for static asset paths
- Proxy returned incorrect `Content-Length` for HEAD responses over HTTP/2, causing clients to wait for a body that never arrives; the header is now stripped for HEAD responses
- Upstream connections could silently fail when reusing stale pooled connections (TCP RST); added explicit connection/read/write/idle timeouts and single automatic retry on connection failure
- Deployment lock contention: replaced PostgreSQL advisory lock with a process-level `tokio::Mutex`, eliminating cross-process lock conflicts and moving container teardown outside the lock scope
- Docker container names are now used instead of Docker network aliases for cross-node environment variable rewriting, fixing service connectivity on remote worker nodes
- Deployment "marking complete" step could hang for the full 60-second timeout when the job queue was busy: the DB poll fallback (which confirms the route table update via database query) was only checked when the queue receiver timed out, but a steady stream of unrelated queue events prevented the timeout from ever firing; the poll now runs on every loop iteration regardless of queue activity
- Remote environment variables are no longer built when no active worker nodes exist, avoiding unnecessary work in single-node deployments
- **Phantom deployments on node drain/failover**: drain and failover previously called `trigger_pipeline` with no branch/tag/commit, creating broken "preview" deployments with empty git context; now uses smart drain logic that retires containers on the draining node when healthy replicas exist on other nodes, and only triggers a full redeploy (with correct git context from the latest successful deployment) when all replicas are on the affected node

### Added
- Automatic `CRON_SECRET` injection into deployed containers: the deployment token is now set as `CRON_SECRET` in the container environment on every deployment, and the cron scheduler sends `Authorization: Bearer <CRON_SECRET>` when invoking endpoints — no manual configuration needed
- Analytics overview drill-down filters for property breakdowns: `filter_country`, `filter_region`, `filter_browser`, and `filter_os` query parameters on the `/events/properties/breakdown` endpoint enable hierarchical navigation (country → region → city, browser → version, OS → version)
- Analytics overview charts: Channels, Devices, Languages, Operating Systems, and UTM Campaigns — each with bar visualization and visitor counts
- Drill-down navigation in Browsers, Locations, and Operating Systems charts: click a row to see versions (browsers/OS) or regions/cities (locations) with breadcrumb navigation and back button
- OpenAPI schema propagation for external plugins: plugins can return an OpenAPI schema during handshake, which Temps merges into the unified API docs with `/x/{plugin_name}/` path prefixing
- `utoipa` OpenAPI annotations on all example plugin handlers (SEO Analyzer, Google Indexing, IndexNow, Lighthouse) with typed request/response schemas
- `AGENTS.md` with codebase guidance, critical rules, and a "Feature Development Workflow" checklist requiring documentation updates alongside code changes
- `PropertyBreakdownFilters` struct in `temps-analytics-events` for type-safe drill-down filter propagation through the service layer

### Changed
- Cron scheduler now sends `Authorization: Bearer <token>` header alongside `X-Cron-Job: true` when invoking cron endpoints; previously only `X-Cron-Job: true` was sent
- `DatabaseCronConfigService` constructor now requires a `DeploymentTokenService` dependency for retrieving cron secrets
- Locations chart replaced static Country/Region/City tab selector with interactive drill-down: clicking a country shows its regions, clicking a region shows its cities
- Browsers chart now supports click-to-drill into browser versions with back navigation
- `PluginReady` handshake message extended with optional `openapi` field for plugin OpenAPI schemas
- `ExternalPluginProcess` struct extended with `openapi_schema` field
- `ExternalPluginsPlugin` caches OpenAPI schemas at startup for synchronous access during schema merging
- Fixed `clippy::map_flatten` lint in `temps-plugin-sdk` runtime (`map().flatten()` → `and_then()`)

- Server-side domain pagination with search: `list_domains` endpoint now accepts `page`, `page_size`, and `search` query parameters, returning `total` count alongside results; default page size is 20, max 100
- Reusable `DomainSelector` combobox component for searching and selecting domains across the app; uses server-side search with debounce, displays domain status badges, and shows "X of Y" overflow hints
- `ProxyLogBatchWriter` for proxy request logging: bounded `mpsc::channel(8192)` with batch INSERT (up to 200 rows per flush, 500ms interval) running on a dedicated OS thread; includes backpressure for HTML responses and graceful shutdown with drain
- Paginated domain management UI with debounced search bar, numbered pagination controls, and mobile-responsive layout
- Structured log aggregator (`temps-log-aggregator` crate): real-time Docker container log collection with automatic container discovery via `sh.temps.*` labels, compressed NDJSON chunk storage (zstd) on filesystem or S3, dual search paths (TimescaleDB index for ERROR/WARN, archive scan for full-text), live tail via Server-Sent Events with project/service/level filtering, automatic retention cleanup with configurable policies, and permission-guarded handlers (`LogsRead`/`LogsDelete`) with audit logging
- Frontend log history viewer with search filters, pagination, and virtualized rendering; accessible via new History tab in project runtime logs page
- OpenTelemetry (OTel) ingest and query system (`temps-otel` crate) with OTLP/protobuf support for traces, metrics, and logs; header-based and path-based ingest routes; `tk_` API key and `dt_` deployment token authentication; `OtelRead`/`OtelWrite` permissions; TimescaleDB storage with hypertables; OpenAPI-documented query endpoints for traces, spans, metrics, and logs; web UI with filterable trace list, waterfall span visualization, and setup instructions
- `deployment_id` field on deployment tokens, allowing OTel ingest to associate telemetry with specific deployments
- `protobuf-compiler` installation in CI workflow for `temps-otel` proto compilation
- External plugin system: standalone binaries in `~/.temps/plugins/` are auto-discovered, spawned, and integrated at boot via stdout JSON handshake (manifest + ready) over Unix domain sockets; Temps reverse-proxies `/api/x/{plugin_name}/*` to each plugin and serves `/api/x/plugins` for manifest listing (#19)
- `temps-plugin-sdk` crate for plugin authors: `ExternalPlugin` trait, `main!()` macro, `PluginContext` (direct Postgres access, data dir), `TempsAuth` extractor, and hyper-over-Unix-socket runtime
- `temps-external-plugins` crate following the standard `TempsPlugin` pattern with service layer, utoipa-annotated handler, and OpenAPI schema registration
- Frontend dynamic plugin integration: sidebar nav entries (platform, settings, project-level), command palette search, and generic `PluginPage` component at `/plugins/:pluginName/*` — all driven by plugin manifests
- Example "Cron Jobs" plugin (`examples/example-plugin/`) demonstrating full CRUD API with the SDK
- Graceful shutdown for external plugins via `ExternalPluginsService` in the console API shutdown handler
- PostgreSQL backup now runs `pg_dump` inside a disposable sidecar container (same image as the service) attached to the shared Docker network, eliminating OOM kills (exit code 137) that occurred when `pg_dumpall` was exec'd inside the live service container; TimescaleDB databases are supported via `--format=custom` with advisory circular-FK warnings suppressed
- All preset providers (Next.js, Vite, Rsbuild, Docusaurus v1/v2, NestJS, Angular, Astro, Dockerfile, Nixpacks) are now registered in `PresetProviderRegistry::new()`; Dockerfile and Nixpacks are registered first to take detection precedence
- Proxy now converts HTML responses to Markdown on the fly when clients send `Accept: text/markdown`, compatible with Cloudflare's Markdown for Agents standard; responses include `Content-Type: text/markdown`, `Vary: Accept`, and `X-Markdown-Tokens` headers; SSE, WebSocket, and responses over 2 MB pass through unchanged
- MCP (Model Context Protocol) server with 210 tools across 30 domain modules (`mcp/`)
- OpenAPI SDK auto-generated via `@hey-api/openapi-ts` for MCP server
- WebSocket support for container runtime logs in MCP server
- 103 integration tests for MCP server
- RustFS service logo and improved service type detection in web UI
- Auto-generate `secret_key` for MinIO service creation
- Analytics seed data utilities (`scripts/seed-data/`)
- Web UI build integration via `build.rs`
- Placeholder dist directory for debug builds
- GitHub Actions release workflow for Linux AMD64
- Release automation script (`scripts/release.sh`)
- Comprehensive development and release documentation
- Resource monitoring tab in project sidebar and monitoring settings page with per-environment CPU, memory, and disk metrics
- Browse Data button on linked service cards in the project storage page
- `status_code_class` query parameter (1xx/2xx/3xx/4xx/5xx) for proxy log stats endpoints
- TimescaleDB compression (7-day) and retention (30-day) policies for `proxy_logs` hypertable
- `cargo clippy` pre-commit hook enabled to catch lint issues before CI

### Changed
- Domain selection throughout the app now uses the `DomainSelector` combobox instead of plain `<Select>` dropdowns, making it possible to find domains when there are many; integrated in `DomainForm`, `AddRoute`, and domain dialogs
- `DomainForm` is now self-contained: fetches wildcard domains internally for initial state matching when editing, removing the `domains` prop dependency from parent components (`AddDomainDialog`, `EditDomainDialog`)
- All `listDomainsOptions()` call sites now use proper pagination or targeted search queries instead of fetching all domains: existence checks use `page_size: 1` with `total`, wildcard lookups use `search: '*.'`, and exact matches use the domain name as search term
- Proxy `LoadBalancer` no longer holds a `request_logger` field or calls synchronous `log_request()` per request; logging is fully delegated to the async batch writer via channel send
- Upgraded Bollard (Docker API client) to 0.20.1 with bollard-stubs 1.52.1; migrated all crates to new API (`query_parameters` module, `VolumeCreateRequest`, `exposed_ports` as `Vec<String>`, `error_detail`/`progress_detail` fields, `vertexes` rename)
- `temps-core` no longer depends on `reqwest`, `hyper`, `hyper-util`, `flat2`, or `tar`; these were moved to `temps-external-plugins` or dropped entirely
- `ServiceRegistry` and `PluginStateRegistry` now use `RwLock` instead of `Mutex`, allowing concurrent reads during request handling
- `BackupError::NotFound` and `BackupError::Internal` converted to structured variants with named fields (`resource`, `detail`, `message`) for richer, grep-able error messages; removed `DatabaseConnectionError` and `Operation` variants
- `From<BackupError> for Problem` updated to exhaustive match (no catch-all `_ =>`) with correct HTTP status codes per variant
- CORS middleware helper replaced with a doc comment pointing to `tower_http::cors::CorsLayer`
- Updated `clippy::ptr_arg` warnings to use `&Path` instead of `&PathBuf`
- Fixed `clippy::only_used_in_recursion` warning in workflow executor
- Rewrote CLAUDE.md with comprehensive error handling, resilience, and testing guidance
- Refined service parameter strategies in `temps-providers`
- Service detail header reorganized: data actions (Browse Data, Backup, Edit, Upgrade) separated from destructive actions (Stop/Start, Delete) with a visual divider
- Vulnerability scanner now uses `--pkg-types library` for image scans and filters out `gobinary`/`rustbinary` result types, reporting only project dependency CVEs instead of OS packages or embedded binary vulnerabilities

### Removed
- Deleted legacy `web/src/pages/CreateService.tsx` and `CreateServiceRefactored.tsx` (superseded by current service creation flow)

### Fixed
- Proxy memory leak caused by unbounded `tokio::spawn` fire-and-forget INSERT per request; replaced with bounded batch writer that prevents unbounded task growth under high traffic
- Domain list pages no longer silently truncate results when there are more domains than the default page size; all consumers now paginate or use targeted search
- Dockerfile path not saved when changed in project settings; `preset_config` was never sent in the API request, never persisted by the backend, and the input was misplaced in General Settings instead of Git Settings where the preset selector lives (#26)
- Fix incorrect `corepack` command used for pnpm in the Next.js preset
- BuildKit build log output now emits vertex names (build step descriptions) in addition to command output, making cached layers visible in deployment logs
- Install script command in documentation now uses `bash` instead of `sh`, fixing failures on Ubuntu 24 where `/bin/sh` is `dash` (#15)
- Build failures when web UI is skipped in debug mode
- CPU percentage calculation in container stats now uses delta between `cpu_stats` and `precpu_stats` instead of absolute values
- `avg_response_time` cast to `float8` in proxy log time bucket stats for correct type handling

### Security
- Patched critical HTTP Request Smuggling vulnerabilities in `pingora-core` (0.7.0 → 0.8.0)
- Patched high-severity `aws-lc-sys` vulnerabilities: PKCS7 signature validation bypass, certificate chain validation bypass, and AES-CCM timing side-channel (0.32.3 → 0.38.0)
- Patched `jsonwebtoken` type confusion authorization bypass in google-indexing-plugin (9 → 10.3.0)
- Updated Flask in example app to 3.1.3 (session cookie fix)
- Addressed security audit findings in `temps-cli` skill: removed `curl|sh` pattern, credential path disclosure, and secret-like example tokens

## [0.1.0] - 2024-10-22

### Added
- Initial project structure
- Core architecture with 30+ workspace crates
- Analytics engine with funnels and session replay
- Error tracking (Sentry-compatible)
- Git provider integrations (GitHub, GitLab)
- Deployment orchestration with Docker
- Reverse proxy with automatic TLS/ACME
- Managed services (PostgreSQL, Redis, S3)
- Status page and uptime monitoring
- Web UI built with React and Rsbuild

[Unreleased]: https://github.com/gotempsh/temps/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/gotempsh/temps/releases/tag/v0.1.0
