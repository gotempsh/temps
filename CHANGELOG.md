# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **AI Agents framework**: `temps-agents` crate introducing project-scoped AI agents that run Claude CLI or OpenAI Codex against your codebase; agents are configured per-project with a system prompt, max turns, and a choice of AI provider
- **Autopilot cron scheduling**: agents can be scheduled with a cron expression (e.g. `0 * * * *`) so they run automatically — the `CronScheduler` service manages all active schedules and fires agent runs without manual intervention
- **Autofixer**: agents integrated with error tracking — open an error group and trigger an AI-powered fix that creates a sandboxed agent run, streams the output, and surfaces a diff or summary in the UI
- **Conversation continuation**: Claude CLI backend supports `--continue` to resume an existing conversation in the working directory, enabling multi-turn autopilot sessions that build on prior context
- **Agent run history**: `agent_runs` and `agent_run_logs` tables track every run with status, stdout/stderr output, exit code, and timestamps; browsable from the Autopilot UI
- **Autopilot UI page**: new Autopilot section per project showing run history, run detail with streamed logs, trigger button, and cron schedule configuration
- **Autofixer panel**: dedicated side panel on the Error Group detail page showing autofixer run status and AI-generated fix output
- **Email tracking analytics UI**: event timeline on email detail page showing individual open/click/bounce/delivery events with IP, user-agent, and metadata; new Analytics tab with delivery rate cards and global event log
- **Global email events endpoint**: `GET /emails/events` lists tracking events across all emails with optional `email_id` and `event_type` filters; `GET /emails/events/stats` returns aggregated open/click/bounce rates
- **Node SDK regenerated**: includes `tracked_html_body`, `track_opens`, `track_clicks`, `TrackingEventResponse`, `TrackedLinkResponse`, `EmailTrackingResponse` types and SDK functions
- **Multi-preset detection**: `detect_all_presets_from_files` returns all matching presets per directory (e.g., Dockerfile + Next.js + Docker Compose in the same root), letting users choose their preferred deployment method instead of silently picking the highest-priority match
- **Database pool configuration**: env vars `TEMPS_DB_MAX_CONNECTIONS` (default 100), `TEMPS_DB_MIN_CONNECTIONS` (default 1), `TEMPS_DB_ACQUIRE_TIMEOUT` (default 30s), and `TEMPS_DB_IDLE_TIMEOUT` (default 600s) for tuning the SQLx connection pool on resource-constrained servers
- **Enter-submit in wizards**: `useEnterSubmit` hook added to Domain, DNS Provider, Domain Creation, and Import wizards — pressing Enter advances to the next step or submits on the final step
- Documentation for new pool environment variables in the environment variables reference
- **Email tracking — tracked HTML storage**: new `tracked_html_body` column stores the final HTML sent to the provider (with tracking pixel and rewritten links), separate from the original `html_body` to avoid triggering fake opens in dashboard previews
- **Email tracking — per-link click breakdown**: email detail page now shows each tracked link with its individual click count
- **Link Project from service detail**: "Link Project" button on the Storage service detail page lets you link a project directly from the Linked Projects section via a searchable combobox
- **Template env var generators**: `EnvVarTemplate` gains a `default_generator` field with three client-side generators — `app_url` (`{scheme}://{repo}.{host}[:port]`), `random_hex_32`, and `random_secret`. The `app_url` generator reads the platform's configured `preview_domain` (preferred) or falls back to `external_url`, preserving its scheme and port — so a local dev install at `http://localhost:8080` generates `http://my-app.localhost:8080` instead of `https://my-app.temps.sh`. Auto-fills required vars on mount, recomputes `app_url`-style values when the repository name or platform settings change (only if the user hasn't manually edited the field), and adds a per-field "Generate" button (Sparkles icon). Wired `NEXTAUTH_URL → app_url` and `NEXTAUTH_SECRET → random_hex_32` in the bundled Next.js SaaS Starter

### Security
- **Container exec tenant isolation**: `exec_command` and `container_terminal` handlers now verify the container belongs to the requested project/environment before allowing access, preventing cross-tenant container exec
- **Path traversal protection**: `FilesystemStorage::resolve_path` rejects storage keys containing `..` components, preventing potential directory escape
- Bump `aws-lc-sys` 0.38.0 → 0.39.1 (RUSTSEC-2026-0044, RUSTSEC-2026-0048: X.509 name constraints bypass)
- Bump `rustls-webpki` 0.103.7 → 0.103.10 (RUSTSEC-2026-0049: CRL distribution point matching)
- Bump `tar` 0.4.44 → 0.4.45 (RUSTSEC-2026-0067, RUSTSEC-2026-0068: symlink follow and PAX header issues)
- Bump `rkyv` 0.7.45 → 0.7.46 (RUSTSEC-2026-0001: undefined behavior on OOM)
- Bump `rustls` 0.23.34 → 0.23.37
- Bump `aiohttp` ≥3.11 → ≥3.13.5 in Python SDK (10 CVEs: duplicate Host headers, null bytes, response splitting, cookie leaks, memory DoS, multipart bypass, CRLF injection, DNS cache DoS, trailer headers, SSRF)
- Bump `next` across examples/fixtures to 15.3.3/16.2.2 (5 CVEs: disk cache growth, request smuggling, postponed buffering DoS, null origin CSRF bypass)
- Bump `testcontainers` 0.27.1 → 0.27.2 / `astral-tokio-tar` 0.5.6 → 0.6.0 (insufficient PAX extension validation, dev-only)
- Bump `openssl` 0.10.74 → 0.10.78 (5 advisories: PSK callback memory leak, AES key wrap bounds, MdCtxRef overflow, Deriver short-buffer overflow, PEM password OOB read)
- Bump `rustls-webpki` 0.103.x → 0.103.13 (DoS via panic on malformed CRL BIT STRING)
- Bump `actix-http` 3.11.2 → 3.12.1 (HTTP/1.1 CL.TE request smuggling, dev-only via nixpacks)
- Bump `svelte` to ^5.53.5 in `@temps-sdk/svelte-analytics` dev/peer deps; lock now resolves to 5.55.5 (4 SSR XSS / dynamic-element advisories)
- Pin `protobufjs` ≥7.5.5 via `overrides`/`resolutions` in the `vercel-ai-tracing` example (critical RCE)
- Bump `next` 16.2.2 → 16.2.3 in `examples/nextjs/basic` (DoS with Server Components)

### Changed
- **Audit Logs page redesign**: switched from card-per-row layout to a responsive table matching the rest of the app (filter bar in a card, type badge + icon per category, responsive `hidden md:/lg:table-cell` columns, skeleton loaders, proper empty state, and paginated footer)
- **Audit operation labels**: added human-readable descriptions for the new `SKILL_*`, `MCP_*`, and `SECRET_*` operation types, and a `humanize()` fallback so any future operation type renders as "Something New" instead of "Performed unknown operation"
- **Faster console API startup**: blocking subsystem initialization moved off the boot hot path. The agents sandbox runtime image (Docker Hub pull or local Dockerfile build) now warms up in a background task instead of blocking plugin init for minutes on first boot; `temps-blob` (RustFS) and `temps-kv` (Redis) defer container init+start to background tasks; `temps-workspace` adopts existing sandbox containers in the background instead of doing one Docker inspect per active session inline; `temps-external-plugins` no longer waits up to 30s × N for handshake during boot — it registers an empty proxy router immediately and discovery swaps a populated router in when it finishes (route into `/x/<plugin>/...` 404s until then, identical to "no plugin installed"). Console API now becomes ready in roughly the time it takes to run plugin DI + bind the listener, regardless of how many external services or plugins are configured

### Fixed
- **GitHub repo name conflict returns 409 instead of 500**: creating a project on GitHub with a repo name that already exists on the linked account previously surfaced as an opaque "Internal Server Error". The GitHub provider now detects the `422 name already exists` response and raises a typed `GitProviderError::RepositoryAlreadyExists { name }`, which the handler maps to **409 Conflict** with an actionable detail message. The frontend `extractProblemDetails` helper was also fixed to (1) use the correct RFC 7807 field name `type` (previously `type_url`), (2) unwrap `body` / `error` / `data` envelopes used by the hey-api openapi-ts client, and (3) gate detection on a proper type guard so unrelated thrown objects are no longer misidentified as ProblemDetails. Users now see a clear "Repository Already Exists" toast and can pick a different name without contacting support.
- **`/projects/new` browser navigation**: the source picker (Import / Template / Git URL / Manual) now mirrors the chosen mode to a `?source=` query param, plus the chosen template (`&template=<slug>`) or validated public Git URL (`&repo=<url>`). Browser back/forward, deep links, and tab restore all work — reloading on `?source=templates&template=nextjs-saas-starter` lands directly in the template configurator, and `?source=git-url&repo=https%3A%2F%2F...` re-validates the URL on mount. Sub-keys are cleared automatically when the source changes so we never end up with stale state like `?source=git-url&template=foo`. The inline mode (used inside the onboarding flow) continues to use plain React state since it doesn't own the URL.
- **`app_url` generator inherits port from external_url when preview_domain is set**: a dev install with `preview_domain=*.localho.st` + `external_url=http://localhost:8080` previously produced `https://my-app.localho.st` (missing port, wrong scheme) because `preview_domain` won the priority and didn't carry transport details. Now when both settings are present, scheme and port are inherited from `external_url` while the host comes from `preview_domain` — yielding `http://my-app.localho.st:8080`.
- **Per-provider icons in Git connection pickers**: GitHub, GitLab, and other connections now render the correct icon in both the `/projects/new` source picker and the Template configurator's Git Provider field, instead of always showing the GitHub mark. Resolved by joining `ConnectionResponse.provider_id` against the cached providers list.
- **Adaptive Git Provider picker in Template configurator**: 1 connection auto-selects and renders as a read-only chip; 2-4 connections render as radio cards (easier to scan than a dropdown); 5+ falls back to the existing select.
- **Template preview images and Configurator fallback**: bundled Next.js templates pointed to `raw.githubusercontent.com/gotempsh/temps-examples/.../preview.png` URLs that return 404, and `TemplateConfigurator` had no `onError` fallback so users saw a broken `<img>` icon on the create-project page. Removed the dead URLs from `templates.yaml` and introduced a shared `<TemplateImage>` component used by both `TemplateCard` and `TemplateConfigurator` with a 3-step fallback chain (remote image → local preset icon → generic GitBranch).
- **CLI: `environments vars` subcommands ignored `--project` flag**: `get`, `set`, `delete`, `import`, `export` used `cmd.parent!.parent!.opts().project` (traversing to `environments` command level where `--project` isn't defined) instead of `cmd.parent!.opts().project` (the `vars` command where it is). This caused "Project undefined not found" errors. The `list` subcommand was not affected.
- **CLI: `services env` crashed with "envVars is not iterable"**: the API endpoint `GET /external-services/{id}/projects/{project_id}/environment` returns `HashMap<String, String>` but the CLI expected `Array<EnvironmentVariableInfo>`. Added handling to convert the object response into the expected array format.
- Storage "Create" button from empty state navigated to `/storage/create` (404) instead of `/settings/storage/create`
- Email event timeline returned 404: UI fetched from `/emails/{id}/events` (unregistered plugin route) instead of `/emails/{id}/tracking/events`; also fixed event type mismatches (`open`/`click` vs `opened`/`clicked`) and added client-side pagination for flat array response
- Gmail image proxy misidentified as "Firefox" in email tracking events; now shows "Gmail (Google Proxy)"
- Email detail back button navigated to default Providers tab instead of Sent Emails tab
- Empty headers tab showed blank content instead of empty state message
- Email analytics crashed with `Cannot read properties of undefined (reading 'substring')` due to missing `email_id` in `TrackingEventResponse`
- External URL in platform settings accepted invalid values with `#` or `?` characters; now validated on both client and server
- Email tracking open/click endpoints returned 500 (`RequestMetadata` extension not found) because public routes lacked the middleware; now gracefully falls back to extracting IP/UA from headers
- Email tracking events failed with `column "link_url" does not exist` on databases where the column was missing; added migration to backfill
- Database connections could accumulate without recycling due to missing `idle_timeout` on the connection pool; now defaults to 10 minutes
- `gh release create` failure on duplicate tags: removed invalid `--clobber` flag, then re-added correctly
- Install script and Homebrew formula pointed to `davidviejo/temps` instead of `gotempsh/temps`, causing 404s on binary download

## [0.0.7] - 2026-03-29

### Added

#### Docker Compose Deployments
- Docker Compose as a first-class deployment preset: deploy multi-container apps via git-push with `DownloadRepo → DeployCompose → MarkComplete` pipeline
- Compose override: user-provided YAML merged at deploy time for port remapping, volume overrides, and command changes without modifying the repository compose file
- Public ports model: explicit control over which compose service ports are proxied publicly; each public port gets its own subdomain
- Service-specific custom domain routing: `service_name` column on `project_custom_domains` lets custom domains target a specific compose service (e.g., `api.example.com` → `api:3000`)
- Compose file picker in project creation: filters files by root directory, shows only compose files within the selected subfolder
- Compose Service selector in domain settings UI for docker-compose projects
- Per-service URLs in container list and detail views
- Screenshot capture for Docker Compose deployments
- Temps system environment variables injected into all compose services via auto-generated `docker-compose.temps-env.yml` override
- Volume preservation across redeployments (`docker compose down` without `--volumes`); full cleanup on project/environment deletion

#### Edge CDN Proxy
- `temps edge` CLI command: lightweight, stateless CDN proxy node powered by Pingora — no database required
- Automatic registration with the control plane via `POST /api/internal/nodes/register` with X25519 public key exchange
- Route table sync every 15 seconds from `GET /api/internal/edge/routes`
- ECIES-encrypted TLS certificate delivery: X25519 ECDH + HKDF-SHA256 + AES-256-GCM with forward secrecy (fresh ephemeral keypair per sync)
- Certificates stored in memory only, never written to disk
- Content-addressable local cache with LRU eviction (90% trigger, 80% target, 60s eviction cycle)
- Heartbeat reporting every 30 seconds with cache statistics (hit rate, disk usage, entry count)
- Configurable via CLI flags and environment variables (`TEMPS_ORIGIN_URL`, `TEMPS_EDGE_TOKEN`, etc.)
- Persistent config at `~/.temps/edge.json` (0600 permissions) with node ID and private key
- Region labels for analytics grouping (`--region us-east`)
- SSRF protection for edge node `api_address` validation: blocks loopback, link-local, metadata, and unspecified IPs

#### Content-Addressable Storage
- Static asset caching via SHA-256 content hashing with git-style blob sharding (`blobs/{prefix}/{hash}`)
- DB-backed URL→hash mapping in `static_asset_cache` table for proxy-level asset resolution
- Stale-chunk fallback: old deployment assets remain accessible until GC runs
- Asset cache purge API: `DELETE /projects/{id}/asset-cache` and per-environment variant
- Purge Asset Cache button in environment settings UI
- Nightly garbage collection for unreferenced blobs

#### Container Exec
- One-shot command execution: `POST /projects/{id}/environments/{env_id}/containers/{container_id}/exec`
- Persistent terminal via WebSocket upgrade (xterm.js compatible) with PTY resize support
- Opt-in per project (`container_exec_enabled`), `ContainersExec` permission guard

#### CLI Deploy Commands
- `temps deploy image`: deploy pre-built Docker images from any registry
- `temps deploy static`: deploy static file directories or archives (`.tar.gz`, `.zip`); auto-creates tar.gz from directories
- `temps deploy git`: trigger the build pipeline from a specific commit, branch, or tag
- All three support `--wait` with configurable `--timeout` and 5-second polling
- Authentication via `TEMPS_API_URL` / `TEMPS_API_TOKEN` environment variables

#### Email Tracking
- Email open tracking: 1x1 transparent tracking pixel injected before `</body>` in outgoing HTML emails when `track_opens: true` is set on the send API; pixel hits `GET /api/emails/{id}/track/open` which returns a GIF and records the open event with IP and user-agent
- Email click tracking: all `<a href="http(s)://...">` links in HTML emails are rewritten to route through `GET /api/emails/{id}/track/click/{link_index}` when `track_clicks: true` is set; the endpoint records the click event and 302-redirects to the original URL; `mailto:`, `tel:`, `#anchor`, and `javascript:` links are preserved unchanged
- `email_events` table for granular tracking event storage (event_type, link_url, link_index, ip_address, user_agent) with foreign key cascade to `emails`
- `email_links` table mapping link indices to original URLs with per-link click counts
- `track_opens`, `track_clicks`, `open_count`, `click_count`, `first_opened_at`, `first_clicked_at` columns on the `emails` table
- `TrackingService` in `temps-email` crate: HTML transformation (pixel injection + link rewriting), event recording, counter management, and link/event queries
- Authenticated tracking data endpoints: `GET /api/emails/{id}/tracking` (summary with unique open/click counts), `GET /api/emails/{id}/tracking/events` (filterable by event_type), `GET /api/emails/{id}/tracking/links` (per-link click stats)
- `configure_public_routes()` on the `TempsPlugin` trait for unauthenticated endpoints (tracking pixel and click redirect), served under `/api` without auth middleware
- `track_opens` and `track_clicks` fields on the `POST /api/emails` send API request body (default: false)
- Open/click count columns in the Sent Emails table (frontend) with eye and click icons
- Tracking stats card on the Email Detail page showing open count, click count, and first-event timestamps
- 116 tests: 12 tracking service integration tests, 14 HTTP handler tests (tower::oneshot), including a full E2E flow test (send → open pixel → click redirect → query tracking summary → verify DB state)

#### Health Monitoring
- Health monitors now accept 404/405 as healthy status codes and support custom check paths via `.temps.yaml`
- E2E deployment test workflow for CI/CD validation

#### Other
- Public repo improvements: URL input in Git Settings, "Public" badge, `git_url` and `is_public_repo` API fields
- Authenticated GitHub API calls for public repos (5000 req/hr instead of 60)
- Infrastructure pages consolidated under Settings layout with sidebar navigation
- Command palette (Cmd+K) synchronized with actual routes and settings structure

### Fixed
- **Workflow context clobbering**: parallel jobs overwrote each other's outputs; executor now merges outputs — root cause of containers not registering after deployment
- **Container registration silently skipped**: `persist_static_assets` blocking `mark_deployment_complete`; now runs as non-blocking best-effort
- **Orphaned container teardown**: added slug-based fallback cleanup for containers with no database records
- **SQL injection surface**: ORDER BY identifiers now quoted for CamelCase PostgreSQL column support; static asset cache DELETE parameterized
- **`temps deploy static` runtime panic**: duplicate `-p` short alias between `--path` and `--project` caused clap to panic; removed short alias from `--path`
- **Edge proxy `.unwrap()` calls**: replaced with `?` error propagation in Pingora header insertion methods
- Compose override port parsing: handles both quoted and unquoted port entries
- Public port suggestions use host port (left side of mapping) instead of container port
- GitHub API rate limiting on public repos: all endpoints use authenticated tokens
- TimescaleDB Docker volume path corrected to `/home/postgres/pgdata/data` across all docs
- CPU stats always showing 0.0%: Docker stats API switched from `one_shot` to `stream` mode
- Docker Registry icon changed from Globe to Boxes in Settings
- Email events query now uses deterministic ordering to prevent flaky test results
- Duplicate `email_events` CREATE TABLE migration converted to ALTER TABLE to fix migration errors
- Next.js Docker preset E2E test reliability improvements
- Compose label injection for log collection now uses correct Docker label keys
- Network throughput display now shows actual rate instead of cumulative total
- Container name truncation fixed in monitoring UI
- Removed erroneous `--` from git checkout command (fixes #40)
- `persist_static_assets` now includes Dockerfile preset and skips pull for local images
- Backend presets correctly skip `persist_static_assets` step

### Changed
- `FsFileStore` rewritten as content-addressable store: identical content shares a single blob
- `persist_static_assets` job no longer blocks `mark_deployment_complete`; runs in parallel
- Replaced all `Command::new("git")` CLI calls with `git2` (libgit2); git CLI is no longer a runtime dependency
- Standalone `temps-compose` crate and Stacks UI removed; Docker Compose is now a deployment preset alongside Dockerfile, Next.js, etc.

## [0.0.6] - 2026-03-19

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
- Encryption at rest for environment variables: all values are now encrypted with AES-256-GCM (via `EncryptionService`) before being stored in the database; existing unencrypted rows are transparently decrypted at read time via an `is_encrypted` compatibility flag; the `WorkflowPlanner` decrypts values before injecting them into deployment containers
- Container restart count tracking: container detail API now returns `restart_count` from Docker, surfacing container instability in the UI
- Downstream connection keepalive limit (Pingora 0.8.0): connections are closed after 1024 requests to prevent slow memory leaks from long-lived keep-alive connections
- Upstream write pending time diagnostics (Pingora 0.8.0): `X-Upstream-Write-Pending` response header exposes how long the upstream took to accept the request body; captured in proxy context for observability
- Preview environment flag support in environment variable settings UI
- GenAI OTel tracing: collect and visualize AI conversations from OpenTelemetry `gen_ai.*` spans with support for Vercel AI SDK `ai.*` attribute fallbacks; includes conversation view, token usage aggregation, and tool call detail
- Deployment promotion: promote deployments between environments with environment protection settings (required reviewers, branch restrictions)
- On-demand scale-to-zero environments: environments sleep after configurable idle timeout and wake automatically on incoming HTTP requests via proxy integration
- AI usage analytics: per-model token tracking with agent/session context, BYOK vs platform key breakdown
- Vercel AI SDK tracing examples (Node.js) and Python GenAI tracing examples
- AI tracing documentation page
- Environment password protection: cookie-based password wall for environments with HMAC-signed cookies, argon2 password hashing, and HTML password form served by the proxy; set via environment settings API with automatic cookie invalidation on password change
- Funnel card step pipeline: funnel list cards now show a horizontal pipeline of steps with completions count and conversion rate per step (e.g., `page_view 1,234 → signup 890 (72%)`) alongside the existing summary metrics
- Automatic `CRON_SECRET` injection into deployed containers: the deployment token is now set as `CRON_SECRET` in the container environment on every deployment, and the cron scheduler sends `Authorization: Bearer <CRON_SECRET>` when invoking endpoints — no manual configuration needed
- Analytics overview drill-down filters for property breakdowns: `filter_country`, `filter_region`, `filter_browser`, and `filter_os` query parameters on the `/events/properties/breakdown` endpoint enable hierarchical navigation (country → region → city, browser → version, OS → version)
- Analytics overview charts: Channels, Devices, Languages, Operating Systems, and UTM Campaigns — each with bar visualization and visitor counts
- Drill-down navigation in Browsers, Locations, and Operating Systems charts: click a row to see versions (browsers/OS) or regions/cities (locations) with breadcrumb navigation and back button
- OpenAPI schema propagation for external plugins: plugins can return an OpenAPI schema during handshake, which Temps merges into the unified API docs with `/x/{plugin_name}/` path prefixing
- `utoipa` OpenAPI annotations on all example plugin handlers (SEO Analyzer, Google Indexing, IndexNow, Lighthouse) with typed request/response schemas
- `PropertyBreakdownFilters` struct in `temps-analytics-events` for type-safe drill-down filter propagation through the service layer
- Server-side domain pagination with search: `list_domains` endpoint now accepts `page`, `page_size`, and `search` query parameters, returning `total` count alongside results; default page size is 20, max 100
- Reusable `DomainSelector` combobox component for searching and selecting domains across the app; uses server-side search with debounce, displays domain status badges, and shows "X of Y" overflow hints
- `ProxyLogBatchWriter` for proxy request logging: bounded `mpsc::channel(8192)` with batch INSERT (up to 200 rows per flush, 500ms interval) running on a dedicated OS thread; includes backpressure for HTML responses and graceful shutdown with drain
- Paginated domain management UI with debounced search bar, numbered pagination controls, and mobile-responsive layout
- Structured log aggregator (`temps-log-aggregator` crate): real-time Docker container log collection with automatic container discovery via `sh.temps.*` labels, compressed NDJSON chunk storage (zstd) on filesystem or S3, dual search paths (TimescaleDB index for ERROR/WARN, archive scan for full-text), live tail via Server-Sent Events with project/service/level filtering, automatic retention cleanup with configurable policies, and permission-guarded handlers (`LogsRead`/`LogsDelete`) with audit logging
- Frontend log history viewer with search filters, pagination, and virtualized rendering; accessible via new History tab in project runtime logs page
- OpenTelemetry (OTel) ingest and query system (`temps-otel` crate) with OTLP/protobuf support for traces, metrics, and logs; header-based and path-based ingest routes; `tk_` API key and `dt_` deployment token authentication; `OtelRead`/`OtelWrite` permissions; TimescaleDB storage with hypertables; OpenAPI-documented query endpoints for traces, spans, metrics, and logs; web UI with filterable trace list, waterfall span visualization, and setup instructions
- `deployment_id` field on deployment tokens, allowing OTel ingest to associate telemetry with specific deployments
- `protobuf-compiler` installation in CI workflow for `temps-otel` proto compilation
- External plugin system: standalone binaries in `~/.temps/plugins/` are auto-discovered, spawned, and integrated at boot via stdout JSON handshake (manifest + ready) over Unix domain sockets; Temps reverse-proxies `/api/x/{plugin_name}/*` to each plugin and serves `/api/x/plugins` for manifest listing
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
- GitHub Actions release workflow for Linux AMD64, macOS AMD64, macOS ARM64, and Docker
- Release automation script (`scripts/release.sh`)
- Resource monitoring tab in project sidebar and monitoring settings page with per-environment CPU, memory, and disk metrics
- Browse Data button on linked service cards in the project storage page
- `status_code_class` query parameter (1xx/2xx/3xx/4xx/5xx) for proxy log stats endpoints
- TimescaleDB compression (7-day) and retention (30-day) policies for `proxy_logs` hypertable
- `cargo clippy` pre-commit hook enabled to catch lint issues before CI
- Service clusters: HA PostgreSQL via pg_auto_failover (monitor + primary + N replicas), multi-host connection strings with `target_session_attrs=read-write`, cluster member tracking in `service_members` table
- Remote managed service creation on worker nodes via agent API with auto-assigned ports and Docker volume management
- DNS-based email validation service with SMTP verification
- CLI `temps project create` enhanced with `--repo`, `--branch`, `--directory`, `--preset`, `--connection`, and `--yes` flags for non-interactive CI/scripting usage

### Changed
- Embedded userspace WireGuard via defguard/boringtun: replaced shell-out to `wg` and `ip` CLI with pure Rust implementations (`defguard_wireguard_rs` + `x25519-dalek`); eliminates `wireguard-tools` system package dependency entirely
- `EnvVarService` (in `temps-environments` and `temps-projects`) now requires `Arc<EncryptionService>` in its constructor; plugin registration injects it from the service registry
- Upgraded Pingora from 0.7.0 to 0.8.0; proxy service now uses `ProxyServiceBuilder` instead of `http_proxy_service()` for explicit `HttpServerOptions` configuration
- Security headers are now disabled by default for new installations; existing installations with saved settings are unaffected
- External service containers (Postgres, Redis, MongoDB, S3/MinIO, RustFS) now bind to `0.0.0.0` instead of `127.0.0.1`, making them reachable from worker nodes via the private network; only affects newly created containers
- Cron scheduler now sends `Authorization: Bearer <token>` header alongside `X-Cron-Job: true` when invoking cron endpoints; previously only `X-Cron-Job: true` was sent
- `DatabaseCronConfigService` constructor now requires a `DeploymentTokenService` dependency for retrieving cron secrets
- Locations chart replaced static Country/Region/City tab selector with interactive drill-down: clicking a country shows its regions, clicking a region shows its cities
- Browsers chart now supports click-to-drill into browser versions with back navigation
- `PluginReady` handshake message extended with optional `openapi` field for plugin OpenAPI schemas
- `ExternalPluginProcess` struct extended with `openapi_schema` field
- `ExternalPluginsPlugin` caches OpenAPI schemas at startup for synchronous access during schema merging
- Domain selection throughout the app now uses the `DomainSelector` combobox instead of plain `<Select>` dropdowns, making it possible to find domains when there are many; integrated in `DomainForm`, `AddRoute`, and domain dialogs
- `DomainForm` is now self-contained: fetches wildcard domains internally for initial state matching when editing, removing the `domains` prop dependency from parent components
- All `listDomainsOptions()` call sites now use proper pagination or targeted search queries instead of fetching all domains
- Proxy `LoadBalancer` no longer holds a `request_logger` field or calls synchronous `log_request()` per request; logging is fully delegated to the async batch writer via channel send
- Upgraded Bollard (Docker API client) to 0.20.1 with bollard-stubs 1.52.1; migrated all crates to new API
- `temps-core` no longer depends on `reqwest`, `hyper`, `hyper-util`, `flat2`, or `tar`; these were moved to `temps-external-plugins` or dropped entirely
- `ServiceRegistry` and `PluginStateRegistry` now use `RwLock` instead of `Mutex`, allowing concurrent reads during request handling
- `BackupError` variants converted to structured variants with named fields for richer error messages
- `From<BackupError> for Problem` updated to exhaustive match (no catch-all `_ =>`) with correct HTTP status codes per variant
- Service detail header reorganized: data actions separated from destructive actions with a visual divider
- Vulnerability scanner now uses `--pkg-types library` for image scans and filters out `gobinary`/`rustbinary` result types, reporting only project dependency CVEs

### Removed
- Deleted legacy `web/src/pages/CreateService.tsx` and `CreateServiceRefactored.tsx` (superseded by current service creation flow)

### Fixed
- **Duplicate live visitors**: proxy double-decrypted the visitor cookie — `ensure_visitor_session` decrypted the cookie and passed the plaintext UUID to `get_or_create_visitor`, which tried to decrypt it again; the second decryption always failed silently, causing a new visitor record on every returning page load; now passes the raw encrypted cookie directly
- Static deployment visitor duplication: `ensure_visitor_session` was called for every static file request (JS, CSS, images); concurrent first-visit requests without cookies each created separate visitors; now skips visitor creation for static asset paths
- Proxy returned incorrect `Content-Length` for HEAD responses over HTTP/2, causing clients to wait for a body that never arrives; the header is now stripped for HEAD responses
- Upstream connections could silently fail when reusing stale pooled connections (TCP RST); added explicit connection/read/write/idle timeouts and single automatic retry on connection failure
- Deployment lock contention: replaced PostgreSQL advisory lock with a process-level `tokio::Mutex`, eliminating cross-process lock conflicts and moving container teardown outside the lock scope
- Docker container names are now used instead of Docker network aliases for cross-node environment variable rewriting, fixing service connectivity on remote worker nodes
- Deployment "marking complete" step could hang for the full 60-second timeout when the job queue was busy; the poll now runs on every loop iteration regardless of queue activity
- Remote environment variables are no longer built when no active worker nodes exist, avoiding unnecessary work in single-node deployments
- **Phantom deployments on node drain/failover**: drain and failover previously called `trigger_pipeline` with no branch/tag/commit, creating broken "preview" deployments with empty git context; now uses smart drain logic that retires containers on the draining node when healthy replicas exist on other nodes
- GenAI trace token counts showed as zero: PostgreSQL `SUM(bigint)` returns `numeric` type, causing Sea-ORM `try_get::<Option<i64>>` to silently fail; added `::bigint` cast to all SUM expressions
- Funnel edit page always showed "Funnel Not Found": `EditFunnel` used `useParams()` to read `funnelId`, but no matching route parameter was defined; now parsed from the URL and passed as a numeric prop
- Funnel card metrics never loaded: `formatDateForAPI` produced `yyyy-MM-dd HH:mm:ss` format but the backend expects ISO 8601; changed to `date.toISOString()`
- Proxy memory leak caused by unbounded `tokio::spawn` fire-and-forget INSERT per request; replaced with bounded batch writer that prevents unbounded task growth under high traffic
- Domain list pages no longer silently truncate results when there are more domains than the default page size; all consumers now paginate or use targeted search
- Dockerfile path not saved when changed in project settings; `preset_config` was never sent in the API request
- Fix incorrect `corepack` command used for pnpm in the Next.js preset
- BuildKit build log output now emits vertex names (build step descriptions) in addition to command output, making cached layers visible in deployment logs
- Install script command in documentation now uses `bash` instead of `sh`, fixing failures on Ubuntu 24 where `/bin/sh` is `dash`
- CPU percentage calculation in container stats now uses delta between `cpu_stats` and `precpu_stats` instead of absolute values
- `avg_response_time` cast to `float8` in proxy log time bucket stats for correct type handling

### Security
- Patched critical HTTP Request Smuggling vulnerabilities in `pingora-core` (0.7.0 → 0.8.0)
- Patched high-severity `aws-lc-sys` vulnerabilities: PKCS7 signature validation bypass, certificate chain validation bypass, and AES-CCM timing side-channel (0.32.3 → 0.38.0)
- Patched `jsonwebtoken` type confusion authorization bypass in google-indexing-plugin (9 → 10.3.0)
- Patched `quinn-proto` unauthenticated remote DoS via panic in QUIC transport parameter parsing (0.11.13 → 0.11.14)
- Updated Vercel AI SDK to 5.x to fix file upload whitelist bypass vulnerability
- Updated Flask in example app to 3.1.3 (session cookie fix)

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

[Unreleased]: https://github.com/gotempsh/temps/compare/v0.0.7...HEAD
[0.0.7]: https://github.com/gotempsh/temps/compare/v0.0.6...v0.0.7
[0.0.6]: https://github.com/gotempsh/temps/compare/v0.1.0...v0.0.6
[0.1.0]: https://github.com/gotempsh/temps/releases/tag/v0.1.0
