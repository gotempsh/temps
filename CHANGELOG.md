# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Proxy metrics, dashboard and default alerts**: The Pingora proxy now records per-request metrics in lock-free atomic counters (status classes 1xx–5xx plus a fixed duration-bucket histogram in `temps-proxy/src/metrics.rs`) — a few relaxed atomic adds per request, no locks or I/O on the hot path, constant memory at any request rate. Recording happens once per request in the Pingora `logging()` end-of-request hook, so short-circuited responses (redirects, password walls, ACME challenges, admin-gate denials) are counted too. A background `ProxyMetricsSampler` flushes interval deltas every `monitoring.scrape_interval_secs` to the metrics store (ClickHouse or TimescaleDB, same backend selection as the console) as `SourceKind::Node` points for the control-plane node (`proxy.requests`, `proxy.requests_2xx/3xx/4xx/5xx`, `proxy.requests_project/console/other` (destination split: project route vs console fallback vs proxy-handled — always sums to `proxy.requests`), `proxy.error_rate_percent`, `proxy.request_duration_avg_ms/p50/p95/p99`, and a backend-vs-proxy latency split for proxied requests — `proxy.upstream_duration_*` (upstream connect + processing + TTFB) and `proxy.self_duration_*` (total minus backend, the proxy's own overhead)), readable via the existing `GET /nodes/{id}/metrics` endpoint in both single-process and split proxy/console (ADR-017) topologies. A new "Proxy" page in the web console charts traffic, error rate, and latency percentiles. Migration `m20260708_000001_add_node_id_to_monitoring_alert_rules` adds `node_id` as a third alert-rule target so the `AlertEvaluator` can watch node-scoped metrics; default proxy rules are seeded at startup (error rate >20%, p99 >5s — one rule per metric, matching the (target, metric_name) unique-index semantics).

### Fixed

- **Crypto dependency alignment**: Move direct `hmac` users and workspace `hkdf` onto digest-0.11-compatible releases so the merged `sha2` 0.11 dependency update continues to compile instead of requiring a rollback.
- **Deployment token routes**: Mount `deployment_tokens::configure_routes()` and merge `DeploymentTokensApiDoc` in `DeploymentsPlugin` so deployment-token API paths are reachable at runtime and visible in the generated OpenAPI schema.

### Tests

- **MinIO restore coverage**: Extend `test_s3_backup_and_restore_to_s3` to create 100 MinIO buckets with synthetic files, back them up through the production S3 mirror path, restore into a second service, and verify all restored objects so managed S3 compatibility regressions are caught in the Docker backup suite.
- **MinIO test container cleanup**: Keep the Docker test helper from sweeping active `temps-test-minio-*` containers created by the same test run, because multi-MinIO backup tests need source and destination containers alive at the same time.

## [0.1.0-beta.42] - 2026-07-04

### Added

- **web:** Surface AI invocations inline in trace views ([#185](https://github.com/gotempsh/temps/issues/185))
- **otel:** Label filters and per-series dynamic alerting for metric alerts
- **otel:** Cross-project trace linking (ADR-027) ([#186](https://github.com/gotempsh/temps/issues/186))

### Fixed

- **analytics:** Exclude zero-visitor groups from property breakdown
- **analytics:** Exclude zero-visitor groups from ClickHouse breakdown
- **domains:** Stop ACME TXT records from stacking across renewals ([#182](https://github.com/gotempsh/temps/issues/182))
- **analytics:** Stop fabricating +/-100% dashboard trend when there's no baseline
- **providers:** Harden postgres major upgrades ([#151](https://github.com/gotempsh/temps/issues/151))
- **deployer:** Make cluster DNS injection opt-in (experimental beta) ([#194](https://github.com/gotempsh/temps/issues/194))

## [0.1.0-beta.41] - 2026-07-02

### CI

- **changelog:** Skip preview comment on fork PRs

### Fixed

- **deployer:** Stop temps-dns-resolver being a container DNS SPOF

### Miscellaneous

- **changelog:** Generate CHANGELOG.md with git-cliff instead of hand-editing

## [0.1.0-beta.40] - 2026-07-01

### Added
- **Managed S3 backend driver contract**: `temps-providers` now defines a `ManagedS3Backend` protocol for RustFS-compatible and Garage-compatible object-storage lifecycle operations, keeping `rustfs` as the default while requiring `garage` to be managed by an out-of-process provider over `provider_socket` so AGPL storage engines are not compiled into the Temps binary.
- **MinIO as an S3 backend option**: Managed `s3` services can now select `backend=minio` alongside the default `rustfs` and Garage backend selector, so operators can keep MinIO compatibility without creating a separate current service type.

- **notifications:** Add Cloudflare Email Sending provider ([#160](https://github.com/gotempsh/temps/issues/160))
- **otel:** ClickHouse-first OTEL metrics storage with full-fidelity decode
- **otel:** Temporality-aware rate() and histogram-summary aggregation
- **web:** Wire metrics explorer to Phase C API + all-metrics overview
- **otel:** Metric dashboards v1 (sections + metric tiles)
- **web:** Unify metrics + dashboards into one Metrics surface
- **otel:** Metric alert rules v1 (threshold alerts + evaluator)
- **otel:** V1 metric anomaly detection (robust seasonal MAD band)
- **web:** Make the anomaly alert form legible
- **otel:** Anomaly backtest — "would this have fired?"
- **web:** Shade the anomaly band on the metrics explorer
- **web:** Datadog-style "what's wrong" surface for OTEL metrics
- **web:** Deploy markers on the metrics explorer chart (Tier 2)
- **web:** Cross-signal "what changed" strip on the metrics drill-in (Tier 2)
- **web:** Datadog-style firing status on metric dashboards
- **web:** Show the anomaly band + breach on the metric chart
- **web:** Show the anomaly band on standalone metric tiles
- **web:** Custom date-range filter in the metrics explorer
- **otel:** Embed a Datadog-style chart in metric-alert emails
- **otel:** Add x-axis time ticks to alert email chart
- **otel:** Humanize metric alert notification text (ADR-021 Tier 1)
- **ai:** General AiService foundation for typed/structured output (ADR-022)
- **ai:** Add multi-turn streaming (chat_stream) to AiService (ADR-023)
- **ai:** Ai_conversations + ai_messages schema + project toggle (ADR-023)
- **ai:** Temps-ai-chat crate — conversation service + provider trait (ADR-023)
- **ai:** Deployment debug-chat provider + SSE endpoints + plugin (ADR-023)
- **projects:** Expose ai_alert_summaries_enabled + ai_debug_chat_enabled in project API (ADR-021/023)
- **web:** AI Assistance settings card — debug chat + alert summary toggles (ADR-021/023)
- **web:** DeploymentDebugChat — streaming AI debug chat on failed deployments (ADR-023)
- **ai:** Log-tail enrichment + alert investigation provider (ADR-023)
- **web:** Generalize DebugChat + mount on alert form (ADR-023)
- **ai:** Agentic repo tool-calling, live context refresh & cross-project chat list (ADR-023)
- **web:** AI Providers settings + persistent cross-project assistant dock (ADR-022/023)
- **web:** Make the AI assistant button global in the top bar (ADR-023)
- **ai:** Stream tool calls/results to the chat UI + project favicons in the switcher (ADR-023)
- **ai:** Persist tool calls for reload + chat timestamps + icon-only back button (ADR-023)
- **observe:** Show only root spans in the feed and search all spans by name
- **observability:** Storage-agnostic TraceReader (temps-core trait + otel impl)
- **ai-chat:** Project-level chats, AI trace tools, and write-path security
- **otel:** Full-fidelity TimescaleDB metrics storage at ClickHouse parity
- **ai:** Streaming agentic chat + virtual CLI tool, trace UI, alarms, mobile fixes
- **multi-node:** Harden enrollment, secrets, proxy + mTLS PKI keystone
- **multi-node:** Node enrollment tokens + registration rate limiting
- **multi-node:** MTLS cert provisioning — cluster CA + CSR signing (WS-2.1)
- **multi-node:** MTLS transport — agent TLS serving + CP client (WS-2.1)
- **multi-node:** Deploy over mTLS — wire CP client into deploy path + IP SAN fix (WS-2.1)
- **multi-node:** Route node drain through the mTLS deployer factory (WS-2.1)
- **multi-node:** MTLS for remote log streaming + shared client factory (WS-2.1)
- **multi-node:** MTLS for the terminal WebSocket proxy (WS-2.1)
- **multi-node:** Reject edge-route pulls from non-active nodes (WS-3.4, netiso-6)
- **deployments:** Inject node identity env vars into every container
- **nodes:** Surface the control plane as a node (id 0) in the admin API
- **nodes:** Alert operators when a worker node goes offline
- **nodes:** Alert operators when a worker node recovers (back online)
- **nodes:** Resource + responsiveness alerts for worker nodes
- **nodes:** Configurable resource-alert thresholds; defer latency anomaly
- **logs:** Remote-node container logs in searchable history with container/node filters
- **logs:** "All containers" live mode — merge per-container streams with source
- **monitoring:** Control-plane self-metrics (CPU/mem/disk) + alerts
- **dns:** Control-plane DNS resolver for single-node service discovery (ADR-024)
- **ai-api-tools:** Add vetted write index + prepare/execute path
- **entities,migrations:** Ai_pending_actions table + per-project write toggle
- **ai-chat:** Propose-then-confirm write actions (service+endpoints+tool)
- **serve:** Wire AI write tool with curated mutation allowlist
- **web,projects:** Confirm/reject card + per-project write-actions toggle
- **web:** Enable read-only AI chat inline from the chat surfaces
- **web:** Enable AI write actions from the chat (confirm-gated)
- **web:** Label temps_write tool cards with the command, like temps
- **web:** Show the redacted request params on the proposed-action card
- **git:** Add list_directory to GitProviderService (GitHub + GitLab)
- **ai-chat:** General repo-explore tools in every chat (git-connected)
- **ai:** AI write tools (propose-then-confirm) + chained plans, manual deploys, deploy fixes
- **projects:** Add project source-type change endpoint
- **web:** Deployment source card and docker/static deploy UI
- **git:** Add Gitea, Bitbucket, and Generic git providers ([#177](https://github.com/gotempsh/temps/issues/177))

### Documentation

- Add local build prerequisites ([#148](https://github.com/gotempsh/temps/issues/148))
- **changelog:** Add Unreleased entry for OpenTelemetry metrics ([#158](https://github.com/gotempsh/temps/issues/158))
- **adr:** ADR-021 humanized alert notification text
- **adr:** ADR-022 general AI foundation for typed/structured output
- **adr:** ADR-023 persistent AI debugging conversations
- **changelog:** Add full-fidelity TimescaleDB metrics entry ([#173](https://github.com/gotempsh/temps/issues/173))
- **adr:** Add ADR-020 multi-node deployment hardening + audit
- **adr-020:** Reassess WS-3 against the Kubernetes routing model
- **changelog:** Multi-node hardening + worker monitoring
- **examples:** Add echo-server example + dev-cluster multi-node deploy
- **deployer:** Note the deliberate get_service for CP DNS bootstrap
- **changelog:** Add OTel project-scope + AI tool-discovery security entries
- **changelog:** Document streaming AI chat + temps API tool, trace detail, OTLP log fixes
- **changelog:** AI propose-then-confirm write actions

### Fixed

- **web:** Wrap long values in trace span detail panel ([#159](https://github.com/gotempsh/temps/issues/159))
- **auth:** Constrain deployment token permissions
- **auth:** Preserve email access and enforce project scope for deployment tokens
- **otel:** Temporality-correct histogram_summary (no cumulative double-count)
- **web:** Edit form dropped changed Select values (aggregation, detection)
- **web:** Keep the project header on one line for long titles
- **otel:** Qualify service_name in trace WHERE to dodge alias-shadow
- **ai:** Default model fallback so a provider key alone enables AI (ADR-022)
- **ai:** PR #158 review — tenant-scope guards, traversal/UTF-8 hardening, typing-bubble + tool-loop tests (ADR-023)
- **ai:** PR #158 review — medium findings (ADR-022/023)
- **cli:** Cancel the running migration backend on Ctrl+C
- **otel:** Scope-guard metric label endpoints + 400 on bad label key
- **multi-node:** Address security-auditor findings (enrollment/mTLS hardening)
- **logs:** History filter dropdowns list all sources, not just the current selection
- **logs:** Chronologically interleave the "All containers" live stream
- **mtls:** Server-authoritative cert SANs + close #162 review gaps
- **security:** Patch reachable dependency advisories (postgres, quic, aiohttp)
- **notifications:** Disambiguate update_email_provider operationId ([#163](https://github.com/gotempsh/temps/issues/163))
- **env-vars,dns:** 409 on duplicate env var + idempotent DNS publish
- **dns:** Forward NODATA as NOERROR, not NXDOMAIN
- **dns:** IPv4-only gateway selection + feeder update-propagation test
- **otel,ai:** Scope-guard OTel query handlers, permission-aware AI discovery, clippy
- **edge:** Stop forwarding edge token on asset misses ([#169](https://github.com/gotempsh/temps/issues/169))
- **deployments:** Enforce container exec scope ([#166](https://github.com/gotempsh/temps/issues/166))
- **ai-chat,serve:** Harden propose-then-confirm per security review
- **ai-chat:** Write-actions-on must not block the read-only chat
- **ai-write:** Add redeploy op, human-readable proposals, better op guidance
- **ai-write:** Show the full write-op catalog so redeploy is discoverable
- **ai-chat,web:** Sidebar visibility + redeploy env + write-op diagnostic
- **web:** Mobile horizontal scroll in deployment job logs

### Miscellaneous

- **web:** Regenerate SDK for AI chat routes + project AI toggles (ADR-023)
- **dev-cluster:** Add cluster-formation e2e trace script
- **examples:** Add OpenTelemetry + error-tracking multi-node demo
- **web:** Regenerate OpenAPI SDK after merging main
- **web/sdk:** Add ai_write_actions_enabled to project SDK types
- **web/sdk:** Regenerate OpenAPI client for AI write actions
- **web:** Regenerate OpenAPI SDK

### Performance

- **proxy:** Cache IP block-list and geolocation to cut per-request DB load ([#174](https://github.com/gotempsh/temps/issues/174))

### Refactor

- **otel:** Forward-compatible detector schema for metric alert rules
- **web:** Group project nav into OpenTelemetry + Monitoring
- **ai:** Extract reusable temps-ai crate with schemas + diagnostics

### Styling

- **analytics-events:** Cargo fmt
- **web:** Full-width metrics + dashboards pages

### Testing

- **otel:** Live-ClickHouse round-trip for exp-histogram/summary/exemplar columns
- **multi-node:** Real container deploy over mTLS (gated live test)
- **dev-cluster:** From-scratch multi-node e2e harness (mTLS join + deploy)
- **providers:** Isolate lifecycle tests' ports and container names ([#171](https://github.com/gotempsh/temps/issues/171))

## [0.1.0-beta.39] - 2026-06-25

### Added

- **temps-cli:** Per-instance default project, static deploy in `up`, full build logs ([#154](https://github.com/gotempsh/temps/issues/154))
- **deployments:** Rebuild from source on rollback for git projects ([#155](https://github.com/gotempsh/temps/issues/155))
- **templates:** One-click demo deploy for activation (no Git account) ([#157](https://github.com/gotempsh/temps/issues/157))

### Documentation

- **agents:** Document changelog CI gate and skip-changelog label ([#156](https://github.com/gotempsh/temps/issues/156))

### Fixed

- **ci:** Serialize heavy provider backup tests + dedupe port selection ([#152](https://github.com/gotempsh/temps/issues/152))

## [0.1.0-beta.38] - 2026-06-23

### Added

- **tls:** On-demand certs for the console host + sslip.io auto-enable ([#147](https://github.com/gotempsh/temps/issues/147))

## [0.1.0-beta.35] - 2026-06-19

### Added

- **tls:** On-demand HTTP-01 issuance (ADR-018) + renewal-safety fixes ([#137](https://github.com/gotempsh/temps/issues/137))
- **monitoring:** Enable monitoring by default for new services + DNS/domain UI polish ([#142](https://github.com/gotempsh/temps/issues/142))
- **sdk:** Add @temps-sdk/api generated OpenAPI client package ([#143](https://github.com/gotempsh/temps/issues/143))

## [0.1.0-beta.34] - 2026-06-17

### Added

- Anonymous telemetry + Postgres shm_size_mb + custom health-check path ([#135](https://github.com/gotempsh/temps/issues/135))

## [0.1.0-beta.33] - 2026-06-17

### Added

- **serve:** Split proxy and console into independent processes (ADR-017 Phase 1)
- **proxy:** Wire cross-process on-demand wake into `temps proxy` (ADR-017 Phase 2)
- **serve:** Version-skew detection + `temps upgrade --split` guidance (ADR-017 Phase 3)
- **environments:** Per-environment attack_mode override

### Documentation

- **adr-017:** Design Phase 4 — zero-downtime proxy restarts via Pingora graceful upgrade

### Fixed

- **deployments:** Deploy environments uncapped by default, limits opt-in ([#132](https://github.com/gotempsh/temps/issues/132))
- **monitoring:** Fire CPU alarms relative to the container CPU limit
- **git:** SSRF guard + token-drop on GitLab archive redirects

## [0.1.0-beta.31] - 2026-06-11

### Fixed

- **proxy:** Probe app readiness (TCP) before completing on-demand wake ([#128](https://github.com/gotempsh/temps/issues/128))
- **proxy:** Filter on-demand wake/sleep by node_id + stop leaking detail in 503 bodies
- **logs:** Filter history logs by deployment id (uuid → i32) ([#131](https://github.com/gotempsh/temps/issues/131))
- **web:** Improve runtime History log filter layout and multi-day timestamps ([#130](https://github.com/gotempsh/temps/issues/130))

## [0.1.0-beta.30] - 2026-06-10

### Fixed

- **proxy:** Wake on-demand environments via in-process ForceRouteReload to fix first-request 503 ([#124](https://github.com/gotempsh/temps/issues/124))

## [0.1.0-beta.29] - 2026-06-09

### Added

- **deployments:** Capture & view logs of previous deployments + security hardening ([#123](https://github.com/gotempsh/temps/issues/123))

### Fixed

- **domains:** Show actionable card for unclassifiable ACME challenges

## [0.1.0-beta.28] - 2026-06-08

### Fixed

- **security:** Bump vitest to 4.1.0, aiohttp to 3.14.0, pingora to 0.8.1 ([#120](https://github.com/gotempsh/temps/issues/120))
- **git:** Ignore null-SHA push events to prevent failed 0000000 deployments ([#121](https://github.com/gotempsh/temps/issues/121))
- **serve:** Remove duplicate backup scheduler + feat(console-kit): headerActions slot ([#122](https://github.com/gotempsh/temps/issues/122))

## [0.1.0-beta.27] - 2026-06-05

### Added

- **onboarding:** Setup_complete flag, wizard bypass, per-domain HTTPS, activation checklist
- **onboarding:** Improve getting-started checklist UX
- **proxy:** Add AI-agent pages endpoint to proxy logs

### Documentation

- **skills:** Refresh add-custom-domain and platform-setup skills
- **changelog:** Record metrics-store mismatch fix, on-demand sandbox build, and related changes

### Fixed

- **projects:** Send null instead of empty string for git_url when using PAT provider
- **projects:** Send null instead of empty string for git_url in all create-project paths
- **settings:** Surface effective metrics store and warn on ClickHouse mismatch
- **agents:** Build sandbox image on demand only, never at startup
- **domains:** Renew HTTP-01 certificates via the order-based ACME flow
- **deployments:** Use project slug for remote deployment hostnames
- **web:** Console UI polish across palette, activity graph, log viewer, date picker
- **agents:** Use ghcr.io/gotempsh/temps-preview-gateway image
- **deployments:** Pass private registry credentials when pulling external images

### Miscellaneous

- Migrate CLI dry-run/confirm/live-progress + chart tooltip fix

## [0.1.0-beta.26] - 2026-06-03

### Added

- **analytics:** Add `temps backfill clickhouse` standalone migration subcommand ([#109](https://github.com/gotempsh/temps/issues/109))
- **metrics:** Unified database observability — service metrics, OTLP ingest, monitoring UI ([#108](https://github.com/gotempsh/temps/issues/108))
- **analytics:** AI agents overview — timeline chart, breakdown cards, status ([#113](https://github.com/gotempsh/temps/issues/113))
- **logs:** Grep -C surrounding lines in log search + trace duration sort ([#114](https://github.com/gotempsh/temps/issues/114))
- **clickhouse:** Opt-in CH telemetry backends + TimescaleDB trace summaries, backfill, migrate reporting & data-model hardening ([#116](https://github.com/gotempsh/temps/issues/116))

### Documentation

- **skills:** Fix HIGH-risk security findings in temps-platform-setup skill ([#110](https://github.com/gotempsh/temps/issues/110))

### Fixed

- **domains:** Recover stuck TLS orders and fix HTTP-01 auto-renewal ([#111](https://github.com/gotempsh/temps/issues/111))

## [0.1.0-beta.25] - 2026-05-31

### Added

- **analytics:** Per-project AI Crawler activity feed ([#107](https://github.com/gotempsh/temps/issues/107))

## [0.1.0-beta.24] - 2026-05-30

### Added

- **cli:** TEMPS_CONTEXT env override + correct api-key login server
- **analytics:** AI agent traffic analytics + proxy-log filtering
- **analytics:** Per-agent breakdown when expanding a crawled page
- **analytics:** AI agents detail redesign + CLI ai-agents commands
- **containers:** Live log levels + pause/resume and credential masking
- **migrations:** Backfill proxy_logs.bot_name with canonical AI-agent names

### Documentation

- **changelog:** Record fast LB bind + TEMPS_CONTEXT CLI changes

### Fixed

- **proxy:** Run AI-agent detection on the live proxy-log ingest path

### Performance

- **proxy:** Bind the load balancer before loading routes

### Refactor

- **migrations:** Make AI-agent bot_name backfill a manual script

## [0.1.0-beta.23] - 2026-05-29

### Added

- **auth:** Working password reset via email-only delivery ([#102](https://github.com/gotempsh/temps/issues/102))

## [0.1.0-beta.22] - 2026-05-27

### Added

- **git:** Vercel-style sticky PR/MR preview comments on deploys ([#96](https://github.com/gotempsh/temps/issues/96))
- **proxy:** Branded 404 for unknown hosts behind admin gate ([#97](https://github.com/gotempsh/temps/issues/97))
- **projects:** On-demand preview environments by default
- **deployer:** Control-plane build concurrency + per-build resource caps
- **auth:** Per-OIDC-provider trust_idp_email opt-out for email_verified gate
- **email:** Generic SMTP provider, edit endpoint, and branded health-check email ([#101](https://github.com/gotempsh/temps/issues/101))

### Documentation

- **changelog:** Record the on-demand previews / cancel comment / build limits work

### Fixed

- **security:** 0.1.0 hardening pass (29 findings, 3 rounds) ([#95](https://github.com/gotempsh/temps/issues/95))
- **security:** Close 2 remaining 0.1.0 release blockers ([#98](https://github.com/gotempsh/temps/issues/98))
- **git:** Update PR preview comment on deployment cancel
- **web:** GitLab logo + cleaner delete-provider toast

### Miscellaneous

- Bump version, serialize docker tests, dns updates

## [0.1.0-beta.21] - 2026-05-24

### Added

- **auth:** OIDC SSO + Keycloak dev tooling + workflow trigger fix ([#93](https://github.com/gotempsh/temps/issues/93))

## [0.1.0-beta.20] - 2026-05-21

### Added

- **web:** Change platform logo and favicon to the "t" lettermark
- **notifications:** Real data aggregation for weekly digest
- **email:** Native email validation, drop check-if-email-exists
- **ai-gateway:** Paginate and filter recent requests usage log

### Fixed

- **notifications:** Rebuild weekly digest email with table-based layout
- **otel:** Report the configured rate limit in RateLimitExceeded
- **deps:** Bump idna to 3.15 in Python SDK (CVE-2024-3651 bypass)
- **import-docker:** Import RestartPolicyNameEnum from bollard::models
- **deps:** Upgrade hickory-dns to 0.26.1 (DNS CVEs)
- **dns:** Migrate temps-dns-resolver test files to hickory 0.26

### Miscellaneous

- Remove unused temps-mcp crate (drops rmcp CVE)

### Refactor

- **proxy:** Remove dead RequestLogger code path

### Styling

- **deployments:** Cargo fmt routing-inputs block

### Testing

- **proxy:** Fix visitor/session tests to create real DB rows

## [0.1.0-beta.9] - 2026-05-11

### Fixed

- **sandbox:** Bake safe.directory into image and surface chown failures ([#83](https://github.com/gotempsh/temps/issues/83))
- **workspace:** Chown bind-mount work_dir to sandbox uid on the host ([#84](https://github.com/gotempsh/temps/issues/84))

## [0.1.0-beta.8] - 2026-05-10

### Fixed

- **ci:** Set automatic_deploy=true in E2E project creation ([#80](https://github.com/gotempsh/temps/issues/80))
- **deployments:** Always trigger initial deployment for new environments ([#81](https://github.com/gotempsh/temps/issues/81))
- **ci:** Wait for deployment 'completed' state before verifying app ([#82](https://github.com/gotempsh/temps/issues/82))

### Testing

- **observability:** Add hide_bots: None to EventFilters constructors in merge_integration tests ([#79](https://github.com/gotempsh/temps/issues/79))

## [0.1.0-beta.7] - 2026-05-09

### Added

- **workspace,sandbox:** V0.0.8 security audit + sandbox path centralization + CLI auto-auth ([#73](https://github.com/gotempsh/temps/issues/73))
- **cli:** Release channels for temps upgrade and install.sh ([#74](https://github.com/gotempsh/temps/issues/74))
- **providers:** Resource limits + runtime panel for external services ([#75](https://github.com/gotempsh/temps/issues/75))
- **analytics:** Runtime-toggleable ClickHouse backend ([#76](https://github.com/gotempsh/temps/issues/76))
- **workspace,deployer:** Preview password encryption + reboot-safe Docker secrets path
- **auth:** In-app password change with MFA gate and other-session revocation
- Misc improvements across git credential, sandbox, workspace, and log viewers

### Documentation

- **changelog:** Note runtime logs overhaul + storage UI polish

### Fixed

- **auth:** Preserve session cookie when clearing mfa_session after verify ([#77](https://github.com/gotempsh/temps/issues/77))
- **logs,storage:** Runtime logs overhaul (pagination, ordering, density) + storage UI polish
- **logs:** Satisfy clippy::unnecessary_sort_by on chunk + result reorder
- **web:** Projects.tsx tweak
- Respect automatic_deploy flag on git push + Observe theme tokens

## [0.1.0-beta.6] - 2026-05-03

### Added

- **cli:** Credential-based login (temps login + logout + whoami + context) ([#69](https://github.com/gotempsh/temps/issues/69))
- **dns,providers:** Internal DNS for HA databases + full cluster lifecycle (provision · scale · promote · backup · restore) ([#66](https://github.com/gotempsh/temps/issues/66))
- **observability:** Unified Observe page (cockpit + console) ([#71](https://github.com/gotempsh/temps/issues/71))

### Fixed

- **deps:** Patch 14 dependabot security advisories ([#61](https://github.com/gotempsh/temps/issues/61))
- **templates:** Image fallbacks and env var generators ([#63](https://github.com/gotempsh/temps/issues/63))
- **git:** Return 409 when GitHub repo name already exists ([#64](https://github.com/gotempsh/temps/issues/64))
- **domains:** Show renew action for ACME certificates with non-standard verification_method ([#65](https://github.com/gotempsh/temps/issues/65))
- Keep deployment log stream connected on redeploy and harden project creation ([#68](https://github.com/gotempsh/temps/issues/68))
- **operational:** Branch picker, container limits + kill reason, GitLab token refresh ([#70](https://github.com/gotempsh/temps/issues/70))
- **sandbox:** Pin image version, chown /workspace, GHCR channel split ([#72](https://github.com/gotempsh/temps/issues/72))

### Performance

- **startup:** Defer blocking subsystem init off the console boot path ([#67](https://github.com/gotempsh/temps/issues/67))

## [0.1.0-beta.3] - 2026-04-25

### Added

- **email-tracking:** Add event timeline UI and analytics dashboard ([#53](https://github.com/gotempsh/temps/issues/53))
- **agents:** AI autopilot agents framework with cron scheduling and autofixer ([#58](https://github.com/gotempsh/temps/issues/58))

### Fixed

- Configurable db pool, multi-preset detection, enter-submit wizards ([#52](https://github.com/gotempsh/temps/issues/52))
- **security:** Resolve dependency vulns and fix container exec tenant isolation ([#54](https://github.com/gotempsh/temps/issues/54))
- **email:** Fix event timeline 404 and event type mismatches ([#56](https://github.com/gotempsh/temps/issues/56))
- **email:** Email tracking fixes, analytics endpoints, SDK regen, settings validation ([#57](https://github.com/gotempsh/temps/issues/57))
- **deps:** Resolve all 18 dependabot vulnerabilities ([#60](https://github.com/gotempsh/temps/issues/60))
- **cli:** Fix project resolution in env vars subcommands and services env response ([#59](https://github.com/gotempsh/temps/issues/59))

## [0.0.8] - 2026-03-30

### Fixed

- **ci:** Add --clobber to gh release create to prevent duplicate tag failures ([#51](https://github.com/gotempsh/temps/issues/51))

## [0.0.7] - 2026-03-29

### Added

- **ci:** Add E2E deployment tests workflow
- **email:** Add open and click tracking for transactional emails

### Fixed

- Remove erroneous -- from git checkout command ([#40](https://github.com/gotempsh/temps/issues/40))
- **migrations:** Convert duplicate email_events CREATE to ALTER TABLE ([#45](https://github.com/gotempsh/temps/issues/45))
- **email:** Add deterministic ordering to get_events query ([#46](https://github.com/gotempsh/temps/issues/46))
- **presets:** Fix Next.js Docker e2e test reliability ([#47](https://github.com/gotempsh/temps/issues/47))
- **ci:** Generate self-signed certificate for E2E tests ([#48](https://github.com/gotempsh/temps/issues/48))
- **ci:** Remove duplicate trigger-pipeline call causing vite-e2e cancellation ([#49](https://github.com/gotempsh/temps/issues/49))

### Miscellaneous

- **release:** Finalize 0.0.7 changelog with missing fixes and updated date

## [0.0.7-beta1] - 2026-03-28

### Added

- **compose:** Add Docker Compose stack management
- **compose:** Add domain routing, repo-backed stacks, and replace git CLI with git2
- **compose:** Add repo compose file discovery and sync UI
- **compose:** Add port conflict validation before deploy
- **compose:** Add delete button to stack detail view
- **compose:** Add port overrides for compose stack port remapping
- **compose:** Add branch listing, fix port parser, improve create UX
- **compose:** Add docker-compose preset and service_name column (ADR-007)
- **deployer:** Add ComposeExecutor for docker compose deployments
- **compose:** Wire docker-compose into deployment pipeline
- **compose:** Wire DeployComposeJob into workflow execution service
- **compose:** Add teardown, proxy routing, and UI preset support
- **compose:** Inject Temps system env vars into all compose containers
- **compose:** Preserve volumes, add build support, expose service_name
- **containers:** Add exec and persistent terminal (xterm.js WebSocket)
- **compose:** Add user-provided compose override, PWD fix, error logging, env-file loading
- **ui:** Add public repo preset detection and compose override in git settings
- **compose:** Public repo improvements and session replay fixes
- **file-store:** Implement content-addressable storage with SHA-256
- **deploy:** Re-enable persist_static_assets with CAS backend
- **cas:** Database-backed URL→hash mapping with git-style blob sharding
- **cas:** DB-backed URL→hash mapping, git-style blob sharding, nightly GC
- **api:** Add purge asset cache endpoints
- **ui:** Add Purge Asset Cache button in environment settings
- **edge:** Add edge CDN proxy with ECIES TLS cert distribution
- **compose:** Service-specific domain routing, SQL safety, edge hardening
- **email:** Add open tracking, click tracking, and bounce/complaint webhooks
- **monitors:** Accept 404/405 as healthy and support custom check paths from .temps.yaml

### Documentation

- Update release checklist with verified items
- Update changelog with all unreleased changes
- **changelog:** Write v0.0.7 release notes

### Fixed

- **compose:** Add throwOnError to all stacks API calls
- **compose:** Throw on API errors in stacks client
- **compose:** Fix port parser trimming bug and persist tab on refresh
- **ui:** Hide application port field for docker-compose preset
- **ui:** Handle preset::path format in compose field visibility
- **ui:** Send preset_config for docker-compose in all project creation paths
- **compose:** Read compose file from workflow context, not guessed path
- **ui:** Fix port validation for docker-compose preset
- **ui:** Log form validation errors on submit failure
- **ui:** Allow NaN port value for docker-compose (field is hidden)
- **ui:** Skip setting port to 0 for docker-compose preset
- **compose:** Pass deployment_id to MarkDeploymentCompleteJob + add compose path to git settings
- **compose:** Log errors to deployment stream + fix container conflicts
- **compose:** Improve error logging and env var availability
- **git:** Fetch up to 100 branches for public repos
- **git:** Paginate branch listing for public repos (GitHub + GitLab)
- **compose:** Always use repo dir for compose commands
- **compose:** Set PWD env var for compose commands
- **ui:** Show all presets in fallback list including Docker Compose and Dockerfile
- **ui:** Show Repository link for public repos using github URL fallback
- **ui:** Public repo preset detection, compose override persistence, hide git connection for public repos
- **docs:** Use correct TimescaleDB-HA volume path for data persistence
- **compose:** Filter compose files by root dir and add screenshot job
- **ui:** Show URL input for public repo projects in Git Settings
- **ui:** Show full URL and Public badge for public repo projects
- **ui:** Fix public repo URL parsing and save in Git Settings
- **api:** Accept git_url and is_public_repo in update git settings
- **compose:** Fix preset name comparison, port parsing, and override save
- **compose:** Use host port (left side) for public port suggestions
- **git:** Use GitHub connection token for public repo API calls
- **git:** Validate GitHub token before using for public repo API calls
- **projects:** Use authenticated GitHub token in trigger-pipeline for public repos
- **deploy:** Persist_static_assets must not block mark_deployment_complete
- **deploy:** Fallback to slug-based container teardown for orphaned containers
- **deploy:** Add detailed logging for container registration in mark_complete
- **workflow:** Merge parallel job outputs instead of overwriting context
- **proxy:** Serve stale chunks via DB+CAS instead of filesystem paths
- **proxy:** Add missing temps-file-store dependency for CAS static serving
- **ui:** Show actual network throughput rate instead of cumulative total
- **ui:** Remove CPU/Memory from monitoring page, fix container name truncation
- **compose:** Inject Temps labels via compose override for log collection
- **ui:** Align container selector text to the left
- **compose:** Use correct Docker label keys for log collection
- **deploy:** Skip persist_static_assets for backend presets, accept 404 as healthy
- **deploy:** Include Dockerfile preset in persist_static_assets, skip pull for local images

### Miscellaneous

- **migrations:** Add missing asset manifest and static cache migrations
- Commit remaining unstaged changes from CAS refactor and presets

### Refactor

- **ui:** Move infrastructure pages under settings layout and sync cmd+k
- **file-store:** Manifest-based CAS instead of per-path ref files
- **ui:** Remove standalone Stacks section from sidebar and routes
- Remove standalone temps-compose crate and compose stacks

## [0.0.6] - 2026-03-19

### Added

- Embedded WireGuard, password protection, remote services & public settings ([#33](https://github.com/gotempsh/temps/issues/33))

### Documentation

- **changelog:** Finalize v0.0.6 release notes

### Fixed

- **web:** Fix funnel edit page and metrics display ([#35](https://github.com/gotempsh/temps/issues/35))

## [0.0.6-beta5] - 2026-03-11

### Added

- **web:** Add import .env file to project creation wizard
- **ai-gateway:** Add AI gateway with multi-provider support
- **ai-gateway:** Add GenAI OTel tracing, deployment promotion, on-demand environments

### Documentation

- **changelog:** Add entries for GenAI tracing, deployment promotion, on-demand environments

### Fixed

- **auth:** Update test_role_all assertion for Demo role
- **web:** Make AI Gateway page responsive for mobile devices

## [0.0.6-beta4] - 2026-03-09

### Added

- **multinode:** Add container reconciliation, scheduling config UI, and integration tests

## [0.0.6-beta3] - 2026-03-09

### Added

- External plugins, analytics overview charts, and CRON_SECRET auto-injection ([#28](https://github.com/gotempsh/temps/issues/28))
- **multinode:** Add multi-node cluster support with worker agents
- **security:** Encrypt environment variables at rest with AES-256-GCM
- **multinode:** Add container restart count and node management improvements
- **multinode:** Add alarm system, container health monitoring, and node management
- **multinode:** Teardown old containers on remote nodes and improve node management

### Documentation

- **changelog:** Add entries for multi-node cluster support

### Fixed

- **multinode:** Skip building remote env vars when no active nodes exist
- **deployments:** Prevent busy queue from starving route confirmation poll
- **deployments:** Use non-blocking advisory lock and move teardown outside lock
- **multinode:** Use Docker container names for cross-node env var rewriting
- **web:** Clean up env vars settings and add preview flag support
- **deployments:** Replace PG advisory lock with process-level mutex
- **proxy:** Add upstream connection timeouts and retry on stale connections
- **proxy:** Strip content-length from HEAD responses over HTTP/2
- **deps:** Patch critical and high severity vulnerabilities

## [0.0.6-beta.2] - 2026-03-02

### Added

- Implement job queue for route table updates and enhance deployment handling
- Add Google Indexing API plugin for Temps
- Enhance Docker deployment process with .temps.yaml support

### Documentation

- Update deployment and monitoring documentation for clarity and accuracy

## [0.0.6-beta.1] - 2026-02-28

### Added

- **domains:** Implement paginated domain listing with search functionality
- **logs:** Add structured log aggregator with Docker container log collection
- **logs:** Introduce structured log aggregator and frontend log history viewer
- **domains:** Enhance domain management with wildcard support and pagination
- Add auth rate limiting, external plugin system, docs overhaul, and settings UI

### Documentation

- Update CHANGELOG with proxy batch writer, domain pagination, and DomainSelector
- **changelog:** Add entry for Dockerfile path fix ([#26](https://github.com/gotempsh/temps/issues/26))

### Fixed

- **logs:** Use i32 project IDs, restore BuildKit build logs, and add log history UI
- **projects:** Persist Dockerfile path in project settings
- **presets:** Fix incorrect corepack command for pnpm
- **deployments:** Use Dockerfile preset in container logs WebSocket tests
- **deployments:** Use Dockerfile preset in container logs WebSocket tests

## [0.0.5] - 2026-02-27

### Added

- **mcp:** Implement MCP server with 210 tools for full platform management
- **proxy:** Add Accept: text/markdown support for AI agents
- **providers,backup,core:** Pg_dump sidecar backup, preset registry, and error hardening
- **core:** Add Next.js docs template to project templates
- **external-plugins:** Add external plugin system for standalone binary plugins ([#20](https://github.com/gotempsh/temps/issues/20))
- **otel:** Add OpenTelemetry ingest, query, and frontend traces UI ([#18](https://github.com/gotempsh/temps/issues/18))

### Documentation

- **changelog:** Add MCP server and security audit entries to changelog

### Fixed

- **cli:** Simplify closure to function reference in domain list
- **skill:** Address security audit findings in temps-cli skill
- **cli:** Escape curly braces in MDX docs output and remove credential path
- **cli:** Correct package name and command references in CLI docs
- **mcp:** Fix get_deployment_logs to fetch and parse JSONL log content
- **proxy:** Fix clippy unnecessary_literal_unwrap in markdown test and update changelog
- **proxy:** Extract <main> content before HTML-to-Markdown conversion
- **core:** Lifecycle management, bounded caches, and memory safety across 11 crates
- **environments:** Remove useless .into() conversions on chrono::Utc::now() in tests
- **backup:** Switch pg_dump to plain format to fix OOM and add error logging
- **backup:** Extend command duration for backup sidecar to prevent OOM issues
- Resolve clippy warnings for CI compliance
- **docs:** Use bash instead of sh in install script commands ([#16](https://github.com/gotempsh/temps/issues/16))
- **platform:** Proxy log retention, service UX improvements, vulnerability scanner filtering, and resource monitoring ([#14](https://github.com/gotempsh/temps/issues/14))
- **ci:** Add protoc dependency to release workflow for temps-otel build

### Miscellaneous

- **mcp:** Update version to 0.1.3 and enhance CLI help documentation
- Update Docker configurations and backup service enhancements
- Simplify Docker Compose configuration for PostgreSQL service

### Refactor

- **backup:** Update backup file extensions and improve sidecar memory management
- **backup:** Enhance backup process with direct file writing and improved error handling
- **backup:** Update backup container configuration for improved access and clarity
- **backup:** Optimize pg_dump execution to prevent memory issues

### Testing

- **proxy:** Add pipeline integration tests for markdown edge cases

## [0.0.4] - 2026-02-17

### Added

- **analytics:** Improve dashboard with drag-to-zoom, referrer tracking, and UX fixes

### Fixed

- **proxy:** Remove internal ID headers from proxy responses
- **deployments:** Add pre-flight image existence check before rollback
- **cli:** Move logs under deployments subcommand and fix log rendering
- **cli:** Populate required parameters when creating services in wizard
- **cli:** Add hardcoded fallback for service required parameters

## [0.0.3] - 2026-02-16

### CI

- **tests:** Add retry resilience for flaky test failures
- **tests:** Enhance test resilience with targeted retries and clippy advisory

### Documentation

- **README:** Add node-sdk package information to the documentation

### Fixed

- **workflows:** Enable pull request trigger for Rust tests workflow
- **deployer:** Remove unnecessary u64 cast flagged by clippy
- **providers:** Cap health check backoff and detect dead containers
- **query:** Add missing DataSource import in redis and s3 doctests

### Miscellaneous

- **dependencies:** Update various package versions and clean up Cargo.lock
- **gitignore:** Add eclipse IDE files to gitignore

### Refactor

- **providers:** Standardize PostgreSQL default to v18-alpine
- **commands:** Update data directory handling to use Path instead of PathBuf

## [0.0.2-beta9] - 2026-02-16

### Added

- **migration:** Implement migration command and related functionality
- **analytics:** Add recent activity endpoint for real-time event tracking
- **dependencies:** Update package versions and clean up Cargo.lock
- **database:** Update TimescaleDB Docker images to pg18

## [0.0.2-beta8] - 2026-02-13

### Added

- **setup:** Implement system user creation for webhook context

## [0.0.2-beta7] - 2026-02-13

### Added

- **analytics:** Add visitor journey and page flow analytics endpoints
- **analytics:** Enhance visitor analytics with EarthGlobe component and new assets
- **analytics:** Implement date filtering for visitor analytics

### Documentation

- Add Cloud ACME Certificates section for TLS provisioning

### Fixed

- **deployments:** Route docker image uploads to correct pipeline for git projects
- **deployments:** Use environment slug for manual deployment URLs

## [0.0.2-beta6] - 2026-02-12

### Documentation

- **README:** Update documentation with new links and mermaid diagrams

### Miscellaneous

- Update .gitignore to exclude certificate and key files
- Clean up localtemps app by removing unused files and directories
- Remove SKILL.md file to streamline project documentation

## [0.0.2-beta5] - 2026-02-06

### Added

- **setup:** Improve GeoLite2 database download feedback and progress indication
- **docker:** Enhance image handling and platform validation
- **analytics:** Update referrer handling for favicon display and naming
- **docker:** Improve image inspection and metadata handling

## [0.0.2-beta4] - 2026-01-29

### Added

- **skills:** Add new skills for custom domain setup, Node.js SDK integration, React analytics, session recording, and deployment management
- **events:** Enhance referrer handling in event metrics recording
- **deployments:** Add local Docker image deployment command and verification job

## [0.0.2-beta3] - 2026-01-27

### Added

- **deployments:** Streamline deployment process and enhance user experience

## [0.0.2-beta2] - 2026-01-23

### Added

- **setup:** Enhance IP address confirmation and password handling in non-interactive mode

## [0.0.2-beta1] - 2026-01-23

### Added

- **templates:** Add project templates configuration and demo mode enhancements
- **templates:** Introduce template management and TLS enhancements
- **demo:** Enhance demo mode functionality and UI components
- **workflow:** Add job configuration with custom dependencies and required flag
- **deployments:** Introduce remote deployment support and enhance project source types
- **deployments:** Add support for Docker image and static file deployments

### Miscellaneous

- **dependencies:** Update Next.js and React versions in package configuration

## [0.0.1] - 2026-01-13

### Added

- **blob, kv:** Add update functionality for Blob and KV services
- **migration:** Enhance UTM fields migration using SeaORM API
- **blob, kv:** Enhance service initialization and status handling
- **localtemps:** Initialize LocalTemps desktop app with Tauri, React, and TypeScript
- **localtemps:** Update dependencies and add DMG build script
- **localtemps:** Integrate analytics features with SeaORM and React Query
- **localtemps:** Enhance UI components and integrate new dependencies
- **analytics:** Enhance AnalyticsInspector with session replay and event categorization
- **setup:** Enhance DNS setup process with propagation verification and cleanup
- **screenshot:** Introduce NoopScreenshotProvider and enhance Chrome availability check
- **serve:** Add screenshot provider option to ServeCommand
- **temps-blob:** Migrate from MinIO to RustFS for blob storage
- **redis:** Implement pagination for Redis key listing and querying
- **demo:** Implement demo mode functionality with user role and UI adjustments

### Miscellaneous

- **redis:** Update Docker image version from 7-alpine to 8-alpine

### Refactor

- **migration:** Streamline UTM fields index creation and deletion
- **rustfs:** Implement non-blocking health check for RustFS container

## [0.0.1-beta28] - 2026-01-05

### Added

- **cli:** Enhance setup command with new options and output formats

## [0.0.1-beta27] - 2026-01-04

### Added

- **cli:** Enhance temps-cli with new commands and documentation generation
- **analytics:** Enhance analytics features with UTM tracking and visitor activity filtering
- **blob, kv:** Introduce temps-blob and temps-kv services with comprehensive functionality
- **cli, services:** Update temps-cli and introduce new services for blob and key-value storage

## [0.0.1-beta26] - 2026-01-02

### Added

- **email:** Add email validation service and update related components

### Miscellaneous

- **release:** Update macOS runner version from 13 to 15 for build jobs

## [0.0.1-beta25] - 2025-12-16

### Added

- **backup:** Integrate urlencoding for password handling in PostgreSQL connections
- **backup:** Add external service backup functionality
- **download-repo:** Enhance repository cloning logic with ref-based strategy
- **email:** Introduce email service with AWS SES and Scaleway support
- **email:** Add web interface for email management with Mailhog-like capture
- **email:** Integrate temps-email crate and register EmailPlugin
- **email:** Update EmailProvidersManagement with new icons and layout enhancements
- **email:** Add email management endpoints and SDK integration
- **email:** Add test email functionality for email providers
- **email:** Enhance test email functionality and error handling
- **deployment-tokens:** Implement deployment token management and validation
- **email:** Implement DNS verification utilities and enhance domain management
- **dns:** Introduce DNS management capabilities with Cloudflare and Namecheap support
- **dns:** Add DNS provider management and integration
- **env-vars:** Enhance environment variable management with new commands
- **docker:** Implement security hardening for Docker images using distroless
- **routes:** Add route management functionality with new AddRoute page
- **url-validation:** Introduce comprehensive URL validation to prevent SSRF attacks
- **build-image:** Enhance Docker image build process with project slug and prune command
- **vulnerability-scanner:** Introduce vulnerability scanning functionality
- **vulnerability-scanner:** Enhance vulnerability scanning with Docker image support
- **custom-domains:** Enhance redirect URL validation and error handling
- **vulnerability-scanner:** Add deployment_id to vulnerability scans and enhance related functionality
- **security:** Introduce vulnerability scanning features and UI components
- **vulnerability-scanner:** Add vulnerability scan completion notifications and trigger scan API
- **migrations:** Add environments route trigger for deployment updates
- **vulnerabilities:** Add new fields and notification handler for vulnerability scans
- **dns-providers:** Add support for Azure and Google Cloud DNS providers
- **dns:** Integrate DNS provider management and automatic DNS setup for email domains
- **setup:** Add initial setup command for Temps configuration
- **projects:** Revamp project creation process with enhanced user prompts and service integration
- **docker:** Transition to Alpine-based Node.js image for enhanced security
- **cli:** Enhance project creation with search-based repository selection and auto-sync
- **mcp:** Add tools handler and Temps API client integration
- **domains:** Add automatic DNS challenge record provisioning
- **setup:** Enhance setup command with git connection creation, geolite2 validation, and improved UX
- **cli:** Add runtime-logs command for container log streaming
- **cli:** Add environment configuration commands

### Fixed

- **backup:** Escape special characters in PostgreSQL password for Docker environment
- **backup:** Update error handling and content type for S3 uploads
- **network:** Always use container names for services and IPv4 for proxy

### Miscellaneous

- **workflows:** Comment out pull_request trigger in Rust tests workflow

### Refactor

- **docker:** Remove prune command for Next.js projects in Dockerfile generation
- **command-palette:** Format keywords for better readability in navigation items

## [0.0.1-beta24] - 2025-11-27

### Fixed

- **release:** Correct cache action version in release workflow

## [0.0.1-beta23] - 2025-11-26

### Added

- **backup:** Implement backup management commands in CLI
- **dependencies:** Update Cargo.toml and Cargo.lock for AWS SDK and new features
- **webhooks:** Add comprehensive diagnostic logging for webhook delivery troubleshooting
- **webhooks:** Add endpoint to retrieve specific webhook delivery details
- **temps-cli:** Set up API client and configuration for improved interaction

### Fixed

- **webhooks:** Start listener in background to avoid blocking plugin initialization

### Refactor

- **tests:** Improve Minio client configuration and clean up VisitorsList component

## [0.0.1-beta22] - 2025-11-17

### Refactor

- **digest:** Update notification digest structure and enhance project statistics

## [0.0.1-beta21] - 2025-11-17

### Added

- **query:** Introduce query service and enhance data browsing capabilities
- **ServiceDataBrowser:** Add icons for S3 bucket, prefix, and object types
- **metadata:** Enhance EntityInfo with additional metadata fields and improve error logging
- **docker:** Add Dockerfile for release and enhance CI/CD workflow
- **deployment:** Introduce deployment cancellation and preview environment support
- **activity-graph:** Add deployment activity graph endpoint and frontend component

## [0.0.1-beta20] - 2025-11-13

### Added

- **import:** Implement Docker container import functionality
- **projects:** Refactor ProjectServiceInfo to include detailed project metadata

### Fixed

- **logs:** Enhance log line and viewer components with text selection support
- **postgres:** Update default Docker image to postgres:18-alpine and adjust related configurations

## [0.0.1-beta19] - 2025-11-11

### Added

- **backup:** Auto-create S3 buckets when creating S3 targets
- Add live visitors feature and Docker registry settings
- Add preview environment functionality for projects
- **docker:** Add support for prebuilt binary to skip Rust compilation
- **docker:** Enhance Docker deployment with multi-stage builds and GeoLite2 management

### Fixed

- **docker:** Remove hardcoded database URL and improve Alpine compatibility
- **docker:** Add build tools and fix bun installation for Alpine Linux
- **docker:** Simplify Dockerfile by removing dumb-init and fix architecture compatibility

## [0.0.1-beta18] - 2025-11-10

### Refactor

- Streamline wasm-pack installation in CI workflows

## [0.0.1-beta17] - 2025-11-09

### Fixed

- Update CI workflows to ensure WASM build environment is correctly configured

## [0.0.1-beta16] - 2025-11-09

### Added

- Refactor project detail components and enhance sidebar functionality
- Enhance CI workflows and service management features

## [0.0.1-beta15] - 2025-11-09

### Added

- Implement container management and metrics retrieval features

## [0.0.1-beta14] - 2025-11-09

### Added

- Add integration and streaming tests for chunked transfer encoding
- Implement container metrics and management features

## [0.0.1-beta13] - 2025-11-08

### Added

- Enhance Docker testing and service configuration management

### Refactor

- Update test for solution verification with realistic difficulty

## [0.0.1-beta12] - 2025-11-06

### Added

- Add docker image configuration to Postgres service
- Refactor analytics to utilize proxy logs and introduce visitor ID tracking
- Add once_cell dependency and refactor network name handling
- Implement keyboard shortcut for adding environment variables
- Enhance environment variable logging and add new SDK files
- Improve component rendering and environment variable handling
- Add testing scripts and documentation for Temps MCP Server
- Add unique visitor count to ProjectCard component
- Optimize analytics query for overall stats
- Introduce CAPTCHA protection and IP access control features

### Fixed

- Correct total_count type in SessionLogsResponse

## [0.0.1-beta11] - 2025-10-27

### Added

- Enhance user management by excluding soft-deleted users and adding unique email constraint

### Fixed

- Update README and refactor cookie handling in middleware

## [0.0.1-beta10] - 2025-10-27

### Added

- Enhance authentication and user management logging
- Remove bcrypt support and enforce Argon2 for password hashing
- Enhance visitor analytics with user agent tracking
- Introduce KbdBadge component and implement keyboard shortcuts across various pages

## [0.0.1-beta9] - 2025-10-27

### Added

- Update static deployment paths and enhance TLS settings

## [0.0.1-beta8] - 2025-10-27

### Added

- Update installation instructions and release workflow

## [0.0.1-beta7] - 2025-10-27

### Added

- Enhance URL construction in DeploymentService
- Refine URL construction logic in DeploymentService
- Remove DomainProvisioning component and related routes
- Add error handling for service deletion with linked projects

## [0.0.1-beta6] - 2025-10-27

### Added

- Integrate default crypto provider for TLS in Domains Plugin

## [0.0.1-beta5] - 2025-10-27

### Added

- Revise release workflow and enhance deployment metadata

## [0.0.1-beta4] - 2025-10-27

### Added

- Update release workflow and dependencies

## [0.0.1-beta3] - 2025-10-27

### Added

- Introduce test release workflow and update main release configuration

## [0.0.1-beta2] - 2025-10-26

### Added

- Enhance release workflow with existing release deletion and file handling

## [0.0.1-beta1] - 2025-10-26

### Added

- Add go/python/flask as examples
- Add basic Flask application example
- Implement Java preset for Nixpacks
- Add timestamps option to container log queries
- Enhance static file deployment support in proxy service
- Add installation script and Homebrew formula generation to release workflow

### CI

- Update runner configuration for all tests to use ubuntu-latest-4-cores
- Enhance release workflow with macOS builds and disk cleanup steps

### Documentation

- Add CHANGELOG and release documentation
- Overhaul README for clarity and structure
- Enhance README with streamlined quick start guide
- Update README for improved PostgreSQL setup instructions

### Miscellaneous

- Update ci
- Optimize release profile and clean up CI workflows
- Update TimescaleDB image references to use latest version
- Update dependencies and enhance testing setup
- Enhance CI workflow with disk cleanup and deployment updates
- Update dependencies and enhance project structure
- Update go.sum in example go
- Update go.sum with new dependencies in gin-basic example
- Update dependencies and enhance project structure
- Update dependencies and improve project configurations
- Update dependencies and improve code structure
- Enable cargo formatting and clippy checks in pre-commit configuration
- Remove Nixpacks integration from temps-deployer
- Refactor migration files and update schema definitions

### Refactor

- Simplify log retrieval parameters with structured types
- Update integration tests to use new test configuration service
- Improve code formatting and readability across multiple files

### Testing

- Update log retrieval tests to use structured logging format

<!-- generated by git-cliff -->
