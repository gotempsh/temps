# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0-beta.56] - 2026-08-21

### Added

- **Temps Cloud:** Connect a self-hosted instance to the optional managed control plane in two steps from Settings, mirror OpenTelemetry spans without putting the managed service on the ingest path, and surface connection, buffering, and credential health through the console and CLI.
- **providers:** Reset all accumulated `pg_stat_statements` statistics from the Query Performance page through a write-protected, audited API with explicit destructive-action confirmation.
- **providers:** `pg_stat_statements`-based slow-query monitoring for user-provisioned Postgres services — a dedicated "Query Performance" page (sortable, paginated, with a per-query detail view) alongside a `GET /external-services/{id}/pg-stat-statements/slow-queries` endpoint and a `temps services slow-queries --id <id>` CLI command ([#460](https://github.com/gotempsh/temps/pull/460))
- **providers:** self-service "Enable & Restart" action to load `pg_stat_statements` on standalone Postgres services that predate this feature — clustered/HA services are rejected with a clear error instead, since a blind single-container restart bypasses controlled failover ([#460](https://github.com/gotempsh/temps/pull/460))
- **web:** date/time range picker (15m/1h/24h/7d + custom) on the service Logs screen, and a `temps services logs --id <id>` CLI command with `--from`/`--to` filtering ([#460](https://github.com/gotempsh/temps/pull/460))
- **funnels:** Add time-range shortcuts and show exact completion seconds ([#750](https://github.com/gotempsh/temps/issues/750))
- **projects:** Always show a health indicator on the project card ([#747](https://github.com/gotempsh/temps/issues/747))

### Documentation

- Require DCO sign-off on every commit, add Code of Conduct ([#752](https://github.com/gotempsh/temps/issues/752))

### Fixed

- **projects:** Report the connected Git provider instead of guessing it ([#740](https://github.com/gotempsh/temps/issues/740))
- **web:** Open the highlighted command palette row on Enter ([#751](https://github.com/gotempsh/temps/issues/751))
- **proxy:** Scope the traffic aggregation window cap to unique counts ([#755](https://github.com/gotempsh/temps/issues/755))
- **web:** Handle mutation and fetch errors in ErrorGroupDetail and ErrorEventDetail ([#746](https://github.com/gotempsh/temps/issues/746))

### Testing

- Fix flaky Postgres/Chrome tests and make MongoDB health timeouts diagnosable ([#754](https://github.com/gotempsh/temps/issues/754))

## [0.1.0-nightly.20260821.cdad16f9] - 2026-08-20

### Added

- **flags:** Allow TEMPS_FLAGS_REFRESH_INTERVAL_MS to override the 30s poll ([#733](https://github.com/gotempsh/temps/issues/733))
- **web:** Searchable, refreshable project select on the Proxy page ([#734](https://github.com/gotempsh/temps/issues/734))

### Fixed

- **email:** Scope deployment delivery to authorized projects ([#685](https://github.com/gotempsh/temps/issues/685))
- **web:** Handle fetch errors in BackupDetail and ScheduleDetail with actionable error state. ([#737](https://github.com/gotempsh/temps/issues/737))
- **session-replay:** Stop duplicating and over-recording replay events ([#739](https://github.com/gotempsh/temps/issues/739))

## [0.1.0-nightly.20260820.bfa3b1e2] - 2026-08-19

### Added

- **projects:** Opt in to deploying a project from alternate sources ([#716](https://github.com/gotempsh/temps/issues/716))
- **settings:** Bound project resource overrides with operator ceilings ([#713](https://github.com/gotempsh/temps/issues/713))
- **otel:** Count and alarm on rate-limited/quota-exceeded ingest requests ([#730](https://github.com/gotempsh/temps/issues/730))
- **ai-chat:** Expand read allowlist with ~130 safe GET endpoints ([#732](https://github.com/gotempsh/temps/issues/732))

### CI

- **rust-tests:** Verify temps-captcha-wasm pkg/ is a reproducible build ([#731](https://github.com/gotempsh/temps/issues/731))

### Documentation

- **installation:** Pin wasm-pack to required version 0.13.1 ([#706](https://github.com/gotempsh/temps/issues/706))

### Fixed

- **compose:** Make sandbox capability denials and override rejections actionable ([#723](https://github.com/gotempsh/temps/issues/723))
- **security:** Close 14 open findings from the security review ([#709](https://github.com/gotempsh/temps/issues/709))
- **web:** Support RFC3339 Sentry timestamps ([#727](https://github.com/gotempsh/temps/issues/727))
- **cli:** Honor --data-dir when starting the proxy alongside the console ([#724](https://github.com/gotempsh/temps/issues/724))
- **funnels:** Match CustomData filters against the props column ([#729](https://github.com/gotempsh/temps/issues/729))

### Performance

- **cli:** Build the serve runtime once, before the database pool ([#722](https://github.com/gotempsh/temps/issues/722))
- **geo:** Load the GeoLite2 city database once per process ([#721](https://github.com/gotempsh/temps/issues/721))

## [0.1.0-nightly.20260819.c2d15ad4] - 2026-08-18

### Added

- **monitoring:** Alert when file descriptors/sockets near exhaustion ([#712](https://github.com/gotempsh/temps/issues/712))
- **web:** Deep-link into plugin routes from the console ([#720](https://github.com/gotempsh/temps/issues/720))

### Fixed

- **monitoring:** Normalise uncapped container CPU alerts against host cores ([#714](https://github.com/gotempsh/temps/issues/714))
- **proxy:** Keep tracking page views when Fetch Metadata is absent ([#711](https://github.com/gotempsh/temps/issues/711))
- **web:** Hide Git configuration for projects with no repository ([#717](https://github.com/gotempsh/temps/issues/717))
- **proxy:** Honour the configured preview gateway host port ([#719](https://github.com/gotempsh/temps/issues/719))

## [0.1.0-nightly.20260818.86db3048] - 2026-08-17

### Added

- **onboarding:** Surface the next setup step
- **onboarding:** Add navigable copyable setup prompts
- **email:** Generate DMARC recommendation, fix DNS TXT-record verification precision ([#304](https://github.com/gotempsh/temps/issues/304))
- **analytics:** Select page timeframe from charts ([#696](https://github.com/gotempsh/temps/issues/696))
- **deployments:** Per-project Docker image retention with nightly pruning ([#172](https://github.com/gotempsh/temps/issues/172))
- **cli:** Deploy a drop archive into an existing project ([#703](https://github.com/gotempsh/temps/issues/703))

### Documentation

- **readme:** Fix star history chart across all locales ([#690](https://github.com/gotempsh/temps/issues/690))

### Fixed

- **projects:** Show traffic and deployment media
- **ui:** Keep preset icons visible in dark mode
- **monitoring:** Separate and widen alert rules
- **onboarding:** Anchor checklist navigation controls
- **projects:** Rollback failed domain reassignments
- **ci:** Grant missing permissions for dependency-scan scheduled/manual runs ([#691](https://github.com/gotempsh/temps/issues/691))
- **proxy:** Bucket projects-health summary hours without toUnixTimestamp64Milli ([#698](https://github.com/gotempsh/temps/issues/698))
- **providers:** Allowlist pgvector images, and let operators extend the list ([#697](https://github.com/gotempsh/temps/issues/697))
- **docker:** Bump Alpine base images off EOL 3.19/3.20 to 3.22 ([#692](https://github.com/gotempsh/temps/issues/692))
- **session-replay:** Batch event inserts instead of one round-trip per event ([#701](https://github.com/gotempsh/temps/issues/701))
- **dashboard:** Correct sessions and project navigation ([#700](https://github.com/gotempsh/temps/issues/700))
- **migrate:** Show post-migration maintenance progress and cancel the right backend ([#707](https://github.com/gotempsh/temps/issues/707))
- **proxy:** Redact credentials from persisted proxy logs ([#699](https://github.com/gotempsh/temps/issues/699))
- **compose:** Deliver project secrets to Docker Compose stacks ([#702](https://github.com/gotempsh/temps/issues/702))
- **skills:** Repin the temps skill to the published 0.1.34 integrity ([#710](https://github.com/gotempsh/temps/issues/710))

### Miscellaneous

- **cli:** Release @temps-sdk/cli v0.1.34 and repin skills ([#708](https://github.com/gotempsh/temps/issues/708))

### Performance

- **proxy:** Bound preview limiter flood work

## [0.1.0-nightly.20260817.651f4a16] - 2026-08-16

### Added

- **web:** Redesign project navigation
- **web:** Simplify platform sidebar
- **web:** Restore monitoring workspace
- **web:** Prioritize AI activation in onboarding
- **web:** Verify AI harness activation
- **platform:** Expose feature maturity labels
- **web:** Streamline AI harness onboarding
- **web:** Add platform tool section icons
- **web:** Redesign project list

### Documentation

- **readme:** Tag temps.sh links with UTM parameters ([#688](https://github.com/gotempsh/temps/issues/688))
- **cli:** Sync analytics command references

### Fixed

- **deps:** Patch event-listener RustSec finding ([#97](https://github.com/gotempsh/temps/issues/97))
- **environments:** Allow clearing node placement
- **deployments:** Treat an empty target_nodes list as unconstrained
- **security:** Close final placement and preview gaps
- **providers:** Restore redis database allocation isolation ([#683](https://github.com/gotempsh/temps/issues/683))
- **ai-gateway:** Allow re-enabling a disabled provider model via add ([#687](https://github.com/gotempsh/temps/issues/687))
- **ci:** Keep tag releases from writing Actions caches ([#686](https://github.com/gotempsh/temps/issues/686))
- **web:** Widen service creation layout
- **web:** Clarify platform tools group label
- **platform:** Address navigation redesign review blockers
- **security:** Scope redesign resource access

### Refactor

- **web:** Replace monitoring page with proxy
- **web:** Group platform navigation by intent
- **web:** Promote git providers in navigation
- **web:** Open monitoring on alerts
- **web:** Separate AI harness onboarding
- **web:** Move AI harness out of sidebar

### Testing

- **e2e:** Focus multinode scenario on worker deployment

## [0.1.0-nightly.20260816.3bf831a1] - 2026-08-16

### Added

- **analytics:** Add generic API traffic drilldowns ([#669](https://github.com/gotempsh/temps/issues/669))
- **otel:** Span attribute facets for fast filtering at scale
- **otel:** Production-harden facet backfill + add TimescaleDB parity
- **otel,cli:** Regenerate API clients for facet status/retry, add CLI retry
- **proxy:** Add scoped API traffic drilldowns ([#675](https://github.com/gotempsh/temps/issues/675))

### Documentation

- **providers:** Describe structured MariaDB filters
- **e2e:** Describe HA probe callers

### Fixed

- **otel:** Harden claimed project slug diagnostics ([#666](https://github.com/gotempsh/temps/issues/666))
- **presets:** Upgrade Autopack Python source builds ([#667](https://github.com/gotempsh/temps/issues/667))
- **projects:** Stop mislabeling git connection errors as project-not-found ([#668](https://github.com/gotempsh/temps/issues/668))
- **web:** Chart deploy-marker overlap and Cmd+K palette drift ([#671](https://github.com/gotempsh/temps/issues/671))
- **projects:** Scope env detection and improve repository connections ([#665](https://github.com/gotempsh/temps/issues/665))
- **projects:** Harden shared git settings follow-up ([#673](https://github.com/gotempsh/temps/issues/673))
- **analytics:** Cast ClickHouse traffic percentiles ([#672](https://github.com/gotempsh/temps/issues/672))
- **security:** Harden critical trust boundaries
- **security:** Bundle high severity hardening
- **security:** Address integration review findings
- **security:** Close residual tenant identity races
- **security:** Centralize tenant hostname claims
- **security:** Canonicalize tenant identity claims
- **security:** Isolate proxy inspection quotas
- **ci:** Align tests with security hardening
- **security:** Close upgrade and placement bypasses
- **security:** Validate upgrade recovery paths
- **security:** Fail closed on invalid placement
- **security:** Enforce constrained scheduling
- **tests:** Use supported postgres images in upgrade guard
- **tests:** Use an allowlisted postgres image that actually exists
- **tests:** Use allowlisted postgres image for image-update tests
- **providers:** Probe walg.env from the volume when no live container exists
- **proxy:** Scope production-HTTPS default to resolved project traffic
- **e2e:** Enroll multinode workers over https
- **e2e:** Emit portable probe images
- **e2e:** Use valid control-plane probe placement
- **otel:** Address review findings on facet backfill hardening
- **auth:** Block admin MFA enrollment during password login ([#678](https://github.com/gotempsh/temps/issues/678))
- **deployments:** Enforce project access on cron reads ([#679](https://github.com/gotempsh/temps/issues/679))
- **providers:** Constrain legacy managed service sweep ([#680](https://github.com/gotempsh/temps/issues/680))

### Miscellaneous

- **cli,web:** Regenerate API clients against the merged backend
- **cli,web:** Regenerate API clients after second main merge

### Performance

- **ci:** Inherit trusted nextest target cache ([#674](https://github.com/gotempsh/temps/issues/674))

### Styling

- **providers:** Satisfy rustfmt on wal-probe test helper call
- **proxy:** Satisfy rustfmt on new production-https tests

### Testing

- **providers:** Use managed postgres images
- **e2e:** Expect cross-project DNS isolation

## [0.1.0-nightly.20260815.a23a4903] - 2026-08-14

### Added

- **web:** Redesign project deployment actions ([#660](https://github.com/gotempsh/temps/issues/660))
- **analytics:** Add AI-powered API traffic insights ([#661](https://github.com/gotempsh/temps/issues/661))
- **cli:** Expose performance insights filters ([#662](https://github.com/gotempsh/temps/issues/662))

### Fixed

- **web:** Show branch selector on Connect repository page ([#657](https://github.com/gotempsh/temps/issues/657))
- **analytics:** Preserve events backfill progress output ([#658](https://github.com/gotempsh/temps/issues/658))
- **proxy:** Add per-project/environment concurrent-connection cap ([#655](https://github.com/gotempsh/temps/issues/655))
- **git:** Share live preset/env-example/compose fetches across users ([#659](https://github.com/gotempsh/temps/issues/659))
- **web:** Simplify project header links ([#663](https://github.com/gotempsh/temps/issues/663))
- **otel:** Include project slug in auth warnings ([#664](https://github.com/gotempsh/temps/issues/664))

## [0.1.0-nightly.20260814.af018b31] - 2026-08-13

### Added

- **web:** Surface cluster DNS toggle in Worker Nodes settings ([#647](https://github.com/gotempsh/temps/issues/647))
- **cli:** Add metrics command to query OTel project metrics ([#649](https://github.com/gotempsh/temps/issues/649))
- **web:** Add copy URL on project deployment information ([#651](https://github.com/gotempsh/temps/issues/651))
- **proxy:** Add per-project latency percentiles and custom time ranges ([#652](https://github.com/gotempsh/temps/issues/652))
- **deployments:** Send redacted failure trace to Temps, or open a GitHub issue ([#653](https://github.com/gotempsh/temps/issues/653))

### Build

- **deps:** Bump axum-test from 18.7.0 to 21.0.0 ([#614](https://github.com/gotempsh/temps/issues/614))
- **deps:** Bump schemars from 0.8.22 to 1.2.1 ([#615](https://github.com/gotempsh/temps/issues/615))
- **deps:** Bump totp-rs from 5.7.2 to 6.0.0 ([#607](https://github.com/gotempsh/temps/issues/607))
- **deps:** Bump jsonwebtoken from 10.4.0 to 11.0.0 ([#608](https://github.com/gotempsh/temps/issues/608))

### Fixed

- **proxy:** Keep RouteTableListener alive in split-mode proxy ([#644](https://github.com/gotempsh/temps/issues/644))
- **cli:** Watch the newly triggered deployment, not the previous one ([#637](https://github.com/gotempsh/temps/issues/637))
- **deployments:** Reclaim Docker build cache and old deployment images ([#645](https://github.com/gotempsh/temps/issues/645))
- **web:** Re-measure AI composer height once the dock's open transition settles ([#654](https://github.com/gotempsh/temps/issues/654))
- **database:** Replace testcontainers flat-sleep with real postgres readiness wait ([#648](https://github.com/gotempsh/temps/issues/648))
- **git:** Make git connections shared across all users ([#656](https://github.com/gotempsh/temps/issues/656))

### Miscellaneous

- **cli:** Bump @temps-sdk/cli to 0.1.33 ([#650](https://github.com/gotempsh/temps/issues/650))

## [0.1.0-nightly.20260813.e22f5ddf] - 2026-08-12

### Added

- **proxy:** Configurable request timeouts (global ceiling + per-project override, SSE/WebSocket-aware) ([#642](https://github.com/gotempsh/temps/issues/642))

## [0.1.0-nightly.20260812.86c4c52e] - 2026-08-12

### Added

- **deployments:** Make public readiness check configurable per project/environment
- **cli:** Add public readiness timeout/disable flags to projects config
- **skills:** Add Temps workflow router
- **sandbox:** Sandbox snapshots — take/restore for Docker backend (ADR-037) ([#622](https://github.com/gotempsh/temps/issues/622))
- **skills:** Add start-temps dev skill for local server bootstrap ([#634](https://github.com/gotempsh/temps/issues/634))
- **ai:** Add subscription-backed agent CLI provider foundation
- **ai:** Add provider-aware interactive chat controls

### CI

- **skills:** Secure Temps router

### Fixed

- **cli:** Require target context in write examples
- **deployer:** Require published ports reachable before compose readiness
- **cli:** Pass detected composePath through on drop deploys
- **deployments:** Guard TempDirGuard against removing paths outside safe temp roots ([#633](https://github.com/gotempsh/temps/issues/633))
- **web:** Match new-project repo picker to compact project-settings design
- **skills:** Harden Temps router generation
- **skills:** Contain capability detector reads
- **skills:** Ignore blocking special files ([#638](https://github.com/gotempsh/temps/issues/638))
- **ci:** Fix three unrelated main CI failures ([#632](https://github.com/gotempsh/temps/issues/632))
- **status-page:** Don't collapse degraded monitors into project-down status ([#640](https://github.com/gotempsh/temps/issues/640))
- **deployments:** Remove the always-broken public-readiness rollback gate ([#641](https://github.com/gotempsh/temps/issues/641))
- **ai:** Harden AgentCliAiService against security review findings
- **web:** Restore global AI chat access
- **ai:** Stabilize cli provider chat
- **ai:** Refresh chat model capabilities
- **ai:** Make harness controls and tools reliable
- **ai:** Render API timestamps as dates in answers
- **ai:** Isolate subscription chat tool runtime
- **ai:** Enforce provider runtime boundaries

### Performance

- **otel:** Fix unbounded ClickHouse scans ([#639](https://github.com/gotempsh/temps/issues/639))

### Refactor

- **ai:** Unify provider capabilities and turn runtime

### Testing

- **database:** Derive latest migration in round-trip test
- **ai:** Add infrastructure prompt catalog

## [0.1.0-nightly.20260812.73dbd99e] - 2026-08-12

### Added

- **external-plugins:** Add caller-scoped platform API
- **compose:** Make Git deployments usable end to end ([#592](https://github.com/gotempsh/temps/issues/592))
- **error-tracking:** Add same-origin Sentry tunnel for browser SDKs
- **self-update:** Apply pending migrations before restarting ([#629](https://github.com/gotempsh/temps/issues/629))

### Build

- **deps:** Bump docker/setup-buildx-action from 3 to 4 ([#604](https://github.com/gotempsh/temps/issues/604))
- **deps:** Bump nanoid from 3.3.18 to 6.0.1 in /web ([#601](https://github.com/gotempsh/temps/issues/601))
- **deps:** Bump tower from 0.4.13 to 0.5.3 ([#611](https://github.com/gotempsh/temps/issues/611))
- **deps:** Bump the web-minor-patch group across 1 directory with 12 updates
- **deps:** Bump tokio-tungstenite from 0.29.0 to 0.30.0
- **deps:** Bump the cargo-minor-patch group across 1 directory with 8 updates
- **deps:** Bump actions/checkout from 5 to 7
- **deps:** Bump pem from 3.0.6 to 4.0.0 ([#612](https://github.com/gotempsh/temps/issues/612))
- **deps:** Bump tower-http from 0.6.11 to 0.7.0 ([#613](https://github.com/gotempsh/temps/issues/613))
- **deps:** Bump octocrab from 0.49.9 to 0.54.1 ([#610](https://github.com/gotempsh/temps/issues/610))
- **deps:** Bump framer-motion from 12.43.0 to 13.0.0 in /web

### Documentation

- **cli:** Restructure skill references
- **cli:** Use pinned zero-install commands

### Fixed

- **web:** Redirect to login on expired session instead of hanging on stale screen
- **error-tracking:** Stop leaking DB error detail on tunnel path, fix wildcard route resolution
- **proxy:** Canonicalize Google and Bing crawler names ([#625](https://github.com/gotempsh/temps/issues/625))
- **proxy:** Stop leaking raw HTML under a text/markdown Content-Type ([#626](https://github.com/gotempsh/temps/issues/626))
- **domains:** Persist request_challenge renewal failures, add attempt history
- **deployments:** Clean up leaked /tmp/temps-deployments dirs on download failure ([#630](https://github.com/gotempsh/temps/issues/630))
- **cli:** Generate complete Node-compatible command docs
- **ci:** Pin readiness probes to local proxy
- **deployments:** Probe HTTPS routes via local HTTP listener

### Miscellaneous

- **sdk:** Regenerate CLI and web SDKs after merging main
- **cli:** Release v0.1.31
- **web:** Remove framer-motion dependency

### Testing

- **e2e:** Verify internal application and database DNS ([#619](https://github.com/gotempsh/temps/issues/619))
- Align GitHub and migration expectations
- **e2e:** Wait for deployment completion

## [0.1.0-nightly.20260811.7f6037c7] - 2026-08-10

### Added

- **ai-agents:** Classify plain Googlebot as Google/Mixed
- **cli:** Add otel-forward commands for OTLP relay destinations ([#591](https://github.com/gotempsh/temps/issues/591))

### Documentation

- **security:** Design PostgreSQL datasource TLS trust ([#337](https://github.com/gotempsh/temps/issues/337))

### Fixed

- **ai-agents:** Add ClickHouse backfill counterpart, closes coverage gap
- **cli:** Align apikeys create role vocabulary with backend Role enum ([#616](https://github.com/gotempsh/temps/issues/616))
- **analytics:** Show real duration on bounce/exit pageviews instead of blank ([#618](https://github.com/gotempsh/temps/issues/618))

### Testing

- **e2e:** Close remaining scenario gaps (mariadb restore, env vars, api keys) ([#599](https://github.com/gotempsh/temps/issues/599))
- **e2e:** Add multinode mTLS coverage and scenario CI ([#617](https://github.com/gotempsh/temps/issues/617))

## [0.1.0-nightly.20260810.ccf9c612] - 2026-08-10

### Added

- **e2e:** Otel-quota-scenario + fix silently-inert OTel storage quota ([#586](https://github.com/gotempsh/temps/issues/586))
- **providers:** MongoDB restore_in_place, restore_to_new_service, restore_capabilities ([#596](https://github.com/gotempsh/temps/issues/596))
- **redis:** Implement restore_capabilities, restore_in_place, restore_to_new_service ([#597](https://github.com/gotempsh/temps/issues/597))
- **restore:** S3/MinIO managed-service restore_in_place ([#595](https://github.com/gotempsh/temps/issues/595))

### Fixed

- Correct dual-license metadata and stale redirect status doc ([#598](https://github.com/gotempsh/temps/issues/598))

### Testing

- **e2e:** Add pitr-scenario for point-in-time postgres recovery ([#585](https://github.com/gotempsh/temps/issues/585))
- **e2e:** Deploy-lifecycle-scenario (rollback/pause/resume/promote) + fix 3 real bugs ([#587](https://github.com/gotempsh/temps/issues/587))
- **e2e:** Add db-ha-failover-scenario for Postgres HA/pg_auto_failover ([#588](https://github.com/gotempsh/temps/issues/588))
- **e2e:** Add pg-upgrade-scenario for Postgres major-version upgrades ([#593](https://github.com/gotempsh/temps/issues/593))

## [0.1.0-nightly.20260809.a6a711b9] - 2026-08-08

### Added

- **projects:** Mark environment variables as secrets at project creation
- **otel:** Add OtelRelay extension point for telemetry forwarding plugins ([#583](https://github.com/gotempsh/temps/issues/583))
- **teams:** Add ProjectAccessChecker::invalidate_permissions_cache ([#589](https://github.com/gotempsh/temps/issues/589))

### Fixed

- **security:** Harden postgres credential transport
- **security:** Enforce private postgres consumers

### Styling

- **web:** Drop incidental prettier reformatting from the env-var change

### Testing

- **e2e:** DNS/TLS/email/CLI + observability/data-storage e2e coverage ([#582](https://github.com/gotempsh/temps/issues/582))

## [0.1.0-nightly.20260808.e02f5881] - 2026-08-07

### Added

- **sandbox:** Add persistent workspace sandboxes with wake-on-access
- **cli:** Resolve workspace source from project, repo, or local checkout
- **sandbox:** Add interactive terminal and `sandbox shell`
- **web:** Regenerate the SDK and surface workspaces in the console
- **deployer:** Satisfy compose env_file references automatically

### Build

- **cli:** Add spec:update to refresh openapi.json canonically

### CI

- **cli:** Enforce canonical openapi.json in a hook and in CI
- **sandbox:** Install protoc in the beta image prepare job

### Documentation

- **adr:** Add ADR-036 for persistent workspace sandboxes

### Fixed

- **settings:** Close the review findings on console updates
- **auth:** Only require step-up from users who have enrolled MFA
- **sandbox:** Restore terminal echo, work dir, and CLI response shapes
- **sandbox:** Address review findings on the terminal and step-up policy
- **sandbox:** Address review findings on workspaces and the terminal
- **sandbox:** Normalise the PTY from the host so terminals echo on any image
- **web:** Raise the js-yaml override past the advisory, and make detach honest
- **sandbox:** Repair compound --cmd, bound agent writes, surface oversized input
- **projects:** Normalize blank directory on settings and git updates
- **web:** Detect presets and prefill the URL on the public-repo path
- **auth:** Audit permission denials safely
- **auth:** Bound permission denial audit storage
- **security:** Bound query memory and audit data
- **security:** Reject oversized rows before encoding
- **security:** Redact audited data identifiers
- **cli:** Sanitize untrusted terminal output
- **cli:** Sanitize invalid filter errors
- **security:** Resolve PR review findings
- **security:** Enforce pre-wire Redis budgets
- **redis:** Bound aggregate admission work
- **security:** Close final review gaps
- **security:** Bound metadata scan work
- **redis:** Preserve bounded cursor paging
- **review:** Close final compatibility gap

### Miscellaneous

- **cli:** Regenerate openapi spec and client
- **sdk:** Release analytics-core v0.0.3, react-analytics v0.0.5, analytics-browser v0.0.3, svelte-analytics v0.0.2, vue-analytics v0.0.2 ([#575](https://github.com/gotempsh/temps/issues/575))
- **cli:** Release v0.1.30, release node-sdk v0.0.7 ([#574](https://github.com/gotempsh/temps/issues/574))

### Styling

- **cli:** Canonicalize openapi.json key order and formatting

### Testing

- **e2e:** Make console/CLI e2e suite parallel-safe, add API-key coverage

## [0.1.0-nightly.20260807.146be0c2] - 2026-08-06

### Added

- **analytics:** Crawler opt-in for breakdowns, and prune NULL keys early
- **settings:** Apply releases and restart from the console
- **settings:** Version page, release channels, and install without a restart
- **providers:** Read-only GET rows endpoint and per-service AI data access opt-in
- **cli:** Add 'temps data' for read-only browsing of service data
- **query-postgres,web:** Show views, list-level stats, and per-engine labels
- **web:** Independent tree scroll, column selection, and row detail panel
- **web:** Container overview, sidebar sizes, per-tab scroll, and S3 fixes
- **otel:** Rank operations by latency, volume, and variability
- **cli:** Add `traces span-stats` for operation latency
- **web:** Add an Operations view to Traces

### Documentation

- **analytics:** Declare include_crawlers on the timeline endpoint
- Require features to be discoverable and onboard when unconfigured
- **cli:** Document the data command group
- **skills:** Document the data command group in the temps-cli skill

### Fixed

- **analytics:** Classify self-referrals as Direct, not Referral
- **analytics:** Exclude crawler traffic from breakdowns and top pages
- **cli:** Make analytics overview locations and sparkline respect --period
- **analytics:** Populate visitor language instead of always "Unknown"
- **analytics:** Send language from the shared browser SDK core too
- **analytics:** Close review findings on attribution, language and CH parity
- **cli:** Reject release tags that escape the temps repository
- **web:** Restore deep paths on reload and compact the data browser
- **providers:** Clamp row limit for every caller and cover the AI gate with tests
- **web:** Drop a sort field the current table does not have
- **query-postgres,web:** Count views in entity_count, inline the entity breadcrumb
- **security:** Close connection-string injection, SQL denylist bypass, and markdown exfiltration
- **security:** Share one SQL validator, close MariaDB bypasses and a remote panic
- **security:** Close the nine findings from the data-browser audit
- **security:** Close the review findings, and make the TLS ladder actually work
- **security:** Close the re-audit findings, including a UNION injection I added
- **web:** Give sole create actions an N shortcut and guard it behind overlays
- **web:** Rank palette results by relevance, add Teams, resizable data-browser tree
- **web:** Show unified-trace projects as a colour legend, not a slug per span
- **web:** Make Back work on deep-linked pages
- **web:** Address self-review findings on the shortcut hook and project dot
- **web:** Order sibling spans correctly and show exact durations
- **config,proxy:** Stop settings writes from wiping the admin gate
- **proxy:** Close fail-open paths in the admin gate reload
- **ci:** Unbreak the two checks that are red on every PR
- **otel:** Bound span-stats project count and time window

### Miscellaneous

- Use neutral placeholder names in examples and comments
- **scripts:** Seed OTel traces across projects and services
- **scripts:** Replay a captured trace into a local instance

### Performance

- **query-postgres:** Use planner stats for row count, and report table size
- **query:** Avoid full scans for MariaDB row counts, add MongoDB size

### Revert

- **web:** Drop the data-browser tree navigateTo routing

## [0.1.0-nightly.20260806.c64e8f98] - 2026-08-06

### Added

- **console:** Improve user and project onboarding ([#552](https://github.com/gotempsh/temps/issues/552))
- **flags:** Multi-language integration guide, and shiki-highlighted code blocks ([#556](https://github.com/gotempsh/temps/issues/556))
- **env-vars:** Convert an existing variable to a write-only secret ([#555](https://github.com/gotempsh/temps/issues/555))
- **dns:** Govern unattended provider automation ([#554](https://github.com/gotempsh/temps/issues/554))

### Fixed

- **git:** Stop invisible connections from blocking provider deletion ([#553](https://github.com/gotempsh/temps/issues/553))
- **telemetry:** Report git-describe version, not static Cargo.toml version ([#558](https://github.com/gotempsh/temps/issues/558))
- **sandbox:** Free volumes and work dirs when a sandbox is destroyed ([#523](https://github.com/gotempsh/temps/issues/523))

## [0.1.0-nightly.20260805.c6bd08de] - 2026-08-04

### Added

- **presets:** Replace the nixpacks build engine with autopack ([#530](https://github.com/gotempsh/temps/issues/530))
- **auth:** Add step-up verification for sensitive actions ([#547](https://github.com/gotempsh/temps/issues/547))
- **flags:** Feature flags Phase 1 — backend, CLI, SDK and console UI ([#526](https://github.com/gotempsh/temps/issues/526))
- **ai:** Let the assistant propose metric alert rules from a project's own telemetry ([#521](https://github.com/gotempsh/temps/issues/521))
- **teams:** Teams and project-scoped RBAC in OSS ([#486](https://github.com/gotempsh/temps/issues/486))
- **drop:** Deploy uploaded source archives ([#549](https://github.com/gotempsh/temps/issues/549))

### Fixed

- **proxy:** Keep backend latency for streaming sessions ([#545](https://github.com/gotempsh/temps/issues/545))
- **web:** Advance traces time window on refresh and skip it for trace-id search ([#550](https://github.com/gotempsh/temps/issues/550))
- **deps:** Patch aiohttp and cryptography Dependabot advisories ([#551](https://github.com/gotempsh/temps/issues/551))
- **otel:** Bound ingest memory under exporter bursts ([#544](https://github.com/gotempsh/temps/issues/544))

## [0.1.0-nightly.20260804.c51ac6c2] - 2026-08-03

### Added

- **providers:** Reset pg_stat_statements statistics ([#527](https://github.com/gotempsh/temps/issues/527))
- **sandbox:** Mint shareable preview links with expiring session grants ([#525](https://github.com/gotempsh/temps/issues/525))
- **telemetry:** Add deployment failure taxonomy and template context ([#546](https://github.com/gotempsh/temps/issues/546))

### Fixed

- **web:** Stop env var edit modal showing blank value after save ([#524](https://github.com/gotempsh/temps/issues/524))
- **web:** Add icons and prioritize projects in command search
- **deps:** Bump brace-expansion override to ^5.0.9 for GHSA-rgw5-rvv9-x895 ([#548](https://github.com/gotempsh/temps/issues/548))

### Miscellaneous

- **deps-dev:** Bump @types/react-dom in /web in the react group ([#528](https://github.com/gotempsh/temps/issues/528))

### Refactor

- **web:** Move project setup into project sidebar ([#541](https://github.com/gotempsh/temps/issues/541))

## [0.1.0-nightly.20260803.ae960395] - 2026-08-02

### Added

- **providers:** Warn when a cluster has no node accepting writes
- **audit:** Support events whose actor has no resolvable account ([#411](https://github.com/gotempsh/temps/issues/411))
- **audit:** Record failed logins and rejected MFA codes ([#412](https://github.com/gotempsh/temps/issues/412))
- **environments:** Per-environment HTTP→HTTPS redirect override, with an unconditional ACME bypass ([#522](https://github.com/gotempsh/temps/issues/522))
- **security:** Block cloud-metadata egress from app containers

### CI

- Fix the cache-budget leak that makes Compose Security take 92 minutes ([#516](https://github.com/gotempsh/temps/issues/516))

### Fixed

- **providers:** Make cluster creation survive bad placement and stay retryable
- **deployments:** Write the clone failure into the deploy log
- **projects:** Reject repo changes that would strand the clone URL
- **web:** Stop claiming a project deployed when its only run failed
- **providers:** Treat unreachable cluster nodes as leaderless
- **proxy:** Exclude streaming sessions from latency metrics ([#514](https://github.com/gotempsh/temps/issues/514))
- **domains:** Validate custom-domain input and scope it to the caller's project ([#515](https://github.com/gotempsh/temps/issues/515))
- **cli:** Repo lookup by search, nested group paths, and --secret for env vars ([#517](https://github.com/gotempsh/temps/issues/517))
- **core:** Make Visit links reachable on sslip.io installs ([#488](https://github.com/gotempsh/temps/issues/488))
- **web:** Show a back button in the collapsed sidebar's sub-navigation ([#520](https://github.com/gotempsh/temps/issues/520))
- **git:** Keep clone credentials out of errors
- **auth:** Remove code-execution permissions from PlatformAdmin
- **auth:** Close platform admin sandbox creation gap
- **web:** Make the AI chat page fill the content area exactly ([#518](https://github.com/gotempsh/temps/issues/518))
- **providers:** Persist inferred parameters on the plugin init path
- **providers:** Check loopback when picking host ports, and report real service health
- **providers:** Route MariaDB port selection through the shared finder
- **providers:** Persist cluster topology atomically
- **email:** Close SSRF hole and DNS-rebinding gap in SMTP deliverability probe
- **email:** Complete SMTP SSRF hardening
- **email:** Keep validation configuration database-ready
- **email:** Close remaining SMTP proxy SSRF gaps
- **security:** Make metadata egress blocking atomic

### Testing

- **web:** Verify project deployment labels

## [0.1.0-nightly.20260802.ba96c0f7] - 2026-08-02

### Added

- **deployer:** Attach app containers to required extra Docker networks ([#501](https://github.com/gotempsh/temps/issues/501))
- **deployments:** Support worker nodes of a different architecture
- **deployments:** Make cross-architecture builds opt-in
- **deployments:** Report skipped nodes and refuse impossible replica counts
- **projects:** Expose cross-architecture builds in the deployment config API

### Documentation

- **multi-node:** Document opt-in cross-builds and replica shortfall
- **multi-node:** Correct the config key and regenerate the SDK

### Fixed

- **web:** Align react with react-dom at 19.2.8 and guard against future drift ([#505](https://github.com/gotempsh/temps/issues/505))
- **deployments:** Reject cross-builds the legacy Docker builder mislabels
- **deployments:** Never record a guessed node architecture
- **deployments:** Close two paths that still trusted a guessed platform
- **deployments:** Bound cross-build targets and record per-replica images
- **deployments:** Act only on a confirmed control-plane platform
- **deployments:** Discover the daemon platform on paths that never build
- **deployer:** Keep the ARM variant through discovery and verification
- **deployments:** Degrade instead of failing the cluster's builds
- **deployments:** Check replica shortfall before the local-only fallback
- **deployments:** Record the peer address on architecture-change audits
- **deployments:** Clean up containers before owner deletion
- **deployments:** Remove unsafe cross-instance orphan sweep
- **providers:** Derive managed service container names in one place ([#503](https://github.com/gotempsh/temps/issues/503))

### Testing

- **web:** Add console end-to-end UI tests, and fix the post-login 404 they found ([#511](https://github.com/gotempsh/temps/issues/511))

## [0.1.0-nightly.20260801.324ad120] - 2026-08-01

### CI

- Run temps-otel and four other crates' integration tests

### Documentation

- **contributing:** Specify temps binary for running server

### Fixed

- **auth:** Gate user administration on a dedicated users:manage permission
- **auth:** Restrict audit:read to administration roles
- **web:** Gate audit-log UI to roles with audit:read
- **web:** Import Navigate from react-router, not react-router-dom
- **otel:** Restore exact span dedup after dropping FINAL
- **sdk:** Regenerate clients for the include_total contract
- **imports:** Make repository link optional, add Portainer TLS skip
- **cli:** Environments resources ignored -p, always resolving "Project not found"
- **providers:** Enforce project scoping for session/API-key/CLI callers
- **import:** Deploy docker-source imports for real instead of leaving them pending
- **domains:** Reserve the console hostname from project domains
- **kv,blob:** Confine data-plane access to the caller's projects
- **auth:** Select jsonwebtoken crypto provider ([#497](https://github.com/gotempsh/temps/issues/497))

### Performance

- **otel:** Make trace-summaries scale with the query window

### Styling

- **kv,blob:** Apply rustfmt

### Testing

- **audit:** Add authorization tests for audit-log endpoints
- **auth:** Relocate audit:read matrix test to avoid overlap with #350

## [0.1.0-nightly.20260731.42fe6068] - 2026-07-30

### Fixed

- **ci:** Pin privileged workflow actions ([#473](https://github.com/gotempsh/temps/issues/473))
- **auth:** Enforce privilege ceiling on key rotation ([#472](https://github.com/gotempsh/temps/issues/472))
- **security:** Audit explicit credential reveals ([#459](https://github.com/gotempsh/temps/issues/459))
- **providers:** Sort slow queries server-side instead of client-side ([#480](https://github.com/gotempsh/temps/issues/480))
- **projects:** Resolve nixpacks variants to base nixpacks preset
- **presets:** Generalize nixpacks provider selection
- **web:** Align preset config request types
- **deployments:** Persist terminal status for jobs that never run
- **screenshots:** Redact credentials from remote provider health-check errors
- **core:** Cancel jobs left pending when a workflow aborts mid-batch

### Miscellaneous

- **web:** Regenerate Nixpacks preset types

### Testing

- **presets:** Remove ineffective struct update

## [0.1.0-nightly.20260730.e145a940] - 2026-07-30

### Fixed

- **release:** Install protoc for sandbox helpers ([#479](https://github.com/gotempsh/temps/issues/479))

## [0.1.0-nightly.20260730.ed853d3a] - 2026-07-30

### Fixed

- **release:** Use git cli for dependencies ([#476](https://github.com/gotempsh/temps/issues/476))

## [0.1.0-nightly.20260729.347ef444] - 2026-07-29

### Fixed

- **release:** Identify repository for nightly dispatch ([#475](https://github.com/gotempsh/temps/issues/475))

## [0.1.0-nightly.20260729.bd382d68] - 2026-07-29

### Added

- **skills:** Add temps-best-practices skill ([#453](https://github.com/gotempsh/temps/issues/453))
- **providers:** Pg_stat_statements slow-query monitoring + service log filtering ([#460](https://github.com/gotempsh/temps/issues/460))

### Documentation

- **skills:** Add runtime and telemetry guardrails ([#469](https://github.com/gotempsh/temps/issues/469))

### Fixed

- **otel:** Warn AI chat that only duration_ms is milliseconds, not raw attributes ([#461](https://github.com/gotempsh/temps/issues/461))
- **skills:** Harden skill security and add full audit gate ([#462](https://github.com/gotempsh/temps/issues/462))
- **web:** Duplicate day-label tooltip + cli: --connection flag for projects git ([#456](https://github.com/gotempsh/temps/issues/456))
- **deployments:** Scope job metadata to projects ([#465](https://github.com/gotempsh/temps/issues/465))
- **web:** Portal DropdownMenuSubContent to escape clipped parent ([#467](https://github.com/gotempsh/temps/issues/467))
- **release:** Dispatch builds for nightly tags ([#470](https://github.com/gotempsh/temps/issues/470))
- **release:** Recover failed nightly dispatches ([#471](https://github.com/gotempsh/temps/issues/471))

### Performance

- **build:** Isolate BuildKit cache mounts ([#463](https://github.com/gotempsh/temps/issues/463))

## [0.1.0-nightly.20260729.419505e0] - 2026-07-28

### Added

- **monitoring:** Metric alerts as config-as-code in .temps.yaml ([#454](https://github.com/gotempsh/temps/issues/454))
- **cli:** Add `temps projects secrets` subcommand ([#458](https://github.com/gotempsh/temps/issues/458))

### Documentation

- **readme:** Move why-statement above the fold, add stop-paying banner ([#457](https://github.com/gotempsh/temps/issues/457))

### Fixed

- **cli:** Add per-command --context flag to avoid silent wrong-server runs ([#455](https://github.com/gotempsh/temps/issues/455))

## [0.1.0-nightly.20260727.32b7f235] - 2026-07-27

### Added

- **agents:** Configurable AI autofix runs with per-provider turn limits ([#435](https://github.com/gotempsh/temps/issues/435))
- **email:** Choose provider type on a dedicated page before configuring ([#438](https://github.com/gotempsh/temps/issues/438))
- **web:** Onboard users into AI autofix from error tracking ([#439](https://github.com/gotempsh/temps/issues/439))
- **sandbox:** Track agent-run sandboxes as first-class sandbox items ([#436](https://github.com/gotempsh/temps/issues/436))
- **import:** Kubernetes, Coolify, Dokploy, CapRover, Portainer, and Kamal importers with deploy-and-verify ([#441](https://github.com/gotempsh/temps/issues/441))

### CI

- **release:** Add nightly build workflow ([#452](https://github.com/gotempsh/temps/issues/452))

### Documentation

- README overhaul, unified project creation, provider brand logos ([#446](https://github.com/gotempsh/temps/issues/446))

### Fixed

- **web:** Stop analytics/errors setup-redirect from breaking project tour ([#434](https://github.com/gotempsh/temps/issues/434))
- **agents:** Stop dumping raw CLI JSONL as autofix error messages ([#437](https://github.com/gotempsh/temps/issues/437))
- **web:** Clear all high-severity bun audit advisories ([#443](https://github.com/gotempsh/temps/issues/443))
- **deps:** Patch Dependabot advisories in next, setuptools and serde_with ([#442](https://github.com/gotempsh/temps/issues/442))
- **import:** Trim session credential lifetime + drop stale web cast ([#448](https://github.com/gotempsh/temps/issues/448))
- **monitoring:** Close race that fires false ContainerCrash alerts on deploy ([#451](https://github.com/gotempsh/temps/issues/451))

### Performance

- **observe:** Stop proxy-log and span listings scanning the whole retention window ([#447](https://github.com/gotempsh/temps/issues/447))

## [0.1.0-beta.54] - 2026-07-24

### Added

- **otel:** Store cross-project trace refs in ClickHouse when enabled ([#429](https://github.com/gotempsh/temps/issues/429))

### Fixed

- **web:** Bump rrweb-player to 2.1.1 (broken 2.1.0 dist → blank replays) ([#430](https://github.com/gotempsh/temps/issues/430))

## [0.1.0-beta.53] - 2026-07-23

### Added

- **error-tracking:** Source context for native stack traces (Go/Rust/all languages) ([#419](https://github.com/gotempsh/temps/issues/419))
- **web:** Add project onboarding tour
- **web:** Add subtle "Take a tour" relaunch on project overview
- **error-tracking:** Default source capture to the Docker build context + configurable root ([#423](https://github.com/gotempsh/temps/issues/423))

### Fixed

- **web:** Ignore spurious empty-string onValueChange from Radix Select ([#424](https://github.com/gotempsh/temps/issues/424))
- **deployments:** Stop infinite reconnect loop on container logs ([#425](https://github.com/gotempsh/temps/issues/425))
- **observability:** Read Observe feed through ClickHouse-aware storage backends ([#426](https://github.com/gotempsh/temps/issues/426))

### Miscellaneous

- **templates:** Update observability-starter entry for Cadence demo

## [0.1.0-beta.52] - 2026-07-22

### Added

- **containers:** Show metrics history in container detail ([#415](https://github.com/gotempsh/temps/issues/415))
- **config:** Expose sandbox backend selection in settings API and UI ([#414](https://github.com/gotempsh/temps/issues/414))

### Fixed

- **web:** Override brace-expansion and js-yaml to patched versions ([#409](https://github.com/gotempsh/temps/issues/409))
- **observability:** Prevent ClickHouse Array(Nothing) decode failures ([#408](https://github.com/gotempsh/temps/issues/408))
- **clickhouse:** Align query result integer types ([#416](https://github.com/gotempsh/temps/issues/416))

## [0.1.0-beta.51] - 2026-07-21

### Added

- **deployments:** Preview commits before deployment ([#379](https://github.com/gotempsh/temps/issues/379))
- **email:** Dedup shared email UI helpers, working event filters, per-domain delivery stats ([#307](https://github.com/gotempsh/temps/issues/307))
- **deployments:** Preview tag commits before deployment ([#383](https://github.com/gotempsh/temps/issues/383))
- **email:** Add provider detail page with domains and delivery tracking setup ([#382](https://github.com/gotempsh/temps/issues/382))
- **telemetry:** Add deploy_cancelled event ([#385](https://github.com/gotempsh/temps/issues/385))
- **web:** Add metrics storage backend selector to monitoring settings ([#399](https://github.com/gotempsh/temps/issues/399))
- **sandbox:** Firecracker microVM backend alongside Docker (ADR-029)

### Fixed

- **cli:** Type email command output and sanitize rendered bodies ([#306](https://github.com/gotempsh/temps/issues/306))
- **email:** Secure SES SNS event processing with one-click tracking setup ([#297](https://github.com/gotempsh/temps/issues/297))
- **email:** Stop leaking suppressed recipient addresses via logs/error_message ([#380](https://github.com/gotempsh/temps/issues/380))
- **web:** Make DNS records table horizontally scroll on mobile ([#381](https://github.com/gotempsh/temps/issues/381))
- **email:** Correct SES IAM action namespace from sesv2: to ses: ([#384](https://github.com/gotempsh/temps/issues/384))
- **analytics:** Derive bounce/entry/exit from session pageviews at query time ([#398](https://github.com/gotempsh/temps/issues/398))
- **sandbox:** Address PR #400 review — CI, OpenAPI/SDK, guard, security
- **sandbox:** Satisfy clippy + vercel-compat guardrail (PR #400 CI)
- **cli:** Update sandbox_url tests to canonical plural route
- **proxy:** Resolve request-log detail by request_id across storage backends ([#402](https://github.com/gotempsh/temps/issues/402))
- **audit:** Keep audit history when a user account is deleted ([#386](https://github.com/gotempsh/temps/issues/386))

### Miscellaneous

- **auth:** Remove magic-link login ([#375](https://github.com/gotempsh/temps/issues/375))

## [0.1.0-beta.50] - 2026-07-17

### Added

- **backup:** Dump only critical table data in control-plane backups ([#367](https://github.com/gotempsh/temps/issues/367))
- **proxy:** Trust CF-Connecting-IP from verified Cloudflare egress ranges ([#368](https://github.com/gotempsh/temps/issues/368))
- **monitoring:** Track container CPU/memory for external services ([#371](https://github.com/gotempsh/temps/issues/371))
- **observability:** Compress immutable telemetry after 24h ([#370](https://github.com/gotempsh/temps/issues/370))
- **telemetry:** Aggregated anonymous error_summary event ([#373](https://github.com/gotempsh/temps/issues/373))
- **telemetry-api:** Accept error_summary event ([#374](https://github.com/gotempsh/temps/issues/374))
- **auth:** Let deployment tokens call the AI gateway (ai_gateway:execute) ([#377](https://github.com/gotempsh/temps/issues/377))

### CI

- **compose-security:** Cache Docker toolchain layers and prebuild with fast profile ([#362](https://github.com/gotempsh/temps/issues/362))

### Documentation

- **skill:** Add remote-over-SSH install method to temps-platform-setup ([#366](https://github.com/gotempsh/temps/issues/366))

### Fixed

- **core:** Resolve request IP trust-awarely for audit/logging ([#363](https://github.com/gotempsh/temps/issues/363))
- **skill:** Remove piped shell install and explicit credential paths from docs ([#365](https://github.com/gotempsh/temps/issues/365))
- **deployments:** Emit deploy_succeeded telemetry on the real success path ([#372](https://github.com/gotempsh/temps/issues/372))
- **webhooks:** Pin delivery to validated IP to close DNS-rebinding SSRF ([#332](https://github.com/gotempsh/temps/issues/332))
- **observability:** Read active Timescale policies ([#378](https://github.com/gotempsh/temps/issues/378))

## [0.1.0-beta.49] - 2026-07-16

### Added

- **analytics:** Add insights panel with stat and AI insights
- **analytics:** Put insights behind a compact toggle button
- **ai-chat:** Default-on read-only chat; route analytics AI insights through project chat
- **analytics:** Add raw event entries drill-down with JSON props view ([#359](https://github.com/gotempsh/temps/issues/359))
- **settings:** Show web-console banner when a newer release is available ([#353](https://github.com/gotempsh/temps/issues/353))

### Fixed

- **analytics:** Harden AI insights prompt + warn on partial AI disable
- **proxy:** Harden preview cookie session handling ([#361](https://github.com/gotempsh/temps/issues/361))

### Miscellaneous

- **web:** Replace rocketship logo assets with the t brand mark ([#358](https://github.com/gotempsh/temps/issues/358))

### Performance

- **metrics:** Time-bound external-service latest-metric queries ([#364](https://github.com/gotempsh/temps/issues/364))

## [0.1.0-beta.48] - 2026-07-15

### Added

- **skills:** Add estimate-temps-savings skill ([#357](https://github.com/gotempsh/temps/issues/357))

### Fixed

- **backup:** Stop stranding failed uploads that block retention cleanup ([#356](https://github.com/gotempsh/temps/issues/356))

### Miscellaneous

- **mcp:** Remove @temps-sdk/mcp package ([#355](https://github.com/gotempsh/temps/issues/355)) [**BREAKING**]

## [0.1.0-beta.47] - 2026-07-15

### Added

- **error-tracking:** Deep-link error alert emails, verify Slack stays HTML-free ([#308](https://github.com/gotempsh/temps/issues/308))
- **analytics:** Add daily returning visitor metric ([#346](https://github.com/gotempsh/temps/issues/346))
- **backups:** Add retention cleanup and manual deletion ([#336](https://github.com/gotempsh/temps/issues/336))
- **monitoring:** Monitor all mounted disks for disk-space alerts ([#349](https://github.com/gotempsh/temps/issues/349))
- **settings:** Add flat public hostname strategy ([#146](https://github.com/gotempsh/temps/issues/146))

### CI

- Remove dependabot auto-merge workflow ([#345](https://github.com/gotempsh/temps/issues/345))

### Fixed

- **metrics:** Don't UNION checkpoint queries across pg_stat_checkpointer/bgwriter ([#290](https://github.com/gotempsh/temps/issues/290))
- **metrics:** Use mongodb's re-exported bson instead of a standalone dep ([#291](https://github.com/gotempsh/temps/issues/291))
- **migrations:** Skip empty visitor deduplication rewrites ([#294](https://github.com/gotempsh/temps/issues/294))
- **deployer:** Harden compose deployments
- **deployer:** Close compose security policy bypasses from review
- **deployer:** Reject interpolation bypass and confine compose paths
- **deployer:** Prevent compose conflict container deletion
- **deployer:** Fold inline compose override allow-list into host-escape hardening
- **deployer:** Close compose host-escape bypasses found in review
- **core:** Update node pki for rcgen 0.14
- **deps:** Resolve RustSec advisory updates
- **proxy:** Update test cert generation for rcgen 0.14
- **deployer:** Close volumes_from and absolute bind-mount host-escape bypasses
- **deployer:** Close remaining compose host-escape gaps from review
- **deployer:** Close compose symlink escape paths
- **deployments:** Cap hosted website memory by default ([#164](https://github.com/gotempsh/temps/issues/164))
- **deps:** Unbreak build — pin aws-smithy (schema 0.1.0) and revert sqlx to 0.8 ([#333](https://github.com/gotempsh/temps/issues/333))
- **auth:** Prevent MFA challenge session from authenticating real requests ([#326](https://github.com/gotempsh/temps/issues/326))
- **web:** Satisfy ESLint 10 assignment rules
- **git:** Constant-time comparison for GitHub webhook HMAC signatures ([#334](https://github.com/gotempsh/temps/issues/334))
- **auth:** Close assign_role privilege-escalation (admin gate + single target) ([#324](https://github.com/gotempsh/temps/issues/324))
- **webhooks:** Close retry_delivery cross-tenant IDOR ([#329](https://github.com/gotempsh/temps/issues/329))
- **otel:** Make otel_spans compression effective by dropping trace_id from segmentby ([#348](https://github.com/gotempsh/temps/issues/348))
- **compose:** Require DB/Redis secrets, stop publishing internal ports on 0.0.0.0 ([#330](https://github.com/gotempsh/temps/issues/330))
- **git:** Bind webhook tokens to projects ([#335](https://github.com/gotempsh/temps/issues/335))
- **query-postgres:** Block function-call SQLi bypass in data-explorer WHERE clauses ([#328](https://github.com/gotempsh/temps/issues/328))

### Miscellaneous

- **deployer:** Narrow compose hardening scope
- **deps-dev:** Bump @eslint/js from 9.37.0 to 10.0.1 in /web

### Performance

- **web:** Lazy-load and paginate proxy traffic-by-project table ([#292](https://github.com/gotempsh/temps/issues/292))

### Styling

- **deployer:** Cargo fmt compose policy

### Testing

- **metrics:** Regression coverage for pg_stat_checkpointer/bgwriter query ([#293](https://github.com/gotempsh/temps/issues/293))

## [0.1.0-beta.46] - 2026-07-12

### Added

- **otel,proxy:** Parameterize ClickHouse retention via per-row retention_days
- **web:** Add per-dimension web vitals breakdown to speed insights
- **analytics-performance:** Geo breakdowns and segment filters for speed metrics
- **web:** World map of web vitals by country on the speed page
- **analytics-performance:** Read-time bot filtering for speed metrics

### Documentation

- **agents:** Document Docker safety constraints ([#232](https://github.com/gotempsh/temps/issues/232))
- **retention:** Remove EE mentions from OSS comments

### Fixed

- **web:** Adapt chart and replay types to recharts 3 and rrweb-player 2 ([#277](https://github.com/gotempsh/temps/issues/277))
- **otel,proxy:** Register RetentionResolver via the service registry
- **otel,proxy:** Defer RetentionResolver lookup past plugin registration order
- **analytics:** Exclude bots and datacenter IPs from live-visitors ([#281](https://github.com/gotempsh/temps/issues/281))
- **proxy:** Thread a shared RetentionResolver into the live Pingora proxy
- **retention:** Write-once guard on RetentionResolverSlot + guardrail docs
- **analytics-performance:** Group page metrics on performance_metrics columns
- **react-analytics:** Send pathname field the speed ingest endpoint expects
- **security:** Close three unauthenticated-access CRITICALs ([#288](https://github.com/gotempsh/temps/issues/288))
- **providers:** Reject client-supplied internal-only fields on external-service create ([#287](https://github.com/gotempsh/temps/issues/287))

### Miscellaneous

- **deps-dev:** Bump eslint from 9.37.0 to 10.6.0 in /web ([#213](https://github.com/gotempsh/temps/issues/213))
- **deps-dev:** Bump @rsbuild/core from 1.5.1 to 2.1.4 in /web ([#209](https://github.com/gotempsh/temps/issues/209))
- **deps-dev:** Bump @rsbuild/plugin-react from 1.4.0 to 2.1.0 in /web ([#217](https://github.com/gotempsh/temps/issues/217))
- **sdk:** Bump analytics-browser, kv, blob, node-sdk for npm publish ([#286](https://github.com/gotempsh/temps/issues/286))

### Performance

- **proxy:** Serve proxy-log stats from a 1-minute continuous aggregate ([#278](https://github.com/gotempsh/temps/issues/278))
- **otel:** Make storage quota opt-in, disabled by default ([#283](https://github.com/gotempsh/temps/issues/283))

## [0.1.0-beta.45] - 2026-07-11

### Added

- **web:** Add Let's Encrypt contact email to Settings page ([#273](https://github.com/gotempsh/temps/issues/273))
- **dns:** Add Pebble challtestsrv-backed DNS provider for local testing ([#275](https://github.com/gotempsh/temps/issues/275))

### CI

- **release:** Cut macOS/web build time in release pipeline ([#271](https://github.com/gotempsh/temps/issues/271))

### Fixed

- **proxy:** Isolate preview connection-pool by sandbox target ([#274](https://github.com/gotempsh/temps/issues/274))
- **proxy:** Track real body bytes for request/response bandwidth ([#276](https://github.com/gotempsh/temps/issues/276))

## [0.1.0-beta.44] - 2026-07-10

### Fixed

- **domains:** Wire config/DNS services into TlsService for auto-renewal ([#270](https://github.com/gotempsh/temps/issues/270))

## [0.1.0-beta.43] - 2026-07-10

### Added

- **analytics-sdk:** Exclude sensitive paths from session replay by default
- **auth:** Audit concurrent sessions and allow requiring MFA for admins ([#189](https://github.com/gotempsh/temps/issues/189))
- **deployments:** Add generic DeploymentGate extension point ([#229](https://github.com/gotempsh/temps/issues/229))
- **providers:** Add managed S3 backend protocol + minIO support ([#144](https://github.com/gotempsh/temps/issues/144))
- **auth:** Add platform-admin role, key/token rotation, and MCP kill-switch ([#191](https://github.com/gotempsh/temps/issues/191))
- **deployments:** Add SecretsManagerResolver OSS extension seam (ADR 0009 §1-2) ([#252](https://github.com/gotempsh/temps/issues/252))
- **proxy:** Hot-path metrics with node dashboard and default alerts ([#258](https://github.com/gotempsh/temps/issues/258))
- **auth:** Add ProjectAccessChecker trait and project_access_guard! macro (ADR 028 Phase A) ([#260](https://github.com/gotempsh/temps/issues/260))
- **ai-chat:** Allow AI write tool to provision and link external services ([#256](https://github.com/gotempsh/temps/issues/256))
- **auth:** Wire project_access_guard! into all project-scoped handlers (ADR 028 Phase B) ([#261](https://github.com/gotempsh/temps/issues/261))
- **ai-tools:** Expose deployment and node metrics to AI read allowlist ([#265](https://github.com/gotempsh/temps/issues/265))
- **auth:** Project-scoped permission narrowing (project_permission_guard!) ([#268](https://github.com/gotempsh/temps/issues/268))

### CI

- Add dependency and container scanning for Temps' own supply chain ([#187](https://github.com/gotempsh/temps/issues/187))
- Auto-merge dependabot patch/minor PRs once CI passes ([#228](https://github.com/gotempsh/temps/issues/228))
- Reduce rust-cache eviction churn and stop starving PRs of it ([#248](https://github.com/gotempsh/temps/issues/248))
- Fix unit-tests build taking 22m+ due to feature-flag cache mismatch
- Build test binaries once via cargo-nextest, share across all jobs ([#253](https://github.com/gotempsh/temps/issues/253))

### Documentation

- Forbid environment variables for runtime configuration
- Require new features to scale on small resources ([#257](https://github.com/gotempsh/temps/issues/257))
- **adr:** Project-scoped RBAC enforcement (ADR 028) ([#259](https://github.com/gotempsh/temps/issues/259))

### Fixed

- **ci:** Make rust-tests reliably fail on test failures
- **providers:** Bind provisioned service ports to 127.0.0.1 instead of 0.0.0.0 ([#190](https://github.com/gotempsh/temps/issues/190))
- **ci:** Serialize docker-deployments integration tests to stop Postgres deadlocks ([#195](https://github.com/gotempsh/temps/issues/195))
- **test-utils:** Disable TimescaleDB background workers in TestDatabase ([#196](https://github.com/gotempsh/temps/issues/196))
- **ci:** Disable TimescaleDB background workers in integration-tests services container ([#197](https://github.com/gotempsh/temps/issues/197))
- **web:** Resolve gitlab vs github commit links and trace UI polish ([#231](https://github.com/gotempsh/temps/issues/231))
- **web:** Add web TypeScript check to CI and fix all errors ([#233](https://github.com/gotempsh/temps/issues/233))
- Resolve stacked dependency-bump compilation breaks on main ([#247](https://github.com/gotempsh/temps/issues/247))
- Repair rcgen 0.14 API breaks on main ([#250](https://github.com/gotempsh/temps/issues/250))
- **providers:** Persist actual bound port after container creation retry ([#254](https://github.com/gotempsh/temps/issues/254))
- **auth:** Enforce privilege ceiling on custom API key permissions ([#267](https://github.com/gotempsh/temps/issues/267))
- **analytics:** Resolve visitor_id on lookup miss instead of dropping it ([#269](https://github.com/gotempsh/temps/issues/269))

### Miscellaneous

- **scripts:** Add local testcontainer orphan-cleanup script ([#246](https://github.com/gotempsh/temps/issues/246))

### Performance

- **proxy:** Remove per-request database dependencies from the hot path ([#230](https://github.com/gotempsh/temps/issues/230))
- **otel:** Cache storage quota checks to cut ingest DB load ([#262](https://github.com/gotempsh/temps/issues/262))
- **auth:** Throttle last_used_at writes on every authenticated request ([#264](https://github.com/gotempsh/temps/issues/264))
- Bound proxy-log and monitor-health hypertable lookups by time ([#266](https://github.com/gotempsh/temps/issues/266))

### Security

- Gate unauthenticated admin endpoints + expand AI chat allowlist ([#249](https://github.com/gotempsh/temps/issues/249))

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
