# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **One-click demo deploy from templates (no Git account).** A brand-new user
  can deploy a fully-instrumented demo app — analytics, error tracking,
  distributed tracing, and a Postgres database — in one click without first
  connecting a Git provider. `POST /projects/from-template` now accepts an
  *optional* `git_provider_connection_id`: with a connection it forks into your
  Git account as before; without one it deploys directly from the template's
  public source repo (the template subfolder as the build directory) and
  auto-queues the first deploy. The `TemplateConfigurator` and first-project
  empty state surface a "Try the demo app" path, auto-attaching an existing
  matching service (e.g. Postgres) when present. Public-repo deploys can't
  receive push webhooks, so auto-deploy-on-push is disabled in that mode (#157).
- **CLI: static deploys in `temps up`.** The setup wizard now offers a "Static
  (upload a pre-built folder)" option that creates a `static_files` project and
  uploads a built folder — no Docker, no git. It auto-detects the output folder
  (`dist`/`build`/`out`/`public`/`_site`/`output`, or a root `index.html`) and
  lets you pick a custom one. New `--static` / `--static-dir` flags
  (`@temps-sdk/cli`, #154).

### Changed
- **Rollback falls back to rebuilding from source for git projects when the
  stored image is gone.** Rollback reuses the target deployment's Docker image
  when it's still in the local cache (the common case — rolling back a recent
  deploy), which is near-instant and byte-identical to what you're rolling back
  to. Previously that was the *only* path, so it failed once the nightly cleanup
  pruned the image (`image no longer exists locally` for anything older than
  ~7 days) and couldn't reconstruct static deployments. Now, for git-sourced
  projects, rollback rebuilds from source at the target deployment's commit
  whenever the image is unavailable (pruned, or a static preset with no runnable
  image) — going through the same build + health-check pipeline as a normal
  deploy. Non-git projects (docker_image / static_files / manual without a git
  ref) keep the image-reuse path unchanged (#155).
- **CLI: the default project is scoped per instance.** `defaultProject` now
  lives on the active CLI context instead of one global key, so a project
  created on one Temps server no longer leaks as the default on another (which
  printed `Using project … (from global-config)` and 404'd). `temps up` also
  falls back to the setup wizard when a stale default no longer resolves, rather
  than dead-ending on the 404 (`@temps-sdk/cli`, #154).

### Fixed
- **CLI: `temps up` / `deploy:local-image` now stream the full Docker build
  log.** Build output was collapsed into a single rewriting spinner line, so the
  per-step BuildKit log was lost (you'd only see e.g. `#25 exporting layers`).
  The build now runs with `--progress=plain` and streams each line
  (`@temps-sdk/cli`, #154).
- **`docker-providers` CI no longer hangs/flakes under load.** The heavyweight
  `test_{mongodb,postgres,redis,s3}_backup_and_restore_to_s3` tests each spin up
  a MinIO container plus a service container and stream a real wal-g/mongodump
  backup; running them in parallel saturated Docker and raced for host ports
  (`Bind for 0.0.0.0:<port> failed: port is already allocated`), causing 300s
  timeouts (most visibly the MongoDB backup step). They now run in a dedicated
  serial `docker-backups` CI group (`--test-threads=1`), mirroring the existing
  `postgres-upgrades` group, and are skipped from the parallel `docker-providers`
  group.
- **Host-port selection is less collision-prone.** The per-service
  `find_available_port` helpers (mongodb/postgres/redis/s3/rustfs and
  `parameter_strategies`) are consolidated into `externalsvc::port_util`, which
  advances a process-wide offset so concurrent allocations diverge instead of all
  landing on the same base port, and exposes a Docker-aware variant that skips
  ports already published by running containers.


## [0.1.0-beta.38] - 2026-06-23

### Added
- **On-demand HTTP-01 TLS now covers the console host.** `temps serve` derives
  `console.<zone>` and passes it to the proxy's `OnDemandCertConfig.console_host`,
  exempting exactly the console host from the cert-eligible-route check (it is
  served as a fall-through and has no `CachedPeerTable` route) while still
  requiring it to be in-zone — so QuickStart installs get HTTPS for the console
  and apps without an eager per-host provisioning step. Every other host still
  needs a cert-eligible route, preserving the random-SNI flood defense.
- **`temps setup --letsencrypt-email <addr>` (env `LETSENCRYPT_EMAIL`).** The
  ACME contact address is now configured explicitly and written to
  `settings.letsencrypt.email`. An empty value is ignored, so re-running setup
  without the flag does not wipe a configured address. This is the single source
  of truth for ACME issuance — see the Fixed entry below for the behaviour change.
- **Per-environment "Deploy on push" toggle.** `DeploymentConfig.automatic_deploy`
  is now `Option<bool>`, letting each environment that tracks a branch decide
  deploy-on-push vs on-demand independently, with env-wins merge semantics (the
  environment value overrides the project default). Surfaced as a toggle on
  `EnvironmentConfigurationCard`; multiple environments tracking the same branch
  now each deploy according to their own setting.
- **Daily anonymous instance heartbeat for active-instance accuracy.** A new
  `InstanceHeartbeat` telemetry event fires once per 24h (first beat one interval
  after boot) carrying the same non-identifying depth-of-usage counts as
  `instance_started` (projects, environments, managed services, worker nodes) so
  a live-but-idle install is no longer undercounted as inactive. Spawned only when
  the reporter is enabled; a no-op under the `TEMPS_TELEMETRY=0` opt-out. Heartbeat
  and `instance_started` also now carry `has_git_provider` (bool) and a coarse
  `capacity_tier` RAM band (xs/small/medium/large/xl/xxl, never exact specs).
- **Settings → Cleanup page.** New UI (`CleanupPage`) to configure automatic
  resource cleanup: enable/disable, `run_hour_utc`, `image_max_age_days`,
  `keep_deployments_per_env`, `build_cache_max_age_days`, and
  `build_cache_max_size_mb`.
- **Settings → OTLP ingest card.** New `OtelIngestCard` to view and toggle the
  OTLP ingest key from platform settings (`updateOtelIngest`).
- **Type-to-confirm delete for external services.** `DeleteServiceButton` /
  `ServiceDetail` now require typing the service name to confirm deletion, and
  `ServiceHealthCard` surfaces a degraded-status alert.

### Changed
- **On-demand TLS auto-enable is now gated on reachability, not the domain
  suffix.** `temps setup` previously turned on on-demand HTTP-01 TLS only for
  `*.sslip.io` installs; the gate is now `!preview_domain.is_empty() &&
  !is_loopback_zone(preview_domain)`, so any non-empty, non-loopback base domain
  (sslip.io or a custom wildcard domain like `apps.example.com`) qualifies, and
  loopback/local mode is the only disqualifier. A pre-loaded wildcard cert still
  pre-empts on-demand issuance for every host it covers.
- **Full deployment/build logs replay on first load.** The tail replay backlog
  (`DEFAULT_TAIL_REPLAY_LINES` in `temps-logs`) was raised from 1000 to 100,000
  lines so opening a log view shows the complete log instead of a silently
  truncated tail; `tail_log_with_replay` is now exposed.
- **Container teardown is bounded per pass.** `mark_deployment_complete` and
  `workflow_execution_service` now cap teardown work at
  `MAX_TEARDOWN_DEPLOYMENTS_PER_PASS` and apply a per-container
  `CONTAINER_TEARDOWN_TIMEOUT_SECS`, so a slow or stuck container teardown can no
  longer stall the whole pass.
- **First-touch telemetry milestones now fire exactly once per instance.** Five
  "first" events (`analytics_first_event_received`, `session_replay_first_session`,
  `ai_gateway_first_request`, `first_deploy_succeeded`, `error_tracking_first_error`)
  previously re-fired on every matching action, so telemetry volume scaled with the
  self-hoster's production traffic. They are now emitted once via
  `TelemetryReporter::report_once`, backed by a durable `telemetry_milestones`
  table (migration `m20260621_000001_create_telemetry_milestones`) with an
  in-process guard, giving exactly-once semantics across restarts and the split
  proxy/console processes. No-op under the `TEMPS_TELEMETRY=0` opt-out.
- **New environment names are lowercased on creation** (e.g. `Test` → `test`).
- **Branch picker is a searchable combobox.** `BranchSelector` now filters
  branches as you type and shows protected-branch badges.
- **`@temps-sdk/analytics-browser` ships a full (rrweb-inlined) build and a CDN
  bundle.** A new `auto-full` entry inlines rrweb/`@rrweb/packer` so session
  recording works without the `--external` light build, and the package now
  exposes a `./cdn` export plus `unpkg`/`jsdelivr` (`dist/temps.min.js`) for
  script-tag installs.

### Fixed
- **Scale-to-zero wake no longer returns a spurious 503 on the first request.**
  The wake path reported a container "ready" as soon as a TCP connect to its
  mapped host port succeeded, but Docker's userland proxy (`docker-proxy`,
  default on Docker Desktop and on Linux) accepts connections the instant the
  container starts — before the app inside binds its port — so the first request
  raced startup. `temps-deployer::readiness` now issues an HTTP GET (via
  `DeploymentMode::build_container_url`, the same URL resolution `DeployImageJob`
  uses), treating any 2xx/3xx/404/405 as ready, so a TCP-open-but-silent
  container is correctly reported not-ready. The redundant TCP readiness gate in
  `mark_deployment_complete` was removed (the standard deploy paths already run
  the stronger HTTP health check before the route flip).
- **ACME issuance now requires a real Let's Encrypt contact email instead of a
  bogus fallback.** Email resolution previously fell back to the first user's
  address and then `system@temps.dev`; on a fresh install the first user is
  `system@localhost`, which Let's Encrypt rejects, silently failing every cert
  (including the auto-issued console cert on sslip.io quick installs).
  `settings.letsencrypt.email` is now the only source — `get_acme_email` and the
  on-demand `DomainServiceProvisioner` return it or nothing, with no first-user
  or `system@temps.dev` fallback. `provision_certificate` hard-errors
  (`TlsError::Configuration`) on an empty email rather than registering an ACME
  account with a bogus contact, and `build_on_demand_cert_manager` warns at boot
  when on-demand TLS is enabled but no contact email is configured.
- **A domain's existing certificate is served regardless of ACME lifecycle
  state.** The TLS cert loader's status filter was removed, so any domain with a
  loaded cert + key is served even mid-challenge (e.g. while in
  `challenge_requested`); this fixes a rate-limit loop where the in-flight
  challenge status blocked the still-valid cert during the ~5-second challenge
  window. `begin_on_demand_issuing` also now skips re-issuance when the cert is
  already active, and `DomainDetail` shows Certificate/Expires/Last-issued facts
  even when the status is `on_demand_failed`.
- **`--database-url` password no longer leaks into process listings.** When the
  database URL was passed as a CLI flag (`temps serve --database-url=postgres://user:pass@host/db`),
  the full connection string — including the password — was visible to any user
  on the server via `pgrep -af`, `ps aux`, or `/proc/self/cmdline`. Temps now
  scrubs the value from its own argv immediately after clap parses the args, so
  the process table shows `xxx` in place of the real password. The `--help`
  output also masks the env value (`hide_env_values`). The `--database-url` flag
  continues to work for backwards compatibility; using `TEMPS_DATABASE_URL` as
  an environment variable remains the recommended approach since env vars are
  never visible in process listings.


## [0.1.0-beta.36] - 2026-06-20

### Fixed
- **Traces page no longer crashes on traces with an empty service name.** A
  trace whose root span never set `service.name` arrives with an empty
  `service_name`, which flowed into the Service filter dropdown as a Radix
  `<SelectItem value="">` and threw ("must have a value prop that is not an
  empty string"), taking down the whole Traces page via the error boundary.
  Empty/missing service names are now filtered out of the dropdown, and the
  trace table shows `(unnamed)`/`unknown` fallbacks instead of blank cells.


## [0.1.0-beta.35] - 2026-06-19

### Added
- **`active_renewal_failed` domain status.** A domain whose certificate is still
  valid but whose last renewal attempt failed now enters a distinct
  degraded-but-serving state instead of being silently disabled. The proxy keeps
  serving the existing certificate (the cert-serving queries match a new
  `CERT_SERVING_STATUSES` set), and the console/CLI surface a "renewal failed"
  warning so operators can fix the renewal before the certificate expires.
- **Monitoring is on by default for new services.** Creating an external service
  now seeds the engine's default alert rules (idempotent, all engines) and, for
  OTLP-push engines (rustfs/s3), provisions and applies the OTLP ingest key at
  creation. All monitoring setup is best-effort and never fails service creation.

### Changed
-

### Fixed
- **Cancelling or failing a certificate renewal no longer drops the existing
  valid certificate.** Previously, cancelling an in-flight renewal (or a renewal
  that failed) reset the domain's status away from `active`; because the proxy
  only serves certificates for `active` domains, a still-valid certificate
  silently stopped being served and HTTPS broke. Renewal cancel/failure now
  preserves the live certificate when it is still usable (present cert + key and
  not within a 5-minute clock-skew margin of expiry), and a renewal-failure that
  occurs while a valid certificate remains keeps serving it under the new
  `active_renewal_failed` status. Also fixed a latent bug where the
  challenge-failure path computed a status update but never persisted it.
- **Idempotent column-add migrations now scope their existence checks to the
  current schema.** The `IF NOT EXISTS` guards in the monitoring-settings,
  `api_keys.service_id`, and `alarms.service_id` migrations queried
  `information_schema.columns` without a `table_schema` filter. When several
  schemas share one database (as in the parallel test harness), a column added
  in one schema made the guard skip the `ALTER` in another, and the follow-up
  `CREATE INDEX` then failed with `column "service_id" does not exist`. The
  guards now filter on `table_schema = current_schema()`.
- **Uptime monitors no longer report a false outage on local installs.** When
  `external_url` is unset, the monitor target URL is now built against the proxy
  HTTP listener port instead of defaulting to `https`/:443 (which was
  unreachable and surfaced as a "Major Outage").
- **Deploy cancellations and 4xx health-check noise are treated correctly.** A
  cancelled deployment is no longer recorded as a failure, and 4xx responses
  during health probing are no longer misclassified as outages (#136).

### Security
- **On-demand HTTP-01 TLS issuance hardening (ADR-018).** The custom ACME-client
  path was hardened per security review as part of the on-demand certificate
  issuance feature.


## [0.1.0-beta.34] - 2026-06-17

### Added
- **Anonymous product telemetry.** Temps now reports anonymous, aggregate
  usage events (deploy attempted vs. succeeded/failed, project/service/feature
  creation, where instances stall) so maintainers can tell whether the platform
  is working for self-hosters. **No PII** is collected — events carry a random
  per-instance id (never derived from anything machine-identifying), event
  names, and non-identifying properties (counts, enum labels, durations). A
  coarse, fixed-category reason is sent for failed deploys (never raw logs), and
  the request IP is used transiently to derive a country code that is stored
  while the IP itself is never persisted. Telemetry is **on by default**;
  operators opt out with `TEMPS_TELEMETRY=0` and can redirect it with
  `TEMPS_TELEMETRY_ENDPOINT`. The ingest API and its event schema are open
  source (`telemetry-api/`).
- **Custom health-check path.** Define the HTTP path used for the container
  health check and the uptime monitor instead of the default `/` — set it in
  `.temps.yaml` (`health.path`), at deploy time (`--health-check-path` on
  `deploy:image` / `deploy:local-image` / `deploy:static`), or per-monitor
  (`--check-path` on `monitors create`). Useful for APIs with a dedicated
  endpoint such as `/api/healthz`.
- **Configurable Postgres shared memory.** Managed Postgres services accept a
  `shm_size_mb` setting (container `/dev/shm`), fixing "could not resize shared
  memory segment … No space left on device" under parallel-query load. Changing
  it recreates the container (shared memory is fixed at create time).


## [0.1.0-beta.33] - 2026-06-17

### Added
- **Split proxy and console into independent processes (ADR-017)**: `temps serve` gains a `--role` flag (`all` — the default, unchanged single-binary control plane; or `console` — run only the console/API/web/workers). Paired with the standalone `temps proxy` command, this lets the Pingora data plane (`:80`/`:443`) and the Axum control plane run as separate processes so the console can be upgraded/restarted without dropping production traffic. Adds `GET /healthz` (liveness) and `GET /readyz` (readiness, 503→200 once plugins initialize), an optional `--console-admin-address` to split public-ingest from admin/UI routes (with `--admin-allowed-ips`/`--admin-allowed-hosts` allowlists), cross-process on-demand wake wiring in `temps proxy`, and version-skew detection so the proxy warns when its binary diverges from the console during a rolling upgrade (`temps upgrade --split` guidance included). Default single-binary behavior is unchanged.
- **Per-environment attack mode**: `attack_mode` (CAPTCHA challenge protection) can now be overridden per environment instead of being project-wide only. The new `environments.attack_mode` is tri-state — inherit the project setting (default, no behavior change on upgrade), force on, or force off — so you can, e.g., enable challenges on production while leaving preview environments open. Exposed in the environment settings API/UI and recorded in the audit log.

### Fixed
- **CPU alarms now fire relative to the container's CPU limit** (`temps-monitoring`): Docker's `cpu_percent` is per-core (100% == one core), so a container allowed 2 cores reads ~200% at full use. The alarm compared raw `cpu_percent` against a flat 90% threshold, firing at ~47% of the actual limit. Alarms (and the seeded alert rule) now threshold a new limit-relative `container.cpu_utilization_percent` metric (100% == limit saturated); the raw `container.cpu_percent` and `container.cpu_limit_cores` are still emitted for the cores view.
- **Deployed environments are no longer silently CPU/memory-capped when no limit is configured** (`temps-deployments`): `ResourceUsage::default()` seeded `1000000u` (1 core) / `512Mi`, so any deploy path that built a job without calling `.resources(...)` — notably `rollback_to_deployment` and `promote_deployment` — applied a phantom Docker `nano_cpus`/`memory` cap even though neither the project nor the environment `deployment_config` set one. The default is now all-`None` (uncapped unless opted in), and rollback/promote resolve CPU/memory limits + requests env→project the same way the normal deploy path does. Verified end-to-end: a `None`-limit deploy produces `HostConfig.NanoCpus=0`/`Memory=0`, while an explicit limit still reaches the container.

### Security
- **SSRF guard + auth-token drop on Git archive redirects** (`temps-git`): the GitLab provider now validates archive-download redirect targets against a host allowlist (HTTPS-only; `*.gitlab.com`, `*.gitlab-static.net`, and the configured self-hosted instance host/subdomains — rejecting internal/metadata IPs and lookalike/suffix/userinfo/trailing-dot spoofs), follows at most one redirect, and never forwards the `Authorization` header to the redirect target. This brings GitLab to parity with the GitHub provider's existing guard. The 5 GiB archive size cap is now enforced before each chunk is written (preventing one over-limit chunk from reaching disk) in both providers. security-auditor approved.


## [0.1.0-beta.31] - 2026-06-11

### Added
- **Filter runtime history logs by deployment**: the history log viewer now has a Deployment dropdown that scopes logs to a single deployment, alongside the existing environment/service/level/time filters. Selecting a deployment also defaults the time range to that deployment's lifespan (so you don't have to guess timestamps) and is reflected in the URL via `?deploy_id=` for shareable deep links. Backing this, the chunk `deploy_id` was switched from `uuid` to `integer` to match the deployment id (`deployments.id`) the system actually tags containers with — the previous `uuid` typing meant the collector silently parsed the integer deploy label to `NULL`, so every chunk stored no deployment and the filter matched nothing. The log collector now parses the `sh.temps.deploy_id` label as an `i32`, and the search path filters chunks by deployment at both the index (SQL) and line level.

### Changed
- **Runtime History log filters and timestamps** (#130): the History log filter controls are regrouped into a single cluster — a scope row (environment / service / deployment / time range) over a refine row (search / context lines / columns), with the level chips labeled — so they read as one intentional filter axis instead of floating buttons. Row timestamps now prefix the weekday and date (e.g. `Mon Jun 10 13:23:18.361`) automatically when the visible logs span more than one calendar day, so a multi-day filter no longer leaves you guessing which day a line belongs to; single-day views stay compact (time only).

### Fixed
- **On-demand wake now waits for the app to actually accept connections, not just for Docker to report `Running`**: `ContainerLifecycleAdapter::is_container_healthy` (the readiness gate `do_wake` polls before completing a scale-to-zero wake) checked only `ContainerStatus::Running`, so a wake could finish before the application inside had bound its port — the first request would then be proxied to a not-yet-listening upstream and get a spurious 503. It now TCP-probes the container's lowest published host port on loopback after confirming `Running` (short timeout, treated as not-ready-yet on failure); the lowest port is chosen deterministically (Docker reports ports unordered), and containers with no published port fall back to the `Running` check. Scoped to local single-node containers (remote-node wake is tracked separately). Closes the last independent first-request 503 path on local on-demand environments (follow-up to the routing fix in v0.1.0-beta.30).
- **On-demand wake/sleep no longer breaks on multi-node deployments** (#126): `do_wake` and `sleep_environment` loaded *all* of a deployment's `deployment_containers` and started/stopped each one via the local Docker daemon, with no `node_id` filter. On a multi-node cluster, containers owned by a remote worker don't exist on the control plane's daemon, so the local `start_container` failed and the whole wake reverted — scale-to-zero only ever worked on single-node deployments. `OnDemandManager` now tracks a `local_node_id` and treats a container as local iff its `node_id` is `NULL` (control-plane-local) or matches that id; wake starts only the local containers (skipping remote-owned ones with a warning), a fully-remote environment returns an explicit retryable error instead of falsely reporting a successful wake, and sleep is symmetric (remote containers are left to their own node's idle sweep). Remote-node *wake* itself remains a separate, unshipped multi-node feature.
- **On-demand 503 responses no longer leak `environment_id` or internal error strings** (#127): the `wake_throttled`/`wake_pending`/`wake_failed` 503 bodies are served to unauthenticated clients (a sleeping environment has no auth context yet) yet disclosed the opaque `environment_id` and, for `wake_failed`, the interpolated `OnDemandError` Display string (which can carry container/deployment context). All three bodies are now static — clients key retries off `Retry-After`, not the id — and the detailed wake error is logged server-side only.


## [0.1.0-beta.30] - 2026-06-10

### Fixed
- **On-demand (scale-to-zero) environments no longer 503 on the first request**: a sleeping environment is excluded from the proxy route table, so the first request must wake it and reload routes before it can be served. The wake path was the one caller still relying on fire-and-forget PostgreSQL `NOTIFY route_table_changes` (instead of the deterministic in-process `Job::ForceRouteReload` the deploy pipeline uses), did a single `resolve_context` with no retry, and treated a reload-wait timeout as success — so when the route hadn't reloaded yet the request fell back to the console upstream and the client saw a 503/404 for its own domain. `do_wake` now publishes `Job::ForceRouteReload` (in addition to the PG NOTIFY, kept for remote nodes — including on the no-containers path), `wait_for_route_reload` is lost-wakeup-safe and returns a real timeout signal, and the proxy re-resolves the route in a bounded loop after waking (returning an explicit retryable `503 wake_pending` rather than the console fallback). Concurrent requests parked in the wake path are capped with a semaphore to bound request-hold amplification in the proxy hot path.


## [0.1.0-beta.29] - 2026-06-09

### Added
- **Historical deployment container logs**: runtime container logs used to vanish the moment a deployment was superseded and its containers were torn down — making it impossible to debug "what did the container that ran a few days ago actually print?". Now, just before a previous deployment's containers are stopped and removed (`MarkDeploymentCompleteJob::cancel_previous_deployments`), each container's logs are captured to a plain-text file under the data dir (via `LogService`) and recorded in a new `deployment_container_logs` table. New read endpoints `GET /api/projects/{project_id}/deployments/{deployment_id}/container-logs` (list) and `.../container-logs/{log_id}` (content), and a "Captured container logs" section on the deployment detail page, let you read the logs of a container that no longer exists (e.g. `web-2`). Capture is best-effort and tail-capped at 8 MiB — it never blocks or fails a deployment.
- **`project_scope_guard!` and `deny_deployment_token!` auth macros** (`temps-auth`) plus `AuthContext::is_scoped_to_project`: the missing tenant-boundary primitive that confines a project-bound deployment token to its own project. `permission_guard!` proves a caller holds a permission; these prove the resource is theirs.

### Changed
-

### Fixed
- **Domain detail no longer renders a blank page for unclassifiable ACME challenges**: a domain whose `verification_method` was a value the UI had no branch for (legacy `acme`, `tls-alpn-01`, or null) showed an empty card while stuck in `challenge_requested`. The page now derives the effective method from the ACME order's `challenge_type` when the domain field is unrecognised, and falls back to a "Certificate challenge required" card (with create/verify/cancel actions and any pending challenge records) so the user is never stranded.
- **SECURITY (CRITICAL): error-tracking admin API was completely unauthenticated**. Every handler in `temps-error-tracking` (`handler.rs`, `alert_rules_handler.rs`) took only `State`/`Path`/`Query` — no `RequireAuth`, no `permission_guard!`. An unauthenticated attacker could enumerate sequential project ids and read/modify every tenant's error groups, events, stack traces, and raw Sentry payloads (PII). All 14 handlers now require authentication (`ErrorTrackingRead`/`Write`/`Create`), are tenant-scoped, and authorize the path `project_id` against the caller. The DSN-authenticated Sentry ingest path was already correctly self-authenticating and is unchanged.
- **SECURITY (HIGH): cross-project IDOR via `FullAccess` deployment tokens**. A deployment token is auto-injected into every deployed container, bound to one project, and typically carries `FullAccess` — which satisfies `permission_guard!` for *any* permission. Handlers across `temps-environments`, `temps-providers`, `temps-projects`, and `temps-analytics` trusted a `project_id`/service id from the request without checking it against the token's bound project, letting a compromised app read another tenant's secrets/env-var plaintext, DB connection strings, projects, and visitor PII — and modify/destroy them. All such handlers now call `project_scope_guard!` (or verify `project_services` linkage for shared external services, or deny deployment tokens on fleet-wide/by-id endpoints).
- **SECURITY (HIGH): SSRF via self-hosted Git provider URL**. `GitProvidersCreate` (granted to ordinary users) synchronously probed `{base_url}/api/v4/user` on create, so a malicious `base_url`/`api_url` (e.g. `http://169.254.169.254`, `http://127.0.0.1`) was a live SSRF oracle into internal infrastructure. URLs are now validated with `temps_core::url_validation::validate_external_url` before use, and the GitHub/GitLab HTTP clients disable redirect following (closing the public-host→metadata 302 bypass).
- **SECURITY (HIGH): unmasked DB credentials over the external-services API**. `get_service_environment_variables` returned plaintext `POSTGRES_URL`/`POSTGRES_PASSWORD` (`mask_sensitive: false`) to any owner; now only an admin receives unmasked values.
- **SECURITY (MEDIUM): OTLP ingest logged the full bearer token** at `debug` level, writing live, mostly non-expiring credentials to logs whenever debug logging was enabled. It now logs only the auth scheme and length.
- **SECURITY (LOW): proxy trusted a client-supplied `X-Forwarded-Proto`**. `is_https_request` now derives the scheme from the actual downstream TLS digest (`is_tls_connection`) instead of the spoofable header, so the `Secure` cookie attribute and the `X-Forwarded-Proto` forwarded upstream can't be influenced by the client.


## [0.1.0-beta.28] - 2026-06-08

### Added
- **`headerActions` console extension slot**: `ConsoleExtensions` now accepts a `headerActions?: ConsoleHeaderAction[]` array rendered top-right in the dashboard `Header` (via `@temps-sdk/console-kit`). Additive and backward-compatible — consoles that register no header actions are unchanged. This is the OSS-side hook the EE AI SRE Copilot header button plugs into.

### Fixed
- **Duplicate backup runs eliminated**: `start_console_api` spawned its own backup scheduler loop in addition to the one `BackupPlugin` already starts during plugin initialization. With both loops running, each independently found every due `backup_schedules` row and enqueued a `Job::BackupRequested`, producing two completed backup runs per service at the same timestamp. The console-side scheduler is removed; the plugin is now the single owner.
- **Null-SHA push events no longer create failed `0000000` deployments**: branch/tag deletions send the all-zeros Git null SHA (`0000…000`) in the push webhook's `after` field, which previously flowed through into a deployment whose `download_repo` job failed with `Failed to checkout ref 0000000…`. `handle_push_event` now short-circuits empty/all-zeros commits before any DB query or job enqueue (covers both GitHub and GitLab), and `checkout_ref` defensively rejects the null SHA with an actionable error.


## [0.1.0-beta.27] - 2026-06-05

### Added
- `temps migrate --dry-run` flag: previews pending migrations read-only without applying them
- `temps migrate --yes`/`-y` flag: skips interactive confirmation gate (auto-skipped in non-TTY environments)
- Live per-migration progress streaming: prints `→ [N/total] name` / `✓ [N/total] name (timing)` as each migration applies
- `run_migrations_streaming` in `temps-database`: fires `Started`/`Finished` events per migration for caller progress callbacks
- `get_pending_migration_names` helper in `temps-database`: read-only query of unapplied migration names
- **Effective metrics storage backend on the settings page**: `GET /api/settings` now returns the `monitoring` block (with the ClickHouse DSN masked to a `clickhouse_url_set` boolean) plus an `effective_metrics_store` field. The latter reconciles the `monitoring.store` toggle with the server's `TEMPS_CLICKHOUSE_*` configuration — so the Metrics Monitoring page shows the backend the runtime is *actually* using, and warns when ClickHouse is selected but its env vars aren't configured (the runtime silently falls back to TimescaleDB in that case). Previously the page rendered client-side defaults because the response omitted `monitoring` entirely.
- **AI-agent pages endpoint** on proxy logs (`get_ai_agent_pages`): lists the pages a given AI crawler/agent visited, with input validation (empty agent rejected, unknown agent returns empty).
- Command palette entries for Sandboxes, Build Limits, and Metrics Monitoring.

### Changed
- Sandbox image is now built **on demand only** (first agent run), never at startup. The agents plugin previously warmed up the image at boot and, when the prebuilt GHCR image wasn't published yet, fell through to a multi-minute local Docker build on every startup — bogging down the host before any agent run was requested.

### Fixed
- Chart tooltips stopped working after a `forwardRef` wrapper changed `ChartTooltip`'s component type identity; recharts matches tooltip children by type so the wrapped component was silently never registered. Reverted `ChartTooltip` to the raw `RechartsPrimitive.Tooltip`.
- **Metrics settings save no longer wipes the ClickHouse DSN**: the update path now preserves the stored `clickhouse_url` when the masked payload omits it, so saving unrelated monitoring settings doesn't clear the DSN and trip the "clickhouse_url required when store is ClickHouse" validation.
- **HTTP-01 certificate renewal**: HTTP-01 domains now renew through the order-based ACME flow (`acme_orders`) instead of the direct `renew_certificate` path, so a renewal that needs validation stays visible and recoverable in the certificate-management UI rather than leaving the domain stuck.
- **Remote deployment hostnames**: remote (non-git-push) deployments built their slug from the environment slug, producing URLs like `<env>-<n>` that diverged from the git-push path. They now use the project slug so all deployment hostnames read `<project>-<n>`.
- Runtime History log viewer now defaults to a 24h time range (was 1h).
- Date-range picker preserves the user's time-of-day inputs across calendar clicks (react-day-picker normalizes selected days to local midnight, which previously wiped the time).
- `DeploymentActivityGraph` day labels flex to the responsive square heights so they stay aligned at any width.
- **Private registry credentials not forwarded on image pull**: `PullExternalImageJob` always passed `None` for auth to the Docker daemon's `create_image` call, so deployments of images from private registries failed with a 401 even when credentials were correctly configured in Settings → Docker Registry. Credentials are now fetched from `DockerRegistrySettings` at job construction time and forwarded via the `X-Registry-Auth` header when `docker_registry.enabled` is true.


## [0.1.0-beta.26] - 2026-06-03

### Added
- **ClickHouse backend for OTel traces** (opt-in, Phase 0–1 of ADR-016): when `TEMPS_CLICKHOUSE_*` is configured, distributed traces/spans are stored in and served from ClickHouse instead of TimescaleDB — making trace list, duration sort, and pagination columnar-fast at millions of traces. The `OtelStorage` span-domain methods (`store_spans`, `query_spans`, `query_trace_summaries`, `count_traces`, `get_trace`, GenAI trace reads) run natively against ClickHouse via a `ClickHouseOtelStorage` that delegates all non-span methods (metrics, logs, insights, health, quota) to the existing TimescaleDB store; the three mutable control rows stay in Postgres. Direct ingest (ClickHouse is the system of record, no fan-out). Off by default — the PG-only path is unchanged. Benchmarked query-time `GROUP BY` (23ms at 400k traces) over a materialized view for simplicity. New `spans` table (`ReplacingMergeTree`, monthly partitions, 90-day TTL) + idempotent CH migration runner.
- **ClickHouse backends for resource metrics and proxy/request logs** (opt-in, same `TEMPS_CLICKHOUSE_*` switch): when ClickHouse is configured, `temps-metrics` and `temps-proxy` route their writes and reads to ClickHouse (`service_metrics` / `proxy_logs` `ReplacingMergeTree` tables) instead of TimescaleDB, alongside the traces and analytics-events backends — so all four observability domains live in one consistent `temps` database (configurable). Off by default; the PG-only path is byte-identical. Each domain ships an idempotent CH migration runner with allowlist-validated database names.
- **`otel_trace_summaries` pre-aggregated trace table (TimescaleDB path)**: one indexed row per trace (root-span rollup — name, service, kind, duration, span/error counts), maintained by an upsert on span ingest and backfilled from existing spans in a chunked migration. Replaces the per-request `GROUP BY trace_id` over the spans hypertable, turning **sort-by-duration** from a sort-after-aggregate into an index scan (≈46× faster, `Index Scan` with no `Sort` node). This is the TimescaleDB equivalent of the columnar speed-up ClickHouse gives the traces list.
- **`temps backfill clickhouse --domain`**: copies historical proxy-logs, traces, and resource-metrics from TimescaleDB into ClickHouse so the UI shows a continuous record after enabling the backend (analytics events already replicate live via the outbox). New `--domain events|proxy-logs|traces|metrics|all` flag (defaults to `events` — existing behavior preserved). Idempotent (`_version` = source timestamp → `ReplacingMergeTree` dedup on re-runs), keyset paging on the unique `id` for proxy/spans and lossless OFFSET-within-time-slices for the keyless `service_metrics` hypertable, with an INSERT-time-TTL guard that warns up front and reconciles the landed count.
- **`temps migrate` plan + per-migration reporting**: prints the pending-migration plan up front, applies each migration one at a time with a ✓/✗ line and timing, and a final summary; on failure it stops, marks the failed migration, and lists the ones not reached. Prints "already up to date" and exits cleanly when there's nothing to apply.

### Fixed
- **Trace count/pagination parity**: `count_traces` now applies the same `status` and `min_duration_ms` filters as `query_trace_summaries`, so paginated totals match the result set (fixed in both the ClickHouse and TimescaleDB paths).
- **Trace name search**: `name_pattern` substring search now escapes LIKE metacharacters (`%`, `_`, `\`), so a literal `%` no longer matches everything (both backends).
- **Secret masking**: ClickHouse connection configs (`temps-otel`, `temps-metrics`, `temps-proxy`, and `temps-analytics-backend`) now mask the password in their `Debug` output, preventing accidental leak via `{:?}` logging.
- **GenAI event timestamps**: an unparsable event timestamp now falls back to the Unix epoch with a warning, instead of being silently relabelled to the current time (both backends).
- **ClickHouse data-model hardening (audit pass)**: dropped the `events_5m_mv` SummingMergeTree — fed by an at-least-once outbox while the base table dedups, it double-counted and was never read; added `FINAL` to the aggregating `proxy_logs` reads so a re-run/insert-retry's duplicate rows can't inflate request/error/byte counts; made metric-label JSON serialize deterministically (`BTreeMap`) so the same series can't split across the CH dedup key; and dropped the unused (0-row, no reader) `sessions` table. All via forward-only CH migrations.

### Added
- **Surrounding lines in log search** (grep -C): a "Context ± lines" input in the History Logs viewer shows N raw log lines before and after every search match, inline. Configurable 0–50 (default off, persisted), overlapping windows between nearby matches merge into one continuous block, and context lines are returned in the same `/logs/search` response — no extra requests. Backend adds `context_lines` to the log search filter and per-match `before`/`after` context (`temps-log-aggregator`); the surrounding lines are raw neighbors that ignore the level/text filters, the way `grep -C` does. The matched search term is highlighted inline in the message, and the matched ("origin") line is visually anchored against the dimmed context — mirroring Datadog's "view in context".
- **Sort traces by duration**: the Traces list now supports server-side sort on the Duration and Timestamp columns (click the header to toggle asc/desc, persisted to the URL). New `sort_by` (`start_time`|`duration`) and `sort_order` (`asc`|`desc`) query params on `GET /otel/trace-summaries`; duration sorts on `MAX(duration_ms)` per trace with a stable tie-break. Default remains newest-first by start time.
- **Database monitoring**: full metrics observability for external services (Postgres, Redis, MongoDB, RustFS/S3). New `temps-metrics` crate scrapes each enabled service into a TimescaleDB hypertable with hourly/daily continuous aggregates. Per-service monitoring page at `/storage/:id/monitoring` with hero stats, time-series charts (1h/6h/24h/7d), categorized metric groups, and threshold-based alert rules. Metrics collection is always wired up; the per-service "Enable Monitoring" toggle is the single control.
- **RustFS OTLP metrics ingest**: RustFS containers push OTLP metrics to Temps via a service-scoped `si_` API key (new `MetricsIngest` role) sent in the `Authorization` header. Enabling monitoring on an S3/RustFS service auto-provisions the key and restarts the container with the OTLP endpoint configured. Default RustFS image bumped to `1.0.0-beta.6` (first line with reliable OTLP header support).
- Per-service metrics collectors: PostgreSQL (31 metrics), MongoDB (32, incl. uptime/page-faults/asserts), Redis (17, incl. ops/sec, commands, network), RustFS/S3 (cluster capacity, objects, operations, process stats).
- **Internal URL** platform setting (`internal_url`): how service containers reach the Temps API from inside the Docker network (OTLP metrics, agent callbacks). Resolves via setting → `TEMPS_INTERNAL_API_URL` env → `http://host.docker.internal:<proxy-port>`. Editable in Settings.
- "Last received at …" freshness indicator on monitoring views, backed by a tiny `service_metrics_status` table (O(1) lookup, upserted on write — no hypertable scan).
- Configurable auto-refresh interval (Off / 5s / 10s / 30s / 1m) on the monitoring page, persisted in localStorage.
- Service upgrade support for RustFS (was previously "not implemented").
- **`temps migrate` command** — applies pending database migrations as an explicit, decoupled step with the server stopped and **no timeout**. Recommended upgrade flow for production and large databases: download the new binary → `temps migrate --database-url=…` → restart the server. `temps serve` still auto-applies pending migrations on boot for simple installs, so this is fully backward compatible.
- `temps doctor` now reports pending migrations explicitly ("N applied, up to date" vs "N applied, M PENDING — run `temps migrate` before restarting the server"), and detects an uninitialized schema.
- Default alert rules for MongoDB and RustFS/S3 services (MongoDB: current connections, queued reads/writes, WiredTiger cache pressure, replication-buffer pressure, cursor timeouts; RustFS: offline nodes, low free capacity warning/critical). The `AlertEvaluator` also back-seeds default rules for every metrics-enabled service on startup (idempotent `ON CONFLICT DO NOTHING`), self-healing services whose engine had no default seeds when monitoring was first enabled.
- Service-scoped alarms now record which external service triggered them via a new nullable `alarms.service_id` column (FK `ON DELETE SET NULL`, indexed; migration `m20260601_000010`). Previously a database-metric alarm only stored its `project_id`, so an operator (or the notification email) saw "Project N" with no indication of which service breached. The `AlertEvaluator` threads `service_id` through the alarm context for service-scoped rules.
- **Per-database breakdown for Postgres monitoring**: a new "Databases" section on the monitoring page with a database selector (defaults to "All databases", or pick a single database). Postgres per-database metrics (size, cache-hit, tuple-fetch, commits, rollbacks, deadlocks, temp files, tuple DML) are now scoped here — a Postgres instance can host many unrelated databases, so showing one collapsed number was misleading. Genuinely instance-wide metrics (connections, locks, table tuple/scan stats, WAL, replication) stay in the top tiles. New endpoint `GET /external-services/{id}/metrics/by-database` and store method `query_latest_by_label` group the latest value of each metric by `datname`.
- The service monitoring page now sets a descriptive document title (`<service> · Monitoring`) instead of the generic "Temps".
- **`temps backfill clickhouse` subcommand** — standalone, one-shot data migration tool for moving historical events from the PostgreSQL/TimescaleDB `events` hypertable into ClickHouse. Runs out-of-process from `temps serve`, so it never contends with the live fan-out worker's outbox locks and does not double the PG write load on the primary during migration. Reuses the live fan-out's `ChEventRow` shape and `row_to_ch` mapper as the single source of truth — backfilled rows are byte-identical to live-ingested rows, and re-runs are safe via ClickHouse's `ReplacingMergeTree(_version)` dedupe on `event_id`. Flags: `--from` / `--to` RFC3339 window (defaults to MIN/MAX timestamp), `--project-id` filter, `--batch-size` (default 10k), `--chunk-days` (default 1), `--rate-limit-events-per-sec` throttle for live-server backfills, `--apply-migrations` to create CH schema, `--dry-run` to preview the chunk plan, and `--resume` with checkpoint at `$TEMPS_DATA_DIR/clickhouse-backfill.state`. Progress bar via indicatif.
- **AI agents analytics overview**: the AI Agents page (`/analytics/ai-agents`, now in the analytics sidebar) is a full overview rather than two tables. A new **"AI agents over time"** stacked bar chart plots crawler request volume per time bucket, split by provider or agent, with a per-bucket tooltip listing each series' count and a total. Below it, a card grid surfaces **Top Agents**, **Top Providers**, **Crawl purpose** (model training / search indexing / user fetch, from the AI taxonomy), **Response status** (2xx/3xx/4xx/5xx — are crawlers being served or hitting broken/blocked pages?), and **Top Pages crawled**. All read from request logs (`is_bot = true`). New endpoints: `GET /proxy-logs/stats/ai-agents/timeline` (time-bucketed, server-side gap-filled via a `generate_series` spine so the x-axis stays continuous; bucket width auto-selected from the window or overridden via `?bucket=`, validated against an interval allowlist) and `GET /proxy-logs/stats/ai-status`.

### Changed
- **Compression CODECs on the ClickHouse telemetry tables** (`spans`, `service_metrics`, `proxy_logs`, `events`): `Delta`/`DoubleDelta` on timestamps, `Gorilla` on metric values, and `ZSTD` on the JSON-text columns, applied via non-destructive `ALTER … MODIFY COLUMN`. Measured on existing data: `service_metrics` −52%, `proxy_logs` −34% on disk.
- Cumulative counter metrics (`_total`/`_count`) now display the running total in stat tiles and rate-of-change in charts, computed via a TimescaleDB LAG window query.
- `service_metrics` raw retention 7 → 30 days; daily-aggregate retention 2 years → 1 year.
- MinIO removed from the S3 service creation UI; RustFS is now the sole default object-storage engine.
- Service-detail monitoring card no longer polls the metrics API when monitoring is disabled for that service.
- Runtime/container log viewer redesigned to match the history viewer: ANSI color rendering, inferred severity levels with colored badges, parsed timestamps, toggleable timestamp/level/service columns (persisted per-browser), and fixed-height virtualized rows. The live tail buffer is bounded at 1000 lines (older lines fall off the top, like `docker logs --tail`) so a chatty service can't freeze the tab.
- Database migration startup timeout raised from 120s to 600s, and a 15s `lock_timeout` is set before applying so a migration blocked on a lock fails fast instead of hanging the whole startup window. Migrations that exceed the window now print guidance to run `temps migrate` manually with the new binary before restarting.
- Migrations are now tolerant of unknown/extra applied rows: a migration history containing rows this binary doesn't define (e.g. from a newer build or the Enterprise Edition) no longer blocks startup — only this binary's own pending migrations are applied.
- The AI Agents overview and the visitor Journey tab now default to **Last 24 hours** (was Last 7 days), matching the rest of the analytics tabs.

### Fixed
- Project sidebar now expands the correct section (e.g. Analytics) when a deep sub-route is hard-refreshed or opened from a direct link — previously it fell back to the top-level project nav because the drill-down state was only seeded on mount (before the project had loaded) and never re-synced. Sub-route matching is also prefix-aware, so deeper pages (e.g. the AI Agents "View all" tables) keep their section's nav item highlighted.
- The AI Agents overview no longer duplicates its breakdown cards with full tables inline. The ranked, searchable Agents / Pages-crawled tables (with Unique IPs and per-page agent drill-down) moved to a dedicated "View all" page (`/analytics/ai-agents/all`), reached from the Top Agents, Top Providers, and Top Pages cards. The Top Providers link opens the tables pre-grouped by provider via `?group=provider`.
- Metrics scraper releases its per-service in-flight slot via an RAII drop guard, preventing a permanent scrape stall if a collector task panics.
- Monitoring queries stop polling on permanent errors (monitoring disabled / not found / 503) instead of retrying every few seconds.
- Retention migration uses `if_exists` (not the non-existent `if_not_exists`) on `remove_retention_policy`, fixing a fresh-install setup crash.
- Service detail refetches after a service upgrade so the new image/status is reflected immediately.
- Redis counter metrics (`evicted_keys_total`, `keyspace_hits_total`, `keyspace_misses_total`, `expired_keys_total`) were mislabelled as `Gauge` instead of `Counter`, so their raw cumulative values were charted directly instead of as a rate of change.
- Service-scoped (database) alarms no longer fail to fire: `alarms.environment_id` and `alarms.deployment_id` are now nullable. The evaluator previously wrote sentinel `0` values, which violated the environment/deployment foreign keys and dropped every database alarm. The FKs are recreated `ON DELETE SET NULL` so historical alarms survive environment/deployment deletion, and cooldown de-duplication now matches `IS NULL` per scope level.
- MongoDB metrics scraper now connects with `?authSource=admin` (matching how the provider provisions the service user as a root user in the `admin` database) instead of deriving authSource from the path database, which caused SCRAM "Authentication failed" and zero metrics. Unauthenticated MongoDB services connect without credentials.
- Postgres connection metrics now count only client backends (`backend_type = 'client backend'`) instead of every row in `pg_stat_activity`. Engine background processes — `walwriter`, `checkpointer`, `bgwriter`, `autovacuum launcher`, `archiver`, `logical replication launcher`, walsenders, and PG18's async `io worker` pool — were being lumped into "Connections Other" (~8–10 phantom connections that never count against an app's budget). Added `pg.connections` (a gauge: sum of client-backend states) and made it the headline tile/chart, so an idle connection pool reads as "N connected" instead of a misleading "0 active". Named without a `_total` suffix on purpose — `_total`/`_count` metrics are charted as a rate-of-change, which would flatten a steady gauge to 0.
- Postgres per-database metrics (database size, cache-hit ratio, deadlocks, commits/rollbacks, temp files, tuple DML, fetch ratio) showed only **one arbitrary database** in the stat tiles instead of the whole instance — the store's latest-value query keeps one row per metric name via `DISTINCT ON`, so it picked a random `datname` series. The collector now also emits an **unlabelled instance-wide aggregate** for each (SUM for counters/sizes, correctly *recomputed* weighted ratios for cache-hit/fetch — averaging per-db ratios is mathematically wrong), the store prefers the aggregate series — the one with the **fewest label keys** (the aggregate carries only base labels like `engine`; per-series rows add `datname`) — for both tiles and charts, and the scraper's counter-delta baseline is now keyed by labels so per-`datname` series and the aggregate don't clobber each other. The hourly/daily continuous aggregates were recreated with `labels` in their `GROUP BY` (migration `m20260601_000009`) so ranges > 7 days are correct too; per-database series are surfaced in the new "Databases" breakdown section.
- Live log viewers (runtime logs + container logs) rendered overlapping lines: rows were fixed at 22px tall while long messages (e.g. JSON payloads) wrap to several visual lines, so wrapped content spilled onto the rows below. The virtualizer now measures each row's real height (`measureElement`) instead of assuming a single line.
- Service monitoring UI (`MonitoringCard`, `ServiceMonitoring`) now calls the generated OpenAPI SDK for every metrics endpoint instead of hand-rolled `fetch` helpers, removing the risk of locally-typed request/response shapes drifting from the backend contract.
- **HTTP-01 certificates silently stopped auto-renewing after a single failed attempt.** The renewal scan (`find_expiring_certificates`) filtered to `status = "active"`, but a failed renewal leaves the domain in `pending_http`/`challenge_requested` — so it was never reconsidered and the certificate expired. The scan now reconsiders any domain with an expiring stored certificate regardless of current status, and each HTTP-01 renewal attempt logs the domain status and days-until-expiry for diagnosis.
- **Domain detail page stranded the user after a cancelled or otherwise terminal ACME order.** Every recovery action was gated on a live order with populated challenge data, so a `cancelled`/`invalid`/`revoked`/`deactivated` order left an empty actions menu and no way to start over. Terminal orders are now treated as absent: "Create new order" appears as the primary action and in the kebab menu whenever the domain is awaiting issuance, for both DNS-01 and HTTP-01.
- Domain "Expires soon" badge now shows the actual time remaining (e.g. `Expires in 18h`, `Expires in 4d`) and turns red within 48 h or after expiry, instead of a static label. Certificate and ACME-order dates across the domain UI now render in the viewer's locale via `Intl.DateTimeFormat` instead of a hardcoded `MMM d, yyyy` format.
- **`temps-platform-setup` skill: HIGH-risk security findings** (`skills/temps-platform-setup`) — remediated the issues flagged by the Gen Agent Trust Hub audit. Replaced the `curl … | bash` installer with a download → review → run flow (`REMOTE_CODE_EXECUTION`); swapped every literal secret (`ghp_…`, `glpat-…`, `dop_v1_…`, `AKIA…`/`wJalr…`, `tk_…`, Cloudflare token, zone id) for `<YOUR_*>` placeholders and documented the real env-var fallbacks verified against `temps-cli` clap args (`CREDENTIALS_UNSAFE`); fixed wrong DNS flag names (`--route53-access-key` → `--aws-access-key-id`, `--route53-secret-key` → `--aws-secret-access-key`); and added a Security Considerations section treating external command output (logs, `git clone`, `env import`, error events) as untrusted to blunt prompt injection (`PROMPT_INJECTION`).

### Security
- **Metric names are validated against an allowlist (`[a-zA-Z0-9_.:-]`) at every write path**, not just on read. `TimescaleMetricsStore::write_batch` and both OTLP ingest paths (`do_ingest_service_metrics` for `si_` service tokens, and the deployment path via `otlp_to_store_point`) now drop any point whose name falls outside the allowlist before it is interpolated into SQL. Previously only the query path validated names, leaving the write path — which receives attacker-controllable names off the OTLP wire — as the weaker link.
- **`si_` service-ingest tokens are now rate-limited** (per `service_id`, 600 requests/60s), closing a gap where the service-metrics ingest path bypassed the rate-limit/quota checks applied to project tokens. A runaway or compromised exporter can no longer flood the metrics write channel / TimescaleDB; excess requests get HTTP 429 (`OtelError::ServiceRateLimitExceeded`).
- **Closed IDOR gaps in the metrics handlers.** `toggle_service_metrics` now verifies the service belongs to the caller before mutating it (previously any `ExternalServicesWrite` holder could enable monitoring on another tenant's service — which provisions an `si_` key and restarts that container). The deployment metrics handlers (`get_deployment_metrics_range`, `get_deployment_metrics_latest`, `toggle_deployment_metrics`) now verify deployment ownership via a new `assert_deployment_owned_by_caller` check, returning 404 (not 403) so other tenants' resources aren't enumerable.


## [0.1.0-beta.25] - 2026-05-31

### Added
- **Per-project AI Crawler Activity feed** (`web`) — a new "AI Crawlers" entry in the project Observe sidebar (`projects/:slug/ai-crawlers`) shows a chronological feed of AI-agent requests (ClaudeBot, GPTBot, PerplexityBot, …) hitting that project's sites, newest first. Complements the aggregated AI-agent views (ranked agents, top crawled pages) with a time-ordered stream. Reads the existing `GET /proxy-logs?is_ai_agent=true` endpoint via the generated SDK; provider/agent filters and a configurable page size (25/50/100/200) are synced to the URL. Scoped to the current project.
- CLI: `session-replay` commands.

### Fixed
- CLI: `--api-key` is now pinned for the login validation request, so `temps login --api-key=<key>` validates the supplied key even when an active context already holds one (previously `getApiKey()`'s priority order — env > context > secrets — could shadow the supplied key and validate the wrong one).


## [0.1.0-beta.24] - 2026-05-30

### Added
- CLI: `TEMPS_CONTEXT` environment variable pins the active context for a shell/CI session without mutating `.contexts.json`; surfaced in `context ls`, `context use`, and `whoami`, with a one-time warning when it names a context that doesn't exist.

### Changed
- Proxy: the load balancer now binds its listeners (80/443) immediately and loads the route table asynchronously, instead of blocking startup on a full route load. Cuts proxy start time from seconds to milliseconds; a request arriving before the first load briefly waits for it rather than being misrouted.

### Fixed
- CLI: api-key login now validates against the server passed via the positional argument or `--url` before checking the key, so logging into a named server no longer validates against the active context / localhost default and wipes credentials on failure.
- Proxy: AI crawlers (ClaudeBot, OAI-SearchBot, PerplexityBot, GPTBot, etc.) were stored with loose user-agent substrings (e.g. `"Bot/"`) instead of their canonical taxonomy names, causing the AI Agents analytics page to show no agents. The live proxy-log ingest path now runs `ai_agent_detector` first.


## [0.1.0-beta.23] - 2026-05-29

### Added
- **Self-service password reset UI** (`web`) — two new public routes complete the password-reset flow. `/forgot-password` requests a reset link (enumeration-safe: always confirms "check your inbox" on 200, surfaces an inline "no email provider configured" alert when reset is unavailable), and `/auth/reset-password` is the target of the `{base_url}/auth/reset-password?token=…` link in the reset email — it reads the token from the query string and sets a new password, with a Zod schema mirroring the backend complexity rules (≥8 chars, upper/lower/digit/special) so users get inline validation instead of a round-trip 400. A "Forgot password?" link on the login form is gated on the server's `password_reset_available` flag, so it only shows when an email provider can actually deliver. Back-to-login and post-reset navigation target `/` (this console has no `/login` route — `ProtectedLayout` renders the login screen at the unauthenticated root).
- **Per-OIDC-provider `trust_idp_email` opt-out for the `email_verified` claim gate** (`temps-auth`, `temps-entities`, `temps-migrations`, `web`) — Temps refuses to link or JIT-provision SSO accounts unless the IdP's ID token asserts `email_verified: true`, because an attacker who can register `victim@example.com` at the IdP without verifying it can otherwise take over the victim's existing password/magic-link account on first SSO login. That security check turns into pure noise against corporate IdPs where an admin controls every user account and the IdP simply doesn't emit the claim — Okta's Org Authorization Server is the canonical example (no Claims tab to add it, no Token Preview to confirm). Migration `m20260526_000002_add_trust_idp_email_to_oidc_providers` adds a new `oidc_providers.trust_idp_email boolean not null default false` column, and `oidc_service::resolve_user` now skips both gates (the link-existing-account path and the JIT-provision path) when the per-provider flag is set. The local `users.email_verified` column is still only flipped to `true` when the IdP actually asserted it — `trust_idp_email` bypasses the *gate*, not the truth of what the IdP said. Every bypass logs a `warn` line on target `temps_auth::oidc::trust_bypass` so it's visible in operations. The OIDC provider edit form in the console exposes the flag as a switch in the "First-login policy" section with explicit security warnings; the create/update audit rows surface the flag value (created) and field-changed list (updated) so an auditor can answer "who turned this on and when" without reading source. Defaults `false` on upgrade — admins must opt in per provider.
- **Control-plane build limits to stop deploys from saturating the host** (`temps-core`, `temps-deployer`, `web`) — `docker build` used to run unbounded and grab 50% of host CPU/RAM per build, so three simultaneous deploys could effectively pin the box (3 × 50% memory headroom is well past 100% on a busy server). Adds a process-wide `tokio::sync::Semaphore` inside `DockerRuntime` that gates every `build_image` / `build_image_with_callback` call to `AppSettings.build_limits.max_concurrent`. When the semaphore is full, additional builds queue (log message + `[BUILD QUEUED]` line in the build log stream) — they do not fail. Per-build CPU/memory caps are now driven by `BuildLimitsSettings { max_concurrent, cpu_limit_cores, memory_limit_mb }` and forwarded to `BuildImageOptions { memory, cpuquota, cpuperiod }`; the legacy 50%-of-host heuristic stays as a fallback when either dimension is 0 so existing installs see no behaviour change until an operator visits the new settings page. Worker nodes (those run via `temps cli agent`) skip the plugin entirely so they keep the unbounded behaviour — by design, since each worker is dedicated hardware. New admin settings page at **Settings → Infrastructure → Build Limits** (`web/src/pages/settings/BuildLimitsPage.tsx`) exposes the three knobs with min/max validation. Note: Docker BuildKit's memory field is `i32` bytes (≈ 2 GiB), so per-build memory caps above 2048 MB are silently clamped — same upstream limitation as before. Defaults: 2 concurrent builds, 0/0 resource caps (= legacy heuristic).
- **On-demand preview environments by default** (`temps-projects`, `temps-deployments`) — new project-level setting that puts every newly auto-created preview environment into on-demand mode, so feature-branch previews scale to zero when idle instead of running 24/7. Migration `m20260526_000001` adds three columns to `projects`: `preview_envs_on_demand` (bool, default false), `preview_envs_idle_timeout_seconds` (int, default 300), `preview_envs_wake_timeout_seconds` (int, default 30). When the flag is on, `create_preview_environment` seeds the new env's `deployment_config` with `on_demand=true` plus the project's idle/wake timeouts; other knobs (cpu/memory/replicas/security) stay None so the env → project → global inheritance still applies. Only affects previews created **after** the flag is enabled; existing environments keep their current `deployment_config`. Service validates timeouts (60..=86400s idle, 5..=120s wake) before writing. Settings UI exposes the toggle plus the two timeout inputs gated behind the existing "Enable Preview Environments" switch, with client-side zod range validation matching the backend.
- **GitLab logo on the project Git section** (`web`) — when a project is connected via GitLab (OAuth App or self-hosted), the repository card now shows the orange tanuki logo instead of the GitHub Octocat. New `web/src/icons/Gitlab.tsx` matches the existing `Github.tsx` shape (24×24 viewBox, official `#FC6D26` fill). The upstream-repo link's fallback URL is now provider-aware too, so GitLab projects with a missing `git_url` land on `gitlab.com/<owner>/<repo>` rather than `github.com/...`.
- **Generic SMTP email provider for "I only have SMTP credentials" setups** (`temps-email`, `web`) — third `EmailProviderType` alongside AWS SES and Scaleway, for operators who can't (or don't want to) use the upstream provider's admin API. The canonical case is AWS where IAM gives you SES SMTP creds (`email-smtp.<region>.amazonaws.com:587`) but not API access, but it equally covers Sendgrid, Mailgun, Postmark, self-hosted Postfix, and local Mailpit. New `temps-email/src/providers/smtp.rs` sends via lettre with STARTTLS / implicit-TLS / plain modes; loopback hosts (`localhost`/`127.0.0.1`/`::1`) auto-loosen TLS verification so local testing just works. Domains added under an SMTP provider are *imported*: `create_identity` returns an empty `DomainIdentity` (no SPF/DKIM/MX), `verify_identity` reports `Verified` immediately, and `delete_identity` is a no-op — the user owns DNS at the upstream mail server (e.g. the AWS SES console). New `SmtpCredentialsRequest` payload on `POST /api/email-providers` with `host`, `port`, `username`, `password`, `encryption` (`starttls`/`tls`/`none`), and `accept_invalid_certs`. Frontend provider type dropdown grows a third option with a Server icon, a contextual warning that DNS is the user's responsibility, and per-mode port autosuggest (587/465/25 for starttls/tls/none).
- **`PATCH /api/email-providers/{id}` with frontend Edit dialog** (`temps-email`, `web`) — partial-update endpoint with a matching Edit dialog for SES, Scaleway, and SMTP. The previously disabled `Edit` dropdown item is now live for all three provider types. `provider_type` is immutable (the encrypted credentials format is fixed at creation time) but `name`, `region`, `is_active`, and credentials are individually updatable. **Omitting a credential block preserves the stored secret**, so operators can rename or change region without re-typing passwords — and the frontend exploits this: secret fields render blank with "Leave blank to keep current" helper text, and only fields that diverged from the loaded state are sent on submit. Service layer rejects empty names and rejects credential variants that don't match the existing `provider_type` (`Cannot change provider type from <a> to <b>`). New `EMAIL_PROVIDER_UPDATED` audit event logs only the field *names* that changed — never their values — so the audit log never leaks secret material. Edit dialog locks the `provider_type` indicator and prefills non-secret fields from the masked response: for SMTP that includes host / port / encryption / `accept_invalid_certs`, all derived from the existing `get_masked_credentials` shape. 12 new unit tests in `temps-email` covering rename, no-op, empty-name rejection, type-mismatch rejection, credential rotation re-encryption (ciphertext changes), and credential preservation (ciphertext byte-identical when caller omits the block).

### Changed
- **Branded notification-provider health-check email** (`temps-notifications`) — the email-provider health check was sending a bare `"Health check email"` body with no Temps branding, which landed in inboxes looking like a misconfigured debug ping rather than a real platform message. `EmailProvider::email_health_check` now builds a `multipart/alternative` message matching the regular notification template — dark header bar with the Temps mark, info badge ("Health check"), human-readable title ("Notification provider is reachable"), a monospace details table (instance hostname, SMTP relay `host:port`, From address, timestamp), and the standard footer. Subject changed from `"Health Check"` to `"[Temps] Notification provider health check"`. Same path also picks up three safety fixes: empty `to_addresses` no longer panics on `to_addresses[0]` (the SMTP `test_connection()` succeeding is still a valid health signal even without a recipient); a malformed recipient string no longer aborts the check; and failures now log which stage tripped (`connection` vs `send`) plus the SMTP host and recipient so operators can act on the failure. Instance label reads `TEMPS_HOSTNAME` → `TEMPS_PUBLIC_HOSTNAME` → `"temps instance"`. Two new unit tests pin the brand markers and the table-based layout so future edits can't silently regress to flex/grid (which breaks in Outlook).
- **Branded "test email" body for the email-provider Send Test action** (`temps-email`) — the "Send Test Email" flow on the Email Providers page used a plain `<h1>` + `<hr>` layout with the raw provider_type enum (`ses` / `scaleway` / `smtp`) printed verbatim, so the test message looked unrelated to Temps and exposed an internal identifier instead of a human label. `ProviderService::send_test_email` now renders the same dark-header / success-badge / details-table / footer layout used by the notification email, with the provider_type rendered through `pretty_provider_type` (`AWS SES` / `Scaleway` / `SMTP` / uppercased fallback for unknown variants). Subject changed from `"Temps Email Provider Test - <name>"` to `"[Temps] Email provider test — <name>"`. Provider name and region are now HTML-escaped before interpolation via a new `escape_html` helper, closing a reflected-XSS surface where a user-controlled provider name (e.g. `<script>…</script>`) would land verbatim in the recipient's inbox. Extracted into free `render_test_email_html` / `render_test_email_text` functions so the body is unit-testable without a `ProviderService` instance; 5 new tests cover the brand markers, the Outlook-safe `<table>` layout invariant, the pretty-name helper, the HTML-escape helper, and the plaintext fallback. Verified end-to-end against a local Mailpit container.

### Fixed
- **Password reset actually works now — and stops leaking reset links to Slack/admins** (`temps-core`, `temps-notifications`, `temps-auth`, `web`) — the password-reset endpoints, email template, and audit logging all existed, but the feature was dead on two counts. `AuthService::is_email_configured()` was hardcoded to `return false`, so `POST /auth/password-reset/request` always answered 503 and the console hid the entry point entirely. Worse, the auth emails routed through `NotificationService::send_email`, which converts the message into a generic `Notification` and fans it out to **every** enabled notification provider — so a reset link would be posted to Slack/webhook channels, and the email provider itself ignores the message recipient and instead mails its configured `to_addresses` **plus all admin users**, meaning the link reached admins and never the user who requested it. Adds two recipient-respecting trait methods to `temps-core::NotificationService` (`send_transactional_email`, `is_email_provider_configured`, both with safe defaults); the `temps-notifications` impl loads **only** enabled `provider_type = "email"` providers, sends directly to `message.to`, takes the `From` from the provider's own config, and bypasses the alert fan-out + throttling. `AuthService::is_email_configured()` is now async and checks specifically for an enabled email provider (a Slack-only setup no longer counts), and the three auth emails (password reset, email verification, magic link) all send via the transactional path. `email_status` / `password_reset_available` now reflect reality, so the "Forgot password?" link appears exactly when reset links can be delivered. 87 `temps-notifications` + 5 `temps-auth` email tests pass.
- **PR/MR preview comment now updates on deployment cancel** (`temps-git`, `temps-deployments`) — cancelling a deployment used to leave the sticky comment stuck on "🚧 Deploying preview" because two pieces of the pipeline were missing. `PrCommentListener::handle_job` had no `Job::DeploymentCancelled` arm (the catch-all silently swallowed it) and `CommentPhase` had no `Cancelled` variant. Adds a `CommentPhase::Cancelled` body ("⛔ Preview deployment cancelled" with commit SHA + optional logs link), a matching listener arm that loads the deployment row and edits the existing sticky comment in place, and a publish step in `DeploymentService::cancel_deployment` so user-initiated cancels emit the event (best-effort: a queue failure logs a warning and lets the cancel succeed, mirroring the success/failure paths). Works identically on GitHub and GitLab — the provider split in `GitPrCommenter::upsert_inner` is body-string-agnostic, and the GitLab `gitlab_upsert` notes path edits the existing sticky note in place via the HTML marker. Required token scope is unchanged: GitHub Apps need `pull_requests:write`, GitLab tokens need `api`.
- **Useful toast when deleting a git provider with active connections** (`web`) — `GitProviderDetail.tsx`'s delete mutation had a custom `onError` that interpolated `err.message` into the toast body, but the API returns an RFC 7807 Problem Details object (no `.message` field), so the toast rendered the raw JSON (`{"detail":"Cannot delete provider GitLab Gala because it has 1 connection(s)","title":"Invalid Configuration"}`). Removed the local handler and added `meta: { errorTitle: 'Failed to delete provider' }` so the existing global mutation handler in `App.tsx` renders the title as the toast title and the API's `detail` as the description — matching the pattern used by every other delete mutation in the console. Dialog close moved to `onSettled` so it closes on both success and failure paths.


## [0.1.0-beta.22] - 2026-05-26

### Added
- **Vercel-style sticky PR/MR preview comments**: when a deployment for a branch with an open pull request (GitHub) or merge request (GitLab) moves through created → succeeded/failed, Temps posts or updates a single sticky comment with the preview URL. The comment is keyed by a hidden HTML marker (`<!-- temps-preview:project=N:env=N -->`) scoped per `(project, environment)` so subsequent updates edit in place rather than spam the PR. New `temps-git::PrCommenter` trait + `GitPrCommenter` impl dispatches to GitHub REST (`/repos/.../pulls` + issue comments) or GitLab REST (`/merge_requests` + notes). A background `PrCommentListener` subscribes to existing `Job::DeploymentCreated` / `DeploymentSucceeded` / `DeploymentFailed` broadcasts — no schema changes, no new event types, no wiring required in `temps-deployments`. Graceful degradation: missing git connection, no open PR, unsupported provider, or 403/401 from the provider all log at warn/debug and never block deploys. Newly-created GitHub Apps already request `pull_requests:write` in the manifest; existing installations without that scope hit a logged 403 and would need to upgrade their app permissions.
- **Branded 404 page for unknown hosts behind the admin gate**: when `temps-core::admin_gate` denies a request to a host that has no deployed app (typically a domain mid-DNS-propagation), the proxy used to return a 50-byte `<h1>404 - Not Found</h1>` body indistinguishable from nginx/haproxy. New `temps-proxy::branded_404::render(host, request_id)` builds a self-contained dark-mode HTML page with the Temps `t` brand mark, a `DEPLOYMENT_NOT_FOUND · 404` chip, the requested host, the request ID for support correlation, and a link to the domains docs. Zero external assets (no remote fonts, scripts, or images) so the page renders even when the host hasn't resolved DNS to anything else yet. Reflected-XSS-safe: `Host:` header and request ID are HTML-escaped before interpolation. `meta robots noindex,nofollow` so misconfigured domains don't pollute search results.

### Changed
-

### Fixed
-

### Security
- **High: CLI device-flow API key stored in plaintext column** (`temps-auth`) — `cli_login_sessions.api_key_plaintext` persisted the freshly-minted, full-power API token (`tk_...`, valid 90 days) as raw `TEXT` until the next CLI poll consumed it (typically seconds, but up to 900s on session expiry). Every other long-lived credential in the codebase (OIDC client_secret, environment-variable secrets, workspace preview passwords) used `EncryptionService` AES-256-GCM at rest; this column was the exception. A DB-read primitive (leaked backup, future SQLi, nosy DBA) yielded directly-usable session tokens. Fixed by encrypting before persist in `cli_device_approve` and decrypting at delivery in `deliver_approved`; the column type stays `TEXT` (now holding ciphertext, named historically). Two new error variants `EncryptionFailed` / `DecryptionFailed` both map to 500 so crypto state isn't leaked to the CLI. Pinned by two new unit tests: round-trip lossless + ciphertext-doesn't-contain-plaintext, and AEAD tamper-detection (flipped byte → decryption fails closed). No migration needed — sessions that were `approved` pre-upgrade will fail decryption and resolve as `ExpiredToken`, forcing one re-login.
- **High: Shell command injection via Postgres password in WAL backup engines** (`temps-backup`) — `query_current_wal_lsn` in both `engines/postgres_walg.rs` and `engines/postgres_cluster.rs` built a `sh -c "PGPASSWORD={pwd} psql -U {user} -d {db} …"` command string with `format!()` and no shell escaping. A password containing `'; rm -rf /; #` would break out of the wrapper and execute arbitrary shell inside the Postgres container (which the backup engines run with `network_mode: host` and frequently as root). The matching sibling functions in the same crates already used the safe pattern (env vars via `bollard::exec::CreateExecOptions::env`); these two LSN-probe helpers had been overlooked. Fixed by extracting `build_lsn_exec_args(pg) -> (cmd, env)` in both files: credentials now travel exclusively in `PGUSER` / `PGPASSWORD` / `PGDATABASE` env entries (which `psql` honors natively) and the cmd vector contains only literal psql flags. Also incidentally fixes a Medium "PGPASSWORD in /proc/<pid>/cmdline" exposure. Pinned by 6 unit tests (per file × 3 cases): credentials absent from cmd even with adversarial input, env contains all three credentials byte-for-byte, empty-string inputs don't crash.
- **Critical: Agent run deployment tokens had wildcard scope and no revocation** (`temps-agents`) — tokens were minted with `FullAccess` permission and a fixed 2-hour TTL regardless of how long the run actually took, so a leaked token granted unrestricted API access for up to two hours after the run finished. Expiry is now `timeout + 120s` and the token is explicitly revoked on every exit path (success, failure, cancel, no-changes). `FullAccess` is retained pending a purpose-built `AgentRunWrite` permission (`TODO(0.2.0)`) because the workspace memory API rejects narrower scopes.
- **Critical: AI Gateway BYOK SSRF via `X-Provider-Base-URL`** (`temps-ai-gateway`) — the header value was forwarded verbatim to the upstream HTTP client, letting any user with `AiGatewayExecute` redirect gateway traffic to `169.254.169.254` (cloud IMDS), internal services, or any private host. The header is now validated through `temps_core::url_validation::validate_external_url` before use.
- **Critical: `pg_notify` SQL injection in deployment fan-out** (`temps-deployments`) — `mark_deployment_complete` built the notify payload via `format!()` with manual apostrophe escaping, making it injectable through a project name or deployment ID. Replaced with `Statement::from_sql_and_values` bound parameters. Two other `Statement::from_string` sites in the crate were audited and documented as safe (constant table identifiers / hardcoded channel names).
- **Critical: Git remote URL SSRF via libgit2** (`temps-projects`) — `git_url` accepted `file://` (local file read), `git://` (unauthenticated + unencrypted, deprecated by GitHub in 2022), `ssh://` and SCP-style `git@host:repo` (internal-host probing + cred leakage), and `http://` (plaintext creds in URL). All handed to libgit2 without validation. A new `validate_git_url` helper in `temps_core::url_validation` now enforces **https-only** and rejects private IP ranges; credentials are redacted from error messages via `redact_url_password`. Self-hosted git on plain HTTP must terminate TLS in front (caddy / nginx).
- **High: Legacy `/webhook/github` route had no signature verification** (`temps-git`) — the route was deleted entirely. The verified `github_webhook_events` endpoint is the only inbound path now.
- **High: GitLab webhook accepted requests from tokenless projects** (`temps-git`) — the signature check branched `None => true`, allowing unsigned webhooks from projects that never stored a token. Flipped to `None => false`; operators must re-enroll the webhook in GitLab to issue a signing token.
- **High: XFF spoofing in audit-log IP attribution** (`temps-auth`) — `X-Forwarded-For` was trusted unconditionally, so any client could spoof the IP recorded in the audit log. A new `resolve_client_ip` helper (shared with the rate limiter) trusts `XFF` only when the direct TCP peer is loopback.
- **High: Worker `private_address` SSRF on node registration** (`temps-deployments`) — the field accepted arbitrary strings including `169.254.169.254`. Now validated against a blocklist covering loopback, link-local (including AWS IMDS), multicast, broadcast, unspecified, and their IPv6 equivalents; RFC-1918 and public IPs remain valid.
- **High: Open redirect via Host header in GitHub OAuth callback** (`temps-git`) — the post-OAuth redirect URL was assembled from the `Host:` header, allowing a crafted request to redirect victims to an attacker-controlled domain. Now uses the configured `external_url` from settings; startup fails with a clear error if `external_url` is unset.
- **High: Importer `base_url` SSRF (Coolify, Dokploy)** (`temps-import`) — `ImportCredentials.base_url` flowed into importer HTTP calls without validation. Now validated in `ImportOrchestrator::discover` and `create_plan` before any outbound request is made.
- **High: Zip-slip and tar-slip in static bundle extraction** (`temps-deployments`) — `extract_tar_gz` and `extract_zip` used a lexical `starts_with` check that did not reject `..` components, and `entry.unpack()` followed symlinks outside the destination, providing a read primitive for arbitrary files including the encryption key. Now: every entry path is checked for `Component::ParentDir` and `Component::RootDir` before extraction, and `Symlink`/`Link` entries are skipped with a warning.
- **High: Decompression-bomb limit missing in sandbox tar upload** (`temps-sandbox`) — no cap on total or per-entry decompressed size. A 200 MiB aggregate and 50 MiB per-entry limit are now enforced via header-size pre-check and a saturating cumulative counter.
- **High: Bundle `data_dir` path traversal via unsanitized DB path** (`temps-deployments`) — `download_bundle` joined `data_dir` with a DB-stored `bundle_path` without canonicalizing, allowing a tampered DB row to escape the data directory. Both paths are now canonicalized and the result must be a prefix of `data_dir`; upload time applies the same check.
- **High: Blob downloads missing `Content-Disposition: attachment`** (`temps-blob`) — blobs served as `text/html`, `image/svg+xml`, or `application/javascript` were rendered in the browser, enabling stored XSS via uploaded files. Responses now carry `Content-Disposition: attachment` with a CR/LF/NUL-stripped percent-encoded filename, plus `X-Content-Type-Options: nosniff`.
- **High: Smoke-test sandbox used `network_mode: "host"`** (`temps-agents`) — the host network mode gave the test sandbox unrestricted host network access. Switched to the same restricted bridge production sandboxes use.
- **High: Default sandbox network could reach internal services** (`temps-agents`) — the sandbox bridge gateway and `host.docker.internal` were reachable from inside the sandbox, allowing AI-generated code to call back to the control plane or other internal hosts. Best-effort iptables egress rules (blocking RFC-1918, 169.254/16, and 127/8) are now installed at sandbox-network creation time; on platforms without iptables (macOS Docker Desktop, rootless Docker) a WARN is logged and sandbox creation continues normally.
- **High: OIDC DNS-rebinding TOCTOU on discovery** (`temps-auth`) — the issuer hostname was validated synchronously before reqwest established a TCP connection, leaving a window where DNS could be rebound to a private IP between validation and connect. An SSRF-safe DNS resolver is now installed on the OIDC reqwest client and re-validates every resolved IP at connect time.
- **High: Agent run IDOR — 11 handlers ignored `project_id`** (`temps-agents`) — handlers used `Path((_project_id, run_id))` (underscore prefix, value unused) and looked up runs by `id` alone, so any user with the relevant role could cancel, retry, or stream any run in any project by supplying their own `project_id` with a guessed `run_id`. A new `ensure_run_in_project` helper is called from every affected handler; cross-project lookups return 404 with no existence disclosure.
- **Medium: BYOK API keys could leak into reqwest error logs** (`temps-ai-gateway`) — `From<reqwest::Error>` serialized the full error including the request URL, which could embed a BYOK key. `From` now calls `.without_url()` before stringifying.
- **Medium: Trivy scanner mounted docker.sock read-write** (`temps-vulnerability-scanner`) — Trivy only needs read access to image layers but had RW socket access, equivalent to host root if the Trivy binary or image were compromised. Remounted read-only; `cap_drop: ALL` added to the Trivy `HostConfig`.
- **Medium: Static-bundle decompression-bomb limit missing** (`temps-deployments`) — analogous to the sandbox fix above; aggregate and per-entry decompressed-size caps now enforced during tar.gz and zip extraction.
- **Medium: Status-monitor `check_path` URL and header injection** (`temps-status-page`) — `check_path` was concatenated to the base URL without sanitization. A new `validate_check_path` rejects paths that do not start with `/`, contain `@` (userinfo injection), contain `://` (scheme injection), or contain CR/LF/NUL/tab (header injection). Validation runs at write time and again at use time so legacy rows cannot bypass it.
- **High: Cross-tenant session-replay event injection** (`temps-analytics-session-replay`) — `add_session_replay_events` accepted a `session_replay_id` from the POST body and appended rrweb events without verifying that the request's Host header resolved to the same project the session was created for. An attacker who could guess or observe another project's session ID could inject DOM-mutation events that would later be replayed in the admin console (stored-XSS primitive). The public ingest path now resolves project_id from Host before any session operation, and the service-layer lookup returns 404 (not 403) on cross-project access to avoid existence disclosure. The admin `add_events` path was patched with the same check via a new `get_project_id_for_session` helper.
- **High: Login endpoint leaked internal errors and enabled user enumeration** (`temps-auth`) — the 401 response set its `detail` field to `e.to_string()` on every `UserAuthError`, exposing raw Postgres error text on `DatabaseError` and producing a distinct message for `UserNotFound` vs `InvalidCredentials`. The handler now returns a constant `"Invalid email or password."` for both auth-failure variants and a constant `"Authentication system error. Please try again later."` (HTTP 500) for any internal error, with the original error chain logged server-side via `tracing::error!`.
- **High: Sentry ingest read raw `X-Forwarded-For` with no proxy-trust check** (`temps-error-tracking`) — public Sentry-compat ingest endpoints (`/{project_id}/store/`, `/{project_id}/envelope/`) accepted any caller's spoofed `X-Forwarded-For`, polluting geolocation and analytics attribution. Replaced with `temps_auth::resolve_client_ip`, which trusts XFF only when the direct TCP peer is loopback.
- **High: Sentry JSON ingest had no body-size cap (slow-POST DoS)** (`temps-error-tracking`) — `ingest_sentry_event` and `ingest_sentry_envelope` ran without a `DefaultBodyLimit` layer, so a sustained multi-GB stream could hold a Tokio worker thread until Axum's internal limit fired. Both routes now sit behind `DefaultBodyLimit::max(2 MiB)`. The envelope decompression-bomb guard (10 MB decompressed) is unchanged.
- **High: Sentry source-map upload had no per-file size limit** (`temps-error-tracking`) — multi-GB source-map uploads via a leaked DSN key could exhaust disk. A 50 MiB cap is now enforced both at the route layer (`DefaultBodyLimit::max(50 MiB)`) and at the per-field check inside the multipart reader, returning HTTP 413 via a new `ErrorTrackingError::PayloadTooLarge` variant.
- **Medium: Swagger UI and OpenAPI JSON were public when the admin gate was in default noop mode** (`temps-cli`) — `/swagger-ui` and `/api-docs/openapi.json` were reachable by any unauthenticated caller, leaking the full API schema as a reconnaissance map. They now require authentication via a `require_auth_for_docs` middleware that reads `temps_auth::AuthContext` from request extensions, regardless of admin-gate configuration.
- **Medium: `/api/auth/email-status` leaked OIDC provider integer IDs to unauthenticated callers** (`temps-auth`) — the response exposed `id: i32` plus the admin-chosen provider `name`, enabling enumeration of internal provider IDs and naming conventions. `OidcProviderSummary` now uses a deterministic `slug` (kebab-case name + 4-byte SHA-256 suffix of `id || name`) instead of the integer ID, and the OIDC login route was changed from `/auth/oidc/login/{provider_id}` to `/auth/oidc/login/{slug}`. The integer ID is no longer exposed to unauthenticated callers.


## [0.1.0-beta.21] - 2026-05-24

### Added
- **OIDC single sign-on (SSO) for the console**: log in to Temps with any standards-compliant OpenID Connect identity provider (Keycloak, Auth0, Okta, Authentik, Zitadel, Microsoft Entra/Azure AD, Google Workspace). New `temps-auth/src/oidc_*` module (errors/handler/service/types) backed by the `openidconnect` 4.x crate handles discovery, PKCE code-flow, nonce/state validation, userinfo, and just-in-time user provisioning. Admin endpoints (`POST/GET/PATCH/DELETE /auth/oidc/providers`, `POST /auth/oidc/providers/{id}/test`) and public flow endpoints (`GET /auth/oidc/providers`, `POST /auth/oidc/{provider}/start`, `GET /auth/oidc/{provider}/callback`) wired through the existing `AuthState`. Two migrations: `m20260522_000001_oidc_sso` creates `oidc_providers` + `oidc_login_states` and adds `users.oidc_provider_id` / `users.oidc_subject` (unique compound index for the federation key); `m20260522_000002_oidc_role_mappings` adds `oidc_role_mappings` so groups/claims from the IdP map to Temps roles automatically. Web: new `/settings/auth` page (`AuthSettingsPage`, `CreateOidcProviderPage`, `OidcProviderForm`, `OidcRoleMappingsCard`, `oidc-provider-constants`) for IdP CRUD + role mapping, "Continue with {provider}" buttons on the login form. Local dev tooling under `tools/keycloak-dev/` (docker-compose + realm export + setup script) for a one-command Keycloak realm against `localhost:9080` — see `tools/keycloak-dev/setup.sh`.
- **Console extension points (`@temps-sdk/console-kit`)**: new local workspace package under `web/packages/console-kit/` exporting `ConsoleExtensionsProvider` + `useConsoleExtensions` so downstream consoles (notably `temps-ee/web`) can inject extra routes into the authenticated shell without forking `App.tsx`. The OSS console mounts `ConsoleExtensionsProvider` and renders any `routes` it provides alongside the built-in routes; rsbuild resolves the package via an explicit `resolve.alias` so it works even when `node_modules/@temps-sdk/console-kit` is stale or missing. Parallel `web/packages/ui/` package set up for shared shadcn/ui primitives (see ADR 0003 — OSS↔EE frontend reuse).
- **Admin gate** (`temps-core::admin_gate` + `temps-cli` plugin): allowlist-based access control for the management surface (`/admin/*`). Configurable via DB-backed `settings` row (UI in `/settings/security`) or `TEMPS_ADMIN_ALLOWED_IPS` / `TEMPS_ADMIN_ALLOWED_HOSTS` / `TEMPS_ADMIN_TRUST_FORWARDED_FOR` env vars (env wins, DB is read-only when env is active). `X-Forwarded-For` is trusted only when the immediate peer is loopback so external clients can't spoof their source IP.

### Changed
- **Workflow trigger save no longer enables triggers the user didn't configure**: `AgentSettingsDialog` initialised the new-issue / regression / manual toggles to `?? true` and emitted every trigger block on save, so opening Edit and clicking Save would silently turn on `error.new_issue`, `error.regression`, and `manual` even when the YAML omitted them. Defaults now flip to `?? false`, and the save payload only includes `error` / `manual` / `schedule` blocks when their corresponding inputs are actually set — round-tripping an Edit → Save with no UI changes now leaves `triggers` byte-identical.
- **`openidconnect` 3.x → 4.0.1** (with `oauth2` 5, `reqwest` 0.12, `rustls` 0.23, `rustls-webpki` 0.103): clears three transitive CVEs in `rustls-webpki` 0.101 (RUSTSEC-2026-0098/0099/0104 — name-constraint bypass + CRL panic) that came in via the OIDC dep chain. API breaking changes in `discover_async` / `request_async` now take a `&reqwest::Client` directly, which lets us own one shared HTTP client with explicit 10s timeout and `Policy::none()` redirects (SSRF mitigation per openidconnect docs).

### Fixed
- **`fix(deps)`: bump `mongodb` 3.6.0 → 3.7.0** to drop transitive `hickory-proto` 0.25 — closes the last residual of the hickory NSEC3 / O(n²) zone-walking CVEs (mongodb 3.6 hard-pinned the vulnerable resolver).

### Security
- **Critical: OIDC account-takeover via unverified email linking** — `resolve_user` linked an incoming IdP identity onto an existing local account by email match alone. An attacker who could sign up at any configured IdP with `victim@example.com` (unverified) could then SSO-login and take over the victim's pre-existing Temps account. The link path AND the JIT-provisioning path now both require `claims.email_verified() == Some(true)`; failures emit `OidcError::EmailNotVerified` with an abuse-log entry.
- **High: SSRF defense for OIDC discovery** — `assert_issuer_host_allowed` pre-resolves the issuer hostname and refuses any IP in RFC 1918, link-local (incl. AWS IMDS at 169.254.169.254), CGNAT (100.64/10), or multicast ranges. Loopback hostnames (`localhost` / `127.0.0.1` / `::1`) are explicitly allowed for local Keycloak / Authentik dev — they can't reach anything the temps process can't already touch.
- **High: Raw IdP `error_description` reflected into browser URL** — the OIDC callback's `?error=` branch was echoing the IdP's free-text error into the redirect's `?reason=` query param, leaking it into the browser address bar, history, and `Referer` headers. Replaced with short opaque codes (`idp_error`, `state_expired`, `idp_unreachable`, etc.) that the login page translates into friendly messages. Raw text is logged server-side only.
- **High: `openidconnect` had three transitive `rustls-webpki` CVEs** — bumped to 4.0.1 (see Changed). `cargo audit` now reports zero hickory + zero rustls-webpki vulnerabilities on `main`.
- **Medium: OIDC token-exchange error chain was lossy** — `RequestTokenError`'s `Display` impl drops the `#[source]` chain, so `e.to_string()` only emitted variant labels ("Server returned error response", "Failed to parse server response"). The token-exchange `.map_err` now pattern-matches variants and surfaces (a) the IdP's parsed OAuth `error: error_description` for `ServerResponse`, (b) the raw body for `Parse` (catches HTML error pages from misconfigured gateways), and (c) the full source chain for `Request` failures via a new `describe_discovery_error` helper.
- **Medium: JWKS stale-cache outage on signing-key rotation** — when an IdP rotated its JWK, every login failed with `IdTokenInvalid` until the 1h discovery-cache TTL expired. `exchange_code` now retries verification once with a forced discovery refresh when the failure text looks like a signing-key problem (matches on `no matching key` / `kid` / `signature` / `jwks` — claim-validation failures don't trigger the retry so config bugs aren't masked).
- **Medium: `enabled` flag was not re-checked on callback** — `start_login` checked `provider.enabled`, the callback path did not. Disabling a provider mid-flight left every in-flight SSO state (up to 10 min) completing successfully. `complete_oidc_login` now re-checks `provider.enabled` after `get_provider` and returns `ProviderDisabled` otherwise.
- **Medium: backslash open redirect in `validate_return_to`** — `/\evil.com` passed the `starts_with('/')` + `!starts_with("//")` checks; Chrome / Edge normalize `\` to `/`, turning it into a scheme-relative URL. Validator now refuses any backslash and any control character (CR / LF / NUL / tab) — same allow-list applied on the frontend in `MfaVerify.tsx`.
- **Medium: `consume_login_state` was non-atomic** — SELECT-then-DELETE allowed two concurrent callback requests with the same state to both pass the SELECT before either ran the DELETE, so the nonce + PKCE verifier could be consumed twice (IdP single-use on the auth code was the outer gate). Replaced with a single `DELETE ... RETURNING *` raw statement so the row is observed by exactly one caller.
- **Medium: admin gate was fail-open on DB load error** — corrupt `settings` row or DB outage silently installed a noop config, exposing the management surface to any IP/host. An attacker with DB write access could disable the gate by corrupting one row. Boot path now fail-CLOSED: load errors propagate as boot failures with an explicit `error!` log telling the operator how to recover (repair the row or set `TEMPS_ADMIN_*` env vars). "No row found" remains the intentional default-noop case.
- **Medium: admin OIDC writes had no audit log** — `create_oidc_provider`, `update_oidc_provider`, `delete_oidc_provider`, `create_oidc_role_mapping`, and `delete_oidc_role_mapping` now emit `OidcProviderCreatedAudit` / `UpdatedAudit` (with `fields_changed` so auditors can tell whether `client_secret` was rotated without comparing values) / `DeletedAudit` and `OidcRoleMappingCreatedAudit` / `DeletedAudit`.
- **Medium: Auth0 + Google integration was broken end-to-end** — three separate bugs all surfaced as cryptic top-line errors: (1) workspace `openidconnect` declared `default-features = false, features = ["reqwest"]` which dropped `rustls-tls`, so the bundled reqwest had no TLS connector and rejected every `https://` URL with `invalid URL, scheme is not http`; (2) `normalize_issuer_url` stripped trailing slashes but OIDC Core §16.13 requires byte-for-byte issuer match (Auth0 publishes its issuer with a trailing slash); (3) Auth0 returns `updated_at` in id_tokens as RFC 3339 when the upstream connection is Google/social, violating OIDC Core §5.1. Fixed all three with explicit feature flags, slash-preserving normalization, and the `accept-rfc3339-timestamps` openidconnect feature.
- **Low: Sea-ORM error redaction** — `OidcError::Database` was returning raw Sea-ORM error text to clients (table names, column names, SQL snippets). Now returns a stable generic message; full error goes to the server log.
- **Low: `idp_group` length + control-char validation** — `oidc_role_mappings.idp_group` is unbounded `text`. Capped at 256 chars and rejects control characters so an admin-only path can't stuff a giant string in there that gets byte-compared against every claim value on every SSO login.
- **Low: live OIDC integration test** — new opt-in `crates/temps-auth/tests/oidc_discovery_live.rs` (`TEMPS_RUN_LIVE_OIDC_TESTS=1`) that hits a real Auth0 tenant to regression-guard the trailing-slash + TLS code paths.


## [0.1.0-beta.19] - 2026-05-20

### Added
- **Manual (non-git) project creation from the CLI**: `bunx @temps-sdk/cli projects create` gains `--manual`, `--source-type` (`manual`, `docker_image`, or `static_files`), `--image`, and `--port` flags so you can create Docker-image and static-files projects without linking a git repository. The git-based flow is unchanged when `--repo` is supplied.

### Fixed
- **AI Gateway returned 401 for valid API keys**: the OpenAI-compatible endpoints (`/ai/v1/chat/completions`, `/ai/v1/models`, `/ai/v1/embeddings`) were registered via `configure_public_routes`, which mounts on the no-auth router — but the handlers use the `RequireAuth` extractor, which reads the `AuthContext` injected by `auth_middleware`. Since that middleware only runs on the authenticated router, every request 401'd with "Authentication Required" *before* the `tk_` API key was ever validated, so no diagnostic was logged. The gateway routes now register via `configure_routes` alongside the admin/usage/pricing routes, so they sit on the authenticated surface: valid API keys authenticate and the `AiGatewayExecute` permission check runs as intended.
- **Static deployments were not served until an unrelated route reload**: `mark_deployment_complete` flipped `current_deployment_id` and fired the route-table `NOTIFY` before writing `static_dir_location`/`image_name`, which `load_routes()` reads to build an environment's backend. For static deployments the `NOTIFY` fired while `static_dir_location` was still NULL, so the proxy built a route with no static directory. A new Phase 0 step persists the routing-relevant deployment fields first, so the route table sees a consistent record the moment the `NOTIFY` fires.
- **Inflated session-engagement and bot traffic in analytics**: auto-fired view events (`page_view`, `page_leave`, `*_viewed`) — which intersection observers trigger for bots too — could mark a session "engaged" on their own. A session now counts as engaged only with ≥10s of measured wall-clock time or a genuine interaction event. Zero-duration session replays (never-finalized single-burst sessions, typically bots) are excluded from replay listings, and user-agent bot detection in the events pipeline is broadened.


## [0.1.0-beta.18] - 2026-05-19

### Added
- **Per-schedule backup scope — pick which databases a schedule backs up, and whether the control plane is included**: backup schedules used to fan out to every external service on the host unconditionally, with an unavoidable control-plane backup attached to every run. Two new boolean fields on `backup_schedules` give operators real control: `target_all_services` (defaults `true`) auto-includes every current and future external DB so the common case stays one-click, and a new `backup_schedule_services` join table (migration `m20260519_000001`) carries the explicit list when an operator opts into "Specific databases". `include_control_plane` (defaults `true`) lets schedules that exist purely to orchestrate external-DB backups drop the control-plane row. Service-layer validators (`BackupService::{create,update}_backup_schedule`) reject states that would have nothing to back up (control plane off + target_all_services off + no attached services); flipping `target_all_services → true` clears the explicit membership ("all means all"). Four new endpoints — `GET/POST /backups/schedules/{id}/services`, `DELETE /backups/schedules/{id}/services/{service_id}`, `GET /backups/external-services/{service_id}/schedules` — with audit logging and OpenAPI registration. UI: reusable `ScheduleServicesSelector` (checkbox list with indeterminate "Select all", hides already-attached); Create and Edit pages get an "All databases (recommended) / Specific databases" radio plus an "Also back up the Temps control plane" Switch; the schedule detail page surfaces both flags in the configuration card and only renders the per-service attach/detach card in 'specific' mode. Migration backfills existing rows to `target_all_services=true` and `include_control_plane=true` so behaviour is identical on upgrade. Covered by 6 unit tests (MockDatabase, Docker-skip) + 3 integration tests against TestDatabase (attach/detach round-trip, flip-to-all clears membership, fan-out skips control plane when flag is off).
- **S3 bucket lifecycle rules enforce backup retention even when temps is offline**: every backup upload now carries `temps-managed=true` and `temps-retention-days=N` object tags (plus `temps-schedule-id` / `temps-backup-id` for traceability), and a new `S3LifecycleService` reconciles per-bucket `BucketLifecycleConfiguration` rules from current `backup_schedules` state. One tag-filtered rule per distinct retention value (`temps-retention-7d`, `temps-retention-30d`, …) so S3 expires expired objects autonomously. Reconcile fires fire-and-forget on schedule create/update/delete (only when `retention_period` or `enabled` changes), plus an hourly drift sweep that re-pushes the desired state — manual edits in the AWS console eventually converge. Tag-based filters were chosen over per-schedule prefixes so existing backup keys are untouched and restore still works; old objects (written before this change) simply lack the tags and are ignored by the rules. App-side `enforce_retention` still runs as the primary cleanup path; providers that reject `PutBucketLifecycleConfiguration` (Cloudflare R2, Backblaze B2, or insufficient IAM permissions) return `ReconcileOutcome::Unsupported` and we silently fall back — backups are never blocked because S3 rejected a lifecycle config. Live testcontainer roundtrip coverage against MinIO and RustFS validates the full `apply_lifecycle` → `get_bucket_lifecycle_configuration` shape; skips gracefully without Docker. Solves the "control plane offline for a week → storage costs balloon" failure mode.
- **Public/admin console listener split**: the control plane can now bind admin/management routes (auth, dashboard, CRUD, settings, SwaggerUI, the SPA) to a separate address from public ingest (analytics events, error tracking, AI gateway, worker node sync, email tracking, Sentry/OTLP). Set `TEMPS_CONSOLE_ADMIN_ADDRESS=127.0.0.1:8081` (or any private interface) to enable; leave it unset for the existing single-listener behavior. Optional defense-in-depth via `TEMPS_ADMIN_ALLOWED_IPS` (comma-separated IPs/CIDRs), `TEMPS_ADMIN_ALLOWED_HOSTS` (comma-separated Host header values), and `TEMPS_ADMIN_TRUST_FORWARDED_FOR` (honor `X-Forwarded-For` only from loopback peers, anti-spoof). Denied requests on the admin gate return `404 Not Found`, not `403 Forbidden`, so probes can't fingerprint the admin surface. Each plugin classifies its own routes via the existing `configure_routes` (admin) / `configure_public_routes` (public) hooks — analytics events, session replay, performance, error tracking (Sentry + sentry-cli), email tracking, AI gateway, and the worker-facing multi-node endpoints have been split accordingly. SwaggerUI and the embedded SPA now mount on the admin listener only. See [docs/howto/admin-listener](docs/howto/admin-listener/page.mdx).
- **Paginated "visitors in segment" page**: clicking any non-page dimension row (e.g. "Chrome" in Browsers, "United States" in Countries, an event name, a referrer, a UTM value) now navigates to `/projects/:slug/analytics/segments/:dimension/:value` — a paginated list of visitors that match the segment in the selected date range, sorted by last action descending (25 per page). Rows link to the existing visitor detail page so you can see the full journey for any visitor. Powered by new optional `filter_*` query params on `GET /analytics/visitors` (`filter_country`, `filter_region`, `filter_city`, `filter_channel`, `filter_referrer`, `filter_event`, `filter_browser`, `filter_os`, `filter_device`, `filter_language`, `filter_utm_source`, `filter_utm_medium`, `filter_utm_campaign`, `filter_utm_term`, `filter_utm_content`); visitor-side filters resolve against `visitor` / `ip_geolocations` while event-side filters use an `EXISTS (SELECT 1 FROM events …)` semi-join scoped by `(project_id, visitor_id, timestamp)` so existing composite indexes (`idx_events_visitor_timestamp`, `idx_visitor_project_last_seen`) carry the query. Date filter (quick or custom) is preserved across overview → dimensions → segment visitors → back.
- **Analytics "view all" dimension pages with date-filter propagation**: every overview chart (events, referrers, browsers, operating systems, devices, locations, channels, languages, UTM source/medium/campaign/term/content) now has a **View all** button in its header that opens a dedicated `/projects/:slug/analytics/dimensions/:dimension` page. The page fetches up to the analytics API cap (100 rows) — far beyond the top 5/10 surfaced on the dashboard — and adds an inline filter input for client-side narrowing. Events list rows on the dimension page link through to the existing event detail view, which now shows a "first → last" timestamp range alongside richer per-visitor columns (visitor UUID + numeric id, device, browser, location, referrer). The active date filter (quick filter or custom range — `filter` / `from` / `to`) is preserved on every hop: overview → dimensions → event detail → back. Previously the event detail tab hardcoded "Last 24 hours" and dropped whatever range the user had selected.
- **CLI device-authorization (browser) login flow**: `bunx @temps-sdk/cli login` opens your browser to a `/cli-login/:userCode` approval page where you sign in with the same credentials, MFA, and SSO flows you use for the web UI — the CLI never prompts for a password. The CLI requests a `device_code` + short `user_code` from the new `POST /auth/cli/device/start` endpoint (best-effort `open` / `xdg-open` / `start`, with a printed URL fallback for headless / SSH / sandbox shells), and polls `POST /auth/cli/device/poll` until you approve the device. The approval page is mounted inside `ProtectedLayout` so unauthenticated users get bounced through the standard `/login` screen — no fork of the auth UI. New backing table `cli_login_sessions` tracks `device_code` / `user_code` / status, mints the API key on approval, and delivers the plaintext to the CLI exactly once before clearing it. The OAuth 2.0 device-flow status codes (`authorization_pending`, `slow_down`, `access_denied`, `expired_token`, `approved`) are honoured; `slow_down` doubles the CLI's polling interval up to a 10s cap. Set `TEMPS_NO_BROWSER=1` to skip the auto-open attempt (the URL is still printed). See [docs/howto/cli-login](docs/howto/cli-login/page.mdx).
- **Edit and confirm-delete actions for agent-sandbox secrets**: the secrets table at `/agent-sandbox/secrets` gains a pencil action that reopens the upsert dialog in edit mode (name locked since it's the backend key, metadata pre-filled, a new value required to rotate since the stored value is encrypted and never returned in plaintext), and a trash action that now routes through an `AlertDialog` naming the secret and warning that anything referencing `${TEMPS_SECRET:NAME}` will fail to resolve. Previously the only actions were create and silent-delete.
- **Postgres WAL health probe + service-detail warning panel**: detects four "silent disk-filler" conditions on managed Postgres services (WAL bloat vs `max_wal_size`, stale replication slots, archive backlog, `archive_mode=on` with empty `archive_command`) and surfaces them on the service detail page with copy-to-clipboard remediation SQL. New `GET /external-services/{id}/wal-health` endpoint; snapshot persisted under the generic new `external_services.health_metadata` JSONB column so future engines can add sibling signals without further migrations.

### Changed
- **`EditBackupSchedule` page uses the generated OpenAPI SDK instead of a hand-rolled fetch shim**: `web/src/lib/backup-schedules.ts` (a hand-rolled `PATCH /api/backups/schedules/{id}` helper that predated the endpoint being in the OpenAPI surface) is deleted; the Edit page now uses `updateBackupScheduleMutation` and `UpdateBackupScheduleRequest` from the generated client. Removes a maintenance hazard where new fields on the request body had to be added in two places. Convention reinforced in `AGENTS.md`: hand-rolled `fetch` helpers under `web/src/lib/` are not allowed; if a binding is missing the fix is to expose the endpoint via `utoipa::path` and regenerate, not to write a shim.
- **`temps login` is now browser-only for interactive use; `--api-key` is the headless path.** All credential entry happens in the web UI — there is no terminal password prompt anymore. Headless / CI authentication uses a pre-minted API key from the dashboard's **Settings → API Keys** page, passed via `--api-key`.
- **Default agent turn caps raised**: committed agents now default to `max_turns: 30` (was 10), and the ephemeral dry-run cap rises to 50 (was 20). The Claude CLI invocation in `temps-agents` now treats `max_turns <= 0` as "omit the `--max-turns` flag entirely", letting a reviewed YAML opt into unlimited turns while `timeout_seconds` + `daily_budget_cents` still bound the run.

### Removed
- **`POST /auth/cli/login` (email + password endpoint)**, along with its `CliLoginPasswordRequest` / `CliLoginResponse` schemas and the two-stage MFA `mfa_session_token` handshake. The endpoint is no longer registered, no longer in the OpenAPI document, and no longer routed. Existing API keys (including those minted by the old endpoint) continue to work — only the password-grant endpoint is gone.
- **CLI flags `--email` / `--password` / `--magic` / `--mfa` / `--device`** on `temps login`. The interactive flow is the browser device flow unconditionally; `--api-key` is preserved for headless / CI. Magic-link login through the CLI is no longer supported (magic links still work for browser logins from the web `/login` page).

### Fixed
- **Backup uploads to Cloudflare R2 no longer fail with `service error`**: every backup against an R2 bucket failed with `create_multipart_upload failed: service error` (5+ minute wall-clock, no diagnostic detail). Two root causes: (1) every S3 SDK call site rendered errors via `format!("...: {}", e)`, which for any 4xx/5xx collapses to the string "service error" — the HTTP status, service code, request id, and XML body were all thrown away; (2) the AWS SDK sends `x-amz-tagging` as a request header on `PutObject` and `CreateMultipartUpload`, and R2 returns `501 NotImplemented` on that header. Moving tagging to a follow-up `PutObjectTagging` call still failed — R2 also returns `501 NotImplemented` on that endpoint. Object tagging is simply not implemented on R2. Fix: added `describe_sdk_error` in `engines::v2_common` that pattern-matches every `SdkError` variant and surfaces HTTP status / service code / request_id / x-amz-id-2 / a truncated response body; all upload sites (single-part, create/upload/complete multipart, metadata companion, `head_bucket`) and the three `From<SdkError> for BackupError` impls now use it, so future S3 failures will say *what* actually went wrong. Tags are still applied via `PutObjectTagging` after every successful upload, but `apply_object_tags` now treats failures matching `is_unsupported_error` (NotImplemented, MethodNotAllowed, MalformedXML, AccessDenied, lifecycle-specific InvalidArgument) as best-effort — it logs a warn under target `temps_backup::tagging` and returns Ok so the backup itself succeeds. AWS S3 / MinIO / compliant stores still tag normally; tag-driven bucket lifecycle is unavailable on R2 (already best-effort in the reconciler) so app-side `BackupService::enforce_retention` is the retention source of truth there. Two regression tests pin the exact R2 error shapes for both the `x-amz-tagging` upload-header path and the `PutObjectTagging` path so a future SDK upgrade can't silently regress the matcher.
- **GitHub App scoped token mint failures are now logged with context**: each fallible step of the GitHub App installation token flow (private key parse, JWT creation, octocrab client build, installation fetch, `access_tokens_url` parse, GitHub `access_tokens` POST) now emits an `error!` line with `installation_id` and `app_id` so a "GitHub rejected access_tokens" failure can be traced back to the specific installation. The new logs call out the two common causes — requested repo not selected on the installation, or the App lacks the requested permission — so operators stop having to re-derive context from the call site. Pure observability change; no behavior change to the token mint itself.
- **Sandbox bring-up now runs a dedicated `normalize_ownership` step on both create and recover.** The container post-start chown is factored into a separate method that does `chown -R temps:temps` on both the home volume (best-effort: warns on non-zero exit, continues) and the bind-mounted `/home/temps/workspace` (fatal with `stat`-based verification so dev-machine bind-mount backends that return EPERM for logical no-ops don't abort, but real prod permission failures do). This is the in-container defense-in-depth that complements the host-side `chown_workdir_to_sandbox_user` from beta.9 — fixes the residual "Permission denied" failures on `mkdir reports/`, `git commit`, and lockfile creation under workspace.
- **Postgres `archive_mode=on` with empty `archive_command` no longer causes runaway `pg_wal` growth.** Earlier versions baked `archive_mode=on` into the container CMD unconditionally, so any Postgres service whose `archive_command` was never set (no S3 source linked, or `enable_wal_archiving` never reached) accumulated WAL forever — we observed 191 GB `pg_wal` in production. New services now start with `archive_mode=off`; `enable_wal_archiving` recreates the container with `archive_mode=on` baked into CMD when WAL-G is configured. `PostgresService::start` reconciles by probing the volume for `walg.env` and comparing against the running container's CMD, recreating if they disagree — operator-initiated Stop/Start auto-repairs existing services with the bad combo. The bad combo is now unrepresentable for any service that's been restarted at least once. `start_service` also refreshes the WAL health snapshot inline after a recreate so the UI reflects the new state within ~1s instead of waiting for the next 30s probe cycle.


## [0.1.0-beta.9] - 2026-05-11

### Fixed
- **Bind-mounted sandbox `work_dir` was owned by root on the host, breaking host-side tooling**: `WorkspaceState::ensure_work_dir` created `<data>/workspaces/<id>/work` as whichever uid the `temps` server runs as (root in default Docker installs), then bind-mounted it into the sandbox where `SANDBOX_CHOWN` (`temps:temps`) reassigned ownership *inside* the namespace only. Anything running on the host that touched the directory (backups, `du -sh`, log shippers, the autopilot's `git status` polling) hit `Permission denied`. The workspace creation path now resolves the sandbox uid:gid via the sandbox-user constants and `chown`s the host directory at mount-prep time, before the bind-mount is established. Userns-remap installs still get the correct mapped owner because `SANDBOX_CHOWN` runs after. (#84)
- **Sandbox `git pull` failed with `fatal: detected dubious ownership`**: `/home/temps/workspace` came up owned by uid `0:0` inside sandbox containers when the host-side `temps` server ran as root or the Docker daemon used userns-remap, so Git refused to operate on the bind-mount. The Dockerfile generator now bakes `git config --system --add safe.directory /home/temps/workspace` into the sandbox image while still root, so Git trusts the workspace regardless of stat owner — belt-and-suspenders against userns-remap and against post-start chown failures. Takes effect after sandbox images are rebuilt and pushed (`SANDBOX_CHANNEL=beta ./scripts/build-sandbox-images.sh`, then promote to `stable`). (#83)
- **Silent sandbox-creation failures from swallowed exec errors**: `DockerSandboxProvider::create_sandbox` had three `let _ = start_exec(...)` post-start sites (home `chown`, work-dir `chown`, AI-CLI restore) that discarded both the exit code and stderr — so a chown that failed against a userns-remapped container left users with a "successfully created" sandbox that then exploded the first time Git or any uid-sensitive tool ran. Replaced with a new `run_root_exec` helper that drains the output stream, inspects the exit code, and logs stderr on non-zero. The two chown steps now fail sandbox creation visibly instead of leaving a silently broken workspace; the AI-CLI restore stays best-effort but its exit code is logged. (#83)

## [0.1.0-beta.8] - 2026-05-10

### Fixed
- **E2E deployment tests**: the polling loop in `.github/workflows/e2e-tests.yml` treated `state == "running"` as terminal success, racing the build and curling deployed apps before their containers existed (HTTP 000). It now waits for `state == "completed"` — the only terminal-success variant of `temps-entities::PipelineStatus` — and retries the HTTPS verify for up to 60s after completion to absorb route-table propagation. Surfaced after #80/#81 unblocked initial deployment creation. (#82)
- **Initial deployment for new environments**: project creation now reliably triggers the first deployment when `automatic_deploy=true`, instead of leaving the environment without a current deployment. (#81)
- **`automatic_deploy` flag honoured in CI E2E**: the test harness now sets `automatic_deploy=true` when creating projects so the auto-trigger path is exercised end-to-end. (#80)

### Tests
- **`merge_integration` observability tests**: `EventFilters` constructors now include `hide_bots: None` so the test crates compile against the updated struct. (#79)

## [0.1.0-beta.7] - 2026-05-09

### Added
- **Custom date range in Log History**: the time-range select gains a `Custom range…` option that opens the shared `DateRangePicker` with HH:MM precision. Picking it seeds the calendar from the previously-active preset for a smooth handoff (no empty starting state). Custom ranges feed `start_time` / `end_time` directly to `/api/logs/search`; the `filterKey` includes the picked window so the auto-loading rope resets when the range changes. The full-text search box auto-disables (with a tooltip) when the active range exceeds the server's `MAX_FULLTEXT_HOURS = 24` cap — covers both the new 7d/30d presets and any custom range over a day, and prevents silent 400s
- **`Last 7 days` and `Last 30 days` time-range presets in Log History**: complement the existing 15m/1h/6h/24h presets for forensic search. Server-side full-text search still capped at 24h via `MAX_FULLTEXT_HOURS`; the UI surfaces this as a disabled search box with tooltip rather than letting the request 400
- **Live-rate counter and "sampled" chip in the Live log viewer**: status row now reads `Live · X lines · ~Y lps`, computed from a 5-second sliding window of WebSocket frame arrivals sampled on the existing 1Hz tick. When `lps >= 60` an amber `sampled` chip appears, signaling that the visible buffer is rolling faster than a focused user can read — at that rate even the most attentive reader sees less than 10% of arrivals before they roll off the 5,000-line cap. Tuning lives in `LPS_WINDOW_MS` and `LPS_SAMPLED_THRESHOLD`
- **Per-line fade-in animation on freshly arrived log batches**: rows in the most recently flushed batch get a 120ms `opacity 0 → 1` keyframe (`log-fade-in` in `globals.css`) so the eye perceives the new batch arriving as motion rather than a wall expanding silently. Applied via index comparison against `lastBatchStart` — rows already in the DOM render with no animation cost. Respects `prefers-reduced-motion`
- **Auto-loading log history**: the History viewer's chevron pager (which never worked anyway thanks to the cursor bug below) is replaced with an `IntersectionObserver`-driven rope. A sentinel at the top of the scroll container fires `handleLoadOlder()` when the user scrolls within 200px of the top; older pages prepend silently and scroll position is preserved across the prepend. The "Load older" button stays mounted as a fallback for error retry and accessibility but is rarely needed in practice. Pages chain — small first pages auto-fill the viewport
- **Resource limits + runtime/stats panel for external services**: managed databases (postgres, postgres_cluster, redis, mongodb, rustfs, s3) can now be capped per-container with optional memory, swap, and CPU limits. Three new endpoints: `GET /external-services/{id}/runtime` (status, restart count, OOM-killed flag, applied caps), `GET .../stats` (one-shot CPU/memory snapshot), and `PATCH .../resources` (persist + live-apply via `docker.update_container`). The frontend gets a Resources card on the service detail page that polls runtime every 30s and stats every 5s, plus an Edit-limits dialog with independent Memory + CPU toggles, an explicit OOM warning when memory caps are enabled, and a per-member apply summary toast (`applied`/`stopped`/`missing`/`failed`/`requires_recreate`). The CPU meter rebases its percent against the cap when one is set ("0.2% of 4 cores capped (host 16)") so the bar is meaningful instead of pinned at host_cores/cap_cores. All `None` fields = unlimited; legacy services with no `resources` block continue to run unconstrained
- **Release channels for `temps upgrade`**: new `--channel {stable,beta}` flag picks which release stream the upgrader subscribes to. `stable` (default) only considers non-prerelease tags; `beta` includes both stable and beta releases so a beta host always receives the freshest available version. **CLI-only by design** — there is no env-var fallback, so a user must pass `--channel beta` explicitly to opt into prereleases. `temps upgrade` (no flags) always lands on stable. The `install.sh` curl-pipe installer accepts the same `--channel` flag with identical semantics. Pinning a specific `--version` ignores the channel. The old boolean `--stable` flag is kept as a hidden alias for backward compat with existing scripts
- **Runtime-toggleable ClickHouse analytics backend**: operators can now point Temps at a ClickHouse cluster for analytics reads instead of TimescaleDB by setting `TEMPS_CLICKHOUSE_URL` / `TEMPS_CLICKHOUSE_DATABASE` / `TEMPS_CLICKHOUSE_USER` / `TEMPS_CLICKHOUSE_PASSWORD` on `temps serve` — same binary, no rebuild, no cargo feature flag. When all four are set the analytics events plugin (1) applies the embedded CH schema migrations (events / events_5m_mv / sessions, tracked in `_temps_ch_migrations`), (2) spawns the `ChFanoutWorker` on the tokio runtime, and (3) swaps the read-side `Arc<dyn AnalyticsEvents>` to `ClickHouseEventsBackend`. Postgres remains the system of record; `record_event` writes to PG synchronously and enqueues into a new `events_ch_outbox` table that the worker drains asynchronously via `FOR UPDATE SKIP LOCKED` + `clickhouse::Client::insert()`. CH outage never blocks ingestion; retries are safe via `ReplacingMergeTree(_version)` dedupe keyed on `event_id`. Default behavior unchanged when env vars are unset (single-binary PG-only). All 12 `query_*` methods translate the `*Spec` value-types into CH dialect (count() / uniq() per aggregation level, toStartOfInterval + WITH FILL for gap-fill, FINAL on every read for ReplacingMergeTree correctness); property breakdowns and timelines group on plain CH columns (geo fields denormalized at fan-out time so country/region/city work without a cross-database join); the multi-project dashboard runs three CH queries with in-memory sparkline densification mirroring the Postgres impl. Worker also runs an hourly retention sweep (delivered outbox rows older than 7 days are deleted) and a 5-minute dead-letter scan (warns when rows exceed `max_attempts` so operators can investigate). Live testcontainer integration test against `clickhouse/clickhouse-server:24.8` validates migrations, row mapping, and every implemented query against a real CH server; skips gracefully without Docker (per CLAUDE.md). See [ADR-012](docs/adr/012-clickhouse-analytics-backend.md) for the bring-your-own design rationale and the operator runbook in [docs/howto/enable-clickhouse-analytics](docs/howto/enable-clickhouse-analytics/page.mdx) for the full enable/verify/rollback flow including documented divergences (HLL approximation, self-referral filter on referrer_hostname is PG-only, the `Analytics` trait stays on PG since visitor/session queries are mutable and not events-shaped)
- **`AnalyticsEvents` query value-types**: refactored the analytics read trait so each method takes a single `*Spec` struct (`EventsCountSpec`, `EventsTimelineSpec`, `PropertyBreakdownSpec`, …) instead of a long parameter list. Validation lives in the constructor (`limit` clamping at 100, builder-style scope narrowing). Backends consume the spec and render however they want — Timescale builds parameterized SQL with `$1, $2`; CH uses `clickhouse::Client::query()` with typed `.bind()`. Adding a third backend means implementing the trait, nothing else
- **In-sandbox CLI auto-auth**: `MessageExecutor` now materializes the three CLI auth files on workspace session init and on every refresh, so `bunx @temps-sdk/cli` works inside sandboxes without sourcing `~/.env`. Files written: `~/.temps/.contexts.json` (multi-instance store with a single active `workspace` context), `~/.temps/.secrets` (legacy `temps_api_key` / `temps_user_id` / `temps_email`), and `~/.config/temps-cli-nodejs/config.json` (`apiUrl` / `outputFormat` / `colorEnabled`). `apiUrl` resolves from platform `external_url` with a `TEMPS_INTERNAL_API_URL` env override and a `host.docker.internal:3000` fallback for local dev
- **Centralized sandbox identifiers**: new `crates/temps-agents/src/sandbox/user.rs` module defines `SANDBOX_USER`, `SANDBOX_GROUP`, `SANDBOX_CHOWN`, `SANDBOX_HOME`, and `SANDBOX_WORK_DIR` constants. Every load-bearing `/home/temps`, `temps:temps`, and `/workspace` literal in the Dockerfile generator, mount logic, chown calls, and Claude projects-dir derivation now flows through these constants — single source of truth for the sandbox identity model

### Changed
- **Log History flipped to chronological (ASC) ordering**: results now arrive oldest-first / newest-last in both the API response and the UI render. The first page anchors to the bottom on initial load (most recent visible by default) and "load older" prepends at the top, matching `journalctl` / `tail -f` orientation rather than the previous "newest at top" snapshot view. This is a deliberate symmetry pass — both Live and History tabs now read downward as time progresses, eliminating the cross-tab cognitive flip
- **Live viewer "Every Ns" mode is now an HTTP poll, not a throttled WebSocket**: `Pause | Live | Every 5s/30s/60s` had three increasingly buggy interactions with the streaming WS (blank pane on entry, duplicate bursts on toggle, stale `Connecting…` banner). Interval mode now skips the WebSocket entirely and polls `/api/logs/search` with `page_size=500`, replacing the visible buffer with a fresh snapshot. Fires immediately on entry (no more 5s of empty screen) and reuses the existing "Refresh now" button to trigger an off-cycle poll. Live and Pause modes unchanged — Live still uses the WebSocket with its rAF flush pump, Pause still freezes the buffer with the socket closed
- **Live viewer flush rate throttled to ~30Hz**: `LIVE_FLUSH_MIN_GAP_MS = 33` gates how often a buffered batch is committed to React state. Previously every WebSocket frame scheduled a `requestAnimationFrame` flush; on a 1500 lps firehose the eye perceived a blurred wall of replacing text. The throttle keeps the cadence eye-paceable on any incoming rate while remaining imperceptibly delayed on slow streams. Cleanup paths cover the new deferred-flush timeout in Pause-transition / source-change / unmount
- **Log viewer mode toggles preserve the visible buffer**: switching Live ↔ Pause ↔ Interval no longer wipes the on-screen logs. Source-only signature (env, container, dates, tail, timestamps — everything except mode) is captured in a ref and compared on every WS effect run; only genuine source changes wipe. Eliminates the "blank pane for 5s after picking Every 5s" UX regression that the previous "wipe on any non-resume-from-Pause run" gate produced
- **Log viewer auto-follow tolerance bumped from `< 1px` to `< 8px`**, plus user-intent gating: scrolling away from the bottom no longer disengages follow on virtualizer-reflow-induced scroll events (multi-line JSON logs grow ~40px after the virtualizer measures them, which used to flip `autoScroll` off uninvited). Wheel / touch / keyboard events mark intent; reflow events don't. Auto-scroll now runs in `requestAnimationFrame` so the snap catches up to the post-measurement scrollHeight in the same paint cycle
- **Log viewer density tightened end-to-end**: log row line-height `leading-relaxed → leading-snug` with `py-0` (was `py-0.5`); toolbar wrapper `p-4 space-y-4 → px-3 py-2 space-y-2`; log pane padding `p-4 → px-3 py-2`; pane height switched from a fixed `h-[600px]` to `h-[calc(100vh-280px)] min-h-[300px]` so the rope fills available viewport height. `estimateLineHeight` updated to match the new measurements. On a 1080px viewport the visible log count roughly doubles
- **"Storage" surface renamed to "Databases"** everywhere user-facing: page title, breadcrumb, navigation label. URL stays `/storage` for back-compat with bookmarks and deep-links from docs. The page now also shows a compact CPU/memory readout per running service in the list, polled every 10s, so operators can spot a misbehaving database from the index page without drilling in. Per-member layout in `ServiceResourcesPanel` collapsed to a single horizontal row with inline meters and a live-indicator dot when stats are fresh
- **`EditResourceLimitsDialog` swap input semantics fixed**: the field labeled "Swap" now means "extra swap above memory" (in MiB), not Docker's raw `memory_swap` total. The UI translates to `memory + swap` at submit time, so the API contract is unchanged but the input matches its label. Existing services with `swap == memory` (the previous "swap disabled" sentinel) round-trip cleanly to "extra swap = 0" in the form
- **Command palette icons disambiguated for log entries**: `Logs` (live runtime), `Log History` (search), and `Request Logs` (HTTP traffic) all rendered with the same `ScrollText` glyph, which made search-by-icon useless. Now: `ScrollText` for live runtime, `History` for the History deep-link, `Network` for HTTP request logs (different feature entirely)
- **Integration-tests CI timeout raised from 45m → 90m**: gives the postgres-upgrades matrix group headroom on cold runners where pg17/pg18 image pulls land alongside several serial orchestrator tests. Common-path runs are unaffected; the timeout only kicks in for true outliers
- **Workspace mount path moves from `/workspace` to `/home/temps/workspace`**: keeps the working directory under the sandbox user's home, eliminating the cross-tree `chown` and aligning with how AI CLIs (Claude, Codex) resolve project roots. Claude's `claude_projects_dir` now derives from `SANDBOX_WORK_DIR.replace('/', '-')` so resume keys stay consistent with the new path
- **`AuthFlavor::seed_path` is now `seed_path_rel` + `seed_path()` helper**: paths are stored relative to `SANDBOX_HOME` and joined at use-time, so renaming the home directory is a one-constant change instead of a grep-and-edit across every flavor
- **`get_temps_api_url` is async**: now reads platform settings to surface the configured `external_url`. `WorkspacePlugin` plumbs `Arc<ConfigService>` into `MessageExecutor` so the resolver is reachable from the session-write path

### Fixed
- **Git push events deployed projects with auto-deploy disabled**: the `automatic_deploy` flag was stored on `DeploymentConfig`, exposed via API, and rendered in the project UI, but `process_git_push_event` (in `temps-deployments/src/services/job_processor.rs`) never read it before queuing a deployment. Toggling "deploy on push" off had no effect — every webhook still produced a build, container, and rollout. The push pipeline now resolves the effective flag via a new `is_automatic_deploy_enabled(project_cfg, env_cfg)` helper that mirrors `DeploymentConfig::merge` semantics (env overrides project; OR-on-booleans; both-`None` → false, since auto-deploy is opt-in) and returns early with an `info!` log when disabled, before the duplicate-deployment scan, in-flight cancellation, or job-queue work runs. Both GitHub and GitLab webhooks funnel through `git_provider_manager.handle_push_event` → `GitPushEventJob`, so the single gate covers both providers. Six unit tests cover each project/env combination
- **Log History pagination was emitted but never honored**: `archive_search` returned a `next_cursor` in the response but the search service ignored `filter.cursor` on the way back in, so clicking the next-page chevron returned the same first page. Cursor is now parsed (`<ts_millis>:<chunk_uuid>`), narrows the effective `end_time`, and a strict `< cursor.ts` check inside the chunk scan prevents the boundary line from being returned twice. Pagination tests previously asserted only `lines.length <= page_size`, which couldn't have caught this — replaced with a real test that fetches page 1, sends its cursor as page 2's request, and verifies both no-overlap and strictly-older timestamps
- **Log History silently truncated wide time windows to the OLDEST 200 lines**: `archive_search` ran chunks in `find_chunks` order (oldest-first by insertion), accumulated matches into an unbounded `Vec`, then `break`'d the chunk loop the moment `all_matches.len() > page_size`. Result: a "Last 6 hours" query returned roughly the same lines as "Last 1 hour" because the scanner bailed before reading any chunk past the start of the window — and the lines it did return were the oldest in the window, not the newest. Replaced with a bounded `BinaryHeap<Reverse<HeapEntry>>` of size `page_size + 1`. Memory stays at O(K) regardless of total matches; time is O(N log K). All chunks are now scanned. Regression test seeds two batches at -3h30m and -30m, asserts that "Last 1h" returns 5 lines and "Last 4h" returns all 10 — would have failed under the old early-exit
- **`total_scanned` was a misleading raw byte count, not a match count**: the field was incremented before the time/level/service/text filter ran, so a query showing "200 logs (344 scanned)" implied 144 lines were filtered out by the user's level/text predicates. In reality most of those 144 were just chunk bleed-through outside the requested time window — chunk files store many minutes of logs and the scanner reads the whole file then filters. Counter now increments only after every filter passes, so the displayed number actually reflects "matches in window" the way users expect
- **Log search `page_size` cap raised from 500 → 2000**: the previous undocumented hardcoded `min(page_size, 500)` silently clamped any frontend request above 500 lines, which the History viewer hit on its very first page once the default frontend page size became 500. Cap moved to 2000 with a comment explaining the bound (memory-safe given the `MAX_FULLTEXT_HOURS` ceiling). Frontend default is 500; "Load older" appends 500 more
- **Log viewer follow mode silently disengaged on every multi-line JSON log**: `handleScroll` checked `scrollHeight - scrollTop - clientHeight < 1`, but tanstack-virtual measures rows lazily as they scroll into view; a multi-line JSON log can grow by 60+ pixels post-measurement. Each measurement-induced reflow dispatched a scroll event with `isAtBottom === false`, which flipped `autoScroll` off uninvited. Tolerance bumped to 8px and a `userScrolledRef` gate distinguishes user-initiated scrolls (wheel, touch, keyboard) from reflow events — only the former can disengage follow. The auto-scroll-to-bottom effect now runs in `requestAnimationFrame` so it catches up to the virtualizer's post-measurement `scrollHeight` in the same paint cycle
- **Live mode showed "Connecting to log stream…" banner in non-Live modes**: `connectionStatus` lingered at `'connecting'` after the WS effect early-returned in Pause / Interval mode. Banner gating updated to `mode.kind === 'live' && connectionStatus === 'connecting'` (and same for the error / permanent-error variants). Same fix applied to the `opacity-50` class on the log pane
- **Stale skill content tests**: updated to match current `temps-cli/SKILL.md` headings so the suite stops failing on docs drift
- **MFA verification left users stranded on the login page**: `verify_mfa_challenge` built a `HeaderMap` containing the new `session=…` cookie via `create_session_cookie`, then called `response_headers.insert(SET_COOKIE, mfa_clear_cookie)` to expire the temporary `mfa_session` cookie. `HeaderMap::insert` replaces all values for the key, so the response went out with only `mfa_session=; Max-Age=0` and no real session — the immediate `/user/me` refetch returned 401, and `ProtectedLayout` bounced the user back to `<Login />` even though the toast had already announced "MFA verified successfully". Fixed by switching to `append`, plus a regression test that mirrors the handler's exact header merge and asserts both `Set-Cookie` entries survive

### Security
- **Shell-escape user-controllable `--model` flag**: AI CLI invocations now escape the model name before passing it to the shell, preventing argument injection via crafted model strings
- **Reject unknown `ai_provider` values with logged fallback**: invalid provider names no longer silently coerce to a default; they're rejected and the rejection is audited
- **CLI auth handler hardening**: tightened input validation and error surfacing on the `cli_login` / `cli_logout` paths
- **Route sync hardening**: tightened validation on the route sync pipeline to prevent malformed inputs from reaching the proxy config
- **Sandbox filesystem hardening**: extra guards on sandbox FS handlers to reject path-escape attempts at the handler boundary (defense in depth on top of the existing `FilesystemStorage::resolve_path` check)
- **Email tracking hardening**: tightened validation on tracking event ingestion to reject malformed payloads
- **Deployment workflow planner hardening**: tightened input validation on the workflow planner to reject malformed deployment requests at the planning stage

## [0.1.0-beta.6] - 2026-05-03

### Added
- **Unified Observe page (`/projects/:slug/observe`)**: new project-level page that merges Requests, Traces, Errors, and Revenue into a single time-ordered timeline. Cockpit header shows one sparkline per kind over the selected time range — clickable to toggle that kind on/off. Below it, a console-style monospace stream with color-coded gutters per kind (sky=requests, violet=traces, rose=errors, emerald=revenue), `HH:mm:ss` timestamps, and rich one-line summaries. Side panel renders entirely from the row payload — no follow-up fetch in the common case. Filter state lives in URL search params so the page is shareable. Traces default OFF (high-volume hypertable, opt-in via cockpit card). Runtime logs are intentionally NOT included — they live on the dedicated Logs page because their volume would dominate the merged business-signal timeline
- **`temps-observability` crate**: new merge service backing the Observe page. Per-kind parallel queries against `proxy_logs`, `error_events`, `revenue_events`, and the `otel_spans` TimescaleDB hypertable (raw SQL via `FromQueryResult`); k-way merged by `ts DESC` and trimmed to a single page. Heavy fields are truncated server-side (stack frames → first 5, span attributes → first 20 keys alphabetized, headers → whitelist of ~10 keys) with `*_truncated` flags so the panel can offer a "Show full" un-truncate fetch when needed. Wire shape is a discriminated `ObservabilityEvent` union — every row carries its own `type` discriminator and all data the panel needs
- **Cross-source correlation columns**: new migration `m20260502_000001_add_observe_correlation` adds `proxy_logs.trace_id` + `proxy_logs.error_group_id` + indexes, `revenue_events.deployment_id` + `revenue_events.environment_id` + `revenue_events.trace_id`, and `error_events.trace_id_indexed` (denormalized from `data.trace.trace_id` JSONB for index speed). All nullable — old rows just render without correlation links. Lets the Observe view jump from any event to its peers in the same trace
- **W3C `traceparent` extraction in proxy**: `temps-proxy` now parses inbound `traceparent` headers and stamps the 32-hex trace_id onto `proxy_logs.trace_id` at write time. Validates length, hex chars, and rejects the all-zero invalid trace_id reserved by the W3C spec. Six unit tests cover the happy path, missing/malformed/invalid inputs, and case-folding
- **OTel trace_id promotion in error tracking**: `temps-error-tracking` ingestion now probes `data.sentry.contexts.trace.trace_id`, top-level `data.contexts.trace.trace_id`, and `data.trace.trace_id` and promotes the first valid 32-hex value to the indexed `error_events.trace_id_indexed` column. Seven unit tests cover all three layouts plus invalid lengths/chars and the all-zero case

### Fixed
- **Workspace preview routing returned 404**: workspace session URLs of shape `ws-<16hex>-<port>.<domain>` were misrouted to the sandbox lookup path because the synchronous host parser couldn't disambiguate the 16-hex label between `wss_` (workspace) and sandbox public IDs. Added `resolve_preview_target` that runs after parsing — when the parser produces `Sandbox(<hex>)`, it queries `workspace_sessions` for `public_id = wss_<hex>` and swaps to `WorkspaceSession(row.id)` if found. One extra unique-index hit per request. Also genericized the 404 body from "Workspace preview not found" to "Preview not found" since both targets land in the same branch
- **Sandboxes failed to start after image updates**: `ensure_image_for_runtime` short-circuited on `:latest` because `inspect_image` returned Ok for any cached tag, so hosts never re-pulled stale images. Replaced the hard-coded `:latest` with a `SANDBOX_IMAGE_VERSION` constant (currently `0.1.0`); bumping the constant now forces every host to pull the new tag. The release pipeline pushes versioned + `latest` tags to GHCR (`ghcr.io/gotempsh/`) instead of Docker Hub

### Changed
- **Beta vs stable release channels for GHCR images**: the release workflow now publishes server and sandbox images on separate channels so a beta release can never overwrite a stable image at the same version. Stable tags (`v1.2.3`) push the canonical `:<ver>`, `:<ver>-stable`, `:latest`, `:stable`, plus `:<sha>`. Prerelease tags (`v1.2.3-beta.4`) push only `:<ver>-beta`, `:beta`, and `:<sha>` — never the unsuffixed canonical ref. Operators running a beta build set `TEMPS_SANDBOX_CHANNEL=beta` so the host pulls the matching beta sandbox image; default behavior is unchanged (stable channel). The manual `scripts/build-sandbox-images.sh` defaults to `SANDBOX_CHANNEL=beta` to prevent an accidental local push from poisoning stable
- **Workspace bind-mount permissions**: temps server runs as root in prod, so the host work_dir was root-owned and `USER temps` inside the container couldn't write anywhere in `/workspace` — TUIs failed, dev servers couldn't open lockfiles, and seed scripts run via `docker exec` inherited the host inode UID. Added a post-start `chown -R temps:temps /workspace` exec as root, mirroring the existing `/home/temps` pattern (CHOWN+FOWNER caps already present)
- **Workspace previews without a password show 409 instead of 404**: `lookup_preview_session` now distinguishes "session exists but no `preview_password_hash`" from "no row at all". The first case maps to a new `PreviewAuthOutcome::NotConfigured` and renders a friendly HTML page (409 Conflict) explaining how to set a preview password from the workspace settings — instead of the cryptic 404 that previously suggested the workspace itself was missing
- **Error group titles "Error: Unknown error"**: error groups created from Sentry SDK `captureMessage()` calls (which omit `exception.values[].value`) were stored with the literal title `"Error: Unknown error"`, masking the real message. The ingestion pipeline now probes the raw Sentry payload at `logentry.formatted`, `logentry.message`, top-level `message`, `exception.values[0].value`, the first breadcrumb message, and `extra.message` for a usable string. Same fallback runs at read time in the Observe row mapper, so existing groups also render their real text — group titles in the database remain as-is, but the Observe row and the error detail page now show the real message regardless. Future events will get correctly-titled groups at create time

 `bunx @temps-sdk/cli login <url>` now authenticates with email + password (and TOTP MFA when enabled) instead of requiring users to mint and paste an API key. Internally hits a new `POST /auth/cli/login` endpoint that mints a 90-day API key scoped to the user's role. The previous fragile `/auth/login` + cookie-scrape + `/api/tokens` flow is replaced. `--api-key` and `--magic` login modes still work.
- **CLI multi-context support**: `bunx @temps-sdk/cli context list | use | remove | current` lets one workstation stay logged into multiple self-hosted Temps servers simultaneously. Each context stores `{name, url, apiKey, email, keyPrefix, expiresAt}` in `~/.temps/.contexts.json` (mode 0600). The active context drives `apiUrl` and `apiKey` resolution for every other CLI command, with environment variables (`TEMPS_API_URL`, `TEMPS_API_TOKEN`, `TEMPS_API_KEY`) still taking precedence for CI / one-off overrides. Distinct from `temps instances`, which manages Temps Cloud VPS instances.
- **CLI `temps logout` revokes server-side**: now calls `POST /auth/cli/logout` (best effort) to revoke the API key on the server before clearing local credentials. Pass `--local-only` to skip the network call when the server is unreachable, or `--context <name>` to log out of a specific context.
- **CLI `temps whoami` surfaces context**: shows the active context name, key prefix, and expiry alongside the user info; `--json` includes the full context object.
- **Server `/auth/cli/login` and `/auth/cli/logout` endpoints**: new rate-limited endpoints in `temps-auth` with two-stage MFA (returns `mfa_required: true` + `mfa_session_token`, then accepts code on retry). Mints scoped api_keys named `cli:<device>`, role inherited from the user, 90-day TTL. Logout revokes the presented bearer key. Successful and failed logins are audited with `login_method = "cli_password"`. Registered in OpenAPI as `cli_login` / `cli_logout` to avoid operationId collisions.
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
- **Internal DNS plane for HA databases (ADR-011)**: new `temps-dns-resolver` crate runs a per-node Hickory DNS resolver that long-polls the control plane and serves UDP/TCP DNS on the bridge gateway. Containers get the resolver wired into `/etc/resolv.conf` automatically (`HostConfig.dns`), so `*.temps.local` resolves natively from inside any cluster member. Three tiers of records: per-member A records (Tier 2), role-aliased VIPs (Tier 3 — `primary.<svc>.temps.local`, `replica.<svc>.temps.local`, `<svc>.temps.local` round-robin), and a fallback to the node's underlay address + host port when no overlay IP exists (covers monitors on the control plane and single-host setups)
- **Postgres HA cluster scale-up (`Add Replica`)**: dedicated `/storage/:id/members/add` page (not a modal) with a 4-step provisioning timeline that polls `GET /external-services/{id}/members/{member_id}` every 1s until the new replica reaches `done` or `failed`. Backend returns `202 Accepted` immediately after inserting the placeholder row; container provisioning + DNS registration runs in a background task that updates `service_members.provisioning_step` after each phase
- **Postgres HA cluster scale-down (`Remove`)**: per-row trash button on every removable replica. Refuses to remove the monitor (singleton), the current primary (failover first), or any member whose removal would drop the cluster below the 2-data-member quorum required for HA. UI mirrors the validation rules so we never offer a button that will 400
- **Postgres HA cluster failover (`Promote ↑`)**: per-row promote button on every replica that runs `pg_autoctl perform promotion` inside the chosen container (local docker exec or remote agent). The monitor demotes the current primary; the role reconciler refreshes role-aliased VIP DNS records on its next tick (≤30s) so app connections to `<cluster>.temps.local` follow the new primary without restart
- **Postgres HA cluster role sync**: role reconciler runs every 5s per cluster, queries the `pg_auto_failover` monitor, and updates `service_members.role` to match reality (mapped via underlay IP). Spawned at plugin startup for every running cluster — survives control-plane restarts. Cluster Members table + ClusterHealthPanel now reflect actual primary/replica state instead of stale create-time labels
- **Cluster Health panel**: live pg_auto_failover monitor view (polls every 5s) with reported→goal state transitions, sync state, replication lag, quorum dot, and a liveness override (`unreachable` when `health<0` or stale beyond 30s). Refresh button for on-demand polling
- **WAL-G physical backups for Postgres HA clusters**: new `gotempsh/postgres-ha:18-bookworm-walg` image bakes WAL-G v3.0.8 alongside `pg_auto_failover`. The Backup button on a cluster service routes through a new `ExternalServiceManager::backup_postgres_cluster` that finds the current primary, writes `walg.env` to every running data member (so failover doesn't lose archiving), runs `wal-g backup-push` against the primary, and enables continuous WAL streaming via `ALTER SYSTEM SET archive_command`. `archive_command` lives in `postgresql.auto.conf` which is replicated through streaming, so a future failover keeps WAL flowing to the same S3 prefix without operator intervention
- **In-place restore for Postgres HA clusters**: Restore on a cluster backup tears down every member (containers + volumes + DNS + service_members rows), pre-seeds the new primary's pgdata via `wal-g backup-fetch` in a one-shot helper container (writes `recovery.signal` + `restore_command` so postgres replays WAL up to consistency on first boot), then re-runs `initialize_cluster` so the rebuilt cluster picks up the recovered data. Same names, same ordinals, same FQDNs — connections via `<cluster>.temps.local` reconnect transparently. Single-host clusters only in this MVP (multi-host needs an agent-side helper-container RPC, refused with a clear message)
- **`add_cluster_member` + `remove_cluster_member` + `promote_cluster_member` REST endpoints**: `POST /external-services/{id}/members`, `GET /external-services/{id}/members/{member_id}`, `DELETE …/{member_id}`, `POST …/{member_id}/promote`. New audit ops `EXTERNAL_SERVICE_CLUSTER_MEMBER_{ADDED,REMOVED,PROMOTED}`
- **`gotempsh/postgres-ha:18-bookworm-walg` Docker image**: source at `images/postgres-ha/Dockerfile`, multi-arch (amd64+arm64), `postgres:18-bookworm` + `pg_auto_failover` + `wal-g v3.0.8`. Pushed to Docker Hub. The legacy `:18-bookworm` tag is left untouched so existing prod clusters keep working until they're recreated against the `-walg` tag

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
- **Runtime log viewer no longer freezes on chatty containers + adds Live/Pause/Interval modes**: a high-volume container (e.g. ~1500 lines/sec) used to lock the runtime page because every WebSocket frame triggered a `setState` and a dead `document.querySelectorAll('[id^="search-match-"]')` DOM scan ran on every log line. The viewer now batches incoming lines via `requestAnimationFrame` and caps the visible list at 5,000. A new mode selector (Live / Pause / Every 5s · 30s · 1m) is persisted per project to `localStorage`; **Pause closes the WebSocket** so no bytes are wasted while the user reads what's on screen, and resuming reconnects with `tail=200` for a smooth catch-up. The pending buffer between flushes is bounded at 1,000 lines and surfaces a `dropped` count so users notice when they're sampling a firehose rather than recording it. Also fixed the same per-line setState problem in `useLogStream` (container logs viewer)
- **Cmd+K "Logs" no longer hidden when typing "logs"**: the command palette wraps Fuse.js for fuzzy matching but `cmdk`'s built-in matcher was *also* filtering on `CommandItem.value` (which is `project-nav-/projects/{slug}/runtime` for the project Logs entry). The URL doesn't contain "logs", so cmdk silently dropped it while keeping "Request Logs" (URL contains `request-logs`). Added `shouldFilter={false}` to the `<Command>` so Fuse alone decides matches
- **Deployment logs WebSocket survives redeploys**: the live log stream on the deployment detail page used to give up the moment it received any normal (1000) WebSocket closure — exactly what happens when a redeploy churns the underlying log source — leaving users staring at a frozen pane. The reconnect path also called `setLogs([])` on every retry, wiping the partial output they had. The hook now reconnects with exponential backoff capped at 10s on every closure (the backend tail is an infinite stream, so any close is unexpected) until the component unmounts, preserves prior log state instead of clearing it, and dedupes by absolute file `line` so the last-1000-lines replay the backend sends after reconnect doesn't show up twice
- **Single click no longer creates 15 duplicate projects**: the "Create Project" button was disabled via React state (`isSubmitting`), but state updates aren't synchronous — a fast double-click slipped multiple submissions through before the disabled prop landed, and since the backend has no `(user, name)` uniqueness each request created a separate project. Both `ProjectConfigurator` and `ManualProjectConfigurator` now use a `useRef` synchronous re-entry guard at the top of `handleSubmit` so the second invocation is dropped before any network call
- **Project creation cleans up half-created rows on failure**: when `create_project` failed any step after the project row was inserted (default environment, env var inserts, storage-link wiring), the project row was left dangling — the handler returned 500 but a half-initialized project showed up in the user's list, and the real cause was buried inside `ProjectError::Other(format!(...))` so the HTTP status was always 500 even when the underlying problem was a 4xx. Post-insert work is now isolated in `finalize_project_creation`; on any error a single `Entity::delete_by_id` on the project row cascades through the foreign keys to clean up environments, env vars, and service links. This avoids holding any cross-service table locks (which a long-lived transaction would). New typed `ProjectError` variants — `EnvironmentCreationFailed`, `EnvVarCreationFailed`, `StorageLinkFailed`, `SlugConflict` — replace the `Other(String)` flatten so failures preserve the original cause and HTTP status
- **Slug conflicts return 409 instead of 500/400**: `SlugAlreadyExists` (was 400) and the new `SlugConflict` (was a generic 500 from `ProjectError::Other`) both map to **409 Conflict**, so concurrent project creates racing on the same slug surface as something the frontend can act on. Detection is now centralized in `is_unique_violation`, which classifies via SQLSTATE `23505` plus textual fallbacks for cross-driver compatibility
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
- **Per-node DNS resolver couldn't sync from the control plane**: the resolver hit `/internal/nodes/{id}/dns/changes` directly but plugin routes mount under `/api`. Every poll silently failed with a JSON parse error (the server returned the SPA's `index.html`), so the per-node Hickory zone never had any records. Prefixed the URL with `/api/`; tests updated to match
- **Container DNS not wired to the temps registry**: cluster containers used Docker's embedded resolver (127.0.0.11) which doesn't know about the registry, so `*.temps.local` always returned NXDOMAIN inside containers. The agent now sets `HostConfig.dns = [<bridge>]` on every container it creates, so containers resolve cluster FQDNs natively
- **Monitor's FQDN never registered in DNS**: `compute_ip` is `None` for the monitor (it's on the control plane, not the multi-host overlay), and the registration code skipped any member without a `compute_ip`. New `resolve_member_underlay` helper synthesises a fallback record using the node's underlay address + host port, so the monitor's FQDN points *somewhere* even without the overlay
- **`Option<DnsRegistry>` silent no-op in cluster init**: a background `tokio::spawn` constructed a fresh `ExternalServiceManager` without the DNS registry, so cluster member containers were created but their DNS records were never published. Removed the `Option`, made `DnsRegistry` a required `Arc<T>` so a missing registry fails at startup instead of silently
- **`reportedstate` enum deserialization panic in role reconciler**: the monitor query needed `reportedstate::text` cast — `pg_auto_failover` stores it as a typed enum and `tokio_postgres` can't deserialize unknown enum types. Reconciler crashed on every tick before this fix
- **`service_members.role` stayed stale after failover**: reconciler updated DNS records but never wrote the row label, so the UI's Cluster Members table showed lies after a promotion. Now syncs the row from the monitor every tick
- **Reconciler not spawned at startup**: only `initialize_cluster` and `add_cluster_member` spawned reconcilers, so any control-plane restart left existing clusters with no role sync. Now spawned for every running postgres cluster at plugin init

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

[Unreleased]: https://github.com/gotempsh/temps/compare/v0.1.0-beta.6...HEAD
[0.1.0-beta.6]: https://github.com/gotempsh/temps/compare/v0.1.0-beta.5...v0.1.0-beta.6
[0.0.7]: https://github.com/gotempsh/temps/compare/v0.0.6...v0.0.7
[0.0.6]: https://github.com/gotempsh/temps/compare/v0.1.0...v0.0.6
[0.1.0]: https://github.com/gotempsh/temps/releases/tag/v0.1.0
