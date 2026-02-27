# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
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
