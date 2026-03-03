# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
